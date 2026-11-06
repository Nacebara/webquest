//! Внутреннее состояние мира: аккаунты, сообщения, реестры, сбои. Всё под одним мьютексом
//! [`super::SimWorld`]; функции здесь синхронные и не ждут — задержки ответов делает
//! транспорт отдельной задачей tokio.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use domain::{Asset, Decimal, Money, Platform, Step};
use parsers::{LinkKind, WalletLink};
use tokio::sync::broadcast;

use super::model::{
    AccountId, Call, CheckOrigin, CheckSpec, CheckStatus, Fault, InvoiceAmount, InvoiceSpec,
    SimCheck, SimError, SimInvoice, SimInvoiceStatus,
};
use super::texts;
use crate::transport::{Chat, RawButton, RawMessage};

/// Начало времени мира: 2026-09-21 18:13:20 UTC (мс).
pub(crate) const START_MS: i64 = 1_790_000_000_000;
/// На столько мс сдвигается время мира при каждом вызове транспорта, кроме `history`.
pub(crate) const DEFAULT_TICK_MS: i64 = 100;
const BROADCAST_CAPACITY: usize = 4096;
/// Первый id сообщения аккаунта (как у живого аккаунта с историей).
const FIRST_MSG_ID: i32 = 1000;
const FIRST_USER_ID: i64 = 7_000_000_001;
const CHECK_NUM_BASE: u64 = 63_865_900;
const INVOICE_NUM_BASE: u64 = 69_544_490;
/// Детерминированный ГПСЧ (SplitMix64): коды чеков и счетов одинаковы от запуска к запуску.
pub(crate) struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

const CODE_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Что показывает сообщение бота. Callback-данные живых ботов — команды экрана («Success»,
/// «Back», «count-1»), а не уникальные id: смысл нажатия зависит от того, что на экране сейчас.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Screen {
    /// CryptoBot `/checks`: «Создать чек», «Активные чеки».
    CbChecksMenu,
    CbChooseCoin,
    CbAskAmount(Asset),
    /// Сумма укладывается в баланс не меньше двух раз: «Пропустить ›» — одна активация.
    /// `random_id` — у сообщения с суммой (0, если сумма выбрана кнопкой).
    CbAskActivations {
        amount: Money,
        max: u32,
        random_id: i64,
    },
    CbCheck(String),
    CbConfirmDelete(String),
    CbManage,
    XrChequesMenu,
    XrPersonal,
    XrChooseCoin,
    XrAskAmount(Asset),
    XrConfirm {
        amount: Money,
        random_id: i64,
    },
    XrCheck(String),
    /// Счёт CryptoBot в фиате: выбор монеты.
    CbInvoiceCoins(String),
    CbInvoiceConfirm {
        code: String,
        asset: Asset,
    },
    CbInvoiceTrust {
        code: String,
        asset: Asset,
    },
    /// «Выберите сеть для оплаты счёта» — денег на балансе не хватает.
    CbInvoiceNetwork(String),
    XrInvoice(String),
    XrInvoiceConfirm(String),
    XrInvoiceInsufficient(String),
    /// Экран без кнопок-команд (результат, уведомление).
    Static,
}

/// Черновик сообщения бота (без id и времени — их назначает мир при публикации).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Draft {
    pub text: String,
    pub entity_urls: Vec<String>,
    pub buttons: Vec<Vec<RawButton>>,
    pub via_bot: Option<Platform>,
    pub screen: Option<Screen>,
}

impl Draft {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn with_buttons(mut self, buttons: Vec<Vec<RawButton>>) -> Self {
        self.buttons = buttons;
        self
    }

    pub fn with_urls(mut self, urls: Vec<String>) -> Self {
        self.entity_urls = urls;
        self
    }

