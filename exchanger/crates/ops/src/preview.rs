//! «Предпросмотр экранов» (CLAUDE.md п. 21, PLAN-v1 1.8): бот присылает владельцу каждый
//! экран клиента и пульта настоящим сообщением на данных макета — сверить и принять. Из пульта
//! («Система» → «Предпросмотр экранов») и из демо (`/preview`, `exch demo --preview`).

use std::time::Duration;

use botapi::{BotApi, SendMessage, SendWidget};
use ui::widgets::WidgetKind;

use crate::Look;

/// «Предпросмотр экранов» (CLAUDE.md п. 21): все экраны клиента по порядку макета, каждый —
/// с подписью «N/всего · название», чтобы сверить с макетом и принять; затем экраны пульта.
pub async fn send_preview(api: &BotApi, look: &Look, chat: i64) {
    send_client_preview(api, look, chat, None).await;
    send_panel_preview(api, look, chat).await;
    send_widget_preview(api, look, chat).await;
}

/// Данные макетов виджетов, принятых владельцем 13–14 ноября: подпись, кнопки и состояние.
const WIDGET_SAMPLES: &str = include_str!("../../../docs/mockups/widgets/samples.json");

/// Виджет на данных макета — для «Предпросмотра» (шаг 6.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WidgetSample {
    pub kind: WidgetKind,
    /// Подпись с токенами пульта `{e:…}` / `{m:…}`.
    pub caption: String,
    /// Кнопки: стиль, значок (`c:wait` — цветной), подпись.
    pub keys: Vec<Vec<(String, String, String)>>,
    /// Состояние `chat_data` (JSON).
    pub data: String,
}

/// Токены макетов (`{cb}` — цветной значок, `{-shield}` — линейный) → токены пульта.
fn mockup_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let (mono, name_from) = match tail.strip_prefix('-') {
            Some(t) => (true, t),
            None => (false, tail),
        };
        let end = name_from
            .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(name_from.len());
        if end > 0 && name_from[end..].starts_with('}') {
            out.push_str(if mono { "{m:" } else { "{e:" });
            out.push_str(&name_from[..end]);
            out.push('}');
            rest = &name_from[end + 1..];
        } else {
            out.push('{');
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// «@-120» в макете — время относительно «сейчас» → unix-время `now − 120`.
fn live(v: &serde_json::Value, now: i64) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) => {
            let shift = s
                .strip_prefix('@')
                .filter(|t| t.len() > 1 && (t.starts_with('+') || t.starts_with('-')))
                .and_then(|t| t.parse::<i64>().ok());
            match shift {
                Some(d) => Value::from(now + d),
                None => v.clone(),
            }
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| live(x, now)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, x)| (k.clone(), live(x, now))).collect())
        }
        other => other.clone(),
    }
}

