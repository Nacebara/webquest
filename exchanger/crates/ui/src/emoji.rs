//! Эмодзи по ролям (SPEC §6.4). При Premium у владельца бота роль раскрывается в
//! `<tg-emoji emoji-id="…">⏳</tg-emoji>` и в `icon_custom_emoji_id` кнопок; без Premium или
//! без id для роли — в обычный символ.
//!
//! Откуда id ([`EmojiSettings`]):
//! 1. свой эмодзи владельца для роли (`settings.custom_emoji`, роль → id) — любой премиум-эмодзи,
//!    как в боте-гаранте: владелец присылает эмодзи, бот берёт его `custom_emoji_id`;
//! 2. иначе — значок набора exch выбранной вариации (`settings.emoji_style`: анимированная,
//!    статичная или выключено; id наборов — `settings.emoji_packs`, их пишет `exch emoji install`).
//!    В тексте — цветная плитка ([`Emoji::text_icon`]), на кнопке — линейный значок, который
//!    Telegram перекрашивает в цвет текста ([`Emoji::button_icon`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::html::Frag;

/// Роль эмодзи. Ключ ([`Emoji::key`]) — имя в `settings.custom_emoji` и в плейсхолдере `{e_…}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Emoji {
    /// Ожидание (анимированный).
    Wait,
    /// Успех (анимированный).
    Ok,
    /// Ошибка (анимированный).
    Err,
    Warn,
    Invoice,
    Refund,
    Manual,
    Pause,
    Maintenance,
    Ban,
    Support,
    Cb,
    Xr,
    Copy,
    Exchange,
    Rates,
    History,
    Privacy,
    Bell,
    /// Выгодный курс встречного потока (DESIGN-v0.2 Р15).
    Promo,
    /// Инвестиции (DESIGN-v0.2 Р18).
    Invest,
    /// «Проверяю чек» — три точки «в работе» (макет «Эмодзи v2», выбор владельца).
    Process,
    /// «Отправляю … в xRocket».
    Sending,
    /// Кнопка «Забрать» у чека выплаты.
    Claim,
    /// Кнопка «Детали».
    Details,
    /// Кнопка «Скрыть».
    Hide,
    /// «Помощь» в меню.
    Help,
    /// «Курс 1:1 · обычно 10 секунд» — молния в подвале приветствия.
    Fast,
    /// «Лимит на сегодня исчерпан».
    Limit,
    /// Точки статуса (макет «Эмодзи v2»): работает, пауза, закрыто.
    DotOk,
    DotWait,
    DotClosed,
    /// Кнопка «Назад».
    Back,
    /// Кнопка заявки в «Истории».
    Order,
    /// Кнопка «Ещё» (следующая страница).
    More,
    /// Кнопка «Красивый вид» — вернуть карточку после «Простого вида».
    RichView,
    /// «Правила» в «Помощи» — щит.
    Rules,
}

impl Emoji {
    pub const ALL: [Emoji; 37] = [
        Emoji::Wait,
        Emoji::Ok,
        Emoji::Err,
        Emoji::Warn,
        Emoji::Invoice,
        Emoji::Refund,
        Emoji::Manual,
        Emoji::Pause,
        Emoji::Maintenance,
        Emoji::Ban,
        Emoji::Support,
        Emoji::Cb,
        Emoji::Xr,
        Emoji::Copy,
        Emoji::Exchange,
        Emoji::Rates,
        Emoji::History,
        Emoji::Privacy,
        Emoji::Bell,
        Emoji::Promo,
        Emoji::Invest,
        Emoji::Process,
        Emoji::Sending,
        Emoji::Claim,
        Emoji::Details,
        Emoji::Hide,
        Emoji::Help,
        Emoji::Fast,
        Emoji::Limit,
        Emoji::DotOk,
        Emoji::DotWait,
        Emoji::DotClosed,
        Emoji::Back,
        Emoji::Order,
        Emoji::More,
        Emoji::RichView,
        Emoji::Rules,
    ];

