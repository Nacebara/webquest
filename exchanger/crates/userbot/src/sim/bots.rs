//! Поведение ботов @send (CryptoBot) и @xrocket в симуляторе: что бот отвечает на текст,
//! нажатие кнопки, инлайн-запрос. Функции меняют мир и возвращают сообщения к публикации;
//! сбои и задержки накладывает транспорт.

use std::str::FromStr;

use domain::{Asset, Decimal, Money, Platform};

use super::model::{
    AccountId, CheckOrigin, CheckSpec, CheckStatus, ClaimOutcome, InvoiceAmount, InvoiceSpec,
    SimError, SimInvoiceStatus, WebAppPayment,
};
use super::state::{
    Dialog, Draft, InlineItem, InlineQuery, Op, Outbound, PIN_ATTEMPTS, WorldState,
};
use super::texts;
use crate::transport::{
    ButtonKind, CallbackAnswer, Chat, InlineResult, InlineResults, RawButton, TransportError,
};

/// Хост мини-приложения оплаты CryptoBot в симуляторе (`.invalid` — заведомо не настоящий).
pub(crate) const WEBAPP_HOST: &str = "pay.send.sim.invalid";

fn callback(text: &str, data: &str) -> RawButton {
    RawButton {
        text: text.to_owned(),
        kind: ButtonKind::Callback(data.as_bytes().to_vec()),
    }
}

fn url_button(text: &str, url: &str) -> RawButton {
    RawButton {
        text: text.to_owned(),
        kind: ButtonKind::Url(url.to_owned()),
    }
}

fn rpc(name: &str) -> TransportError {
    TransportError::Rpc {
        code: 400,
        name: name.to_owned(),
    }
}

fn reply(account: AccountId, platform: Platform, draft: Draft) -> Outbound {
    Outbound {
        account,
        chat: Chat::WalletBot(platform),
        op: Op::New(draft),
    }
}

fn edit(account: AccountId, platform: Platform, msg_id: i32, draft: Draft) -> Outbound {
    Outbound {
        account,
        chat: Chat::WalletBot(platform),
        op: Op::Edit(msg_id, draft),
    }
}

/// Сумма, введённая человеком: `10`, `10.5`, `10,5`. Только положительная.
pub(crate) fn parse_user_amount(s: &str) -> Option<Decimal> {
    let s = s.trim().replace(',', ".");
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let d = Decimal::from_str(&s).ok()?;
    (d > Decimal::ZERO).then(|| d.normalize())
}

fn is_invoice_code(platform: Platform, code: &str) -> bool {
    match platform {
        Platform::CryptoBot => code.starts_with("IV"),
        Platform::XRocket => code.starts_with("inv_"),
    }
}

// ---------- текст в чат бота ----------

/// Аккаунт написал боту текст (уже сохранён как исходящее сообщение).
pub(crate) fn on_text(
    st: &mut WorldState,
    acc: AccountId,
    platform: Platform,
    text: &str,
    random_id: i64,
) -> Vec<Outbound> {
    let trimmed = text.trim();
    if trimmed.starts_with('/') {
        st.set_dialog(acc, platform, Dialog::Idle);
        let mut parts = trimmed.split_whitespace();
        let cmd = parts
            .next()
            .unwrap_or_default()
            .split('@')
            .next()
            .unwrap_or_default();
        let arg = parts.next();
        return match (cmd, arg, platform) {
            ("/start", None, _) => {
                vec![reply(acc, platform, Draft::text(texts::welcome(platform)))]
            }
            ("/start", Some(p), _) if is_invoice_code(platform, p) => {
                vec![invoice_card(st, acc, platform, p)]
            }
            ("/start", Some(p), _) => claim(st, acc, platform, p, None).1,
            ("/wallet", _, _) => vec![wallet(st, acc, platform)],
            ("/checks", _, Platform::CryptoBot) => vec![reply(
                acc,
                platform,
                Draft::text(texts::CB_CHECKS_MENU).with_buttons(vec![
                    vec![callback("Создать чек", "cb:checks:create")],
                    vec![callback("Мои чеки", "cb:checks:list")],
                ]),
            )],
            ("/cheques", _, Platform::XRocket) => vec![reply(
                acc,
                platform,
                Draft::text(texts::XR_CHEQUES_MENU).with_buttons(vec![
                    vec![callback("Персональный", "xr:cheques:personal")],
                    vec![callback("Мульти-чек", "xr:cheques:multi")],
                ]),
            )],
            _ => vec![reply(acc, platform, Draft::text(texts::UNKNOWN_COMMAND))],
        };
    }
    if trimmed.ends_with("Кошелёк") {
        st.set_dialog(acc, platform, Dialog::Idle);
        return vec![wallet(st, acc, platform)];
    }
    match st.dialog(acc, platform) {
        Dialog::AwaitCheckAmount(asset) => {
            menu_amount(st, acc, platform, asset, trimmed, random_id)
        }
        Dialog::AwaitCheckPassword(code) => claim(st, acc, platform, &code, Some(trimmed)).1,
        Dialog::Idle => vec![reply(acc, platform, Draft::text(texts::UNKNOWN_COMMAND))],
    }
}

