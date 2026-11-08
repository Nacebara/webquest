//! Чек, созданный через инлайн-режим кошелька (`@send 10usdt`, `@xrocket 10`) прямо в чате с
//! ботом: сначала приходит заглушка без ссылки, ссылка появляется кнопкой позже — правкой того же
//! сообщения (docs/LOVEC-PORTING.md §8.1). Клиент ничего не делает: бот достаёт ссылку сам,
//! первым сработавшим способом.
//!
//! 1. **Правка сообщения** — `edited_message` ([`crate::handler`]), обычно приходит первой.
//! 2. **Ответ-цитата.** Бот отвечает на сообщение с чеком «жду ссылку» и раз в [`STEP`] обновляет
//!    свой ответ; в ответ на отправку и каждую правку Telegram отдаёт текущий вид сообщения с
//!    чеком (`reply_to_message`) — с кнопками.
//! 3. **Служебная пересылка.** Несколько раз за ожидание бот пересылает сообщение с чеком в
//!    служебный чат (`settings.service_chat`, иначе — владельцу) без звука, читает кнопки
//!    пересланной копии и сразу её удаляет.
//!
//! За `check_link_wait` ссылки нет — клиенту «чек не открыт, деньги в нём», владельцу событие с устройством
//! сообщения (без ссылок), чтобы поправить разбор. Каким способом взят каждый чек — в журнале.

use std::sync::Arc;
use std::time::Instant;

use botapi::Message;
use domain::Platform;
use parsers::links::extract_from_message;
use parsers::{LinkError, WalletLink};
use serde_json::json;

use crate::{Inner, TgBotError, intake};

type Found = Vec<Result<WalletLink, LinkError>>;

/// Ссылки на чек или счёт в сообщении (текст, ссылки в тексте, все виды кнопок).
fn links_in(message: Option<&Message>) -> Option<Found> {
    let m = message?;
    let text = m.text_and_entities().map_or("", |(t, _)| t);
    let found = extract_from_message(text, &m.entity_urls(), &m.button_urls());
    found.iter().any(Result::is_ok).then_some(found)
}

/// Устройство сообщения для события владельцу: виды кнопок и хосты ссылок, без самих ссылок.
fn shape(message: Option<&Message>) -> serde_json::Value {
    let Some(m) = message else {
        return json!(null);
    };
    let buttons: Vec<String> = m
        .reply_markup
        .iter()
        .flat_map(|k| k.inline_keyboard.iter().flatten())
        .map(|b| match &b.action {
            botapi::InlineButtonAction::Url(u) => format!("url:{}", host(u)),
            botapi::InlineButtonAction::WebApp(u) => format!("web_app:{}", host(u)),
            botapi::InlineButtonAction::CopyText(_) => "copy_text".to_owned(),
            botapi::InlineButtonAction::CallbackData(_) => "callback".to_owned(),
            botapi::InlineButtonAction::Disabled => "disabled".to_owned(),
            botapi::InlineButtonAction::Other => "other".to_owned(),
        })
        .collect();
    json!({
        "via_bot": m.via_bot.as_ref().and_then(|b| b.username.clone()),
        "edited": m.edit_date.is_some(),
        "text_len": m.text_and_entities().map_or(0, |(t, _)| t.chars().count()),
        "buttons": buttons,
    })
}

/// Хост и путь ссылки без параметров: по ним видно форму, а код чека не утекает.
fn host(url: &str) -> String {
    url.split(['?', '#'])
        .next()
        .unwrap_or("")
        .chars()
        .take(60)
        .collect()
}

/// Запустить ожидание ссылки для сообщения `check_msg` (без ссылки, через инлайн кошелька).
pub(crate) fn start(inner: Arc<Inner>, chat: i64, user: i64, check_msg: i32, platform: Platform) {
    tokio::spawn(async move {
        if let Err(e) = run(&inner, chat, user, check_msg, platform).await {
            tracing::warn!(user, error = %e, "waiting for the check link failed");
        }
    });
}

