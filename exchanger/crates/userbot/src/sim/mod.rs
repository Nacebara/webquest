//! Симулятор ботов @send (CryptoBot) и @xrocket (DESIGN-v0.2 §5) — для тестов сценариев
//! юзербота и движка без сети.
//!
//! Один детерминированный [`SimWorld`] (общий `Arc<Mutex<…>>`) держит балансы аккаунтов по
//! платформам и активам, реестры чеков и счетов, сообщения в чатах, экран каждого сообщения
//! бота и траты CryptoBot для «лимита без код-пароля». [`SimTransport`] — реализация
//! [`crate::Transport`] для одного аккаунта; экземпляров на мир может быть сколько угодно.
//!
//! Тексты и экраны — живые, из записи владельца ([`texts`], сверка с записью — тест
//! `same_as_live`); как боты ведут себя на каждом шаге — в описании модуля `bots`:
//! - `/start <чек>` — «🕘 Получение…» с правкой в «📥 Вы получили…» (xRocket — «⏱ Получение…»
//!   → «💰 Вы успешно получили…»), «уже активирован» (xRocket: «…активирован вами», если
//!   забрал этот же аккаунт), «не найден», «предназначен для другого получателя», подписка,
//!   капча, пароль, только Premium (синтетика); у xRocket — «не найден» на первый `/start`
//!   свежего чека ([`SimWorld::set_xrocket_fresh_not_found`]); создатель получает
//!   «… активировал(а) ваш чек …»;
//! - меню чеков: CryptoBot `/checks` → «Создать чек» → монета → сумма → «Пропустить ›» (если
//!   на мультичек хватает) → экран чека; xRocket `/cheques` → «Персональный» → «Создать чек»
//!   → монета → сумма → «✅ Подтверждаю» → экран чека; удаление чека;
//! - счета `IV…` / `inv_…`: CryptoBot — выбор монеты (счёт в фиате) или сразу подтверждение,
//!   «Выберите сеть…» при нехватке, «доверяете этому получателю?» для нового получателя,
//!   код-пароль сверх «лимита без код-пароля» ([`SimWorld::set_passcode_free_limit`],
//!   по умолчанию $500); xRocket — способ оплаты, подтверждение, «⏱ Загрузка» и квитанция;
//!   статусы оплачен / истёк, многоразовые счета;
//! - экран «Кошелёк» (`/wallet`), входящие переводы «📥 Вы получили … от @x.».
//!
//! Дубль `random_id` в `send_text` → `RandomIdDuplicate` без последствий.
//! Сбои — [`SimWorld::inject`]: таймаут до и после выполнения, разрыв, `FLOOD_WAIT`,
//! задержка ответа, промежуточный экран с правкой, незнакомый текст, правка вместо нового
//! сообщения, молчание.
//!
//! Время мира виртуальное: каждый вызов транспорта (кроме `history`) сдвигает его на шаг
//! ([`SimWorld::set_tick`], по умолчанию 100 мс), тест может сдвинуть его сам
//! ([`SimWorld::advance`]). Задержки ответов — настоящие `tokio::time::sleep`
//! (в тестах — `start_paused`). Id сообщений — общий счётчик аккаунта, как у личных чатов
//! в Telegram: в каждом чате они растут, но идут не подряд.

mod bots;
mod model;
mod state;
pub mod texts;
mod transport;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use domain::{Asset, Decimal, Money, Platform};

pub use model::{
    AccountId, Call, CheckOrigin, CheckSpec, CheckStatus, ClaimOutcome, Fault, InvoiceAmount,
    InvoicePayment, InvoiceSpec, SimCheck, SimError, SimInvoice, SimInvoiceStatus,
};
pub use transport::SimTransport;

use state::{Delayed, Draft, Op, Outbound, WorldState};

use crate::transport::{Chat, RawMessage};