    pub fn on(mut self, screen: Screen) -> Self {
        self.screen = Some(screen);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Op {
    New(Draft),
    Edit(i32, Draft),
    /// Новое сообщение и сразу его правки: «🕘 Получение…» → «📥 Вы получили…», «💸» → «👌».
    Staged(Draft, Vec<Draft>),
}

impl Op {
    /// Сбои, которые подменяют ответ, работают с его последней версией.
    fn settled(self) -> Op {
        match self {
            Op::Staged(first, then) => Op::New(then.into_iter().last().unwrap_or(first)),
            other => other,
        }
    }
}

/// Сообщение бота, которое нужно опубликовать в чате аккаунта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outbound {
    pub account: AccountId,
    pub chat: Chat,
    pub op: Op,
}

/// Отложенная публикация (сбои `DelayReply` и `ProcessingFirst`).
#[derive(Debug)]
pub(crate) struct Delayed {
    pub delay: Duration,
    pub ops: Vec<Outbound>,
}

/// Состояние диалога аккаунта с ботом.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Dialog {
    #[default]
    Idle,
    /// Меню чеков: валюта выбрана, бот ждёт сумму. `menu` — сообщение меню (xRocket отвечает
    /// на сумму правкой его, CryptoBot — новым сообщением).
    AwaitCheckAmount { asset: Asset, menu: i32 },
    /// `/start` чека с паролем: бот ждёт пароль.
    AwaitCheckPassword(String),
}

#[derive(Debug, Clone)]
pub(crate) struct FaultRule {
    on: Call,
    fault: Fault,
    remaining: u32,
}

pub(crate) struct AccountState {
    pub username: String,
    pub premium: bool,
    pub channels: BTreeSet<String>,
    /// CryptoBot «Траты без код-пароля»: до этой суммы (USD за 24 часа, вместе с текущей
    /// операцией) код не спрашивают; `None` — спрашивают всегда. По умолчанию $500.
    pub passcode_free_limit: Option<Decimal>,
    /// Траты CryptoBot для лимита: (мс Unix, USD).
    pub cb_spends: Vec<(i64, Decimal)>,
    pub balances: BTreeMap<(Platform, Asset), Decimal>,
    next_msg_id: i32,
    pub chats: HashMap<Chat, Vec<RawMessage>>,
    pub random_ids: HashSet<i64>,
    pub dialogs: HashMap<Platform, Dialog>,
    faults: Vec<FaultRule>,
    pub tx: broadcast::Sender<RawMessage>,
}

impl AccountState {
    /// Как подписать аккаунт в уведомлениях: `@username` или `пользователь <id>`.
    pub fn display(&self, id: AccountId) -> String {
        if self.username.is_empty() {
            format!("пользователь {}", id.0)
        } else {
            format!("@{}", self.username)
        }
    }
}

pub(crate) struct WorldState {
    pub now_ms: i64,
    pub tick_ms: i64,
    rng: SplitMix64,
    next_user_id: i64,
    pub accounts: BTreeMap<AccountId, AccountState>,
    pub checks: BTreeMap<String, SimCheck>,
    pub check_order: Vec<String>,
    pub invoices: BTreeMap<String, SimInvoice>,
    pub invoice_order: Vec<String>,
    /// Что показывает каждое сообщение бота: (аккаунт, чат, id).
    pub screens: HashMap<(AccountId, Chat, i32), Screen>,
    /// CryptoBot: плательщик уже переводил этому получателю (иначе — «Вы уверены, что
    /// доверяете этому получателю?»). Получатель `None` — счёт внешнего магазина.
    pub trusted: HashSet<(AccountId, Option<AccountId>)>,
    next_payment: u64,
    /// Сколько единиц фиата стоит 1 единица актива: `(USDT, "USD") = 1`.
    pub fiat_rates: BTreeMap<(Asset, String), Decimal>,
    /// xRocket отвечает «не найден» на первый `/start` чека моложе этого окна (мс).
    pub xrocket_fresh_window_ms: Option<i64>,
}

impl WorldState {
    pub fn new(seed: u64) -> Self {
        let mut fiat_rates = BTreeMap::new();
        fiat_rates.insert((Asset::Usdt, "USD".to_owned()), Decimal::ONE);
        fiat_rates.insert((Asset::Ton, "USD".to_owned()), Decimal::from(5));
        Self {
            now_ms: START_MS,
            tick_ms: DEFAULT_TICK_MS,
            rng: SplitMix64(seed),
            next_user_id: FIRST_USER_ID,
            accounts: BTreeMap::new(),
            checks: BTreeMap::new(),
            check_order: Vec::new(),
            invoices: BTreeMap::new(),
            invoice_order: Vec::new(),
            screens: HashMap::new(),
            trusted: HashSet::new(),
            next_payment: 1,
            fiat_rates,
            xrocket_fresh_window_ms: None,
        }
    }

