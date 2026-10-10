//! Преобразование объектов TL (слой 227) в типы контракта `userbot::transport`.
//!
//! Чистые функции без сети — покрыты тестами офлайн. Что учтено:
//! - чат определяется только по закреплённым user id ботов и своему id (CLAUDE.md п. 10):
//!   всё остальное (люди, группы, каналы) отбрасывается;
//! - `UpdateShortMessage` grammers превращает в `UpdateNewMessage` **без** `reply_markup`
//!   (в короткой форме его нет); перечитать сообщение можно через `history(chat, id - 1, 1)`;
//! - смещения сущностей — в UTF-16 (как считает Telegram), а не в байтах и не в `char`;
//! - кнопка `callback` с `requires_password` (нужен SRP 2FA) отдаётся как `Other`: такое
//!   нажатие юзербот не делает, сценарий уходит человеку (LOVEC-PORTING §8.4);
//! - кнопки обычной (reply) клавиатуры в `buttons` не попадают: контракт — инлайн-клавиатура;
//! - rich-сообщения (`rich_message`, блоки страницы) в текст не разворачиваются: такой ответ
//!   бота придёт с пустым текстом и будет `Unknown` у парсеров (безопасно: пауза направления).

use domain::Platform;
use grammers_tl_types as tl;
use userbot::transport::{
    ButtonKind, CallbackAnswer, Chat, InlineResult, InlineResults, RawButton, RawMessage,
};

use crate::config::{PinnedBot, PinnedBots};

/// Кому принадлежат чаты: свой аккаунт и закреплённые боты кошельков.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatContext {
    pub self_id: i64,
    pub cryptobot_id: i64,
    pub xrocket_id: i64,
}

impl ChatContext {
    pub fn new(self_id: i64, bots: &PinnedBots) -> Self {
        Self {
            self_id,
            cryptobot_id: bots.cryptobot.id,
            xrocket_id: bots.xrocket.id,
        }
    }

    pub fn platform_of(&self, user_id: i64) -> Option<Platform> {
        if user_id == self.cryptobot_id {
            Some(Platform::CryptoBot)
        } else if user_id == self.xrocket_id {
            Some(Platform::XRocket)
        } else {
            None
        }
    }

    pub fn bot_id(&self, platform: Platform) -> i64 {
        match platform {
            Platform::CryptoBot => self.cryptobot_id,
            Platform::XRocket => self.xrocket_id,
        }
    }

    /// Чат юзербота по `peer_id` сообщения; `None` — чужой чат, игнорируем.
    pub fn chat_of_peer(&self, peer: &tl::enums::Peer) -> Option<Chat> {
        match peer {
            tl::enums::Peer::User(u) if u.user_id == self.self_id => Some(Chat::SavedMessages),
            tl::enums::Peer::User(u) => self.platform_of(u.user_id).map(Chat::WalletBot),
            tl::enums::Peer::Chat(_) | tl::enums::Peer::Channel(_) => None,
        }
    }
}

/// Тип кнопки мини-приложения: от него зависит метод `requestWebView` / `requestSimpleWebView`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebAppKind {
    /// `keyboardButtonWebView` → `messages.requestWebView` (peer = бот).
    WebView,
    /// `keyboardButtonSimpleWebView` → `messages.requestSimpleWebView`.
    SimpleWebView,
}

/// Одна кнопка инлайн-клавиатуры.
pub fn button_of(button: &tl::enums::KeyboardButton) -> RawButton {
    use tl::enums::KeyboardButton as B;
    let kind = match button {
        B::Url(b) => ButtonKind::Url(b.url.clone()),
        B::UrlAuth(b) => ButtonKind::Url(b.url.clone()),
        B::Callback(b) if !b.requires_password => ButtonKind::Callback(b.data.clone()),
        B::WebView(b) => ButtonKind::WebApp(b.url.clone()),
        B::SimpleWebView(b) => ButtonKind::WebApp(b.url.clone()),
        B::SwitchInline(b) => ButtonKind::SwitchInline(b.query.clone()),
        B::Copy(b) => ButtonKind::CopyText(b.copy_text.clone()),
        B::Callback(_)
        | B::Button(_)
        | B::RequestPhone(_)
        | B::RequestGeoLocation(_)
        | B::Game(_)
        | B::Buy(_)
        | B::InputKeyboardButtonUrlAuth(_)
        | B::RequestPoll(_)
        | B::InputKeyboardButtonUserProfile(_)
        | B::UserProfile(_)
        | B::RequestPeer(_)
        | B::InputKeyboardButtonRequestPeer(_) => ButtonKind::Other,
    };
    RawButton {
        text: button.text(),
        kind,
    }
}

