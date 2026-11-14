//! Шаг 6.7 плана: каждый утверждённый виджет укладывается в лимиты Telegram (HTML ≤ 32 КБ, ≤ 4 КБ
//! после zlib-9, без нулевого байта; состояние из макетов ≤ 1 КБ), а список виджетов в коде —
//! ровно тот, что в макетах (`docs/mockups/widgets/samples.json`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;

use serde_json::Value;
use ui::widgets::{DATA_MAX, HTML_MAX, WidgetKind, ZIP_MAX};

fn zlib9(data: &[u8]) -> usize {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(data).unwrap();
    enc.finish().unwrap().len()
}

fn samples() -> serde_json::Map<String, Value> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/mockups/widgets/samples.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// «@-120» в макете — время относительно «сейчас»; в бою это 10-значное unix-время.
fn live(v: &Value) -> Value {
    match v {
        Value::String(s)
            if s.len() > 2
                && s.starts_with('@')
                && s[1..2].chars().all(|c| c == '+' || c == '-')
                && s[2..].chars().all(|c| c.is_ascii_digit()) =>
        {
            Value::from(1_793_877_600_i64)
        }
        Value::Array(a) => Value::Array(a.iter().map(live).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), live(x))).collect()),
        other => other.clone(),
    }
}

#[test]
fn every_widget_fits_telegram_limits() {
    let mut problems = Vec::new();
    for kind in WidgetKind::ALL {
        let html = kind.html();
        let packed = zlib9(html.as_bytes());
        if html.is_empty() || html.len() > HTML_MAX {
            problems.push(format!("{}: HTML {} байт", kind.name(), html.len()));
        }
        if packed > ZIP_MAX {
            problems.push(format!("{}: сжатым {packed} байт", kind.name()));
        }
        if html.contains('\0') {
            problems.push(format!("{}: нулевой байт", kind.name()));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn widget_list_is_the_approved_one_and_sample_states_fit() {
    let samples = samples();
    let mut in_mockups: Vec<&str> = samples
        .values()
        .filter_map(|v| v.get("w").and_then(Value::as_str))
        .collect();
    in_mockups.sort_unstable();
    in_mockups.dedup();
    let mut in_code: Vec<&str> = WidgetKind::ALL.iter().map(|k| k.name()).collect();
    in_code.sort_unstable();
    assert_eq!(in_code, in_mockups, "виджеты в коде и в макетах");
    for (name, demo) in &samples {
        let Some(states) = demo.get("states").and_then(Value::as_array) else {
            continue;
        };
        for st in states {
            if st.get("rich").is_some() {
                continue;
            }
            for key in ["data", "answer"] {
                if let Some(d) = st.get(key) {
                    let bytes = serde_json::to_string(&live(d)).unwrap().len();
                    assert!(bytes <= DATA_MAX, "{name}/{}: {key} {bytes} байт", st["id"]);
                }
            }
        }
    }
    assert_eq!(WidgetKind::from_name("dash"), Some(WidgetKind::Dash));
    assert!(WidgetKind::Dash.for_staff() && !WidgetKind::OrderA.for_staff());
}
