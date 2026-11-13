//! `config.toml` — настройки без секретов (как у гаранта): пояс владельца, поддержка, логи,
//! пульс. Секреты — только в `.env`. Переменные окружения сильнее файла.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// Файл по умолчанию — рядом с `.env`, его создаёт `exch setup`.
pub const EXAMPLE: &str = include_str!("../../../config.example.toml");

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    owner: Owner,
    #[serde(default)]
    log: Log,
    #[serde(default)]
    health: Health,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    timezone: Option<String>,
    support: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Log {
    format: Option<String>,
    level: Option<String>,
    /// Папка журналов (шаг 2.6); `off` — не писать в файл.
    dir: Option<String>,
    /// Сколько дней хранить журналы.
    keep_days: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Health {
    url: Option<String>,
}

/// Значения файла под именами переменных окружения (`TZ_OWNER`, `SUPPORT_CONTACT`, …).
pub fn parse(text: &str) -> Result<HashMap<&'static str, String>, String> {
    let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut out = HashMap::new();
    let mut put = |k: &'static str, v: Option<String>| {
        if let Some(v) = v.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) {
            out.insert(k, v);
        }
    };
    put("TZ_OWNER", f.owner.timezone);
    put("SUPPORT_CONTACT", f.owner.support);
    put("LOG_FORMAT", f.log.format);
    put("RUST_LOG", f.log.level);
    put("LOG_DIR", f.log.dir);
    put("LOG_KEEP_DAYS", f.log.keep_days.map(|d| d.to_string()));
    put("HEALTHCHECK_URL", f.health.url);
    Ok(out)
}

/// Прочитать файл, если он есть. Ошибка — с номером строки, чтобы её можно было найти.
pub fn load(path: &Path) -> Result<HashMap<&'static str, String>, String> {
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse(text.strip_prefix('\u{feff}').unwrap_or(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses_and_maps_to_env_names() {
        let m = parse(EXAMPLE).unwrap();
        assert_eq!(m.get("TZ_OWNER").map(String::as_str), Some("Europe/Moscow"));
        assert_eq!(m.get("LOG_FORMAT").map(String::as_str), Some("text"));
        assert!(!m.contains_key("SUPPORT_CONTACT"), "пусто — не задано");
    }

    #[test]
    fn typos_are_reported() {
        assert!(parse("[owner]\ntimezon = \"UTC\"").is_err());
    }
}