fn wallet(st: &WorldState, acc: AccountId, platform: Platform) -> Outbound {
    let balances: Vec<Money> = Asset::ALL
        .iter()
        .map(|a| st.balance_money(acc, platform, *a))
        .collect();
    reply(
        acc,
        platform,
        Draft::text(texts::wallet(platform, &balances)),
    )
}

// ---------- активация чека ----------

/// `/start <код>` от аккаунта `acc` (или ввод пароля, если `password` задан).
/// Возвращает исход и сообщения: ответ активирующему и уведомление создателю.
pub(crate) fn claim(
    st: &mut WorldState,
    acc: AccountId,
    platform: Platform,
    code: &str,
    password: Option<&str>,
) -> (ClaimOutcome, Vec<Outbound>) {
    let (outcome, notice) = evaluate_claim(st, acc, platform, code, password);
    let text = match outcome {
        ClaimOutcome::Received(m) => texts::received(platform, &m),
        ClaimOutcome::AlreadyActivated => texts::ALREADY_ACTIVATED.to_owned(),
        ClaimOutcome::NotFound => texts::not_found(code).to_owned(),
        ClaimOutcome::NotForYou => texts::NOT_FOR_YOU.to_owned(),
        ClaimOutcome::NeedsSubscription => {
            let channel = st
                .checks
                .get(code)
                .and_then(|c| c.subscription.clone())
                .unwrap_or_default();
            texts::needs_subscription(&channel)
        }
        ClaimOutcome::Captcha => texts::CAPTCHA.to_owned(),
        ClaimOutcome::PasswordRequired => texts::PASSWORD_PROMPT.to_owned(),
        ClaimOutcome::WrongPassword => texts::WRONG_PASSWORD.to_owned(),
        ClaimOutcome::PremiumOnly => texts::PREMIUM_ONLY.to_owned(),
        ClaimOutcome::OwnCheck => match st.checks.get(code) {
            Some(c) => texts::own_check(platform, &c.amount, &c.url),
            None => texts::CHECK_NOT_FOUND.to_owned(),
        },
    };
    let dialog = match outcome {
        ClaimOutcome::PasswordRequired | ClaimOutcome::WrongPassword => {
            Dialog::AwaitCheckPassword(code.to_owned())
        }
        _ => Dialog::Idle,
    };
    st.set_dialog(acc, platform, dialog);
    let mut outs = vec![reply(acc, platform, Draft::text(text))];
    outs.extend(notice);
    (outcome, outs)
}

