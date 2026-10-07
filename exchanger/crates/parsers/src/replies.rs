//! Классификация сообщений ботов кошельков (SPEC §1.3, §10.6). Словари — из lovec
//! `replies.rs` (подстроки в нижнем регистре после нормализации), расширенные под exch.
//! Неоднозначный или незнакомый текст — `Unknown`: движок ставит направление на паузу.
//!
//! Нормализация ([`normalize`]): NFKC, эмодзи и невидимые символы убраны, нижний регистр,
//! `ё` → `е`, пробелы схлопнуты. Суммы берутся из исходного текста ([`crate::amount`]).
//!
//! Правило для денег: ответ, который может означать «деньги ушли» или «деньги пришли», признаём
//! только по явным признакам. При сомнении — `Unknown` или промежуточный класс: ожидание
//! закончится таймаутом, и движок пойдёт в сверку, а не в повтор.

use std::sync::LazyLock;

use domain::{Asset, Decimal, Platform};
use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::amount::{self, Hit, Num};
use crate::links::{self, LinkKind, WalletLink};

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

// ---------------------------------------------------------------------------------------------
// Нормализация
// ---------------------------------------------------------------------------------------------

/// Невидимые символы: исчезают без следа (иначе «вы\u{200b}получили» не совпадёт).
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2060}'..='\u{2064}'
            | '\u{feff}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{20e3}'
            | '\u{1f3fb}'..='\u{1f3ff}'
            | '\u{e0000}'..='\u{e007f}'
            | '\u{e0100}'..='\u{e01ef}'
    )
}

/// Пиктограммы и эмодзи: заменяются пробелом («✅Вы получили» → «вы получили»).
fn is_emoji(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1F000..=0x1FAFF
            | 0x2600..=0x27BF
            | 0x2B00..=0x2BFF
            | 0x2190..=0x21FF
            | 0x2300..=0x23FF
            | 0x25A0..=0x25FF
            | 0x2900..=0x297F
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
    )
}

/// Текст для поиска по словарям: NFKC, без эмодзи и невидимых символов, нижний регистр,
/// `ё` → `е`, любые пробелы и переводы строк — один пробел, без пробелов по краям.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.nfkc() {
        if is_invisible(c) {
            continue;
        }
        if c.is_whitespace() || is_emoji(c) {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        for lc in c.to_lowercase() {
            out.push(if lc == 'ё' { 'е' } else { lc });
        }
    }
    out
}

fn has(norm: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| norm.contains(n))
}

// ---------------------------------------------------------------------------------------------
// Активация чека
// ---------------------------------------------------------------------------------------------

/// Словари lovec `replies.rs:41-90` (нормализованные, `ё` → `е`).
const WRONG_PASSWORD: &[&str] = &[
    "неверный пароль",
    "неправильный пароль",
    "wrong password",
    "incorrect password",
    "invalid password",
];
const LOST: &[&str] = &[
    "уже активирован",
    "уже получен",
    "already activated",
    "already been activated",
    "already claimed",
];
const WON: &[&str] = &[
    "успешно получили",
    "вы получили",
    "you received",
    "you have received",
    "successfully received",
];
const INVALID: &[&str] = &[
    "не найден",
    "не существует",
    "недоступен",
    "not found",
    "does not exist",
];
const CAPTCHA: &[&str] = &[
    "капч",
    "captcha",
    "символы, которые вы видите",
    "на картинке",
];
const NOT_FOR_US: &[&str] = &[
    "не можете активировать",
    "предназначен для другого",
    "not intended for you",
    "cannot activate",
];
const PASSWORD: &[&str] = &["пароль", "password"];
const NEEDS_JOIN: &[&str] = &["подпиш", "подписк", "subscribe", "join the"];
const IN_PROGRESS: &[&str] = &[
    "получение",
    "обработ",
    "processing",
    "подождите",
    "please wait",
];
/// Уведомление создателю чека (lovec `replies.rs:96-99`; английский вариант — для
/// `lang_code = "en"`, SPEC §1.3).
const CLAIM_SUBJECT: &[&str] = &["ваш чек", "your check", "your cheque"];
const CLAIM_VERB: &[&str] = &["активировал", "получил", "activated", "claimed", "received"];
/// Отправитель во входящем переводе: «Вы получили 5 USDT от @x» (lovec AUDIT.md:82).
const FROM_SENDER: &[&str] = &["от @", "from @", "от пользователя", "from user"];
/// Чек только для Telegram Premium (SPEC §1.3).
const PREMIUM: &[&str] = &["premium", "премиум"];

