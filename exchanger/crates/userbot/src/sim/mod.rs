//! Симулятор ботов @send (CryptoBot) и @xrocket (DESIGN-v0.2 §5) — для тестов сценариев
//! юзербота и движка без сети.
//!
//! Один детерминированный [`SimWorld`] (общий `Arc<Mutex<…>>`) держит балансы аккаунтов по
//! платформам и активам, реестры чеков и счетов, сообщения в чатах, инлайн-запросы и сессии
//! мини-приложения. [`SimTransport`] — реализация [`crate::Transport`] для одного аккаунта;
//! экземпляров на мир может быть сколько угодно (по одному на аккаунт Telegram).
//!
//! Что умеют боты (тексты — [`texts`], совместимы со словарями lovec):
//! - `/start <чек>` — все классы ответа: «Вы получили 100 USDT.», «Этот чек уже активирован.»,
//!   «Чек не найден.» / «Мульти-чек не найден.», «Вы не можете активировать этот чек.»,
//!   «Подпишитесь на канал …», капча, «Введите пароль от чека.», только для Premium;
//!   у xRocket — «не найден» на первый `/start` свежего чека
//!   ([`SimWorld::set_xrocket_fresh_not_found`]); создатель получает «Ваш чек … активировал @…»;
//! - меню чеков: CryptoBot `/checks` → «Создать чек» → валюта → сумма → ссылка
//!   `https://t.me/send?start=CQ…` и URL-кнопка; xRocket `/cheques` → «Персональный» →
//!   «Создать чек» → валюта → сумма → `https://t.me/xrocket?start=t_…`; «Мои чеки»;
//!   «Недостаточно средств»;
//! - инлайн: `10usdt` у @send, `10` у @xrocket → результаты по активам; чек создаётся
//!   в момент `send_inline`, сообщение «через бота» несёт URL-кнопку со ссылкой;
//! - счета `IV…` / `inv_…`: карточка, кнопки валют, «Оплатить»; у CryptoBot — кнопка
//!   мини-приложения: `open_webapp` даёт URL сессии, шаг PIN —
//!   [`SimWorld::complete_webapp_payment`]; статусы оплачен / истёк, многоразовые счета;
//! - экран «Кошелёк» (`/wallet`), входящие переводы «Вы получили 5 USDT от @x.».
//!
//! Дубль `random_id` в `send_text` / `send_inline` → `RandomIdDuplicate` без последствий.
//! Сбои — [`SimWorld::inject`]: таймаут до и после выполнения, разрыв, `FLOOD_WAIT`,
//! задержка ответа, промежуточное «Подождите, идёт обработка…» с правкой, незнакомый текст,
//! правка вместо нового сообщения, молчание.
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

use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use domain::{Asset, Decimal, Money, Platform};

pub use model::{
    AccountId, Call, CheckOrigin, CheckSpec, CheckStatus, ClaimOutcome, Fault, InvoiceAmount,
    InvoicePayment, InvoiceSpec, SimCheck, SimError, SimInvoice, SimInvoiceStatus, WebAppPayment,
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

    /// PIN мини-приложения CryptoBot. Пока не задан, подходит любой.
    pub fn set_pin(&self, account: AccountId, pin: &str) -> Result<(), SimError> {
        self.lock().account_mut(account)?.pin = Some(pin.to_owned());
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
        let mut st = self.lock();
        let check = st
            .checks
            .get_mut(code)
            .ok_or_else(|| SimError::UnknownCheck(code.to_owned()))?;
        let refund = check.locked();
        check.status = CheckStatus::Cancelled;
        let (creator, platform, asset) = (check.creator, check.platform, check.amount.asset());
        if let (Some(c), Ok(m)) = (creator, Money::new(refund, asset)) {
            st.credit(c, platform, m)?;
        }
        Ok(())
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

    /// Шаг PIN мини-приложения CryptoBot: `url` — то, что вернул `open_webapp`.
    /// При успехе деньги списаны, счёт оплачен, в чат плательщика приходит
    /// «✅ Счёт IV… оплачен.», создателю — уведомление.
    pub fn complete_webapp_payment(&self, url: &str, pin: &str) -> Result<WebAppPayment, SimError> {
        let session = bots::query_param(url, "session")
            .and_then(|s| u64::from_str(&s).ok())
            .filter(|_| url.contains(bots::WEBAPP_HOST))
            .ok_or_else(|| SimError::UnknownSession(url.to_owned()))?;
        let mut st = self.lock();
        st.tick();
        let (result, outs) = bots::complete_webapp(&mut st, session, pin)?;
        st.publish_all(outs);
        Ok(result)
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
        st.publish(Outbound {
            account: to,
            chat: Chat::WalletBot(platform),
            op: Op::New(Draft::text(texts::incoming_transfer(platform, &m, &from))),
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