fn evaluate_claim(
    st: &mut WorldState,
    acc: AccountId,
    platform: Platform,
    code: &str,
    password: Option<&str>,
) -> (ClaimOutcome, Option<Outbound>) {
    let now = st.now_ms;
    let window = st.xrocket_fresh_window_ms;
    let Some((premium, channels)) = st
        .accounts
        .get(&acc)
        .map(|a| (a.premium, a.channels.clone()))
    else {
        return (ClaimOutcome::NotFound, None);
    };
    let Some(check) = st.checks.get_mut(code) else {
        return (ClaimOutcome::NotFound, None);
    };
    if check.platform != platform || check.status == CheckStatus::Cancelled {
        return (ClaimOutcome::NotFound, None);
    }
    // xRocket: «не найден» на совсем свежий чек, со второй попытки — как обычно
    // (lovec ledger.rs:19-20, AUDIT-2.md:1850).
    if platform == Platform::XRocket
        && !check.fresh_not_found_served
        && window.is_some_and(|w| now - check.created_at_ms < w)
    {
        check.fresh_not_found_served = true;
        return (ClaimOutcome::NotFound, None);
    }
    if check.creator == Some(acc) {
        return (ClaimOutcome::OwnCheck, None);
    }
    if check.status == CheckStatus::Claimed || check.claimed_by.contains(&acc) {
        return (ClaimOutcome::AlreadyActivated, None);
    }
    if check.for_user.is_some_and(|u| u != acc) {
        return (ClaimOutcome::NotForYou, None);
    }
    if check.premium_only && !premium {
        return (ClaimOutcome::PremiumOnly, None);
    }
    if check
        .subscription
        .as_ref()
        .is_some_and(|ch| !channels.contains(ch))
    {
        return (ClaimOutcome::NeedsSubscription, None);
    }
    if check.captcha {
        return (ClaimOutcome::Captcha, None);
    }
    if let Some(expected) = &check.password {
        match password {
            None => return (ClaimOutcome::PasswordRequired, None),
            Some(p) if p != expected => return (ClaimOutcome::WrongPassword, None),
            Some(_) => {}
        }
    }
    check.claimed_by.push(acc);
    if check.claimed_by.len() >= usize::try_from(check.activations).unwrap_or(usize::MAX) {
        check.status = CheckStatus::Claimed;
    }
    let (amount, creator, url) = (check.amount, check.creator, check.url.clone());
    // Аккаунт проверен в начале, зачисление не падает.
    let _ = st.credit(acc, platform, amount);
    let notice = creator.map(|c| {
        let who = st.display(acc);
        reply(
            c,
            platform,
            Draft::text(texts::claim_notice(platform, &amount, &who, &url)).with_urls(vec![url]),
        )
    });
    (ClaimOutcome::Received(amount), notice)
}

// ---------- меню чеков ----------

fn menu_amount(
    st: &mut WorldState,
    acc: AccountId,
    platform: Platform,
    asset: Asset,
    text: &str,
    random_id: i64,
) -> Vec<Outbound> {
    let Some(m) = parse_user_amount(text).and_then(|d| Money::new(d, asset).ok()) else {
        return vec![reply(acc, platform, Draft::text(texts::BAD_AMOUNT))];
    };
    st.set_dialog(acc, platform, Dialog::Idle);
    let spec = CheckSpec::new(platform, m).by(acc);
    match st.insert_check(spec, CheckOrigin::Menu { random_id }) {
        Ok(check) => vec![reply(
            acc,
            platform,
            Draft::text(texts::check_created(platform, &m, &check.url))
                .with_urls(vec![check.url.clone()])
                .with_buttons(vec![vec![url_button("Открыть чек", &check.url)]]),
        )],
        Err(SimError::InsufficientFunds { available, .. }) => vec![reply(
            acc,
            platform,
            Draft::text(texts::insufficient_for_check(platform, &available)),
        )],
        Err(_) => vec![reply(acc, platform, Draft::text(texts::BAD_AMOUNT))],
    }
}

