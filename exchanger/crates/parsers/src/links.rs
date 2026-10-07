//! Ссылки на чеки и счета (SPEC §1.2; форматы — lovec `bots.rs`, `links.rs`).
//!
//! Коды: CryptoBot чек `CQ` + 10 = 12 символов, счёт `IV…`; xRocket чек `t_` + 15 = 17,
//! мульти-чек `mci_` + 15 = 19 и `mc_` + 15 = 18, счёт `inv_…`. Боты: `send` = `CryptoBot`,
//! `xrocket` = `tonRocketBot`; тестнет `CryptoTestnetBot`, `xrocket_testnet_bot` — не принимаем.
//! Формы: `https://t.me/<bot>?start=<p>`, без схемы, `telegram.me`, `telegram.dog`, `www.`,
//! `https://<bot>.t.me/?start=`, `t.me/<bot>/app?startapp=<p>`, `tg://resolve?domain=<bot>&start=<p>`
//! (и с `start` раньше `domain`), `?ref=…&start=<p>`. Имя бота — без учёта регистра, только ASCII.
//!
//! Разбор — через `url::Url`, не регуляркой по всему тексту (SPEC §1.2, п. 2). Регулярка нужна
//! только в [`extract_from_message`], чтобы найти в тексте кандидатов; каждый кандидат потом
//! разбирается [`parse_url`]. Сумма из ссылки не берётся никогда (SPEC §1.2, п. 6).
//!
//! Юзербот не переходит по ссылке и не резолвит имя из неё: наружу уходит только `param`,
//! и отправляется он боту по закреплённому peer ID (CLAUDE.md, «Юзербот», п. 10).

use std::ops::Range;
use std::sync::LazyLock;

use domain::Platform;
use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LinkKind {
    Check,
    Invoice,
}

/// Распознанная ссылка на чек или счёт официального бота кошелька.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WalletLink {
    pub platform: Platform,
    pub kind: LinkKind,
    /// Параметр `start`: `CQxxxxxxxxxx`, `t_…`, `mci_…`, `mc_…`, `IV…`, `inv_…`.
    pub param: String,
}

impl WalletLink {
    /// Каноническая ссылка, которую можно показать клиенту.
    pub fn canonical_url(&self) -> String {
        let bot = match self.platform {
            Platform::CryptoBot => "send",
            Platform::XRocket => "xrocket",
        };
        format!("https://t.me/{bot}?start={}", self.param)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("not a link to a wallet bot")]
    NotWalletLink,
    /// Ссылка на похожего, но не официального бота (фишинг, омоглифы).
    #[error("link points to a non-official bot: {0}")]
    ForeignBot(String),
    #[error("testnet links are not accepted")]
    Testnet,
    /// Ссылка на бота кошелька, но не чек и не счёт (реферальная, меню и т. п.).
    #[error("wallet bot link is neither a check nor an invoice")]
    NotCheckOrInvoice,
}

// ---------------------------------------------------------------------------------------------
// Боты и коды
// ---------------------------------------------------------------------------------------------

/// Официальные имена ботов в нижнем регистре: основное и алиас (DESIGN-v0.2 §3, `bots.rs`).
const CRYPTOBOT_NAMES: [&str; 2] = ["send", "cryptobot"];
const XROCKET_NAMES: [&str; 2] = ["xrocket", "tonrocketbot"];

/// Тестнет: `CryptoTestnetBot`, `xrocket_testnet_bot` (DESIGN-v0.2 §3, lovec `links.rs:141`) и
/// `ton_rocket_test_bot` (SPEC §1.2). Кроме того, как в lovec, тестнетом считается любое имя
/// со словом `testnet` — без учёта регистра.
const TESTNET_NAMES: [&str; 3] = [
    "cryptotestnetbot",
    "xrocket_testnet_bot",
    "ton_rocket_test_bot",
];

/// Хвост кода после префикса.
#[derive(Debug, Clone, Copy)]
enum Tail {
    /// Ровно столько символов `[A-Za-z0-9_-]` (алфавит кода чека в lovec).
    CheckExact(usize),
    /// От `min` до `max` символов `[A-Za-z0-9]` (коды счетов).
    InvoiceRange(usize, usize),
}

#[derive(Debug, Clone, Copy)]
struct Marker {
    platform: Platform,
    kind: LinkKind,
    prefix: &'static str,
    tail: Tail,
}

/// Таблица кодов (lovec `bots.rs:65-86`, DESIGN-v0.2 §3). `mc_` + 10 (13 символов) сюда
/// намеренно не входит: такие чеки xRocket активируются только через веб-приложение, `/start`
/// для них не работает (AUDIT-2.md:1776) — это `NotCheckOrInvoice`.
///
/// Длина кода счёта в источниках не зафиксирована, поэтому для `IV…` и `inv_…` принимаем
/// диапазон; карточка счёта всё равно проверяется после `/start` (SPEC §1.4).
const MARKERS: [Marker; 6] = [
    Marker {
        platform: Platform::CryptoBot,
        kind: LinkKind::Check,
        prefix: "CQ",
        tail: Tail::CheckExact(10),
    },
    Marker {
        platform: Platform::CryptoBot,
        kind: LinkKind::Invoice,
        prefix: "IV",
        tail: Tail::InvoiceRange(6, 32),
    },
    Marker {
        platform: Platform::XRocket,
        kind: LinkKind::Check,
        prefix: "t_",
        tail: Tail::CheckExact(15),
    },
    Marker {
        platform: Platform::XRocket,
        kind: LinkKind::Check,
        prefix: "mci_",
        tail: Tail::CheckExact(15),
    },
    Marker {
        platform: Platform::XRocket,
        kind: LinkKind::Check,
        prefix: "mc_",
        tail: Tail::CheckExact(15),
    },
    Marker {
        platform: Platform::XRocket,
        kind: LinkKind::Invoice,
        prefix: "inv_",
        tail: Tail::InvoiceRange(6, 32),
    },
];

