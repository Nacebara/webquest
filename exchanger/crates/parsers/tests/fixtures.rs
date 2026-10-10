//! Фикстуры парсеров: `tests/fixtures/*.json`. Каждый файл — `{ "about": …, "cases": [ … ] }`,
//! каждая запись — вход и ожидаемый результат.
//!
//! Новый (живой) текст бота кошелька — новая запись в нужном файле, а не правка регулярки под
//! один текст (CLAUDE.md, «Юзербот», п. 12). Тест проверяет все записи и печатает все
//! расхождения разом.

use std::str::FromStr;

use domain::{Asset, Decimal, Platform};
use parsers::amount::{parse_fiat, parse_money};
use parsers::links::{extract_from_message, parse_bare_code, parse_url};
use parsers::replies::{
    ActivationReply, InvoicePayReply, InvoiceStatus, classify_activation, classify_invoice_payment,
    parse_balance, parse_created_check, parse_invoice_card,
};
use parsers::{LinkError, LinkKind, WalletLink};
use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Deserialize)]
struct File<T> {
    #[allow(dead_code)]
    about: String,
    cases: Vec<T>,
}

fn load<T: DeserializeOwned>(json: &str) -> Vec<T> {
    serde_json::from_str::<File<T>>(json)
        .expect("fixture file is valid JSON of the expected shape")
        .cases
}

fn platform(s: &str) -> Platform {
    Platform::from_db_name(s).unwrap_or_else(|| panic!("unknown platform in fixture: {s}"))
}

fn asset(s: &str) -> Asset {
    Asset::from_str(s).unwrap_or_else(|_| panic!("unknown asset in fixture: {s}"))
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str_exact(s).unwrap_or_else(|_| panic!("bad decimal in fixture: {s}"))
}

fn money(pair: &Option<(String, String)>) -> Option<(Decimal, Asset)> {
    pair.as_ref().map(|(a, c)| (dec(a), asset(c)))
}

fn fiat(pair: &Option<(String, String)>) -> Option<(Decimal, String)> {
    pair.as_ref().map(|(a, c)| (dec(a), c.clone()))
}

/// Собрать все расхождения и упасть один раз со списком.
#[derive(Default)]
struct Report(Vec<String>);

impl Report {
    fn check<T: PartialEq + std::fmt::Debug>(&mut self, what: &str, input: &str, got: T, want: T) {
        if got != want {
            self.0.push(format!(
                "{what} {input:?}:\n    got  {got:?}\n    want {want:?}"
            ));
        }
    }

    fn finish(self, name: &str, total: usize) {
        assert!(
            self.0.is_empty(),
            "{name}: {} of {total} fixtures failed\n{}",
            self.0.len(),
            self.0.join("\n")
        );
        assert!(total > 0, "{name}: no fixtures loaded");
    }
}

// ---- ссылки -------------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
struct LinkOk {
    platform: String,
    kind: String,
    param: String,
}

impl LinkOk {
    fn link(&self) -> WalletLink {
        let kind = match self.kind.as_str() {
            "check" => LinkKind::Check,
            "invoice" => LinkKind::Invoice,
            other => panic!("unknown link kind in fixture: {other}"),
        };
        WalletLink {
            platform: platform(&self.platform),
            kind,
            param: self.param.clone(),
        }
    }
}

/// Ожидаемый результат разбора ссылки: `ok` или `err` (+ `bot` для `foreign_bot`).
/// `input` есть только в links.json; в messages.json это элементы списка `expect`.
#[derive(Deserialize, Debug)]
struct LinkCase {
    #[serde(default)]
    input: String,
    ok: Option<LinkOk>,
    err: Option<String>,
    bot: Option<String>,
}

impl LinkCase {
    fn result(&self) -> Result<WalletLink, LinkError> {
        if let Some(ok) = &self.ok {
            return Ok(ok.link());
        }
        Err(match self.err.as_deref() {
            Some("not_wallet_link") => LinkError::NotWalletLink,
            Some("foreign_bot") => {
                LinkError::ForeignBot(self.bot.clone().expect("foreign_bot fixture names the bot"))
            }
            Some("testnet") => LinkError::Testnet,
            Some("not_check_or_invoice") => LinkError::NotCheckOrInvoice,
            other => panic!("fixture needs ok or a known err, got {other:?}"),
        })
    }
}

