//! Экраны ботов кошельков, по которым юзербот выдаёт чеки и платит по счетам (DESIGN-v0.2,
//! Р4–Р5): меню `/checks` и `/cheques`, ответ на создание чека, экраны оплаты счёта.
//!
//! Словари — по живой записи владельца (6 ноября 2026, `tests/fixtures/live_*.json`): меню
//! чеков, экраны созданного чека, счета обоих кошельков. Новый вариант текста — фикстурой и
//! правилом, которое не пересекается с другими. Правило для денег то же, что в
//! [`crate::replies`]: «чек создан» и «оплачено» признаём только по явным признакам, иначе `Unknown`.

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
/// Кнопка оплаты счёта: CryptoBot «💳 Оплатить 5.006321 USDT», xRocket «Оплатить». Рядом у
/// CryptoBot — «Оплатить анонимно: Нет»: это переключатель, а не оплата ([`is_pay_button`]).
pub const PAY_BUTTON: &[&str] = &["оплатить", "pay"];
const PAY_TOGGLES: &[&str] = &["анонимно", "anonym", "еще раз", "again"];
/// CryptoBot: «💸 Отправить платеж получателю» — второе подтверждение для нового получателя.
pub const SEND_PAYMENT_BUTTON: &[&str] = &["отправить платеж", "send payment"];
/// xRocket: способ оплаты счёта «xRocket» (с баланса), а не «Адрес» (перевод в сеть).
pub const VIA_WALLET_BUTTON: &[&str] = &["xrocket"];
/// CryptoBot: «Пропустить ›» — одна активация вместо мультичека.
pub const SKIP_BUTTON: &[&str] = &["пропустить", "skip"];
/// xRocket: «✅ Подтверждаю» — подтверждение суммы чека.
pub const CONFIRM_BUTTON: &[&str] = &["подтверждаю", "i confirm"];

/// Кнопка, которая платит: «Оплатить…», но не «Оплатить анонимно» и не «Оплатить ещё раз».
pub fn is_pay_button(text: &str) -> bool {
    let norm = normalize(text);
    has(&norm, PAY_BUTTON) && !has(&norm, PAY_TOGGLES)
}
/// Бот просит ввести сумму чека.
pub const AMOUNT_PROMPT: &[&str] = &["сумм", "amount"];

/// Ответ меню на ввод суммы: чек создан.
const CREATED: &[&str] = &["создан", "created"];
/// Экран созданного чека: CryptoBot «Любой может активировать этот чек. Скопируйте ссылку…»,
/// xRocket «Ссылка на чек:».
const CHECK_SCREEN: &[&str] = &[
    "скопируйте ссылку",
    "любой может активировать",
    "ссылка на чек",
    "copy the link",
    "link to the check",
];
/// CryptoBot: «Пришлите количество активаций, чтобы создать мультичек (до 2 активаций).»
const ASK_ACTIVATIONS: &[&str] = &["количество активаций", "number of activations"];
/// xRocket: «Пожалуйста, подтвердите корректность данных:».
const CONFIRM_DATA: &[&str] = &["подтвердите корректность данных", "confirm the data"];
/// Строка суммы на экране подтверждения чека xRocket: «Сумма чека: 1 USDT (1.0$)».
const CHECK_AMOUNT_LABEL: &[&str] = &["сумма чека", "check amount"];

/// Есть ли в тексте (после [`normalize`]) одна из подстрок словаря.
pub fn text_matches(text: &str, needles: &[&str]) -> bool {
    has(&normalize(text), needles)
}

/// Ответ меню чеков на введённую сумму.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckCreationReply {
    Created(CreatedCheck),
    /// «Недостаточно средств» / «Недостаточно монет» — чек точно не создан.
    InsufficientFunds,
    /// CryptoBot: сумма укладывается в баланс дважды, бот предлагает мультичек — нажать
    /// «Пропустить ›». Чек ещё не создан.
    AskActivations,
    /// xRocket: «Сумма чека: 1 USDT … подтвердите корректность данных» — нажать «✅ Подтверждаю».
    /// Чек ещё не создан.
    Confirm {
        amount: Option<(Decimal, Asset)>,
    },
    Unknown,
}