fn active_checks(st: &WorldState, acc: AccountId, platform: Platform) -> Draft {
    let lines: Vec<(Money, String)> = st
        .check_order
        .iter()
        .rev()
        .filter_map(|code| st.checks.get(code))
        .filter(|c| {
            c.creator == Some(acc) && c.platform == platform && c.status == CheckStatus::Active
        })
        .take(20)
        .map(|c| (c.amount, c.url.clone()))
        .collect();
    if lines.is_empty() {
        return Draft::text(texts::NO_ACTIVE_CHECKS);
    }
    let buttons = lines
        .iter()
        .map(|(m, url)| {
            vec![url_button(
                &format!("Чек на {}", texts::money(platform, m)),
                url,
            )]
        })
        .collect();
    Draft::text(texts::active_checks(platform, &lines))
        .with_urls(lines.iter().map(|(_, u)| u.clone()).collect())
        .with_buttons(buttons)
}

fn choose_asset(platform: Platform, prefix: &str) -> Draft {
    let row = |a: Asset| {
        vec![callback(
            a.platform_code(platform),
            &format!("{prefix}:asset:{}", a.code()),
        )]
    };
    Draft::text(texts::CHOOSE_CHECK_ASSET)
        .with_buttons(Asset::ALL.iter().map(|a| row(*a)).collect())
}

// ---------- нажатия ----------

/// Нажатие callback-кнопки сообщения `msg_id`.
pub(crate) fn on_press(
    st: &mut WorldState,
    acc: AccountId,
    chat: Chat,
    msg_id: i32,
    data: &[u8],
) -> Result<(CallbackAnswer, Vec<Outbound>), TransportError> {
    let msg = st
        .find_message(acc, chat, msg_id)
        .ok_or_else(|| rpc("MESSAGE_ID_INVALID"))?;
    let has_button = msg
        .buttons
        .iter()
        .flatten()
        .any(|b| matches!(&b.kind, ButtonKind::Callback(d) if d.as_slice() == data));
    let Chat::WalletBot(platform) = chat else {
        return Err(rpc("DATA_INVALID"));
    };
    if !has_button || msg.out {
        return Err(rpc("DATA_INVALID"));
    }
    let data = std::str::from_utf8(data).map_err(|_| rpc("DATA_INVALID"))?;
    let parts: Vec<&str> = data.split(':').collect();
    let ok = |outs: Vec<Outbound>| Ok((CallbackAnswer::default(), outs));
    match parts.as_slice() {
        ["cb", "checks", "create"] => ok(vec![edit(
            acc,
            platform,
            msg_id,
            choose_asset(platform, "cb:checks"),
        )]),
        ["xr", "cheques", "create"] => ok(vec![edit(
            acc,
            platform,
            msg_id,
            choose_asset(platform, "xr:cheques"),
        )]),
        ["cb", "checks", "list"] | ["xr", "cheques", "list"] => ok(vec![edit(
            acc,
            platform,
            msg_id,
            active_checks(st, acc, platform),
        )]),
        ["cb", "checks", "asset", code] | ["xr", "cheques", "asset", code] => {
            let asset = Asset::from_str(code).map_err(|_| rpc("DATA_INVALID"))?;
            st.set_dialog(acc, platform, Dialog::AwaitCheckAmount(asset));
            let available = st.balance_money(acc, platform, asset);
            ok(vec![edit(
                acc,
                platform,
                msg_id,
                Draft::text(texts::ask_check_amount(platform, asset, &available)),
            )])
        }
        ["xr", "cheques", "personal"] => ok(vec![edit(
            acc,
            platform,
            msg_id,
            Draft::text(texts::XR_PERSONAL).with_buttons(vec![
                vec![callback("Создать чек", "xr:cheques:create")],
                vec![callback("Мои чеки", "xr:cheques:list")],
            ]),
        )]),
        ["xr", "cheques", "multi"] => Ok((
            CallbackAnswer {
                message: Some(texts::MULTI_UNSUPPORTED.to_owned()),
                alert: true,
                url: None,
            },
            Vec::new(),
        )),
        ["inv", code, "asset", asset] => {
            let asset = Asset::from_str(asset).map_err(|_| rpc("DATA_INVALID"))?;
            ok(vec![edit(
                acc,
                platform,
                msg_id,
                pay_screen(st, acc, platform, code, asset),
            )])
        }
        ["inv", code, "pay", asset] if platform == Platform::XRocket => {
            let asset = Asset::from_str(asset).map_err(|_| rpc("DATA_INVALID"))?;
            let (text, notice) = match pay_invoice(st, acc, code, asset) {
                Ok((paid, notice)) => (texts::invoice_paid(platform, code, &paid), notice),
                Err(e) => (pay_error_text(platform, &e), None),
            };
            let mut outs = vec![edit(acc, platform, msg_id, Draft::text(text))];
            outs.extend(notice);
            ok(outs)
        }
        _ => Err(rpc("DATA_INVALID")),
    }
}

