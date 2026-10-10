//! Создание чека выплаты или возврата — как обычный пользователь (DESIGN-v0.2, Р5).
//!
//! 1. Метка операции в «Избранном» (`🏷 exch ord-…-payout`). По ней `find_issued_check`
//!    находит чек этой операции после сбоя или рестарта: в Telegram id сообщений личных чатов
//!    и «Избранного» — общий счётчик аккаунта, поэтому чек операции — между её меткой и
//!    меткой следующей.
//! 2. Инлайн: `@send 97.5usdt` / `@xrocket 97.5` → ровно один результат «чек на эту сумму» →
//!    отправка в «Избранное» с `random_id` операции → ссылка из URL-кнопки.
//! 3. Если инлайн не дошёл до отправки — меню: CryptoBot `/checks` → «Создать чек» → USDT →
//!    сумма; xRocket `/cheques` → «Персональный» → «Создать чек» → USDT → сумма.
//!
//! После отправки инлайн-результата или суммы чек мог появиться: дальше никаких повторов
//! с новым `random_id` и никакого меню — только `Unknown` и сверка.

use domain::{Money, Platform};
use parsers::replies::parse_created_check;
use parsers::screens::{
    self, AMOUNT_PROMPT, CREATE_CHECK_BUTTON, CheckCreationReply, InlineResultKind, PERSONAL_BUTTON,
};
use tokio::time::Instant;

use super::conv::{Conv, SendError};
use super::gate::Pace;
use super::{UserbotWallet, rid, typed_amount};
use crate::transport::{ButtonKind, Chat, InlineResults, RawMessage, Transport, TransportError};
use crate::wallet::{BotError, IssueMethod, IssueOutcome, IssuedCheck, OpTag};

const MARKER_PREFIX: &str = "🏷 exch ";

/// Текст метки операции в «Избранном».
pub fn marker_text(key: &str) -> String {
    format!("{MARKER_PREFIX}{key}")
}

/// Сообщение — метка какой-то операции exch.
pub fn is_marker(text: &str) -> bool {
    text.starts_with(MARKER_PREFIX)
}

pub(crate) async fn run<T: Transport>(
    w: &UserbotWallet<T>,
    amount: &Money,
    tag: &OpTag,
) -> Result<IssueOutcome, BotError> {
    let mut conv = Conv::open(w, Some(&tag.key)).await;
    let marker = match conv
        .send(
            Chat::SavedMessages,
            &marker_text(&tag.key),
            rid::derive(tag.random_id, rid::MARKER),
            Pace::Own,
        )
        .await
    {
        Ok(sent) => sent.msg,
        Err(SendError::NotSent(e)) => return Err(e),
        // Чек ещё не создавался: операцию можно закрыть как невыполненную.
        Err(SendError::Unknown(detail)) => {
            return Err(BotError::Unavailable(format!("operation marker: {detail}")));
        }
    };

    if w.cfg.inline_checks {
        match inline(&mut conv, amount, tag, &marker).await? {
            Step::Done(outcome) => return Ok(outcome),
            Step::Fallback(reason) => {
                tracing::info!(op = %tag.key, %reason, "inline check not sent, using the bot menu");
            }
        }
    }
    menu(&mut conv, amount, tag).await
}

enum Step {
    Done(IssueOutcome),
    /// Инлайн не дошёл до отправки: чек точно не создан.
    Fallback(String),
}

fn inline_query(platform: Platform, amount: &Money) -> String {
    match platform {
        Platform::CryptoBot => format!(
            "{}{}",
            typed_amount(amount),
            amount.asset().platform_code(platform).to_lowercase()
        ),
        Platform::XRocket => typed_amount(amount),
    }
}

fn unknown(detail: impl Into<String>) -> IssueOutcome {
    IssueOutcome::Unknown {
        detail: detail.into(),
    }
}

/// Чек из сообщения, если ссылка ровно одна и сумма та, что просили.
fn check_in(platform: Platform, msg: &RawMessage, amount: &Money) -> Result<IssuedCheck, String> {
    let check = parse_created_check(platform, &msg.text, &msg.button_urls())
        .ok_or_else(|| "no check link in the message".to_owned())?;
    match check.amount {
        Some((a, asset)) if a == amount.amount() && asset == amount.asset() => Ok(IssuedCheck {
            url: check.url,
            param: check.param,
            amount: *amount,
        }),
        other => Err(format!(
            "check {} has amount {other:?}, expected {}",
            check.param,
            typed_amount(amount)
        )),
    }
}