/// Первый вид каждого утверждённого виджета из макетов — по порядку [`WidgetKind::ALL`].
pub(crate) fn widget_samples(now: i64) -> Vec<WidgetSample> {
    let Ok(serde_json::Value::Object(demos)) = serde_json::from_str(WIDGET_SAMPLES) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for kind in WidgetKind::ALL {
        let state = demos.values().find_map(|d| {
            (d.get("w").and_then(|w| w.as_str()) == Some(kind.name()))
                .then(|| d.get("states").and_then(|s| s.as_array()))
                .flatten()
                .and_then(|states| states.iter().find(|s| s.get("rich").is_none()))
        });
        let Some(state) = state else { continue };
        let keys = state
            .get("keys")
            .and_then(|k| k.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r.as_array())
                    .map(|r| {
                        r.iter()
                            .filter_map(|b| b.as_array())
                            .map(|b| {
                                let f = |i: usize| {
                                    b.get(i).and_then(|x| x.as_str()).unwrap_or("").to_owned()
                                };
                                (f(0), f(1), mockup_tokens(&f(2)))
                            })
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(WidgetSample {
            kind,
            caption: mockup_tokens(state.get("caption").and_then(|c| c.as_str()).unwrap_or("")),
            keys,
            data: state
                .get("data")
                .map(|d| live(d, now).to_string())
                .unwrap_or_default(),
        });
    }
    out
}

impl WidgetSample {
    /// Сообщение-виджет: «N/всего · название» и подпись макета, кнопки — примеры
    /// ([`crate::Cb::Sample`]), значки — из набора.
    pub(crate) fn request(&self, look: &Look, chat: i64, n: usize, total: usize) -> SendWidget {
        let data = crate::Cb::Sample.encode();
        let rows: Vec<Vec<botapi::InlineKeyboardButton>> = self
            .keys
            .iter()
            .map(|row| {
                row.iter()
                    .map(|(style, icon, text)| {
                        let label = match icon.strip_prefix("c:") {
                            Some(c) => format!("{{e:{c}}} {text}"),
                            None if icon.is_empty() => text.clone(),
                            None => format!("{{m:{icon}}} {text}"),
                        };
                        let mut b = botapi::InlineKeyboardButton::callback(label, data.clone());
                        b.style = match style.as_str() {
                            "primary" => Some(botapi::ButtonStyle::Primary),
                            "success" => Some(botapi::ButtonStyle::Success),
                            "danger" => Some(botapi::ButtonStyle::Danger),
                            _ => None,
                        };
                        look.button(b)
                    })
                    .collect()
            })
            .collect();
        let caption = format!(
            "<i>{n}/{total} · {}</i>\n{}",
            ui::html::esc(self.kind.title()),
            look.text(&self.caption)
        );
        let mut req = SendWidget::new(chat, self.kind.html())
            .caption_html(caption)
            .allow_queries()
            .silent();
        if !self.data.is_empty() {
            req = req.chat_data(self.data.clone());
        }
        if !rows.is_empty() {
            req = req.reply_markup(botapi::InlineKeyboardMarkup::new(rows));
        }
        req
    }
}

/// Виджеты на данных макетов (шаг 6.7): каждый утверждённый виджет настоящим сообщением —
/// посмотреть в своём Telegram, светлая и тёмная тема, телефон и компьютер.
pub async fn send_widget_preview(api: &BotApi, look: &Look, chat: i64) {
    let samples = widget_samples(chrono::Utc::now().timestamp());
    let total = samples.len();
    let _ = api
        .send_message(
            &SendMessage::html(chat, format!("<b>Виджеты</b> · данные макетов · {total}")).silent(),
        )
        .await;
    for (i, sample) in samples.iter().enumerate() {
        let req = sample.request(look, chat, i + 1, total);
        if let Err(e) = api.send_widget(&req).await {
            tracing::warn!(widget = sample.kind.name(), error = %e, "widget preview not sent");
            if e.widget_not_allowed() {
                // Остальные не пройдут тоже: одно сообщение вместо девятнадцати.
                let _ = api
                    .send_message(&SendMessage::html(
                        chat,
                        "⚠️ Telegram пока не разрешает этому боту виджеты \
                         (<code>INPUT_MEDIA_WIDGET_NOT_ALLOWED</code>). Экраны работают и без них: \
                         подпись и кнопки — те же.",
                    ))
                    .await;
                return;
            }
            let _ = api
                .send_message(&SendMessage::html(
                    chat,
                    format!(
                        "⚠️ Telegram не принял виджет «{}»: {}",
                        ui::html::esc(sample.kind.title()),
                        ui::html::esc(&e.to_string())
                    ),
                ))
                .await;
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
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
    // Критичные тревоги шага 2.8 (утверждены владельцем 14.11).
    let now = chrono::Utc::now();
    let critical = [
        (
            "login_lost",
            serde_json::json!({"account": "ub-2", "userbot_id": 2, "platforms": ["xrocket"]}),
        ),
        (
            "account_restricted",
            serde_json::json!({"account": "ub-1", "userbot_id": 1, "platforms": ["cryptobot"]}),
        ),
        (
            "flood_long",
            serde_json::json!({"account": "ub-1", "userbot_id": 1, "platforms": ["cryptobot"],
            "wait_secs": 2820, "until": (now + chrono::Duration::minutes(47)).to_rfc3339()}),
        ),
        (
            "engine_down",
            serde_json::json!({"since": (now - chrono::Duration::minutes(6)).to_rfc3339(),
            "error": "another engine instance is running"}),
        ),
        ("second_instance", serde_json::json!({})),
        (
            "unknown_reply",
            serde_json::json!({"platform": "xrocket", "text": "Чек CQ12…9F временно недоступен, попробуйте позже",
            "order": 41, "public": "7F3A1C9D2B", "direction": "xr_to_cb_check", "at": now.to_rfc3339()}),
        ),
        (
            "kill_switch",
            serde_json::json!({"by": "Олег", "at": now.to_rfc3339()}),
        ),
    ];
    for (title, payload) in critical {
        if let Some(screen) = crate::critical_screen(&texts, title, &payload, 0, now) {
            send_card(api, chat, &look.screen(&screen)).await;
        }
    }
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

    /// Шаг 6.7: «Предпросмотр» присылает каждый утверждённый виджет на данных макета; подпись
    /// и состояние — в лимитах Telegram, значки — токенами набора, кнопки — только примеры.
    #[test]
    fn preview_sends_every_widget_within_limits() {
        let now = 1_793_877_600;
        let samples = widget_samples(now);
        let kinds: Vec<WidgetKind> = samples.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            WidgetKind::ALL.to_vec(),
            "каждый виджет — ровно один раз"
        );
        let look = Look::plain();
        for s in &samples {
            let name = s.kind.name();
            assert!(!s.caption.contains("{-"), "{name}: {}", s.caption);
            assert!(
                !s.data.contains("\"@"),
                "{name}: время не подставлено: {}",
                s.data
            );
            assert!(
                ui::widgets::data_fits(&s.data),
                "{name}: {} байт",
                s.data.len()
            );
            let req = s.request(&look, 42, 1, samples.len());
            let caption = req.caption.clone().unwrap_or_default();
            assert!(
                caption.chars().count() <= ui::widgets::CAPTION_MAX,
                "{name}: подпись {}",
                caption.chars().count()
            );
            assert!(
                !caption.contains("{e:") && !caption.contains("{m:"),
                "{name}: {caption}"
            );
            assert_eq!(req.html, s.kind.html());
            if let Some(botapi::ReplyMarkup::Inline(m)) = &req.reply_markup {
                for b in m.inline_keyboard.iter().flatten() {
                    assert_eq!(
                        b.action,
                        botapi::InlineButtonAction::CallbackData(crate::Cb::Sample.encode()),
                        "{name}: {}",
                        b.text
                    );
                }
            }
        }
        assert_eq!(
            mockup_tokens("{-shield} {cb} <b>x</b> {}"),
            "{m:shield} {e:cb} <b>x</b> {}"
        );
    }

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