/// Максимальная длина параметра `start` в Telegram (SPEC §1.2, п. 3).
const MAX_PARAM_LEN: usize = 64;
/// Длиннее ссылок на чек не бывает; всё, что длиннее, — не наш случай.
const MAX_URL_LEN: usize = 2048;

fn is_param_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Параметр `start` допустимого вида: `^[A-Za-z0-9_-]{1,64}$`.
fn is_valid_param(param: &str) -> bool {
    !param.is_empty() && param.len() <= MAX_PARAM_LEN && param.bytes().all(is_param_byte)
}

impl Tail {
    fn matches(self, tail: &str) -> bool {
        match self {
            Tail::CheckExact(len) => tail.len() == len && tail.bytes().all(is_param_byte),
            Tail::InvoiceRange(min, max) => {
                (min..=max).contains(&tail.len()) && tail.bytes().all(|b| b.is_ascii_alphanumeric())
            }
        }
    }
}

/// Что за код в параметре `start` для этого бота: чек, счёт или ни то ни другое (`None`).
/// Код чужой платформы (`t_…` у @send) — `None`: бот его не активирует.
pub fn classify_start_param(platform: Platform, param: &str) -> Option<LinkKind> {
    if !is_valid_param(param) {
        return None;
    }
    MARKERS
        .iter()
        .filter(|m| m.platform == platform)
        .find(|m| {
            param
                .strip_prefix(m.prefix)
                .is_some_and(|t| m.tail.matches(t))
        })
        .map(|m| m.kind)
}

/// Похож ли параметр на код чека или счёта хоть какой-то платформы.
fn is_wallet_code(param: &str) -> bool {
    Platform::ALL
        .into_iter()
        .any(|p| classify_start_param(p, param).is_some())
}

/// Платформа официального бота по имени (без `@`, без учёта регистра, только ASCII).
pub fn official_platform(bot: &str) -> Option<Platform> {
    let is = |names: &[&str]| names.iter().any(|n| n.eq_ignore_ascii_case(bot));
    if is(&CRYPTOBOT_NAMES) {
        Some(Platform::CryptoBot)
    } else if is(&XROCKET_NAMES) {
        Some(Platform::XRocket)
    } else {
        None
    }
}

/// Бот тестовой сети: известные имена или слово `testnet` в имени (как в lovec).
pub fn is_testnet_bot(bot: &str) -> bool {
    TESTNET_NAMES.iter().any(|n| n.eq_ignore_ascii_case(bot))
        || bot.to_ascii_lowercase().contains("testnet")
}

/// «Скелет» имени для сравнения с официальными: NFKC, нижний регистр, кириллические и
/// греческие двойники латиницы, `0`→`o`, `1`/`i`/`l`→`l`, без невидимых символов.
fn skeleton(name: &str) -> String {
    name.nfkc()
        .flat_map(char::to_lowercase)
        .filter_map(|c| {
            let c = match c {
                'а' | 'α' => 'a',
                'в' | 'β' => 'b',
                'с' | 'ϲ' => 'c',
                'ԁ' => 'd',
                'е' | 'ё' | 'ε' => 'e',
                'һ' | 'н' => 'h',
                'ј' => 'j',
                'к' | 'κ' => 'k',
                'м' => 'm',
                'η' | 'п' => 'n',
                '0' | 'о' | 'ο' => 'o',
                'р' | 'ρ' => 'p',
                'ԛ' => 'q',
                'ѕ' => 's',
                'т' | 'τ' => 't',
                'υ' => 'u',
                'ν' => 'v',
                'ԝ' => 'w',
                'х' | 'χ' => 'x',
                'у' | 'γ' | 'ү' => 'y',
                '1' | 'i' | 'l' | '|' | 'ı' | 'і' | 'ї' | 'ӏ' | 'ι' => 'l',
                c if c.is_alphanumeric() || c == '_' => c,
                _ => return None,
            };
            Some(c)
        })
        .collect()
}

/// Имя — не официальное, но похоже на официальное или тестовое (омоглиф, `CryptoB0t`).
fn looks_like_official(name: &str) -> bool {
    let sk = skeleton(name);
    CRYPTOBOT_NAMES
        .iter()
        .chain(&XROCKET_NAMES)
        .chain(&TESTNET_NAMES)
        .any(|n| skeleton(n) == sk)
}

// ---------------------------------------------------------------------------------------------
// Разбор одной ссылки
// ---------------------------------------------------------------------------------------------

/// Значение `start` / `startapp` из ссылки.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StartParam {
    Absent,
    One(String),
    /// Несколько разных значений (`?start=A&start=B`): какое увидит клиент — неизвестно.
    Conflict,
}

impl StartParam {
    fn add(&mut self, value: String) {
        *self = match std::mem::replace(self, StartParam::Absent) {
            StartParam::Absent => StartParam::One(value),
            StartParam::One(v) if v == value => StartParam::One(v),
            _ => StartParam::Conflict,
        };
    }
}

