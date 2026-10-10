//! Активация чека клиента: `/start <код>` боту кошелька и разбор ответа (lovec `fastsend.rs`,
//! `replies.rs`; LOVEC-PORTING §3).
//!
//! Повторять активацию безопасно: второй раз чек не зачислят. Но второй `/start` с НОВЫМ
//! `random_id` после выигрыша ответит «уже активирован» — выигрыш выглядел бы проигрышем
//! (lovec AUDIT.md:46). Поэтому новый `random_id` — только для одного повтора xRocket после
//! явного «не найден»: на него чек точно не зачислен.

use domain::{Money, Platform};
use parsers::ActivationReply;
use parsers::replies::classify_activation;

use super::conv::{Conv, SendError};
use super::gate::Pace;
use super::{UserbotWallet, rid};
use crate::transport::{Chat, RawMessage, Transport};
use crate::wallet::{ActivationOutcome, BotError, OpTag, RejectReason};

enum Attempt {
    Reply(ActivationReply, RawMessage),
    /// Ответа нет или отправка не подтверждена.
    NoReply(Option<RawMessage>),
}

pub(crate) async fn run<T: Transport>(
    w: &UserbotWallet<T>,
    param: &str,
    tag: &OpTag,
) -> Result<ActivationOutcome, BotError> {
    let platform = w.platform;
    let chat = Chat::WalletBot(platform);
    let text = format!("/start {param}");
    let mut conv = Conv::open(w, Some(&tag.key)).await;
    conv.wait_quarantine(chat).await;

    let mut attempt = once(&mut conv, chat, &text, tag.random_id).await?;
    if let (Platform::XRocket, Some(delay), Attempt::Reply(ActivationReply::NotFound, _)) =
        (platform, w.cfg.xrocket_not_found_retry, &attempt)
    {
        tokio::time::sleep(delay).await;
        let retry_id = rid::derive(tag.random_id, rid::XROCKET_RETRY);
        match once(&mut conv, chat, &text, retry_id).await {
            Ok(second) => attempt = second,
            // Повтор не ушёл — остаётся явное «не найден» первой попытки.
            Err(e) => tracing::info!(error = %e, "xRocket retry after 'not found' was not sent"),
        }
    }

    Ok(match attempt {
        Attempt::Reply(reply, raw) => outcome(reply, raw),
        Attempt::NoReply(raw) => ActivationOutcome::Unknown { raw },
    })
}

async fn once<T: Transport>(
    conv: &mut Conv<'_, T>,
    chat: Chat,
    text: &str,
    random_id: i64,
) -> Result<Attempt, BotError> {
    let platform = conv.platform();
    let mark = conv.mark().await;
    let sent = match conv.send(chat, text, random_id, Pace::Bot).await {
        Ok(sent) => sent,
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            tracing::warn!(%detail, "activation command not confirmed");
            conv.quarantine(chat);
            return Ok(Attempt::NoReply(None));
        }
    };
    let deadline = conv.deadline();
    let mut last_unknown: Option<RawMessage> = None;
    let reply = conv
        .wait(chat, sent.msg.id, mark, deadline, sent.located, |m| {
            let class = classify_activation(platform, &m.text);
            if class == ActivationReply::Unknown {
                last_unknown = Some(m.clone());
            }
            class.is_terminal()
        })
        .await;
    match reply {
        Some(m) => Ok(Attempt::Reply(classify_activation(platform, &m.text), m)),
        None => {
            conv.quarantine(chat);
            Ok(Attempt::NoReply(last_unknown))
        }
    }
}

fn outcome(reply: ActivationReply, raw: RawMessage) -> ActivationOutcome {
    let rejected = ActivationOutcome::Rejected;
    match reply {
        ActivationReply::Received { amount, asset } => match Money::new(amount, asset) {
            Ok(m) if !m.amount().is_zero() => ActivationOutcome::Received(m),
            _ => ActivationOutcome::Unknown { raw: Some(raw) },
        },
        ActivationReply::AlreadyActivated => rejected(RejectReason::AlreadyActivated),
        ActivationReply::NotFound => rejected(RejectReason::NotFound),
        ActivationReply::NotForYou => rejected(RejectReason::NotForUs),
        ActivationReply::NeedsSubscription => rejected(RejectReason::NeedsSubscription),
        ActivationReply::Captcha => rejected(RejectReason::Captcha),
        ActivationReply::PasswordRequired => rejected(RejectReason::PasswordProtected),
        ActivationReply::PremiumOnly => rejected(RejectReason::PremiumOnly),
        ActivationReply::InProgress
        | ActivationReply::ClaimNotice
        | ActivationReply::IncomingTransfer { .. }
        | ActivationReply::Unknown => ActivationOutcome::Unknown { raw: Some(raw) },
    }
}
