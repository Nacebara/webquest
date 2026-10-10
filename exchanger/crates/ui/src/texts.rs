//! Каталог текстов из `ru.toml` (CLAUDE.md, правило 16). Структура повторяет файл один в один,
//! `deny_unknown_fields` ловит опечатки в ключах, отсутствующий ключ — ошибка загрузки.
//! Встроенный каталог проверяется тестом, поэтому `Texts::builtin_ru` в бою не падает.

use domain::{Direction, OrderState, Platform};
use serde::Deserialize;

const BUILTIN_RU: &str = include_str!("../ru.toml");

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextsError {
    #[error("ru.toml: {0}")]
    Parse(String),
    /// Незакрытая или пустая фигурная скобка плейсхолдера.
    #[error("ru.toml: bad placeholder in {key}: {text:?}")]
    Placeholder { key: String, text: String },
}

/// Значение для каждого направления обмена.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerDirection<T> {
    pub xr_to_cb_check: T,
    pub cb_to_xr_check: T,
    pub pay_cb_invoice: T,
    pub pay_xr_invoice: T,
}

impl<T> PerDirection<T> {
    pub fn get(&self, direction: Direction) -> &T {
        match direction {
            Direction::XrToCbCheck => &self.xr_to_cb_check,
            Direction::CbToXrCheck => &self.cb_to_xr_check,
            Direction::PayCbInvoice => &self.pay_cb_invoice,
            Direction::PayXrInvoice => &self.pay_xr_invoice,
        }
    }
}

/// Значение для каждой платформы.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerPlatform<T> {
    pub cryptobot: T,
    pub xrocket: T,
}

