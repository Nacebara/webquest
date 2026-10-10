//! Сырые запросы TL слоя 227. Только чистые построители: какие флаги и поля уходят на сервер,
//! проверяется тестами офлайн.
//!
//! Высокоуровневые методы grammers (`send_message`, `InlineResult::send`) не используются: они
//! генерируют свой `random_id`, и идемпотентный повтор становится невозможен (LOVEC-PORTING §3.2).

use domain::Platform;
use grammers_tl_types as tl;
use url::Url;
use userbot::transport::Chat;

use crate::convert::WebAppKind;

/// Предел Telegram на одну страницу истории.
pub const MAX_HISTORY_LIMIT: u32 = 100;

/// Бот кошелька после сверки при старте: закреплённый id и access hash этой сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotPeer {
    pub platform: Platform,
    pub id: i64,
    pub access_hash: i64,
}

impl BotPeer {
    pub fn input_peer(&self) -> tl::enums::InputPeer {
        tl::types::InputPeerUser {
            user_id: self.id,
            access_hash: self.access_hash,
        }
        .into()
    }

    pub fn input_user(&self) -> tl::enums::InputUser {
        tl::types::InputUser {
            user_id: self.id,
            access_hash: self.access_hash,
        }
        .into()
    }
}

/// Оба бота кошельков.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotPeers {
    pub cryptobot: BotPeer,
    pub xrocket: BotPeer,
}

impl BotPeers {
    pub fn get(&self, platform: Platform) -> &BotPeer {
        match platform {
            Platform::CryptoBot => &self.cryptobot,
            Platform::XRocket => &self.xrocket,
        }
    }

    /// Peer чата: личка с ботом по закреплённому id или «Избранное» (`inputPeerSelf`).
    pub fn input_peer(&self, chat: Chat) -> tl::enums::InputPeer {
        match chat {
            Chat::WalletBot(p) => self.get(p).input_peer(),
            Chat::SavedMessages => tl::enums::InputPeer::PeerSelf,
        }
    }
}

/// `messages.sendMessage` с заданным `random_id`, без превью ссылок (как `/start` в lovec).
pub fn send_message(
    peer: tl::enums::InputPeer,
    text: &str,
    random_id: i64,
) -> tl::functions::messages::SendMessage {
    tl::functions::messages::SendMessage {
        no_webpage: true,
        silent: false,
        background: false,
        clear_draft: false,
        noforwards: false,
        update_stickersets_order: false,
        invert_media: false,
        allow_paid_floodskip: false,
        peer,
        reply_to: None,
        message: text.to_owned(),
        random_id,
        reply_markup: None,
        entities: None,
        schedule_date: None,
        schedule_repeat_period: None,
        send_as: None,
        quick_reply_shortcut: None,
        effect: None,
        allow_paid_stars: None,
        suggested_post: None,
        rich_message: None,
    }
}

/// `messages.getInlineBotResults`: запрос от имени себя (`peer = inputPeerSelf`, «Избранное»).
pub fn inline_query(
    bot: tl::enums::InputUser,
    query: &str,
) -> tl::functions::messages::GetInlineBotResults {
    tl::functions::messages::GetInlineBotResults {
        bot,
        peer: tl::enums::InputPeer::PeerSelf,
        geo_point: None,
        query: query.to_owned(),
        offset: String::new(),
    }
}

/// `messages.sendInlineBotResult` с заданным `random_id`; подпись «via @bot» не скрываем —
/// по ней сообщение-чек находится в «Избранном».
pub fn send_inline_result(
    peer: tl::enums::InputPeer,
    query_id: i64,
    result_id: &str,
    random_id: i64,
) -> tl::functions::messages::SendInlineBotResult {
    tl::functions::messages::SendInlineBotResult {
        silent: false,
        background: false,
        clear_draft: false,
        hide_via: false,
        peer,
        reply_to: None,
        random_id,
        query_id,
        id: result_id.to_owned(),
        schedule_date: None,
        send_as: None,
        quick_reply_shortcut: None,
        allow_paid_stars: None,
    }
}

/// `messages.getBotCallbackAnswer` без пароля (кнопки с `requires_password` не нажимаем).
pub fn press_callback(
    peer: tl::enums::InputPeer,
    msg_id: i32,
    data: &[u8],
) -> tl::functions::messages::GetBotCallbackAnswer {
    tl::functions::messages::GetBotCallbackAnswer {
        game: false,
        peer,
        msg_id,
        data: Some(data.to_vec()),
        password: None,
    }
}

