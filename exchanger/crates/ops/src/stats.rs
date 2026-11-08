//! «Статистика» пульта — макет «Пульт → Статистика»: заголовок с периодом, картинка (плитки
//! и график, [`crate::chart`]), «По направлениям», «Ручной разбор», подвал «Обновлено …»,
//! кнопки периода. rich-сообщение с картинкой внутри (`InputRichMessage.media`).

use chrono::{Datelike as _, Duration, NaiveDateTime, Timelike as _};
use domain::Decimal;

use crate::chart::{Chart, Day, Kpi};
use crate::texts::{Texts, esc};
use crate::{Cb, Inner, OpsError, btn};

const MONTHS: [&str; 12] = [
    "янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
];
const WEEKDAYS: [&str; 7] = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"];

/// Готовая карточка: rich-HTML, картинка и кнопки.
pub struct StatsCard {
    pub html: String,
    pub chart: Chart,
    pub keyboard: botapi::InlineKeyboardMarkup,
}

/// Строка «По направлениям»: куда, обменов, p95 в секундах.
pub struct DirectionRow {
    pub name: String,
    pub completed: i64,
    pub p95_secs: i64,
}

/// Данные карточки, из базы или из макета.
pub struct StatsData {
    pub days: u8,
    pub title: String,
    pub turnover: Decimal,
    pub completed: i64,
    pub gross: Decimal,
    pub prev: Option<(Decimal, i64, Decimal)>,
    pub series: Vec<Day>,
    pub directions: Vec<DirectionRow>,
    /// Ручной разбор: заявок, медиана и максимум, секунды.
    pub manual: (i64, i64, i64),
    pub updated: String,
}

fn date(d: NaiveDateTime) -> String {
    format!("{} {}", d.day(), MONTHS[d.month0() as usize % 12])
}

fn delta(cur: Decimal, prev: Decimal) -> Option<i64> {
    if prev <= Decimal::ZERO {
        return None;
    }
    let pct = ((cur - prev) * Decimal::ONE_HUNDRED / prev).round();
    i64::try_from(pct.mantissa())
        .ok()
        .map(|m| m / 10_i64.pow(pct.scale()))
}

fn minutes(secs: i64) -> i64 {
    (secs + 59) / 60
}

impl StatsData {
    pub fn card(&self, t: &Texts) -> StatsCard {
        let (pt, pc, pg) = self.prev.unwrap_or_default();
        let chart = Chart {
            kpis: vec![
                Kpi {
                    label: t.t("stats.k_turnover"),
                    value: ui::fmt::grouped(self.turnover),
                    delta_pct: self.prev.and_then(|_| delta(self.turnover, pt)),
                },
                Kpi {
                    label: t.t("stats.k_count"),
                    value: ui::fmt::grouped(Decimal::from(self.completed)),
                    delta_pct: self
                        .prev
                        .and_then(|_| delta(Decimal::from(self.completed), Decimal::from(pc))),
                },
                Kpi {
                    label: t.t("stats.k_gross"),
                    value: ui::fmt::grouped(self.gross),
                    delta_pct: self.prev.and_then(|_| delta(self.gross, pg)),
                },
            ],
            days: self.series.clone(),
            legend: t.t("stats.legend"),
        };
        let mut html = format!(
            "<h3>{}</h3><img src=\"tg://photo?id=chart\"/>",
            esc(&self.title)
        );
        html.push_str(&format!(
            "<details><summary>{}</summary><table compact striped><tr><th>{}</th><th align=\"right\">{}</th><th align=\"right\">{}</th></tr>",
            t.t("stats.by_dir"),
            t.t("stats.col_to"),
            t.t("stats.col_count"),
            t.t("stats.col_p95"),
        ));
        for d in &self.directions {
            html.push_str(&format!(
                "<tr><td>{}</td><td align=\"right\">{}</td><td align=\"right\">{} с</td></tr>",
                esc(&d.name),
                d.completed,
                d.p95_secs
            ));
        }
        if self.directions.is_empty() {
            html.push_str(&format!(
                "<tr><td colspan=\"3\">{}</td></tr>",
                t.t("stats.none")
            ));
        }
        html.push_str("</table></details>");
        let (n, median, max) = self.manual;
        let manual = if n > 0 {
            t.f(
                "stats.manual_line",
                &[
                    ("n", &n.to_string()),
                    ("median", &minutes(median).to_string()),
                    ("max", &minutes(max).to_string()),
                ],
            )
        } else {
            t.t("stats.manual_none")
        };
        html.push_str(&format!(
            "<details><summary>{}</summary><p>{manual}</p></details><footer>{}</footer>",
            t.t("stats.manual"),
            t.f("stats.updated", &[("time", &self.updated)])
        ));
        let period = |days: u8, key: &str| {
            let b = btn(t.t(key), &Cb::Stats(days));
            if days == self.days {
                b.style(botapi::ButtonStyle::Primary)
            } else {
                b
            }
        };
        let keyboard = botapi::InlineKeyboardMarkup::new(vec![
            vec![
                period(1, "stats.b_today"),
                period(7, "stats.b_7"),
                period(30, "stats.b_30"),
            ],
            vec![btn(t.t("common.home"), &Cb::Menu)],
        ]);
        StatsCard {
            html,
            chart,
            keyboard,
        }
    }

