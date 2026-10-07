//! Суммы из текстов ботов кошельков: `100 USDT`, `1 234,56 USDT`, `0.5 TON`, `10 TONCOIN`,
//! `$5.00` (фиат игнорируем). Только `Decimal`, без промежуточных `f64`.
//!
//! Правила числа (SPEC §1.3, «Извлечение суммы»; логика — lovec `amount.rs`, но без `f64` и
//! строже к неоднозначным записям):
//! - пробел, неразрывный (U+00A0), узкий неразрывный (U+202F), тонкий (U+2009) и цифровой
//!   (U+2007) пробелы — разделители тысяч, только перед группой ровно из трёх цифр;
//! - при пробелах-разделителях дробная часть — после единственной `.` или `,`: `1 234,56`;
//! - есть и `.`, и `,` — дробная часть после последнего из них, второй — разделитель тысяч
//!   с правильными группами: `1,234.56`, `1.234,56`;
//! - одна `.` — десятичная точка (`0.5`, `1.234`): боты пишут дробь через точку;
//! - одна `,` — десятичная запятая (`0,5`, `12,3456`), **кроме** записи `1,234`: это и 1234,
//!   и 1,234 — такую сумму не угадываем, результат «неясно».
//!
//! «Неясная» сумма у кода актива — это `None` для всего текста, а не переход к следующей
//! сумме: иначе из «Вы получили 1,234 TON. Баланс: 5 TON» получилось бы 5 TON.

use domain::{Asset, Decimal, Platform};

/// Число рядом с кодом: нет числа, число есть, но разобрать его однозначно нельзя, значение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Num {
    Absent,
    Unclear,
    Value(Decimal),
}

/// Самая длинная запись числа, которую разбираем; длиннее — «неясно».
const MAX_NUMBER_CHARS: usize = 48;

/// Коды активов в текстах платформы. У xRocket TON называется `TONCOIN`, но в текстах бота
/// встречается и `TON` («Rocket-чек на 400 TON», lovec AUDIT.md:66).
fn asset_codes(platform: Platform) -> &'static [(&'static str, Asset)] {
    match platform {
        Platform::CryptoBot => &[("USDT", Asset::Usdt), ("TON", Asset::Ton)],
        Platform::XRocket => &[
            ("USDT", Asset::Usdt),
            ("TONCOIN", Asset::Ton),
            ("TON", Asset::Ton),
        ],
    }
}

/// Фиатные коды из SPEC §1.4 (счета Crypto Pay в фиате).
const FIAT_CODES: [&str; 20] = [
    "USD", "EUR", "RUB", "BYN", "UAH", "GBP", "CNY", "KZT", "UZS", "GEL", "TRY", "AMD", "THB",
    "INR", "BRL", "IDR", "AZN", "AED", "PLN", "ILS",
];

/// Знаки валют, которые однозначно указывают на код.
const FIAT_SIGNS: [(char, &str); 8] = [
    ('$', "USD"),
    ('€', "EUR"),
    ('₽', "RUB"),
    ('£', "GBP"),
    ('₴', "UAH"),
    ('₸', "KZT"),
    ('₹', "INR"),
    ('₺', "TRY"),
];

/// Горизонтальный пробел между числом и кодом (любой Unicode-пробел, кроме перевода строки).
pub(crate) fn is_hspace(c: char) -> bool {
    c.is_whitespace() && c != '\n' && c != '\r' && c != '\u{2028}' && c != '\u{2029}'
}

/// Пробел, которым боты разделяют тысячи.
fn is_group_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\u{a0}' | '\u{202f}' | '\u{2009}' | '\u{2007}' | '\u{2008}' | '\u{200a}'
    )
}

fn ends_with_digit(s: &str) -> bool {
    s.ends_with(|c: char| c.is_ascii_digit())
}

/// Сразу за пробелом — группа ровно из трёх цифр.
fn three_digits_follow(rest: &str) -> bool {
    let b = rest.as_bytes();
    b.len() >= 3
        && b[..3].iter().all(u8::is_ascii_digit)
        && !b.get(3).is_some_and(u8::is_ascii_digit)
}

/// Код без учёта регистра на позиции `at` с границами слова: слева не буква (цифра можно:
/// `10usdt`), справа не буква, не цифра и не `_`.
fn code_at(text: &str, at: usize, code: &str) -> bool {
    let Some(word) = text.get(at..at + code.len()) else {
        return false;
    };
    if !word.eq_ignore_ascii_case(code) {
        return false;
    }
    let left_ok = !text[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphabetic() || c == '_');
    let right_ok = !text[at + code.len()..]
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_');
    left_ok && right_ok
}

