//! Публичные типы мира симулятора: аккаунты, чеки, счета, сбои.

use std::time::Duration;

use domain::{Asset, Decimal, Money, Platform};

/// Аккаунт Telegram в мире симулятора (user id). Юзерботы и клиенты — одинаковые аккаунты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Можно активировать (у мульти-чека остались активации).
    Active,
    /// Все активации использованы.
    Claimed,
    /// Удалён создателем, деньги вернулись.
    Cancelled,
}

/// Откуда взялся чек — для проверок в тестах («чек создан ровно один раз на `random_id`»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOrigin {
    /// Помощник теста ([`super::SimWorld::create_check`]).
    Helper,
    /// Меню бота: `/checks` или `/cheques`. `random_id` — у сообщения с суммой.
    Menu { random_id: i64 },
    /// Инлайн-режим: `send_inline` с этим `random_id`.
    Inline { random_id: i64 },
}

/// Что положить в мир помощником [`super::SimWorld::create_check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckSpec {
    pub platform: Platform,
    /// Сумма одной активации.
    pub amount: Money,
    /// Создатель; `None` — внешний клиент: деньги приходят «извне», ничей баланс не списывается.
    pub creator: Option<AccountId>,
    /// Персональный чек: активировать может только этот аккаунт.
    pub for_user: Option<AccountId>,
    /// Число активаций; больше 1 — мульти-чек (у xRocket код `mci_…`).
    pub activations: u32,
    pub captcha: bool,
    /// Пароль чека (только CryptoBot).
    pub password: Option<String>,
    /// Канал, на который нужно подписаться (`@channel`).
    pub subscription: Option<String>,
    pub premium_only: bool,
}

impl CheckSpec {
    pub fn new(platform: Platform, amount: Money) -> Self {
        Self {
            platform,
            amount,
            creator: None,
            for_user: None,
            activations: 1,
            captcha: false,
            password: None,
            subscription: None,
            premium_only: false,
        }
    }

    /// Чек создаёт аккаунт мира: сумма списывается с его баланса.
    pub fn by(mut self, creator: AccountId) -> Self {
        self.creator = Some(creator);
        self
    }

    pub fn personal_for(mut self, user: AccountId) -> Self {
        self.for_user = Some(user);
        self
    }

    pub fn multi(mut self, activations: u32) -> Self {
        self.activations = activations;
        self
    }

    pub fn with_captcha(mut self) -> Self {
        self.captcha = true;
        self
    }

    pub fn with_password(mut self, password: &str) -> Self {
        self.password = Some(password.to_owned());
        self
    }

    pub fn with_subscription(mut self, channel: &str) -> Self {
        self.subscription = Some(channel.to_owned());
        self
    }

    pub fn premium_only(mut self) -> Self {
        self.premium_only = true;
        self
    }
}

/// Чек в реестре мира (снимок).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimCheck {
    /// Параметр `start`: `CQ…`, `t_…`, `mci_…`.
    pub code: String,
    /// Ссылка для клиента: `https://t.me/send?start=CQ…`, `https://t.me/xrocket?start=t_…`.
    pub url: String,
    pub platform: Platform,
    /// Сумма одной активации.
    pub amount: Money,
    pub creator: Option<AccountId>,
    pub for_user: Option<AccountId>,
    pub activations: u32,
    /// Кто уже активировал, по порядку.
    pub claimed_by: Vec<AccountId>,
    pub status: CheckStatus,
    pub captcha: bool,
    pub password: Option<String>,
    pub subscription: Option<String>,
    pub premium_only: bool,
    /// Время мира (мс) создания чека.
    pub created_at_ms: i64,
    pub origin: CheckOrigin,
    /// xRocket уже ответил «не найден» на этот свежий чек (сбой бывает один раз).
    pub fresh_not_found_served: bool,
}

impl SimCheck {
    /// Сколько денег ещё лежит в чеке (неиспользованные активации).
    pub fn locked(&self) -> Decimal {
        if self.status != CheckStatus::Active {
            return Decimal::ZERO;
        }
        let left = self.activations.saturating_sub(self.claimed_count());
        self.amount.amount() * Decimal::from(left)
    }

    fn claimed_count(&self) -> u32 {
        u32::try_from(self.claimed_by.len()).unwrap_or(u32::MAX)
    }
}

/// Исход активации чека — то же, что бот написал в ответ на `/start <код>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Received(Money),
    AlreadyActivated,
    NotFound,
    NotForYou,
    NeedsSubscription,
    Captcha,
    PasswordRequired,
    WrongPassword,
    PremiumOnly,
    /// Создатель открыл свой же чек: бот показывает его, но не зачисляет.
    OwnCheck,
}