// ---------- счета ----------

fn card_view(inv: &super::SimInvoice) -> texts::CardView<'_> {
    let amount_line = match &inv.amount {
        InvoiceAmount::Crypto(m) => texts::money(inv.platform, m),
        InvoiceAmount::Fiat { amount, currency } => {
            format!("{} {currency}", texts::amount(*amount))
        }
    };
    texts::CardView {
        platform: inv.platform,
        code: &inv.code,
        amount_line,
        description: inv.description.as_deref(),
        single_use: inv.single_use,
        status_line: texts::status_line(inv.status),
    }
}

/// Карточка счёта на `/start IV…` / `/start inv_…`.
fn invoice_card(st: &WorldState, acc: AccountId, platform: Platform, code: &str) -> Outbound {
    let Some(inv) = st.invoices.get(code).filter(|i| i.platform == platform) else {
        return reply(acc, platform, Draft::text(texts::INVOICE_NOT_FOUND));
    };
    let active = inv.status == SimInvoiceStatus::Active;
    let view = card_view(inv);
    let mut draft = Draft::text(texts::invoice_card(&view, active));
    if active {
        let row: Vec<RawButton> = inv
            .accepted_assets
            .iter()
            .filter(|a| st.invoice_due(inv, **a).is_some())
            .map(|a| {
                callback(
                    a.platform_code(platform),
                    &format!("inv:{code}:asset:{}", a.code()),
                )
            })
            .collect();
        if !row.is_empty() {
            draft = draft.with_buttons(vec![row]);
        }
    }
    reply(acc, platform, draft)
}

/// Экран после выбора валюты: сумма к оплате и кнопка «Оплатить».
fn pay_screen(
    st: &WorldState,
    acc: AccountId,
    platform: Platform,
    code: &str,
    asset: Asset,
) -> Draft {
    let Some(inv) = st.invoices.get(code) else {
        return Draft::text(texts::INVOICE_NOT_FOUND);
    };
    match inv.status {
        SimInvoiceStatus::Paid => return Draft::text(texts::INVOICE_ALREADY_PAID),
        SimInvoiceStatus::Expired => return Draft::text(texts::INVOICE_EXPIRED),
        SimInvoiceStatus::Active => {}
    }
    let Some(due) = st.invoice_due(inv, asset) else {
        return Draft::text(texts::INVOICE_NOT_FOUND);
    };
    let available = st.balance_money(acc, platform, asset);
    let view = card_view(inv);
    let pay = match platform {
        Platform::CryptoBot => RawButton {
            text: "Оплатить".to_owned(),
            kind: ButtonKind::WebApp(webapp_url(code, asset)),
        },
        Platform::XRocket => callback("Оплатить", &format!("inv:{code}:pay:{}", asset.code())),
    };
    Draft::text(texts::invoice_pay_screen(&view, &due, &available)).with_buttons(vec![vec![pay]])
}