/// Куда ведёт ссылка: имя бота и параметр запуска.
struct Target {
    bot: String,
    start: StartParam,
}

/// Разобрать одну ссылку (SPEC §1.2): кнопку, сущность `text_link` или адрес из текста.
///
/// Порядок решений: не ссылка Telegram → `NotWalletLink`; не-ASCII или похожее на официальное
/// имя → `ForeignBot`; тестнет → `Testnet`; чужой бот → `ForeignBot`, если в ссылке код чека
/// или счёта, иначе `NotWalletLink`; официальный бот без кода своей платформы →
/// `NotCheckOrInvoice`.
pub fn parse_url(url: &str) -> Result<WalletLink, LinkError> {
    let s = url.trim();
    if s.is_empty()
        || s.len() > MAX_URL_LEN
        || s.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(LinkError::NotWalletLink);
    }
    let target = if starts_with_ci(s, "tg:") {
        tg_target(s)?
    } else {
        web_target(s)?
    };
    judge(target)
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn judge(target: Target) -> Result<WalletLink, LinkError> {
    let Target { bot, start } = target;
    if bot.is_empty() {
        return Err(LinkError::NotWalletLink);
    }
    let code_like = matches!(&start, StartParam::One(p) if is_wallet_code(p));
    if !bot.is_ascii() {
        // В юзернеймах Telegram только ASCII: такое имя — подделка или вовсе не бот.
        return if code_like || looks_like_official(&bot) {
            Err(LinkError::ForeignBot(bot))
        } else {
            Err(LinkError::NotWalletLink)
        };
    }
    if is_testnet_bot(&bot) {
        return Err(LinkError::Testnet);
    }
    let Some(platform) = official_platform(&bot) else {
        return if code_like || looks_like_official(&bot) {
            Err(LinkError::ForeignBot(bot))
        } else {
            Err(LinkError::NotWalletLink)
        };
    };
    let StartParam::One(param) = start else {
        return Err(LinkError::NotCheckOrInvoice);
    };
    let kind = classify_start_param(platform, &param).ok_or(LinkError::NotCheckOrInvoice)?;
    Ok(WalletLink {
        platform,
        kind,
        param,
    })
}

/// Собрать `start`/`startapp` из пар запроса.
fn collect_start<'a>(
    pairs: impl Iterator<Item = (std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>)>,
) -> (StartParam, Vec<String>) {
    let mut start = StartParam::Absent;
    let mut domains = Vec::new();
    for (key, value) in pairs {
        match key.as_ref() {
            "start" | "startapp" => start.add(value.into_owned()),
            "domain" => domains.push(value.into_owned()),
            _ => {}
        }
    }
    (start, domains)
}

/// `tg://resolve?domain=<bot>&start=<p>`, `tg:resolve?domain=@<bot>&start=<p>`,
/// `tg://resolve?start=<p>&domain=<bot>`.
fn tg_target(s: &str) -> Result<Target, LinkError> {
    let url = Url::parse(s).map_err(|_| LinkError::NotWalletLink)?;
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err(LinkError::NotWalletLink);
    }
    let action = match url.host_str() {
        Some(host) if matches!(url.path(), "" | "/") => host,
        Some(_) => return Err(LinkError::NotWalletLink),
        None => url.path(),
    };
    if !action.eq_ignore_ascii_case("resolve") {
        return Err(LinkError::NotWalletLink);
    }
    let (start, domains) = collect_start(url.query_pairs());
    let mut bot: Option<String> = None;
    for d in domains {
        let d = d.strip_prefix('@').unwrap_or(&d).to_owned();
        match &bot {
            None => bot = Some(d),
            Some(b) if *b == d => {}
            Some(_) => return Err(LinkError::NotWalletLink),
        }
    }
    let bot = bot.ok_or(LinkError::NotWalletLink)?;
    Ok(Target { bot, start })
}

/// Хосты Telegram, на которых имя бота — первый сегмент пути.
const WEB_HOSTS: [&str; 3] = ["t.me", "telegram.me", "telegram.dog"];