/// Классифицировать ответ бота на `/start <чек>`.
///
/// Порядок — как в lovec, с дополнениями exch (в lovec уведомление «ваш чек …» проверялось
/// до неверного пароля; здесь неверный пароль — первым, тексты этих классов не пересекаются):
/// 1. неверный пароль — раньше всего (в нём есть «пароль», но это не новый запрос пароля);
/// 2. «ваш чек» + «активировал»/«получил» — уведомление о нашем чеке (`ClaimNotice`);
/// 3. «вы получили … от @» — входящий перевод, а не выигрыш (ловушка lovec AUDIT.md:82);
/// 4. уже активирован; 5. только Premium (если нет фразы получения); 6. не для вас;
/// 7. получили — `Received` только с суммой в поддерживаемом активе, иначе `Unknown`;
/// 8. не найден; 9. капча; 10. пароль; 11. подписка; 12. обработка; иначе `Unknown`.
pub fn classify_activation(platform: Platform, text: &str) -> ActivationReply {
    let t = normalize(text);
    if t.is_empty() {
        return ActivationReply::Unknown;
    }
    let money = || amount::parse_money(&links::blank_urls(text), platform);

    if has(&t, WRONG_PASSWORD) {
        return ActivationReply::PasswordRequired;
    }
    if has(&t, CLAIM_SUBJECT) && has(&t, CLAIM_VERB) {
        return ActivationReply::ClaimNotice;
    }
    let won = has(&t, WON);
    if won && has(&t, FROM_SENDER) {
        return match money() {
            Some((amount, asset)) => ActivationReply::IncomingTransfer { amount, asset },
            None => ActivationReply::Unknown,
        };
    }
    if has(&t, LOST) {
        ActivationReply::AlreadyActivated
    } else if !won && has(&t, PREMIUM) {
        ActivationReply::PremiumOnly
    } else if has(&t, NOT_FOR_US) {
        ActivationReply::NotForYou
    } else if won {
        match money() {
            Some((amount, asset)) => ActivationReply::Received { amount, asset },
            None => ActivationReply::Unknown,
        }
    } else if has(&t, INVALID) {
        ActivationReply::NotFound
    } else if has(&t, CAPTCHA) {
        ActivationReply::Captcha
    } else if has(&t, PASSWORD) {
        ActivationReply::PasswordRequired
    } else if has(&t, NEEDS_JOIN) {
        ActivationReply::NeedsSubscription
    } else if has(&t, IN_PROGRESS) {
        ActivationReply::InProgress
    } else {
        ActivationReply::Unknown
    }
}

// ---------------------------------------------------------------------------------------------
// Созданный чек
// ---------------------------------------------------------------------------------------------

/// Чек, созданный юзерботом (инлайн-сообщение в «Избранном» или ответ бота в меню чеков).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedCheck {
    /// Ссылка для клиента: `https://t.me/send?start=CQ…`, `https://t.me/xrocket?start=t_…`.
    pub url: String,
    pub param: String,
    pub amount: Option<(Decimal, Asset)>,
}