/// Вхождения кодов активов платформы по порядку: (позиция, длина кода, актив).
pub(crate) fn code_occurrences(text: &str, platform: Platform) -> Vec<(usize, usize, Asset)> {
    let codes = asset_codes(platform);
    text.char_indices()
        .filter(|(_, c)| c.is_ascii_alphabetic())
        .filter_map(|(at, _)| {
            codes
                .iter()
                .find(|(code, _)| code_at(text, at, code))
                .map(|(code, asset)| (at, code.len(), *asset))
        })
        .collect()
}

/// Первая сумма с кодом поддерживаемого актива в тексте.
///
/// Ищем `<число> <КОД>` (пробел между ними может отсутствовать: `10USDT`). Код без числа
/// перед ним (`Tether (USDT): …`) пропускаем; неоднозначное число — `None` для всего текста.
pub fn parse_money(text: &str, platform: Platform) -> Option<(Decimal, Asset)> {
    find_money(text, platform).and_then(|hit| hit.value)
}

/// Первая сумма в тексте и где она стоит.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hit<T> {
    /// Байтовая позиция кода или знака валюты.
    pub at: usize,
    /// `None` — число есть, но неоднозначное.
    pub value: Option<T>,
}

/// Как [`parse_money`], но с позицией и с различием «нет суммы» / «сумма неясна».
pub(crate) fn find_money(text: &str, platform: Platform) -> Option<Hit<(Decimal, Asset)>> {
    for (at, _, asset) in code_occurrences(text, platform) {
        match number_before(&text[..at]) {
            Num::Absent => continue,
            Num::Unclear => return Some(Hit { at, value: None }),
            Num::Value(v) => {
                return Some(Hit {
                    at,
                    value: Some((v, asset)),
                });
            }
        }
    }
    None
}

/// Первая сумма в фиате: `$5.00`, `5 $`, `10 USD`, `USD 10`, `1 000 ₽`. Код — ISO 4217.
/// Неоднозначное число у первого же знака или кода — `None`.
pub fn parse_fiat(text: &str) -> Option<(Decimal, String)> {
    find_fiat(text).and_then(|hit| hit.value)
}

/// Как [`parse_fiat`], но с позицией и с различием «нет суммы» / «сумма неясна».
pub(crate) fn find_fiat(text: &str) -> Option<Hit<(Decimal, String)>> {
    for (at, c) in text.char_indices() {
        let (code, len) = if let Some((_, code)) = FIAT_SIGNS.iter().find(|(s, _)| *s == c) {
            (*code, c.len_utf8())
        } else if let Some(code) = FIAT_CODES.iter().find(|code| {
            text[at..].starts_with(**code)
                && !text[..at]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphabetic)
                && !text[at + code.len()..]
                    .chars()
                    .next()
                    .is_some_and(|n| n.is_alphanumeric() || n == '_')
        }) {
            (*code, code.len())
        } else {
            continue;
        };
        let found = match number_after(&text[at + len..]) {
            Num::Absent => number_before(&text[..at]),
            found => found,
        };
        match found {
            Num::Absent => continue,
            Num::Unclear => return Some(Hit { at, value: None }),
            Num::Value(v) => {
                return Some(Hit {
                    at,
                    value: Some((v, code.to_owned())),
                });
            }
        }
    }
    None
}

/// Отдельно стоящее число (`1 234,56`, `0.5`) по тем же правилам, что и суммы.
pub fn parse_number(s: &str) -> Option<Decimal> {
    match number_value(s.trim()) {
        Num::Value(v) => Some(v),
        Num::Absent | Num::Unclear => None,
    }
}

/// Число, которым заканчивается `s` (перед кодом актива).
pub(crate) fn number_before(s: &str) -> Num {
    let s = s.trim_end_matches(is_hspace);
    let mut start = s.len();
    let mut group = 0usize;
    for (i, c) in s.char_indices().rev() {
        // Разделитель: точка, запятая или пробел перед группой ровно из трёх цифр.
        let separator =
            c == '.' || c == ',' || (is_group_space(c) && group == 3 && ends_with_digit(&s[..i]));
        if c.is_ascii_digit() {
            group += 1;
        } else if separator {
            group = 0;
        } else {
            break;
        }
        start = i;
    }
    let token = &s[start..];
    if !token.bytes().any(|b| b.is_ascii_digit()) {
        return Num::Absent;
    }
    let before = &s[..start];
    if let Some(prev) = before.chars().next_back() {
        // Число приклеено к слову, со знаком или с валютой («x5», «-5», «$5»), либо перед ним
        // ещё цифры через пробел, которые не складываются в группы тысяч («12 34,5»).
        let glued = prev.is_alphanumeric()
            || matches!(prev, '_' | '-' | '−' | '+')
            || FIAT_SIGNS.iter().any(|(s, _)| *s == prev);
        let broken_group =
            is_group_space(prev) && ends_with_digit(&before[..before.len() - prev.len_utf8()]);
        if glued || broken_group {
            return Num::Unclear;
        }
    }
    number_value(token)
}

