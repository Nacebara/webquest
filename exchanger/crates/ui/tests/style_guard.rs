//! Страж стиля (решение владельца 16.11): стиль интерфейса не должен «поплыть» через месяц.
//!
//! - Заголовков h1–h6 нет нигде: вместо них значок и жирная строка.
//! - В текстах клиента нет обычных эмодзи: только роли премиум-эмодзи (`{e_…}`), у которых есть
//!   запасной символ на случай, если наборы не установлены.

use std::fs;
use std::path::{Path, PathBuf};

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("папка исходников") {
        let path = entry.expect("запись папки").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Обычный эмодзи: пиктограммы, символы, стрелки-эмодзи и вариант-селектор U+FE0F.
/// Типографские стрелки «→» и «⇄» (U+2190–U+21FF) — не эмодзи, они разрешены.
fn is_plain_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF | 0xFE0F)
}

#[test]
fn no_headings_in_interface_code_or_texts() {
    let mut files = Vec::new();
    for krate in ["ui", "ops", "tgbot"] {
        rust_files(&crates_dir().join(krate).join("src"), &mut files);
        files.push(crates_dir().join(krate).join("ru.toml"));
    }
    let mut bad = Vec::new();
    for file in files.iter().filter(|f| f.exists()) {
        let text = fs::read_to_string(file).expect("файл читается");
        for (n, line) in text.lines().enumerate() {
            if (1..=6).any(|h| line.contains(&format!("<h{h}"))) {
                bad.push(format!("{}:{}", file.display(), n + 1));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "заголовки вместо «значок + жирная строка»: {bad:?}"
    );
}

#[test]
fn client_texts_use_premium_emoji_roles_only() {
    let text = fs::read_to_string(crates_dir().join("ui").join("ru.toml")).expect("ru.toml");
    let bad: Vec<String> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with('#'))
        .filter(|(_, l)| l.chars().any(is_plain_emoji))
        .map(|(n, l)| format!("{}: {}", n + 1, l.trim()))
        .collect();
    assert!(
        bad.is_empty(),
        "обычные эмодзи в текстах клиента — нужна роль {{e_…}}: {bad:#?}"
    );
}

/// Чек — на предъявителя: созданный в группе заберёт посторонний (аудит 16.11, К-6). Создавать
/// чек — только здесь, в чате с ботом.
#[test]
fn never_advise_creating_a_check_in_any_chat() {
    let mut files = vec![crates_dir().join("ui").join("ru.toml")];
    let widgets = crates_dir().join("..").join("assets").join("widgets");
    for entry in fs::read_dir(&widgets).expect("assets/widgets") {
        files.push(entry.expect("запись папки").path());
    }
    for file in files {
        let text = fs::read_to_string(&file)
            .expect("файл читается")
            .to_lowercase();
        assert!(
            !text.contains("в любом чате") && !text.contains("в любой чат"),
            "{}: чек создают только здесь, в чате с ботом",
            file.display()
        );
    }
}
