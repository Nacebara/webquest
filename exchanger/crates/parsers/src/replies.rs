//! Классификация сообщений ботов кошельков (SPEC §1.3, §10.6). Словари — из lovec
//! `replies.rs` (подстроки в нижнем регистре после нормализации), расширенные под exch.
//! Неоднозначный или незнакомый текст — `Unknown`: движок ставит направление на паузу.

use domain::{Asset, Decimal, Platform};
use serde::{Deserialize, Serialize};

/// Ответ бота на `/start <чек>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivationReply {
    /// «Вы получили 100 USDT».
    Received {
        amount: Decimal,
        asset: Asset,
    },
    /// «Этот чек уже активирован».
    AlreadyActivated,
    /// «Чек не найден», «Мульти-чек не найден», «не существует».
    NotFound,
    /// «Вы не можете активировать этот чек» — чек персональный для другого.
    NotForYou,
    /// «Подпишитесь на канал».
    NeedsSubscription,
    Captcha,
    /// «Введите пароль» — чеки с паролем не принимаем (решение владельца).
    PasswordRequired,
    PremiumOnly,
    /// «Подождите, идёт обработка» — не финальный ответ, ждём дальше.
    InProgress,
    /// «Ваш чек активировал @…» — клиент забрал наш чек выплаты.
    ClaimNotice,
    /// «Вы получили 5 USDT от @x» — входящий перевод, НЕ ответ на активацию.
    IncomingTransfer {
        amount: Decimal,
        asset: Asset,
    },
    Unknown,
}

impl ActivationReply {
    /// Финальный ли это ответ на активацию (а не промежуточный или посторонний).
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self,
            ActivationReply::InProgress
                | ActivationReply::ClaimNotice
                | ActivationReply::IncomingTransfer { .. }
                | ActivationReply::Unknown
        )
    }
}

pub fn classify_activation(platform: Platform, text: &str) -> ActivationReply {
    let _ = (platform, text);
    ActivationReply::Unknown
}

/// Чек, созданный юзерботом (инлайн-сообщение в «Избранном» или ответ бота в меню чеков).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedCheck {
    /// Ссылка для клиента: `https://t.me/send?start=CQ…`, `https://t.me/xrocket?start=t_…`.
    pub url: String,
    pub param: String,
    pub amount: Option<(Decimal, Asset)>,
}

/// Найти созданный чек в тексте и кнопках сообщения.
pub fn parse_created_check(
    platform: Platform,
    text: &str,
    button_urls: &[String],
) -> Option<CreatedCheck> {
    let _ = (platform, text, button_urls);
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvoiceStatus {
    Active,
    Paid,
    Expired,
    Unknown,
}

/// Карточка чужого счёта после `/start IV…` или `/start inv_…`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceCard {
    pub status: InvoiceStatus,
    /// Сумма в активе, если счёт в крипте.
    pub amount: Option<(Decimal, Asset)>,
    /// Сумма в фиате и код валюты, если счёт в фиате.
    pub fiat: Option<(Decimal, String)>,
    /// `Some(false)` — многоразовый счёт: такие не оплачиваем.
    pub single_use: Option<bool>,
    pub description: Option<String>,
}

pub fn parse_invoice_card(platform: Platform, text: &str) -> InvoiceCard {
    let _ = (platform, text);
    InvoiceCard {
        status: InvoiceStatus::Unknown,
        amount: None,
        fiat: None,
        single_use: None,
        description: None,
    }
}

/// Результат нажатия «Оплатить».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvoicePayReply {
    Paid,
    AlreadyPaid,
    Expired,
    InsufficientFunds,
    Unknown,
}

pub fn classify_invoice_payment(platform: Platform, text: &str) -> InvoicePayReply {
    let _ = (platform, text);
    InvoicePayReply::Unknown
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceLine {
    pub asset: Asset,
    pub amount: Decimal,
}

/// Экран «Кошелёк» бота: доступные балансы по активам.
pub fn parse_balance(platform: Platform, text: &str) -> Vec<BalanceLine> {
    let _ = (platform, text);
    Vec::new()
}
