//! Сторожа значков (решение владельца 21.11): каждый значок, который ставит бот, есть в наборах
//! Telegram (`tools/emoji/demo-packs.json` — наборы, которые видит владелец), и у каждой кнопки
//! пульта и инвестора есть значок нашего набора. Сегодняшние нарушения — списками `MISSING_*`
//! и `BARE_BUTTONS`; список только сокращается: исправили — тест скажет убрать строку.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

fn repo(path: &str) -> String {
    format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))
}

/// Значков нет в наборах Telegram (SCREENS-21-11, раздел 1): клиент видит обычный эмодзи.
const MISSING_ANIMATED: &[&str] = &[
    "color/closed",
    "color/empty",
    "color/in_check",
    "color/invest",
    "color/other_coin",
    "color/with_me",
    "mono/invest",
    "mono/reply",
];
const MISSING_STATIC: &[&str] = &[
    "color/closed",
    "color/down",
    "color/empty",
    "color/flame",
    "color/in_check",
    "color/invest",
    "color/other_coin",
    "color/with_me",
    "mono/down",
    "mono/invest",
    "mono/reply",
];

/// Кнопки без значка (SCREENS-21-11, раздел 1): ключ `ru.toml`.
const BARE_BUTTONS: &[&str] = &[
    "ops:dir.b_fee",
    "ops:dir.b_min_fee",
    "ops:dir.b_lever",
    "ops:dir.b_limits",
    "ops:dir.b_newcomer",
    "ops:dir.b_tiers",
    "ops:dir.b_mode_to_tiers",
    "ops:dir.b_mode_to_flat",
    "ops:dir.b_clear_auto",
    "ops:dir.b_fiat",
    "ops:promo.b_off",
    "ops:promo.b_hint",
    "ops:promo.b_auto",
    "ops:promo.b_rate",
    "ops:promo.b_thresholds",
    "ops:promo.b_target",
    "ops:user.b_terms",
    "ops:user.b_daily",
    "ops:user.b_unban",
    "ops:user.b_note",
    "ops:user.b_fee",
    "ops:user.b_min_fee",
    "ops:user.b_min",
    "ops:user.b_max",
    "ops:user.b_exempt_on",
    "ops:user.b_exempt_off",
    "ops:user.b_reset",
    "ops:settings.b_newcomer",
    "ops:settings.b_maint",
    "ops:settings.b_kill_off",
    "ops:settings.b_emoji",
    "ops:staff.b_role",
    "ops:staff.b_disable",
    "ops:invest.b_investor",
    "ops:invest.b_move",
    "ops:maint.b_more",
    "ui:menu.invest",
    "ui:invest.btn_deposit",
    "ui:invest.btn_withdraw",
    "ui:invest.btn_howto",
    "ui:invest.btn_refresh",
    "ui:invest.btn_all",
    "ui:invest.btn_home",
];

fn packs() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(repo("tools/emoji/demo-packs.json")).unwrap())
        .unwrap()
}

fn walk(prefix: &str, v: &toml::Value, out: &mut Vec<(String, String)>) {
    match v {
        toml::Value::Table(t) => {
            for (k, v) in t {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                walk(&key, v, out);
            }
        }
        toml::Value::String(s) => out.push((prefix.to_owned(), s.clone())),
        _ => {}
    }
}

fn texts(file: &str) -> Vec<(String, String)> {
    let table: toml::Table = toml::from_str(&std::fs::read_to_string(repo(file)).unwrap()).unwrap();
    let mut out = Vec::new();
    walk("", &toml::Value::Table(table), &mut out);
    out
}

/// Значки, которые ставит бот: роли клиента (текст и кнопки) и токены `{e:…}` / `{m:…}` пульта.
fn used_icons() -> BTreeSet<String> {
    let mut out: BTreeSet<String> = ui::Emoji::ALL
        .iter()
        .flat_map(|r| [r.text_icon().to_owned(), r.button_icon().to_owned()])
        .collect();
    for (_, s) in texts("crates/ops/ru.toml") {
        for part in s.split('{').skip(1) {
            let token = part.split('}').next().unwrap_or_default();
            if let Some(n) = token.strip_prefix("e:") {
                out.insert(format!("color/{n}"));
            } else if let Some(n) = token.strip_prefix("m:") {
                out.insert(format!("mono/{n}"));
            }
        }
    }
    out
}

/// Значок есть в наборе: точное имя или (как `Look::icon`) линейный вместо цветного.
fn in_pack(pack: &serde_json::Value, icon: &str) -> bool {
    let name = icon.split('/').nth(1).unwrap_or_default();
    pack.get(icon).is_some()
        || (icon.starts_with("color/") && pack.get(format!("mono/{name}")).is_some())
}

#[test]
fn every_icon_the_bot_uses_is_in_the_telegram_packs() {
    let packs = packs();
    let used = used_icons();
    let mut problems = Vec::new();
    for (style, known) in [("animated", MISSING_ANIMATED), ("static", MISSING_STATIC)] {
        let pack = &packs[style];
        for icon in &used {
            let missing = !in_pack(pack, icon);
            let listed = known.contains(&icon.as_str());
            if missing && !listed {
                problems.push(format!("{style}: «{icon}» нет в наборе Telegram"));
            }
        }
        for icon in known.iter() {
            if in_pack(pack, icon) {
                problems.push(format!(
                    "{style}: «{icon}» уже в наборе — уберите из списка"
                ));
            }
        }
    }
    let all: BTreeSet<String> = [("ops", "crates/ops/ru.toml"), ("ui", "crates/ui/ru.toml")]
        .iter()
        .flat_map(|(bot, file)| {
            texts(file)
                .into_iter()
                .map(move |(k, _)| format!("{bot}:{k}"))
        })
        .collect();
    for id in BARE_BUTTONS {
        if !all.contains(*id) {
            problems.push(format!("{id}: такого ключа нет — уберите из BARE_BUTTONS"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn every_button_has_an_icon() {
    let mut problems = Vec::new();
    for (bot, file) in [("ops", "crates/ops/ru.toml"), ("ui", "crates/ui/ru.toml")] {
        for (key, text) in texts(file) {
            let last = key.rsplit('.').next().unwrap_or_default();
            let button = last.starts_with("b_")
                || last.starts_with("btn_")
                || (bot == "ui" && key == "menu.invest");
            if !button {
                continue;
            }
            let id = format!("{bot}:{key}");
            let icon = text.contains("{m:")
                || text.contains("{e:")
                || text.contains("{m_")
                || text.contains("{icon}");
            let listed = BARE_BUTTONS.contains(&id.as_str());
            if !icon && !listed {
                problems.push(format!("{id} = «{text}»: кнопка без значка"));
            }
            if icon && listed {
                problems.push(format!("{id}: значок есть — уберите из BARE_BUTTONS"));
            }
        }
    }
    let all: BTreeSet<String> = [("ops", "crates/ops/ru.toml"), ("ui", "crates/ui/ru.toml")]
        .iter()
        .flat_map(|(bot, file)| {
            texts(file)
                .into_iter()
                .map(move |(k, _)| format!("{bot}:{k}"))
        })
        .collect();
    for id in BARE_BUTTONS {
        if !all.contains(*id) {
            problems.push(format!("{id}: такого ключа нет — уберите из BARE_BUTTONS"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}
