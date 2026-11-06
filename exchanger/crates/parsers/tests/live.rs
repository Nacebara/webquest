//! Живые ответы @send и @xrocket (две записи владельца 6 ноября 2026, `fixtures/live_wallet_replies.json`).
//! Смысл каждого ответа размечен по значению; тест проверяет, что парсеры понимают его так же.
//! Все расхождения печатаются разом.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use domain::{Asset, Decimal, Platform};
use parsers::replies::{ActivationReply, InvoiceStatus, classify_activation, parse_balance};
use parsers::screens::{
    CheckCreationReply, InvoiceScreen, classify_check_creation, classify_invoice_screen,
    read_invoice_card,
};
use serde_json::Value;

fn platform(v: &Value) -> Platform {
    match v.as_str().unwrap() {
        "cryptobot" => Platform::CryptoBot,
        "xrocket" => Platform::XRocket,
        other => panic!("unknown platform {other}"),
    }
}

fn dec(v: &Value) -> Decimal {
    Decimal::from_str(v.as_str().unwrap()).unwrap()
}

fn money(v: &[Value]) -> Option<(Decimal, Asset)> {
    match v {
        [amount, asset] => Some((
            dec(amount),
            Asset::from_str(asset.as_str().unwrap()).unwrap(),
        )),
        _ => None,
    }
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

fn expected_activation(e: &Value) -> ActivationReply {
    if let Some(list) = e.as_array() {
        if list[0] == "received_other" {
            return ActivationReply::ReceivedOther {
                amount: dec(&list[1]),
                code: list[2].as_str().unwrap().to_owned(),
            };
        }
        let (amount, asset) = money(&list[1..]).unwrap();
        return match list[0].as_str().unwrap() {
            "received" => ActivationReply::Received { amount, asset },
            "incoming_transfer" => ActivationReply::IncomingTransfer { amount, asset },
            other => panic!("unknown activation {other}"),
        };
    }
    match e.as_str().unwrap() {
        "in_progress" => ActivationReply::InProgress,
        "claim_notice" => ActivationReply::ClaimNotice,
        "already_activated" => ActivationReply::AlreadyActivated,
        "already_activated_by_us" => ActivationReply::AlreadyActivatedByUs,
        "not_for_you" => ActivationReply::NotForYou,
        "incoming_transfer_other" => ActivationReply::IncomingTransferOther,
        "not_found" => ActivationReply::NotFound,
        "unknown" => ActivationReply::Unknown,
        other => panic!("unknown activation {other}"),
    }
}

fn expected_screen(e: &Value) -> InvoiceScreen {
    if let Some(list) = e.as_array() {
        let number = opt_str(&list[1]);
        let amount = money(&list[2..]);
        return match list[0].as_str().unwrap() {
            "choose_coin" => InvoiceScreen::ChooseCoin {
                number,
                // Сумму в фиате проверяет карточка (invoice_card).
                fiat: None,
            },
            "confirm" => InvoiceScreen::Confirm { number, amount },
            "choose_method" => InvoiceScreen::ChooseMethod { number, amount },
            "paid" => InvoiceScreen::Paid { number, amount },
            other => panic!("unknown invoice screen {other}"),
        };
    }
    match e.as_str().unwrap() {
        "progress" => InvoiceScreen::Progress,
        "trust_recipient" => InvoiceScreen::TrustRecipient,
        "insufficient_funds" => InvoiceScreen::InsufficientFunds,
        "open_amount" => InvoiceScreen::OpenAmount,
        "already_paid" => InvoiceScreen::AlreadyPaid,
        other => panic!("unknown invoice screen {other}"),
    }
}

/// Без фиата: в экране выбора монеты сравниваем только вид и номер.
fn screen_shape(s: InvoiceScreen) -> InvoiceScreen {
    match s {
        InvoiceScreen::ChooseCoin { number, .. } => {
            InvoiceScreen::ChooseCoin { number, fiat: None }
        }
        other => other,
    }
}

#[test]
fn live_wallet_replies() {
    let doc: Value =
        serde_json::from_str(include_str!("fixtures/live_wallet_replies.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    let mut failures = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let p = platform(&c["platform"]);
        let text = c["text"].as_str().unwrap();
        let urls: Vec<String> = c["urls"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|u| u.as_str().map(str::to_owned))
            .collect();
        let buttons: Vec<String> = c["buttons"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap().iter())
            .map(|b| b.as_str().unwrap().to_owned())
            .collect();
        let e = &c["expect"];
        let (got, want) = match c["kind"].as_str().unwrap() {
            "activation" => (
                format!("{:?}", classify_activation(p, text)),
                format!("{:?}", expected_activation(e)),
            ),
            "invoice_screen" => (
                format!("{:?}", screen_shape(classify_invoice_screen(p, text))),
                format!("{:?}", expected_screen(e)),
            ),
            "invoice_card" => {
                let card = read_invoice_card(p, text, &buttons);
                let status = match e["status"].as_str().unwrap() {
                    "active" => InvoiceStatus::Active,
                    "paid" => InvoiceStatus::Paid,
                    other => panic!("unknown status {other}"),
                };
                let fiat = e["fiat"]
                    .as_array()
                    .map(|f| (dec(&f[0]), f[1].as_str().unwrap().to_owned()));
                let amount = e["amount"].as_array().and_then(|a| money(a));
                // Разовость проверяем там, где она размечена (многоразовый счёт — `false`).
                let single = e["single_use"].as_bool();
                let got_single = single.and(card.single_use);
                (
                    format!(
                        "{:?} {:?} {:?} {:?} {} {:?}",
                        card.status,
                        card.number,
                        card.fiat,
                        card.amount,
                        card.insufficient_funds,
                        got_single
                    ),
                    format!(
                        "{:?} {:?} {:?} {:?} {} {:?}",
                        status,
                        opt_str(&e["number"]),
                        fiat,
                        amount,
                        e["insufficient_funds"].as_bool().unwrap(),
                        single
                    ),
                )
            }
            "check_creation" => {
                let got = match classify_check_creation(p, text, &urls) {
                    CheckCreationReply::Created(c) => {
                        format!("created {} {:?}", c.param, c.amount)
                    }
                    other => format!("{other:?}"),
                };
                let want = match (e.as_str(), e.as_array()) {
                    (Some("ask_activations"), _) => "AskActivations".to_owned(),
                    (Some("insufficient_funds"), _) => "InsufficientFunds".to_owned(),
                    (Some("unknown"), _) => "Unknown".to_owned(),
                    (_, Some(list)) if list[0] == "created" => format!(
                        "created {} {:?}",
                        list[1].as_str().unwrap(),
                        money(&list[2..])
                    ),
                    (_, Some(list)) if list[0] == "confirm" => format!(
                        "{:?}",
                        CheckCreationReply::Confirm {
                            amount: money(&list[1..])
                        }
                    ),
                    other => panic!("unknown check creation {other:?}"),
                };
                (got, want)
            }
            "balance" => {
                let got: Vec<String> = parse_balance(p, text)
                    .into_iter()
                    .filter(|b| b.asset == Asset::Usdt)
                    .map(|b| format!("{} {} held {}", b.amount, b.asset.code(), b.held))
                    .collect();
                let want: Vec<String> = e
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| {
                        format!(
                            "{} {} held {}",
                            dec(&l[0]),
                            l[1].as_str().unwrap(),
                            dec(&l[2])
                        )
                    })
                    .collect();
                (format!("{got:?}"), format!("{want:?}"))
            }
            other => panic!("unknown kind {other}"),
        };
        if got != want {
            let head: String = text.chars().take(90).collect();
            failures.push(format!(
                "#{i} {} {p:?}: {head:?}\n    got  {got}\n    want {want}",
                c["kind"]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} live cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