    pub const fn key(self) -> &'static str {
        match self {
            Emoji::Wait => "wait",
            Emoji::Ok => "ok",
            Emoji::Err => "err",
            Emoji::Warn => "warn",
            Emoji::Invoice => "invoice",
            Emoji::Refund => "refund",
            Emoji::Manual => "manual",
            Emoji::Pause => "pause",
            Emoji::Maintenance => "maintenance",
            Emoji::Ban => "ban",
            Emoji::Support => "support",
            Emoji::Cb => "cb",
            Emoji::Xr => "xr",
            Emoji::Copy => "copy",
            Emoji::Exchange => "exchange",
            Emoji::Rates => "rates",
            Emoji::History => "history",
            Emoji::Privacy => "privacy",
            Emoji::Bell => "bell",
            Emoji::Promo => "promo",
            Emoji::Invest => "invest",
            Emoji::Process => "process",
            Emoji::Sending => "sending",
            Emoji::Claim => "claim",
            Emoji::Details => "details",
            Emoji::Hide => "hide",
            Emoji::Help => "help",
            Emoji::Fast => "fast",
            Emoji::Limit => "limit",
            Emoji::DotOk => "dot_ok",
            Emoji::DotWait => "dot_wait",
            Emoji::DotClosed => "dot_closed",
            Emoji::Back => "back",
            Emoji::Order => "order",
            Emoji::More => "more",
            Emoji::RichView => "rich_view",
            Emoji::Rules => "rules",
        }
    }

    /// Обычный эмодзи — фолбэк без Premium (SPEC §6.4).
    pub const fn plain(self) -> &'static str {
        match self {
            Emoji::Wait => "⏳",
            Emoji::Ok => "✅",
            Emoji::Err => "❌",
            Emoji::Warn => "⚠️",
            Emoji::Invoice => "🧾",
            Emoji::Refund => "↩️",
            Emoji::Manual => "🛠",
            Emoji::Pause => "⏸",
            Emoji::Maintenance => "🛠",
            Emoji::Ban => "⛔",
            Emoji::Support => "💬",
            Emoji::Cb => "🔵",
            Emoji::Xr => "🚀",
            Emoji::Copy => "📋",
            Emoji::Exchange => "💱",
            Emoji::Rates => "📈",
            Emoji::History => "📜",
            Emoji::Privacy => "🔒",
            Emoji::Bell => "🔔",
            Emoji::Promo => "🔥",
            Emoji::Invest => "💼",
            Emoji::Process => "🔄",
            Emoji::Sending => "📤",
            Emoji::Claim => "📥",
            Emoji::Details => "👁",
            Emoji::Hide => "✖️",
            Emoji::Help => "❓",
            Emoji::Fast => "⚡",
            Emoji::Limit => "🚫",
            Emoji::DotOk => "🟢",
            Emoji::DotWait => "🟡",
            Emoji::DotClosed => "⚪",
            Emoji::Back => "◀️",
            Emoji::Order => "🔖",
            Emoji::More => "🔽",
            Emoji::RichView => "✨",
            Emoji::Rules => "🛡",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.key() == key)
    }

    /// Значок набора exch для текста (`assets/emoji/manifest.json`, `color/…` — плитки).
    pub const fn text_icon(self) -> &'static str {
        match self {
            Emoji::Wait => "color/wait",
            Emoji::Ok => "color/done",
            Emoji::Err => "color/err",
            Emoji::Warn => "color/warn",
            Emoji::Invoice => "color/invoice",
            Emoji::Refund => "color/refund",
            Emoji::Manual => "color/manual",
            Emoji::Pause => "color/pause",
            Emoji::Maintenance => "color/maint",
            Emoji::Ban => "color/ban",
            Emoji::Support => "color/support",
            Emoji::Cb => "color/cb",
            Emoji::Xr => "color/xr",
            Emoji::Copy => "mono/copy",
            Emoji::Exchange => "color/swap",
            Emoji::Rates => "color/rates",
            Emoji::History => "color/history",
            Emoji::Privacy => "color/lock",
            Emoji::Bell => "mono/bell",
            Emoji::Promo => "color/fire",
            Emoji::Invest => "color/vault",
            Emoji::Process => "color/process",
            Emoji::Sending => "color/sending",
            Emoji::Claim => "mono/claim",
            Emoji::Details => "mono/eye",
            Emoji::Hide => "mono/close",
            Emoji::Help => "mono/help",
            Emoji::Fast => "color/fast",
            Emoji::Limit => "color/limit",
            Emoji::DotOk => "color/dot_ok",
            Emoji::DotWait => "color/dot_wait",
            Emoji::DotClosed => "color/dot_closed",
            Emoji::Back => "mono/back",
            Emoji::Order => "mono/order",
            Emoji::More => "mono/refresh",
            Emoji::RichView => "mono/sparkle",
            Emoji::Rules => "color/shield",
        }
    }

    /// Значок набора exch для кнопки: линейный, Telegram перекрашивает его в цвет текста.
    pub const fn button_icon(self) -> &'static str {
        match self {
            Emoji::Wait => "mono/wait",
            Emoji::Ok => "mono/done",
            Emoji::Err => "mono/close",
            Emoji::Warn => "mono/alert",
            Emoji::Invoice => "mono/invoice",
            Emoji::Refund => "mono/refund",
            Emoji::Manual => "mono/eye",
            Emoji::Pause => "mono/pause",
            Emoji::Maintenance => "mono/maint",
            Emoji::Ban => "mono/ban",
            Emoji::Support => "mono/support",
            Emoji::Cb => "mono/cb",
            Emoji::Xr => "mono/xr",
            Emoji::Copy => "mono/copy",
            Emoji::Exchange => "mono/swap",
            Emoji::Rates => "mono/rates",
            Emoji::History => "mono/history",
            Emoji::Privacy => "mono/lock",
            Emoji::Bell => "mono/bell",
            Emoji::Promo => "mono/fire",
            Emoji::Invest => "mono/vault",
            Emoji::Process => "mono/spin",
            Emoji::Sending => "mono/fast",
            Emoji::Claim => "mono/claim",
            Emoji::Details => "mono/eye",
            Emoji::Hide => "mono/close",
            Emoji::Help => "mono/help",
            Emoji::Fast => "mono/fast",
            Emoji::Limit => "mono/limit",
            Emoji::DotOk => "color/dot_ok",
            Emoji::DotWait => "color/dot_wait",
            Emoji::DotClosed => "color/dot_closed",
            Emoji::Back => "mono/back",
            Emoji::Order => "mono/order",
            Emoji::More => "mono/refresh",
            Emoji::RichView => "mono/sparkle",
            Emoji::Rules => "mono/shield",
        }
    }
}