    /// Данные макета (7 дней · 29 окт – 4 ноя) — для «Предпросмотра экранов».
    pub fn sample(t: &Texts) -> Self {
        let day = |label: &str, turnover: i64, profit: i64| Day {
            label: label.into(),
            turnover: Decimal::from(turnover),
            profit: Decimal::from(profit),
        };
        Self {
            days: 7,
            title: t.f(
                "stats.title_n",
                &[("days", &"7"), ("from", &"29 окт"), ("to", &"4 ноя")],
            ),
            turnover: Decimal::from(74_830),
            completed: 812,
            gross: Decimal::from(1721),
            prev: Some((Decimal::from(65_640), 745, Decimal::from(1624))),
            series: vec![
                day("чт", 8900, 205),
                day("пт", 9600, 221),
                day("сб", 11800, 271),
                day("вс", 12900, 297),
                day("пн", 10400, 239),
                day("вт", 9300, 214),
                day("ср", 11930, 274),
            ],
            directions: vec![
                DirectionRow {
                    name: "CB → xR".into(),
                    completed: 591,
                    p95_secs: 13,
                },
                DirectionRow {
                    name: "xR → CB".into(),
                    completed: 164,
                    p95_secs: 15,
                },
                DirectionRow {
                    name: "Счета".into(),
                    completed: 57,
                    p95_secs: 22,
                },
            ],
            manual: (9, 7 * 60, 31 * 60),
            updated: "14:05".into(),
        }
    }
}

