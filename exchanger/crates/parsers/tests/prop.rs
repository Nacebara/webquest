//! Свойства парсеров на случайных входах (proptest; идеи — lovec `tests/parsers_prop.rs`):
//! разбор никогда не паникует, каноническая ссылка разбирается обратно в ту же ссылку, любая
//! форма ссылки даёт тот же результат, подложенные в текст ссылки находятся все и по порядку,
//! суммы в обычных записях разбираются точно (в `Decimal`, без `f64`).

use domain::{Asset, Decimal, Platform};
use parsers::amount::{parse_fiat, parse_money, parse_number};
use parsers::links::{extract_from_message, parse_bare_code, parse_url};
use parsers::replies::{
    ActivationReply, classify_activation, classify_invoice_payment, normalize, parse_balance,
    parse_created_check, parse_invoice_card,
};
use parsers::{LinkError, LinkKind, WalletLink};
use proptest::prelude::*;
use proptest::sample::select;

/// Кусочки «опасного» шума: всё, что ищут парсеры, плюс юникод (как NOISE в lovec).
const NOISE: &[&str] = &[
    " ",
    "\n",
    "=",
    "?",
    "&",
    "/",
    ":",
    "//",
    "%",
    "%D0%A1",
    "start",
    "startapp",
    "start=",
    "startapp=",
    "CQ",
    "IV",
    "t_",
    "mc_",
    "mci_",
    "inv_",
    "https://",
    "http://",
    "t.me/",
    "t.me",
    ".t.me",
    "telegram.dog/",
    "www.",
    "send",
    "CryptoBot",
    "xrocket",
    "tonRocketBot",
    "CryptoTestnetBot",
    "@",
    "#",
    "Чек",
    "на",
    "USDT",
    "TON",
    "TONCOIN",
    "$",
    "₽",
    "5",
    "0,5",
    "1 234",
    "000",
    ".",
    ",",
    "🦋",
    "🚀",
    "é",
    "\u{a0}",
    "\u{202f}",
    "\u{200b}",
    "\u{200d}",
    "\u{fe0f}",
    "\u{0}",
    "»",
    "«",
    ")",
    "(",
    "tg:",
    "tg://resolve",
    "domain=",
    "Testnet",
    "𝓐",
    "С",
    "вы получили",
    "от @",
    "уже активирован",
    "пароль",
    "оплачен",
    "не ",
];

fn noise(max_parts: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(select(NOISE), 0..=max_parts).prop_map(|parts| parts.concat())
}

/// Безопасный шум вокруг ссылки: без символов, которые продолжают адрес.
const SAFE: &[&str] = &[
    " ", "\n", "Чек", " на ", "USDT", "🦋", "!", "вот:", "5 TON", "(", "«",
];

fn safe(max_parts: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(select(SAFE), 0..=max_parts).prop_map(|parts| parts.concat())
}

/// Произвольная корректная ссылка на чек или счёт.
fn wallet_link() -> impl Strategy<Value = WalletLink> {
    let link = |platform, kind, prefix: &'static str| {
        move |tail: String| WalletLink {
            platform,
            kind,
            param: format!("{prefix}{tail}"),
        }
    };
    prop_oneof![
        "[A-Za-z0-9_-]{10}".prop_map(link(Platform::CryptoBot, LinkKind::Check, "CQ")),
        "[A-Za-z0-9]{6,32}".prop_map(link(Platform::CryptoBot, LinkKind::Invoice, "IV")),
        "[A-Za-z0-9_-]{15}".prop_map(link(Platform::XRocket, LinkKind::Check, "t_")),
        "[A-Za-z0-9_-]{15}".prop_map(link(Platform::XRocket, LinkKind::Check, "mci_")),
        "[A-Za-z0-9_-]{15}".prop_map(link(Platform::XRocket, LinkKind::Check, "mc_")),
        "[A-Za-z0-9]{6,32}".prop_map(link(Platform::XRocket, LinkKind::Invoice, "inv_")),
    ]
}

/// Число форм в [`render`].
const FORMS: usize = 16;

