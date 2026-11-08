//! Картинка «Статистики» — макет «Пульт → Статистика»: три плитки (оборот, обменов, валовая
//! прибыль с изменением к прошлому периоду) и график — столбцы оборота по дням, поверх
//! зелёная линия валовой прибыли со своей шкалой справа. Одна PNG внутри rich-карточки
//! (`InputRichMessage.media`, решение владельца 9 ноября: плитки и график одной картинкой).
//!
//! Рисуем SVG и растеризуем `resvg`; шрифт встроен (DejaVu Sans), чтобы картинка была
//! одинаковой на сервере и на ноутбуке. Координаты — `Decimal` (дробей с плавающей точкой
//! в проекте нет).

use std::sync::OnceLock;

use domain::Decimal;
use resvg::{tiny_skia, usvg};

const FONT: &[u8] = include_bytes!("../../../assets/fonts/DejaVuSans.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../../../assets/fonts/DejaVuSans-Bold.ttf");

/// Цвета макета (кнопки и точки Telegram).
const INK: &str = "#131A2A";
const MUTED: &str = "#5E6D7F";
const LINE: &str = "#DCE2EA";
const CARD: &str = "#FFFFFF";
const BG: &str = "#F2F4F7";
const BAR: &str = "#3A86F0";
const PROFIT: &str = "#2FA34F";
const DOWN: &str = "#E0483F";

/// Плитка: подпись, значение, изменение к прошлому периоду в % (`None` — сравнивать не с чем).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kpi {
    pub label: String,
    pub value: String,
    pub delta_pct: Option<i64>,
}