/// Вариация набора exch (`settings.emoji_style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmojiStyle {
    /// Анимированные значки (TGS).
    #[default]
    Animated,
    /// Статичные (PNG 100×100, кадр покоя тех же значков).
    Static,
    /// Без премиум-эмодзи: только обычные символы, свои эмодзи владельца тоже не показываются.
    Off,
}

impl EmojiStyle {
    pub const ALL: [EmojiStyle; 3] = [EmojiStyle::Animated, EmojiStyle::Static, EmojiStyle::Off];

    pub const fn key(self) -> &'static str {
        match self {
            EmojiStyle::Animated => "animated",
            EmojiStyle::Static => "static",
            EmojiStyle::Off => "off",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// id установленных наборов exch: вариация → значок (`color/done`) → `custom_emoji_id`
/// (`settings.emoji_packs`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmojiPacks {
    #[serde(default)]
    pub animated: BTreeMap<String, String>,
    #[serde(default, rename = "static")]
    pub still: BTreeMap<String, String>,
}

impl EmojiPacks {
    pub fn of(&self, style: EmojiStyle) -> Option<&BTreeMap<String, String>> {
        match style {
            EmojiStyle::Animated => Some(&self.animated),
            EmojiStyle::Static => Some(&self.still),
            EmojiStyle::Off => None,
        }
    }

    pub fn of_mut(&mut self, style: EmojiStyle) -> Option<&mut BTreeMap<String, String>> {
        match style {
            EmojiStyle::Animated => Some(&mut self.animated),
            EmojiStyle::Static => Some(&mut self.still),
            EmojiStyle::Off => None,
        }
    }
}

/// Настройки эмодзи: вариация, наборы и свои эмодзи владельца по ролям (роль → id).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmojiSettings {
    pub style: EmojiStyle,
    pub packs: EmojiPacks,
    pub custom: BTreeMap<String, String>,
}