/// Найти созданный чек в тексте и кнопках сообщения.
///
/// Берём ссылки на чек этой платформы из кнопок и текста. Если найдено несколько **разных**
/// чеков — `None`: какой из них наш, по сообщению не понять. `url` — каноническая ссылка
/// ([`WalletLink::canonical_url`]), сумма — из текста, если есть.
pub fn parse_created_check(
    platform: Platform,
    text: &str,
    button_urls: &[String],
) -> Option<CreatedCheck> {
    let from_buttons = links::extract_from_message("", &[], button_urls);
    let from_text = links::extract_from_message(text, &[], &[]);
    let mut found: Option<WalletLink> = None;
    for link in from_buttons.into_iter().chain(from_text).flatten() {
        if link.platform != platform || link.kind != LinkKind::Check {
            continue;
        }
        match &found {
            None => found = Some(link),
            Some(f) if f.param == link.param => {}
            Some(_) => return None,
        }
    }
    let link = found?;
    Some(CreatedCheck {
        url: link.canonical_url(),
        amount: amount::parse_money(&links::blank_urls(text), platform),
        param: link.param,
    })
}

// ---------------------------------------------------------------------------------------------
// Счета
// ---------------------------------------------------------------------------------------------

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

/// Отрицание прямо перед словом статуса: «не оплачен», «not paid», «not yet paid».
const NEGATION: &str = r"(?P<neg>не\s+|еще\s+не\s+|not\s+(?:yet\s+|been\s+)?)?";

fn status_regex(words: &str) -> Regex {
    // Шаблоны постоянные и покрыты тестами: ошибка здесь — ошибка сборки, а не данных.
    #[allow(clippy::expect_used)]
    Regex::new(&format!(r"\b{NEGATION}(?:{words})\b")).expect("status regex is valid")
}

static PAID_WORDS: LazyLock<Regex> = LazyLock::new(|| status_regex(r"оплачен[аоы]?|paid"));
static EXPIRED_WORDS: LazyLock<Regex> =
    LazyLock::new(|| status_regex(r"истек(?:ла|ло|ли)?|просрочен[аоы]?|expired"));
static ACTIVE_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    status_regex(
        r"активен|активна|активный|active|ожидает оплаты|awaiting payment|waiting for payment",
    )
});

/// Есть ли слово статуса без отрицания перед ним.
fn affirmed(norm: &str, re: &Regex) -> bool {
    re.captures_iter(norm).any(|c| c.name("neg").is_none())
}

const MULTI_USE: &[&str] = &[
    "многоразов",
    "multi",
    "reusable",
    "multiple payments",
    "неограниченн",
];
const SINGLE_USE: &[&str] = &[
    "одноразов",
    "single-use",
    "single use",
    "one-time",
    "one time",
];
const DESCRIPTION_LABELS: [&str; 2] = ["описание", "description"];

/// Разобрать карточку счёта. Осторожно: статус — только если в тексте ровно один из
/// «активен» / «оплачен» / «истёк» без отрицания, иначе `Unknown`.
///
/// Сумма: если первой в тексте стоит сумма в активе («5 USDT ($5.00)») — это счёт в крипте,
/// `fiat = None`; если первой стоит фиатная («$10.00 … 10.05 USDT») — счёт в фиате,
/// `amount = None`.
pub fn parse_invoice_card(platform: Platform, text: &str) -> InvoiceCard {
    let norm = normalize(text);
    let paid = affirmed(&norm, &PAID_WORDS);
    let expired = affirmed(&norm, &EXPIRED_WORDS);
    let active = affirmed(&norm, &ACTIVE_WORDS);
    let status = match (active, paid, expired) {
        (true, false, false) => InvoiceStatus::Active,
        (false, true, false) => InvoiceStatus::Paid,
        (false, false, true) => InvoiceStatus::Expired,
        _ => InvoiceStatus::Unknown,
    };

    let clean = links::blank_urls(text);
    let money = amount::find_money(&clean, platform);
    let fiat = amount::find_fiat(&clean);
    let (amount, fiat) = match (money, fiat) {
        (Some(m), Some(f)) if f.at < m.at => (None, f.value),
        (Some(m), Some(_)) => (m.value, None),
        (m, f) => (m.and_then(|h| h.value), f.and_then(|h| h.value)),
    };

    let single_use = match (has(&norm, MULTI_USE), has(&norm, SINGLE_USE)) {
        (true, false) => Some(false),
        (false, true) => Some(true),
        _ => None,
    };

    InvoiceCard {
        status,
        amount,
        fiat,
        single_use,
        description: description(text),
    }
}