/// Разобрать экран меню чеков после ввода суммы.
///
/// «Создан» — экран чека (явное слово «создан» или признаки экрана созданного чека) и ровно
/// одна ссылка на чек этой платформы; уведомление «Ваш чек активировал…» сюда не подходит.
pub fn classify_check_creation(
    platform: Platform,
    text: &str,
    button_urls: &[String],
) -> CheckCreationReply {
    let norm = normalize(text);
    let insufficient = has(&norm, INSUFFICIENT);
    let screen = (has(&norm, CREATED) || has(&norm, CHECK_SCREEN))
        && replies::classify_activation(platform, text) != ActivationReply::ClaimNotice;
    let check = replies::parse_created_check(platform, text, button_urls);
    if has(&norm, ASK_ACTIVATIONS) && check.is_none() && !insufficient {
        return CheckCreationReply::AskActivations;
    }
    if has(&norm, CONFIRM_DATA) && check.is_none() && !insufficient {
        return CheckCreationReply::Confirm {
            amount: labelled_money(platform, text, CHECK_AMOUNT_LABEL),
        };
    }
    match (screen, insufficient, check) {
        (true, false, Some(check)) => CheckCreationReply::Created(check),
        (false, true, None) => CheckCreationReply::InsufficientFunds,
        _ => CheckCreationReply::Unknown,
    }
}

/// Сумма из строки с подписью: «Сумма чека: 1 USDT (1.0$)», «Отправляете: 5.006321 USDT ($5)».
fn labelled_money(platform: Platform, text: &str, labels: &[&str]) -> Option<(Decimal, Asset)> {
    text.lines()
        .filter(|line| has(&normalize(line), labels))
        .find_map(|line| amount::parse_money(&links::blank_urls(line), platform))
}

// ---------------------------------------------------------------------------------------------
// Счета (живые экраны)
// ---------------------------------------------------------------------------------------------

/// Экран бота во время оплаты чужого счёта.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvoiceScreen {
    /// CryptoBot: «Выберите монету для оплаты счета #IV69544493 на сумму $5.» — счёт в фиате;
    /// сумма в монете — на её кнопке («USDT · 5.006321 USDT»).
    ChooseCoin {
        number: Option<String>,
        fiat: Option<(Decimal, String)>,
    },
    /// xRocket: «🧾 Счёт #INV7276063 … Сумма: 10 USDT … Выберите способ оплаты:».
    ChooseMethod {
        number: Option<String>,
        amount: Option<(Decimal, Asset)>,
    },
    /// Подтверждение с суммой: CryptoBot «Подтвердите оплату счёта … Отправляете: 5.006321 USDT»,
    /// xRocket «🔸 Подтвердите ваш платёж на 2 USDT (2.0$)». Кнопка — [`is_pay_button`].
    Confirm {
        number: Option<String>,
        amount: Option<(Decimal, Asset)>,
    },
    /// CryptoBot: «Вы уверены, что доверяете этому получателю?» — новый получатель; оплата —
    /// кнопкой «💸 Отправить платеж получателю».
    TrustRecipient,
    /// Денег не хватает: CryptoBot «Выберите сеть для оплаты счёта в USDT.» (предлагает
    /// пополнить), xRocket «🔺 Недостаточно средств на балансе.». Ничего не списано.
    InsufficientFunds,
    /// CryptoBot: «Пришлите сумму USDT для оплаты счёта.» — сумму выбирает плательщик: не платим.
    OpenAmount,
    /// «📥 Вы оплатили счёт #IV… на сумму 🪙 5.006321 USDT ($5).», «📋 Вы успешно оплатили счёт…».
    Paid {
        number: Option<String>,
        amount: Option<(Decimal, Asset)>,
    },
    AlreadyPaid,
    Expired,
    NotFound,
    /// Бот просит код-пароль: траты выше «лимита без код-пароля». Код не вводим никогда.
    PasscodeRequired,
    /// Промежуточное: «👌», «💸», «⏱ Загрузка». Ждём следующего сообщения.
    Progress,
    Unknown,
}