    pub fn tick(&mut self) {
        self.now_ms += self.tick_ms;
    }

    pub fn now_secs(&self) -> i64 {
        self.now_ms.div_euclid(1000)
    }

    // ---------- аккаунты ----------

    pub fn add_account(&mut self, username: &str) -> AccountId {
        let id = AccountId(self.next_user_id);
        self.next_user_id += 1;
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        self.accounts.insert(
            id,
            AccountState {
                username: username.trim_start_matches('@').to_owned(),
                premium: false,
                channels: BTreeSet::new(),
                passcode_free_limit: Some(Decimal::from(500)),
                cb_spends: Vec::new(),
                balances: BTreeMap::new(),
                next_msg_id: FIRST_MSG_ID,
                chats: HashMap::new(),
                random_ids: HashSet::new(),
                dialogs: HashMap::new(),
                faults: Vec::new(),
                tx,
            },
        );
        id
    }

    pub fn account(&self, id: AccountId) -> Result<&AccountState, SimError> {
        self.accounts.get(&id).ok_or(SimError::UnknownAccount(id))
    }

    pub fn account_mut(&mut self, id: AccountId) -> Result<&mut AccountState, SimError> {
        self.accounts
            .get_mut(&id)
            .ok_or(SimError::UnknownAccount(id))
    }

    pub fn display(&self, id: AccountId) -> String {
        self.accounts
            .get(&id)
            .map_or_else(|| format!("пользователь {}", id.0), |a| a.display(id))
    }

    pub fn dialog(&self, id: AccountId, platform: Platform) -> Dialog {
        self.accounts
            .get(&id)
            .and_then(|a| a.dialogs.get(&platform).cloned())
            .unwrap_or_default()
    }

    pub fn set_dialog(&mut self, id: AccountId, platform: Platform, dialog: Dialog) {
        if let Some(a) = self.accounts.get_mut(&id) {
            a.dialogs.insert(platform, dialog);
        }
    }

    // ---------- балансы ----------

    pub fn balance(&self, id: AccountId, platform: Platform, asset: Asset) -> Decimal {
        self.accounts
            .get(&id)
            .and_then(|a| a.balances.get(&(platform, asset)).copied())
            .unwrap_or(Decimal::ZERO)
    }

    pub fn balance_money(&self, id: AccountId, platform: Platform, asset: Asset) -> Money {
        Money::new(self.balance(id, platform, asset), asset).unwrap_or_else(|_| Money::zero(asset))
    }

    pub fn set_balance(
        &mut self,
        id: AccountId,
        platform: Platform,
        asset: Asset,
        amount: Decimal,
    ) -> Result<(), SimError> {
        if amount.is_sign_negative() && !amount.is_zero() {
            return Err(SimError::Invalid(format!("negative balance {amount}")));
        }
        self.account_mut(id)?
            .balances
            .insert((platform, asset), amount.normalize());
        Ok(())
    }

    pub fn credit(&mut self, id: AccountId, platform: Platform, m: Money) -> Result<(), SimError> {
        let a = self.account_mut(id)?;
        let slot = a
            .balances
            .entry((platform, m.asset()))
            .or_insert(Decimal::ZERO);
        *slot = (*slot + m.amount()).normalize();
        Ok(())
    }

    /// Списать; при нехватке — `InsufficientFunds`, баланс не меняется.
    pub fn debit(&mut self, id: AccountId, platform: Platform, m: Money) -> Result<(), SimError> {
        let available = self.balance_money(id, platform, m.asset());
        if available.amount() < m.amount() {
            return Err(SimError::InsufficientFunds {
                needed: m,
                available,
            });
        }
        let a = self.account_mut(id)?;
        let slot = a
            .balances
            .entry((platform, m.asset()))
            .or_insert(Decimal::ZERO);
        *slot = (*slot - m.amount()).normalize();
        if platform == Platform::CryptoBot {
            let usd = self.usd_value(&m);
            let now = self.now_ms;
            self.account_mut(id)?.cb_spends.push((now, usd));
        }
        Ok(())
    }

