//! Сырые запросы TL слоя 227. Только чистые построители: какие флаги и поля уходят на сервер,
//! проверяется тестами офлайн.
//!
//! Высокоуровневые методы grammers (`send_message`) не используются: они
//! генерируют свой `random_id`, и идемпотентный повтор становится невозможен (LOVEC-PORTING §3.2).

use domain::Platform;
use grammers_tl_types as tl;
use userbot::transport::Chat;

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
        let msg = send_message(tl::enums::InputPeer::PeerSelf, "/checks", 3);
        let back = wire(&msg);
        assert!(back.no_webpage);
        assert_eq!((back.random_id, back.message.as_str()), (3, "/checks"));
        let press = press_callback(bots().xrocket.input_peer(), 9, b"x");
        assert_eq!(wire(&press), press);
    }
}