/// Сумма счёта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvoiceAmount {
    Crypto(Money),
    /// Сумма в фиате (`USD`, `EUR`, …); к оплате — в активе по курсу мира.
    Fiat {
        amount: Decimal,
        currency: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimInvoiceStatus {
    Active,
    Paid,
    Expired,
}

/// Что положить в мир помощником [`super::SimWorld::create_invoice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceSpec {
    pub platform: Platform,
    pub amount: InvoiceAmount,
    /// Чем можно оплатить фиатный счёт. Для счёта в крипте — только его актив.
    pub accepted_assets: Vec<Asset>,
    /// `false` — многоразовый счёт (мульти-счёт).
    pub single_use: bool,
    pub description: Option<String>,
    /// Получатель денег; `None` — внешний продавец (деньги уходят из мира).
    pub creator: Option<AccountId>,
}

impl InvoiceSpec {
    pub fn crypto(platform: Platform, amount: Money) -> Self {
        Self {
            platform,
            accepted_assets: vec![amount.asset()],
            amount: InvoiceAmount::Crypto(amount),
            single_use: true,
            description: None,
            creator: None,
        }
    }

    pub fn fiat(platform: Platform, amount: Decimal, currency: &str, accepted: &[Asset]) -> Self {
        Self {
            platform,
            amount: InvoiceAmount::Fiat {
                amount,
                currency: currency.to_owned(),
            },
            accepted_assets: accepted.to_vec(),
            single_use: true,
            description: None,
            creator: None,
        }
    }

    pub fn multi_use(mut self) -> Self {
        self.single_use = false;
        self
    }

    pub fn description(mut self, text: &str) -> Self {
        self.description = Some(text.to_owned());
        self
    }

    pub fn by(mut self, creator: AccountId) -> Self {
        self.creator = Some(creator);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoicePayment {
    pub payer: AccountId,
    pub paid: Money,
    pub at_ms: i64,
}

/// Счёт в реестре мира (снимок).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimInvoice {
    /// Параметр `start`: `IV…` или `inv_…`.
    pub code: String,
    pub url: String,
    pub platform: Platform,
    pub amount: InvoiceAmount,
    pub accepted_assets: Vec<Asset>,
    pub single_use: bool,
    pub description: Option<String>,
    pub creator: Option<AccountId>,
    pub status: SimInvoiceStatus,
    pub payments: Vec<InvoicePayment>,
    pub created_at_ms: i64,
}

/// Итог шага PIN в мини-приложении CryptoBot ([`super::SimWorld::complete_webapp_payment`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAppPayment {
    /// Оплачено, списано столько.
    Paid(Money),
    /// Неверный PIN, деньги не тронуты.
    WrongPin {
        attempts_left: u32,
    },
    /// PIN введён неверно слишком много раз — сессия закрыта.
    Blocked,
    AlreadyPaid,
    Expired,
    InsufficientFunds {
        available: Money,
        needed: Money,
    },
    /// Сессию мини-приложения уже использовали (повторная отправка формы).
    SessionUsed,
    /// Свой же счёт оплатить нельзя.
    OwnInvoice,
}

/// Какой вызов транспорта ломать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Any,
    SendText,
    Press,
    InlineQuery,
    SendInline,
    OpenWebApp,
    History,
}

impl Call {
    pub(crate) fn matches(self, actual: Call) -> bool {
        self == Call::Any || self == actual
    }
}

/// Сбой для следующих N вызовов (см. [`super::SimWorld::inject`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Запрос не дошёл: `Timeout`, ничего не сделано, `random_id` не занят.
    TimeoutBeforeEffect,
    /// Запрос выполнен (сообщение отправлено, чек создан, ответ бота пришёл в поток),
    /// но вызов вернул `Timeout`.
    TimeoutAfterEffect,
    /// Разрыв до выполнения.
    Disconnected,
    /// Разрыв после выполнения.
    DisconnectedAfterEffect,
    /// `FLOOD_WAIT`, ничего не сделано.
    FloodWait(Duration),
    /// Ошибка RPC до выполнения (например, `PEER_ID_INVALID`).
    Rpc { code: i32, name: String },
    /// Ответ бота придёт в поток через это время (действие выполнено сразу).
    DelayReply(Duration),
    /// Сначала «Подождите, идёт обработка…», через `final_after` — правка того же
    /// сообщения финальным текстом.
    ProcessingFirst { final_after: Duration },
    /// Финальный ответ заменён незнакомым текстом (действие выполнено).
    UnknownReply(String),
    /// Бот правит своё прошлое сообщение в чате вместо нового ответа.
    EditInsteadOfNew,
    /// Бот молчит (действие выполнено).
    NoReply,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SimError {
    #[error("unknown account {0:?}")]
    UnknownAccount(AccountId),
    #[error("check not found: {0}")]
    UnknownCheck(String),
    #[error("invoice not found: {0}")]
    UnknownInvoice(String),
    #[error("insufficient funds: need {needed}, available {available}")]
    InsufficientFunds { needed: Money, available: Money },
    #[error("unknown webapp session: {0}")]
    UnknownSession(String),
    #[error("invalid: {0}")]
    Invalid(String),
}