    /// Стоимость в USD по курсам мира (USDT = 1 USD); неизвестный курс — номинал.
    pub fn usd_value(&self, m: &Money) -> Decimal {
        let rate = self
            .fiat_rates
            .get(&(m.asset(), "USD".to_owned()))
            .copied()
            .unwrap_or(Decimal::ONE);
        m.amount() * rate
    }

    /// Спросит ли CryptoBot код-пароль: траты за 24 часа вместе с `m` выше лимита.
    pub fn passcode_needed(&self, id: AccountId, m: &Money) -> bool {
        let Ok(acc) = self.account(id) else {
            return true;
        };
        let Some(limit) = acc.passcode_free_limit else {
            return true;
        };
        let since = self.now_ms - 24 * 3600 * 1000;
        let spent: Decimal = acc
            .cb_spends
            .iter()
            .filter(|(at, _)| *at > since)
            .map(|(_, usd)| *usd)
            .sum();
        spent + self.usd_value(m) > limit
    }

    // ---------- реестры ----------

    fn random_code(&mut self, prefix: &str, tail: usize) -> String {
        loop {
            let mut code = String::with_capacity(prefix.len() + tail);
            code.push_str(prefix);
            for _ in 0..tail {
                let idx =
                    usize::try_from(self.rng.next_u64() % CODE_ALPHABET.len() as u64).unwrap_or(0);
                code.push(char::from(CODE_ALPHABET[idx]));
            }
            if !self.checks.contains_key(&code) && !self.invoices.contains_key(&code) {
                return code;
            }
        }
    }

    /// Положить чек в реестр; если у чека есть создатель — списать с него
    /// `amount × activations`. При нехватке денег чек не создаётся.
    pub fn insert_check(
        &mut self,
        spec: CheckSpec,
        origin: CheckOrigin,
    ) -> Result<SimCheck, SimError> {
        if spec.activations == 0 {
            return Err(SimError::Invalid("activations must be > 0".into()));
        }
        if spec.amount.amount().is_zero() {
            return Err(SimError::Invalid("check amount must be > 0".into()));
        }
        if spec.platform == Platform::XRocket && spec.password.is_some() {
            return Err(SimError::Invalid("xRocket checks have no passwords".into()));
        }
        let total = spec
            .amount
            .amount()
            .checked_mul(Decimal::from(spec.activations))
            .ok_or_else(|| SimError::Invalid("check total overflows".into()))?;
        let total =
            Money::new(total, spec.amount.asset()).map_err(|e| SimError::Invalid(e.to_string()))?;
        if let Some(creator) = spec.creator {
            self.debit(creator, spec.platform, total)?;
        }
        let code = match (spec.platform, spec.activations > 1) {
            (Platform::CryptoBot, _) => self.random_code("CQ", 10),
            (Platform::XRocket, false) => self.random_code("t_", 15),
            (Platform::XRocket, true) => self.random_code("mci_", 15),
        };
        let url = WalletLink {
            platform: spec.platform,
            kind: LinkKind::Check,
            param: code.clone(),
        }
        .canonical_url();
        let check = SimCheck {
            code: code.clone(),
            url,
            platform: spec.platform,
            amount: spec.amount,
            creator: spec.creator,
            for_user: spec.for_user,
            activations: spec.activations,
            claimed_by: Vec::new(),
            status: CheckStatus::Active,
            captcha: spec.captcha,
            password: spec.password,
            subscription: spec.subscription,
            premium_only: spec.premium_only,
            created_at_ms: self.now_ms,
            origin,
            fresh_not_found_served: false,
        };
        self.checks.insert(code.clone(), check.clone());
        self.check_order.push(code);
        Ok(check)
    }

