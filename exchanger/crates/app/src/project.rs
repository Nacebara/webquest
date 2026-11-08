//! Папка проекта, как у гаранта: `.env` (секреты) и `config.toml` (остальное) лежат в корне
//! проекта, бинарник — в `target/release`. Запуск двойным щелчком по `exch.exe` идёт из
//! `target/release`, поэтому файлы ищутся и рядом с бинарником, и на два уровня выше.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Где лежат файлы настроек.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Корень проекта: здесь `.env` и `config.toml`.
    pub dir: PathBuf,
    pub env: PathBuf,
    pub config: PathBuf,
}

impl Paths {
    fn in_dir(dir: PathBuf) -> Self {
        Self {
            env: dir.join(".env"),
            config: dir.join("config.toml"),
            dir,
        }
    }

    /// Файлы рядом с явно указанным `--env-file`.
    pub fn for_env_file(env: PathBuf) -> Self {
        let dir = env
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self {
            config: dir.join("config.toml"),
            env,
            dir,
        }
    }
}

/// Найти корень проекта: первая из папок «текущая, папка бинарника, её родитель, родитель
/// родителя», где есть `.env`; если `.env` ещё нигде нет — первая, похожая на корень проекта
/// (`config.example.toml` или `.env.example`); иначе текущая.
pub fn locate() -> Paths {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut candidates = vec![cwd.clone()];
    if let Ok(exe) = std::env::current_exe() {
        candidates.extend(exe.ancestors().skip(1).take(3).map(Path::to_path_buf));
    }
    locate_in(&candidates).unwrap_or_else(|| Paths::in_dir(cwd))
}

fn locate_in(candidates: &[PathBuf]) -> Option<Paths> {
    candidates
        .iter()
        .find(|d| d.join(".env").is_file())
        .or_else(|| {
            candidates.iter().find(|d| {
                d.join("config.example.toml").is_file() || d.join(".env.example").is_file()
            })
        })
        .map(|d| Paths::in_dir(d.clone()))
}

/// Файл `.env` построчно: комментарии, порядок и незнакомые переменные сохраняются,
/// меняются только строки нужных ключей.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvFile {
    lines: Vec<String>,
}

impl EnvFile {
    pub fn parse(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        Self {
            lines: text.lines().map(str::to_owned).collect(),
        }
    }

    pub fn read(path: &Path) -> std::io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(Self::parse(&text))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn key_of(line: &str) -> Option<&str> {
        let line = line.trim_start();
        if line.starts_with('#') {
            return None;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, _) = line.split_once('=')?;
        let key = key.trim();
        (!key.is_empty()).then_some(key)
    }

    /// Значение ключа. Пусто — `None`.
    pub fn get(&self, key: &str) -> Option<String> {
        self.values().remove(key)
    }

    /// Все непустые значения — по правилам dotenv (кавычки, экранирование). Ключ повторён —
    /// действует первая строка, как при запуске (`dotenvy` не перезаписывает уже заданное).
    pub fn values(&self) -> HashMap<String, String> {
        let text = self.to_text();
        let mut out = HashMap::new();
        for (k, v) in
            dotenvy::from_read_iter(std::io::Cursor::new(text.into_bytes())).filter_map(Result::ok)
        {
            // Частая ошибка: `KEY=KEY=…` (как в `Config::from_lookup`).
            let prefix = format!("{k}=");
            let mut v = v.trim().to_owned();
            while let Some(rest) = v.strip_prefix(prefix.as_str()) {
                v = rest.trim().to_owned();
            }
            out.entry(k).or_insert(v);
        }
        out.retain(|_, v| !v.is_empty());
        out
    }

    /// Задать значение: заменить строку ключа (все повторы, кроме первого, — убрать) или
    /// дописать в конец.
    pub fn set(&mut self, key: &str, value: &str) {
        let line = format!("{key}={}", quote(value));
        let mut found = false;
        self.lines.retain_mut(|l| {
            if Self::key_of(l) != Some(key) {
                return true;
            }
            if found {
                return false;
            }
            found = true;
            l.clone_from(&line);
            true
        });
        if !found {
            self.lines.push(line);
        }
    }