/// Число, с которого начинается `s` (после знака валюты или кода).
pub(crate) fn number_after(s: &str) -> Num {
    let s = s.trim_start_matches(is_hspace);
    if !s.starts_with(|c: char| c.is_ascii_digit()) {
        return Num::Absent;
    }
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() || c == '.' || c == ',' {
            end = i + 1;
        } else if is_group_space(c)
            && ends_with_digit(&s[..i])
            && three_digits_follow(&s[i + c.len_utf8()..])
        {
            // Разделитель тысяч: следующая группа продолжит число.
        } else {
            break;
        }
    }
    if s[end..]
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
    {
        // «5k», «5.00USDT» после знака доллара — не просто число.
        return Num::Unclear;
    }
    // Точка или запятая в конце — конец предложения: «$5.», «$5,».
    number_value(s[..end].trim_end_matches(['.', ',']))
}

/// Группы тысяч: первая — 1–3 цифры без ведущего нуля, остальные — ровно 3 цифры.
fn grouped(int: &str, is_sep: impl Fn(char) -> bool) -> bool {
    let mut groups = int.split(is_sep);
    let first = groups.next().unwrap_or_default();
    let digits = |g: &str| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit());
    digits(first)
        && first.len() <= 3
        && !first.starts_with('0')
        && groups.all(|g| g.len() == 3 && digits(g))
}