impl Inner {
    /// Статистика из базы за последние `days` дней (1 — сегодня по часам).
    pub(crate) async fn stats_data(&self, days: u8) -> Result<StatsData, OpsError> {
        let t = &self.texts;
        let tz = self.cfg.tz.as_str();
        let mut conn = self.pool.acquire().await?;
        let (local_now, midnight) = storage::admin::local_midnight(&mut conn, tz).await?;
        let len = Duration::days(i64::from(days));
        let from = midnight - Duration::days(i64::from(days) - 1);
        let to = from + len;
        let unit = if days == 1 { "hour" } else { "day" };
        let points = storage::admin::daily_stats(&mut conn, from, to, tz, unit).await?;
        let prev_points =
            storage::admin::daily_stats(&mut conn, from - len, from, tz, "day").await?;
        let local_from = local_now.date().and_hms_opt(0, 0, 0).unwrap_or(local_now)
            - Duration::days(i64::from(days) - 1);
        // Пустые дни и часы — нулём, чтобы ось была непрерывной.
        let mut series = Vec::new();
        let slots = if days == 1 { 24 } else { i64::from(days) };
        for i in 0..slots {
            let at = if days == 1 {
                local_from + Duration::hours(i)
            } else {
                local_from + Duration::days(i)
            };
            let p = points.iter().find(|p| p.bucket == at);
            let label = match days {
                1 => format!("{}", at.hour()),
                7 => WEEKDAYS[at.weekday().num_days_from_monday() as usize].to_owned(),
                _ => at.day().to_string(),
            };
            // На 24 часах и 30 днях подписываем не каждое деление.
            let every = if slots > 12 { 3 } else { 1 };
            series.push(Day {
                label: if i % every == 0 { label } else { String::new() },
                turnover: p.map_or(Decimal::ZERO, |p| p.turnover),
                profit: p.map_or(Decimal::ZERO, |p| p.fees),
            });
        }
        let sum = |pts: &[storage::admin::DayPoint]| {
            pts.iter()
                .fold((Decimal::ZERO, 0, Decimal::ZERO), |(t, c, g), p| {
                    (t + p.turnover, c + p.completed, g + p.fees)
                })
        };
        let (turnover, completed, gross) = sum(&points);
        let prev = sum(&prev_points);
        let mut directions: Vec<DirectionRow> = Vec::new();
        for d in storage::admin::direction_stats(&mut conn, from, to).await? {
            let name = match d.direction.as_str() {
                "cb_to_xr_check" => "CB → xR".to_owned(),
                "xr_to_cb_check" => "xR → CB".to_owned(),
                _ => t.t("stats.invoices"),
            };
            match directions.iter_mut().find(|r| r.name == name) {
                Some(row) => {
                    row.completed += d.completed;
                    row.p95_secs = row.p95_secs.max(d.p95_secs);
                }
                None => directions.push(DirectionRow {
                    name,
                    completed: d.completed,
                    p95_secs: d.p95_secs,
                }),
            }
        }
        let m = storage::admin::manual_stats(&mut conn, from, to).await?;
        let last = local_now.date().and_hms_opt(0, 0, 0).unwrap_or(local_now);
        let title = if days == 1 {
            t.f("stats.title_1", &[("date", &date(last))])
        } else {
            t.f(
                "stats.title_n",
                &[
                    ("days", &days.to_string()),
                    ("from", &date(local_from)),
                    ("to", &date(last)),
                ],
            )
        };
        Ok(StatsData {
            days,
            title,
            turnover,
            completed,
            gross,
            prev: Some(prev),
            series,
            directions,
            manual: (m.orders, m.median_secs, m.max_secs),
            updated: format!("{:02}:{:02}", local_now.hour(), local_now.minute()),
        })
    }

    /// Прислать карточку статистики. Картинку Telegram не принял — те же цифры текстом.
    pub(crate) async fn send_stats(&self, chat: i64, days: u8) -> Result<(), OpsError> {
        let card = self.stats_data(days).await?.card(&self.texts);
        let sent = match card.chart.png() {
            Ok(png) => self
                .api
                .send_rich_with_photo(chat, &card.html, "chart", png, Some(&card.keyboard))
                .await
                .map(|_| ()),
            Err(e) => {
                tracing::warn!(error = %e, "stats chart not rendered");
                Err(botapi::Error::Config("chart not rendered".into()))
            }
        };
        if let Err(e) = sent {
            tracing::warn!(error = %e, "stats card not sent, plain text instead");
            let kpis: Vec<String> = card
                .chart
                .kpis
                .iter()
                .map(|k| format!("{}: <b>{}</b>", esc(&k.label), esc(&k.value)))
                .collect();
            let mut req = botapi::SendMessage::html(chat, kpis.join("\n"));
            req.reply_markup = Some(botapi::ReplyMarkup::Inline(card.keyboard));
            self.api.send_message(&req).await?;
        }
        Ok(())
    }
}

/// Карточка статистики на данных макета — для «Предпросмотра экранов» (демо).
pub fn sample_card() -> Result<StatsCard, OpsError> {
    let t = Texts::builtin().map_err(|e| OpsError::Texts(e.to_string()))?;
    Ok(StatsData::sample(&t).card(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_card_is_the_mockup() {
        let card = sample_card().unwrap();
        assert!(
            card.html.starts_with("<h3>📊 7 дней · 29 окт – 4 ноя</h3>"),
            "{}",
            card.html
        );
        assert!(card.html.contains("<img src=\"tg://photo?id=chart\"/>"));
        assert!(card.html.contains("CB → xR") && card.html.contains("13 с"));
        assert!(
            card.html
                .contains("9 заявок · медиана 7 мин · максимум 31 мин")
        );
        assert_eq!(card.chart.kpis[0].value, "74\u{a0}830");
        assert_eq!(card.chart.kpis[0].delta_pct, Some(14));
        assert_eq!(card.chart.kpis[1].delta_pct, Some(9));
        assert_eq!(card.chart.kpis[2].delta_pct, Some(6));
        assert!(card.chart.png().is_ok());
    }
}