    /// Создатель удалил чек: остаток возвращается ему, `/start` отвечает «не найден».
    pub fn cancel_check(&mut self, code: &str) -> Result<(), SimError> {
        let check = self
            .checks
            .get_mut(code)
            .ok_or_else(|| SimError::UnknownCheck(code.to_owned()))?;
        let refund = check.locked();
        check.status = CheckStatus::Cancelled;
        let (creator, platform, asset) = (check.creator, check.platform, check.amount.asset());
        if let (Some(c), Ok(m)) = (creator, Money::new(refund, asset)) {
            self.credit(c, platform, m)?;
        }
        Ok(())
    }

    pub fn insert_invoice(&mut self, spec: InvoiceSpec) -> Result<SimInvoice, SimError> {
        let positive = match &spec.amount {
            InvoiceAmount::Crypto(m) => !m.amount().is_zero(),
            InvoiceAmount::Fiat { amount, .. } => *amount > Decimal::ZERO,
        };
        if !positive {
            return Err(SimError::Invalid("invoice amount must be > 0".into()));
        }
        if spec.accepted_assets.is_empty() {
            return Err(SimError::Invalid("invoice accepts no assets".into()));
        }
        if let Some(c) = spec.creator {
            self.account(c)?;
        }
        let code = match spec.platform {
            Platform::CryptoBot => self.random_code("IV", 10),
            Platform::XRocket => self.random_code("inv_", 15),
        };
        let url = WalletLink {
            platform: spec.platform,
            kind: LinkKind::Invoice,
            param: code.clone(),
        }
        .canonical_url();
        let invoice = SimInvoice {
            code: code.clone(),
            url,
            platform: spec.platform,
            amount: spec.amount,
            accepted_assets: spec.accepted_assets,
            single_use: spec.single_use,
            description: spec.description,
            creator: spec.creator,
            status: SimInvoiceStatus::Active,
            payments: Vec::new(),
            created_at_ms: self.now_ms,
        };
        self.invoices.insert(code.clone(), invoice.clone());
        self.invoice_order.push(code);
        Ok(invoice)
    }

    /// Сумма к оплате счёта в активе: для фиата — по курсу мира, вверх до шага актива.
    pub fn invoice_due(&self, invoice: &SimInvoice, asset: Asset) -> Option<Money> {
        if !invoice.accepted_assets.contains(&asset) {
            return None;
        }
        match &invoice.amount {
            InvoiceAmount::Crypto(m) => (m.asset() == asset).then_some(*m),
            InvoiceAmount::Fiat { amount, currency } => {
                let rate = self.fiat_rates.get(&(asset, currency.clone()))?;
                if rate.is_zero() {
                    return None;
                }
                let raw = amount.checked_div(*rate)?;
                let step = Step::from_dp(asset_dp(asset))?;
                Money::new(step.ceil(raw), asset).ok()
            }
        }
    }

    /// Номер платежа xRocket: `i<номер счёта>_<n>`.
    pub fn next_payment(&mut self) -> u64 {
        let n = self.next_payment;
        self.next_payment += 1;
        n
    }

    /// Числовой id чека в callback-данных CryptoBot (`confirm-delete-check-63865902`).
    pub fn check_num(&self, code: &str) -> Option<u64> {
        let pos = self.check_order.iter().position(|c| c == code)?;
        u64::try_from(pos).ok().map(|p| CHECK_NUM_BASE + p)
    }

    pub fn check_by_num(&self, num: u64) -> Option<&SimCheck> {
        let pos = usize::try_from(num.checked_sub(CHECK_NUM_BASE)?).ok()?;
        self.checks.get(self.check_order.get(pos)?)
    }

    /// Номер счёта, который бот пишет в тексте (`#IV69544493`, `#INV7276063`); код ссылки другой.
    pub fn invoice_number(&self, code: &str) -> Option<String> {
        let pos = self.invoice_order.iter().position(|c| c == code)?;
        let n = INVOICE_NUM_BASE + u64::try_from(pos).ok()?;
        Some(match self.invoices.get(code)?.platform {
            Platform::CryptoBot => format!("IV{n}"),
            Platform::XRocket => format!("INV{n}"),
        })
    }

    pub fn screen(&self, id: AccountId, chat: Chat, msg_id: i32) -> Option<&Screen> {
        self.screens.get(&(id, chat, msg_id))
    }