async fn inline<T: Transport>(
    conv: &mut Conv<'_, T>,
    amount: &Money,
    tag: &OpTag,
    marker: &RawMessage,
) -> Result<Step, BotError> {
    let platform = conv.platform();
    conv.pace(Pace::Bot).await?;
    let results = match conv
        .transport()
        .inline_query(platform, &inline_query(platform, amount))
        .await
    {
        Ok(results) => results,
        Err(TransportError::FloodWait(d)) => {
            return Err(conv.not_sent(TransportError::FloodWait(d)));
        }
        Err(e) => return Ok(Step::Fallback(format!("inline query: {e}"))),
    };
    let wanted = InlineResultKind::Check {
        amount: amount.amount(),
        asset: amount.asset(),
    };
    let matching: Vec<&str> = results
        .results
        .iter()
        .filter(|r| {
            screens::classify_inline_result(platform, r.title.as_deref(), r.description.as_deref())
                == wanted
        })
        .map(|r| r.id.as_str())
        .collect();
    let [result_id] = matching.as_slice() else {
        return Ok(Step::Fallback(format!(
            "{} inline results match the amount",
            matching.len()
        )));
    };
    let result_id = (*result_id).to_owned();

    conv.pace(Pace::Bot).await?;
    let mark = conv.mark().await;
    let sent = send_inline(conv, &results, &result_id, tag.random_id, marker).await;
    let msg = match sent {
        Ok(msg) => msg,
        Err(SendError::NotSent(BotError::Transport(TransportError::Rpc { code, name }))) => {
            return Ok(Step::Fallback(format!(
                "send inline result: RPC {code} {name}"
            )));
        }
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => return Ok(Step::Done(unknown(detail))),
    };

    match check_in(platform, &msg, amount) {
        Ok(check) => {
            return Ok(Step::Done(IssueOutcome::Issued {
                check,
                method: IssueMethod::Inline,
            }));
        }
        Err(e) if msg.button_urls().is_empty() => {
            if screens::classify_check_creation(platform, &msg.text, &[])
                == CheckCreationReply::InsufficientFunds
            {
                return Ok(Step::Done(IssueOutcome::InsufficientFunds));
            }
            tracing::info!(op = %tag.key, error = %e, "inline message has no link yet, waiting for an edit");
        }
        Err(e) => return Ok(Step::Done(unknown(e))),
    }
    // Бот может дописать кнопку правкой отправленного сообщения.
    let deadline = conv.deadline();
    let edited = conv
        .wait_edit(Chat::SavedMessages, msg.id, mark, deadline, |m| {
            !m.button_urls().is_empty()
        })
        .await;
    Ok(Step::Done(match edited {
        Some(m) => match check_in(platform, &m, amount) {
            Ok(check) => IssueOutcome::Issued {
                check,
                method: IssueMethod::Inline,
            },
            Err(e) => unknown(e),
        },
        None => unknown("inline check message has no link"),
    }))
}

/// Отправить инлайн-результат в «Избранное». Повтор — только тем же `random_id`; при
/// `RANDOM_ID_DUPLICATE` сообщение ищем после метки операции.
async fn send_inline<T: Transport>(
    conv: &mut Conv<'_, T>,
    results: &InlineResults,
    result_id: &str,
    random_id: i64,
    marker: &RawMessage,
) -> Result<RawMessage, SendError> {
    let platform = conv.platform();
    let call = conv
        .transport()
        .send_inline(Chat::SavedMessages, platform, results, result_id, random_id)
        .await;
    let call = match call {
        Err(e) if e.outcome_unknown() => {
            tracing::warn!(error = %e, "send inline outcome unknown, resending with the same random_id");
            if let Err(flood) = conv.pace(Pace::Bot).await {
                return find_inline_after(conv, marker)
                    .await
                    .ok_or_else(|| SendError::Unknown(format!("{e}; resend impossible: {flood}")));
            }
            match conv
                .transport()
                .send_inline(Chat::SavedMessages, platform, results, result_id, random_id)
                .await
            {
                Ok(msg) => Ok(msg),
                Err(e2) => {
                    if let TransportError::FloodWait(d) = e2 {
                        let _ = conv.not_sent(TransportError::FloodWait(d));
                    }
                    return find_inline_after(conv, marker)
                        .await
                        .ok_or_else(|| SendError::Unknown(format!("{e}; resend: {e2}")));
                }
            }
        }
        other => other,
    };
    match call {
        Ok(msg) => {
            conv.record(&msg, true).await;
            Ok(msg)
        }
        Err(TransportError::RandomIdDuplicate) => find_inline_after(conv, marker)
            .await
            .ok_or_else(|| SendError::Unknown("RANDOM_ID_DUPLICATE, message not found".to_owned())),
        Err(e) => Err(SendError::NotSent(conv.not_sent(e))),
    }
}