/// Обычный символ вместо значка набора (без Premium и в запасном тексте), по имени значка
/// без вида: `alarm`, `queue`. Как `FB` в макете.
pub fn fallback_char(name: &str) -> &'static str {
    match name {
        "alarm" => "🚨",
        "alert" => "⚠️",
        "warn" => "⚠️",
        "live" => "🟢",
        "dot_ok" => "🟢",
        "dot_wait" => "🟠",
        "dot_act" => "🔴",
        "dot_work" => "🔵",
        "dot_closed" => "⚪",
        "step_done" => "✅",
        "step_now" => "🔵",
        "step_todo" => "⚪",
        "prog_25" => "◔",
        "prog_50" => "◑",
        "prog_75" => "◕",
        "done" => "✅",
        "err" | "close" => "❌",
        "kill" => "⛔",
        "pause" => "⏸",
        "play" => "▶️",
        "queue" => "📋",
        "invoice" => "🧾",
        "vault" => "🏦",
        "robot" => "🤖",
        "dash" => "🎛",
        "chart" => "📊",
        "plug" => "🔌",
        "cb" => "🔷",
        "xr" => "🚀",
        "swap" => "🔁",
        "rates" => "📈",
        "history" => "🕘",
        "support" => "💬",
        "shield" => "🛡",
        "lock" => "🔒",
        "key" => "🔑",
        "refresh" => "🔄",
        "settings" => "⚙️",
        "users" | "staff" => "👥",
        "user" => "👤",
        "journal" => "📓",
        "search" => "🔎",
        "fire" => "🔥",
        "flood" => "⌛",
        "maint" => "🛠",
        "ban" => "🚫",
        "limit" => "📏",
        "usdt" => "💲",
        "wallet" => "👛",
        "inbox" => "📥",
        "back" => "◀️",
        "home" => "🏠",
        "eye" => "👁",
        "emoji" => "😊",
        "megaphone" | "broadcast" => "📣",
        "export" => "📤",
        "sparkle" => "✨",
        "help" => "❓",
        "order" => "🔖",
        "server" => "🖥",
        "power" => "🛑",
        "edit" => "✏️",
        "take" => "✋",
        "trash" => "🗑",
        "calendar" => "📅",
        "filter" => "🔽",
        "star" => "⭐",
        "gift" => "🎁",
        "coin" => "🪙",
        "ack" => "👍",
        _ => "•",
    }
}

/// `custom_emoji_id` — число до 32 цифр (ничего другого в атрибут HTML не попадёт).
pub fn is_custom_emoji_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmojiError {
    #[error("unknown emoji role {0:?}")]
    UnknownRole(String),
    #[error("custom_emoji_id for {0:?} must be 1-32 digits")]
    BadId(&'static str),
}

/// Набор эмодзи для рендера.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmojiSet {
    premium: bool,
    /// id для текста.
    ids: BTreeMap<Emoji, String>,
    /// id для иконок кнопок; нет — как в тексте.
    buttons: BTreeMap<Emoji, String>,
    /// Весь действующий набор по ключу значка (`color/alarm`, `mono/queue`): пульт берёт
    /// значки не по ролям клиента, а прямо по ключу макета. Без Premium — пусто.
    pack: BTreeMap<String, String>,
}

impl EmojiSet {
    /// Без Premium: только обычные эмодзи.
    pub fn plain() -> Self {
        Self::default()
    }

    /// С Premium: id кастомных эмодзи по ролям (одни и те же в тексте и на кнопках). Роли
    /// без id — обычными символами.
    pub fn premium(ids: impl IntoIterator<Item = (Emoji, String)>) -> Result<Self, EmojiError> {
        let mut map = BTreeMap::new();
        for (role, id) in ids {
            if !is_custom_emoji_id(&id) {
                return Err(EmojiError::BadId(role.key()));
            }
            map.insert(role, id);
        }
        Ok(Self {
            premium: true,
            ids: map,
            buttons: BTreeMap::new(),
            pack: BTreeMap::new(),
        })
    }

