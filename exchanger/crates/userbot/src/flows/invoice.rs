//! Чужие счета: просмотр и оплата (DESIGN-v0.2, Р4; LOVEC-PORTING §8.3–§8.4).
//!
//! Оплата: `/start IV…` (`inv_…`) → карточка → кнопка валюты → экран «к оплате» → сверка
//! суммы с ожидаемой → «Оплатить». У обоих кошельков «Оплатить» — кнопка в чате: нажимаем один
//! раз и больше никогда. CryptoBot не спрашивает код-пароль, пока траты аккаунта за 24 часа не
//! выходят за «лимит без код-пароля» (DESIGN-v0.2 Р4, Р11); движок за этим следит. Если бот всё
//! же просит код — код мы не вводим никогда: `NeedsHuman`, деньги не двигались. Кнопку, которая
//! открывает мини-приложение, не нажимаем — тоже `NeedsHuman`.

use domain::{Asset, Money};
use parsers::replies::{classify_invoice_payment, parse_invoice_card};
use parsers::screens::{self, PAY_BUTTON};
use parsers::{InvoicePayReply, InvoiceStatus};

use super::conv::{Conv, SendError};
use super::gate::Pace;
use super::{UserbotWallet, rid, typed_amount};
use crate::transport::{ButtonKind, Chat, RawMessage, Transport};
use crate::wallet::{BotError, InvoiceInfo, OpTag, PayOutcome};

const NOT_FOUND: &[&str] = &["не найден", "не существует", "not found", "does not exist"];

/// Похоже ли сообщение на ответ про счёт: карточка с понятным статусом или «не найден».
fn is_invoice_reply(platform: domain::Platform, m: &RawMessage) -> bool {
    parse_invoice_card(platform, &m.text).status != InvoiceStatus::Unknown
        || screens::text_matches(&m.text, NOT_FOUND)
}

/// Открыть счёт и дождаться карточки. Ничего не платит: любая неудача — «не выполнялось».
/// Открытая карточка: наша команда, ответ бота и признак «команду отправил прошлый вызов».
struct Opened {
    cmd: RawMessage,
    card: RawMessage,
    earlier_call: bool,
}

async fn open_card<T: Transport>(
    conv: &mut Conv<'_, T>,
    param: &str,
    random_id: i64,
) -> Result<Opened, BotError> {
    let platform = conv.platform();
    let chat = Chat::WalletBot(platform);
    conv.wait_quarantine(chat).await;
    let mark = conv.mark().await;
    let sent = match conv
        .send(chat, &format!("/start {param}"), random_id, Pace::Bot)
        .await
    {
        Ok(sent) if sent.earlier_call => {
            return Ok(Opened {
                card: sent.msg.clone(),
                cmd: sent.msg,
                earlier_call: true,
            });
        }
        Ok(sent) => sent,
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            conv.quarantine(chat);
            return Err(BotError::Unavailable(format!("invoice command: {detail}")));
        }
    };
    let deadline = conv.deadline();
    let card = conv
        .wait(chat, sent.msg.id, mark, deadline, sent.located, |m| {
            is_invoice_reply(platform, m)
        })
        .await;
    match card {
        Some(card) => Ok(Opened {
            cmd: sent.msg,
            card,
            earlier_call: false,
        }),
        None => {
            conv.quarantine(chat);
            Err(BotError::Unavailable(
                "the bot did not show the invoice".to_owned(),
            ))
        }
    }
}

pub(crate) async fn inspect<T: Transport>(
    w: &UserbotWallet<T>,
    param: &str,
) -> Result<InvoiceInfo, BotError> {
    let mut conv = Conv::open(w, None).await;
    let opened = open_card(&mut conv, param, rid::new_random_id()).await?;
    Ok(InvoiceInfo {
        param: param.to_owned(),
        card: parse_invoice_card(w.platform, &opened.card.text),
    })
}

fn needs_human(reason: impl Into<String>) -> PayOutcome {
    PayOutcome::NeedsHuman {
        reason: reason.into(),
    }
}

fn from_reply(reply: InvoicePayReply) -> Option<PayOutcome> {
    match reply {
        InvoicePayReply::Paid => Some(PayOutcome::Paid),
        InvoicePayReply::AlreadyPaid => Some(PayOutcome::AlreadyPaid),
        InvoicePayReply::Expired => Some(PayOutcome::Expired),
        InvoicePayReply::InsufficientFunds => Some(PayOutcome::InsufficientFunds),
        InvoicePayReply::PasscodeRequired => Some(needs_human(
            "CryptoBot asks for the passcode: spending is over the passcode-free limit",
        )),
        InvoicePayReply::Unknown => None,
    }
}

