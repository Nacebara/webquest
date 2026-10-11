//! Движок заявок exch (HANDOVER §5, M3).
//!
//! - [`Engine::submit_check`] — клиент прислал чек: заявка, удержание резерва целевого
//!   кошелька и операция активации — одной транзакцией;
//! - [`Engine::process_due`] — исполнитель outbox: берёт созревшие операции (коммит
//!   `dispatched` ДО действия), зовёт юзербота ([`userbot::WalletBot`]) и записывает итог
//!   вместе с переходом заявки, проводками и следующими операциями — одной транзакцией;
//! - [`Engine::recover`] — после рестарта: активации повторяются (это безопасно), выплаты
//!   с неизвестным исходом — только сверка (`find_issued_check`), никогда не повтор;
//! - [`Engine::run`] — цикл: восстановление, затем исполнитель по `LISTEN exch_operations`
//!   и раз в секунду.
//!
//! Правила денег — CLAUDE.md п. 1–10. Здесь они держатся так: переход заявки только через
//! `domain::fsm` (в `storage::orders::apply_event`); эффекты перехода — в той же транзакции
//! ([`effects`]); выплата — одна попытка, неясный исход — сверка или `MANUAL_REVIEW`.

mod effects;
mod settle;
mod submit;
mod worker;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use domain::fsm::OrderState;
use domain::{Asset, Decimal, Platform};
use sqlx::PgPool;
use storage::StorageError;
use tokio::sync::broadcast;
use userbot::WalletBot;

pub use submit::{SubmitReject, Submitted};

/// Настройки движка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    /// Часть резерва, которую не отдаём под заявки (SPEC §3.4).
    pub reserve_safety: Decimal,
    /// Через сколько вернуться к операции, которая не выполнялась (кошелёк недоступен).
    pub unavailable_backoff: Duration,
    /// Через сколько повторить активацию с неизвестным исходом.
    pub activation_retry_backoff: Duration,
    /// Сколько операций выполнять за один проход (берутся по одной).
    pub batch: i64,
    /// Сколько ждать появления чека при сверке выплаты с неизвестным исходом, прежде чем
    /// отдать заявку человеку (бот мог ещё не дописать кнопку или ответ меню).
    pub reconcile_grace: Duration,
    /// Сколько операция может не выполняться (кошелёк недоступен, сверка не удаётся),
    /// прежде чем заявка уйдёт к человеку, а направления платформы — на паузу.
    pub escalate_after: Duration,
    /// Операция `dispatched` дольше этого — осиротела (ошибка или падение посреди работы).
    pub stale_after: Duration,
    /// Как часто повторять сверку выплат с неизвестным исходом и искать осиротевшие.
    pub housekeeping_every: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            reserve_safety: Decimal::ZERO,
            unavailable_backoff: Duration::from_secs(30),
            activation_retry_backoff: Duration::from_secs(30),
            batch: 16,
            reconcile_grace: Duration::from_secs(600),
            escalate_after: Duration::from_secs(1800),
            stale_after: Duration::from_secs(900),
            housekeeping_every: Duration::from_secs(30),
        }
    }
}

