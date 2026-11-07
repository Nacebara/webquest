//! Тексты пульта из `ops/ru.toml`: плоский каталог `раздел.ключ`. Подстановки `{имя}` — уже
//! готовый HTML: числа и подписи кода, пользовательский текст — через [`esc`].

use std::collections::HashMap;
use std::fmt::Display;

const BUILTIN_RU: &str = include_str!("../ru.toml");

#[derive(Debug, Clone)]
pub struct Texts {
    map: HashMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
#[error("ops/ru.toml: {0}")]
pub struct TextsError(String);

impl Texts {
    pub fn builtin() -> Result<Self, TextsError> {
        Self::parse(BUILTIN_RU)
    }

    pub fn parse(src: &str) -> Result<Self, TextsError> {
        let table: toml::Table = toml::from_str(src).map_err(|e| TextsError(e.to_string()))?;
        let mut map = HashMap::new();
        for (section, value) in table {
            let toml::Value::Table(entries) = value else {
                return Err(TextsError(format!("[{section}] must be a table")));
            };
            for (key, value) in entries {
                let toml::Value::String(text) = value else {
                    return Err(TextsError(format!("{section}.{key} must be a string")));
                };
                map.insert(format!("{section}.{key}"), text);
            }
        }
        Ok(Self { map })
    }

    pub fn has(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Текст без подстановок. Нет ключа — сам ключ (тест не даёт до этого дойти).
    pub fn t(&self, key: &str) -> String {
        self.map.get(key).cloned().unwrap_or_else(|| {
            tracing::error!(key, "ops text is missing");
            key.to_owned()
        })
    }

    /// Текст с подстановками: значения — готовый HTML.
    pub fn f(&self, key: &str, args: &[(&str, &dyn Display)]) -> String {
        let mut out = self.t(key);
        for (name, value) in args {
            out = out.replace(&format!("{{{name}}}"), &value.to_string());
        }
        out
    }
}

/// Пользовательский текст в HTML.
pub fn esc(s: &str) -> String {
    ui::html::esc(s).into_owned()
}

/// `<code>…</code>` с экранированием.
pub fn code(s: &str) -> String {
    format!("<code>{}</code>", esc(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_loads_and_fills() {
        let t = Texts::builtin().unwrap();
        assert_eq!(t.t("roles.owner"), "Владелец");
        assert_eq!(
            t.f("common.not_allowed", &[("role", &"Админ")]),
            "Это действие доступно роли «Админ» и выше."
        );
        assert_eq!(esc("<b>&"), "&lt;b&gt;&amp;");
    }
}
