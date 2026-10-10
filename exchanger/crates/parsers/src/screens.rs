//! Экраны ботов кошельков, по которым юзербот выдаёт чеки и платит по счетам (DESIGN-v0.2,
//! Р4–Р5): инлайн-результаты `@send 10usdt` / `@xrocket 10`, меню `/checks` и `/cheques`,
//! ответ на создание чека, сумма к оплате на экране счёта.
//!
//! Словари собраны по симулятору (`userbot::sim::texts`) и описанию владельца. Настоящие тексты
//! кнопок и экранов — уточнить по скриншотам и записи ответов (HANDOVER §7, п. 5–6): новый
//! вариант — в словарь и в тест ниже. Правило для денег то же, что в [`crate::replies`]:
//! «чек создан» признаём только по явным признакам, иначе `Unknown`.

use domain::{Asset, Decimal, Platform};
use serde::{Deserialize, Serialize};

use crate::amount;
use crate::links;
use crate::replies::{self, ActivationReply, CreatedCheck, INSUFFICIENT, has, normalize};

/// Кнопка «Создать чек» в меню чеков обеих платформ.
pub const CREATE_CHECK_BUTTON: &[&str] = &[
    "создать чек",
    "новый чек",
    "create check",
    "create cheque",
    "new check",
    "new cheque",
];
/// xRocket `/cheques`: тип «Персональный» (одна активация).
pub const PERSONAL_BUTTON: &[&str] = &["персональн", "personal"];
/// Кнопка оплаты счёта.
pub const PAY_BUTTON: &[&str] = &["оплатить", "pay"];
/// Бот просит ввести сумму чека.
pub const AMOUNT_PROMPT: &[&str] = &["сумм", "amount"];

/// Вариант инлайн-ответа: чек, счёт, «недостаточно средств» или что-то другое.
const INLINE_INVOICE: &[&str] = &["счет", "запрос", "invoice", "request"];
const INLINE_CHECK: &[&str] = &["чек", "отправить", "check", "cheque", "send"];
/// Ответ меню на ввод суммы: чек создан.
const CREATED: &[&str] = &["создан", "created"];
/// Строка с суммой к оплате на экране счёта.
const DUE_LABELS: &[&str] = &["к оплате", "to pay", "amount due"];

/// Есть ли в тексте (после [`normalize`]) одна из подстрок словаря.
pub fn text_matches(text: &str, needles: &[&str]) -> bool {
    has(&normalize(text), needles)
}

/// Вариант из ответа на инлайн-запрос к боту кошелька.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InlineResultKind {
    /// «Отправить 10 USDT» / «Чек на 10 USDT» — отправка создаст чек на эту сумму.
    Check {
        amount: Decimal,
        asset: Asset,
    },
    /// «Запросить 10 USDT» — это счёт, не чек.
    Invoice,
    Insufficient,
    Other,
}

/// Что предлагает инлайн-результат. Сумма — из заголовка, если там её нет — из описания.
/// Признаки счёта и нехватки средств сильнее признаков чека.
pub fn classify_inline_result(
    platform: Platform,
    title: Option<&str>,
    description: Option<&str>,
) -> InlineResultKind {
    let norm = normalize(&format!(
        "{} {}",
        title.unwrap_or_default(),
        description.unwrap_or_default()
    ));
    if has(&norm, INLINE_INVOICE) {
        return InlineResultKind::Invoice;
    }
    if has(&norm, INSUFFICIENT) {
        return InlineResultKind::Insufficient;
    }
    if !has(&norm, INLINE_CHECK) {
        return InlineResultKind::Other;
    }
    let money = title
        .and_then(|t| amount::parse_money(t, platform))
        .or_else(|| description.and_then(|d| amount::parse_money(d, platform)));
    match money {
        Some((amount, asset)) => InlineResultKind::Check { amount, asset },
        None => InlineResultKind::Other,
    }
}

/// Ответ меню чеков на введённую сумму.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckCreationReply {
    Created(CreatedCheck),
    /// «Недостаточно средств» — чек точно не создан.
    InsufficientFunds,
    Unknown,
}

/// Разобрать ответ на сумму в меню чеков. «Создан» — только при явном слове «создан» и ровно
/// одной ссылке на чек этой платформы; уведомление «Ваш чек активировал…» сюда не подходит.
pub fn classify_check_creation(
    platform: Platform,
    text: &str,
    button_urls: &[String],
) -> CheckCreationReply {
    let norm = normalize(text);
    let insufficient = has(&norm, INSUFFICIENT);
    let created = has(&norm, CREATED)
        && replies::classify_activation(platform, text) != ActivationReply::ClaimNotice;
    let check = replies::parse_created_check(platform, text, button_urls);
    match (created, insufficient, check) {
        (true, false, Some(check)) => CheckCreationReply::Created(check),
        (false, true, None) => CheckCreationReply::InsufficientFunds,
        _ => CheckCreationReply::Unknown,
    }
}

/// Кнопка выбора актива: `USDT`, `💵 USDT`, `Tether (USDT)`; у TON — `TON` или `TONCOIN`.
pub fn is_asset_button(text: &str, asset: Asset) -> bool {
    let codes: &[&str] = match asset {
        Asset::Usdt => &["usdt"],
        Asset::Ton => &["ton", "toncoin"],
    };
    normalize(text)
        .split(|c: char| !c.is_alphanumeric())
        .any(|token| codes.contains(&token))
}