/// `https://t.me/<bot>?start=…` и все веб-формы: без схемы, `http`, `telegram.me`,
/// `telegram.dog`, `www.`, `<bot>.t.me`, `/<bot>/app?startapp=…`.
fn web_target(s: &str) -> Result<Target, LinkError> {
    let full = if starts_with_ci(s, "https://") || starts_with_ci(s, "http://") {
        s.to_owned()
    } else if s.contains("://") {
        return Err(LinkError::NotWalletLink);
    } else {
        format!("https://{s}")
    };
    let url = match Url::parse(&full) {
        Ok(url) => url,
        // Хост, который не прошёл IDNA, но выглядит как `<двойник>.t.me`, — подделка.
        Err(_) => {
            return match raw_subdomain_label(&full) {
                Some(label) if !label.is_ascii() => Err(LinkError::ForeignBot(label)),
                _ => Err(LinkError::NotWalletLink),
            };
        }
    };
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err(LinkError::NotWalletLink);
    }
    let host = url.host_str().ok_or(LinkError::NotWalletLink)?;
    let base = WEB_HOSTS
        .iter()
        .find(|h| host == **h || host.strip_suffix(**h).is_some_and(|p| p.ends_with('.')))
        .ok_or(LinkError::NotWalletLink)?;
    let labels = host
        .strip_suffix(base)
        .and_then(|p| p.strip_suffix('.'))
        .filter(|p| *p != "www");

    // Метка поддомена, записанная не-ASCII (омоглиф), приходит из IDNA как `xn--…`.
    if labels.is_some_and(|l| l.split('.').any(|part| part.starts_with("xn--"))) {
        let label =
            raw_subdomain_label(&full).unwrap_or_else(|| labels.unwrap_or_default().to_owned());
        return Err(LinkError::ForeignBot(label));
    }

    let segments: Vec<String> = url
        .path_segments()
        .map(|it| it.map(percent_decode).collect())
        .unwrap_or_default();
    let mut path: Vec<&str> = segments.iter().map(String::as_str).collect();
    if path.last().is_some_and(|s| s.is_empty()) {
        path.pop();
    }

    let (bot, rest): (String, &[&str]) = match (labels, path.split_first()) {
        // `https://<bot>.t.me/?start=…`: имя бота — метка поддомена, путь пустой.
        (Some(label), None) if *base == "t.me" && !label.contains('.') => (label.to_owned(), &[]),
        (Some(_), None) => return Err(LinkError::NotWalletLink),
        // `t.me/<bot>/…`; метка поддомена при непустом пути — опечатка вроде `Ok.t.me/send`,
        // поддомены t.me принадлежат Telegram, поэтому читаем путь.
        (_, Some((first, rest))) => ((*first).to_owned(), rest),
        (None, None) => return Err(LinkError::NotWalletLink),
    };

    let (start, _) = collect_start(url.query_pairs());
    // После имени бота допустим только короткий идентификатор мини-приложения: `/app`.
    let start = match rest {
        [] => start,
        [app] if is_app_name(app) => start,
        _ => StartParam::Absent,
    };
    Ok(Target { bot, start })
}

fn is_app_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_PARAM_LEN
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Первая метка хоста в исходной записи (до IDNA): для `https://Сrypto.t.me/…` — `Сrypto`.
/// Только для хостов на `.t.me`.
fn raw_subdomain_label(full: &str) -> Option<String> {
    const SUFFIX: &str = ".t.me";
    let rest = full.split_once("://").map_or(full, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = match host.rsplit_once(':') {
        Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    };
    let cut = host.len().checked_sub(SUFFIX.len())?;
    if !host.get(cut..)?.eq_ignore_ascii_case(SUFFIX) {
        return None;
    }
    let label = host.get(..cut)?;
    let label = label.strip_prefix("www.").unwrap_or(label);
    (!label.is_empty()).then(|| label.to_owned())
}

/// Декодировать `%XX` в сегменте пути (UTF-8 с заменой битых последовательностей).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            )
        {
            // h, l < 16, поэтому h * 16 + l < 256.
            out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------------------------
// Голый код и сообщение целиком
// ---------------------------------------------------------------------------------------------

/// Голый код без ссылки (`CQ…`, `t_…`, `mci_…`, `mc_…`): клиент мог прислать только его.
///
/// Текст — ровно один код чека (пробелы, кавычки и знаки препинания по краям не мешают).
/// Коды счетов голыми не принимаем: `IV…` слишком легко спутать с обычным словом.
pub fn parse_bare_code(text: &str) -> Option<WalletLink> {
    let code = text.trim().trim_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '.' | ',' | ';' | ':' | '!' | '?' | '(' | ')' | '«' | '»' | '"' | '\'' | '`'
            )
    });
    Platform::ALL.into_iter().find_map(|platform| {
        (classify_start_param(platform, code) == Some(LinkKind::Check)).then(|| WalletLink {
            platform,
            kind: LinkKind::Check,
            param: code.to_owned(),
        })
    })
}

/// Символы, на которых адрес в тексте заканчивается: пробелы, кавычки, скобки, `<>`,
/// многоточие и тире (их часто приклеивают к ссылке).
const URL_TAIL: &str = r#"[^\s<>"'()\[\]{}«»“”„‘’`|…—–]"#;

/// Кандидаты в ссылки Telegram в тексте: веб-формы с хостом t.me / telegram.me / telegram.dog
/// (с меткой поддомена или `www.`) и `tg:`-ссылки.
static CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(
        r"(?i)(?:https?://)?(?:[\p{{L}}\p{{N}}_-]+\.)*(?:t\.me|telegram\.me|telegram\.dog)\b{URL_TAIL}*|tg:{URL_TAIL}+"
    );
    // Шаблон постоянный и покрыт тестами: ошибка здесь — ошибка сборки, а не данных.
    #[allow(clippy::expect_used)]
    Regex::new(&pattern).expect("CANDIDATE regex is valid")
});

/// Символ перед кандидатом, при котором это не начало ссылки, а середина другого слова или
/// адреса: `xt.me`, `evil.com/t.me/…`, `?u=https://t.me/…`.
fn glued_before(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '_' | '-' | '/' | '@' | '=' | '&' | '?' | '#' | '%' | '+' | '~' | '\\'
        )
}