#[test]
fn links_fixtures() {
    let cases: Vec<LinkCase> = load(include_str!("fixtures/links.json"));
    let mut report = Report::default();
    for c in &cases {
        report.check("parse_url", &c.input, parse_url(&c.input), c.result());
        // Каждая распознанная ссылка переживает круг через каноническую форму.
        if let Ok(link) = parse_url(&c.input) {
            report.check(
                "canonical round trip",
                &c.input,
                parse_url(&link.canonical_url()),
                Ok(link.clone()),
            );
        }
    }
    report.finish("links.json", cases.len());
}

#[derive(Deserialize)]
struct BareCase {
    input: String,
    ok: Option<LinkOk>,
}

#[test]
fn bare_code_fixtures() {
    let cases: Vec<BareCase> = load(include_str!("fixtures/bare_codes.json"));
    let mut report = Report::default();
    for c in cases.iter() {
        let want = c.ok.as_ref().map(LinkOk::link);
        report.check("parse_bare_code", &c.input, parse_bare_code(&c.input), want);
    }
    report.finish("bare_codes.json", cases.len());
}

#[derive(Deserialize)]
struct MessageCase {
    text: String,
    #[serde(default)]
    entity_urls: Vec<String>,
    #[serde(default)]
    button_urls: Vec<String>,
    expect: Vec<LinkCase>,
}

#[test]
fn message_fixtures() {
    let cases: Vec<MessageCase> = load(include_str!("fixtures/messages.json"));
    let mut report = Report::default();
    for c in &cases {
        let want: Vec<_> = c.expect.iter().map(LinkCase::result).collect();
        report.check(
            "extract_from_message",
            &c.text,
            extract_from_message(&c.text, &c.entity_urls, &c.button_urls),
            want,
        );
    }
    report.finish("messages.json", cases.len());
}

// ---- суммы --------------------------------------------------------------------------------

#[derive(Deserialize)]
struct AmountCase {
    platform: String,
    text: String,
    money: Option<(String, String)>,
}

#[test]
fn amount_fixtures() {
    let cases: Vec<AmountCase> = load(include_str!("fixtures/amounts.json"));
    let mut report = Report::default();
    for c in &cases {
        report.check(
            "parse_money",
            &c.text,
            parse_money(&c.text, platform(&c.platform)),
            money(&c.money),
        );
    }
    report.finish("amounts.json", cases.len());
}

#[derive(Deserialize)]
struct FiatCase {
    text: String,
    fiat: Option<(String, String)>,
}

#[test]
fn fiat_fixtures() {
    let cases: Vec<FiatCase> = load(include_str!("fixtures/fiat.json"));
    let mut report = Report::default();
    for c in &cases {
        report.check("parse_fiat", &c.text, parse_fiat(&c.text), fiat(&c.fiat));
    }
    report.finish("fiat.json", cases.len());
}

// ---- ответы ботов -------------------------------------------------------------------------

#[derive(Deserialize)]
struct ActivationCase {
    platform: String,
    text: String,
    class: String,
    amount: Option<String>,
    asset: Option<String>,
}

impl ActivationCase {
    fn expected(&self) -> ActivationReply {
        let amount = || {
            (
                dec(self.amount.as_deref().expect("amount for this class")),
                asset(self.asset.as_deref().expect("asset for this class")),
            )
        };
        match self.class.as_str() {
            "received" => {
                let (amount, asset) = amount();
                ActivationReply::Received { amount, asset }
            }
            "incoming_transfer" => {
                let (amount, asset) = amount();
                ActivationReply::IncomingTransfer { amount, asset }
            }
            "already_activated" => ActivationReply::AlreadyActivated,
            "not_found" => ActivationReply::NotFound,
            "not_for_you" => ActivationReply::NotForYou,
            "needs_subscription" => ActivationReply::NeedsSubscription,
            "captcha" => ActivationReply::Captcha,
            "password_required" => ActivationReply::PasswordRequired,
            "premium_only" => ActivationReply::PremiumOnly,
            "in_progress" => ActivationReply::InProgress,
            "claim_notice" => ActivationReply::ClaimNotice,
            "unknown" => ActivationReply::Unknown,
            other => panic!("unknown activation class in fixture: {other}"),
        }
    }
}