/// Ссылка в одной из форм SPEC §1.2 / DESIGN-v0.2 §3.
fn render(link: &WalletLink, form: usize, alias: bool) -> String {
    let bot = match (link.platform, alias) {
        (Platform::CryptoBot, false) => "send",
        (Platform::CryptoBot, true) => "CryptoBot",
        (Platform::XRocket, false) => "xrocket",
        (Platform::XRocket, true) => "tonRocketBot",
    };
    let p = &link.param;
    match form % FORMS {
        0 => format!("https://t.me/{bot}?start={p}"),
        1 => format!("http://t.me/{bot}?start={p}"),
        2 => format!("t.me/{bot}?start={p}"),
        3 => format!("https://telegram.me/{bot}?start={p}"),
        4 => format!("https://telegram.dog/{bot}?start={p}"),
        5 => format!("https://www.t.me/{bot}?start={p}"),
        6 => format!("https://{bot}.t.me/?start={p}"),
        7 => format!("https://t.me/{bot}/app?startapp={p}"),
        8 => format!("tg://resolve?domain={bot}&start={p}"),
        9 => format!("tg://resolve?start={p}&domain={bot}"),
        10 => format!("tg:resolve?domain=@{bot}&start={p}"),
        11 => format!("https://t.me/{bot}?ref=abc&start={p}"),
        12 => format!("www.telegram.me/{bot}?start={p}"),
        13 => format!("{bot}.t.me?start={p}"),
        14 => format!("https://t.me/{bot}?start={p}#x"),
        _ => format!("HTTPS://T.ME/{}?start={p}", bot.to_uppercase()),
    }
}

fn platform() -> impl Strategy<Value = Platform> {
    select(Platform::ALL.to_vec())
}

/// Десятичное число с точностью до 9 знаков (как у TON и xRocket).
fn decimal() -> impl Strategy<Value = Decimal> {
    (0u64..=1_000_000_000_000_000, 0u32..=9).prop_map(|(m, scale)| Decimal::new(m as i64, scale))
}