    /// Из настроек ([`EmojiSettings`]). `premium_ok` — у владельца бота есть Premium и
    /// кастомные эмодзи доходят (SPEC §6.4); `false` или вариация «выключено» — обычные символы.
    /// Свои эмодзи владельца проверяются и тогда: ошибка в настройках видна сразу.
    pub fn from_settings(premium_ok: bool, settings: &EmojiSettings) -> Result<Self, EmojiError> {
        let mut custom = BTreeMap::new();
        for (key, id) in &settings.custom {
            let role =
                Emoji::from_key(key).ok_or_else(|| EmojiError::UnknownRole(key.to_owned()))?;
            if !is_custom_emoji_id(id) {
                return Err(EmojiError::BadId(role.key()));
            }
            custom.insert(role, id.clone());
        }
        let Some(pack) = settings.packs.of(settings.style).filter(|_| premium_ok) else {
            return Ok(Self::plain());
        };
        let from_pack = |icon: &str| pack.get(icon).filter(|id| is_custom_emoji_id(id)).cloned();
        let mut ids = BTreeMap::new();
        let mut buttons = BTreeMap::new();
        for role in Emoji::ALL {
            if let Some(id) = custom.get(&role) {
                ids.insert(role, id.clone());
                buttons.insert(role, id.clone());
                continue;
            }
            if let Some(id) = from_pack(role.text_icon()) {
                ids.insert(role, id);
            }
            if let Some(id) = from_pack(role.button_icon()) {
                buttons.insert(role, id);
            }
        }
        let pack = pack
            .iter()
            .filter(|(_, id)| is_custom_emoji_id(id))
            .map(|(k, id)| (k.clone(), id.clone()))
            .collect();
        Ok(Self {
            premium: true,
            ids,
            buttons,
            pack,
        })
    }

    pub fn is_premium(&self) -> bool {
        self.premium
    }

    /// `custom_emoji_id` значка набора по ключу (`color/alarm`, `mono/queue`).
    pub fn pack_id(&self, key: &str) -> Option<&str> {
        self.pack.get(key).map(String::as_str)
    }

    /// `custom_emoji_id` роли в тексте, если Premium включён и id задан.
    pub fn custom_id(&self, role: Emoji) -> Option<&str> {
        if self.premium {
            self.ids.get(&role).map(String::as_str)
        } else {
            None
        }
    }

    /// `custom_emoji_id` иконки кнопки: свой для кнопок, иначе как в тексте.
    pub fn button_id(&self, role: Emoji) -> Option<&str> {
        if self.premium {
            self.buttons
                .get(&role)
                .or_else(|| self.ids.get(&role))
                .map(String::as_str)
        } else {
            None
        }
    }

    /// Эмодзи в тексте сообщения.
    pub fn frag(&self, role: Emoji) -> Frag {
        match self.custom_id(role) {
            Some(id) => {
                let html = format!("<tg-emoji emoji-id=\"{id}\">{}</tg-emoji>", role.plain());
                Frag::dual(html.clone(), html)
            }
            None => Frag::trusted(role.plain()),
        }
    }

