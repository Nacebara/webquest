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
    /// Пауза с причиной — короткая подпись для таблиц.
    pub paused_no_reserve: String,
    pub paused_maintenance: String,
    pub paused_technical: String,
    /// Пауза с причиной — фраза для «Обменять чек».
    pub why_no_reserve: String,
    pub why_maintenance: String,
    pub why_technical: String,
    /// Причина неизвестна.
    pub why_paused: String,
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
    /// «Помощь» — третья кнопка второго ряда (макет B1).
    pub help: String,
    /// Только у инвесторов (DESIGN-v0.2 Р18).
    pub invest: String,
    pub placeholder: String,
    /// Подписи кнопок прежних версий меню: у клиента может остаться старая клавиатура.
    pub legacy: Vec<LegacyMenu>,
}

/// Старая подпись кнопки меню и действие, которое она вызывает.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyMenu {
    pub text: String,
    pub action: String,
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
    /// «Инструкция» под «Пришлите чек» (макет B1).
    pub instruction: String,
    /// «Вернуть 50.00» при недоплате счёта.
    pub refund_amount: String,
    /// «Ещё счёт» после оплаты.
    pub another_invoice: String,
    /// B1: «Забрать» (зелёная, ссылка на чек), «Детали» / «Скрыть» — то же сообщение.
    pub claim: String,
    pub details: String,
    pub hide: String,
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
    /// «Простой вид» — в «Курс и лимиты» (макет B1).
    pub plain_view: String,
    pub rich_view: String,
    pub prev: String,
    pub next: String,
    pub page: String,
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
    /// Колонки таблицы: «Куда · Комиссия · USDT».
    pub col_to: String,
    pub col_fee: String,
    pub col_usdt: String,
    /// Одна строка на оба счёта: «Оплата счёта».
    pub invoice_row: String,
    pub paused: String,
    pub row: String,
    pub closed: String,
    pub footer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermsTexts {
    pub title: String,
    pub col_to: String,
    pub col_fee: String,
    pub col_usdt: String,
    pub paused: String,
    pub off: String,
    pub row: String,
    pub min_fee: String,
    pub new_user: String,
    pub updated: String,
    /// Комиссия со ступенями: «от 1,2 %».
    pub fee_from: String,
    pub tiers_title: String,
    pub tier_line: String,
    pub tier_first: String,
    pub tier_next: String,
    pub tier_sep: String,
    /// Выгодный курс встречного потока.
    pub promo: String,
    pub personal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeTexts {
    /// «Что меняем?» — один вопрос, две кнопки (макет B1).
    pub title: String,
    /// Кнопка направления: «CryptoBot → xRocket · 2.5 %».
    pub button: String,
    /// Направление на паузе — неактивная кнопка.
    pub button_paused: String,
    /// Строка под вопросом, только если что-то на паузе: почему (шаг 0.1 PLAN-v1).
    pub paused: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendCheckTexts {
    /// «🔷 CryptoBot → 🚀 xRocket · 1–500 USDT» — путь денег.
    pub head: String,
}

/// «Квитанция» — строки сумм столбиком (выбор владельца 8 ноября 2026).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptTexts {
    /// «Ваш чек CryptoBot».
    pub check: String,
    /// «Комиссия 2.5 %».
    pub fee: String,
    /// «Чек xRocket» — итог.
    pub payout: String,
    /// «Счёт CryptoBot».
    pub invoice: String,
    pub to_pay: String,
    pub got: String,
    pub short: String,
    /// «Скопируйте, чтобы создать чек ⬇️» — над кнопкой с командой.
    pub copy_hint: String,
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
    pub col_invoice: String,
    pub col_pay: String,
    pub col_fee: String,
    pub col_usdt: String,
    /// Строка таблицы в простом виде.
    pub row: String,
    /// То же, когда пределы сумм у счетов разные (колонка «USDT»).
    pub row_range: String,
    pub body: String,
    /// Подвал с пределами сумм — когда они одинаковые у всех открытых счетов.
    pub footer: String,
    /// Подвал без пределов: они в таблице или счета не принимаются.
    pub footer_bare: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderTexts {
    /// «Проверяю чек» — сумма чека ещё неизвестна.
    pub checking: String,
    /// «Проверяю чек на 100.00 USDT».
    pub checking_amount: String,
    /// «Отправляю 97.50 USDT в xRocket» — деньги клиента уже у нас.
    pub sending: String,
    /// Строка-успокоение через 20 секунд (макет: «Дольше обычного»).
    pub slow: String,
    /// B1 «Готово»: «97.50 USDT ждут вас в xRocket».
    pub done: String,
    /// B1 «Детали»: та же строка + цитата с разбивкой и номером заявки.
    pub details: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceTexts {
    /// «🧾 Счёт CryptoBot → 🚀 чек xRocket» — путь денег над квитанцией.
    pub head: String,
    /// «⏳ Цена держится до 21:08 · через 15 минут».
    pub expires: String,
    /// «через 15 минут» — запасной текст отсчёта (Telegram ведёт его сам).
    pub left: String,
    /// «минуту», «минуты», «минут».
    pub minute_forms: [String; 3],
    /// Описание счёта от продавца — курсивом под путём денег.
    pub description: String,
    /// Счёт в фиате — строка под путём денег: «Счёт на 4500 RUB: курс сдвинется …».
    pub fiat: String,
    pub awaiting: String,
    pub underpaid: String,
    pub paying: String,
    pub paid: String,
    /// Переплата вернулась чеком (кнопка «Забрать»).
    pub excess_refund: String,
    /// Переплата возвращается, чек ещё создаётся.
    pub excess_refund_pending: String,
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
    /// «Меньше минимума · от 1.00 USDT / Верну 0.50 чеком CryptoBot.» — до и после чека.
    pub below_minimum: String,
    /// Кнопка «Забрать 51.25 USDT».
    pub claim: String,
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
    pub check_used: String,
    pub below_minimum: String,
    pub above_maximum: String,
    pub paused: String,
    pub paused_until: String,
    pub no_reserve: String,
    pub maintenance: String,
    pub maintenance_until: String,
    pub daily_limit: String,
    pub rate_limited: String,
    pub banned: String,
    pub banned_until: String,
    pub duplicate: String,
    pub invoice_unsuitable: String,
    pub payout_delayed: String,
    pub unsupported_asset: String,
    pub invoice_paid_elsewhere: String,
    pub rate_moved: String,
    pub invoice_in_work: String,
    pub invoice_pending: String,
    pub invoice_check_busy: String,
    pub invoice_refused: String,
    pub invoice_paused: String,
    pub quote_expired: String,
    pub internal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceRefusalTexts {
    pub paused: String,
    pub maintenance: String,
    pub daily_limit: String,
    pub no_reserve: String,
    pub below_minimum: String,
    pub above_maximum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceProblemTexts {
    pub fiat: String,
    pub unavailable: String,
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
    pub col_when: String,
    pub col_what: String,
    pub col_result: String,
    pub row: String,
    /// Короткие направления: «CB → xR», «счёт CB».
    pub dir_cb_to_xr: String,
    pub dir_xr_to_cb: String,
    pub dir_pay_cb: String,
    pub dir_pay_xr: String,
    /// «Итог» не-денежных исходов.
    pub refund: String,
    pub working: String,
    pub failed: String,
    /// «Всего 23 обмена · 2 310 USDT».
    pub total: String,
    pub exchange_forms: [String; 3],
    /// «Когда»: «14:04» сегодня, «вчера», «2 ноя».
    pub yesterday: String,
    pub months: [String; 12],
    pub empty: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardTexts {
    pub title: String,
    /// «🟢 Готово · 5 ноя, 14:04 · 9 секунд».
    pub status: String,
    pub took: String,
    pub sent: String,
    pub fee: String,
    pub fee_value: String,
    pub got: String,
    pub invoice: String,
    pub refunded: String,
    pub check: String,
    pub check_ready: String,
    pub amount_wallet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpTexts {
    pub title: String,
    pub intro: String,
    pub footer: String,
    /// Разделы: заголовок (раскрывается тапом), абзацы, кнопка действия.
    pub exchange_title: String,
    pub exchange: Vec<String>,
    pub exchange_bind: String,
    pub invoice_title: String,
    pub invoice: Vec<String>,
    pub money_title: String,
    pub money: Vec<String>,
    pub rules_title: String,
    pub rules: Vec<String>,
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
    pub title: String,
    pub contact: String,
    pub sent: String,
    pub sent_no_order: String,
    pub unavailable: String,
    pub unavailable_contact: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiscTexts {
    pub no_link: String,
    pub cancelled: String,
    pub cancelled_flow: String,
    /// Ссылка на счёт, пока оплата счетов не подключена к движку.
    pub invoice_soon: String,
    /// Чек через инлайн-режим кошелька ещё создаётся — ссылка придёт правкой.
    pub check_pending: String,
    pub check_waiting: String,
    pub check_missing: String,
}

/// Раздел «💼 Инвестиции» (DESIGN-v0.2 Р18).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestTexts {
    pub title: String,
    pub balance: String,
    pub profit: String,
    pub terms: String,
    pub terms_no_lock: String,
    pub withdrawable: String,
    pub locked: String,
    pub empty: String,
    pub moves_title: String,
    pub move_line: String,
    pub kind_deposit: String,
    pub kind_withdrawal: String,
    pub status_requested: String,
    pub status_processing: String,
    pub status_done: String,
    pub status_rejected: String,
    pub status_failed: String,
    pub status_manual: String,
    pub check_link: String,

    pub deposit: String,
    pub deposit_accepted: String,
    pub deposit_done: String,
    pub deposit_manual: String,
    pub withdraw: String,
    pub withdraw_nothing: String,
    pub withdraw_amount: String,
    pub withdraw_auto_note: String,
    pub withdraw_owner_note: String,
    pub withdraw_requested: String,
    pub withdraw_processing: String,
    pub withdraw_done: String,
    pub withdraw_rejected: String,
    pub withdraw_failed: String,
    pub withdraw_manual: String,

    pub not_a_check: String,
    pub bad_amount: String,
    pub over_withdrawable: String,
    pub open_withdrawal: String,
    pub wallet_unavailable: String,
    pub internal: String,

    pub btn_deposit: String,
    pub btn_withdraw: String,
    pub btn_howto: String,
    pub btn_refresh: String,
    pub btn_all: String,
    pub btn_home: String,

    pub howto_title: String,
    pub howto_lead: String,
    pub howto_example_title: String,
    pub howto_col_what: String,
    pub howto_col_amount: String,
    pub howto_row: String,
    pub ex_deposit: String,
    pub ex_capital: String,
    pub ex_part: String,
    pub ex_turnover: String,
    pub ex_revenue: String,
    pub ex_on_you: String,
    pub ex_share: String,
    pub ex_day: String,
    pub ex_month: String,
    pub howto_example_note: String,
    pub howto_accrual_title: String,
    pub howto_accrual: Vec<String>,
    pub howto_withdraw_title: String,
    pub howto_withdraw_lock: String,
    pub howto_withdraw_no_lock: String,
    pub howto_withdraw_auto: String,
    pub howto_withdraw_owner: String,
    pub howto_withdraw_owner_all: String,
    pub howto_withdraw_one: String,
    pub howto_risks_title: String,
    pub howto_risks: Vec<String>,
    pub howto_footer: String,
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
    pub receipt: ReceiptTexts,
    pub howto: HowtoTexts,
    pub invoice_prompt: InvoicePromptTexts,
    pub order: OrderTexts,
    pub invoice: InvoiceTexts,
    pub manual: ManualTexts,
    pub refund: RefundTexts,
    pub refund_reasons: RefundReasonTexts,
    pub errors: ErrorTexts,
    pub invoice_problems: InvoiceProblemTexts,
    pub invoice_refusals: InvoiceRefusalTexts,
    pub history: HistoryTexts,
    pub card: CardTexts,
    pub help: HelpTexts,
    pub privacy: PrivacyTexts,
    pub support: SupportTexts,
    pub misc: MiscTexts,
    pub invest: InvestTexts,
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
        let broken = BUILTIN_RU.replace("\nno_link = ", "\nno_lnk = ");
        assert!(matches!(
            Texts::from_toml(&broken),
            Err(TextsError::Parse(_))
        ));
    }

    #[test]
    fn broken_placeholder_is_rejected() {
        let broken = BUILTIN_RU.replace(
            "checking = \"{icon} Проверяю чек\"",
            "checking = \"{icon Проверяю чек\"",
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
