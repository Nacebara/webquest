//! «Предпросмотр экранов» (CLAUDE.md п. 21, PLAN-v1 1.8): бот присылает владельцу каждый
//! экран клиента и пульта настоящим сообщением на данных макета — сверить и принять. Из пульта
//! («Система» → «Предпросмотр экранов») и из демо (`/preview`, `exch demo --preview`).

use std::time::Duration;

use botapi::{BotApi, SendMessage};

use crate::Look;

/// «Предпросмотр экранов» (CLAUDE.md п. 21): все экраны клиента по порядку макета, каждый —
/// с подписью «N/всего · название», чтобы сверить с макетом и принять; затем экраны пульта.
pub async fn send_preview(api: &BotApi, look: &Look, chat: i64) {
    send_client_preview(api, look, chat, None).await;
    send_panel_preview(api, look, chat).await;
}

/// Экраны клиента, у которых в названии есть `only` (без учёта регистра; `None` — все):
/// «/preview счёт» в демо — только экраны счёта. Номера — как в полном предпросмотре.
pub async fn send_client_preview(api: &BotApi, look: &Look, chat: i64, only: Option<&str>) {
    let ui = match ui::Ui::builtin(look.emoji().clone(), ui::UiConfig::default()) {
        Ok(ui) => ui,
        Err(e) => {
            tracing::warn!(error = %e, "client texts are invalid");
            return;
        }
    };
    let ui = &ui;
    let screens = ui::preview::client_at(ui, chrono::Utc::now());
    let total = screens.len();
    let only = only.map(str::to_lowercase);
    for (i, screen) in screens.iter().enumerate() {
        if only
            .as_deref()
            .is_some_and(|w| !screen.title.to_lowercase().contains(w))
        {
            continue;
        }
        let caption = format!("<b>{}/{total} · {}</b>", i + 1, ui::html::esc(screen.title));
        let _ = api
            .send_message(&SendMessage::html(chat, caption).silent())
            .await;
        for message in &screen.messages {
            let sent = match &message.rich_html {
                Some(rich) => {
                    let mut req = botapi::SendRichMessage::new(chat, rich.clone());
                    req.disable_notification = Some(true);
                    req.reply_markup = message.keyboard.reply_markup();
                    api.send_rich_message(&req).await.map(|_| ())
                }
                None => {
                    let mut req = SendMessage::html(chat, message.html.clone()).silent();
                    req.reply_markup = message.keyboard.reply_markup();
                    api.send_message(&req).await.map(|_| ())
                }
            };
            if let Err(e) = sent {
                tracing::warn!(screen = screen.title, error = %e, "preview screen not sent");
                let _ = api
                    .send_message(&SendMessage::html(
                        chat,
                        format!(
                            "⚠️ Telegram не принял экран: {}",
                            ui::html::esc(&e.to_string())
                        ),
                    ))
                    .await;
            }
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
    }
    if only.is_none() {
        let _ = api
            .send_message(&SendMessage::html(
                chat,
                format!("<b>Готово: {total} экранов клиента.</b> Пишите номер и что не так — например «7: кнопка не того цвета». Дальше — экраны пульта."),
            ))
            .await;
    }
}

/// Экраны пульта на данных макета: главный экран в трёх состояниях, «Статистика».
pub async fn send_panel_preview(api: &BotApi, look: &Look, chat: i64) {
    let texts = match crate::Texts::builtin() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "ops texts are invalid");
            return;
        }
    };
    for (title, data) in crate::sample_dash() {
        let caption = format!("<b>Пульт · Главный экран · {title}</b> (данные макета)");
        let _ = api
            .send_message(&SendMessage::html(chat, caption).silent())
            .await;
        let screen = sample(look.screen(&data.screen(&texts)));
        let mut req = botapi::SendRichMessage::new(chat, screen.rich.clone().unwrap_or_default());
        req.disable_notification = Some(true);
        req.reply_markup = (!screen.keyboard.is_empty()).then(|| {
            botapi::ReplyMarkup::Inline(botapi::InlineKeyboardMarkup::new(screen.keyboard.clone()))
        });
        if let Err(e) = api.send_rich_message(&req).await {
            tracing::warn!(title, error = %e, "dash preview not sent");
            let _ = api
                .send_message(&SendMessage::html(
                    chat,
                    format!(
                        "⚠️ Telegram не принял экран: {}",
                        ui::html::esc(&e.to_string())
                    ),
                ))
                .await;
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
    // Меню пульта на данных макета: владелец, на разборе 2, обращение 1.
    let _ = api
        .send_message(&SendMessage::html(chat, "<b>Пульт · Меню</b> (данные макета)").silent())
        .await;
    let menu = sample(look.screen(&crate::menu_screen(
        &texts,
        storage::staff::Role::Owner,
        &crate::MenuCounts {
            review: 2,
            inbox: 1,
            ..Default::default()
        },
    )));
    let mut req = SendMessage::html(chat, menu.text.clone()).silent();
    req.reply_markup = Some(botapi::ReplyMarkup::Inline(
        botapi::InlineKeyboardMarkup::new(menu.keyboard.clone()),
    ));
    if let Err(e) = api.send_message(&req).await {
        tracing::warn!(error = %e, "menu preview not sent");
    }
    // «Резервы и сверка» и «Недостача» на данных макета.
    let _ = api
        .send_message(
            &SendMessage::html(chat, "<b>Пульт · Резервы и сверка</b> (данные макета)").silent(),
        )
        .await;
    send_card(
        api,
        chat,
        &look.screen(&crate::sample_reserves(&texts).screen(&texts)),
    )
    .await;
    let _ = api
        .send_message(&SendMessage::html(chat, "<b>Пульт · Недостача</b> (данные макета)").silent())
        .await;
    let payload = serde_json::json!({
        "platform": "xrocket", "expected": "640", "actual": "628", "diff": "-12",
        "paused": true, "account": "ub-1",
    });
    send_card(
        api,
        chat,
        &look.screen(&crate::mismatch_screen(&texts, &payload, 0)),
    )
    .await;
    // «Очередь разбора» и «Карточка заявки» на данных макета.
    let _ = api
        .send_message(
            &SendMessage::html(chat, "<b>Пульт · Очередь разбора</b> (данные макета)").silent(),
        )
        .await;
    send_card(
        api,
        chat,
        &look.screen(&crate::sample_queue().screen(&texts, &crate::Links::default(), 0)),
    )
    .await;
    let _ = api
        .send_message(
            &SendMessage::html(chat, "<b>Пульт · Карточка заявки</b> (данные макета)").silent(),
        )
        .await;
    let card = crate::sample_order().screen(
        &texts,
        storage::staff::Role::Owner,
        crate::sample_now(),
        &crate::Links::default(),
    );
    send_card(api, chat, &look.screen(&card)).await;
    // Шаг 1.5 (утверждено 13.11): сверка при неясной выплате — подсказка в карточке,
    // недостача «с учётом неясной выплаты», «Сверка не проходит».
    let _ = api
        .send_message(
            &SendMessage::html(
                chat,
                "<b>Пульт · Сверка при неясной выплате</b> (данные макета)",
            )
            .silent(),
        )
        .await;
    let mut hinted = crate::sample_order();
    hinted.reconcile = Some(crate::ReconcileHint {
        at: crate::sample_now(),
        platform: domain::Platform::XRocket,
        kind: "create_payout_check".into(),
        done: Some(true),
    });
    let card = hinted.screen(
        &texts,
        storage::staff::Role::Owner,
        crate::sample_now(),
        &crate::Links::default(),
    );
    send_card(api, chat, &look.screen(&card)).await;
    let payload = serde_json::json!({
        "platform": "xrocket", "expected": "902.5", "ledger": "1000", "actual": "890.5",
        "diff": "-12", "paused": true, "account": "ub-1", "below_all": true,
        "unsettled": [{"id": 7, "public": "K7M2Q9XR4T", "kind": "create_payout_check", "status": "unknown", "amount": "97.5"}],
    });
    send_card(
        api,
        chat,
        &look.screen(&crate::mismatch_screen(&texts, &payload, 0)),
    )
    .await;
    let payload = serde_json::json!({
        "platform": "xrocket", "since": (chrono::Utc::now() - chrono::Duration::minutes(35)).to_rfc3339(),
        "minutes": 35, "reason": "in_progress",
        "ops": [{"id": 7, "public": "K7M2Q9XR4T", "kind": "create_payout_check", "status": "dispatched", "amount": "97.5"}],
    });
    send_card(
        api,
        chat,
        &look.screen(&crate::stalled_screen(&texts, &payload, 0)),
    )
    .await;
    // Пульт: «Статистика» на данных макета — плитки и график одной картинкой.
    let _ = api
        .send_message(
            &SendMessage::html(chat, "<b>Пульт · Статистика</b> (данные макета)").silent(),
        )
        .await;
    match crate::stats::sample_card()
        .map(|c| look.stats(c))
        .map(|mut c| {
            sample_keyboard(&mut c.keyboard.inline_keyboard, &crate::Cb::Sample.encode());
            c
        }) {
        Ok(card) => match card.chart.png() {
            Ok(png) => {
                if let Err(e) = api
                    .send_rich_with_photo(chat, &card.html, "chart", png, Some(&card.keyboard))
                    .await
                {
                    tracing::warn!(error = %e, "stats preview not sent");
                }
            }
            Err(e) => tracing::warn!(error = %e, "stats chart not rendered"),
        },
        Err(e) => tracing::warn!(error = %e, "stats sample failed"),
    }
}

/// Экран пульта: карточкой (rich), если она есть, иначе обычным HTML; кнопки — как есть.
/// Пример экрана для «Предпросмотра»: у всех кнопок — и под сообщением, и в тексте карточки —
/// вместо настоящего действия [`crate::Cb::Sample`] (ссылки и «копировать» остаются). На
/// данных макета id настоящие: «Закрыть без выплаты» на примере сработала бы по живой заявке
/// №15 (аудит v2, П13).
pub(crate) fn sample(mut screen: crate::Screen) -> crate::Screen {
    let data = crate::Cb::Sample.encode();
    sample_keyboard(&mut screen.keyboard, &data);
    screen.rich = screen.rich.map(|r| sample_text(&r, &data));
    screen.text = sample_text(&screen.text, &data);
    screen
}

fn sample_keyboard(rows: &mut [Vec<botapi::InlineKeyboardButton>], data: &str) {
    for button in rows.iter_mut().flatten() {
        if let botapi::InlineButtonAction::CallbackData(d) = &mut button.action {
            *d = data.to_owned();
        }
    }
}

/// `data="…"` у кнопок в тексте (`<tg-button type="callback_data" … data="…">`).
fn sample_text(html: &str, data: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(" data=\"") {
        let (head, tail) = rest.split_at(i + " data=\"".len());
        out.push_str(head);
        out.push_str(data);
        match tail.find('"') {
            Some(end) => rest = &tail[end..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

async fn send_card(api: &BotApi, chat: i64, screen: &crate::Screen) {
    let screen = &sample(screen.clone());
    let markup = (!screen.keyboard.is_empty()).then(|| {
        botapi::ReplyMarkup::Inline(botapi::InlineKeyboardMarkup::new(screen.keyboard.clone()))
    });
    let sent = match &screen.rich {
        Some(rich) => {
            let mut req = botapi::SendRichMessage::new(chat, rich.clone());
            req.disable_notification = Some(true);
            req.reply_markup = markup;
            api.send_rich_message(&req).await.map(|_| ())
        }
        None => {
            let mut req = SendMessage::html(chat, screen.text.clone()).silent();
            req.reply_markup = markup;
            api.send_message(&req).await.map(|_| ())
        }
    };
    if let Err(e) = sent {
        tracing::warn!(error = %e, "panel preview screen not sent");
        let _ = api
            .send_message(&SendMessage::html(
                chat,
                format!(
                    "⚠️ Telegram не принял экран: {}",
                    ui::html::esc(&e.to_string())
                ),
            ))
            .await;
    }
    tokio::time::sleep(Duration::from_millis(350)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// П13: на примерах из «Предпросмотра» ни одна кнопка не ведёт к настоящему действию.
    #[test]
    fn samples_carry_no_live_buttons() {
        let texts = crate::Texts::builtin().unwrap();
        let card = crate::sample_order().screen(
            &texts,
            storage::staff::Role::Owner,
            crate::sample_now(),
            &crate::Links::default(),
        );
        let queue = crate::sample_queue().screen(&texts, &crate::Links::default(), 0);
        let mut url = crate::Screen::new("x");
        url.keyboard = vec![vec![
            botapi::InlineKeyboardButton::url("t.me", "https://t.me/x"),
            botapi::InlineKeyboardButton::callback("go", crate::Cb::Order(15).encode()),
        ]];
        for screen in [card, queue, url] {
            let s = sample(screen);
            let data = crate::Cb::Sample.encode();
            for b in s.keyboard.iter().flatten() {
                if let botapi::InlineButtonAction::CallbackData(d) = &b.action {
                    assert_eq!(d, &data, "{}", b.text);
                }
            }
            for html in std::iter::once(&s.text).chain(s.rich.as_ref()) {
                for part in html.split(" data=\"").skip(1) {
                    assert!(part.starts_with(&data), "{part}");
                }
            }
        }
        let s = sample({
            let mut u = crate::Screen::new("x");
            u.keyboard = vec![vec![botapi::InlineKeyboardButton::url(
                "t.me",
                "https://t.me/x",
            )]];
            u
        });
        assert_eq!(
            s.keyboard[0][0].action,
            botapi::InlineButtonAction::Url("https://t.me/x".into())
        );
    }
}
