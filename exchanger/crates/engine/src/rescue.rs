//! «Спасти резерв» (решение владельца 17.11.2026, вариант Б).
//!
//! Владелец одной кнопкой выводит весь баланс кошелька юзербота в чек: аккаунту грозит
//! блокировка, кто-то чужой вошёл в него, кошелёк ограничил аккаунт. Юзербот создаёт чек на
//! всю сумму через меню бота кошелька — тем же путём, что выплату клиенту, — и ссылку
//! получает только владелец (тревога `rescue_done`, адресат — владелец). Владелец забирает
//! чек своим аккаунтом.
//!
//! Деньги:
//! - сумма — по живому замеру баланса перед созданием чека (не по леджеру: в чек уходит то,
//!   что реально лежит), вниз до шага выплаты; у CryptoBot — не больше запаса до «лимита без
//!   код-пароля» за 24 часа (DESIGN-v0.2 Р4, Р11): иначе бот спросит код, а его мы не вводим;
//! - операция outbox `create_payout_check` с одной попыткой; неясный исход — поиск созданного
//!   чека (`find_issued_check`), не нашли — решает владелец («Чек был» / «Чека не было»),
//!   никогда не второй чек;
//! - чек создан — проводка `capital_out` (деньги ушли владельцу). Вернуть их в оборот —
//!   пополнить кошелёк и провести взнос по сверке, как обычно;
//! - приём и выплаты через этот кошелёк — на паузе ([`RESCUE_PAUSE`]) с момента запроса:
//!   новые заявки не начнутся, а начатые выплаты, если деньги уже ушли в чек, уйдут на разбор.
//!
//! Стоп-кран спасение не задерживает: он останавливает выплаты клиентам, а не защиту денег.

use domain::ledger;
use domain::money::Step;
use domain::{Asset, Decimal, Money, Platform};
use serde_json::json;
use storage::StorageError;
use storage::alerts::{Level, NewAlert, Topic};
use storage::audit::NewAudit;
use storage::directions::Side;
use storage::operations::{OpStatus, OperationRow};
use storage::rescues::{Rescue, RescueStatus};
use storage::staff::{Role, StaffMember};
use userbot::{BotError, IssueOutcome, IssuedCheck};

use crate::worker::{older_than, tag};
use crate::{Engine, EngineError};

/// Причина паузы кошелька после спасения.
pub const RESCUE_PAUSE: &str = "резерв спасён владельцем: деньги кошелька выведены в чек — снимите паузу, когда кошелёк снова пополнен";

const ASSET: Asset = Asset::Usdt;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RescueRefusal {
    #[error("only the owner can rescue the reserve")]
    Forbidden,
    #[error("the wallet is not connected")]
    NoWallet,
    #[error("this wallet already has an unfinished rescue")]
    AlreadyOpen,
    #[error("the wallet balance could not be read: {0}")]
    Unreadable(String),
    /// В кошельке меньше минимального чека (или запас до лимита CryptoBot меньше).
    #[error("nothing to rescue: {available} available, the smallest check is {min}")]
    Nothing { available: Decimal, min: Decimal },
    #[error("the rescue is not waiting for a decision")]
    WrongState,
}

#[derive(Debug, thiserror::Error)]
pub enum RescueError {
    #[error(transparent)]
    Refused(#[from] RescueRefusal),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

impl From<StorageError> for RescueError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::RescueOpen => RescueRefusal::AlreadyOpen.into(),
            StorageError::RescueState(_) => RescueRefusal::WrongState.into(),
            other => RescueError::Engine(other.into()),
        }
    }
}

impl From<sqlx::Error> for RescueError {
    fn from(e: sqlx::Error) -> Self {
        RescueError::Engine(e.into())
    }
}

/// Что получилось на запрос.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requested {
    pub rescue: Rescue,
    /// Сумма чека меньше баланса: CryptoBot — лимит без код-пароля.
    pub capped: bool,
    /// Сколько операций по кошельку было в работе (их выплаты могут уйти на разбор).
    pub in_flight: i64,
}

fn owner(staff: &StaffMember) -> Result<(), RescueRefusal> {
    if staff.active() && staff.role >= Role::Owner {
        Ok(())
    } else {
        Err(RescueRefusal::Forbidden)
    }
}