#[test]
fn activation_fixtures() {
    let cases: Vec<ActivationCase> = load(include_str!("fixtures/activation.json"));
    let mut report = Report::default();
    for c in &cases {
        report.check(
            "classify_activation",
            &c.text,
            classify_activation(platform(&c.platform), &c.text),
            c.expected(),
        );
    }
    // Каждый класс покрыт хотя бы одной фикстурой.
    for class in [
        "received",
        "already_activated",
        "not_found",
        "not_for_you",
        "needs_subscription",
        "captcha",
        "password_required",
        "premium_only",
        "in_progress",
        "claim_notice",
        "incoming_transfer",
        "unknown",
    ] {
        if !cases.iter().any(|c| c.class == class) {
            report.0.push(format!("no fixture for class {class}"));
        }
    }
    report.finish("activation.json", cases.len());
}

#[derive(Deserialize)]
struct CreatedExpect {
    url: String,
    param: String,
    money: Option<(String, String)>,
}

#[derive(Deserialize)]
struct CreatedCase {
    platform: String,
    text: String,
    buttons: Vec<String>,
    expect: Option<CreatedExpect>,
}

#[test]
fn created_check_fixtures() {
    let cases: Vec<CreatedCase> = load(include_str!("fixtures/created_checks.json"));
    let mut report = Report::default();
    for c in &cases {
        let got = parse_created_check(platform(&c.platform), &c.text, &c.buttons)
            .map(|cc| (cc.url, cc.param, cc.amount));
        let want = c
            .expect
            .as_ref()
            .map(|e| (e.url.clone(), e.param.clone(), money(&e.money)));
        report.check("parse_created_check", &c.text, got, want);
    }
    report.finish("created_checks.json", cases.len());
}

#[derive(Deserialize)]
struct CardCase {
    platform: String,
    text: String,
    status: String,
    money: Option<(String, String)>,
    fiat: Option<(String, String)>,
    single_use: Option<bool>,
    description: Option<String>,
}

#[test]
fn invoice_card_fixtures() {
    let cases: Vec<CardCase> = load(include_str!("fixtures/invoice_cards.json"));
    let mut report = Report::default();
    for c in &cases {
        let card = parse_invoice_card(platform(&c.platform), &c.text);
        let status = match c.status.as_str() {
            "active" => InvoiceStatus::Active,
            "paid" => InvoiceStatus::Paid,
            "expired" => InvoiceStatus::Expired,
            "unknown" => InvoiceStatus::Unknown,
            other => panic!("unknown invoice status in fixture: {other}"),
        };
        report.check(
            "parse_invoice_card",
            &c.text,
            (
                card.status,
                card.amount,
                card.fiat,
                card.single_use,
                card.description,
            ),
            (
                status,
                money(&c.money),
                fiat(&c.fiat),
                c.single_use,
                c.description.clone(),
            ),
        );
    }
    report.finish("invoice_cards.json", cases.len());
}

#[derive(Deserialize)]
struct PaymentCase {
    platform: String,
    text: String,
    class: String,
}

#[test]
fn invoice_payment_fixtures() {
    let cases: Vec<PaymentCase> = load(include_str!("fixtures/invoice_payments.json"));
    let mut report = Report::default();
    for c in &cases {
        let want = match c.class.as_str() {
            "paid" => InvoicePayReply::Paid,
            "already_paid" => InvoicePayReply::AlreadyPaid,
            "expired" => InvoicePayReply::Expired,
            "insufficient_funds" => InvoicePayReply::InsufficientFunds,
            "unknown" => InvoicePayReply::Unknown,
            other => panic!("unknown payment class in fixture: {other}"),
        };
        report.check(
            "classify_invoice_payment",
            &c.text,
            classify_invoice_payment(platform(&c.platform), &c.text),
            want,
        );
    }
    report.finish("invoice_payments.json", cases.len());
}

#[derive(Deserialize)]
struct BalanceCase {
    platform: String,
    text: String,
    expect: Vec<(String, String)>,
}

#[test]
fn balance_fixtures() {
    let cases: Vec<BalanceCase> = load(include_str!("fixtures/balances.json"));
    let mut report = Report::default();
    for c in &cases {
        let got: Vec<(Decimal, Asset)> = parse_balance(platform(&c.platform), &c.text)
            .into_iter()
            .map(|b| (b.amount, b.asset))
            .collect();
        let want: Vec<(Decimal, Asset)> =
            c.expect.iter().map(|(a, s)| (dec(a), asset(s))).collect();
        report.check("parse_balance", &c.text, got, want);
    }
    report.finish("balances.json", cases.len());
}