pub(crate) fn webapp_url(code: &str, asset: Asset) -> String {
    format!(
        "https://{WEBAPP_HOST}/invoice?code={code}&asset={}",
        asset.code()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PayError {
    NotFound,
    AlreadyPaid,
    Expired,
    Own,
    Insufficient { available: Money, due: Money },
}

fn pay_error_text(platform: Platform, e: &PayError) -> String {
    match e {
        PayError::NotFound => texts::INVOICE_NOT_FOUND.to_owned(),
        PayError::AlreadyPaid => texts::INVOICE_ALREADY_PAID.to_owned(),
        PayError::Expired => texts::INVOICE_EXPIRED.to_owned(),
        PayError::Own => texts::INVOICE_OWN.to_owned(),
        PayError::Insufficient { available, due } => {
            texts::invoice_insufficient(platform, available, due)
        }
    }
}

/// Оплатить счёт с баланса `payer`: списание, зачисление создателю, статус.
pub(crate) fn pay_invoice(
    st: &mut WorldState,
    payer: AccountId,
    code: &str,
    asset: Asset,
) -> Result<(Money, Option<Outbound>), PayError> {
    let inv = st.invoices.get(code).ok_or(PayError::NotFound)?;
    match inv.status {
        SimInvoiceStatus::Paid => return Err(PayError::AlreadyPaid),
        SimInvoiceStatus::Expired => return Err(PayError::Expired),
        SimInvoiceStatus::Active => {}
    }
    if inv.creator == Some(payer) {
        return Err(PayError::Own);
    }
    let platform = inv.platform;
    let creator = inv.creator;
    let due = st.invoice_due(inv, asset).ok_or(PayError::NotFound)?;
    match st.debit(payer, platform, due) {
        Ok(()) => {}
        Err(SimError::InsufficientFunds { available, .. }) => {
            return Err(PayError::Insufficient { available, due });
        }
        Err(_) => return Err(PayError::NotFound),
    }
    if let Some(c) = creator {
        let _ = st.credit(c, platform, due);
    }
    let now = st.now_ms;
    if let Some(inv) = st.invoices.get_mut(code) {
        inv.payments.push(super::InvoicePayment {
            payer,
            paid: due,
            at_ms: now,
        });
        if inv.single_use {
            inv.status = SimInvoiceStatus::Paid;
        }
    }
    let notice = creator.map(|c| {
        let who = st.display(payer);
        reply(
            c,
            platform,
            Draft::text(texts::invoice_paid_notice(platform, code, &due, &who)),
        )
    });
    Ok((due, notice))
}

/// Шаг PIN в мини-приложении CryptoBot.
pub(crate) fn complete_webapp(
    st: &mut WorldState,
    session_id: u64,
    pin: &str,
) -> Result<(WebAppPayment, Vec<Outbound>), SimError> {
    let session = st
        .sessions
        .get(&session_id)
        .ok_or_else(|| SimError::UnknownSession(session_id.to_string()))?;
    if session.used {
        return Ok((WebAppPayment::SessionUsed, Vec::new()));
    }
    let (acc, code, asset) = (session.account, session.code.clone(), session.asset);
    let expected = st.account(acc)?.pin.clone();
    if expected.as_deref().is_some_and(|p| p != pin) {
        let Some(session) = st.sessions.get_mut(&session_id) else {
            return Err(SimError::UnknownSession(session_id.to_string()));
        };
        session.attempts_left = session.attempts_left.saturating_sub(1);
        if session.attempts_left == 0 {
            session.used = true;
            return Ok((WebAppPayment::Blocked, Vec::new()));
        }
        return Ok((
            WebAppPayment::WrongPin {
                attempts_left: session.attempts_left,
            },
            Vec::new(),
        ));
    }
    let platform = Platform::CryptoBot;
    match pay_invoice(st, acc, &code, asset) {
        Ok((paid, notice)) => {
            if let Some(s) = st.sessions.get_mut(&session_id) {
                s.used = true;
            }
            let mut outs = vec![reply(
                acc,
                platform,
                Draft::text(texts::invoice_paid(platform, &code, &paid)),
            )];
            outs.extend(notice);
            Ok((WebAppPayment::Paid(paid), outs))
        }
        Err(PayError::AlreadyPaid) => Ok((WebAppPayment::AlreadyPaid, Vec::new())),
        Err(PayError::Expired) => Ok((WebAppPayment::Expired, Vec::new())),
        Err(PayError::Own) => Ok((WebAppPayment::OwnInvoice, Vec::new())),
        Err(PayError::Insufficient { available, due }) => Ok((
            WebAppPayment::InsufficientFunds {
                available,
                needed: due,
            },
            Vec::new(),
        )),
        Err(PayError::NotFound) => Err(SimError::UnknownInvoice(code)),
    }
}

/// `open_webapp`: открыть мини-приложение по URL кнопки «Оплатить».
pub(crate) fn open_webapp(
    st: &mut WorldState,
    acc: AccountId,
    bot: Platform,
    button_url: &str,
) -> Result<String, TransportError> {
    let invalid = || rpc("WEBAPP_URL_INVALID");
    if bot != Platform::CryptoBot {
        return Err(invalid());
    }
    let rest = button_url
        .strip_prefix("https://")
        .and_then(|r| r.strip_prefix(WEBAPP_HOST))
        .and_then(|r| r.strip_prefix("/invoice?"))
        .ok_or_else(invalid)?;
    let code = query_param(rest, "code").ok_or_else(invalid)?;
    let asset = query_param(rest, "asset")
        .and_then(|a| Asset::from_str(&a).ok())
        .ok_or_else(invalid)?;
    if !st.invoices.contains_key(&code) {
        return Err(invalid());
    }
    let id = st.next_session_id();
    st.sessions.insert(
        id,
        super::state::WebAppSession {
            account: acc,
            code: code.clone(),
            asset,
            attempts_left: PIN_ATTEMPTS,
            used: false,
        },
    );
    Ok(format!(
        "https://{WEBAPP_HOST}/invoice?code={code}&asset={}&session={id}#tgWebAppData=user%3D{}",
        asset.code(),
        acc.0
    ))
}

/// Значение параметра `name` из строки запроса (до `#`).
pub(crate) fn query_param(query: &str, name: &str) -> Option<String> {
    let query = query.split('#').next()?;
    let query = query.rsplit('?').next()?;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name && !v.is_empty()).then(|| v.to_owned())
    })
}