fn group(int: u64, sep: &str) -> String {
    let digits = int.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(sep);
        }
        out.push(ch);
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn parse_url_never_panics_on_arbitrary_strings(s in any::<String>()) {
        let _ = parse_url(&s);
        let _ = parse_bare_code(&s);
    }

    #[test]
    fn parsers_never_panic_on_noise(s in noise(40), p in platform()) {
        let _ = parse_url(&s);
        let _ = parse_bare_code(&s);
        let found = extract_from_message(&s, std::slice::from_ref(&s), std::slice::from_ref(&s));
        prop_assert!(!found.contains(&Err(LinkError::NotWalletLink)));
        for link in found.iter().flatten() {
            // Всё найденное — корректный код своей платформы, и ссылка воспроизводима.
            prop_assert_eq!(parse_url(&link.canonical_url()), Ok(link.clone()));
        }
        let _ = parse_money(&s, p);
        let _ = parse_fiat(&s);
        let _ = parse_number(&s);
        let _ = normalize(&s);
        let _ = classify_activation(p, &s);
        let _ = parse_created_check(p, &s, std::slice::from_ref(&s));
        let _ = parse_invoice_card(p, &s);
        let _ = classify_invoice_payment(p, &s);
        let _ = parse_balance(p, &s);
    }

    #[test]
    fn parsers_never_panic_on_arbitrary_strings(s in any::<String>(), p in platform()) {
        let _ = extract_from_message(&s, std::slice::from_ref(&s), &[]);
        let _ = parse_money(&s, p);
        let _ = parse_fiat(&s);
        let _ = classify_activation(p, &s);
        let _ = parse_invoice_card(p, &s);
        let _ = classify_invoice_payment(p, &s);
        let _ = parse_balance(p, &s);
    }

    #[test]
    fn classification_is_deterministic(s in noise(20), p in platform()) {
        prop_assert_eq!(classify_activation(p, &s), classify_activation(p, &s));
        prop_assert_eq!(classify_invoice_payment(p, &s), classify_invoice_payment(p, &s));
        // Нормализация идемпотентна.
        let n = normalize(&s);
        prop_assert_eq!(normalize(&n), n);
    }

    #[test]
    fn canonical_url_round_trips(link in wallet_link()) {
        prop_assert_eq!(parse_url(&link.canonical_url()), Ok(link.clone()));
    }

    #[test]
    fn every_form_gives_the_same_link(link in wallet_link(), form in 0..FORMS, alias: bool) {
        let url = render(&link, form, alias);
        prop_assert_eq!(parse_url(&url), Ok(link.clone()), "{}", url);
    }

    #[test]
    fn testnet_is_rejected_in_every_form(link in wallet_link(), form in 0..FORMS) {
        let bot = match link.platform {
            Platform::CryptoBot => "CryptoTestnetBot",
            Platform::XRocket => "xrocket_testnet_bot",
        };
        let url = render(&link, form, false)
            .replace("send", bot)
            .replace("SEND", bot)
            .replace("xrocket", bot)
            .replace("XROCKET", bot);
        prop_assert_eq!(parse_url(&url), Err(LinkError::Testnet), "{}", url);
    }

    #[test]
    fn cyrillic_lookalike_is_foreign(link in wallet_link(), form in 0..FORMS - 1) {
        // Последняя форма пишет имя бота заглавными — там подменять нечего.
        // Первая латинская «c»/«e»/«o» имени заменена кириллической.
        let real = match link.platform {
            Platform::CryptoBot => "send",
            Platform::XRocket => "xrocket",
        };
        let fake = match link.platform {
            Platform::CryptoBot => "s\u{0435}nd",
            Platform::XRocket => "xr\u{043e}cket",
        };
        let url = render(&link, form, false).replace(real, fake);
        prop_assume!(url.contains(fake));
        prop_assert_eq!(parse_url(&url), Err(LinkError::ForeignBot(fake.to_owned())), "{}", url);
    }

    #[test]
    fn planted_links_are_found_in_order(
        planted in prop::collection::vec((wallet_link(), 0..FORMS, any::<bool>()), 1..6),
        before in safe(4),
        seps in prop::collection::vec((select(vec![" ", "\n", "(", "«", ": "]), select(vec![" ", "\n", ".", ")", "»", "!", ", "])), 6),
        fillers in prop::collection::vec(safe(3), 6),
    ) {
        let mut text = before;
        for (i, (link, form, alias)) in planted.iter().enumerate() {
            text.push_str(seps[i].0);
            text.push_str(&render(link, *form, *alias));
            text.push_str(seps[i].1);
            text.push_str(&fillers[i]);
        }
        let mut want: Vec<WalletLink> = Vec::new();
        for (link, _, _) in &planted {
            if !want.iter().any(|w| w.platform == link.platform && w.param == link.param) {
                want.push(link.clone());
            }
        }
        let got: Vec<_> = extract_from_message(&text, &[], &[]);
        let want: Vec<_> = want.into_iter().map(Ok).collect();
        prop_assert_eq!(got, want, "{:?}", text);
    }

    #[test]
    fn bare_check_codes_round_trip(link in wallet_link(), pad in select(vec!["", " ", "\n", "  "])) {
        let text = format!("{pad}{}{pad}", link.param);
        let want = (link.kind == LinkKind::Check).then(|| link.clone());
        prop_assert_eq!(parse_bare_code(&text), want);
    }

    #[test]
    fn amounts_round_trip_in_common_formats(
        int in prop_oneof![0u64..10, 0u64..1_000, 0u64..10_000_000],
        cents in 0u64..100,
        with_cents: bool,
        style in 0usize..3,
        space in select(vec![" ", "\u{a0}", "\u{202f}", "\u{2009}"]),
        template in 0usize..4,
        p in platform(),
    ) {
        let value = if with_cents {
            Decimal::from_str_exact(&format!("{int}.{cents:02}")).expect("valid decimal")
        } else {
            Decimal::from(int)
        };
        let number = match style {
            // Английский стиль: запятая — тысячи, точка — дробь.
            0 => {
                let mut s = group(int, ",");
                if with_cents {
                    s.push_str(&format!(".{cents:02}"));
                }
                s
            }
            // Русский стиль: пробел — тысячи, запятая — дробь.
            1 => {
                let mut s = group(int, space);
                if with_cents {
                    s.push_str(&format!(",{cents:02}"));
                }
                s
            }
            // Без разделителей тысяч, точка — дробь.
            _ => {
                let mut s = int.to_string();
                if with_cents {
                    s.push_str(&format!(".{cents:02}"));
                }
                s
            }
        };
        let (code, asset) = match (template, p) {
            (0 | 1, _) => ("USDT", Asset::Usdt),
            (_, Platform::XRocket) => ("TONCOIN", Asset::Ton),
            _ => ("TON", Asset::Ton),
        };
        let text = match template {
            0 => format!("Вы получили {number} {code} ($1.00)."),
            1 => format!("🦋 Чек на {number}{space}{code}"),
            2 => format!("{number} {code}"),
            _ => format!("Сумма: {number} {code}\nКомиссия: 0 {code}"),
        };
        // «1,234» без дроби — неоднозначно (тысячи или дробь), такую сумму не угадываем.
        let ambiguous = style == 0 && !with_cents && (1_000..1_000_000).contains(&int);
        let want = (!ambiguous).then_some((value, asset));
        prop_assert_eq!(parse_money(&text, p), want, "{:?}", text);
    }

    #[test]
    fn decimal_display_parses_exactly(d in decimal(), p in platform()) {
        let text = format!("{d} USDT");
        prop_assert_eq!(parse_money(&text, p), Some((d, Asset::Usdt)), "{}", text);
    }

    #[test]
    fn win_reply_carries_the_exact_amount(d in decimal(), p in platform(), ton: bool) {
        let (code, asset) = if ton { ("TON", Asset::Ton) } else { ("USDT", Asset::Usdt) };
        prop_assert_eq!(
            classify_activation(p, &format!("✅ Вы получили {d} {code}")),
            ActivationReply::Received { amount: d, asset }
        );
        prop_assert_eq!(
            classify_activation(p, &format!("Вы получили {d} {code} от @someone")),
            ActivationReply::IncomingTransfer { amount: d, asset }
        );
    }
}
