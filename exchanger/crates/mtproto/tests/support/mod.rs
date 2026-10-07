//! Общие помощники интеграционных тестов `mtproto`.
// Тестовая обвязка: паника = провал теста.
#![allow(dead_code, clippy::unwrap_used)]

pub mod fake_tg;

use std::sync::Arc;
use std::time::Duration;

use grammers_tl_types as tl;
use mtproto::session::{MemoryStore, SessionSnapshot, StoredUser};
use mtproto::{MtprotoConfig, SessionKey, seal};
use secrecy::SecretString;

use fake_tg::{FakeTelegram, Reply, Request};

pub const ACCOUNT: &str = "ub-test";
pub const SELF_ID: i64 = 777_000;
pub const CB: i64 = 1_559_501_630;
pub const XR: i64 = 5_014_831_088;
pub const CB_HASH: i64 = 1_111;
pub const XR_HASH: i64 = 2_222;
/// pts, который сервер отдаёт в `updates.getState`.
pub const START_PTS: i32 = 100;

pub fn now() -> i32 {
    i32::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

/// Конфиг с короткими таймаутами для петли 127.0.0.1.
pub fn config() -> MtprotoConfig {
    let mut cfg = MtprotoConfig::new(ACCOUNT, 4242, SecretString::from("api-hash"));
    cfg.timeouts.action = Duration::from_millis(800);
    cfg.timeouts.read = Duration::from_secs(2);
    cfg.timeouts.startup = Duration::from_secs(5);
    cfg.timeouts.reconnect_wait = Duration::from_secs(5);
    cfg.timeouts.ping_every = Duration::from_millis(300);
    cfg.timeouts.ping_timeout = Duration::from_millis(500);
    cfg.timeouts.update_state_sync_every = Duration::from_millis(200);
    cfg
}

pub fn key() -> SessionKey {
    SessionKey::from_bytes(1, &[7; 32])
}

/// Сессия без входа, указывающая на поддельный сервер (auth key есть, пользователя нет) —
/// как после создания ключа перед `exch login`.
pub fn store_without_login(fake: &FakeTelegram) -> Arc<MemoryStore> {
    let mut snap = SessionSnapshot::default();
    let home = snap.home_dc;
    let dc = snap.dc_options.get_mut(&home).unwrap();
    dc.ipv4 = fake.addr;
    dc.auth_key = Some(fake.auth_key());
    let sealed = seal(&key(), ACCOUNT, &snap.encode().unwrap()).unwrap();
    Arc::new(MemoryStore::new(Some(sealed)))
}

/// Сессия, указывающая на поддельный сервер: auth key, свой аккаунт, при желании — боты.
pub fn store_for(fake: &FakeTelegram, with_bot_hashes: bool) -> Arc<MemoryStore> {
    let mut snap = SessionSnapshot::default();
    let home = snap.home_dc;
    let dc = snap.dc_options.get_mut(&home).unwrap();
    dc.ipv4 = fake.addr;
    dc.auth_key = Some(fake.auth_key());
    snap.users.insert(
        SELF_ID,
        StoredUser {
            id: SELF_ID,
            access_hash: Some(5),
            bot: Some(false),
            is_self: Some(true),
        },
    );
    if with_bot_hashes {
        for (id, hash) in [(CB, CB_HASH), (XR, XR_HASH)] {
            snap.users.insert(
                id,
                StoredUser {
                    id,
                    access_hash: Some(hash),
                    bot: Some(true),
                    is_self: None,
                },
            );
        }
    }
    let sealed = seal(&key(), ACCOUNT, &snap.encode().unwrap()).unwrap();
    Arc::new(MemoryStore::new(Some(sealed)))
}

pub fn user(id: i64, access_hash: i64, bot: bool, username: Option<&str>) -> tl::enums::User {
    tl::types::User {
        is_self: id == SELF_ID,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot,
        bot_chat_history: false,
        bot_nochats: false,
        verified: bot,
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
        access_hash: Some(access_hash),
        first_name: Some("Test".into()),
        last_name: None,
        username: username.map(str::to_owned),
        phone: None,
        photo: None,
        status: None,
        // У бота флаг `bot` и `bot_info_version` — один бит (flags.14): версия обязательна.
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
    .into()
}

pub fn peer_user(id: i64) -> tl::enums::Peer {
    tl::types::PeerUser { user_id: id }.into()
}

pub fn message(id: i32, peer: i64, out: bool, text: &str) -> tl::types::Message {
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
        date: now(),
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

pub fn inline_markup(rows: Vec<Vec<tl::enums::KeyboardButton>>) -> tl::enums::ReplyMarkup {
    tl::types::ReplyInlineMarkup {
        rows: rows
            .into_iter()
            .map(|buttons| tl::types::KeyboardButtonRow { buttons }.into())
            .collect(),
    }
    .into()
}

pub fn url_button(text: &str, url: &str) -> tl::enums::KeyboardButton {
    tl::types::KeyboardButtonUrl {
        style: None,
        text: text.into(),
        url: url.into(),
    }
    .into()
}

/// `Updates` с одним апдейтом, как пуш сервера.
pub fn updates(list: Vec<tl::enums::Update>) -> tl::enums::Updates {
    tl::types::Updates {
        updates: list,
        users: Vec::new(),
        chats: Vec::new(),
        date: now(),
        seq: 0,
    }
    .into()
}

pub fn new_message(m: tl::types::Message, pts: i32) -> tl::enums::Update {
    tl::types::UpdateNewMessage {
        message: m.into(),
        pts,
        pts_count: 1,
    }
    .into()
}

pub fn edit_message(m: tl::types::Message, pts: i32) -> tl::enums::Update {
    tl::types::UpdateEditMessage {
        message: m.into(),
        pts,
        pts_count: 1,
    }
    .into()
}

/// Ответы на служебные запросы подключения: `getUsers` (свой аккаунт и боты),
/// `resolveUsername`, `updates.getState`, `updates.getDifference`.
pub fn bootstrap(req: &Request) -> Option<Reply> {
    if let Some(r) = req.parse::<tl::functions::users::GetUsers>() {
        let users: Vec<tl::enums::User> = r
            .id
            .iter()
            .map(|u| match u {
                tl::enums::InputUser::UserSelf => user(SELF_ID, 5, false, Some("exch_test")),
                tl::enums::InputUser::User(u) if u.user_id == CB && u.access_hash == CB_HASH => {
                    user(CB, CB_HASH, true, Some("send"))
                }
                tl::enums::InputUser::User(u) if u.user_id == XR && u.access_hash == XR_HASH => {
                    user(XR, XR_HASH, true, Some("xrocket"))
                }
                other => tl::types::UserEmpty {
                    id: match other {
                        tl::enums::InputUser::User(u) => u.user_id,
                        _ => 0,
                    },
                }
                .into(),
            })
            .collect();
        return Some(Reply::ok(&users));
    }
    if let Some(r) = req.parse::<tl::functions::contacts::ResolveUsername>() {
        let (id, hash) = match r.username.as_str() {
            "send" => (CB, CB_HASH),
            "xrocket" => (XR, XR_HASH),
            _ => return Some(Reply::error(400, "USERNAME_NOT_OCCUPIED")),
        };
        let resolved: tl::enums::contacts::ResolvedPeer = tl::types::contacts::ResolvedPeer {
            peer: peer_user(id),
            chats: Vec::new(),
            users: vec![user(id, hash, true, Some(&r.username))],
        }
        .into();
        return Some(Reply::ok(&resolved));
    }
    if req.is::<tl::functions::updates::GetState>() {
        let state: tl::enums::updates::State = tl::types::updates::State {
            pts: START_PTS,
            qts: 0,
            date: now(),
            seq: 0,
            unread_count: 0,
        }
        .into();
        return Some(Reply::ok(&state));
    }
    if req.is::<tl::functions::updates::GetDifference>() {
        let empty: tl::enums::updates::Difference = tl::types::updates::DifferenceEmpty {
            date: now(),
            seq: 0,
        }
        .into();
        return Some(Reply::ok(&empty));
    }
    None
}