const CHOOSE_COIN: &[&str] = &["выберите монету для оплаты", "choose a coin to pay"];
const CHOOSE_METHOD: &[&str] = &["выберите способ оплаты", "choose a payment method"];
const CONFIRM_PAYMENT: &[&str] = &[
    "подтвердите оплату счета",
    "подтвердите ваш платеж",
    "confirm the payment",
    "confirm your payment",
];
/// Строка суммы на экране подтверждения: CryptoBot «Отправляете: 5.006321 USDT ($5)», xRocket
/// «Подтвердите ваш платёж на 2 USDT»; карточка xRocket «Сумма: 10 USDT (10.0$)».
const SENDING_LABEL: &[&str] = &[
    "отправляете",
    "подтвердите ваш платеж",
    "you send",
    "confirm your payment",
];
const AMOUNT_LABEL: &[&str] = &["сумма:", "amount:"];
const TRUST_RECIPIENT: &[&str] = &["доверяете этому получателю", "trust this recipient"];
/// CryptoBot предлагает пополнить баланс в сети — значит, на балансе не хватает.
const CHOOSE_NETWORK: &[&str] = &["выберите сеть для оплаты", "choose a network to pay"];
const OPEN_AMOUNT: &[&str] = &["пришлите сумму", "send the amount"];
const INVOICE_WORD: &[&str] = &["счет", "invoice"];
const NOT_FOUND: &[&str] = &["не найден", "not found", "не существует", "does not exist"];
/// Служебное «⏱ Загрузка» xRocket.
const LOADING: &[&str] = &["загрузка", "loading"];