/// Наше сообщение через бота после метки (и до следующей метки).
async fn find_inline_after<T: Transport>(
    conv: &mut Conv<'_, T>,
    marker: &RawMessage,
) -> Option<RawMessage> {
    let platform = conv.platform();
    let saved = conv.recent(Chat::SavedMessages).await.ok()?;
    let mut found = saved
        .into_iter()
        .filter(|m| m.id > marker.id)
        .take_while(|m| !(m.out && is_marker(&m.text)))
        .filter(|m| m.out && m.via_bot == Some(platform));
    let first = found.next()?;
    if found.next().is_some() {
        return None;
    }
    conv.record(&first, true).await;
    Some(first)
}

fn callback_button(msg: &RawMessage, mut pick: impl FnMut(&str) -> bool) -> Option<Vec<u8>> {
    msg.buttons.iter().flatten().find_map(|b| match &b.kind {
        ButtonKind::Callback(data) if pick(&b.text) => Some(data.clone()),
        _ => None,
    })
}

fn has_callback(msg: &RawMessage, pick: impl FnMut(&str) -> bool) -> bool {
    callback_button(msg, pick).is_some()
}

/// Нажать кнопку меню и дождаться следующего экрана. Деньги здесь не двигаются: любая
/// неудача — «не выполнялось».
async fn step<T: Transport>(
    conv: &mut Conv<'_, T>,
    chat: Chat,
    after_id: i32,
    screen: &RawMessage,
    button: impl FnMut(&str) -> bool,
    what: &str,
    next: impl FnMut(&RawMessage) -> bool,
) -> Result<RawMessage, BotError> {
    let data = callback_button(screen, button)
        .ok_or_else(|| BotError::Unavailable(format!("check menu: no '{what}' button")))?;
    let mark = conv.mark().await;
    match conv.press(chat, screen.id, &data).await {
        Ok(answer) => {
            if answer.alert {
                return Err(BotError::Unavailable(format!(
                    "check menu: '{what}' answered {:?}",
                    answer.message
                )));
            }
        }
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            tracing::warn!(%detail, button = what, "menu press outcome unknown, waiting for the screen");
        }
    }
    let deadline = conv.deadline();
    conv.wait(chat, after_id, mark, deadline, false, next)
        .await
        .ok_or_else(|| BotError::Unavailable(format!("check menu: no screen after '{what}'")))
}