/// День графика: подпись, оборот, валовая прибыль (USDT).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    pub label: String,
    pub turnover: Decimal,
    pub profit: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chart {
    pub kpis: Vec<Kpi>,
    pub days: Vec<Day>,
    /// Пояснение под графиком.
    pub legend: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ChartError {
    #[error("chart svg: {0}")]
    Svg(String),
    #[error("chart pixmap")]
    Pixmap,
    #[error("chart png: {0}")]
    Png(String),
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn n(v: Decimal) -> String {
    v.round_dp(1).normalize().to_string()
}

fn d(v: i64) -> Decimal {
    Decimal::from(v)
}

/// Круглый шаг шкалы: наименьшее из 1, 2, 2.5, 5 × 10ᵏ, не меньше `v`.
fn nice_step(v: Decimal) -> Decimal {
    let v = v.max(Decimal::new(1, 2));
    let mut base = Decimal::new(1, 2);
    while base * d(10) <= v {
        base *= d(10);
    }
    for m in [d(1), d(2), Decimal::new(25, 1), d(5), d(10)] {
        if base * m >= v {
            return base * m;
        }
    }
    base * d(10)
}

/// Шкалы с общими линиями сетки (как в макете): шаг оборота круглый, делений ≤ 4; у прибыли
/// столько же делений и тоже круглый шаг. `(делений, шаг оборота, шаг прибыли)`.
fn scales(max_t: Decimal, max_p: Decimal) -> (i64, Decimal, Decimal) {
    let step_t = nice_step(max_t.max(d(100)) / d(4));
    let mut k = 1;
    while step_t * d(k) < max_t && k < 4 {
        k += 1;
    }
    let step_p = nice_step(max_p.max(d(4)) / d(k));
    (k, step_t, step_p)
}

/// Подпись деления: «15k», «2.5k», «300».
fn tick(v: Decimal) -> String {
    if v >= d(1000) {
        format!("{}k", (v / d(1000)).normalize())
    } else {
        v.normalize().to_string()
    }
}

impl Chart {
    /// SVG 1080 × 820 — вдвое крупнее, чем показывает Telegram, чтобы было чётко.
    pub fn svg(&self) -> String {
        let (w, h) = (1080, 820);
        let mut s = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="DejaVu Sans">"#
        );
        s.push_str(&format!(
            r#"<rect width="{w}" height="{h}" rx="36" fill="{BG}"/>"#
        ));

        // Плитки.
        let count = i64::try_from(self.kpis.len().max(1)).unwrap_or(1);
        let gap = 20;
        let tile_w = (d(w) - d(48) * d(2) - d(gap) * (d(count) - Decimal::ONE)) / d(count);
        for (i, k) in self.kpis.iter().enumerate() {
            let x = d(48) + (tile_w + d(gap)) * d(i64::try_from(i).unwrap_or(0));
            s.push_str(&format!(
                r#"<rect x="{}" y="40" width="{}" height="170" rx="28" fill="{CARD}"/>"#,
                n(x),
                n(tile_w)
            ));
            s.push_str(&format!(
                r#"<text x="{}" y="88" font-size="28" fill="{MUTED}">{}</text>"#,
                n(x + d(28)),
                esc(&k.label)
            ));
            s.push_str(&format!(
                r#"<text x="{}" y="148" font-size="46" font-weight="bold" fill="{INK}">{}</text>"#,
                n(x + d(28)),
                esc(&k.value)
            ));
            if let Some(p) = k.delta_pct {
                let (arrow, color) = if p >= 0 {
                    ("▲", PROFIT)
                } else {
                    ("▼", DOWN)
                };
                s.push_str(&format!(
                    r#"<text x="{}" y="190" font-size="26" fill="{color}">{arrow} {}&#160;%</text>"#,
                    n(x + d(28)),
                    p.abs()
                ));
            }
        }

        // График: слева оборот, справа прибыль, общие линии сетки.
        let (left, right, top, bottom) = (d(120), d(w - 110), d(260), d(h - 120));
        let plot_h = bottom - top;
        let (k, step_t, step_p) = scales(
            self.days
                .iter()
                .map(|x| x.turnover)
                .max()
                .unwrap_or_default(),
            self.days.iter().map(|x| x.profit).max().unwrap_or_default(),
        );
        let (max_t, max_p) = (step_t * d(k), step_p * d(k));
        let y_t = |v: Decimal| top + plot_h * (Decimal::ONE - v / max_t);
        let y_p = |v: Decimal| top + plot_h * (Decimal::ONE - v / max_p);
        for i in 0..=k {
            let part = d(i) / d(k);
            let y = top + plot_h * (Decimal::ONE - part);
            s.push_str(&format!(
                r#"<line x1="{}" x2="{}" y1="{}" y2="{}" stroke="{LINE}" stroke-width="2"/>"#,
                n(left),
                n(right),
                n(y),
                n(y)
            ));
            s.push_str(&format!(
                r#"<text x="{}" y="{}" font-size="24" text-anchor="end" fill="{MUTED}">{}</text>"#,
                n(left - d(14)),
                n(y + d(8)),
                tick(step_t * d(i))
            ));
            s.push_str(&format!(
                r#"<text x="{}" y="{}" font-size="24" fill="{PROFIT}">{}</text>"#,
                n(right + d(14)),
                n(y + d(8)),
                tick(step_p * d(i))
            ));
        }
        let days = i64::try_from(self.days.len().max(1)).unwrap_or(1);
        let slot = (right - left) / d(days);
        let bar_w = (slot * Decimal::new(56, 2)).min(d(90));
        let mut points = Vec::new();
        for (i, day) in self.days.iter().enumerate() {
            let cx = left + slot * (d(i64::try_from(i).unwrap_or(0)) + Decimal::new(5, 1));
            let y = y_t(day.turnover);
            let bar_h = (bottom - y).max(Decimal::ZERO);
            if bar_h > Decimal::ZERO {
                s.push_str(&format!(
                    r#"<rect x="{}" y="{}" width="{}" height="{}" rx="10" fill="{BAR}" fill-opacity="0.85"/>"#,
                    n(cx - bar_w / d(2)),
                    n(y),
                    n(bar_w),
                    n(bar_h)
                ));
            }
            s.push_str(&format!(
                r#"<text x="{}" y="{}" font-size="24" text-anchor="middle" fill="{MUTED}">{}</text>"#,
                n(cx),
                n(bottom + d(36)),
                esc(&day.label)
            ));
            points.push((cx, y_p(day.profit)));
        }
        if !points.is_empty() {
            let path: Vec<String> = points
                .iter()
                .map(|(x, y)| format!("{},{}", n(*x), n(*y)))
                .collect();
            s.push_str(&format!(
                r#"<polyline points="{}" fill="none" stroke="{PROFIT}" stroke-width="5" stroke-linejoin="round" stroke-linecap="round"/>"#,
                path.join(" ")
            ));
            let last = points.len() - 1;
            for (i, (x, y)) in points.iter().enumerate() {
                let r = if i == last { 9 } else { 6 };
                s.push_str(&format!(
                    r#"<circle cx="{}" cy="{}" r="{r}" fill="{PROFIT}"/>"#,
                    n(*x),
                    n(*y)
                ));
            }
        }
        s.push_str(&format!(
            r#"<text x="48" y="{}" font-size="24" fill="{MUTED}">{}</text>"#,
            h - 36,
            esc(&self.legend)
        ));
        s.push_str("</svg>");
        s
    }

    /// PNG для `InputRichMessage.media`.
    pub fn png(&self) -> Result<Vec<u8>, ChartError> {
        static FONTS: OnceLock<std::sync::Arc<usvg::fontdb::Database>> = OnceLock::new();
        let fonts = FONTS.get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_font_data(FONT.to_vec());
            db.load_font_data(FONT_BOLD.to_vec());
            db.set_sans_serif_family("DejaVu Sans");
            std::sync::Arc::new(db)
        });
        let options = usvg::Options {
            fontdb: fonts.clone(),
            ..usvg::Options::default()
        };
        let tree = usvg::Tree::from_str(&self.svg(), &options)
            .map_err(|e| ChartError::Svg(e.to_string()))?;
        let size = tree.size().to_int_size();
        let mut pixmap =
            tiny_skia::Pixmap::new(size.width(), size.height()).ok_or(ChartError::Pixmap)?;
        resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
        pixmap
            .encode_png()
            .map_err(|e| ChartError::Png(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Chart {
        let day = |label: &str, t: i64, p: i64| Day {
            label: label.into(),
            turnover: d(t),
            profit: d(p),
        };
        Chart {
            kpis: vec![
                Kpi {
                    label: "Оборот".into(),
                    value: "74 830".into(),
                    delta_pct: Some(14),
                },
                Kpi {
                    label: "Обменов".into(),
                    value: "812".into(),
                    delta_pct: Some(9),
                },
                Kpi {
                    label: "Валовая".into(),
                    value: "1 721".into(),
                    delta_pct: Some(-6),
                },
            ],
            days: vec![
                day("чт", 8900, 205),
                day("пт", 9600, 221),
                day("сб", 11800, 271),
                day("вс", 12900, 297),
                day("пн", 10400, 239),
                day("вт", 9300, 214),
                day("ср", 11930, 274),
            ],
            legend: "Столбцы — оборот, USDT (слева) · линия — валовая прибыль (справа)".into(),
        }
    }

    #[test]
    fn scale_steps_are_round() {
        // Макет: оборот до 12 900 → 0, 5k, 10k, 15k; прибыль на тех же линиях.
        assert_eq!(scales(d(12900), d(297)), (3, d(5000), d(100)));
        assert_eq!(scales(d(0), d(0)), (1, d(25), Decimal::new(5, 0)));
        assert_eq!(nice_step(d(240)), Decimal::new(250, 0));
        assert_eq!(tick(d(15000)), "15k");
        assert_eq!(tick(d(2500)), "2.5k");
    }

    #[test]
    fn renders_a_png_with_text() {
        let chart = sample();
        let svg = chart.svg();
        assert!(svg.contains("74 830") && svg.contains("▼ 6"), "{svg}");
        let png = chart.png().unwrap();
        // Посмотреть глазами: EXCH_CHART_OUT=/tmp/chart.png cargo test -p ops chart.
        if let Ok(path) = std::env::var("EXCH_CHART_OUT") {
            std::fs::write(path, &png).unwrap();
        }
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(
            png.len() > 20_000,
            "картинка с текстом и графиком, {} байт",
            png.len()
        );
    }

    #[test]
    fn empty_period_still_renders() {
        let chart = Chart {
            kpis: vec![Kpi {
                label: "Оборот".into(),
                value: "0".into(),
                delta_pct: None,
            }],
            days: Vec::new(),
            legend: String::new(),
        };
        assert!(chart.png().is_ok());
    }
}