impl<T> PerPlatform<T> {
    pub fn get(&self, platform: Platform) -> &T {
        match platform {
            Platform::CryptoBot => &self.cryptobot,
            Platform::XRocket => &self.xrocket,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub send: String,
    pub get: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectionStatusTexts {
    pub open: String,
    pub paused: String,
    pub off: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateTexts {
    pub new: String,
    pub awaiting_funds: String,
    pub intake_pending: String,
    pub intake_unknown: String,
    pub received: String,
    pub payout_pending: String,
    pub invoice_pay_pending: String,
    pub refund_pending: String,
    pub manual_review: String,
    pub completed: String,
    pub refunded: String,
    pub rejected: String,
    pub intake_failed: String,
    pub expired: String,
    pub cancelled: String,
    pub closed_manual: String,
}

impl StateTexts {
    pub fn get(&self, state: OrderState) -> &str {
        match state {
            OrderState::New => &self.new,
            OrderState::AwaitingFunds => &self.awaiting_funds,
            OrderState::IntakePending => &self.intake_pending,
            OrderState::IntakeUnknown => &self.intake_unknown,
            OrderState::Received => &self.received,
            OrderState::PayoutPending => &self.payout_pending,
            OrderState::InvoicePayPending => &self.invoice_pay_pending,
            OrderState::RefundPending => &self.refund_pending,
            OrderState::ManualReview => &self.manual_review,
            OrderState::Completed => &self.completed,
            OrderState::Refunded => &self.refunded,
            OrderState::Rejected => &self.rejected,
            OrderState::IntakeFailed => &self.intake_failed,
            OrderState::Expired => &self.expired,
            OrderState::Cancelled => &self.cancelled,
            OrderState::ClosedManual => &self.closed_manual,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuTexts {
    pub exchange: String,
    pub pay_invoice: String,
    pub rates: String,
    pub history: String,
    pub support: String,
    pub placeholder: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonTexts {
    pub cb_to_xr: String,
    pub xr_to_cb: String,
    pub howto: String,
    pub cancel: String,
    pub back: String,
    pub copy_order: String,
    pub claim_check: String,
    pub copy_link: String,
    pub copy_amount: String,
    pub pay_by_check: String,
    pub refund: String,
    pub support: String,
    pub write_operator: String,
    pub order_question: String,
    pub open_order: String,
    pub notify: String,
    pub more: String,
    pub open_wallet: String,
    pub not_rendered: String,
    pub again: String,
    pub privacy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTexts {
    pub start: String,
    pub terms: String,
    pub history: String,
    pub help: String,
    pub privacy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WelcomeTexts {
    pub title: String,
    pub lead: String,
    pub col_send: String,
    pub col_get: String,
    pub col_fee: String,
    pub row: String,
    pub limits: String,
    pub closed: String,
    pub howto_title: String,
    pub howto_cb: String,
    pub howto_xr: String,
    pub footer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermsTexts {
    pub title: String,
    pub col_direction: String,
    pub col_fee: String,
    pub col_min: String,
    pub col_max: String,
    pub col_status: String,
    pub row: String,
    pub checks_title: String,
    pub checks: Vec<String>,
    pub checks_bind: String,
    pub invoices_title: String,
    pub invoices: Vec<String>,
    pub rules_title: String,
    pub rules: String,
    pub footer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeTexts {
    pub title: String,
    pub lead: String,
    pub line: String,
    pub line_paused: String,
    pub footer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendCheckTexts {
    pub title: String,
    pub body: String,
    pub quick: String,
    pub rules: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HowtoPlatformTexts {
    pub steps: Vec<String>,
    pub menu: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HowtoTexts {
    pub title: String,
    pub quick_title: String,
    pub menu_title: String,
    pub rules: String,
    pub cryptobot: HowtoPlatformTexts,
    pub xrocket: HowtoPlatformTexts,
}

impl HowtoTexts {
    pub fn platform(&self, platform: Platform) -> &HowtoPlatformTexts {
        match platform {
            Platform::CryptoBot => &self.cryptobot,
            Platform::XRocket => &self.xrocket,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoicePromptTexts {
    pub title: String,
    pub body: String,
    pub line: String,
    pub line_paused: String,
    pub examples: String,
    pub rules: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderTexts {
    pub checking: String,
    pub accepted: String,
    pub creating: String,
    pub done: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceTexts {
    pub quote: String,
    pub description: String,
    pub fiat_line: String,
    pub awaiting: String,
    pub underpaid: String,
    pub paying: String,
    pub paid: String,
    pub excess_refund: String,
    pub excess_kept: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualTexts {
    pub review: String,
    pub review_unknown: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefundTexts {
    pub started: String,
    pub done: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefundReasonTexts {
    pub below_minimum: String,
    pub above_maximum: String,
    pub unsupported_asset: String,
    pub invoice_paid_elsewhere: String,
    pub invoice_expired: String,
    pub rate_moved: String,
    pub client_request: String,
    pub underpaid: String,
    pub operator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorTexts {
    pub not_check_or_invoice: String,
    pub foreign_bot: String,
    pub testnet: String,
    pub already_activated: String,
    pub not_found: String,
    pub not_for_us: String,
    pub not_for_us_bind: String,
    pub premium_only: String,
    pub subscription: String,
    pub captcha: String,
    pub password: String,
    pub multi_check: String,
    pub below_minimum: String,
    pub above_maximum: String,
    pub paused: String,
    pub paused_until: String,
    pub no_reserve: String,
    pub maintenance: String,
    pub maintenance_until: String,
    pub rate_limited: String,
    pub banned: String,
    pub banned_until: String,
    pub duplicate: String,
    pub invoice_unsuitable: String,
    pub payout_delayed: String,
    pub unsupported_asset: String,
    pub invoice_paid_elsewhere: String,
    pub rate_moved: String,
    pub quote_expired: String,
    pub internal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceProblemTexts {
    pub reusable: String,
    pub already_paid: String,
    pub expired: String,
    pub no_amount: String,
    pub unsupported_asset: String,
    pub expires_soon: String,
    pub unreadable: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryTexts {
    pub title: String,
    pub col_order: String,
    pub col_when: String,
    pub col_what: String,
    pub col_amount: String,
    pub col_result: String,
    pub row: String,
    pub footer: String,
    pub empty: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardTexts {
    pub title: String,
    pub created: String,
    pub direction: String,
    pub received: String,
    pub fee: String,
    pub paid_out: String,
    pub invoice: String,
    pub refunded: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpTexts {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyTexts {
    pub title: String,
    pub stored_title: String,
    pub stored: Vec<String>,
    pub why: String,
    pub never: String,
    pub footer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportTexts {
    pub with_contact: String,
    pub without_contact: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiscTexts {
    pub no_link: String,
    pub cancelled: String,
    pub cancelled_flow: String,
}

/// Все тексты клиентского бота.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Texts {
    pub wallets: PerPlatform<String>,
    pub directions: PerDirection<String>,
    pub offers: PerDirection<Offer>,
    pub direction_status: DirectionStatusTexts,
    pub states: StateTexts,
    pub menu: MenuTexts,
    pub buttons: ButtonTexts,
    pub commands: CommandTexts,
    pub welcome: WelcomeTexts,
    pub terms: TermsTexts,
    pub exchange: ExchangeTexts,
    pub send_check: SendCheckTexts,
    pub howto: HowtoTexts,
    pub invoice_prompt: InvoicePromptTexts,
    pub order: OrderTexts,
    pub invoice: InvoiceTexts,
    pub manual: ManualTexts,
    pub refund: RefundTexts,
    pub refund_reasons: RefundReasonTexts,
    pub errors: ErrorTexts,
    pub invoice_problems: InvoiceProblemTexts,
    pub history: HistoryTexts,
    pub card: CardTexts,
    pub help: HelpTexts,
    pub privacy: PrivacyTexts,
    pub support: SupportTexts,
    pub misc: MiscTexts,
}

impl Texts {
    /// Встроенный русский каталог (`crates/ui/ru.toml`).
    pub fn builtin_ru() -> Result<Self, TextsError> {
        Self::from_toml(BUILTIN_RU)
    }

    /// Каталог из TOML (например, правленный владельцем файл).
    pub fn from_toml(source: &str) -> Result<Self, TextsError> {
        let texts: Texts = toml::from_str(source).map_err(|e| TextsError::Parse(e.to_string()))?;
        let table: toml::Table =
            toml::from_str(source).map_err(|e| TextsError::Parse(e.to_string()))?;
        check_placeholders("", &toml::Value::Table(table))?;
        Ok(texts)
    }
}

/// Каждая `{` закрыта `}` и между ними — имя из `[a-z0-9_]`.
fn check_placeholders(key: &str, value: &toml::Value) -> Result<(), TextsError> {
    match value {
        toml::Value::String(text) => {
            if placeholders_well_formed(text) {
                Ok(())
            } else {
                Err(TextsError::Placeholder {
                    key: key.to_owned(),
                    text: text.clone(),
                })
            }
        }
        toml::Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(i, v)| check_placeholders(&format!("{key}[{i}]"), v)),
        toml::Value::Table(table) => table.iter().try_for_each(|(k, v)| {
            let path = if key.is_empty() {
                k.clone()
            } else {
                format!("{key}.{k}")
            };
            check_placeholders(&path, v)
        }),
        _ => Ok(()),
    }
}

pub(crate) fn placeholders_well_formed(text: &str) -> bool {
    let mut rest = text;
    while let Some(open) = rest.find(['{', '}']) {
        if rest[open..].starts_with('}') {
            return false;
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return false;
        };
        let name = &after[..close];
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return false;
        }
        rest = &after[close + 1..];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_loads() {
        let texts = Texts::builtin_ru().expect("встроенный ru.toml разбирается");
        assert_eq!(texts.wallets.get(Platform::XRocket), "xRocket");
        assert_eq!(texts.states.get(OrderState::Completed), "готово");
        assert!(!texts.howto.platform(Platform::CryptoBot).steps.is_empty());
    }

    #[test]
    fn typo_in_key_is_rejected() {
        let broken = BUILTIN_RU.replace("[misc]\nno_link", "[misc]\nno_lnk");
        assert!(matches!(
            Texts::from_toml(&broken),
            Err(TextsError::Parse(_))
        ));
    }

    #[test]
    fn broken_placeholder_is_rejected() {
        let broken = BUILTIN_RU.replace(
            "{e_wait} <b>Проверяю чек</b>",
            "{e_wait <b>Проверяю чек</b>",
        );
        assert!(matches!(
            Texts::from_toml(&broken),
            Err(TextsError::Placeholder { key, .. }) if key == "order.checking"
        ));
    }

    #[test]
    fn placeholder_syntax() {
        assert!(placeholders_well_formed("a {b} c {d_1}"));
        assert!(placeholders_well_formed("без плейсхолдеров"));
        for bad in ["{", "}", "{}", "{A}", "{a", "a}", "{a b}"] {
            assert!(!placeholders_well_formed(bad), "{bad}");
        }
    }
}