    pub fn to_text(&self) -> String {
        let mut text = self.lines.join("\n");
        text.push('\n');
        text
    }

    /// Записать файл; прежний сохраняется как `.env.bak`.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if path.is_file() {
            std::fs::copy(path, path.with_extension("bak"))?;
        }
        std::fs::write(path, self.to_text())
    }
}

/// Значение для строки dotenv: простые символы — как есть, остальное — в кавычках.
fn quote(value: &str) -> String {
    let plain = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@%+=,".contains(&b));
    if plain {
        value.to_owned()
    } else if !value.contains('\'') && !value.contains('\n') {
        format!("'{value}'")
    } else {
        let mut out = String::from("\"");
        for c in value.chars() {
            match c {
                '"' | '\\' | '$' => {
                    out.push('\\');
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

/// Строки dotenv для переменных из `config.toml`: их подхватывает `dotenvy` так же, как `.env`.
pub fn dotenv_text(values: &HashMap<&'static str, String>) -> String {
    let mut keys: Vec<_> = values.keys().copied().collect();
    keys.sort_unstable();
    keys.into_iter()
        .map(|k| format!("{k}={}\n", quote(&values[k])))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_keeps_comments_and_unknown_keys() {
        let mut f = EnvFile::parse("\u{feff}# секреты\nDATABASE_URL=old\nFOO=bar\nOWNER_TG_ID=\n");
        f.set(
            "DATABASE_URL",
            "postgres://postgres:p%40ss@localhost:5432/exch",
        );
        f.set("OWNER_TG_ID", "42");
        f.set("SESSION_KEY", "AAEC+/==");
        assert_eq!(
            f.to_text(),
            "# секреты\nDATABASE_URL=postgres://postgres:p%40ss@localhost:5432/exch\nFOO=bar\nOWNER_TG_ID=42\nSESSION_KEY=AAEC+/==\n"
        );
        assert_eq!(f.get("FOO").as_deref(), Some("bar"));
        assert_eq!(f.get("OWNER_TG_ID").as_deref(), Some("42"));
    }

    #[test]
    fn doubled_keys_collapse_and_doubled_names_are_forgiven() {
        let mut f = EnvFile::parse("SESSION_KEY=SESSION_KEY=abc\nX=1\nSESSION_KEY=\n");
        assert_eq!(
            f.get("SESSION_KEY").as_deref(),
            Some("abc"),
            "действует первая строка"
        );
        let f2 = EnvFile::parse("SESSION_KEY=\nSESSION_KEY=abc\n");
        assert_eq!(
            f2.get("SESSION_KEY"),
            None,
            "первая строка пустая — как при запуске"
        );
        f.set("SESSION_KEY", "new");
        assert_eq!(f.to_text(), "SESSION_KEY=new\nX=1\n");
    }

    #[test]
    fn odd_values_survive_a_round_trip() {
        for v in ["a b#c", "it's \"x\" $HOME \\", "простой"] {
            let mut f = EnvFile::default();
            f.set("V", v);
            assert_eq!(f.get("V").as_deref(), Some(v), "{}", f.to_text());
        }
    }

    #[test]
    fn project_root_is_found_from_target_release() {
        let root = std::env::temp_dir().join(format!("exch-locate-{}", std::process::id()));
        let release = root.join("target").join("release");
        std::fs::create_dir_all(&release).unwrap();
        std::fs::write(root.join("config.example.toml"), "").unwrap();
        let candidates = vec![
            release.clone(),
            release.parent().unwrap().to_path_buf(),
            root.clone(),
        ];
        assert_eq!(
            locate_in(&candidates).unwrap().dir,
            root,
            "без .env — корень проекта"
        );
        std::fs::write(release.join(".env"), "").unwrap();
        assert_eq!(
            locate_in(&candidates).unwrap().dir,
            release,
            ".env рядом — он главнее"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
