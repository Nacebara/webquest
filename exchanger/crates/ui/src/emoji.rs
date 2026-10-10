//! Эмодзи по ролям (SPEC §6.4). При Premium у владельца бота роль раскрывается в
//! `<tg-emoji emoji-id="…">⏳</tg-emoji>` и в `icon_custom_emoji_id` кнопок; без Premium или
//! без id для роли — в обычный символ. `custom_emoji_id` подбирает владелец
//! (`settings.custom_emoji`), здесь только проверяется, что это число.

use std::collections::BTreeMap;

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
}

impl Emoji {
    pub const ALL: [Emoji; 19] = [
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
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.key() == key)
    }
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
    ids: BTreeMap<Emoji, String>,
}

impl EmojiSet {
    /// Без Premium: только обычные эмодзи.
    pub fn plain() -> Self {
        Self::default()
    }

    /// С Premium: id кастомных эмодзи по ролям. Роли без id — обычными символами.
    pub fn premium(ids: impl IntoIterator<Item = (Emoji, String)>) -> Result<Self, EmojiError> {
        let mut map = BTreeMap::new();
        for (role, id) in ids {
            let valid = !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit());
            if !valid {
                return Err(EmojiError::BadId(role.key()));
            }
            map.insert(role, id);
        }
        Ok(Self {
            premium: true,
            ids: map,
        })
    }

    /// Из настроек: `premium_ok` — результат пробной отправки при старте (SPEC §6.4),
    /// `custom` — `settings.custom_emoji` (ключ — [`Emoji::key`]).
    pub fn from_settings<'a>(
        premium_ok: bool,
        custom: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, EmojiError> {
        let mut ids = Vec::new();
        for (key, id) in custom {
            let role =
                Emoji::from_key(key).ok_or_else(|| EmojiError::UnknownRole(key.to_owned()))?;
            ids.push((role, id.to_owned()));
        }
        if premium_ok {
            Self::premium(ids)
        } else {
            // Проверяем и без Premium: ошибка в настройках видна сразу, а не после покупки.
            Self::premium(ids)?;
            Ok(Self::plain())
        }
    }

    pub fn is_premium(&self) -> bool {
        self.premium
    }

    /// `custom_emoji_id` роли, если Premium включён и id задан.
    pub fn custom_id(&self, role: Emoji) -> Option<&str> {
        if self.premium {
            self.ids.get(&role).map(String::as_str)
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
            Some(role) => match self.custom_id(role) {
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

    #[test]
    fn settings_are_validated() {
        assert_eq!(
            EmojiSet::from_settings(true, [("wiat", "1")]),
            Err(EmojiError::UnknownRole("wiat".into()))
        );
        assert_eq!(
            EmojiSet::from_settings(true, [("ok", "12\" onclick=\"x")]),
            Err(EmojiError::BadId("ok"))
        );
        let off = EmojiSet::from_settings(false, [("ok", "123")]).unwrap();
        assert!(!off.is_premium());
        assert_eq!(off.custom_id(Emoji::Ok), None);
    }
}