/// Адреса-кандидаты из текста в порядке появления (байтовые диапазоны в `text`).
fn candidate_ranges(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(m) = CANDIDATE.find_at(text, pos) {
        if text[..m.start()]
            .chars()
            .next_back()
            .is_some_and(glued_before)
        {
            // Не ссылка с этого места; ищем дальше со следующего символа.
            pos = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let cand = m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if !cand.is_empty() {
            out.push(m.start()..m.start() + cand.len());
        }
        pos = m.end();
    }
    out
}

/// Насколько результат информативен: распознанная ссылка лучше предупреждения о подделке или
/// тестнете, а оно лучше «не чек» и «не ссылка кошелька».
fn informativeness(r: &Result<WalletLink, LinkError>) -> u8 {
    match r {
        Ok(_) => 3,
        Err(LinkError::ForeignBot(_) | LinkError::Testnet) => 2,
        Err(LinkError::NotCheckOrInvoice) => 1,
        Err(LinkError::NotWalletLink) => 0,
    }
}

/// Разобрать кандидата из текста. К ссылке в тексте часто приклеен следующий текст:
/// «…?start=CQAbCdEfGhIj.Спасибо», «…&domain=send.Чек», «…?start=CQAbCdEfGhIj!USDT».
/// Поэтому, кроме адреса целиком, пробуем его же, обрезанный перед первым не-ASCII символом
/// и по концу кода в `start=` (как lovec, который читает код до первого символа не из его
/// алфавита), и берём самый информативный результат; при равенстве — более ранний.
/// Одиночные адреса из кнопок и сущностей ([`parse_url`]) так не обрезаются.
fn parse_candidate(cand: &str) -> Result<WalletLink, LinkError> {
    let mut best = parse_url(cand);
    let non_ascii_cut = cand
        .find(|c: char| !c.is_ascii())
        .map(|i| cand[..i].trim_end_matches(['.', ',', ';', ':', '!', '?']));
    let code_cut = cut_after_value(cand, &["start=", "startapp="], is_param_byte);
    // Имя бота в tg://resolve?…&domain=<бот>: буквы, цифры, `_` и `@` в начале.
    let domain_cut = cut_after_value(cand, &["domain="], |b| {
        b.is_ascii_alphanumeric() || b == b'_' || b == b'@'
    });
    for cut in [non_ascii_cut, code_cut, domain_cut].into_iter().flatten() {
        if best.is_ok() {
            break;
        }
        if cut.is_empty() || cut.len() >= cand.len() {
            continue;
        }
        let retry = parse_url(cut);
        if informativeness(&retry) > informativeness(&best) {
            best = retry;
        }
    }
    best
}

/// Адрес до конца значения последнего параметра из `keys` (`?key=` или `&key=`), значение —
/// по допустимым байтам `allowed`.
fn cut_after_value<'a>(
    cand: &'a str,
    keys: &[&str],
    allowed: impl Fn(u8) -> bool,
) -> Option<&'a str> {
    let at = keys
        .iter()
        .filter_map(|key| {
            cand.rmatch_indices(key)
                .find(|(i, _)| *i > 0 && matches!(cand.as_bytes()[i - 1], b'?' | b'&'))
                .map(|(i, _)| i + key.len())
        })
        .max()?;
    let len = cand[at..].bytes().take_while(|b| allowed(*b)).count();
    cand.get(..at + len)
}

/// Текст без ссылок Telegram (каждая заменена пробелом): чтобы код в ссылке вроде
/// `…?start=CQAbCdEf5TON` не читался как сумма.
pub(crate) fn blank_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for r in candidate_ranges(text) {
        out.push_str(&text[last..r.start]);
        out.push(' ');
        last = r.end;
    }
    out.push_str(&text[last..]);
    out
}