/// Сумма к оплате на экране счёта после выбора валюты: строка «К оплате: 10 USDT».
pub fn parse_amount_due(platform: Platform, text: &str) -> Option<(Decimal, Asset)> {
    text.lines()
        .filter(|line| has(&normalize(line), DUE_LABELS))
        .find_map(|line| amount::parse_money(&links::blank_urls(line), platform))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const CB: Platform = Platform::CryptoBot;
    const XR: Platform = Platform::XRocket;

    #[test]
    fn inline_results_are_told_apart() {
        assert_eq!(
            classify_inline_result(
                CB,
                Some("Отправить 97.5 USDT"),
                Some("Чек на 97.5 USDT. Доступно: 100 USDT")
            ),
            InlineResultKind::Check {
                amount: dec!(97.5),
                asset: Asset::Usdt
            }
        );
        assert_eq!(
            classify_inline_result(XR, Some("Чек на 0.5 TONCOIN"), None),
            InlineResultKind::Check {
                amount: dec!(0.5),
                asset: Asset::Ton
            }
        );
        assert_eq!(
            classify_inline_result(CB, Some("Запросить 10 USDT"), Some("Счёт на 10 USDT")),
            InlineResultKind::Invoice
        );
        assert_eq!(
            classify_inline_result(
                XR,
                Some("Недостаточно средств"),
                Some("Нужно 10 USDT, доступно 3 USDT")
            ),
            InlineResultKind::Insufficient
        );
        assert_eq!(
            classify_inline_result(CB, Some("Send 5 USDT"), Some("Check for 5 USDT")),
            InlineResultKind::Check {
                amount: dec!(5),
                asset: Asset::Usdt
            }
        );
        // Без суммы или без слова «чек» — не выбираем.
        assert_eq!(
            classify_inline_result(CB, Some("Отправить чек"), None),
            InlineResultKind::Other
        );
        assert_eq!(
            classify_inline_result(CB, Some("10 USDT"), None),
            InlineResultKind::Other
        );
    }

    #[test]
    fn check_creation_needs_word_and_single_link() {
        let url = "https://t.me/send?start=CQAbCdEfGhIj";
        let created = format!("🦋 Чек на 97.5 USDT создан.\n\nСсылка на чек: {url}");
        match classify_check_creation(CB, &created, &[url.to_owned()]) {
            CheckCreationReply::Created(c) => {
                assert_eq!(c.param, "CQAbCdEfGhIj");
                assert_eq!(c.amount, Some((dec!(97.5), Asset::Usdt)));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            classify_check_creation(CB, "Недостаточно средств. Доступно: 3 USDT.", &[]),
            CheckCreationReply::InsufficientFunds
        );
        // Ссылка без слова «создан» (список «Мои чеки», уведомление) — неизвестно.
        let notice = format!("Ваш чек активировал @client.\n\nЧек на 97.5 USDT — {url}");
        assert_eq!(
            classify_check_creation(CB, &notice, &[]),
            CheckCreationReply::Unknown
        );
        let list = format!("Активные чеки:\n\n• Чек на 97.5 USDT — {url}");
        assert_eq!(
            classify_check_creation(CB, &list, &[]),
            CheckCreationReply::Unknown
        );
        // Ссылка на чек другой платформы не считается.
        assert_eq!(
            classify_check_creation(XR, &created, &[url.to_owned()]),
            CheckCreationReply::Unknown
        );
        assert_eq!(
            classify_check_creation(CB, "Неверная сумма. Отправьте число, например 10.5.", &[]),
            CheckCreationReply::Unknown
        );
    }

    #[test]
    fn menu_buttons_and_prompts() {
        assert!(text_matches("➕ Создать чек", CREATE_CHECK_BUTTON));
        assert!(text_matches("Create check", CREATE_CHECK_BUTTON));
        assert!(!text_matches("Мои чеки", CREATE_CHECK_BUTTON));
        assert!(text_matches("👤 Персональный", PERSONAL_BUTTON));
        assert!(text_matches("Оплатить", PAY_BUTTON));
        assert!(text_matches(
            "Отправьте сумму чека в USDT.\n\nДоступно: 100 USDT.",
            AMOUNT_PROMPT
        ));
        assert!(!text_matches("Выберите валюту чека.", AMOUNT_PROMPT));
        assert!(is_asset_button("USDT", Asset::Usdt));
        assert!(is_asset_button("💵 Tether (USDT)", Asset::Usdt));
        assert!(is_asset_button("TONCOIN", Asset::Ton));
        assert!(!is_asset_button("TONCOIN", Asset::Usdt));
        assert!(!is_asset_button("USDTX", Asset::Usdt));
    }

    #[test]
    fn amount_due_is_taken_from_its_line() {
        let screen = "🧾 Счёт IVAbCdEfGhIj\n\nСумма: $10.00\nОдноразовый счёт\n\n\
                      К оплате: 10.05 USDT\nДоступно: 25 USDT";
        assert_eq!(
            parse_amount_due(CB, screen),
            Some((dec!(10.05), Asset::Usdt))
        );
        assert_eq!(parse_amount_due(CB, "Сумма: 10 USDT"), None);
        assert_eq!(
            parse_amount_due(XR, "Amount due: 0.5 TONCOIN"),
            Some((dec!(0.5), Asset::Ton))
        );
    }
}