// ---------- инлайн-режим ----------

/// Разбор инлайн-запроса: `10usdt`, `10 usdt`, `0.5ton`, `10` (все активы).
fn parse_inline_query(query: &str) -> Option<(Decimal, Option<Asset>)> {
    let q = query.trim().to_lowercase();
    let split = q
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(q.len());
    let (num, rest) = q.split_at(split);
    let amount = parse_user_amount(num)?;
    let rest = rest.trim();
    let asset = match rest {
        "" => None,
        other => Some(Asset::from_str(&other.to_uppercase()).ok()?),
    };
    Some((amount, asset))
}

pub(crate) fn on_inline_query(
    st: &mut WorldState,
    acc: AccountId,
    platform: Platform,
    query: &str,
) -> InlineResults {
    let query_id = st.next_query_id();
    let mut items = Vec::new();
    let mut results = Vec::new();
    if let Some((amount, only)) = parse_inline_query(query) {
        for asset in Asset::ALL {
            if only.is_some_and(|a| a != asset) {
                continue;
            }
            let Ok(m) = Money::new(amount, asset) else {
                continue;
            };
            let available = st.balance_money(acc, platform, asset);
            let check_id = format!("check-{}", asset.code());
            if available.amount() >= m.amount() {
                results.push(InlineResult {
                    id: check_id.clone(),
                    title: Some(texts::inline_check_title(platform, &m)),
                    description: Some(texts::inline_check_description(platform, &m, &available)),
                });
                items.push((check_id, InlineItem::Check(m)));
            } else {
                results.push(InlineResult {
                    id: check_id.clone(),
                    title: Some(texts::INLINE_INSUFFICIENT_TITLE.to_owned()),
                    description: Some(texts::inline_insufficient_description(
                        platform, &m, &available,
                    )),
                });
                items.push((check_id, InlineItem::Insufficient(m)));
            }
            if platform == Platform::CryptoBot {
                let invoice_id = format!("invoice-{}", asset.code());
                results.push(InlineResult {
                    id: invoice_id.clone(),
                    title: Some(texts::inline_invoice_title(platform, &m)),
                    description: Some(texts::inline_invoice_description(platform, &m)),
                });
                items.push((invoice_id, InlineItem::Invoice(m)));
            }
        }
    }
    st.inline_queries.insert(
        query_id,
        InlineQuery {
            account: acc,
            platform,
            items,
        },
    );
    InlineResults { query_id, results }
}

