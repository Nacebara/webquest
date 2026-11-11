//! Создание чека выплаты или возврата — через меню бота, как обычный пользователь
//! (DESIGN-v0.2 Р5; решение владельца 6 ноября 2026: только меню, инлайн-режим не нужен —
//! клиенту мы отдаём ссылку на чек). Экраны — по живой записи (`parsers::screens`).
//!
//! 1. Метка операции в «Избранном» (`🏷 exch ord-…-payout`). По ней `find_issued_check`
//!    находит чек этой операции после сбоя или рестарта: в Telegram id сообщений личных чатов
//!    и «Избранного» — общий счётчик аккаунта, поэтому чек операции — между её меткой и
//!    меткой следующей.
//! 2. CryptoBot: `/checks` → «Создать чек» → USDT → сумма текстом → НОВОЕ сообщение: экран
//!    чека; или «Пришлите количество активаций…» → «Пропустить ›» (одна активация) → тот же
//!    экран правкой — чек; или «Недостаточно монет».
//! 3. xRocket: `/cheques` → «Персональный» → «Создать чек» → USDT → сумма текстом → бот
//!    ПРАВИТ сообщение меню: «Сумма чека … подтвердите» → сверка суммы → «✅ Подтверждаю» →
//!    правка: экран чека; или «🔺 Недостаточно средств».
//!
//! Где чек может появиться: CryptoBot — после отправки суммы (если на мультичек денег нет,
//! чек создаёт сама сумма) и после «Пропустить ›»; xRocket — после «✅ Подтверждаю». С этого
//! места — ни повторов с новым `random_id`, ни второго нажатия: только `Unknown` и сверка.
//! Неудача, при которой чек точно не создан (бот показал экран без чека, кнопка не ушла), —
//! `Err`: операция не выполнялась. Её повтор с той же меткой только сверяет (см. [`run`]).

use domain::{Money, Platform};
use parsers::replies::parse_created_check;
use parsers::screens::{
    self, AMOUNT_PROMPT, CONFIRM_BUTTON, CREATE_CHECK_BUTTON, CheckCreationReply, PERSONAL_BUTTON,
    SKIP_BUTTON,
};

use super::conv::{Conv, SendError};
use super::gate::Pace;
use super::{UserbotWallet, rid, typed_amount};
use crate::transport::{ButtonKind, Chat, RawMessage, Transport, TransportError};
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

/// Метка повторной попытки операции (`… #2`, `… #3`): у попытки своё окно в чате кошелька.
/// `attempts` — сколько меток этой операции уже есть.
fn retry_marker_text(key: &str, attempts: u64) -> String {
    format!("{MARKER_PREFIX}{key} #{}", attempts + 1)
}