/// `messages.getHistory`: сообщения с id > `after_id`, начиная с самых старых.
///
/// `offset_id = after_id + 1` и `add_offset = -limit` дают окно из `limit` сообщений сразу
/// после `after_id` (схема Telegram «offsets», как обратный обход в Telethon); `min_id`
/// дополнительно отсекает старые. Ответ всё равно фильтруется и сортируется на нашей стороне.
pub fn get_history(
    peer: tl::enums::InputPeer,
    after_id: i32,
    limit: u32,
) -> tl::functions::messages::GetHistory {
    let limit = i32::try_from(limit.clamp(1, MAX_HISTORY_LIMIT)).unwrap_or(1);
    let after_id = after_id.max(0);
    tl::functions::messages::GetHistory {
        peer,
        offset_id: after_id.saturating_add(1),
        offset_date: 0,
        add_offset: -limit,
        limit,
        max_id: 0,
        min_id: after_id,
        hash: 0,
    }
}

/// `messages.getHistory` без смещений: последние `limit` сообщений чата (сервер отдаёт их от
/// новых к старым, порядок выравнивает [`crate::convert::history_page`]).
pub fn get_recent(peer: tl::enums::InputPeer, limit: u32) -> tl::functions::messages::GetHistory {
    let limit = i32::try_from(limit.clamp(1, MAX_HISTORY_LIMIT)).unwrap_or(1);
    tl::functions::messages::GetHistory {
        peer,
        offset_id: 0,
        offset_date: 0,
        add_offset: 0,
        limit,
        max_id: 0,
        min_id: 0,
        hash: 0,
    }
}

/// `messages.getMessages` по id (личные чаты и «Избранное»).
pub fn get_messages(ids: &[i32]) -> tl::functions::messages::GetMessages {
    tl::functions::messages::GetMessages {
        id: ids
            .iter()
            .map(|&id| tl::types::InputMessageId { id }.into())
            .collect(),
    }
}