async fn run(
    inner: &Arc<Inner>,
    chat: i64,
    user: i64,
    check_msg: i32,
    platform: Platform,
) -> Result<(), TgBotError> {
    let started = Instant::now();
    let waiting = inner
        .api
        .send_reply(chat, check_msg, &inner.ui().check_link_pending(platform))
        .await?;
    if let Some(found) = links_in(waiting.reply_to_message.as_deref()) {
        return take(
            inner,
            chat,
            user,
            check_msg,
            waiting.message_id,
            &found,
            "reply",
        )
        .await;
    }
    let mut last_seen = waiting.reply_to_message.clone();
    let cfg = &inner.cfg;
    let mut forwards = cfg.check_link_forward_at.iter().peekable();
    loop {
        tokio::time::sleep(cfg.check_link_step).await;
        // Ссылку уже взяли из правки сообщения — убрать «жду» и выйти.
        if inner.links_taken(chat, check_msg) {
            delete(inner, chat, waiting.message_id).await;
            return Ok(());
        }
        let elapsed = started.elapsed();
        if elapsed >= cfg.check_link_wait {
            break;
        }
        // 2. Обновить свой ответ — Telegram вернёт текущий вид сообщения с чеком.
        let screen = inner.ui().check_link_waiting(platform, elapsed.as_secs());
        match inner
            .api
            .edit_for_message(chat, waiting.message_id, &screen)
            .await
        {
            Ok(m) => {
                if let Some(found) = links_in(m.reply_to_message.as_deref()) {
                    return take(
                        inner,
                        chat,
                        user,
                        check_msg,
                        waiting.message_id,
                        &found,
                        "reply",
                    )
                    .await;
                }
                if m.reply_to_message.is_some() {
                    last_seen = m.reply_to_message.clone();
                }
            }
            Err(e) => tracing::debug!(error = %e, "waiting message not edited"),
        }
        // 3. Служебная пересылка — несколько раз за ожидание.
        if forwards.peek().is_some_and(|at| elapsed >= **at) {
            forwards.next();
            if let Some(found) = forward_peek(inner, chat, check_msg).await {
                return take(
                    inner,
                    chat,
                    user,
                    check_msg,
                    waiting.message_id,
                    &found,
                    "forward",
                )
                .await;
            }
        }
    }
    // Ссылка так и не появилась: чек не открыт, деньги в нём; владельцу — устройство сообщения.
    tracing::warn!(user, shape = %shape(last_seen.as_deref()), "check link never appeared");
    let screen = inner.ui().check_link_missing(platform);
    let _ = inner
        .api
        .edit_for_message(chat, waiting.message_id, &screen)
        .await;
    let mut conn = inner.pool.acquire().await?;
    storage::alerts::raise(
        &mut conn,
        &storage::alerts::NewAlert {
            level: storage::alerts::Level::Warn,
            topic: storage::alerts::Topic::Errors,
            dedup_key: &format!("check_link_missing:{chat}:{check_msg}"),
            title: "check_link_missing",
            payload: json!({
                "user_id": user,
                "platform": platform.db_name(),
                "shape": shape(last_seen.as_deref()),
            }),
        },
    )
    .await?;
    Ok(())
}

/// Переслать сообщение с чеком в служебный чат, прочитать кнопки копии и удалить её.
async fn forward_peek(inner: &Arc<Inner>, chat: i64, check_msg: i32) -> Option<Found> {
    let target = service_chat(inner).await?;
    match inner
        .api
        .bot()
        .forward_message(target, chat, check_msg)
        .await
    {
        Ok(copy) => {
            let found = links_in(Some(&copy));
            if let Err(e) = inner
                .api
                .bot()
                .delete_message(target, copy.message_id)
                .await
            {
                tracing::warn!(error = %e, "service copy of a check was not deleted");
            }
            found
        }
        Err(e) => {
            tracing::debug!(error = %e, "service forward failed");
            None
        }
    }
}

/// Служебный чат: `settings.service_chat` (`{"id": …}`), иначе личка владельца.
async fn service_chat(inner: &Arc<Inner>) -> Option<i64> {
    let mut conn = inner.pool.acquire().await.ok()?;
    if let Ok(Some(v)) = storage::settings::get(&mut conn, "service_chat").await
        && let Some(id) = v.get("id").and_then(serde_json::Value::as_i64)
    {
        return Some(id);
    }
    storage::staff::owner(&mut conn)
        .await
        .ok()
        .flatten()
        .map(|o| o.tg_user_id)
}

async fn delete(inner: &Arc<Inner>, chat: i64, message_id: i32) {
    if let Err(e) = inner.api.bot().delete_message(chat, message_id).await {
        tracing::debug!(error = %e, "waiting message not deleted");
    }
}

/// Ссылка найдена: убрать «жду» и начать заявку (или пополнение инвестора).
async fn take(
    inner: &Arc<Inner>,
    chat: i64,
    user: i64,
    check_msg: i32,
    waiting: i32,
    found: &Found,
    layer: &'static str,
) -> Result<(), TgBotError> {
    delete(inner, chat, waiting).await;
    if !inner.first_seen_links(chat, check_msg, found) {
        // Уже взята правкой сообщения.
        return Ok(());
    }
    tracing::info!(user, layer, "check link taken from the wallet's button");
    if inner.invest_message(chat, user, "", found).await? {
        return Ok(());
    }
    intake::process(inner, chat, user, found).await
}