    // ---------- сбои ----------

    pub fn inject(&mut self, id: AccountId, on: Call, fault: Fault, times: u32) {
        if let Some(a) = self.accounts.get_mut(&id) {
            a.faults.push(FaultRule {
                on,
                fault,
                remaining: times,
            });
        }
    }

    pub fn clear_faults(&mut self, id: AccountId) {
        if let Some(a) = self.accounts.get_mut(&id) {
            a.faults.clear();
        }
    }

    /// Первое подходящее правило сбоя; счётчик правила уменьшается.
    pub fn take_fault(&mut self, id: AccountId, call: Call) -> Option<Fault> {
        let a = self.accounts.get_mut(&id)?;
        let idx = a
            .faults
            .iter()
            .position(|r| r.remaining > 0 && r.on.matches(call))?;
        let rule = &mut a.faults[idx];
        rule.remaining -= 1;
        let fault = rule.fault.clone();
        if rule.remaining == 0 {
            a.faults.remove(idx);
        }
        Some(fault)
    }

    // ---------- сообщения ----------

    fn next_msg_id(&mut self, id: AccountId) -> Option<i32> {
        let a = self.accounts.get_mut(&id)?;
        let msg_id = a.next_msg_id;
        a.next_msg_id += 1;
        Some(msg_id)
    }

    fn build(&self, chat: Chat, msg_id: i32, out: bool, draft: Draft) -> RawMessage {
        RawMessage {
            chat,
            id: msg_id,
            out,
            date: self.now_secs(),
            edit_date: None,
            text: draft.text,
            entity_urls: draft.entity_urls,
            buttons: draft.buttons,
            via_bot: draft.via_bot,
        }
    }

    fn remember(&mut self, account: AccountId, chat: Chat, msg_id: i32, screen: Option<Screen>) {
        let key = (account, chat, msg_id);
        match screen {
            Some(s) => {
                self.screens.insert(key, s);
            }
            None => {
                self.screens.remove(&key);
            }
        }
    }

    /// Наше исходящее сообщение: в историю, но не в поток (как у отправившей сессии MTProto).
    pub fn store_outgoing(
        &mut self,
        id: AccountId,
        chat: Chat,
        draft: Draft,
    ) -> Option<RawMessage> {
        let msg_id = self.next_msg_id(id)?;
        let msg = self.build(chat, msg_id, true, draft);
        let a = self.accounts.get_mut(&id)?;
        a.chats.entry(chat).or_default().push(msg.clone());
        Some(msg)
    }

    /// Опубликовать сообщение бота: история + поток подписчиков аккаунта.
    pub fn publish(&mut self, ob: Outbound) -> Option<RawMessage> {
        let Outbound { account, chat, op } = ob;
        match op {
            Op::Staged(first, then) => {
                let mut shown = self.publish(Outbound {
                    account,
                    chat,
                    op: Op::New(first),
                })?;
                for draft in then {
                    shown = self.publish(Outbound {
                        account,
                        chat,
                        op: Op::Edit(shown.id, draft),
                    })?;
                }
                Some(shown)
            }
            Op::New(draft) => {
                let msg_id = self.next_msg_id(account)?;
                let screen = draft.screen.clone();
                let msg = self.build(chat, msg_id, false, draft);
                self.remember(account, chat, msg_id, screen);
                let a = self.accounts.get_mut(&account)?;
                a.chats.entry(chat).or_default().push(msg.clone());
                // Ошибка отправки = нет подписчиков; сообщение всё равно есть в истории.
                let _ = a.tx.send(msg.clone());
                Some(msg)
            }
            Op::Edit(msg_id, draft) => {
                let now = self.now_secs();
                let screen = draft.screen.clone();
                let a = self.accounts.get_mut(&account)?;
                let msg = a
                    .chats
                    .get_mut(&chat)?
                    .iter_mut()
                    .find(|m| m.id == msg_id && !m.out)?;
                msg.text = draft.text;
                msg.entity_urls = draft.entity_urls;
                msg.buttons = draft.buttons;
                msg.edit_date = Some(now);
                let edited = msg.clone();
                let _ = a.tx.send(edited.clone());
                self.remember(account, chat, msg_id, screen);
                Some(edited)
            }
        }
    }