impl Engine {
    /// Спасти резерв кошелька `platform`: замер баланса и операция чека владельцу.
    pub async fn rescue_request(
        &self,
        platform: Platform,
        staff: &StaffMember,
        reason: Option<&str>,
    ) -> Result<Requested, RescueError> {
        owner(staff)?;
        let wallet_id = {
            let mut conn = self.pool.acquire().await?;
            storage::wallets::active(&mut conn, platform)
                .await?
                .map(|(id, _)| id)
                .ok_or(RescueRefusal::NoWallet)?
        };
        // Незавершённое спасение — сразу отказ, до замера баланса (индекс в базе — вторая защита).
        {
            let mut conn = self.pool.acquire().await?;
            let open: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM rescues \
                   WHERE wallet_account_id = $1 AND status IN ('pending', 'unknown'))",
            )
            .bind(wallet_id)
            .fetch_one(&mut *conn)
            .await?;
            if open {
                return Err(RescueRefusal::AlreadyOpen.into());
            }
        }
        // Живой замер: в чек уходит то, что реально лежит в кошельке.
        let balances = self
            .wallet(platform)?
            .balances()
            .await
            .map_err(|e| RescueRefusal::Unreadable(e.to_string()))?;
        let measured = balances
            .iter()
            .find(|m| m.asset() == ASSET)
            .map_or(Decimal::ZERO, Money::amount);

        let mut tx = self.pool.begin().await?;
        let step =
            Step::from_decimal(storage::directions::payout_step(&mut tx, platform, ASSET).await?)
                .map_err(EngineError::from)?;
        let mut amount = step.floor(measured);
        let mut capped = false;
        if platform == Platform::CryptoBot {
            let limit = match crate::submit::setting_decimal(&mut tx, "cb_passcode_free_limit_usd")
                .await?
            {
                Some(l) => Some(l),
                None => self.cfg.cb_passcode_free_limit,
            };
            if let Some(limit) = limit {
                let spent = storage::holds::spent_24h(&mut tx, wallet_id).await?;
                let room = step.floor(domain::pricing::passcode_free_headroom(limit, spent));
                if room < amount {
                    amount = room;
                    capped = true;
                }
            }
        }
        let min = storage::directions::min_check(&mut tx, platform, ASSET).await?;
        if amount <= Decimal::ZERO || amount < min {
            return Err(RescueRefusal::Nothing {
                available: amount.max(Decimal::ZERO),
                min,
            }
            .into());
        }
        let id = storage::rescues::insert(
            &mut tx,
            platform,
            wallet_id,
            ASSET,
            amount,
            measured.max(amount),
            staff.tg_user_id,
            reason,
        )
        .await?;
        let key = format!("rescue-{id}");
        storage::operations::insert_rescue(
            &mut tx,
            &storage::operations::NewRescueOperation {
                rescue_id: id,
                platform,
                wallet_account_id: wallet_id,
                idempotency_key: &key,
                amount,
                asset: ASSET,
                random_id: userbot::flows::rid::new_random_id(),
            },
        )
        .await?;
        for side in [Side::Source, Side::Target] {
            storage::directions::auto_pause_platform(&mut tx, platform, side, RESCUE_PAUSE).await?;
        }
        let in_flight: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM operations \
              WHERE wallet_account_id = $1 AND rescue_id IS NULL \
                AND status IN ('pending', 'dispatched', 'unknown') AND finished_at IS NULL",
        )
        .bind(wallet_id)
        .fetch_one(&mut *tx)
        .await?;
        storage::audit::record(
            &mut tx,
            &NewAudit {
                staff_id: staff.tg_user_id,
                action: "reserve.rescue",
                target_type: Some("wallet"),
                target_id: Some(platform.db_name()),
                before: Some(json!({ "measured": measured.to_string() })),
                after: Some(
                    json!({ "rescue": id, "amount": amount.to_string(), "capped": capped }),
                ),
                reason,
                double_confirmed: true,
            },
        )
        .await?;
        let rescue = storage::rescues::get(&mut tx, id, false).await?;
        tx.commit().await?;
        tracing::warn!(rescue = id, platform = platform.db_name(), %amount, %measured, capped, in_flight, "reserve rescue requested by the owner");
        Ok(Requested {
            rescue,
            capped,
            in_flight,
        })
    }

    /// Итог создания чека спасения.
    pub(crate) async fn rescue_on_issue(
        &self,
        op: &OperationRow,
        rescue_id: i64,
        result: Result<IssueOutcome, BotError>,
    ) -> Result<(), EngineError> {
        let outcome = match result {
            Ok(o) => o,
            Err(e) => return self.release(op, &e).await,
        };
        self.wallet_answered(op).await;
        let holds_chat = outcome.holds_chat();
        match outcome {
            IssueOutcome::Issued { check, .. } => self.rescue_issued(op, rescue_id, &check).await,
            IssueOutcome::InsufficientFunds => {
                let reason = "в кошельке не хватило денег на чек: баланс изменился после замера";
                let mut tx = self.pool.begin().await?;
                match storage::operations::finish(
                    &mut tx,
                    op.id,
                    OpStatus::Failed,
                    &json!({ "insufficient_funds": true }),
                    None,
                    Some(reason),
                )
                .await
                {
                    Ok(()) | Err(StorageError::OperationState(_)) => {}
                    Err(e) => return Err(e.into()),
                }
                if let Ok(r) = self
                    .rescue_set(&mut tx, rescue_id, RescueStatus::Failed, None)
                    .await
                {
                    self.rescue_alert(&mut tx, &r, "rescue_failed", reason)
                        .await?;
                }
                tx.commit().await?;
                tracing::error!(
                    rescue = rescue_id,
                    "reserve rescue check not created: insufficient funds"
                );
                Ok(())
            }
            IssueOutcome::Unknown { detail } => {
                {
                    let mut conn = self.pool.acquire().await?;
                    if holds_chat {
                        Self::pause_unanswered(&mut conn, op.platform).await?;
                    }
                    match storage::operations::finish(
                        &mut conn,
                        op.id,
                        OpStatus::Unknown,
                        &json!({ "detail": detail }),
                        None,
                        Some(&detail),
                    )
                    .await
                    {
                        Ok(()) | Err(StorageError::OperationState(_)) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                tracing::warn!(op = op.id, rescue = rescue_id, %detail, "reserve rescue outcome unknown, reconciling");
                self.rescue_reconcile(op, rescue_id).await
            }
        }
    }

    /// Чек спасения создан: итог операции, `capital_out`, ссылка — владельцу.
    async fn rescue_issued(
        &self,
        op: &OperationRow,
        rescue_id: i64,
        check: &IssuedCheck,
    ) -> Result<(), EngineError> {
        let mut tx = self.pool.begin().await?;
        let r = storage::rescues::get(&mut tx, rescue_id, true).await?;
        if r.status == RescueStatus::Issued || r.status == RescueStatus::Failed {
            return Ok(());
        }
        storage::operations::finish(
            &mut tx,
            op.id,
            OpStatus::Succeeded,
            &json!({ "url": check.url }),
            Some(&check.param),
            None,
        )
        .await?;
        let amount = op.amount.unwrap_or_else(|| check.amount.amount());
        Self::rescue_post(&mut tx, op, &r, amount, "спасение резерва").await?;
        let r = self
            .rescue_set(&mut tx, rescue_id, RescueStatus::Issued, Some(&check.url))
            .await?;
        storage::alerts::resolve(&mut tx, &format!("rescue_unknown:{rescue_id}")).await?;
        self.rescue_alert(&mut tx, &r, "rescue_done", "").await?;
        tx.commit().await?;
        tracing::warn!(rescue = rescue_id, %amount, "reserve rescue check issued and sent to the owner");
        Ok(())
    }

    /// Проводка `capital_out`: деньги кошелька ушли владельцу чеком.
    async fn rescue_post(
        conn: &mut sqlx::PgConnection,
        op: &OperationRow,
        r: &Rescue,
        amount: Decimal,
        what: &str,
    ) -> Result<(), EngineError> {
        let memo = format!("{what} №{}", r.id);
        let created_by = format!("staff:{}", r.requested_by);
        storage::ledger::post(
            conn,
            &ledger::capital_out(op.platform, r.asset, amount)?,
            storage::ledger::PostingRefs {
                operation_id: Some(op.id),
                memo: Some(&memo),
                created_by: Some(&created_by),
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    async fn rescue_set(
        &self,
        conn: &mut sqlx::PgConnection,
        id: i64,
        status: RescueStatus,
        url: Option<&str>,
    ) -> Result<Rescue, EngineError> {
        storage::rescues::finish(&mut *conn, id, status, url).await?;
        Ok(storage::rescues::get(conn, id, false).await?)
    }

    /// Тревога владельцу о спасении. Ссылка на чек — только в тревоге `rescue_done`.
    async fn rescue_alert(
        &self,
        conn: &mut sqlx::PgConnection,
        r: &Rescue,
        title: &str,
        reason: &str,
    ) -> Result<(), EngineError> {
        let key = match title {
            "rescue_unknown" => format!("rescue_unknown:{}", r.id),
            _ => format!("rescue:{}", r.id),
        };
        storage::alerts::raise(
            conn,
            &NewAlert {
                level: Level::Critical,
                topic: Topic::Reserves,
                dedup_key: &key,
                title,
                payload: json!({
                    "rescue": r.id,
                    "platform": r.platform.db_name(),
                    "amount": r.amount.normalize().to_string(),
                    "measured": r.measured.normalize().to_string(),
                    "url": if title == "rescue_done" { r.check_url.clone() } else { None },
                    "reason": reason,
                }),
            },
        )
        .await?;
        Ok(())
    }

    /// Неясный исход: найти чек; не нашли за `reconcile_grace` — решает владелец.
    pub(crate) async fn rescue_reconcile(
        &self,
        op: &OperationRow,
        rescue_id: i64,
    ) -> Result<(), EngineError> {
        let amount = op
            .amount
            .ok_or_else(|| EngineError::Invariant(format!("operation {} has no amount", op.id)))?;
        let money = Money::new(amount, op.asset.unwrap_or(ASSET))?;
        let wallet = self.wallet(op.platform)?.clone();
        match wallet.find_issued_check(&money, &tag(op)).await {
            Ok(Some(check)) => self.rescue_issued(op, rescue_id, &check).await,
            Ok(None) if !older_than(op, self.cfg.reconcile_grace) => Ok(()),
            Ok(None) => {
                self.rescue_give_up(
                    op,
                    rescue_id,
                    "исход создания чека неизвестен и чек не найден: проверьте чат кошелька",
                )
                .await
            }
            Err(e) if older_than(op, self.cfg.escalate_after) => {
                self.rescue_give_up(
                    op,
                    rescue_id,
                    &format!("исход создания чека неизвестен, проверить нельзя: {e}"),
                )
                .await
            }
            Err(e) => {
                tracing::warn!(op = op.id, error = %e, "reserve rescue reconciliation postponed");
                Ok(())
            }
        }
    }

    /// Автоматика сдалась: спасение ждёт решения владельца.
    pub(crate) async fn rescue_give_up(
        &self,
        op: &OperationRow,
        rescue_id: i64,
        reason: &str,
    ) -> Result<(), EngineError> {
        let mut tx = self.pool.begin().await?;
        let r = storage::rescues::get(&mut tx, rescue_id, true).await?;
        if r.status == RescueStatus::Pending {
            let r = self
                .rescue_set(&mut tx, rescue_id, RescueStatus::Unknown, None)
                .await?;
            self.rescue_alert(&mut tx, &r, "rescue_unknown", reason)
                .await?;
        }
        storage::operations::give_up(&mut tx, op.id, reason).await?;
        tx.commit().await?;
        tracing::error!(op = op.id, rescue = rescue_id, %reason, "reserve rescue handed to the owner");
        Ok(())
    }

    /// Решение владельца по неясному спасению: чек был (провести вывод) или не было.
    pub async fn rescue_resolve(
        &self,
        rescue_id: i64,
        staff: &StaffMember,
        happened: bool,
    ) -> Result<Rescue, RescueError> {
        owner(staff)?;
        let mut tx = self.pool.begin().await?;
        let r = storage::rescues::get(&mut tx, rescue_id, true).await?;
        if r.status != RescueStatus::Unknown {
            return Err(RescueRefusal::WrongState.into());
        }
        let op = storage::operations::for_rescue(&mut tx, rescue_id)
            .await?
            .into_iter()
            .rev()
            .find(|o| o.status == OpStatus::Unknown)
            .ok_or(RescueRefusal::WrongState)?;
        let by = format!("staff:{}", staff.tg_user_id);
        storage::operations::resolve_by_staff(
            &mut tx,
            op.id,
            if happened {
                OpStatus::Succeeded
            } else {
                OpStatus::Failed
            },
            &json!({ "resolved_by": by, "happened": happened }),
            None,
        )
        .await?;
        let r = if happened {
            let amount = op.amount.unwrap_or(r.amount);
            Self::rescue_post(
                &mut tx,
                &op,
                &r,
                amount,
                "спасение резерва (подтвердил владелец)",
            )
            .await?;
            self.rescue_set(
                &mut tx,
                rescue_id,
                RescueStatus::Issued,
                Some("чек найден владельцем в чате кошелька"),
            )
            .await?
        } else {
            self.rescue_set(&mut tx, rescue_id, RescueStatus::Failed, None)
                .await?
        };
        storage::alerts::resolve(&mut tx, &format!("rescue_unknown:{rescue_id}")).await?;
        storage::audit::record(
            &mut tx,
            &NewAudit {
                staff_id: staff.tg_user_id,
                action: "reserve.rescue_resolve",
                target_type: Some("wallet"),
                target_id: Some(r.platform.db_name()),
                before: Some(json!({ "rescue": rescue_id, "status": "unknown" })),
                after: Some(json!({ "rescue": rescue_id, "happened": happened })),
                reason: None,
                double_confirmed: true,
            },
        )
        .await?;
        tx.commit().await?;
        tracing::warn!(
            rescue = rescue_id,
            happened,
            "reserve rescue resolved by the owner"
        );
        Ok(r)
    }

    /// Последние спасения (для пульта).
    pub async fn rescues_recent(&self, limit: i64) -> Result<Vec<Rescue>, EngineError> {
        let mut conn = self.pool.acquire().await?;
        Ok(storage::rescues::recent(&mut conn, limit).await?)
    }
}