/// Метка именно этой операции: первая (`🏷 exch <ключ>`) или повторной попытки (`… #k`).
fn is_marker_of(text: &str, key: &str) -> bool {
    text.strip_prefix(MARKER_PREFIX)
        .and_then(|t| t.strip_prefix(key))
        .is_some_and(|rest| {
            rest.is_empty()
                || rest
                    .strip_prefix(" #")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Окна попыток операции в общем счётчике id аккаунта: от метки попытки до следующей метки
/// любой операции. Пусто — меток этой операции в «Избранном» не видно.
fn attempt_windows(saved: &[RawMessage], key: &str) -> Vec<(i32, i32)> {
    saved
        .iter()
        .enumerate()
        .filter(|(_, m)| m.out && is_marker_of(&m.text, key))
        .map(|(i, m)| {
            let until = saved[i + 1..]
                .iter()
                .find(|x| x.out && is_marker(&x.text))
                .map_or(i32::MAX, |x| x.id);
            (m.id, until)
        })
        .collect()
}

pub(crate) async fn run<T: Transport>(
    w: &UserbotWallet<T>,
    amount: &Money,
    tag: &OpTag,
) -> Result<IssueOutcome, BotError> {
    let mut conv = Conv::open(w, Some(&tag.key)).await;
    // «Чистота» чата бота — до метки, пока ничего не начато. Команда меню названа заранее:
    // если прошлая попытка остановилась на ней же без ответа, это повтор, а не новая команда.
    conv.ensure_clear(Chat::WalletBot(w.platform), Some(menu_command(w.platform)))
        .await?;
    match conv
        .send(
            Chat::SavedMessages,
            &marker_text(&tag.key),
            rid::derive(tag.random_id, rid::MARKER),
            Pace::Own,
        )
        .await
    {
        // Метка уже стояла: эту операцию начинал прошлый вызов, и чек мог быть создан.
        // Ничего не создаём — только сверка. Так `issue_check` безопасен даже при повторном
        // вызове с тем же `OpTag` (движок так делает после `Err`: операция возвращается в
        // очередь с той же меткой).
        // Метка уже стояла: эту операцию начинал прошлый вызов. Сначала сверка — чек мог быть
        // создан. Не найден — меню проходим заново, только если точно известно, что сумма ещё
        // не отправлялась (чек появляется не раньше суммы; аудит 11.11.2026, М-1). Иначе —
        // «неясно», решает человек. Так `issue_check` безопасен при повторном вызове с тем же
        // `OpTag` (движок так делает после `Err`: операция возвращается в очередь).
        Ok(sent) if sent.earlier_call => {
            match find_with(&conv, amount, tag).await {
                Ok(Some(check)) => {
                    return Ok(IssueOutcome::Issued {
                        check,
                        method: IssueMethod::Menu,
                    });
                }
                Ok(None) => {}
                Err(e) => {
                    return Ok(unknown(format!(
                        "operation was started earlier; reconciliation failed: {e}"
                    )));
                }
            }
            let Some(attempts) = retry_room(&conv, amount, tag).await else {
                tracing::warn!(op = %tag.key, "operation was started earlier and may have sent the amount: reconciling only");
                return Ok(unknown(
                    "operation was started earlier and no check was found",
                ));
            };
            tracing::warn!(op = %tag.key, attempts, "earlier attempt never sent the amount: going through the check menu again");
            match conv
                .send(
                    Chat::SavedMessages,
                    &retry_marker_text(&tag.key, attempts),
                    rid::derive(tag.random_id, rid::attempt(rid::MARKER, attempts)),
                    Pace::Own,
                )
                .await
            {
                Ok(sent) if sent.earlier_call => {
                    return Ok(unknown("retry marker already exists"));
                }
                Ok(_) => {}
                Err(SendError::NotSent(e)) => return Err(e),
                Err(SendError::Unknown(detail)) => {
                    return Err(BotError::Unavailable(format!("operation marker: {detail}")));
                }
            }
            return menu(&mut conv, amount, tag, attempts).await;
        }
        Ok(_) => {}
        Err(SendError::NotSent(e)) => return Err(e),
        // Чек ещё не создавался: операцию можно закрыть как невыполненную.
        Err(SendError::Unknown(detail)) => {
            return Err(BotError::Unavailable(format!("operation marker: {detail}")));
        }
    }
    menu(&mut conv, amount, tag, 0).await
}

fn menu_command(platform: Platform) -> &'static str {
    match platform {
        Platform::CryptoBot => "/checks",
        Platform::XRocket => "/cheques",
    }
}

/// Можно ли пройти меню заново: `Some(n)` — да, и у операции уже `n` меток. Только когда точно
/// известно, что сумма в этой операции не отправлялась: первая метка видна в «Избранном»,
/// история чата кошелька после неё прочитана целиком, и ни в одном окне попыток нет нашего
/// сообщения с суммой.
async fn retry_room<T: Transport>(conv: &Conv<'_, T>, amount: &Money, tag: &OpTag) -> Option<u64> {
    let saved = conv.recent(Chat::SavedMessages).await.ok()?;
    let windows = attempt_windows(&saved, &tag.key);
    let first = windows.first()?.0;
    let label = marker_text(&tag.key);
    if !saved
        .iter()
        .any(|m| m.out && m.id == first && m.text == label)
    {
        return None;
    }
    let bot = conv
        .history_complete(Chat::WalletBot(conv.platform()), first)
        .await?;
    let typed = typed_amount(amount);
    let amount_sent = windows.iter().any(|&(from, until)| {
        bot.iter()
            .any(|m| m.out && m.id > from && m.id < until && m.text == typed)
    });
    if amount_sent {
        return None;
    }
    u64::try_from(windows.len()).ok()
}

fn unknown(detail: impl Into<String>) -> IssueOutcome {
    IssueOutcome::Unknown {
        detail: detail.into(),
    }
}

/// Чек из сообщения, если ссылка ровно одна и сумма та, что просили.
fn check_in(platform: Platform, msg: &RawMessage, amount: &Money) -> Result<IssuedCheck, String> {
    let check = parse_created_check(platform, &msg.text, &msg.button_urls())
        .or_else(|| parse_created_check(platform, &msg.text, &msg.entity_urls))
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

/// Разбор экрана меню после суммы: ссылки — из кнопок и из текста.
fn creation(platform: Platform, m: &RawMessage) -> CheckCreationReply {
    let mut urls = m.button_urls();
    urls.extend(m.entity_urls.iter().cloned());
    screens::classify_check_creation(platform, &m.text, &urls)
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

/// Что показало меню после суммы или финального нажатия.
fn settle(platform: Platform, reply: &RawMessage, amount: &Money) -> IssueOutcome {
    match creation(platform, reply) {
        CheckCreationReply::Created(_) => match check_in(platform, reply, amount) {
            Ok(check) => IssueOutcome::Issued {
                check,
                method: IssueMethod::Menu,
            },
            Err(e) => unknown(e),
        },
        CheckCreationReply::InsufficientFunds => IssueOutcome::InsufficientFunds,
        other => unknown(format!("unexpected check menu screen {other:?}")),
    }
}

async fn menu<T: Transport>(
    conv: &mut Conv<'_, T>,
    amount: &Money,
    tag: &OpTag,
    attempt: u64,
) -> Result<IssueOutcome, BotError> {
    let platform = conv.platform();
    let chat = Chat::WalletBot(platform);
    let asset = amount.asset();
    conv.wait_quarantine(chat).await;

    let command = menu_command(platform);
    let mark = conv.mark().await;
    let cmd = match conv
        .send(
            chat,
            command,
            rid::derive(tag.random_id, rid::attempt(rid::MENU_COMMAND, attempt)),
            Pace::Bot,
        )
        .await
    {
        Ok(sent) => sent,
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
    let cmd_id = cmd.msg.id;
    let mut screen = conv
        .wait(chat, cmd_id, mark, deadline, cmd.located, |m| {
            has_callback(m, |t| screens::text_matches(t, first_button))
        })
        .await
        .ok_or_else(|| BotError::Unavailable(format!("{command}: no check menu")))?;

    if platform == Platform::XRocket {
        screen = step(
            conv,
            chat,
            cmd_id,
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
        cmd_id,
        &screen,
        |t| screens::text_matches(t, CREATE_CHECK_BUTTON),
        "create check",
        |m| has_callback(m, |t| screens::is_asset_button(t, asset)),
    )
    .await?;
    step(
        conv,
        chat,
        cmd_id,
        &screen,
        |t| screens::is_asset_button(t, asset),
        "asset",
        |m| screens::text_matches(&m.text, AMOUNT_PROMPT),
    )
    .await?;

    // Сумма: у CryptoBot после этой отправки чек мог появиться.
    let mark = conv.mark().await;
    let sent = match conv
        .send(
            chat,
            &typed_amount(amount),
            rid::derive(tag.random_id, rid::attempt(rid::MENU_AMOUNT, attempt)),
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
    let amount_id = sent.msg.id;
    let deadline = conv.deadline();
    let reply = conv
        .wait_screen(chat, cmd_id, amount_id, mark, deadline, sent.located, |m| {
            creation(platform, m) != CheckCreationReply::Unknown
        })
        .await;
    let Some(reply) = reply else {
        conv.quarantine(chat);
        return Ok(unknown("no reply to the check amount"));
    };
    match creation(platform, &reply) {
        // CryptoBot: на мультичек хватает — «Пропустить ›» создаёт чек на одну активацию.
        CheckCreationReply::AskActivations => {
            finish(conv, cmd_id, amount_id, &reply, SKIP_BUTTON, "skip", amount).await
        }
        // xRocket: сумма на экране подтверждения должна совпасть с той, что просили.
        CheckCreationReply::Confirm { amount: shown } => {
            if shown != Some((amount.amount(), amount.asset())) {
                return Err(BotError::Unavailable(format!(
                    "check confirmation shows {shown:?}, expected {} {}",
                    typed_amount(amount),
                    amount.asset().code()
                )));
            }
            finish(
                conv,
                cmd_id,
                amount_id,
                &reply,
                CONFIRM_BUTTON,
                "confirm",
                amount,
            )
            .await
        }
        _ => Ok(settle(platform, &reply, amount)),
    }
}

/// Последнее нажатие, которое создаёт чек: «Пропустить ›» / «✅ Подтверждаю». Ровно одно.
async fn finish<T: Transport>(
    conv: &mut Conv<'_, T>,
    cmd_id: i32,
    amount_id: i32,
    screen: &RawMessage,
    button: &[&str],
    what: &str,
    amount: &Money,
) -> Result<IssueOutcome, BotError> {
    let platform = conv.platform();
    let chat = Chat::WalletBot(platform);
    // Без кнопки ничего не нажато — чек не создан.
    let data = callback_button(screen, |t| screens::text_matches(t, button))
        .ok_or_else(|| BotError::Unavailable(format!("check menu: no '{what}' button")))?;
    let mark = conv.mark().await;
    match conv.press(chat, screen.id, &data).await {
        Ok(answer) if answer.alert => {
            // Бот отказал всплывающим окном: экран не сменился, но подтвердить это можно
            // только сверкой.
            return Ok(unknown(format!("'{what}' answered {:?}", answer.message)));
        }
        Ok(_) => {}
        // Нажатие не ушло: чек не создан.
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            tracing::warn!(%detail, button = what, "check press outcome unknown, waiting for the screen");
        }
    }
    let deadline = conv.deadline();
    let reply = conv
        .wait_screen(chat, cmd_id, amount_id, mark, deadline, false, |m| {
            matches!(
                creation(platform, m),
                CheckCreationReply::Created(_) | CheckCreationReply::InsufficientFunds
            )
        })
        .await;
    Ok(match reply {
        Some(reply) => settle(platform, &reply, amount),
        None => {
            conv.quarantine(chat);
            unknown(format!("no check after '{what}'"))
        }
    })
}

/// Сверка после `IssueOutcome::Unknown` или рестарта: чек этой операции между её меткой
/// и следующей меткой, в чате бота. `None` — метки нет, чека нет или кандидатов несколько
/// (тогда решает человек).
pub(crate) async fn find<T: Transport>(
    w: &UserbotWallet<T>,
    amount: &Money,
    tag: &OpTag,
) -> Result<Option<IssuedCheck>, BotError> {
    let conv = Conv::open(w, Some(&tag.key)).await;
    find_with(&conv, amount, tag).await
}

/// [`find`] внутри уже открытого разговора (ворота аккаунта уже взяты).
async fn find_with<T: Transport>(
    conv: &Conv<'_, T>,
    amount: &Money,
    tag: &OpTag,
) -> Result<Option<IssuedCheck>, BotError> {
    let platform = conv.platform();
    let unavailable = |e: TransportError| BotError::Unavailable(format!("history: {e}"));
    let saved = conv
        .recent(Chat::SavedMessages)
        .await
        .map_err(unavailable)?;
    let windows = attempt_windows(&saved, &tag.key);
    if windows.is_empty() {
        tracing::warn!(op = %tag.key, "operation marker not found in Saved Messages");
        return Ok(None);
    }

    let bot = conv
        .recent(Chat::WalletBot(platform))
        .await
        .map_err(unavailable)?;
    // Экран засчитывается, только если перед ним (без других наших сообщений между) стоит
    // НАШЕ сообщение из того же окна попытки: команда меню (xRocket правит экран, пришедший на
    // `/cheques`) или сумма (CryptoBot отвечает на неё новым сообщением). Поздний экран чека
    // другой операции так не попадёт в чужое окно.
    let ours_before = |m: &RawMessage, from: i32, until: i32| {
        bot.iter()
            .filter(|o| o.out && o.id < m.id)
            .map(|o| o.id)
            .max()
            .is_some_and(|o| o > from && o < until)
    };
    let mut found: Vec<IssuedCheck> = Vec::new();
    for &(from, until) in &windows {
        for m in bot
            .iter()
            .filter(|m| !m.out && m.id > from && m.id < until && ours_before(m, from, until))
        {
            if let CheckCreationReply::Created(_) = creation(platform, m) {
                match check_in(platform, m, amount) {
                    Ok(check) => {
                        if !found.iter().any(|c| c.param == check.param) {
                            found.push(check);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(op = %tag.key, error = %e, "menu screen without a matching check")
                    }
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