/// Номер счёта в тексте: CryptoBot «#IV69544493», xRocket «#INV7276063».
fn invoice_number(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == '.' || c == ',')
        .filter_map(|w| w.strip_prefix('#'))
        .find(|w| {
            let digits = w.strip_prefix("INV").or_else(|| w.strip_prefix("IV"));
            digits.is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
}

/// Разобрать экран оплаты счёта (текст сообщения или его правки).
pub fn classify_invoice_screen(platform: Platform, text: &str) -> InvoiceScreen {
    let norm = normalize(text);
    let number = || invoice_number(text);
    if norm.is_empty() {
        // «👌» и «💸» — после нормализации пусто.
        return if text.trim().is_empty() {
            InvoiceScreen::Unknown
        } else {
            InvoiceScreen::Progress
        };
    }
    match replies::classify_invoice_payment(platform, text) {
        replies::InvoicePayReply::PasscodeRequired => return InvoiceScreen::PasscodeRequired,
        replies::InvoicePayReply::Paid => {
            return InvoiceScreen::Paid {
                number: number(),
                amount: amount::parse_money(&links::blank_urls(text), platform),
            };
        }
        replies::InvoicePayReply::AlreadyPaid => return InvoiceScreen::AlreadyPaid,
        replies::InvoicePayReply::Expired => return InvoiceScreen::Expired,
        replies::InvoicePayReply::InsufficientFunds | replies::InvoicePayReply::Unknown => {}
    }
    if has(&norm, TRUST_RECIPIENT) {
        InvoiceScreen::TrustRecipient
    } else if has(&norm, CHOOSE_NETWORK) || has(&norm, INSUFFICIENT) {
        InvoiceScreen::InsufficientFunds
    } else if has(&norm, CONFIRM_PAYMENT) {
        InvoiceScreen::Confirm {
            number: number(),
            amount: labelled_money(platform, text, SENDING_LABEL),
        }
    } else if has(&norm, CHOOSE_COIN) {
        let fiat = text
            .lines()
            .find(|line| has(&normalize(line), CHOOSE_COIN))
            .and_then(amount::parse_fiat);
        InvoiceScreen::ChooseCoin {
            number: number(),
            fiat,
        }
    } else if has(&norm, CHOOSE_METHOD) {
        InvoiceScreen::ChooseMethod {
            number: number(),
            amount: labelled_money(platform, text, AMOUNT_LABEL),
        }
    } else if has(&norm, OPEN_AMOUNT) && has(&norm, INVOICE_WORD) {
        InvoiceScreen::OpenAmount
    } else if has(&norm, NOT_FOUND) {
        InvoiceScreen::NotFound
    } else if has(&norm, LOADING) && norm.split(' ').count() <= 2 {
        InvoiceScreen::Progress
    } else {
        InvoiceScreen::Unknown
    }
}

/// Карточка счёта по первому экрану после `/start IV…` / `/start inv_…`.
///
/// - CryptoBot «Выберите монету…» — счёт в фиате: `fiat` из текста, `amount` — сумма в USDT с
///   кнопки монеты; счёт в USDT открывается сразу экраном подтверждения «Отправляете: …».
/// - xRocket «Счёт #INV… Сумма: 10 USDT … Выберите способ оплаты».
/// - «Выберите сеть для оплаты» / «Недостаточно средств» сразу — счёт есть, но нам не хватает
///   (`insufficient_funds`), сумма неизвестна.
/// - Незнакомый экран — разбор по словам статуса ([`replies::parse_invoice_card`]).
pub fn read_invoice_card(
    platform: Platform,
    text: &str,
    button_texts: &[String],
) -> replies::InvoiceCard {
    use replies::{InvoiceCard, InvoiceStatus};
    // Многоразовый счёт не оплачиваем (SPEC §4: при неясном исходе повтор — двойная оплата).
    // В живой записи такого счёта нет: признак — слово «многоразовый» / «мульти-счёт» на
    // экране, если бот его пишет (docs/RECORDING.md, шаг «многоразовый счёт»).
    let multi = has(&normalize(text), replies::MULTI_USE);
    let active = |number, amount, fiat| InvoiceCard {
        status: InvoiceStatus::Active,
        number,
        amount,
        fiat,
        single_use: Some(!multi),
        ..InvoiceCard::default()
    };
    match classify_invoice_screen(platform, text) {
        InvoiceScreen::ChooseCoin { number, fiat } => {
            let usdt = button_texts
                .iter()
                .find_map(|b| coin_option(platform, b, Asset::Usdt))
                .map(|v| (v, Asset::Usdt));
            active(number, usdt, fiat)
        }
        InvoiceScreen::ChooseMethod { number, amount }
        | InvoiceScreen::Confirm { number, amount } => active(number, amount, None),
        InvoiceScreen::InsufficientFunds => InvoiceCard {
            status: InvoiceStatus::Active,
            insufficient_funds: true,
            ..InvoiceCard::default()
        },
        InvoiceScreen::OpenAmount => InvoiceCard {
            status: InvoiceStatus::Active,
            single_use: Some(false),
            ..InvoiceCard::default()
        },
        InvoiceScreen::AlreadyPaid | InvoiceScreen::Paid { .. } => InvoiceCard {
            status: InvoiceStatus::Paid,
            ..InvoiceCard::default()
        },
        InvoiceScreen::Expired => InvoiceCard {
            status: InvoiceStatus::Expired,
            ..InvoiceCard::default()
        },
        InvoiceScreen::NotFound
        | InvoiceScreen::PasscodeRequired
        | InvoiceScreen::TrustRecipient => InvoiceCard::default(),
        InvoiceScreen::Progress | InvoiceScreen::Unknown => {
            replies::parse_invoice_card(platform, text)
        }
    }
}

/// Кнопка монеты на экране выбора: «USDT · 5.006321 USDT» → сумма к оплате в этой монете.
pub fn coin_option(platform: Platform, button_text: &str, asset: Asset) -> Option<Decimal> {
    if !is_asset_button(button_text, asset) {
        return None;
    }
    match amount::parse_money(button_text, platform) {
        Some((value, a)) if a == asset => Some(value),
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const CB: Platform = Platform::CryptoBot;
    const XR: Platform = Platform::XRocket;

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
}