    pub fn find_message(&self, id: AccountId, chat: Chat, msg_id: i32) -> Option<&RawMessage> {
        self.accounts
            .get(&id)?
            .chats
            .get(&chat)?
            .iter()
            .find(|m| m.id == msg_id)
    }

    fn last_incoming_id(&self, id: AccountId, chat: Chat) -> Option<i32> {
        self.accounts
            .get(&id)?
            .chats
            .get(&chat)?
            .iter()
            .rev()
            .find(|m| !m.out)
            .map(|m| m.id)
    }

    /// Опубликовать ответы бота с учётом сбоя, формирующего ответ. Сбой касается только
    /// сообщений в чате вызова (`own`); уведомления другим аккаунтам уходят как есть.
    /// Возвращает то, что нужно опубликовать позже.
    pub fn deliver(
        &mut self,
        own: (AccountId, Chat),
        outs: Vec<Outbound>,
        fault: Option<&Fault>,
    ) -> Option<Delayed> {
        let (mut mine, others): (Vec<_>, Vec<_>) = outs
            .into_iter()
            .partition(|o| o.account == own.0 && o.chat == own.1);
        for o in others {
            self.publish(o);
        }
        if mine.is_empty() {
            return None;
        }
        let platform = match own.1 {
            Chat::WalletBot(p) => p,
            Chat::SavedMessages => Platform::CryptoBot,
        };
        if matches!(
            fault,
            Some(Fault::UnknownReply(_) | Fault::EditInsteadOfNew | Fault::ProcessingFirst { .. })
        ) {
            let first = mine.remove(0);
            mine.insert(
                0,
                Outbound {
                    op: first.op.settled(),
                    ..first
                },
            );
        }
        match fault {
            Some(Fault::NoReply) => None,
            Some(Fault::DelayReply(d)) => Some(Delayed {
                delay: *d,
                ops: mine,
            }),
            Some(Fault::UnknownReply(text)) => {
                let first = &mut mine[0];
                let draft = Draft::text(text.clone());
                first.op = match &first.op {
                    Op::Edit(id, _) => Op::Edit(*id, draft),
                    Op::New(_) | Op::Staged(..) => Op::New(draft),
                };
                self.publish_all(mine);
                None
            }
            Some(Fault::EditInsteadOfNew) => {
                if let (Op::New(draft), Some(last)) =
                    (&mine[0].op, self.last_incoming_id(own.0, own.1))
                {
                    mine[0].op = Op::Edit(last, draft.clone());
                }
                self.publish_all(mine);
                None
            }
            Some(Fault::ProcessingFirst { final_after }) => {
                let first = mine.remove(0);
                let processing = Draft::text(texts::processing(platform));
                let final_op = match first.op {
                    Op::New(draft) => {
                        let shown = self.publish(Outbound {
                            account: first.account,
                            chat: first.chat,
                            op: Op::New(processing),
                        })?;
                        Op::Edit(shown.id, draft)
                    }
                    Op::Edit(id, draft) => {
                        self.publish(Outbound {
                            account: first.account,
                            chat: first.chat,
                            op: Op::Edit(id, processing),
                        });
                        Op::Edit(id, draft)
                    }
                    staged @ Op::Staged(..) => staged,
                };
                let mut rest = vec![Outbound {
                    account: first.account,
                    chat: first.chat,
                    op: final_op,
                }];
                rest.extend(mine);
                if final_after.is_zero() {
                    self.publish_all(rest);
                    None
                } else {
                    Some(Delayed {
                        delay: *final_after,
                        ops: rest,
                    })
                }
            }
            _ => {
                self.publish_all(mine);
                None
            }
        }
    }

    pub fn publish_all(&mut self, ops: Vec<Outbound>) {
        for o in ops {
            self.publish(o);
        }
    }
}

/// Шаг суммы актива в симуляторе: USDT — 0.01, TON — 0.0001.
pub(crate) fn asset_dp(asset: Asset) -> u32 {
    match asset {
        Asset::Usdt => 2,
        Asset::Ton => 4,
    }
}