/// Отправка инлайн-результата: чек или счёт создаётся здесь, в момент отправки.
/// Возвращает черновик сообщения «через бота» с URL-кнопкой.
pub(crate) fn on_send_inline(
    st: &mut WorldState,
    acc: AccountId,
    platform: Platform,
    query_id: i64,
    result_id: &str,
    random_id: i64,
) -> Result<Draft, TransportError> {
    let query = st
        .inline_queries
        .get(&query_id)
        .filter(|q| q.account == acc && q.platform == platform)
        .ok_or_else(|| rpc("QUERY_ID_INVALID"))?;
    let item = query
        .items
        .iter()
        .find(|(id, _)| id == result_id)
        .map(|(_, item)| item.clone())
        .ok_or_else(|| rpc("RESULT_ID_INVALID"))?;
    let insufficient = Draft {
        text: texts::INLINE_INSUFFICIENT_MESSAGE.to_owned(),
        via_bot: Some(platform),
        ..Draft::default()
    };
    let draft = match item {
        InlineItem::Check(m) => {
            let spec = CheckSpec::new(platform, m).by(acc);
            match st.insert_check(spec, CheckOrigin::Inline { random_id }) {
                Ok(check) => Draft {
                    text: texts::inline_check_message(platform, &m),
                    entity_urls: Vec::new(),
                    buttons: vec![vec![url_button(
                        &texts::inline_check_button(platform, &m),
                        &check.url,
                    )]],
                    via_bot: Some(platform),
                },
                Err(_) => insufficient,
            }
        }
        InlineItem::Insufficient(_) => insufficient,
        InlineItem::Invoice(m) => {
            let spec = InvoiceSpec::crypto(platform, m).by(acc);
            let inv = st
                .insert_invoice(spec)
                .map_err(|e| TransportError::Other(e.to_string()))?;
            Draft {
                text: texts::inline_invoice_message(platform, &m),
                entity_urls: Vec::new(),
                buttons: vec![vec![url_button("Оплатить", &inv.url)]],
                via_bot: Some(platform),
            }
        }
    };
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn user_amounts() {
        assert_eq!(parse_user_amount("10"), Some(dec!(10)));
        assert_eq!(parse_user_amount(" 10,50 "), Some(dec!(10.5)));
        assert_eq!(parse_user_amount("0"), None);
        assert_eq!(parse_user_amount("-1"), None);
        assert_eq!(parse_user_amount("1e3"), None);
        assert_eq!(parse_user_amount("abc"), None);
        assert_eq!(parse_user_amount(""), None);
    }

    #[test]
    fn inline_queries() {
        assert_eq!(
            parse_inline_query("10usdt"),
            Some((dec!(10), Some(Asset::Usdt)))
        );
        assert_eq!(
            parse_inline_query("10 USDT"),
            Some((dec!(10), Some(Asset::Usdt)))
        );
        assert_eq!(
            parse_inline_query("0.5ton"),
            Some((dec!(0.5), Some(Asset::Ton)))
        );
        assert_eq!(
            parse_inline_query("2 toncoin"),
            Some((dec!(2), Some(Asset::Ton)))
        );
        assert_eq!(parse_inline_query("10"), Some((dec!(10), None)));
        assert_eq!(parse_inline_query("10btc"), None);
        assert_eq!(parse_inline_query("usdt"), None);
    }

    #[test]
    fn query_params() {
        let url =
            "https://pay.send.sim.invalid/invoice?code=IVx&asset=USDT&session=7#tgWebAppData=a";
        assert_eq!(query_param(url, "code").as_deref(), Some("IVx"));
        assert_eq!(query_param(url, "session").as_deref(), Some("7"));
        assert_eq!(query_param(url, "tgWebAppData"), None);
    }
}