/// Все ссылки из сообщения клиента в порядке появления, без повторов: текст, URL из сущностей
/// `text_link` и URL кнопок (пересланный чек несёт ссылку в кнопке).
///
/// Порядок: сначала ссылки из текста, затем из сущностей, затем из кнопок. Повтор — та же пара
/// (платформа, параметр) или та же ошибка. Ссылки не на ботов кошельков (`NotWalletLink`:
/// каналы, сайты) в результат не попадают — клиенту про них сказать нечего.
pub fn extract_from_message(
    text: &str,
    entity_urls: &[String],
    button_urls: &[String],
) -> Vec<Result<WalletLink, LinkError>> {
    let mut out: Vec<Result<WalletLink, LinkError>> = Vec::new();
    let from_text = candidate_ranges(text)
        .into_iter()
        .map(|r| parse_candidate(&text[r]));
    let from_urls = entity_urls.iter().chain(button_urls).map(|u| parse_url(u));
    for found in from_text.chain(from_urls) {
        let duplicate = match &found {
            Err(LinkError::NotWalletLink) => true,
            Ok(link) => out.iter().any(|seen| {
                seen.as_ref()
                    .is_ok_and(|s| s.platform == link.platform && s.param == link.param)
            }),
            Err(e) => out.iter().any(|seen| seen.as_ref().err() == Some(e)),
        };
        if !duplicate {
            out.push(found);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CQ: &str = "CQAbCdEfGhIj";
    const T: &str = "t_ABCDEFGHIJKLMNO";
    const MCI: &str = "mci_fdraQh70UOwz3nu";
    const MC: &str = "mc_VhxQyBOeQXDmHAX";

    fn ok(platform: Platform, kind: LinkKind, param: &str) -> Result<WalletLink, LinkError> {
        Ok(WalletLink {
            platform,
            kind,
            param: param.to_owned(),
        })
    }

    fn cb_check() -> Result<WalletLink, LinkError> {
        ok(Platform::CryptoBot, LinkKind::Check, CQ)
    }

    #[test]
    fn cryptobot_check_in_every_form() {
        let forms = [
            format!("https://t.me/send?start={CQ}"),
            format!("http://t.me/send?start={CQ}"),
            format!("t.me/send?start={CQ}"),
            format!("https://telegram.me/send?start={CQ}"),
            format!("telegram.me/CryptoBot?start={CQ}"),
            format!("https://telegram.dog/send?start={CQ}"),
            format!("https://www.t.me/send?start={CQ}"),
            format!("www.telegram.me/send?start={CQ}"),
            format!("https://send.t.me/?start={CQ}"),
            format!("send.t.me?start={CQ}"),
            format!("https://CryptoBot.t.me/?start={CQ}"),
            format!("https://t.me/send/app?startapp={CQ}"),
            format!("https://t.me/send?startapp={CQ}"),
            format!("https://t.me/send/?start={CQ}"),
            format!("tg://resolve?domain=send&start={CQ}"),
            format!("tg://resolve?start={CQ}&domain=send"),
            format!("tg:resolve?domain=@send&start={CQ}"),
            format!("tg://resolve?domain=CryptoBot&appname=app&startapp={CQ}"),
            format!("TG://RESOLVE?domain=send&start={CQ}"),
            format!("https://t.me/send?ref=promo&start={CQ}"),
            format!("https://t.me/send?start={CQ}&ref=promo#frag"),
            format!("HTTPS://T.ME/SEND?start={CQ}"),
            format!("https://t.me/CRYPTOBOT?start={CQ}"),
            format!("https://t.me:443/send?start={CQ}"),
            format!("  https://t.me/send?start={CQ}  "),
        ];
        for url in forms {
            assert_eq!(parse_url(&url), cb_check(), "{url}");
        }
    }

    #[test]
    fn xrocket_checks_in_every_form() {
        for code in [T, MCI, MC] {
            let forms = [
                format!("https://t.me/xrocket?start={code}"),
                format!("t.me/xRocket?start={code}"),
                format!("https://t.me/tonRocketBot?start={code}"),
                format!("https://telegram.dog/xrocket?start={code}"),
                format!("https://xrocket.t.me/?start={code}"),
                format!("https://t.me/xrocket/app?startapp={code}"),
                format!("tg://resolve?domain=xrocket&start={code}"),
                format!("tg://resolve?start={code}&domain=tonRocketBot"),
                format!("https://t.me/xrocket?ref=abc&start={code}"),
            ];
            for url in forms {
                assert_eq!(
                    parse_url(&url),
                    ok(Platform::XRocket, LinkKind::Check, code),
                    "{url}"
                );
            }
        }
    }

    #[test]
    fn invoices_of_both_platforms() {
        assert_eq!(
            parse_url("https://t.me/send?start=IVdixIeFSdqP"),
            ok(Platform::CryptoBot, LinkKind::Invoice, "IVdixIeFSdqP")
        );
        assert_eq!(
            parse_url("https://t.me/CryptoBot?start=IVabc123"),
            ok(Platform::CryptoBot, LinkKind::Invoice, "IVabc123")
        );
        assert_eq!(
            parse_url("https://t.me/xrocket?start=inv_C9QiNcpEbXyZ"),
            ok(Platform::XRocket, LinkKind::Invoice, "inv_C9QiNcpEbXyZ")
        );
        // Слишком короткий хвост счёта — не счёт.
        assert_eq!(
            parse_url("https://t.me/send?start=IVAN"),
            Err(LinkError::NotCheckOrInvoice)
        );
    }

    #[test]
    fn wrong_codes_on_official_bots_are_not_checks() {
        for url in [
            "https://t.me/send",
            "https://t.me/send?start=",
            "https://t.me/send?start=r-12345",
            "https://t.me/send?start=pay",
            "https://t.me/send?start=CQabc",
            "https://t.me/send?start=CQAbCdEfGhIjK",
            // Код xRocket у бота CryptoBot и наоборот.
            "https://t.me/send?start=t_ABCDEFGHIJKLMNO",
            "https://t.me/xrocket?start=CQAbCdEfGhIj",
            // mc_ + 10: только веб-приложение xRocket, /start не работает.
            "https://t.me/xrocket?start=mc_ABCDEFGHIJ",
            "https://t.me/xrocket/app?startapp=mc_ABCDEFGHIJ",
            // t_ длиной 16 встречался в логах lovec — не наш формат.
            "https://t.me/xrocket?start=t_ABCDEFGHIJKLMN",
            // Недопустимые символы в параметре.
            "https://t.me/send?start=CQAbCdEf%2BhIj",
            "https://t.me/send?start=CQAbCdEfGhIj%D1%87",
            // Два разных кода в одной ссылке.
            "https://t.me/send?start=CQAbCdEfGhIj&start=CQzzzzzzzzzz",
            // Лишние сегменты пути.
            "https://t.me/send/a/b?start=CQAbCdEfGhIj",
        ] {
            assert_eq!(parse_url(url), Err(LinkError::NotCheckOrInvoice), "{url}");
        }
    }

    #[test]
    fn testnet_bots_are_rejected() {
        for url in [
            format!("https://t.me/CryptoTestnetBot?start={CQ}"),
            format!("t.me/cryptotestnetbot?start={CQ}"),
            format!("tg://resolve?domain=xrocket_testnet_bot&start={T}"),
            format!("tg://resolve?start={CQ}&domain=CryptoTestnetBot"),
            format!("https://t.me/XRocket_TestNet_Bot/app?startapp={MCI}"),
            format!("https://t.me/ton_rocket_test_bot?start={T}"),
            format!("https://CryptoTestnetBot.t.me/?start={CQ}"),
        ] {
            assert_eq!(parse_url(&url), Err(LinkError::Testnet), "{url}");
        }
        // «testnet» не в имени бота — обычная ссылка.
        assert_eq!(
            parse_url(&format!("https://t.me/send?ref=testnet_promo&start={CQ}")),
            cb_check()
        );
        assert_eq!(
            parse_url(&format!("https://t.me/send/testnet?startapp={CQ}")),
            cb_check()
        );
    }

    #[test]
    fn homoglyphs_and_lookalikes_are_foreign() {
        // Кириллическая «С» в начале имени.
        assert_eq!(
            parse_url(&format!("https://t.me/\u{0421}ryptoBot?start={CQ}")),
            Err(LinkError::ForeignBot("\u{0421}ryptoBot".to_owned()))
        );
        // То же без кода в ссылке: имя похоже на официальное.
        assert_eq!(
            parse_url("https://t.me/\u{0421}ryptoBot"),
            Err(LinkError::ForeignBot("\u{0421}ryptoBot".to_owned()))
        );
        // Кириллическая «е» в send, в tg://.
        assert_eq!(
            parse_url(&format!("tg://resolve?domain=s\u{0435}nd&start={CQ}")),
            Err(LinkError::ForeignBot("s\u{0435}nd".to_owned()))
        );
        // Полноширинные латинские буквы.
        assert!(matches!(
            parse_url(&format!(
                "https://t.me/\u{ff53}\u{ff45}\u{ff4e}\u{ff44}?start={CQ}"
            )),
            Err(LinkError::ForeignBot(_))
        ));
        // Невидимый символ внутри имени.
        assert!(matches!(
            parse_url(&format!("https://t.me/se\u{200b}nd?start={CQ}")),
            Err(LinkError::ForeignBot(_))
        ));
        // Омоглиф в поддомене.
        assert_eq!(
            parse_url(&format!("https://\u{0421}ryptoBot.t.me/?start={CQ}")),
            Err(LinkError::ForeignBot("\u{0421}ryptoBot".to_owned()))
        );
        // ASCII-двойники и чужие боты с кодом чека.
        for (url, bot) in [
            (format!("https://t.me/CryptoB0t?start={CQ}"), "CryptoB0t"),
            ("https://t.me/xr0cket?start=hello".to_owned(), "xr0cket"),
            (
                format!("https://t.me/CryptoBot_support?start={CQ}"),
                "CryptoBot_support",
            ),
            (format!("https://t.me/SomeBot?start={T}"), "SomeBot"),
            (format!("tg://resolve?domain=wallet&start={CQ}"), "wallet"),
        ] {
            assert_eq!(
                parse_url(&url),
                Err(LinkError::ForeignBot(bot.to_owned())),
                "{url}"
            );
        }
    }

    #[test]
    fn non_wallet_links() {
        for url in [
            "",
            "   ",
            "hello",
            "https://t.me/durov",
            "https://t.me/SomeBot?start=hello",
            "https://t.me/",
            "https://t.me",
            "https://t.me/+AbCdEf",
            "https://t.me/s/channel",
            "https://t.me/привет",
            "https://example.com/send?start=CQAbCdEfGhIj",
            "https://t.me.evil.com/send?start=CQAbCdEfGhIj",
            "https://evilt.me/send?start=CQAbCdEfGhIj",
            "https://evil@t.me/send?start=CQAbCdEfGhIj",
            "https://t.me:8443/send?start=CQAbCdEfGhIj",
            "ftp://t.me/send?start=CQAbCdEfGhIj",
            "javascript:alert(1)",
            "tg://msg?to=send&text=CQAbCdEfGhIj",
            "tg://resolve?start=CQAbCdEfGhIj",
            "tg://resolve?domain=send&domain=xrocket&start=CQAbCdEfGhIj",
            "https://t.me/se nd?start=CQAbCdEfGhIj",
            "https://t.me/send?start=CQAb\nCdEfGhIj",
            "https://www.send.t.me/?start=CQAbCdEfGhIj",
        ] {
            assert_eq!(parse_url(url), Err(LinkError::NotWalletLink), "{url:?}");
        }
    }

    #[test]
    fn canonical_url_round_trips() {
        for (platform, kind, param) in [
            (Platform::CryptoBot, LinkKind::Check, CQ),
            (Platform::CryptoBot, LinkKind::Invoice, "IVdixIeFSdqP"),
            (Platform::XRocket, LinkKind::Check, T),
            (Platform::XRocket, LinkKind::Check, MCI),
            (Platform::XRocket, LinkKind::Check, MC),
            (Platform::XRocket, LinkKind::Invoice, "inv_C9QiNcpEbXyZ"),
        ] {
            let link = WalletLink {
                platform,
                kind,
                param: param.to_owned(),
            };
            assert_eq!(parse_url(&link.canonical_url()), Ok(link));
        }
    }

    #[test]
    fn bare_codes() {
        assert_eq!(parse_bare_code(CQ), cb_check().ok());
        assert_eq!(parse_bare_code(&format!("  «{CQ}».\n")), cb_check().ok());
        assert_eq!(parse_bare_code(&format!("`{CQ}`")), cb_check().ok());
        for code in [T, MCI, MC] {
            assert_eq!(
                parse_bare_code(code),
                ok(Platform::XRocket, LinkKind::Check, code).ok()
            );
        }
        for text in [
            "",
            "CQabc",
            "mc_ABCDEFGHIJ",
            "IVdixIeFSdqP",
            "inv_C9QiNcpEbXyZ",
            "вот чек CQAbCdEfGhIj",
            "CQAbCdEfGhIj CQAbCdEfGhIj",
            "https://t.me/send?start=CQAbCdEfGhIj",
            "CQAbCdEfGh\u{0406}j",
        ] {
            assert_eq!(parse_bare_code(text), None, "{text:?}");
        }
    }

    #[test]
    fn message_text_entities_and_buttons() {
        let text = format!("🦋 Чек на 5 USDT ($5.00)\n\nАктивируйте: https://t.me/send?start={CQ}");
        assert_eq!(extract_from_message(&text, &[], &[]), vec![cb_check()]);

        let entity = vec![format!("https://t.me/send?start={CQ}")];
        assert_eq!(extract_from_message("жми", &entity, &[]), vec![cb_check()]);

        let button = vec![format!("https://t.me/xrocket?start={T}")];
        assert_eq!(
            extract_from_message("кнопка", &[], &button),
            vec![ok(Platform::XRocket, LinkKind::Check, T)]
        );
    }

    #[test]
    fn message_order_and_dedup() {
        let text = format!(
            "раз t.me/xrocket?start={T}, два (https://t.me/send?start={CQ}). \
             и снова tg://resolve?domain=CryptoBot&start={CQ}!"
        );
        let entities = vec![format!("https://t.me/xrocket/app?startapp={T}")];
        let buttons = vec![
            format!("https://t.me/send?start={CQ}"),
            format!("https://t.me/xrocket?start={MCI}"),
        ];
        assert_eq!(
            extract_from_message(&text, &entities, &buttons),
            vec![
                ok(Platform::XRocket, LinkKind::Check, T),
                cb_check(),
                ok(Platform::XRocket, LinkKind::Check, MCI),
            ]
        );
    }

    #[test]
    fn message_keeps_errors_but_not_foreign_sites() {
        let text = format!(
            "канал t.me/news, сайт https://example.com, фейк t.me/\u{0421}ryptoBot?start={CQ} \
             тест t.me/CryptoTestnetBot?start={CQ} реф t.me/send?start=r-123 \
             и настоящий t.me/send?start={CQ}"
        );
        assert_eq!(
            extract_from_message(&text, &[], &[]),
            vec![
                Err(LinkError::ForeignBot("\u{0421}ryptoBot".to_owned())),
                Err(LinkError::Testnet),
                Err(LinkError::NotCheckOrInvoice),
                cb_check(),
            ]
        );
    }

    #[test]
    fn message_candidates_respect_boundaries() {
        // Ссылка внутри чужого адреса и «xt.me» — не ссылки Telegram.
        let text = format!(
            "https://evil.com/t.me/send?start={CQ} https://evil.com/?u=https://t.me/send?start={CQ} xt.me/send?start={CQ}"
        );
        assert_eq!(extract_from_message(&text, &[], &[]), vec![]);
        // Опечатка «Ok.t.me/send»: метка поддомена при непустом пути не мешает.
        assert_eq!(
            extract_from_message(&format!("Ok.t.me/send?start={CQ}"), &[], &[]),
            vec![cb_check()]
        );
        // Склеено с кириллицей и тире.
        assert_eq!(
            extract_from_message(
                &format!("чек:https://t.me/send?start={CQ}—спасибо"),
                &[],
                &[]
            ),
            vec![cb_check()]
        );
        // Markdown.
        assert_eq!(
            extract_from_message(&format!("[чек](https://t.me/send?start={CQ})"), &[], &[]),
            vec![cb_check()]
        );
    }

    #[test]
    fn glued_text_after_code_is_cut_off() {
        for text in [
            format!("https://t.me/send?start={CQ}.Спасибо"),
            format!("t.me/send?start={CQ}.Thanks"),
            format!("tg://resolve?domain=send&start={CQ}!USDT"),
            format!("https://t.me/send/app?startapp={CQ}чек"),
        ] {
            assert_eq!(
                extract_from_message(&text, &[], &[]),
                vec![cb_check()],
                "{text}"
            );
        }
        // Обрезка не превращает чужого бота или тестнет в официального.
        assert_eq!(
            extract_from_message(&format!("t.me/CryptoTestnetBot?start={CQ}.x"), &[], &[]),
            vec![Err(LinkError::Testnet)]
        );
        // Код неверной длины так и остаётся не чеком.
        assert_eq!(
            extract_from_message("t.me/send?start=CQabc.x", &[], &[]),
            vec![Err(LinkError::NotCheckOrInvoice)]
        );
        // parse_url для одиночной ссылки (кнопка, сущность) ничего не обрезает.
        assert_eq!(
            parse_url(&format!("https://t.me/send?start={CQ}.x")),
            Err(LinkError::NotCheckOrInvoice)
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(official_platform("SEND"), Some(Platform::CryptoBot));
        assert_eq!(official_platform("TonRocketBot"), Some(Platform::XRocket));
        assert_eq!(official_platform("wallet"), None);
        assert!(is_testnet_bot("CryptoTestnetBot"));
        assert!(!is_testnet_bot("send"));
        assert_eq!(
            classify_start_param(Platform::CryptoBot, CQ),
            Some(LinkKind::Check)
        );
        assert_eq!(classify_start_param(Platform::XRocket, CQ), None);
        assert_eq!(percent_decode("%D0%A1ry%zz%4"), "Сry%zz%4");
    }
}