/// Зерно ГПСЧ по умолчанию: коды чеков одинаковы во всех тестах.
pub const DEFAULT_SEED: u64 = 0x5EED_E8C4;

/// Общий мир симулятора. Клонируется дёшево (`Arc`).
#[derive(Clone)]
pub struct SimWorld {
    inner: Arc<Mutex<WorldState>>,
}

impl Default for SimWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SimWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.lock();
        f.debug_struct("SimWorld")
            .field("now_ms", &st.now_ms)
            .field("accounts", &st.accounts.len())
            .field("checks", &st.checks.len())
            .field("invoices", &st.invoices.len())
            .finish()
    }
}

impl SimWorld {
    pub fn new() -> Self {
        Self::with_seed(DEFAULT_SEED)
    }

    pub fn with_seed(seed: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WorldState::new(seed))),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, WorldState> {
        // Паника внутри симулятора — уже провал теста; состояние читаем как есть.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Опубликовать отложенные ответы отдельной задачей tokio.
    pub(crate) fn spawn_delayed(&self, delayed: Option<Delayed>) {
        let Some(delayed) = delayed else { return };
        let world = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delayed.delay).await;
            world.lock().publish_all(delayed.ops);
        });
    }

    // ---------- аккаунты ----------

    /// Новый аккаунт Telegram (юзербот или клиент). `username` — без `@` или с ним.
    pub fn add_account(&self, username: &str) -> AccountId {
        self.lock().add_account(username)
    }

    /// Транспорт от имени аккаунта. Для неизвестного аккаунта вызовы вернут `NotAuthorized`.
    pub fn transport(&self, account: AccountId) -> SimTransport {
        let tx = self.lock().accounts.get(&account).map(|a| a.tx.clone());
        SimTransport::new(self.clone(), account, tx)
    }

    pub fn username(&self, account: AccountId) -> Option<String> {
        self.lock()
            .accounts
            .get(&account)
            .map(|a| a.username.clone())
    }

    pub fn set_premium(&self, account: AccountId, premium: bool) -> Result<(), SimError> {
        self.lock().account_mut(account)?.premium = premium;
        Ok(())
    }

    /// Аккаунт подписался на канал (`@channel`) — чеки с подпиской на него станут доступны.
    pub fn join_channel(&self, account: AccountId, channel: &str) -> Result<(), SimError> {
        self.lock()
            .account_mut(account)?
            .channels
            .insert(channel.to_owned());
        Ok(())
    }

    /// CryptoBot «лимит без код-пароля» аккаунта (USD за 24 часа); `None` — код всегда.
    pub fn set_passcode_free_limit(
        &self,
        account: AccountId,
        limit: Option<Decimal>,
    ) -> Result<(), SimError> {
        self.lock().account_mut(account)?.passcode_free_limit = limit;
        Ok(())
    }

    // ---------- время и настройки ----------

    /// Время мира, мс Unix.
    pub fn now_ms(&self) -> i64 {
        self.lock().now_ms
    }

    pub fn advance(&self, by: Duration) {
        self.lock().now_ms += duration_ms(by);
    }

    /// Шаг времени на каждый вызов транспорта (кроме `history`).
    pub fn set_tick(&self, tick: Duration) {
        self.lock().tick_ms = duration_ms(tick);
    }

    /// xRocket отвечает «Чек не найден.» на первый `/start` чека моложе `window`;
    /// следующий `/start` обрабатывается как обычно. `None` — выключено (по умолчанию).
    pub fn set_xrocket_fresh_not_found(&self, window: Option<Duration>) {
        self.lock().xrocket_fresh_window_ms = window.map(duration_ms);
    }

    /// Курс для фиатных счетов: сколько `currency` стоит 1 единица `asset`.
    /// По умолчанию USDT = 1 USD, TON = 5 USD.
    pub fn set_fiat_rate(&self, asset: Asset, currency: &str, rate: Decimal) {
        self.lock()
            .fiat_rates
            .insert((asset, currency.to_owned()), rate);
    }

    // ---------- балансы ----------

    pub fn set_balance(
        &self,
        account: AccountId,
        platform: Platform,
        asset: Asset,
        amount: Decimal,
    ) -> Result<(), SimError> {
        self.lock().set_balance(account, platform, asset, amount)
    }

    pub fn credit(&self, account: AccountId, platform: Platform, m: Money) -> Result<(), SimError> {
        self.lock().credit(account, platform, m)
    }

    pub fn balance(&self, account: AccountId, platform: Platform, asset: Asset) -> Decimal {
        self.lock().balance(account, platform, asset)
    }

    /// Балансы аккаунта на платформе по всем активам (`Asset::ALL`).
    pub fn balances(&self, account: AccountId, platform: Platform) -> Vec<Money> {
        let st = self.lock();
        Asset::ALL
            .iter()
            .map(|a| st.balance_money(account, platform, *a))
            .collect()
    }

    /// Все деньги актива на платформе: балансы аккаунтов + неиспользованное в активных чеках.
    /// Меняется только помощниками (чеки внешних клиентов, переводы извне, оплата счетов
    /// внешним продавцам) — удобно для проверки, что деньги не появились и не пропали.
    pub fn total_in_world(&self, platform: Platform, asset: Asset) -> Decimal {
        let st = self.lock();
        let balances: Decimal = st
            .accounts
            .values()
            .filter_map(|a| a.balances.get(&(platform, asset)))
            .copied()
            .sum();
        let locked: Decimal = st
            .checks
            .values()
            .filter(|c| c.platform == platform && c.amount.asset() == asset)
            .map(SimCheck::locked)
            .sum();
        balances + locked
    }

    // ---------- чеки ----------

    /// Положить чек в мир (как будто его создал клиент в своём Telegram).
    pub fn create_check(&self, spec: CheckSpec) -> Result<SimCheck, SimError> {
        let mut st = self.lock();
        st.tick();
        st.insert_check(spec, CheckOrigin::Helper)
    }

    /// Чек внешнего клиента: деньги приходят извне. Возвращает ссылку.
    pub fn client_check(&self, platform: Platform, amount: Money) -> Result<String, SimError> {
        self.create_check(CheckSpec::new(platform, amount))
            .map(|c| c.url)
    }

    pub fn check(&self, code: &str) -> Option<SimCheck> {
        self.lock().checks.get(code).cloned()
    }

    /// Все чеки в порядке создания.
    pub fn checks(&self) -> Vec<SimCheck> {
        let st = self.lock();
        st.check_order
            .iter()
            .filter_map(|c| st.checks.get(c).cloned())
            .collect()
    }

    pub fn checks_created_by(&self, account: AccountId) -> Vec<SimCheck> {
        self.checks()
            .into_iter()
            .filter(|c| c.creator == Some(account))
            .collect()
    }

    /// Создатель удалил чек: остаток возвращается ему, `/start` отвечает «не найден».
    pub fn cancel_check(&self, code: &str) -> Result<(), SimError> {
        self.lock().cancel_check(code)
    }

    /// Аккаунт активирует чек из своего Telegram (`/start <код>` боту): тот же разбор, что
    /// у юзербота, сообщения — в чатах аккаунта, создателю — уведомление.
    /// Принимает код или ссылку `…?start=<код>`.
    pub fn claim_check(
        &self,
        account: AccountId,
        code_or_url: &str,
    ) -> Result<ClaimOutcome, SimError> {
        let code = bots::query_param(code_or_url, "start")
            .or_else(|| bots::query_param(code_or_url, "startapp"))
            .unwrap_or_else(|| code_or_url.trim().to_owned());
        let mut st = self.lock();
        st.account(account)?;
        st.tick();
        let platform = st
            .checks
            .get(&code)
            .map(|c| c.platform)
            .unwrap_or_else(|| platform_by_code(&code));
        let chat = Chat::WalletBot(platform);
        st.store_outgoing(account, chat, Draft::text(format!("/start {code}")));
        let (outcome, outs) = bots::claim(&mut st, account, platform, &code, None);
        st.publish_all(outs);
        Ok(outcome)
    }

    // ---------- счета ----------

    pub fn create_invoice(&self, spec: InvoiceSpec) -> Result<SimInvoice, SimError> {
        let mut st = self.lock();
        st.tick();
        st.insert_invoice(spec)
    }

    pub fn invoice(&self, code: &str) -> Option<SimInvoice> {
        self.lock().invoices.get(code).cloned()
    }

    pub fn invoices(&self) -> Vec<SimInvoice> {
        let st = self.lock();
        st.invoice_order
            .iter()
            .filter_map(|c| st.invoices.get(c).cloned())
            .collect()
    }

    /// Счёт истёк: карточка покажет «истёк», оплата — «Срок действия счёта истёк.».
    pub fn expire_invoice(&self, code: &str) -> Result<(), SimError> {
        let mut st = self.lock();
        let inv = st
            .invoices
            .get_mut(code)
            .ok_or_else(|| SimError::UnknownInvoice(code.to_owned()))?;
        if inv.status == SimInvoiceStatus::Active {
            inv.status = SimInvoiceStatus::Expired;
        }
        Ok(())
    }

    // ---------- сообщения ----------

    /// Входящий перевод извне: баланс растёт, бот пишет «Вы получили 5 USDT от @x.».
    pub fn incoming_transfer(
        &self,
        to: AccountId,
        platform: Platform,
        m: Money,
        from: &str,
    ) -> Result<RawMessage, SimError> {
        let mut st = self.lock();
        st.tick();
        st.credit(to, platform, m)?;
        let from = format!("@{}", from.trim_start_matches('@'));
        let usd = st.usd_value(&m);
        st.publish(Outbound {
            account: to,
            chat: Chat::WalletBot(platform),
            op: Op::New(Draft::text(texts::incoming_transfer(
                platform, &m, usd, &from,
            ))),
        })
        .ok_or(SimError::UnknownAccount(to))
    }

    /// Произвольное сообщение бота в чате аккаунта (реклама, «посторонние» экраны).
    pub fn bot_message(
        &self,
        to: AccountId,
        platform: Platform,
        text: &str,
    ) -> Result<RawMessage, SimError> {
        let mut st = self.lock();
        st.tick();
        st.publish(Outbound {
            account: to,
            chat: Chat::WalletBot(platform),
            op: Op::New(Draft::text(text)),
        })
        .ok_or(SimError::UnknownAccount(to))
    }

    /// Все сообщения чата аккаунта по возрастанию id (последние версии после правок).
    pub fn messages(&self, account: AccountId, chat: Chat) -> Vec<RawMessage> {
        self.lock()
            .accounts
            .get(&account)
            .and_then(|a| a.chats.get(&chat).cloned())
            .unwrap_or_default()
    }

    // ---------- сбои ----------

    /// Следующие `times` вызовов `on` аккаунта получат сбой `fault`. Правила проверяются
    /// по порядку добавления; одно правило на вызов. Сбой, формирующий ответ бота
    /// (`DelayReply`, `ProcessingFirst`, …), на вызовах без ответа просто расходуется.
    pub fn inject(&self, account: AccountId, on: Call, fault: Fault, times: u32) {
        self.lock().inject(account, on, fault, times);
    }

    pub fn clear_faults(&self, account: AccountId) {
        self.lock().clear_faults(account);
    }
}

fn duration_ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// Платформа по виду кода: `CQ…` — CryptoBot, `t_…`/`mc_…`/`mci_…` — xRocket.
fn platform_by_code(code: &str) -> Platform {
    if code.starts_with("t_") || code.starts_with("mc_") || code.starts_with("mci_") {
        Platform::XRocket
    } else {
        Platform::CryptoBot
    }
}
