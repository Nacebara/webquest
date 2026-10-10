//! Числа и время для экранов (SPEC §6.1). Суммы — строкой из `Decimal` без незначащих нулей,
//! у USDT — минимум два знака (`97.50`); разделитель — точка, без пробелов между разрядами:
//! сумму копируют. Проценты — по-русски: `2,5 %`. Время — в поясе из конфига (CLAUDE.md:
//! в UI — пояс владельца или клиента), внутри `<tg-time>` — запасной текст для старых клиентов.

use chrono::{DateTime, FixedOffset, Utc};
use domain::{Asset, Decimal, Money};

use crate::html::{Frag, esc};

/// Сумма без актива: `97.50` (USDT), `1.5` (TON).
pub fn amount(value: Decimal, asset: Asset) -> String {
    let mut n = value.normalize();
    if n.is_zero() {
        n = Decimal::ZERO;
    }
    if asset == Asset::Usdt && n.scale() < 2 {
        n.rescale(2);
    }
    n.to_string()
}

/// Сумма с активом: `97.50 USDT`.
pub fn money(m: &Money) -> String {
    format!("{} {}", amount(m.amount(), m.asset()), m.asset().code())
}

/// Сумма для инлайн-команды кошелька: без лишних нулей (`10`, `12.5`).
pub fn bare_amount(value: Decimal) -> String {
    value.normalize().to_string()
}

/// Процент: `2,5 %` (неразрывный пробел перед знаком).
pub fn percent(pct: Decimal) -> String {
    format!("{}\u{a0}%", pct.normalize().to_string().replace('.', ","))
}

/// Как показывать момент времени.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormat {
    /// `14:05`
    Time,
    /// `07.10 14:05`
    DayTime,
    /// `07.10.2026 14:05`
    Full,
}

impl TimeFormat {
    /// Формат `<tg-time format="…">`. `t` проверен в lovec; `dt` — допущение SPEC §6.1
    /// (сверить с разделом «date-time entity formatting» Bot API).
    pub const fn tg_format(self) -> &'static str {
        match self {
            TimeFormat::Time => "t",
            TimeFormat::DayTime | TimeFormat::Full => "dt",
        }
    }

    const fn pattern(self) -> &'static str {
        match self {
            TimeFormat::Time => "%H:%M",
            TimeFormat::DayTime => "%d.%m %H:%M",
            TimeFormat::Full => "%d.%m.%Y %H:%M",
        }
    }
}

// Смещения проверяются при компиляции.
const UTC_OFFSET: FixedOffset = match FixedOffset::east_opt(0) {
    Some(offset) => offset,
    None => panic!("UTC+0"),
};
const MSK_OFFSET: FixedOffset = match FixedOffset::east_opt(3 * 3600) {
    Some(offset) => offset,
    None => panic!("UTC+3"),
};

/// Часовой пояс запасного текста времени и его подпись (`МСК`, `UTC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clock {
    offset: FixedOffset,
    label: String,
}

impl Clock {
    /// `offset_secs` — смещение от UTC в секундах; `None`, если вне ±24 ч.
    pub fn new(offset_secs: i32, label: impl Into<String>) -> Option<Self> {
        Some(Self {
            offset: FixedOffset::east_opt(offset_secs)?,
            label: label.into(),
        })
    }

    pub fn utc() -> Self {
        Self {
            offset: UTC_OFFSET,
            label: "UTC".to_owned(),
        }
    }

    /// Москва, UTC+3 — пояс по умолчанию для русскоязычных клиентов.
    pub fn msk() -> Self {
        Self {
            offset: MSK_OFFSET,
            label: "МСК".to_owned(),
        }
    }

    /// Текст времени в поясе: `14:05 МСК`.
    pub fn text(&self, at: DateTime<Utc>, format: TimeFormat) -> String {
        let local = at.with_timezone(&self.offset).format(format.pattern());
        if self.label.is_empty() {
            local.to_string()
        } else {
            format!("{local} {}", self.label)
        }
    }

    /// `<tg-time>` с запасным текстом. В обычном HTML — тег, если `tag_in_html`, иначе текст:
    /// если Bot API не примет тег в HTML-режиме, денежное сообщение не должно ломаться.
    pub fn frag(&self, at: DateTime<Utc>, format: TimeFormat, tag_in_html: bool) -> Frag {
        let text = esc(&self.text(at, format)).into_owned();
        let tag = format!(
            "<tg-time unix=\"{}\" format=\"{}\">{text}</tg-time>",
            at.timestamp(),
            format.tg_format()
        );
        let plain = if tag_in_html { tag.clone() } else { text };
        Frag::dual(tag, plain)
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::msk()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn usdt_has_two_decimals_ton_is_normalized() {
        assert_eq!(amount(dec!(97.5), Asset::Usdt), "97.50");
        assert_eq!(amount(dec!(100), Asset::Usdt), "100.00");
        assert_eq!(amount(dec!(0.123456), Asset::Usdt), "0.123456");
        assert_eq!(amount(dec!(1.500), Asset::Ton), "1.5");
        assert_eq!(amount(dec!(0.000), Asset::Usdt), "0.00");
        assert_eq!(amount(dec!(-0), Asset::Ton), "0");
        let m = Money::new(dec!(32.49), Asset::Usdt).unwrap();
        assert_eq!(money(&m), "32.49 USDT");
        assert_eq!(bare_amount(dec!(10.00)), "10");
    }

    #[test]
    fn percent_is_russian() {
        assert_eq!(percent(dec!(2.50)), "2,5\u{a0}%");
        assert_eq!(percent(dec!(3)), "3\u{a0}%");
        assert_eq!(percent(dec!(1.05)), "1,05\u{a0}%");
    }

    #[test]
    fn clock_renders_local_fallback() {
        let at = Utc.with_ymd_and_hms(2026, 10, 7, 21, 5, 0).unwrap();
        let msk = Clock::msk();
        assert_eq!(msk.text(at, TimeFormat::Time), "00:05 МСК");
        assert_eq!(msk.text(at, TimeFormat::Full), "08.10.2026 00:05 МСК");
        let frag = msk.frag(at, TimeFormat::Time, true);
        assert_eq!(
            frag.plain(),
            "<tg-time unix=\"1791407100\" format=\"t\">00:05 МСК</tg-time>"
        );
        assert_eq!(msk.frag(at, TimeFormat::Time, false).plain(), "00:05 МСК");
        assert_eq!(
            Clock::utc().text(at, TimeFormat::DayTime),
            "07.10 21:05 UTC"
        );
        assert!(Clock::new(25 * 3600, "x").is_none());
    }
}