async fn menu<T: Transport>(
    conv: &mut Conv<'_, T>,
    amount: &Money,
    tag: &OpTag,
) -> Result<IssueOutcome, BotError> {
    let platform = conv.platform();
    let chat = Chat::WalletBot(platform);
    let asset = amount.asset();
    conv.wait_quarantine(chat).await;

    let command = match platform {
        Platform::CryptoBot => "/checks",
        Platform::XRocket => "/cheques",
    };
    let mark = conv.mark().await;
    let cmd = match conv
        .send(
            chat,
            command,
            rid::derive(tag.random_id, rid::MENU_COMMAND),
            Pace::Bot,
        )
        .await
    {
        Ok(sent) => sent.msg,
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            return Err(BotError::Unavailable(format!("{command}: {detail}")));
        }
    };
    let first_button = match platform {
        Platform::CryptoBot => CREATE_CHECK_BUTTON,
        Platform::XRocket => PERSONAL_BUTTON,
    };
    let deadline = conv.deadline();
    let mut screen = conv
        .wait(chat, cmd.id, mark, deadline, false, |m| {
            has_callback(m, |t| screens::text_matches(t, first_button))
        })
        .await
        .ok_or_else(|| BotError::Unavailable(format!("{command}: no check menu")))?;

    if platform == Platform::XRocket {
        screen = step(
            conv,
            chat,
            cmd.id,
            &screen,
            |t| screens::text_matches(t, PERSONAL_BUTTON),
            "personal",
            |m| has_callback(m, |t| screens::text_matches(t, CREATE_CHECK_BUTTON)),
        )
        .await?;
    }
    screen = step(
        conv,
        chat,
        cmd.id,
        &screen,
        |t| screens::text_matches(t, CREATE_CHECK_BUTTON),
        "create check",
        |m| has_callback(m, |t| screens::is_asset_button(t, asset)),
    )
    .await?;
    step(
        conv,
        chat,
        cmd.id,
        &screen,
        |t| screens::is_asset_button(t, asset),
        "asset",
        |m| screens::text_matches(&m.text, AMOUNT_PROMPT),
    )
    .await?;

    // Сумма: после этой отправки чек мог появиться.
    let mark = conv.mark().await;
    let sent = match conv
        .send(
            chat,
            &typed_amount(amount),
            rid::derive(tag.random_id, rid::MENU_AMOUNT),
            Pace::Bot,
        )
        .await
    {
        Ok(sent) => sent,
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            conv.quarantine(chat);
            return Ok(unknown(format!("amount not confirmed: {detail}")));
        }
    };
    let deadline: Instant = conv.deadline();
    let reply = conv
        .wait(chat, sent.msg.id, mark, deadline, sent.located, |m| {
            screens::classify_check_creation(platform, &m.text, &m.button_urls())
                != CheckCreationReply::Unknown
        })
        .await;
    let Some(reply) = reply else {
        conv.quarantine(chat);
        return Ok(unknown("no reply to the check amount"));
    };
    Ok(
        match screens::classify_check_creation(platform, &reply.text, &reply.button_urls()) {
            CheckCreationReply::Created(_) => match check_in(platform, &reply, amount) {
                Ok(check) => IssueOutcome::Issued {
                    check,
                    method: IssueMethod::Menu,
                },
                Err(e) => unknown(e),
            },
            CheckCreationReply::InsufficientFunds => IssueOutcome::InsufficientFunds,
            CheckCreationReply::Unknown => unknown("unrecognised reply to the check amount"),
        },
    )
}

/// Сверка после `IssueOutcome::Unknown` или рестарта: чек этой операции между её меткой
/// и следующей меткой — в «Избранном» (инлайн) или в чате бота (меню). `None` — метки нет,
/// чека нет или кандидатов несколько (тогда решает человек).
pub(crate) async fn find<T: Transport>(
    w: &UserbotWallet<T>,
    amount: &Money,
    tag: &OpTag,
) -> Result<Option<IssuedCheck>, BotError> {
    let platform = w.platform;
    let conv = Conv::open(w, Some(&tag.key)).await;
    let unavailable = |e: TransportError| BotError::Unavailable(format!("history: {e}"));
    let saved = conv
        .recent(Chat::SavedMessages)
        .await
        .map_err(unavailable)?;
    let label = marker_text(&tag.key);
    let Some(pos) = saved.iter().rposition(|m| m.out && m.text == label) else {
        tracing::warn!(op = %tag.key, "operation marker not found in Saved Messages");
        return Ok(None);
    };
    let from = saved[pos].id;
    let until = saved[pos + 1..]
        .iter()
        .find(|m| m.out && is_marker(&m.text))
        .map_or(i32::MAX, |m| m.id);
    let in_window = |m: &RawMessage| m.id > from && m.id < until;

    let mut found: Vec<IssuedCheck> = Vec::new();
    let mut add = |check: IssuedCheck| {
        if !found.iter().any(|c| c.param == check.param) {
            found.push(check);
        }
    };
    for m in saved
        .iter()
        .filter(|m| in_window(m) && m.via_bot == Some(platform))
    {
        match check_in(platform, m, amount) {
            Ok(check) => add(check),
            Err(e) => {
                tracing::warn!(op = %tag.key, error = %e, "inline message without a matching check")
            }
        }
    }
    let bot = conv
        .recent(Chat::WalletBot(platform))
        .await
        .map_err(unavailable)?;
    for m in bot.iter().filter(|m| !m.out && in_window(m)) {
        if let CheckCreationReply::Created(_) =
            screens::classify_check_creation(platform, &m.text, &m.button_urls())
        {
            match check_in(platform, m, amount) {
                Ok(check) => add(check),
                Err(e) => {
                    tracing::warn!(op = %tag.key, error = %e, "menu reply without a matching check")
                }
            }
        }
    }
    match found.len() {
        1 => Ok(found.pop()),
        0 => Ok(None),
        n => {
            tracing::error!(op = %tag.key, candidates = n, "several checks in one operation window");
            Ok(None)
        }
    }
}