pub(crate) async fn pay<T: Transport>(
    w: &UserbotWallet<T>,
    param: &str,
    asset: Asset,
    expected: &Money,
    tag: &OpTag,
) -> Result<PayOutcome, BotError> {
    let platform = w.platform;
    let chat = Chat::WalletBot(platform);
    let mut conv = Conv::open(w, Some(&tag.key)).await;
    let opened = open_card(
        &mut conv,
        param,
        rid::derive(tag.random_id, rid::INVOICE_OPEN),
    )
    .await?;
    // Эту оплату уже начинал прошлый вызов: «Оплатить» могли нажать. Второго нажатия нет.
    if opened.earlier_call {
        tracing::warn!(op = %tag.key, "invoice payment was started earlier, not pressing again");
        return Ok(PayOutcome::Unknown {
            detail: "payment was started earlier; check the invoice status".to_owned(),
        });
    }
    let (cmd, card_msg) = (opened.cmd, opened.card);

    let card = parse_invoice_card(platform, &card_msg.text);
    match card.status {
        InvoiceStatus::Active => {}
        InvoiceStatus::Paid => return Ok(PayOutcome::AlreadyPaid),
        InvoiceStatus::Expired => return Ok(PayOutcome::Expired),
        InvoiceStatus::Unknown => return Ok(needs_human("invoice not found or unreadable")),
    }
    if card.single_use == Some(false) {
        return Ok(needs_human("multi-use invoice"));
    }

    // Валюта: экран «к оплате» с кнопкой «Оплатить» (или новый статус счёта).
    let Some(asset_data) = card_msg
        .buttons
        .iter()
        .flatten()
        .find_map(|b| match &b.kind {
            ButtonKind::Callback(data) if screens::is_asset_button(&b.text, asset) => {
                Some(data.clone())
            }
            _ => None,
        })
    else {
        return Ok(needs_human(format!(
            "no {} button on the invoice",
            asset.code()
        )));
    };
    let mark = conv.mark().await;
    match conv.press(chat, card_msg.id, &asset_data).await {
        Ok(_) => {}
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            tracing::warn!(%detail, "asset button outcome unknown, waiting for the screen");
        }
    }
    let deadline = conv.deadline();
    let pay_screen = conv
        .wait(chat, cmd.id, mark, deadline, false, |m| {
            has_pay_button(m) || from_reply(classify_invoice_payment(platform, &m.text)).is_some()
        })
        .await
        .ok_or_else(|| {
            BotError::Unavailable("no payment screen after the asset button".to_owned())
        })?;
    if !has_pay_button(&pay_screen) {
        return Ok(
            from_reply(classify_invoice_payment(platform, &pay_screen.text))
                .unwrap_or_else(|| needs_human("unexpected payment screen")),
        );
    }

    // Сумма к оплате должна совпасть с той, под которую клиент прислал деньги.
    let due = screens::parse_amount_due(platform, &pay_screen.text).or(card.amount);
    if due != Some((expected.amount(), expected.asset())) {
        return Ok(needs_human(format!(
            "amount due {due:?} differs from expected {} {}",
            typed_amount(expected),
            expected.asset().code()
        )));
    }

    let Some(button) = pay_screen
        .buttons
        .iter()
        .flatten()
        .find(|b| screens::text_matches(&b.text, PAY_BUTTON))
    else {
        return Ok(needs_human("no pay button"));
    };
    match button.kind.clone() {
        ButtonKind::Callback(data) => press_pay(&mut conv, pay_screen.id, &data, cmd.id).await,
        ButtonKind::WebApp(_) => Ok(needs_human("the pay button opens a mini app: pay manually")),
        other => Ok(needs_human(format!("unsupported pay button {other:?}"))),
    }
}

fn has_pay_button(m: &RawMessage) -> bool {
    m.buttons
        .iter()
        .flatten()
        .any(|b| screens::text_matches(&b.text, PAY_BUTTON))
}

/// Одно нажатие «Оплатить». После него — только ожидание ответа, без повторов.
async fn press_pay<T: Transport>(
    conv: &mut Conv<'_, T>,
    screen_id: i32,
    data: &[u8],
    after_id: i32,
) -> Result<PayOutcome, BotError> {
    let platform = conv.platform();
    let chat = Chat::WalletBot(platform);
    let mark = conv.mark().await;
    match conv.press(chat, screen_id, data).await {
        Ok(answer) => {
            if let Some(outcome) = answer
                .message
                .as_deref()
                .and_then(|t| from_reply(classify_invoice_payment(platform, t)))
            {
                return Ok(outcome);
            }
        }
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            tracing::warn!(%detail, "pay press outcome unknown, waiting for the bot");
        }
    }
    let deadline = conv.deadline();
    let reply = conv
        .wait(chat, after_id, mark, deadline, false, |m| {
            from_reply(classify_invoice_payment(platform, &m.text)).is_some()
        })
        .await;
    Ok(match reply {
        Some(m) => from_reply(classify_invoice_payment(platform, &m.text)).unwrap_or_else(|| {
            PayOutcome::Unknown {
                detail: "unrecognised payment reply".to_owned(),
            }
        }),
        None => {
            conv.quarantine(chat);
            PayOutcome::Unknown {
                detail: "no reply after the pay button".to_owned(),
            }
        }
    })
}