/// Заявка изменилась — для клиентского бота (обновить сообщение заявки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderUpdate {
    pub order_id: i64,
    pub user_id: i64,
    pub state: OrderState,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error("no wallet bot for {0:?}")]
    NoWallet(Platform),
    #[error("no active wallet account for {0:?}")]
    NoWalletAccount(Platform),
    #[error(transparent)]
    Posting(#[from] domain::ledger::PostingError),
    #[error(transparent)]
    Pricing(#[from] domain::pricing::PricingError),
    #[error(transparent)]
    Money(#[from] domain::money::MoneyError),
    /// Движок уже работает в другом процессе (advisory-блокировка лидера занята).
    #[error("another engine instance is running")]
    AnotherInstance,
    /// Нарушено ожидание кода: данные заявки не сходятся с её состоянием.
    #[error("invariant violated: {0}")]
    Invariant(String),
}

/// Движок заявок.
pub struct Engine {
    pool: PgPool,
    wallets: HashMap<Platform, Arc<dyn WalletBot>>,
    cfg: EngineConfig,
    updates: broadcast::Sender<OrderUpdate>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("wallets", &self.wallets.keys().collect::<Vec<_>>())
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl Engine {
    pub fn new(pool: PgPool, cfg: EngineConfig) -> Self {
        let (updates, _) = broadcast::channel(1024);
        Self {
            pool,
            wallets: HashMap::new(),
            cfg,
            updates,
        }
    }

    /// Кошелёк платформы (юзербот этой платформы).
    pub fn with_wallet(mut self, wallet: Arc<dyn WalletBot>) -> Self {
        self.wallets.insert(wallet.platform(), wallet);
        self
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Поток изменений заявок.
    pub fn subscribe(&self) -> broadcast::Receiver<OrderUpdate> {
        self.updates.subscribe()
    }

    fn wallet(&self, platform: Platform) -> Result<&Arc<dyn WalletBot>, EngineError> {
        self.wallets
            .get(&platform)
            .ok_or(EngineError::NoWallet(platform))
    }

    fn notify(&self, order: &storage::orders::OrderRow) {
        // Нет подписчиков — не ошибка: клиентский бот мог ещё не запуститься.
        let _ = self.updates.send(OrderUpdate {
            order_id: order.id,
            user_id: order.user_id,
            state: order.state,
        });
    }

    /// Счета леджера для активных кошельков и всех активов (идемпотентно). Вызывать при старте.
    pub async fn ensure_ledger_accounts(&self) -> Result<(), EngineError> {
        use domain::ledger::{AccountCode, Revenue};
        let mut tx = self.pool.begin().await?;
        for platform in Platform::ALL {
            let Some((wallet_id, _)) = storage::wallets::active(&mut tx, platform).await? else {
                continue;
            };
            for asset in Asset::ALL {
                storage::ledger::ensure_account(
                    &mut tx,
                    &AccountCode::wallet(platform, asset),
                    asset,
                    Some(wallet_id),
                )
                .await?;
            }
        }
        for asset in Asset::ALL {
            for code in [
                AccountCode::payable(asset),
                AccountCode::revenue(Revenue::Fee, asset),
                AccountCode::revenue(Revenue::Excess, asset),
            ] {
                storage::ledger::ensure_account(&mut tx, &code, asset, None).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Рабочий цикл до отмены: блокировка лидера, восстановление, затем исполнитель
    /// операций и периодическая уборка (сверка неизвестных выплат, осиротевшие операции).
    ///
    /// Второй процесс с тем же `DATABASE_URL` получит [`EngineError::AnotherInstance`]: два
    /// движка приняли бы живые операции друг друга за прерванные.
    pub async fn run(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EngineError> {
        let mut leader = self.pool.acquire().await?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1, $2)")
            .bind(LEADER_LOCK.0)
            .bind(LEADER_LOCK.1)
            .fetch_one(&mut *leader)
            .await?;
        if !locked {
            return Err(EngineError::AnotherInstance);
        }
        self.recover().await?;
        let mut listener = sqlx::postgres::PgListener::connect_with(&self.pool).await?;
        listener.listen("exch_operations").await?;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut housekeeping = tokio::time::interval(self.cfg.housekeeping_every);
        loop {
            while self.process_due().await? > 0 {}
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {}
                _ = housekeeping.tick() => {
                    self.resolve_unknown().await?;
                    self.recover_dispatched(Some(self.cfg.stale_after)).await?;
                }
                note = listener.recv() => {
                    if let Err(e) = note {
                        tracing::warn!(error = %e, "LISTEN connection lost, polling");
                    }
                }
            }
        }
        // Блокировка уходит вместе с соединением; явно — чтобы не ждать его закрытия.
        let _ = sqlx::query("SELECT pg_advisory_unlock($1, $2)")
            .bind(LEADER_LOCK.0)
            .bind(LEADER_LOCK.1)
            .execute(&mut *leader)
            .await;
        Ok(())
    }
}

/// Ключ advisory-блокировки лидера движка.
const LEADER_LOCK: (i32, i32) = (7002, 1);