/// Строка «Описание: …» (или значение на следующей строке, если после двоеточия пусто).
fn description(text: &str) -> Option<String> {
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let norm = normalize(line);
        let labelled = DESCRIPTION_LABELS.iter().any(|label| {
            norm.strip_prefix(label)
                .is_some_and(|rest| rest.trim_start().starts_with(':'))
        });
        if !labelled {
            continue;
        }
        let value = line.split_once(':').map_or("", |(_, v)| v).trim();
        if !value.is_empty() {
            return Some(value.to_owned());
        }
        return lines
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_owned());
    }
    None
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

const ALREADY_PAID: &[&str] = &[
    "уже оплачен",
    "уже оплатили",
    "already paid",
    "already been paid",
];
const INSUFFICIENT: &[&str] = &["недостаточно", "не хватает", "insufficient", "not enough"];
/// Только явные фразы успешной оплаты: голое «Счёт оплачен» может означать и «оплачен
/// кем-то раньше» — тогда `Unknown`, и движок идёт в сверку.
const PAID_NOW: &[&str] = &[
    "вы оплатили",
    "вы успешно оплатили",
    "успешно оплачен",
    "оплата прошла",
    "оплата выполнена",
    "you paid",
    "you have paid",
    "you have successfully paid",
    "successfully paid",
    "paid successfully",
    "payment successful",
    "payment was successful",
    "payment completed",
];

/// Классифицировать ответ на «Оплатить». Признак ровно одного класса — этот класс;
/// ни одного или несколько — `Unknown`.
pub fn classify_invoice_payment(platform: Platform, text: &str) -> InvoicePayReply {
    let _ = platform;
    let t = normalize(text);
    let classes = [
        (has(&t, ALREADY_PAID), InvoicePayReply::AlreadyPaid),
        (has(&t, INSUFFICIENT), InvoicePayReply::InsufficientFunds),
        (affirmed(&t, &EXPIRED_WORDS), InvoicePayReply::Expired),
        (has(&t, PAID_NOW), InvoicePayReply::Paid),
    ];
    let mut hits = classes
        .iter()
        .filter(|(hit, _)| *hit)
        .map(|(_, class)| *class);
    match (hits.next(), hits.next()) {
        (Some(class), None) => class,
        _ => InvoicePayReply::Unknown,
    }
}

// ---------------------------------------------------------------------------------------------
// Баланс
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceLine {
    pub asset: Asset,
    pub amount: Decimal,
}

/// Экран «Кошелёк» бота: доступные балансы по активам.
///
/// Построчно: `Tether: 10.5 USDT` или `USDT: 10.5`. Неясное число в строке — строка
/// пропускается. Актив, который встретился дважды с разными суммами, не возвращается вовсе:
/// лучше «баланс неизвестен», чем неверный.
pub fn parse_balance(platform: Platform, text: &str) -> Vec<BalanceLine> {
    let mut out: Vec<BalanceLine> = Vec::new();
    let mut conflicted: Vec<Asset> = Vec::new();
    for line in text.lines() {
        let Some((asset, amount)) = balance_in_line(line, platform) else {
            continue;
        };
        if conflicted.contains(&asset) {
            continue;
        }
        match out.iter().position(|b| b.asset == asset) {
            None => out.push(BalanceLine { asset, amount }),
            Some(i) if out[i].amount == amount => {}
            Some(i) => {
                out.remove(i);
                conflicted.push(asset);
            }
        }
    }
    out
}