/// Куда ведёт кнопка мини-приложения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebAppTarget {
    /// `keyboardButtonWebView` (или тип неизвестен) → `messages.requestWebView`.
    WebView { url: String },
    /// `keyboardButtonSimpleWebView` → `messages.requestSimpleWebView`.
    Simple { url: String },
    /// Ссылка `t.me/<бот>/<приложение>?startapp=…` → `messages.requestAppWebView`.
    App {
        short_name: String,
        start_param: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebAppUrlError {
    #[error("not a valid mini app URL")]
    Invalid,
    /// Ссылка на мини-приложение другого бота: не открываем (CLAUDE.md п. 10).
    #[error("mini app link points to a bot other than the pinned wallet bot")]
    ForeignBot,
    #[error("unsupported mini app link form")]
    Unsupported,
}

const TME_HOSTS: [&str; 3] = ["t.me", "telegram.me", "telegram.dog"];

/// Выбрать метод открытия мини-приложения по URL кнопки и её запомненному типу.
pub fn webapp_target(
    url: &str,
    known: Option<WebAppKind>,
    bot_username: &str,
) -> Result<WebAppTarget, WebAppUrlError> {
    let parsed = Url::parse(url).map_err(|_| WebAppUrlError::Invalid)?;
    if !matches!(parsed.scheme(), "https" | "http") {
        return Err(WebAppUrlError::Invalid);
    }
    let host = parsed
        .host_str()
        .ok_or(WebAppUrlError::Invalid)?
        .to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if TME_HOSTS.contains(&host) {
        let segments: Vec<&str> = parsed
            .path_segments()
            .map(|s| s.filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();
        let [bot, app] = segments.as_slice() else {
            return Err(WebAppUrlError::Unsupported);
        };
        if !bot.eq_ignore_ascii_case(bot_username.trim_start_matches('@')) {
            return Err(WebAppUrlError::ForeignBot);
        }
        let start_param = parsed
            .query_pairs()
            .find(|(k, _)| k == "startapp")
            .map(|(_, v)| v.into_owned());
        return Ok(WebAppTarget::App {
            short_name: (*app).to_owned(),
            start_param,
        });
    }
    Ok(match known {
        Some(WebAppKind::SimpleWebView) => WebAppTarget::Simple {
            url: url.to_owned(),
        },
        Some(WebAppKind::WebView) | None => WebAppTarget::WebView {
            url: url.to_owned(),
        },
    })
}

pub fn request_webview(
    bot: &BotPeer,
    url: &str,
    platform: &str,
) -> tl::functions::messages::RequestWebView {
    tl::functions::messages::RequestWebView {
        from_bot_menu: false,
        silent: false,
        compact: false,
        fullscreen: false,
        peer: bot.input_peer(),
        bot: bot.input_user(),
        url: Some(url.to_owned()),
        start_param: None,
        theme_params: None,
        platform: platform.to_owned(),
        reply_to: None,
        send_as: None,
    }
}

pub fn request_simple_webview(
    bot: &BotPeer,
    url: &str,
    platform: &str,
) -> tl::functions::messages::RequestSimpleWebView {
    tl::functions::messages::RequestSimpleWebView {
        from_switch_webview: false,
        from_side_menu: false,
        compact: false,
        fullscreen: false,
        bot: bot.input_user(),
        url: Some(url.to_owned()),
        start_param: None,
        theme_params: None,
        platform: platform.to_owned(),
    }
}

pub fn request_app_webview(
    bot: &BotPeer,
    short_name: &str,
    start_param: Option<&str>,
    platform: &str,
) -> tl::functions::messages::RequestAppWebView {
    tl::functions::messages::RequestAppWebView {
        write_allowed: false,
        compact: false,
        fullscreen: false,
        peer: bot.input_peer(),
        app: tl::types::InputBotAppShortName {
            bot_id: bot.input_user(),
            short_name: short_name.to_owned(),
        }
        .into(),
        start_param: start_param.map(str::to_owned),
        theme_params: None,
        platform: platform.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tl::{Deserializable, Identifiable, Serializable};

    fn bots() -> BotPeers {
        BotPeers {
            cryptobot: BotPeer {
                platform: Platform::CryptoBot,
                id: 1_559_501_630,
                access_hash: 11,
            },
            xrocket: BotPeer {
                platform: Platform::XRocket,
                id: 5_014_831_088,
                access_hash: 22,
            },
        }
    }

    #[test]
    fn peers_for_chats() {
        let b = bots();
        assert_eq!(
            b.input_peer(Chat::WalletBot(Platform::CryptoBot)),
            tl::types::InputPeerUser {
                user_id: 1_559_501_630,
                access_hash: 11
            }
            .into()
        );
        assert_eq!(
            b.input_peer(Chat::WalletBot(Platform::XRocket)),
            tl::types::InputPeerUser {
                user_id: 5_014_831_088,
                access_hash: 22
            }
            .into()
        );
        assert_eq!(
            b.input_peer(Chat::SavedMessages),
            tl::enums::InputPeer::PeerSelf
        );
    }

    #[test]
    fn send_message_carries_the_given_random_id() {
        let peer = bots().input_peer(Chat::WalletBot(Platform::CryptoBot));
        let req = send_message(peer.clone(), "/start CQAbCdEfGhIj", -7_331_234_567_890);
        assert!(req.no_webpage);
        assert_eq!(req.random_id, -7_331_234_567_890);
        assert_eq!(req.message, "/start CQAbCdEfGhIj");
        assert!(req.reply_markup.is_none() && req.entities.is_none() && req.send_as.is_none());

        // Повтор с тем же random_id — побайтно тот же запрос (сервер вернёт прежний результат
        // или RANDOM_ID_DUPLICATE), с другим — другой.
        let bytes = req.to_bytes();
        assert_eq!(
            bytes,
            send_message(peer.clone(), "/start CQAbCdEfGhIj", -7_331_234_567_890).to_bytes()
        );
        assert_ne!(
            bytes,
            send_message(peer, "/start CQAbCdEfGhIj", 1).to_bytes()
        );
        assert_eq!(
            u32::from_le_bytes(bytes[..4].try_into().unwrap()),
            tl::functions::messages::SendMessage::CONSTRUCTOR_ID
        );
        // flags: только no_webpage (бит 1).
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 1 << 1);
        // random_id стоит в теле как есть (little-endian i64).
        assert!(
            bytes
                .windows(8)
                .any(|w| w == (-7_331_234_567_890_i64).to_le_bytes())
        );
    }

    #[test]
    fn inline_requests_use_saved_messages_and_random_id() {
        let b = bots();
        let q = inline_query(b.xrocket.input_user(), "10");
        assert_eq!(q.peer, tl::enums::InputPeer::PeerSelf);
        assert_eq!(q.query, "10");
        assert_eq!(q.offset, "");

        let send = send_inline_result(b.input_peer(Chat::SavedMessages), 555, "usdt", 99);
        assert_eq!(send.peer, tl::enums::InputPeer::PeerSelf);
        assert_eq!(
            (send.query_id, send.id.as_str(), send.random_id),
            (555, "usdt", 99)
        );
        assert!(!send.hide_via);
    }

    #[test]
    fn callback_request() {
        let req = press_callback(bots().cryptobot.input_peer(), 42, b"asset:usdt");
        assert_eq!(req.msg_id, 42);
        assert_eq!(req.data.as_deref(), Some(b"asset:usdt".as_slice()));
        assert!(req.password.is_none() && !req.game);
    }

    #[test]
    fn history_window_after_id() {
        let req = get_history(tl::enums::InputPeer::PeerSelf, 100, 20);
        assert_eq!(
            (req.offset_id, req.add_offset, req.limit, req.min_id),
            (101, -20, 20, 100)
        );
        let req = get_history(tl::enums::InputPeer::PeerSelf, 0, 1000);
        assert_eq!(
            (req.offset_id, req.add_offset, req.limit, req.min_id),
            (1, -100, 100, 0)
        );
        let req = get_history(tl::enums::InputPeer::PeerSelf, i32::MAX, 0);
        assert_eq!((req.offset_id, req.limit), (i32::MAX, 1));
        let req = get_recent(tl::enums::InputPeer::PeerSelf, 30);
        assert_eq!(
            (
                req.offset_id,
                req.add_offset,
                req.limit,
                req.min_id,
                req.max_id
            ),
            (0, 0, 30, 0, 0)
        );
        let req = get_recent(tl::enums::InputPeer::PeerSelf, 5000);
        assert_eq!(req.limit, 100);
        let req = get_messages(&[5, 6]);
        assert_eq!(req.id.len(), 2);
    }

    #[test]
    fn requests_survive_the_wire() {
        /// Так тело запроса увидит сервер: id конструктора, затем поля (функции
        /// десериализуемы в dev-сборке, без id конструктора).
        fn wire<F: Serializable + Deserializable + Identifiable>(f: &F) -> F {
            let bytes = f.to_bytes();
            assert_eq!(
                u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                F::CONSTRUCTOR_ID
            );
            F::from_bytes(&bytes[4..]).unwrap()
        }
        let history = get_history(bots().cryptobot.input_peer(), 7, 3);
        assert_eq!(wire(&history), history);
        let send = send_inline_result(tl::enums::InputPeer::PeerSelf, 1, "r", 2);
        let back = wire(&send);
        assert_eq!(back, send);
        assert_eq!(back.random_id, 2);
        assert_eq!(back.peer, tl::enums::InputPeer::PeerSelf);
        let msg = send_message(tl::enums::InputPeer::PeerSelf, "/checks", 3);
        let back = wire(&msg);
        assert!(back.no_webpage);
        assert_eq!((back.random_id, back.message.as_str()), (3, "/checks"));
        let press = press_callback(bots().xrocket.input_peer(), 9, b"x");
        assert_eq!(wire(&press), press);
    }

    #[test]
    fn webapp_targets() {
        assert_eq!(
            webapp_target("https://app.send.tg/invoice?x=1", None, "send"),
            Ok(WebAppTarget::WebView {
                url: "https://app.send.tg/invoice?x=1".into()
            })
        );
        assert_eq!(
            webapp_target(
                "https://app.send.tg/wallet",
                Some(WebAppKind::SimpleWebView),
                "send"
            ),
            Ok(WebAppTarget::Simple {
                url: "https://app.send.tg/wallet".into()
            })
        );
        assert_eq!(
            webapp_target("https://t.me/send/app?startapp=IVabc", None, "send"),
            Ok(WebAppTarget::App {
                short_name: "app".into(),
                start_param: Some("IVabc".into())
            })
        );
        assert_eq!(
            webapp_target("https://www.T.me/Send/wallet", None, "@send"),
            Ok(WebAppTarget::App {
                short_name: "wallet".into(),
                start_param: None
            })
        );
        assert_eq!(
            webapp_target("https://t.me/send_scam_bot/app?startapp=x", None, "send"),
            Err(WebAppUrlError::ForeignBot)
        );
        assert_eq!(
            webapp_target("https://t.me/send?startapp=x", None, "send"),
            Err(WebAppUrlError::Unsupported)
        );
        assert_eq!(
            webapp_target("tg://resolve?domain=send", None, "send"),
            Err(WebAppUrlError::Invalid)
        );
        assert_eq!(
            webapp_target("not a url", None, "send"),
            Err(WebAppUrlError::Invalid)
        );
    }

    #[test]
    fn webview_requests() {
        let b = bots();
        let r = request_webview(&b.cryptobot, "https://app.send.tg/x", "tdesktop");
        assert_eq!(r.peer, b.cryptobot.input_peer());
        assert_eq!(r.bot, b.cryptobot.input_user());
        assert_eq!(r.url.as_deref(), Some("https://app.send.tg/x"));
        assert_eq!(r.platform, "tdesktop");
        let s = request_simple_webview(&b.cryptobot, "https://app.send.tg/y", "android");
        assert_eq!(s.url.as_deref(), Some("https://app.send.tg/y"));
        let a = request_app_webview(&b.xrocket, "wallet", Some("inv_x"), "tdesktop");
        assert_eq!(a.start_param.as_deref(), Some("inv_x"));
        assert!(!a.write_allowed);
    }
}