/// Инлайн-клавиатура по рядам; reply-клавиатура и прочая разметка — пусто.
pub fn buttons_of(markup: Option<&tl::enums::ReplyMarkup>) -> Vec<Vec<RawButton>> {
    match markup {
        Some(tl::enums::ReplyMarkup::ReplyInlineMarkup(m)) => m
            .rows
            .iter()
            .map(|tl::enums::KeyboardButtonRow::Row(row)| {
                row.buttons.iter().map(button_of).collect()
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Кнопки мини-приложений сообщения с их типом.
pub fn webapp_buttons(markup: Option<&tl::enums::ReplyMarkup>) -> Vec<(String, WebAppKind)> {
    let Some(tl::enums::ReplyMarkup::ReplyInlineMarkup(m)) = markup else {
        return Vec::new();
    };
    m.rows
        .iter()
        .flat_map(|tl::enums::KeyboardButtonRow::Row(row)| row.buttons.iter())
        .filter_map(|b| match b {
            tl::enums::KeyboardButton::WebView(b) => Some((b.url.clone(), WebAppKind::WebView)),
            tl::enums::KeyboardButton::SimpleWebView(b) => {
                Some((b.url.clone(), WebAppKind::SimpleWebView))
            }
            _ => None,
        })
        .collect()
}

/// Подстрока по смещению и длине в единицах UTF-16 (как в `MessageEntity`).
pub fn utf16_slice(text: &str, offset: i32, length: i32) -> Option<String> {
    let start = usize::try_from(offset).ok()?;
    let len = usize::try_from(length).ok()?;
    let end = start.checked_add(len)?;
    let units: Vec<u16> = text.encode_utf16().collect();
    let slice = units.get(start..end)?;
    String::from_utf16(slice).ok()
}

/// URL из сущностей `text_link` (скрытая ссылка) и `url` (ссылка в тексте), по порядку.
pub fn entity_urls(text: &str, entities: Option<&[tl::enums::MessageEntity]>) -> Vec<String> {
    entities
        .unwrap_or_default()
        .iter()
        .filter_map(|e| match e {
            tl::enums::MessageEntity::TextUrl(e) => Some(e.url.clone()),
            tl::enums::MessageEntity::Url(e) => utf16_slice(text, e.offset, e.length),
            _ => None,
        })
        .collect()
}

/// Сообщение из чата юзербота; `None` — пустое, служебное или из чужого чата.
pub fn message_to_raw(message: &tl::enums::Message, ctx: &ChatContext) -> Option<RawMessage> {
    let tl::enums::Message::Message(m) = message else {
        return None;
    };
    let chat = ctx.chat_of_peer(&m.peer_id)?;
    Some(RawMessage {
        chat,
        id: m.id,
        out: m.out,
        date: i64::from(m.date),
        edit_date: m.edit_date.map(i64::from),
        text: m.message.clone(),
        entity_urls: entity_urls(&m.message, m.entities.as_deref()),
        buttons: buttons_of(m.reply_markup.as_ref()),
        via_bot: m.via_bot_id.and_then(|id| ctx.platform_of(id)),
    })
}

/// Новое или изменённое сообщение из апдейта (`UpdateShortMessage` grammers уже развернул).
pub fn update_to_raw(update: &tl::enums::Update, ctx: &ChatContext) -> Option<RawMessage> {
    match update {
        tl::enums::Update::NewMessage(u) => message_to_raw(&u.message, ctx),
        tl::enums::Update::EditMessage(u) => message_to_raw(&u.message, ctx),
        _ => None,
    }
}

/// Сырое сообщение апдейта (для запоминания кнопок мини-приложений).
pub fn update_message(update: &tl::enums::Update) -> Option<&tl::types::Message> {
    match update {
        tl::enums::Update::NewMessage(tl::types::UpdateNewMessage {
            message: tl::enums::Message::Message(m),
            ..
        })
        | tl::enums::Update::EditMessage(tl::types::UpdateEditMessage {
            message: tl::enums::Message::Message(m),
            ..
        }) => Some(m),
        _ => None,
    }
}

/// Что нашлось о нашем сообщении в ответе `sendMessage` / `sendInlineBotResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum SentMessage {
    /// Полное сообщение из `UpdateNewMessage` (с кнопками и `via_bot`).
    Message(RawMessage),
    /// `UpdateShortSentMessage`: id, дата и сущности; текст — тот, что мы отправили.
    Short {
        id: i32,
        date: i64,
        entities: Option<Vec<tl::enums::MessageEntity>>,
    },
    /// `UpdateMessageID` с нашим `random_id`, но без самого сообщения.
    IdOnly { id: i32, date: i64 },
    /// Сервер принял запрос, но нашего сообщения в ответе нет.
    NotFound,
}

/// Найти наше сообщение в `Updates` по `random_id` (порт lovec `fastsend.rs:174-199`).
///
/// Порядок: `UpdateShortSentMessage` → `UpdateMessageID` с нашим `random_id` (+ само сообщение
/// с этим id) → если `UpdateMessageID` нет вовсе, единственное исходящее сообщение в нужном чате.
pub fn find_sent_message(
    updates: &tl::enums::Updates,
    random_id: i64,
    chat: Chat,
    ctx: &ChatContext,
) -> SentMessage {
    let (list, date): (&[tl::enums::Update], i32) = match updates {
        tl::enums::Updates::UpdateShortSentMessage(s) => {
            return SentMessage::Short {
                id: s.id,
                date: i64::from(s.date),
                entities: s.entities.clone(),
            };
        }
        tl::enums::Updates::Updates(u) => (&u.updates, u.date),
        tl::enums::Updates::Combined(u) => (&u.updates, u.date),
        tl::enums::Updates::UpdateShort(u) => (std::slice::from_ref(&u.update), u.date),
        tl::enums::Updates::TooLong
        | tl::enums::Updates::UpdateShortMessage(_)
        | tl::enums::Updates::UpdateShortChatMessage(_) => return SentMessage::NotFound,
    };

    let new_messages = || {
        list.iter().filter_map(|u| match u {
            tl::enums::Update::NewMessage(u) => message_to_raw(&u.message, ctx),
            _ => None,
        })
    };

    let mut any_message_id = false;
    let mut ours = None;
    for u in list {
        if let tl::enums::Update::MessageId(m) = u {
            any_message_id = true;
            if m.random_id == random_id {
                ours = Some(m.id);
            }
        }
    }

    match ours {
        Some(id) => new_messages()
            .find(|m| m.id == id && m.chat == chat)
            .map(SentMessage::Message)
            .unwrap_or(SentMessage::IdOnly {
                id,
                date: i64::from(date),
            }),
        None if any_message_id => SentMessage::NotFound,
        None => {
            let mut outgoing = new_messages().filter(|m| m.out && m.chat == chat);
            match (outgoing.next(), outgoing.next()) {
                (Some(only), None) => SentMessage::Message(only),
                _ => SentMessage::NotFound,
            }
        }
    }
}

/// Наше текстовое сообщение по краткому ответу сервера.
pub fn sent_text_message(
    chat: Chat,
    id: i32,
    date: i64,
    text: &str,
    entities: Option<&[tl::enums::MessageEntity]>,
) -> RawMessage {
    RawMessage {
        chat,
        id,
        out: true,
        date,
        edit_date: None,
        text: text.to_owned(),
        entity_urls: entity_urls(text, entities),
        buttons: Vec::new(),
        via_bot: None,
    }
}

/// Результаты инлайн-запроса.
pub fn inline_results(results: &tl::enums::messages::BotResults) -> InlineResults {
    let tl::enums::messages::BotResults::Results(r) = results;
    InlineResults {
        query_id: r.query_id,
        results: r
            .results
            .iter()
            .map(|res| match res {
                tl::enums::BotInlineResult::Result(b) => InlineResult {
                    id: b.id.clone(),
                    title: b.title.clone(),
                    description: b.description.clone(),
                },
                tl::enums::BotInlineResult::BotInlineMediaResult(b) => InlineResult {
                    id: b.id.clone(),
                    title: b.title.clone(),
                    description: b.description.clone(),
                },
            })
            .collect(),
    }
}

pub fn callback_answer(answer: &tl::enums::messages::BotCallbackAnswer) -> CallbackAnswer {
    let tl::enums::messages::BotCallbackAnswer::Answer(a) = answer;
    CallbackAnswer {
        message: a.message.clone(),
        alert: a.alert,
        url: a.url.clone(),
    }
}

pub fn webview_url(result: &tl::enums::WebViewResult) -> String {
    let tl::enums::WebViewResult::Url(u) = result;
    u.url.clone()
}

/// Сообщения из ответа `getHistory` / `getMessages`.
pub fn messages_of(result: tl::enums::messages::Messages) -> Vec<tl::enums::Message> {
    match result {
        tl::enums::messages::Messages::Messages(m) => m.messages,
        tl::enums::messages::Messages::Slice(m) => m.messages,
        tl::enums::messages::Messages::ChannelMessages(m) => m.messages,
        tl::enums::messages::Messages::NotModified(_) => Vec::new(),
    }
}

/// Сообщения чата `chat` с id > `after_id`, по возрастанию id, не больше `limit`.
pub fn history_page(
    messages: &[tl::enums::Message],
    chat: Chat,
    after_id: i32,
    limit: usize,
    ctx: &ChatContext,
) -> Vec<RawMessage> {
    let mut out: Vec<RawMessage> = messages
        .iter()
        .filter_map(|m| message_to_raw(m, ctx))
        .filter(|m| m.chat == chat && m.id > after_id)
        .collect();
    out.sort_by_key(|m| m.id);
    out.dedup_by_key(|m| m.id);
    out.truncate(limit);
    out
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BotIdentityError {
    #[error("pinned bot {platform:?} (id {expected}) was not found")]
    NotFound { platform: Platform, expected: i64 },
    #[error("username of {platform:?} resolves to id {got}, pinned id is {expected}")]
    IdMismatch {
        platform: Platform,
        expected: i64,
        got: i64,
    },
    #[error("pinned {platform:?} id {id} is not a bot account")]
    NotABot { platform: Platform, id: i64 },
    #[error("pinned {platform:?} id {id} has usernames {got:?}, expected @{expected}")]
    UsernameMismatch {
        platform: Platform,
        id: i64,
        expected: String,
        got: Vec<String>,
    },
    #[error("no access hash for pinned {platform:?} id {id}")]
    NoAccessHash { platform: Platform, id: i64 },
}

/// Сверить пользователя с закреплённым ботом: тот же id, это бот, username совпадает с
/// основным или активным дополнительным (без учёта регистра). Возвращает access hash.
pub fn verify_bot_user(
    user: &tl::enums::User,
    pinned: &PinnedBot,
) -> Result<i64, BotIdentityError> {
    let platform = pinned.platform;
    let tl::enums::User::User(u) = user else {
        return Err(BotIdentityError::NotFound {
            platform,
            expected: pinned.id,
        });
    };
    if u.id != pinned.id {
        return Err(BotIdentityError::IdMismatch {
            platform,
            expected: pinned.id,
            got: u.id,
        });
    }
    if !u.bot || u.deleted {
        return Err(BotIdentityError::NotABot { platform, id: u.id });
    }
    let mut names: Vec<String> = u.username.iter().cloned().collect();
    names.extend(
        u.usernames
            .iter()
            .flatten()
            .filter(|tl::enums::Username::Username(n)| n.active)
            .map(|tl::enums::Username::Username(n)| n.username.clone()),
    );
    let expected = pinned.username.trim_start_matches('@');
    if !names.iter().any(|n| n.eq_ignore_ascii_case(expected)) {
        return Err(BotIdentityError::UsernameMismatch {
            platform,
            id: u.id,
            expected: expected.to_owned(),
            got: names,
        });
    }
    u.access_hash
        .ok_or(BotIdentityError::NoAccessHash { platform, id: u.id })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tl::{Deserializable, Serializable};

    pub(crate) const SELF_ID: i64 = 777_000;
    pub(crate) const CB: i64 = 1_559_501_630;
    pub(crate) const XR: i64 = 5_014_831_088;

    pub(crate) fn ctx() -> ChatContext {
        ChatContext::new(SELF_ID, &PinnedBots::default())
    }

    pub(crate) fn peer_user(id: i64) -> tl::enums::Peer {
        tl::types::PeerUser { user_id: id }.into()
    }

    /// Сообщение слоя 227 со всеми необязательными полями пустыми.
    pub(crate) fn tl_message(id: i32, peer: i64, out: bool, text: &str) -> tl::types::Message {
        tl::types::Message {
            out,
            mentioned: false,
            media_unread: false,
            silent: false,
            post: false,
            from_scheduled: false,
            legacy: false,
            edit_hide: false,
            pinned: false,
            noforwards: false,
            invert_media: false,
            offline: false,
            video_processing_pending: false,
            paid_suggested_post_stars: false,
            paid_suggested_post_ton: false,
            id,
            from_id: None,
            from_boosts_applied: None,
            from_rank: None,
            peer_id: peer_user(peer),
            saved_peer_id: None,
            fwd_from: None,
            via_bot_id: None,
            via_business_bot_id: None,
            guestchat_via_from: None,
            reply_to: None,
            date: 1_760_000_000,
            message: text.to_owned(),
            media: None,
            reply_markup: None,
            entities: None,
            views: None,
            forwards: None,
            replies: None,
            edit_date: None,
            post_author: None,
            grouped_id: None,
            reactions: None,
            restriction_reason: None,
            ttl_period: None,
            quick_reply_shortcut_id: None,
            effect: None,
            factcheck: None,
            report_delivery_until_date: None,
            paid_message_stars: None,
            suggested_post: None,
            schedule_repeat_period: None,
            summary_from_language: None,
            rich_message: None,
        }
    }

    fn row(buttons: Vec<tl::enums::KeyboardButton>) -> tl::enums::KeyboardButtonRow {
        tl::types::KeyboardButtonRow { buttons }.into()
    }

    pub(crate) fn inline_markup(
        rows: Vec<Vec<tl::enums::KeyboardButton>>,
    ) -> tl::enums::ReplyMarkup {
        tl::types::ReplyInlineMarkup {
            rows: rows.into_iter().map(row).collect(),
        }
        .into()
    }

    pub(crate) fn url_button(text: &str, url: &str) -> tl::enums::KeyboardButton {
        tl::types::KeyboardButtonUrl {
            style: None,
            text: text.into(),
            url: url.into(),
        }
        .into()
    }

    pub(crate) fn callback_button(text: &str, data: &[u8]) -> tl::enums::KeyboardButton {
        tl::types::KeyboardButtonCallback {
            requires_password: false,
            style: None,
            text: text.into(),
            data: data.to_vec(),
        }
        .into()
    }

    fn all_buttons() -> Vec<Vec<tl::enums::KeyboardButton>> {
        vec![
            vec![
                url_button("Забрать", "https://t.me/send?start=CQAbCdEfGhIj"),
                callback_button("USDT", b"asset:usdt"),
            ],
            vec![
                tl::types::KeyboardButtonWebView {
                    style: None,
                    text: "Оплатить".into(),
                    url: "https://app.send.tg/invoices/IVabc".into(),
                }
                .into(),
                tl::types::KeyboardButtonSimpleWebView {
                    style: None,
                    text: "Кошелёк".into(),
                    url: "https://app.send.tg/wallet".into(),
                }
                .into(),
            ],
            vec![
                tl::types::KeyboardButtonSwitchInline {
                    same_peer: false,
                    style: None,
                    text: "Поделиться".into(),
                    query: "CQAbCdEfGhIj".into(),
                    peer_types: None,
                }
                .into(),
                tl::types::KeyboardButtonCopy {
                    style: None,
                    text: "Скопировать".into(),
                    copy_text: "https://t.me/send?start=CQAbCdEfGhIj".into(),
                }
                .into(),
            ],
            vec![
                tl::types::KeyboardButtonUrlAuth {
                    style: None,
                    text: "Войти".into(),
                    fwd_text: None,
                    url: "https://example.org/login".into(),
                    button_id: 1,
                }
                .into(),
                tl::types::KeyboardButtonCallback {
                    requires_password: true,
                    style: None,
                    text: "Подтвердить паролем".into(),
                    data: b"secret".to_vec(),
                }
                .into(),
                tl::types::KeyboardButtonBuy {
                    style: None,
                    text: "Купить".into(),
                }
                .into(),
            ],
        ]
    }

    #[test]
    fn every_button_kind_is_mapped() {
        let markup = inline_markup(all_buttons());
        let rows = buttons_of(Some(&markup));
        let kinds: Vec<Vec<(&str, &ButtonKind)>> = rows
            .iter()
            .map(|r| r.iter().map(|b| (b.text.as_str(), &b.kind)).collect())
            .collect();
        assert_eq!(
            kinds,
            vec![
                vec![
                    (
                        "Забрать",
                        &ButtonKind::Url("https://t.me/send?start=CQAbCdEfGhIj".into())
                    ),
                    ("USDT", &ButtonKind::Callback(b"asset:usdt".to_vec())),
                ],
                vec![
                    (
                        "Оплатить",
                        &ButtonKind::WebApp("https://app.send.tg/invoices/IVabc".into())
                    ),
                    (
                        "Кошелёк",
                        &ButtonKind::WebApp("https://app.send.tg/wallet".into())
                    ),
                ],
                vec![
                    (
                        "Поделиться",
                        &ButtonKind::SwitchInline("CQAbCdEfGhIj".into())
                    ),
                    (
                        "Скопировать",
                        &ButtonKind::CopyText("https://t.me/send?start=CQAbCdEfGhIj".into())
                    ),
                ],
                vec![
                    (
                        "Войти",
                        &ButtonKind::Url("https://example.org/login".into())
                    ),
                    ("Подтвердить паролем", &ButtonKind::Other),
                    ("Купить", &ButtonKind::Other),
                ],
            ]
        );
        assert_eq!(
            webapp_buttons(Some(&markup)),
            vec![
                (
                    "https://app.send.tg/invoices/IVabc".to_owned(),
                    WebAppKind::WebView
                ),
                (
                    "https://app.send.tg/wallet".to_owned(),
                    WebAppKind::SimpleWebView
                ),
            ]
        );
    }

    #[test]
    fn reply_keyboard_is_not_inline() {
        let markup: tl::enums::ReplyMarkup = tl::types::ReplyKeyboardMarkup {
            resize: true,
            single_use: false,
            selective: false,
            persistent: false,
            rows: vec![row(vec![
                tl::types::KeyboardButton {
                    style: None,
                    text: "Кошелёк".into(),
                }
                .into(),
            ])],
            placeholder: None,
        }
        .into();
        assert!(buttons_of(Some(&markup)).is_empty());
        assert!(buttons_of(None).is_empty());
    }

    #[test]
    fn utf16_offsets() {
        // «🦋» — 2 единицы UTF-16, кириллица — по 1.
        let text = "🦋 Чек: https://t.me/send?start=CQAbCdEfGhIj ок";
        let start = "🦋 Чек: ".encode_utf16().count();
        let len = "https://t.me/send?start=CQAbCdEfGhIj"
            .encode_utf16()
            .count();
        assert_eq!(
            utf16_slice(text, start as i32, len as i32).as_deref(),
            Some("https://t.me/send?start=CQAbCdEfGhIj")
        );
        assert_eq!(utf16_slice(text, 1, 1), None, "splits a surrogate pair");
        assert_eq!(utf16_slice(text, -1, 2), None);
        assert_eq!(utf16_slice(text, 0, 10_000), None);
    }

    #[test]
    fn entity_urls_in_order() {
        let text = "Ваш чек 🚀 https://t.me/xrocket?start=t_ABCDEFGHIJKLMNO и ещё";
        let off = "Ваш чек 🚀 ".encode_utf16().count() as i32;
        let len = "https://t.me/xrocket?start=t_ABCDEFGHIJKLMNO"
            .encode_utf16()
            .count() as i32;
        let entities: Vec<tl::enums::MessageEntity> = vec![
            tl::types::MessageEntityTextUrl {
                offset: 0,
                length: 3,
                url: "https://t.me/send?start=CQhidden0001".into(),
            }
            .into(),
            tl::types::MessageEntityBold {
                offset: 0,
                length: 3,
            }
            .into(),
            tl::types::MessageEntityUrl {
                offset: off,
                length: len,
            }
            .into(),
        ];
        assert_eq!(
            entity_urls(text, Some(&entities)),
            vec![
                "https://t.me/send?start=CQhidden0001".to_owned(),
                "https://t.me/xrocket?start=t_ABCDEFGHIJKLMNO".to_owned(),
            ]
        );
        assert!(entity_urls(text, None).is_empty());
    }

    #[test]
    fn chats_are_recognised_by_pinned_ids_only() {
        let c = ctx();
        assert_eq!(
            c.chat_of_peer(&peer_user(CB)),
            Some(Chat::WalletBot(Platform::CryptoBot))
        );
        assert_eq!(
            c.chat_of_peer(&peer_user(XR)),
            Some(Chat::WalletBot(Platform::XRocket))
        );
        assert_eq!(
            c.chat_of_peer(&peer_user(SELF_ID)),
            Some(Chat::SavedMessages)
        );
        assert_eq!(
            c.chat_of_peer(&peer_user(123)),
            None,
            "a person or a fake bot"
        );
        assert_eq!(
            c.chat_of_peer(&tl::types::PeerChannel { channel_id: CB }.into()),
            None
        );
        assert_eq!(
            c.chat_of_peer(&tl::types::PeerChat { chat_id: CB }.into()),
            None
        );
    }

    #[test]
    fn bot_reply_with_buttons() {
        let mut m = tl_message(41, CB, false, "Вы получили 10 USDT");
        m.reply_markup = Some(inline_markup(vec![vec![url_button(
            "Открыть",
            "https://t.me/send?start=x",
        )]]));
        let raw = message_to_raw(&m.into(), &ctx()).unwrap();
        assert_eq!(raw.chat, Chat::WalletBot(Platform::CryptoBot));
        assert_eq!(raw.id, 41);
        assert!(!raw.out);
        assert_eq!(raw.date, 1_760_000_000);
        assert_eq!(raw.edit_date, None);
        assert_eq!(raw.text, "Вы получили 10 USDT");
        assert_eq!(
            raw.button_urls(),
            vec!["https://t.me/send?start=x".to_owned()]
        );
        assert_eq!(raw.via_bot, None);
    }

    #[test]
    fn inline_check_in_saved_messages() {
        let mut m = tl_message(7, SELF_ID, true, "🚀 Чек на 10 USDT");
        m.via_bot_id = Some(XR);
        m.edit_date = Some(1_760_000_005);
        m.reply_markup = Some(inline_markup(vec![vec![url_button(
            "Получить",
            "https://t.me/xrocket?start=t_ABCDEFGHIJKLMNO",
        )]]));
        let raw = message_to_raw(&m.into(), &ctx()).unwrap();
        assert_eq!(raw.chat, Chat::SavedMessages);
        assert!(raw.out);
        assert_eq!(raw.via_bot, Some(Platform::XRocket));
        assert_eq!(raw.edit_date, Some(1_760_000_005));
        assert!(raw.find_button("получ").is_some());

        let mut fake = tl_message(8, SELF_ID, true, "🚀 Чек на 10 USDT");
        fake.via_bot_id = Some(999);
        let raw = message_to_raw(&fake.into(), &ctx()).unwrap();
        assert_eq!(raw.via_bot, None, "via a non-pinned bot");
    }

    #[test]
    fn foreign_empty_and_service_messages_are_dropped() {
        let c = ctx();
        assert!(message_to_raw(&tl_message(1, 123, false, "привет").into(), &c).is_none());
        let empty: tl::enums::Message = tl::types::MessageEmpty {
            id: 1,
            peer_id: Some(peer_user(CB)),
        }
        .into();
        assert!(message_to_raw(&empty, &c).is_none());
    }

    #[test]
    fn short_message_update_from_grammers_has_no_buttons() {
        // grammers превращает UpdateShortMessage в UpdateNewMessage без reply_markup.
        let update: tl::enums::Update = tl::types::UpdateNewMessage {
            message: tl_message(50, XR, false, "Чек не найден").into(),
            pts: 1,
            pts_count: 1,
        }
        .into();
        let raw = update_to_raw(&update, &ctx()).unwrap();
        assert_eq!(raw.chat, Chat::WalletBot(Platform::XRocket));
        assert!(raw.buttons.is_empty());

        let mut edited = tl_message(50, XR, false, "Вы получили 1 USDT");
        edited.edit_date = Some(1_760_000_009);
        let update: tl::enums::Update = tl::types::UpdateEditMessage {
            message: edited.into(),
            pts: 2,
            pts_count: 1,
        }
        .into();
        let raw = update_to_raw(&update, &ctx()).unwrap();
        assert_eq!(raw.edit_date, Some(1_760_000_009));
        assert!(update_message(&update).is_some());

        let other: tl::enums::Update = tl::types::UpdateMessageId {
            id: 1,
            random_id: 2,
        }
        .into();
        assert!(update_to_raw(&other, &ctx()).is_none());
    }

    fn updates_with(list: Vec<tl::enums::Update>) -> tl::enums::Updates {
        tl::types::Updates {
            updates: list,
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_760_000_100,
            seq: 0,
        }
        .into()
    }

    fn new_message(m: tl::types::Message) -> tl::enums::Update {
        tl::types::UpdateNewMessage {
            message: m.into(),
            pts: 10,
            pts_count: 1,
        }
        .into()
    }

    fn message_id(id: i32, random_id: i64) -> tl::enums::Update {
        tl::types::UpdateMessageId { id, random_id }.into()
    }

    /// Ответ проходит через сериализацию TL, как по сети.
    fn wire(updates: tl::enums::Updates) -> tl::enums::Updates {
        tl::enums::Updates::from_bytes(&updates.to_bytes()).unwrap()
    }

    #[test]
    fn sent_message_is_found_by_random_id() {
        let mut inline = tl_message(90, SELF_ID, true, "🦋 Чек на 10 USDT");
        inline.via_bot_id = Some(CB);
        inline.reply_markup = Some(inline_markup(vec![vec![url_button(
            "Получить 10 USDT",
            "https://t.me/send?start=CQAbCdEfGhIj",
        )]]));
        let other = tl_message(91, SELF_ID, true, "чужое");
        let updates = wire(updates_with(vec![
            message_id(91, 111),
            message_id(90, 4242),
            new_message(other),
            new_message(inline),
        ]));
        match find_sent_message(&updates, 4242, Chat::SavedMessages, &ctx()) {
            SentMessage::Message(m) => {
                assert_eq!(m.id, 90);
                assert_eq!(m.via_bot, Some(Platform::CryptoBot));
                assert_eq!(
                    m.button_urls(),
                    vec!["https://t.me/send?start=CQAbCdEfGhIj".to_owned()]
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            find_sent_message(&updates, 999, Chat::SavedMessages, &ctx()),
            SentMessage::NotFound,
            "a different random_id is never mistaken for ours"
        );
    }

    #[test]
    fn sent_message_id_without_body() {
        let updates = wire(updates_with(vec![message_id(5, 77)]));
        assert_eq!(
            find_sent_message(&updates, 77, Chat::WalletBot(Platform::XRocket), &ctx()),
            SentMessage::IdOnly {
                id: 5,
                date: 1_760_000_100
            }
        );
    }

    #[test]
    fn short_sent_message() {
        let updates: tl::enums::Updates = tl::types::UpdateShortSentMessage {
            out: true,
            id: 321,
            pts: 1,
            pts_count: 1,
            date: 1_760_000_200,
            media: None,
            entities: None,
            ttl_period: None,
        }
        .into();
        let found = find_sent_message(
            &wire(updates),
            1,
            Chat::WalletBot(Platform::CryptoBot),
            &ctx(),
        );
        assert_eq!(
            found,
            SentMessage::Short {
                id: 321,
                date: 1_760_000_200,
                entities: None
            }
        );
        let raw = sent_text_message(
            Chat::WalletBot(Platform::CryptoBot),
            321,
            1_760_000_200,
            "/start CQAbCdEfGhIj",
            None,
        );
        assert!(raw.out);
        assert_eq!(raw.text, "/start CQAbCdEfGhIj");
    }

    #[test]
    fn fallback_to_single_outgoing_message() {
        let mine = tl_message(12, CB, true, "/start CQAbCdEfGhIj");
        let reply = tl_message(13, CB, false, "Подождите");
        let updates = wire(updates_with(vec![
            new_message(mine.clone()),
            new_message(reply),
        ]));
        let found = find_sent_message(&updates, 5, Chat::WalletBot(Platform::CryptoBot), &ctx());
        assert!(matches!(found, SentMessage::Message(m) if m.id == 12));

        let two = wire(updates_with(vec![
            new_message(mine),
            new_message(tl_message(14, CB, true, "/start x")),
        ]));
        assert_eq!(
            find_sent_message(&two, 5, Chat::WalletBot(Platform::CryptoBot), &ctx()),
            SentMessage::NotFound,
            "ambiguous"
        );
        let short_update: tl::enums::Updates = tl::types::UpdateShort {
            update: message_id(33, 5),
            date: 1,
        }
        .into();
        assert_eq!(
            find_sent_message(&short_update, 5, Chat::SavedMessages, &ctx()),
            SentMessage::IdOnly { id: 33, date: 1 }
        );
        assert_eq!(
            find_sent_message(&tl::enums::Updates::TooLong, 5, Chat::SavedMessages, &ctx()),
            SentMessage::NotFound
        );
    }

    #[test]
    fn inline_results_and_answers() {
        let send_message: tl::enums::BotInlineMessage = tl::types::BotInlineMessageText {
            no_webpage: false,
            invert_media: false,
            message: "Чек на 10 USDT".into(),
            entities: None,
            reply_markup: None,
        }
        .into();
        let results: tl::enums::messages::BotResults = tl::types::messages::BotResults {
            gallery: false,
            query_id: 987_654_321,
            next_offset: None,
            switch_pm: None,
            switch_webview: None,
            results: vec![
                tl::types::BotInlineResult {
                    id: "usdt".into(),
                    r#type: "article".into(),
                    title: Some("Отправить 10 USDT".into()),
                    description: Some("Чек".into()),
                    url: None,
                    thumb: None,
                    content: None,
                    send_message: send_message.clone(),
                }
                .into(),
                tl::types::BotInlineMediaResult {
                    id: "ton".into(),
                    r#type: "photo".into(),
                    photo: None,
                    document: None,
                    title: None,
                    description: None,
                    send_message,
                }
                .into(),
            ],
            cache_time: 0,
            users: Vec::new(),
        }
        .into();
        let results = tl::enums::messages::BotResults::from_bytes(&results.to_bytes()).unwrap();
        let mapped = inline_results(&results);
        assert_eq!(mapped.query_id, 987_654_321);
        assert_eq!(
            mapped.results,
            vec![
                InlineResult {
                    id: "usdt".into(),
                    title: Some("Отправить 10 USDT".into()),
                    description: Some("Чек".into())
                },
                InlineResult {
                    id: "ton".into(),
                    title: None,
                    description: None
                },
            ]
        );

        let answer: tl::enums::messages::BotCallbackAnswer =
            tl::types::messages::BotCallbackAnswer {
                alert: true,
                has_url: false,
                native_ui: false,
                message: Some("Недостаточно средств".into()),
                url: None,
                cache_time: 0,
            }
            .into();
        assert_eq!(
            callback_answer(&answer),
            CallbackAnswer {
                message: Some("Недостаточно средств".into()),
                alert: true,
                url: None
            }
        );
        let web: tl::enums::WebViewResult = tl::types::WebViewResultUrl {
            fullsize: false,
            fullscreen: false,
            same_origin: false,
            query_id: Some(1),
            url: "https://app.send.tg/#tgWebAppData=x".into(),
        }
        .into();
        assert_eq!(webview_url(&web), "https://app.send.tg/#tgWebAppData=x");
    }

    #[test]
    fn history_page_is_ascending_and_filtered() {
        let c = ctx();
        let msgs: Vec<tl::enums::Message> = vec![
            tl_message(30, CB, false, "c").into(),
            tl_message(25, CB, true, "b").into(),
            tl_message(20, CB, false, "a").into(),
            tl_message(26, XR, false, "other chat").into(),
            tl_message(10, CB, false, "old").into(),
            tl_message(30, CB, false, "c dup").into(),
        ];
        let page = history_page(&msgs, Chat::WalletBot(Platform::CryptoBot), 15, 10, &c);
        assert_eq!(
            page.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![20, 25, 30]
        );
        let page = history_page(&msgs, Chat::WalletBot(Platform::CryptoBot), 15, 2, &c);
        assert_eq!(page.iter().map(|m| m.id).collect::<Vec<_>>(), vec![20, 25]);

        let wrapped: tl::enums::messages::Messages = tl::types::messages::MessagesSlice {
            inexact: false,
            count: 3,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: msgs.clone(),
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }
        .into();
        assert_eq!(messages_of(wrapped).len(), msgs.len());
        let not_modified: tl::enums::messages::Messages =
            tl::types::messages::MessagesNotModified { count: 1 }.into();
        assert!(messages_of(not_modified).is_empty());
    }

    pub(crate) fn tl_user(id: i64, bot: bool, username: Option<&str>) -> tl::types::User {
        tl::types::User {
            is_self: false,
            contact: false,
            mutual_contact: false,
            deleted: false,
            bot,
            bot_chat_history: false,
            bot_nochats: false,
            verified: true,
            restricted: false,
            min: false,
            bot_inline_geo: false,
            support: false,
            scam: false,
            apply_min_photo: false,
            fake: false,
            bot_attach_menu: false,
            premium: false,
            attach_menu_enabled: false,
            bot_can_edit: false,
            close_friend: false,
            stories_hidden: false,
            stories_unavailable: false,
            contact_require_premium: false,
            bot_business: false,
            bot_has_main_app: false,
            bot_forum_view: false,
            bot_forum_can_manage_topics: false,
            bot_can_manage_bots: false,
            bot_guestchat: false,
            bot_guard: false,
            id,
            access_hash: Some(id ^ 0x55),
            first_name: Some("Bot".into()),
            last_name: None,
            username: username.map(str::to_owned),
            phone: None,
            photo: None,
            status: None,
            bot_info_version: bot.then_some(1),
            restriction_reason: None,
            bot_inline_placeholder: None,
            lang_code: None,
            emoji_status: None,
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            bot_active_users: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
        }
    }

    #[test]
    fn bot_identity_is_verified() {
        let pinned = PinnedBot::default_for(Platform::CryptoBot);
        let ok: tl::enums::User = tl_user(CB, true, Some("send")).into();
        assert_eq!(verify_bot_user(&ok, &pinned), Ok(CB ^ 0x55));

        let upper: tl::enums::User = tl_user(CB, true, Some("SEND")).into();
        assert!(verify_bot_user(&upper, &pinned).is_ok());

        // Основной username другой, но `send` — активный дополнительный.
        let mut alias = tl_user(CB, true, Some("CryptoBot"));
        alias.usernames = Some(vec![
            tl::types::Username {
                editable: false,
                active: true,
                username: "send".into(),
            }
            .into(),
        ]);
        assert!(verify_bot_user(&alias.into(), &pinned).is_ok());

        let mut inactive = tl_user(CB, true, Some("CryptoBot"));
        inactive.usernames = Some(vec![
            tl::types::Username {
                editable: false,
                active: false,
                username: "send".into(),
            }
            .into(),
        ]);
        assert!(matches!(
            verify_bot_user(&inactive.into(), &pinned),
            Err(BotIdentityError::UsernameMismatch { .. })
        ));

        assert!(matches!(
            verify_bot_user(&tl_user(XR, true, Some("send")).into(), &pinned),
            Err(BotIdentityError::IdMismatch { got: XR, .. })
        ));
        assert!(matches!(
            verify_bot_user(&tl_user(CB, false, Some("send")).into(), &pinned),
            Err(BotIdentityError::NotABot { .. })
        ));
        assert!(matches!(
            verify_bot_user(&tl_user(CB, true, Some("send_fake_bot")).into(), &pinned),
            Err(BotIdentityError::UsernameMismatch { .. })
        ));
        let mut no_hash = tl_user(CB, true, Some("send"));
        no_hash.access_hash = None;
        assert!(matches!(
            verify_bot_user(&no_hash.into(), &pinned),
            Err(BotIdentityError::NoAccessHash { .. })
        ));
        let empty: tl::enums::User = tl::types::UserEmpty { id: CB }.into();
        assert!(matches!(
            verify_bot_user(&empty, &pinned),
            Err(BotIdentityError::NotFound { .. })
        ));
    }
}