fn only_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Разобрать запись числа из цифр, `.`, `,` и пробелов-разделителей.
fn number_value(token: &str) -> Num {
    if token.is_empty() {
        return Num::Absent;
    }
    if token.chars().count() > MAX_NUMBER_CHARS
        || !token.starts_with(|c: char| c.is_ascii_digit())
        || !ends_with_digit(token)
        || !token
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',' || is_group_space(c))
    {
        return Num::Unclear;
    }
    let (int, frac): (String, Option<&str>) = if token.contains(is_group_space) {
        let mut seps = token.match_indices(['.', ',']);
        let (int, frac) = match (seps.next(), seps.next()) {
            (None, _) => (token, None),
            (Some((i, _)), None) => (&token[..i], Some(&token[i + 1..])),
            (Some(_), Some(_)) => return Num::Unclear,
        };
        if !grouped(int, is_group_space) {
            return Num::Unclear;
        }
        (int.chars().filter(char::is_ascii_digit).collect(), frac)
    } else {
        let dots = token.matches('.').count();
        let commas = token.matches(',').count();
        match (dots, commas) {
            (0, 0) => (token.to_owned(), None),
            (1, 0) => {
                let (i, f) = token.split_once('.').unwrap_or((token, ""));
                (i.to_owned(), Some(f))
            }
            (0, 1) => {
                let (i, f) = token.split_once(',').unwrap_or((token, ""));
                if f.len() == 3 && grouped(i, |c| c == ',') {
                    // «1,234»: тысячи по-английски или дробь по-русски — не угадываем.
                    return Num::Unclear;
                }
                (i.to_owned(), Some(f))
            }
            (_, 0) => {
                if !grouped(token, |c| c == '.') {
                    return Num::Unclear;
                }
                (token.replace('.', ""), None)
            }
            (0, _) => {
                if !grouped(token, |c| c == ',') {
                    return Num::Unclear;
                }
                (token.replace(',', ""), None)
            }
            _ => {
                // Есть и точки, и запятые: дробь — после последнего разделителя.
                let Some(last) = token.rfind(['.', ',']) else {
                    return Num::Unclear;
                };
                let decimal = char::from(token.as_bytes()[last]);
                let thousands = if decimal == '.' { ',' } else { '.' };
                let (i, f) = (&token[..last], &token[last + 1..]);
                if i.contains(decimal) || !grouped(i, |c| c == thousands) {
                    return Num::Unclear;
                }
                (i.replace(thousands, ""), Some(f))
            }
        }
    };
    if !only_digits(&int) || frac.is_some_and(|f| !only_digits(f)) {
        return Num::Unclear;
    }
    let canonical = match frac {
        Some(f) => format!("{int}.{f}"),
        None => int,
    };
    match Decimal::from_str_exact(&canonical) {
        Ok(v) => Num::Value(v),
        Err(_) => Num::Unclear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const CB: Platform = Platform::CryptoBot;
    const XR: Platform = Platform::XRocket;

    #[test]
    fn plain_amounts() {
        assert_eq!(parse_money("100 USDT", CB), Some((dec!(100), Asset::Usdt)));
        assert_eq!(parse_money("0.5 TON", CB), Some((dec!(0.5), Asset::Ton)));
        assert_eq!(parse_money("10 TONCOIN", XR), Some((dec!(10), Asset::Ton)));
        assert_eq!(parse_money("400 TON", XR), Some((dec!(400), Asset::Ton)));
        assert_eq!(parse_money("10USDT", CB), Some((dec!(10), Asset::Usdt)));
        assert_eq!(parse_money("10 usdt", CB), Some((dec!(10), Asset::Usdt)));
        assert_eq!(
            parse_money("0.000000001 TON", CB),
            Some((dec!(0.000000001), Asset::Ton))
        );
        assert_eq!(
            parse_money("Вы получили\n1 USDT", XR),
            Some((dec!(1), Asset::Usdt))
        );
    }

    #[test]
    fn grouped_amounts_with_every_space() {
        for sep in [" ", "\u{a0}", "\u{202f}", "\u{2009}", "\u{2007}"] {
            let text = format!("1{sep}234,56 USDT");
            assert_eq!(
                parse_money(&text, CB),
                Some((dec!(1234.56), Asset::Usdt)),
                "{text:?}"
            );
            let text = format!("1{sep}000{sep}000 USDT");
            assert_eq!(parse_money(&text, CB), Some((dec!(1000000), Asset::Usdt)));
        }
        assert_eq!(
            parse_money("1 234.5 TON", CB),
            Some((dec!(1234.5), Asset::Ton))
        );
        assert_eq!(
            parse_money("1,234.56 USDT", CB),
            Some((dec!(1234.56), Asset::Usdt))
        );
        assert_eq!(
            parse_money("1.234,56 USDT", CB),
            Some((dec!(1234.56), Asset::Usdt))
        );
        assert_eq!(
            parse_money("1,234,567 USDT", CB),
            Some((dec!(1234567), Asset::Usdt))
        );
        assert_eq!(parse_money("0,5 TON", CB), Some((dec!(0.5), Asset::Ton)));
        assert_eq!(
            parse_money("12,3456 TON", CB),
            Some((dec!(12.3456), Asset::Ton))
        );
        assert_eq!(
            parse_money("0,123 TON", CB),
            Some((dec!(0.123), Asset::Ton))
        );
    }

    #[test]
    fn fiat_and_foreign_assets_are_ignored() {
        assert_eq!(parse_money("$5.00", CB), None);
        assert_eq!(parse_money("5 USD", CB), None);
        assert_eq!(parse_money("5 BTC", CB), None);
        assert_eq!(parse_money("5 USDC", CB), None);
        assert_eq!(parse_money("5 TONX", CB), None);
        // TONCOIN у CryptoBot не код.
        assert_eq!(parse_money("5 TONCOIN", CB), None);
        assert_eq!(
            parse_money("🦋 Чек на 5 USDT ($5.00)", CB),
            Some((dec!(5), Asset::Usdt))
        );
        assert_eq!(
            parse_money("Tether (USDT): 10.5 USDT", CB),
            Some((dec!(10.5), Asset::Usdt))
        );
    }

    #[test]
    fn unclear_amounts_give_none() {
        for text in [
            "1,234 USDT",
            "Вы получили 1,234 TON. Баланс: 5 TON",
            "12..5 USDT",
            "x5 USDT",
            "-5 USDT",
            "$5 USDT",
            "12 34,5 USDT",
            "1234 567 USDT",
            "1 234,5,6 USDT",
            ".5 USDT",
            "5. USDT",
            "1,2,3 USDT",
            "99999999999999999999999999999999999 USDT",
        ] {
            assert_eq!(parse_money(text, CB), None, "{text:?}");
        }
    }

    #[test]
    fn fiat() {
        assert_eq!(parse_fiat("$5.00"), Some((dec!(5), "USD".to_owned())));
        assert_eq!(
            parse_fiat("≈ $ 1,234.50"),
            Some((dec!(1234.5), "USD".to_owned()))
        );
        assert_eq!(
            parse_fiat("Сумма: 10 EUR"),
            Some((dec!(10), "EUR".to_owned()))
        );
        assert_eq!(parse_fiat("1 000 ₽"), Some((dec!(1000), "RUB".to_owned())));
        assert_eq!(parse_fiat("5$"), Some((dec!(5), "USD".to_owned())));
        assert_eq!(parse_fiat("USD 7.5"), Some((dec!(7.5), "USD".to_owned())));
        assert_eq!(parse_fiat("Итого $5."), Some((dec!(5), "USD".to_owned())));
        assert_eq!(parse_fiat("10 USDT"), None);
        assert_eq!(parse_fiat("$5k"), None);
        assert_eq!(parse_fiat("try again"), None);
    }

    #[test]
    fn standalone_numbers() {
        assert_eq!(parse_number("1 234,56"), Some(dec!(1234.56)));
        assert_eq!(parse_number(" 0.5 "), Some(dec!(0.5)));
        assert_eq!(parse_number("1,234"), None);
        assert_eq!(parse_number("abc"), None);
        assert_eq!(parse_number(""), None);
    }
}