    /// Текст кнопки и иконка: при Premium — иконка вместо символа в тексте (DESIGN-v0.2 §6).
    pub fn button(&self, role: Option<Emoji>, label: &str) -> (String, Option<String>) {
        match role {
            None => (label.to_owned(), None),
            Some(role) => match self.button_id(role) {
                Some(id) => (label.to_owned(), Some(id.to_owned())),
                None => (format!("{} {label}", role.plain()), None),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_are_unique() {
        for role in Emoji::ALL {
            assert_eq!(Emoji::from_key(role.key()), Some(role));
        }
        let mut keys: Vec<_> = Emoji::ALL.iter().map(|e| e.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), Emoji::ALL.len());
    }

    #[test]
    fn premium_renders_tg_emoji_and_icons() {
        let set = EmojiSet::premium([(Emoji::Wait, "5368324170671202286".to_owned())]).unwrap();
        assert_eq!(
            set.frag(Emoji::Wait).plain(),
            "<tg-emoji emoji-id=\"5368324170671202286\">⏳</tg-emoji>"
        );
        assert_eq!(
            set.frag(Emoji::Ok).plain(),
            "✅",
            "роль без id — обычным символом"
        );
        assert_eq!(
            set.button(Some(Emoji::Wait), "Ждём"),
            ("Ждём".to_owned(), Some("5368324170671202286".to_owned()))
        );
        assert_eq!(
            set.button(Some(Emoji::Ok), "Ок"),
            ("✅ Ок".to_owned(), None)
        );
    }

    fn custom(pairs: &[(&str, &str)]) -> EmojiSettings {
        EmojiSettings {
            custom: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..EmojiSettings::default()
        }
    }

    #[test]
    fn settings_are_validated() {
        assert_eq!(
            EmojiSet::from_settings(true, &custom(&[("wiat", "1")])),
            Err(EmojiError::UnknownRole("wiat".into()))
        );
        assert_eq!(
            EmojiSet::from_settings(true, &custom(&[("ok", "12\" onclick=\"x")])),
            Err(EmojiError::BadId("ok"))
        );
        let off = EmojiSet::from_settings(false, &custom(&[("ok", "123")])).unwrap();
        assert!(!off.is_premium());
        assert_eq!(off.custom_id(Emoji::Ok), None);
    }

    fn packs() -> EmojiPacks {
        let mut p = EmojiPacks::default();
        for role in Emoji::ALL {
            p.animated
                .insert(role.text_icon().to_owned(), format!("1{}", role as u8));
            p.animated
                .insert(role.button_icon().to_owned(), format!("2{}", role as u8));
            p.still
                .insert(role.text_icon().to_owned(), format!("3{}", role as u8));
        }
        p
    }

    #[test]
    fn variation_picks_its_pack_and_owner_emoji_wins() {
        let mut s = EmojiSettings {
            packs: packs(),
            ..EmojiSettings::default()
        };
        let set = EmojiSet::from_settings(true, &s).unwrap();
        let ok = Emoji::Ok as u8;
        assert_eq!(set.custom_id(Emoji::Ok), Some(format!("1{ok}").as_str()));
        assert_eq!(set.button_id(Emoji::Ok), Some(format!("2{ok}").as_str()));

        // Статичная вариация: кнопки без своих значков берут значок текста.
        s.style = EmojiStyle::Static;
        let set = EmojiSet::from_settings(true, &s).unwrap();
        assert_eq!(set.custom_id(Emoji::Ok), Some(format!("3{ok}").as_str()));
        assert_eq!(set.button_id(Emoji::Ok), Some(format!("3{ok}").as_str()));

        // Свой эмодзи владельца — и в тексте, и на кнопке, в любой вариации.
        s.custom.insert("ok".into(), "5368324170671202286".into());
        let set = EmojiSet::from_settings(true, &s).unwrap();
        assert_eq!(set.custom_id(Emoji::Ok), Some("5368324170671202286"));
        assert_eq!(set.button_id(Emoji::Ok), Some("5368324170671202286"));
        assert_eq!(
            set.custom_id(Emoji::Err),
            Some(format!("3{}", Emoji::Err as u8).as_str())
        );

        // Выключено — обычные символы, свои эмодзи тоже.
        s.style = EmojiStyle::Off;
        let set = EmojiSet::from_settings(true, &s).unwrap();
        assert!(!set.is_premium());
        assert_eq!(set.frag(Emoji::Ok).plain(), "✅");
    }

    #[test]
    fn every_role_has_icons_of_the_exch_set() {
        let manifest = include_str!("../../../assets/emoji/manifest.json");
        for role in Emoji::ALL {
            for icon in [role.text_icon(), role.button_icon()] {
                assert!(
                    manifest.contains(&format!("\"key\": \"{icon}\"")),
                    "{role:?}: нет значка {icon} в наборе"
                );
            }
        }
        for style in EmojiStyle::ALL {
            assert_eq!(EmojiStyle::from_key(style.key()), Some(style));
        }
        let packs: EmojiPacks =
            serde_json::from_str(r#"{"animated": {"color/done": "1"}, "static": {}}"#).unwrap();
        assert_eq!(
            packs.animated.get("color/done").map(String::as_str),
            Some("1")
        );
    }
}