fn balance_in_line(line: &str, platform: Platform) -> Option<(Asset, Decimal)> {
    match amount::find_money(line, platform) {
        Some(Hit {
            value: Some((v, asset)),
            ..
        }) => return Some((asset, v)),
        Some(Hit { value: None, .. }) => return None,
        None => {}
    }
    // «USDT: 10.5», «TON (Toncoin): 1,5».
    for (at, len, asset) in amount::code_occurrences(line, platform) {
        let rest = line[at + len..].trim_start_matches(|c: char| {
            amount::is_hspace(c) || matches!(c, ':' | ')' | '=' | '—' | '–')
        });
        match amount::number_after(rest) {
            Num::Absent => continue,
            Num::Unclear => return None,
            Num::Value(v) => return Some((asset, v)),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const CB: Platform = Platform::CryptoBot;
    const XR: Platform = Platform::XRocket;

    fn received(amount: Decimal, asset: Asset) -> ActivationReply {
        ActivationReply::Received { amount, asset }
    }

    #[test]
    fn normalization() {
        assert_eq!(
            normalize("  ✅Вы\u{a0}ПОЛУЧИЛИ\n\n1 USDT 🎉 "),
            "вы получили 1 usdt"
        );
        assert_eq!(normalize("Идёт\u{200b} обработка"), "идет обработка");
        assert_eq!(normalize("ｆｕｌｌ"), "full");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn every_activation_class() {
        use ActivationReply as R;
        let cases: &[(Platform, &str, ActivationReply)] = &[
            (
                CB,
                "Вы получили 0.5 USDT ($0.50).",
                received(dec!(0.5), Asset::Usdt),
            ),
            (XR, "Вы получили\n1 USDT", received(dec!(1), Asset::Usdt)),
            (CB, "YOU RECEIVED 5 TON", received(dec!(5), Asset::Ton)),
            (
                XR,
                "✅ Вы успешно получили 2,5 TONCOIN",
                received(dec!(2.5), Asset::Ton),
            ),
            (CB, "Этот чек уже активирован.", R::AlreadyActivated),
            (
                CB,
                "This check has already been activated",
                R::AlreadyActivated,
            ),
            (CB, "Чек не найден", R::NotFound),
            (XR, "Мульти-чек не найден.", R::NotFound),
            (CB, "Check not found", R::NotFound),
            (CB, "Вы не можете активировать этот чек", R::NotForYou),
            (CB, "This check is not intended for you", R::NotForYou),
            (CB, "Подпишитесь на канал", R::NeedsSubscription),
            (CB, "Join the channel to activate", R::NeedsSubscription),
            (
                XR,
                "Введите символы, которые вы видите на картинке",
                R::Captcha,
            ),
            (XR, "Solve the captcha", R::Captcha),
            (CB, "Введите пароль от чека", R::PasswordRequired),
            (CB, "Неверный пароль, попробуйте ещё", R::PasswordRequired),
            (CB, "Enter the password", R::PasswordRequired),
            (
                CB,
                "Этот чек только для пользователей Telegram Premium",
                R::PremiumOnly,
            ),
            (
                CB,
                "Вы не можете активировать этот чек: он только для Premium",
                R::PremiumOnly,
            ),
            (
                XR,
                "Чек доступен только с премиум-подпиской",
                R::PremiumOnly,
            ),
            (CB, "Подождите, идёт обработка", R::InProgress),
            (CB, "Processing, please wait", R::InProgress),
            (CB, "Ваш чек активировал @someone", R::ClaimNotice),
            (CB, "🦋 Ваш чек на 5 USDT получил @someone", R::ClaimNotice),
            (CB, "Your check was activated by @someone", R::ClaimNotice),
            (CB, "что-то новое", R::Unknown),
            (CB, "", R::Unknown),
            (CB, "🦋", R::Unknown),
        ];
        for (platform, text, expected) in cases {
            assert_eq!(&classify_activation(*platform, text), expected, "{text:?}");
        }
    }

    #[test]
    fn incoming_transfer_is_not_a_win() {
        assert_eq!(
            classify_activation(CB, "Вы получили 5 USDT от @x"),
            ActivationReply::IncomingTransfer {
                amount: dec!(5),
                asset: Asset::Usdt
            }
        );
        assert_eq!(
            classify_activation(CB, "You received 5 USDT from @x"),
            ActivationReply::IncomingTransfer {
                amount: dec!(5),
                asset: Asset::Usdt
            }
        );
        // Без суммы — не угадываем.
        assert_eq!(
            classify_activation(CB, "Вы получили перевод от @x"),
            ActivationReply::Unknown
        );
    }

    #[test]
    fn win_without_clear_amount_is_unknown() {
        for text in [
            "Вы получили подарок",
            "Вы получили 0.001 BTC",
            "Вы получили 1,234 TON",
            "Вы получили $5.00",
        ] {
            assert_eq!(
                classify_activation(CB, text),
                ActivationReply::Unknown,
                "{text}"
            );
        }
        // Реклама Premium рядом с выигрышем не превращает его в отказ.
        assert_eq!(
            classify_activation(CB, "Вы получили 3 USDT. Купите Telegram Premium в @send"),
            received(dec!(3), Asset::Usdt)
        );
    }

    #[test]
    fn terminal_set() {
        assert!(received(dec!(1), Asset::Usdt).is_terminal());
        assert!(ActivationReply::PasswordRequired.is_terminal());
        assert!(!ActivationReply::InProgress.is_terminal());
        assert!(!ActivationReply::ClaimNotice.is_terminal());
        assert!(!ActivationReply::Unknown.is_terminal());
    }

    #[test]
    fn created_checks() {
        let cq = "https://t.me/send?start=CQAbCdEfGhIj".to_owned();
        assert_eq!(
            parse_created_check(CB, "🦋 Чек на 10 USDT ($10.00)", std::slice::from_ref(&cq)),
            Some(CreatedCheck {
                url: cq.clone(),
                param: "CQAbCdEfGhIj".to_owned(),
                amount: Some((dec!(10), Asset::Usdt)),
            })
        );
        // Ссылка в тексте, алиас бота — в ответе каноническая ссылка.
        assert_eq!(
            parse_created_check(
                CB,
                "Чек создан.\nСсылка: https://t.me/CryptoBot?start=CQAbCdEfGhIj",
                &[]
            ),
            Some(CreatedCheck {
                url: cq,
                param: "CQAbCdEfGhIj".to_owned(),
                amount: None,
            })
        );
        let t = "https://t.me/xrocket?start=t_ABCDEFGHIJKLMNO".to_owned();
        assert_eq!(
            parse_created_check(XR, "🚀 Чек на 2.5 USDT", std::slice::from_ref(&t))
                .map(|c| (c.param, c.amount)),
            Some((
                "t_ABCDEFGHIJKLMNO".to_owned(),
                Some((dec!(2.5), Asset::Usdt))
            ))
        );
        // Чужая платформа, счёт вместо чека, два разных чека — None.
        assert_eq!(parse_created_check(CB, "", std::slice::from_ref(&t)), None);
        assert_eq!(
            parse_created_check(CB, "https://t.me/send?start=IVdixIeFSdqP", &[]),
            None
        );
        assert_eq!(
            parse_created_check(
                CB,
                "t.me/send?start=CQAbCdEfGhIj t.me/send?start=CQzzzzzzzzzz",
                &[]
            ),
            None
        );
        assert_eq!(parse_created_check(CB, "Чек на 10 USDT", &[]), None);
    }

    #[test]
    fn code_in_link_is_not_an_amount() {
        // Без вырезания ссылок «…f5TON» дал бы «неясную» сумму раньше настоящей.
        let text = "https://t.me/send?start=CQAbCdEf5TON\nЧек на 3 USDT";
        assert_eq!(amount::parse_money(text, CB), None);
        let found = parse_created_check(CB, text, &[]);
        assert_eq!(found.map(|c| c.amount), Some(Some((dec!(3), Asset::Usdt))));
    }

    #[test]
    fn invoice_cards() {
        let card = parse_invoice_card(
            CB,
            "🧾 Счёт\n\nСумма: 5 USDT ($5.00)\nОписание: Оплата заказа №1\nСтатус: активен",
        );
        assert_eq!(card.status, InvoiceStatus::Active);
        assert_eq!(card.amount, Some((dec!(5), Asset::Usdt)));
        assert_eq!(card.fiat, None);
        assert_eq!(card.description.as_deref(), Some("Оплата заказа №1"));
        assert_eq!(card.single_use, None);

        assert_eq!(
            parse_invoice_card(CB, "Счёт оплачен").status,
            InvoiceStatus::Paid
        );
        assert_eq!(
            parse_invoice_card(CB, "Invoice paid").status,
            InvoiceStatus::Paid
        );
        assert_eq!(
            parse_invoice_card(CB, "Срок действия счёта истёк").status,
            InvoiceStatus::Expired
        );
        assert_eq!(
            parse_invoice_card(XR, "This invoice has expired").status,
            InvoiceStatus::Expired
        );
        // «истекает» — не «истёк».
        assert_eq!(
            parse_invoice_card(CB, "Истекает через 1 час. Статус: активен").status,
            InvoiceStatus::Active
        );
        for text in [
            "Счёт не оплачен",
            "Not paid yet",
            "Неактивен",
            "Inactive",
            "Счёт",
            "Оплачен, но истёк",
        ] {
            assert_eq!(
                parse_invoice_card(CB, text).status,
                InvoiceStatus::Unknown,
                "{text}"
            );
        }

        let fiat = parse_invoice_card(CB, "Счёт на $10.00\nК оплате: 10.05 USDT");
        assert_eq!(fiat.fiat, Some((dec!(10), "USD".to_owned())));
        assert_eq!(fiat.amount, None);

        assert_eq!(
            parse_invoice_card(XR, "Многоразовый счёт").single_use,
            Some(false)
        );
        assert_eq!(
            parse_invoice_card(XR, "Multi-use invoice").single_use,
            Some(false)
        );
        assert_eq!(
            parse_invoice_card(XR, "Одноразовый счёт").single_use,
            Some(true)
        );
        assert_eq!(
            parse_invoice_card(XR, "📝 Description:\n  Coffee  ")
                .description
                .as_deref(),
            Some("Coffee")
        );
    }

    #[test]
    fn invoice_payment_replies() {
        use InvoicePayReply as P;
        for (text, expected) in [
            ("Вы оплатили счёт на 5 USDT", P::Paid),
            ("✅ Счёт успешно оплачен", P::Paid),
            ("Invoice paid successfully", P::Paid),
            ("Счёт уже оплачен", P::AlreadyPaid),
            ("This invoice has already been paid", P::AlreadyPaid),
            ("Срок действия счёта истёк", P::Expired),
            ("Invoice expired", P::Expired),
            ("Недостаточно средств", P::InsufficientFunds),
            ("Insufficient funds", P::InsufficientFunds),
            ("Not enough USDT", P::InsufficientFunds),
            // Голое «оплачен» — кем? Не угадываем.
            ("Счёт оплачен", P::Unknown),
            ("Счёт не оплачен", P::Unknown),
            ("Недостаточно средств, счёт истёк", P::Unknown),
            ("Ошибка", P::Unknown),
        ] {
            assert_eq!(classify_invoice_payment(CB, text), expected, "{text}");
        }
    }

    #[test]
    fn balances() {
        let cb =
            "👛 Кошелёк\n\nTether: 10.5 USDT ≈ $10.50\nToncoin: 1.2 TON ≈ $3.00\nBitcoin: 0 BTC";
        assert_eq!(
            parse_balance(CB, cb),
            vec![
                BalanceLine {
                    asset: Asset::Usdt,
                    amount: dec!(10.5)
                },
                BalanceLine {
                    asset: Asset::Ton,
                    amount: dec!(1.2)
                },
            ]
        );
        let xr = "💎 TONCOIN: 0.5\n💵 USDT: 1 234,56";
        assert_eq!(
            parse_balance(XR, xr),
            vec![
                BalanceLine {
                    asset: Asset::Ton,
                    amount: dec!(0.5)
                },
                BalanceLine {
                    asset: Asset::Usdt,
                    amount: dec!(1234.56)
                },
            ]
        );
        // Один актив с разными суммами — не знаем, какая верная.
        assert_eq!(
            parse_balance(CB, "USDT: 10\nUSDT: 12\nTON: 1"),
            vec![BalanceLine {
                asset: Asset::Ton,
                amount: dec!(1)
            }]
        );
        assert_eq!(parse_balance(CB, ""), vec![]);
        assert_eq!(parse_balance(CB, "USDT: 1,234"), vec![]);
    }
}
