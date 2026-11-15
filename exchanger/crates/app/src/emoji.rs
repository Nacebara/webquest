//! Премиум-эмодзи клиентского бота: настройки и установка наборов exch (`exch emoji …`).
//! Те же действия есть в пульте: переключить вариацию, поставить свой эмодзи на роль.
//! Клиентский бот подхватывает изменения на ходу (`tgbot::emoji::watch`).
//!
//! Настройки (`settings`, ui::EmojiSettings):
//! - `emoji_style` — `"animated"`, `"static"` или `"off"`;
//! - `emoji_packs` — id значков установленных наборов (`{"animated": {"color/done": "…"}, "static": {…}}`);
//! - `custom_emoji` — свои эмодзи владельца по ролям (`{"ok": "5368…"}`), сильнее набора.
//!
//! Установка: четыре набора кастомных эмодзи бота — анимированные и статичные, цветные плитки
//! и линейные значки (линейные Telegram перекрашивает в цвет текста, а это свойство всего
//! набора). Файлы и порядок — `assets/emoji/manifest.json`. Повторный запуск дописывает то,
//! чего не хватает, и обновляет id: набор — по числу стикеров в нём, без дублей.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use botapi::{
    AddStickerToSet, BotApi, CreateNewStickerSet, InputSticker, StickerFormat, StickerSet,
};
use serde::Deserialize;
use sqlx::PgConnection;
use ui::{Emoji, EmojiPacks, EmojiSettings, EmojiStyle, is_custom_emoji_id};

pub use tgbot::emoji::{CUSTOM_KEY, PACKS_KEY, STYLE_KEY};

/// Настройки эмодзи из базы. Битое значение — ошибка: лучше узнать сразу. Правило чтения —
/// одно с клиентским ботом, который подхватывает изменения пульта на ходу.
pub async fn load(conn: &mut PgConnection) -> anyhow::Result<EmojiSettings> {
    Ok(tgbot::emoji::load(conn).await?.0)
}

async fn save(conn: &mut PgConnection, key: &str, value: serde_json::Value) -> anyhow::Result<()> {
    storage::settings::set(conn, key, &value, None).await?;
    Ok(())
}

pub async fn set_style(conn: &mut PgConnection, style: EmojiStyle) -> anyhow::Result<()> {
    save(conn, STYLE_KEY, serde_json::json!(style.key())).await
}

/// Свой эмодзи на роль: id — из `custom_emoji` сущности сообщения с этим эмодзи.
pub async fn set_custom(conn: &mut PgConnection, role: Emoji, id: &str) -> anyhow::Result<()> {
    if !is_custom_emoji_id(id) {
        bail!("custom_emoji_id is 1–32 digits, got {id:?}");
    }
    let mut s = load(conn).await?;
    s.custom.insert(role.key().to_owned(), id.to_owned());
    save(conn, CUSTOM_KEY, serde_json::to_value(&s.custom)?).await
}

pub async fn reset_custom(conn: &mut PgConnection, role: Emoji) -> anyhow::Result<bool> {
    let mut s = load(conn).await?;
    let removed = s.custom.remove(role.key()).is_some();
    save(conn, CUSTOM_KEY, serde_json::to_value(&s.custom)?).await?;
    Ok(removed)
}

pub fn parse_role(key: &str) -> anyhow::Result<Emoji> {
    Emoji::from_key(key).with_context(|| {
        let all: Vec<&str> = Emoji::ALL.iter().map(|e| e.key()).collect();
        format!("unknown role {key:?}; roles: {}", all.join(", "))
    })
}

/// Что показывает `exch emoji show`.
pub fn describe(s: &EmojiSettings) -> String {
    let mut out = format!("Вариация: {}\n", s.style.key());
    for style in [EmojiStyle::Animated, EmojiStyle::Static] {
        let n = s.packs.of(style).map_or(0, BTreeMap::len);
        out.push_str(&format!("Набор {}: {n} значков\n", style.key()));
    }
    out.push_str("Роли (роль — обычный символ — источник):\n");
    for role in Emoji::ALL {
        let source = match s.custom.get(role.key()) {
            Some(id) => format!("свой эмодзи {id}"),
            None => format!("набор: {} / {}", role.text_icon(), role.button_icon()),
        };
        out.push_str(&format!(
            "  {:<12} {}  {source}\n",
            role.key(),
            role.plain()
        ));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Установка наборов
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestIcon {
    pub key: String,
    pub emoji: String,
    pub animated: String,
    #[serde(rename = "static")]
    pub still: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub icons: Vec<ManifestIcon>,
}

pub fn read_manifest(dir: &Path) -> anyhow::Result<Manifest> {
    let path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} is malformed", path.display()))
}

/// Один набор Telegram: вариация × вид значков.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackPlan {
    pub style: EmojiStyle,
    /// `color` или `mono`.
    pub kind: &'static str,
    pub name: String,
    pub title: String,
    pub repaint: bool,
}

/// Имена наборов: `exch_anim_color_by_<бот>` и т. д. (латиница, цифры, `_`, без `__`).
pub fn plans(bot_username: &str) -> Vec<PackPlan> {
    let bot = bot_username.trim_start_matches('@');
    let mut out = Vec::new();
    for (style, short, title) in [
        (EmojiStyle::Animated, "anim", "анимированные"),
        (EmojiStyle::Static, "still", "статичные"),
    ] {
        for (kind, kind_title, repaint) in [("color", "плитки", false), ("mono", "линии", true)]
        {
            out.push(PackPlan {
                style,
                kind,
                name: format!("exch_{short}_{kind}_by_{bot}"),
                title: format!("exch · {title} · {kind_title}"),
                repaint,
            });
        }
    }
    out
}

/// Сколько ждать между вызовами установки: Telegram ограничивает частоту.
const STEP_PAUSE: Duration = Duration::from_millis(400);
const MAX_ATTEMPTS: u32 = 5;

async fn with_retry<T, F, Fut>(what: &str, mut call: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, botapi::Error>>,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        match call().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < MAX_ATTEMPTS && e.retry_after().is_some() => {
                let wait = e.retry_after().unwrap_or(Duration::from_secs(5));
                eprintln!("  {what}: Telegram просит подождать {} с", wait.as_secs());
                tokio::time::sleep(wait).await;
            }
            Err(e) => return Err(anyhow::Error::new(e).context(what.to_owned())),
        }
    }
}

async fn current(bot: &BotApi, name: &str) -> anyhow::Result<Option<StickerSet>> {
    match bot.get_sticker_set(name).await {
        Ok(set) => Ok(Some(set)),
        Err(e)
            if e.description()
                .is_some_and(|d| d.contains("STICKERSET_INVALID")) =>
        {
            Ok(None)
        }
        Err(e) => Err(anyhow::Error::new(e).context(format!("getStickerSet {name}"))),
    }
}

async fn input(
    bot: &BotApi,
    owner: i64,
    dir: &Path,
    icon: &ManifestIcon,
    style: EmojiStyle,
) -> anyhow::Result<InputSticker> {
    let (rel, format) = match style {
        EmojiStyle::Static => (&icon.still, StickerFormat::Static),
        _ => (&icon.animated, StickerFormat::Animated),
    };
    let path = dir.join(rel);
    let bytes = std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("emoji")
        .to_owned();
    let file = with_retry(&format!("uploadStickerFile {}", icon.key), || {
        bot.upload_sticker_file(owner, format, &file_name, bytes.clone())
    })
    .await?;
    Ok(InputSticker {
        sticker: file.file_id,
        format,
        emoji_list: vec![icon.emoji.clone()],
    })
}

/// Ключ значка у каждого стикера набора — по содержимому файла: Telegram отдаёт файл ровно
/// таким, каким его загрузили (проверено 17.11 на живом наборе). Неизвестный файл — `None`.
pub(crate) fn keys_by_content(
    files: &[Vec<u8>],
    ours: &std::collections::HashMap<Vec<u8>, String>,
) -> Vec<Option<String>> {
    files.iter().map(|f| ours.get(f).cloned()).collect()
}

/// Ключи стикеров набора (Ж-2: по ключу, а не по порядку манифеста). У статичного набора
/// Telegram может перекодировать файлы — тогда ключи берутся по порядку анимированного набора
/// того же вида: установщик всегда добавляет значки в оба набора в одном порядке.
async fn set_keys(
    bot: &BotApi,
    set: &StickerSet,
    dir: &Path,
    icons: &[&ManifestIcon],
    plan: &PackPlan,
    animated: Option<&Vec<Option<String>>>,
) -> anyhow::Result<Vec<Option<String>>> {
    let mut ours = std::collections::HashMap::new();
    for icon in icons {
        let file = match plan.style {
            EmojiStyle::Static => &icon.still,
            _ => &icon.animated,
        };
        let bytes = std::fs::read(dir.join(file)).with_context(|| format!("read {file}"))?;
        ours.insert(bytes, icon.key.clone());
    }
    let mut files = Vec::with_capacity(set.stickers.len());
    for sticker in &set.stickers {
        files.push(
            with_retry(&format!("getFile {}", plan.name), || {
                bot.download_file(&sticker.file_id)
            })
            .await?,
        );
    }
    let mut keys = keys_by_content(&files, &ours);
    if keys.iter().all(Option::is_none)
        && let Some(order) = animated
    {
        keys = order.clone();
        keys.resize(set.stickers.len(), None);
    }
    Ok(keys)
}

/// Установить или дописать один набор по ключам значков: добавляются только те, которых в
/// наборе нет (где бы они ни стояли в манифесте). Вернуть пары «ключ → id» и ключи по порядку
/// стикеров набора (для статичного набора того же вида).
async fn install_pack(
    bot: &BotApi,
    owner: i64,
    dir: &Path,
    plan: &PackPlan,
    icons: &[&ManifestIcon],
    animated: Option<&Vec<Option<String>>>,
) -> anyhow::Result<(Vec<(String, String)>, Vec<Option<String>>)> {
    let Some(set) = current(bot, &plan.name).await? else {
        // Новый набор: первые до 50 значков сразу, остальные — дописываются ниже.
        let first: Vec<&ManifestIcon> = icons.iter().take(50).copied().collect();
        let mut stickers = Vec::with_capacity(first.len());
        for icon in first {
            stickers.push(input(bot, owner, dir, icon, plan.style).await?);
            tokio::time::sleep(STEP_PAUSE).await;
        }
        let request = CreateNewStickerSet::custom_emoji(
            owner,
            &plan.name,
            &plan.title,
            stickers,
            plan.repaint,
        );
        with_retry(&format!("createNewStickerSet {}", plan.name), || {
            bot.create_new_sticker_set(&request)
        })
        .await?;
        eprintln!("  {}: создан", plan.name);
        return Box::pin(install_pack(bot, owner, dir, plan, icons, animated)).await;
    };
    let mut keys = set_keys(bot, &set, dir, icons, plan, animated).await?;
    let missing: Vec<&ManifestIcon> = icons
        .iter()
        .filter(|i| !keys.iter().flatten().any(|k| k == &i.key))
        .copied()
        .collect();
    for (n, icon) in missing.iter().enumerate() {
        let sticker = input(bot, owner, dir, icon, plan.style).await?;
        let request = AddStickerToSet {
            user_id: owner,
            name: plan.name.clone(),
            sticker,
        };
        with_retry(&format!("addStickerToSet {}", plan.name), || {
            bot.add_sticker_to_set(&request)
        })
        .await?;
        keys.push(Some(icon.key.clone()));
        tokio::time::sleep(STEP_PAUSE).await;
        if (n + 1) % 10 == 0 {
            eprintln!("  {}: дописано {} из {}", plan.name, n + 1, missing.len());
        }
    }
    let set = if missing.is_empty() {
        set
    } else {
        current(bot, &plan.name)
            .await?
            .with_context(|| format!("set {} disappeared", plan.name))?
    };
    let pairs = keys
        .iter()
        .zip(&set.stickers)
        .filter_map(|(key, sticker)| {
            let id = sticker
                .custom_emoji_id
                .clone()
                .filter(|id| is_custom_emoji_id(id))?;
            Some((key.clone()?, id))
        })
        .collect();
    Ok((pairs, keys))
}

/// `exch emoji install`: все четыре набора; id сохраняются в `emoji_packs`.
/// Взять уже установленные наборы exch другого бота (`@from`): значки — публичные, их id
/// подходят любому боту, у владельца которого Telegram Premium. Ничего не загружает — читает
/// наборы и сопоставляет стикеры со значками `manifest.json` по содержимому файла (Ж-2);
/// чего в наборах нет — остаётся без id (обычный символ), пока не допишет `install`.
pub async fn adopt(
    conn: &mut PgConnection,
    bot: &BotApi,
    from: &str,
    dir: &Path,
) -> anyhow::Result<EmojiPacks> {
    let manifest = read_manifest(dir)?;
    let mut packs = load(conn).await?.packs;
    let mut animated: BTreeMap<&str, Vec<Option<String>>> = BTreeMap::new();
    for plan in plans(from) {
        let prefix = format!("{}/", plan.kind);
        let icons: Vec<&ManifestIcon> = manifest
            .icons
            .iter()
            .filter(|i| i.key.starts_with(&prefix))
            .collect();
        // Ж-2: берём то, что есть. Набора нет — пропуск, а не отказ.
        let Some(set) = current(bot, &plan.name).await? else {
            eprintln!("{}: набора нет — пропускаю", plan.name);
            continue;
        };
        let keys = set_keys(bot, &set, dir, &icons, &plan, animated.get(plan.kind)).await?;
        let pack = packs
            .of_mut(plan.style)
            .context("installable styles are animated and static")?;
        let mut found = 0;
        for (key, sticker) in keys.iter().zip(&set.stickers) {
            if let (Some(key), Some(id)) = (
                key,
                sticker
                    .custom_emoji_id
                    .as_ref()
                    .filter(|id| is_custom_emoji_id(id)),
            ) {
                pack.insert(key.clone(), id.clone());
                found += 1;
            }
        }
        let missing = icons
            .iter()
            .filter(|i| !keys.iter().flatten().any(|k| k == &i.key))
            .count();
        eprintln!(
            "{} — {found} значков, нет в наборе {missing} (дописать: exch emoji install): t.me/addemoji/{}",
            plan.title, plan.name
        );
        if plan.style == EmojiStyle::Animated {
            animated.insert(plan.kind, keys);
        }
    }
    save(conn, PACKS_KEY, serde_json::to_value(&packs)?).await?;
    Ok(packs)
}

pub async fn install(
    conn: &mut PgConnection,
    bot: &BotApi,
    owner: i64,
    dir: &Path,
) -> anyhow::Result<EmojiPacks> {
    let manifest = read_manifest(dir)?;
    let me = bot.get_me().await.context("getMe")?;
    let username = me.username.context("the bot has no username")?;
    let mut packs = load(conn).await?.packs;
    // Порядок ключей анимированного набора каждого вида — для статичного того же вида.
    let mut animated: BTreeMap<&str, Vec<Option<String>>> = BTreeMap::new();
    for plan in plans(&username) {
        let prefix = format!("{}/", plan.kind);
        let icons: Vec<&ManifestIcon> = manifest
            .icons
            .iter()
            .filter(|i| i.key.starts_with(&prefix))
            .collect();
        eprintln!("{} — {} значков", plan.title, icons.len());
        let (pairs, keys) =
            install_pack(bot, owner, dir, &plan, &icons, animated.get(plan.kind)).await?;
        if plan.style == EmojiStyle::Animated {
            animated.insert(plan.kind, keys);
        }
        let pack = packs
            .of_mut(plan.style)
            .context("installable styles are animated and static")?;
        for (key, id) in pairs {
            pack.insert(key, id);
        }
        // Сохраняем после каждого набора: прерванная установка не теряет готовое.
        save(conn, PACKS_KEY, serde_json::to_value(&packs)?).await?;
        eprintln!("  готово: t.me/addemoji/{}", plan.name);
    }
    Ok(packs)
}

/// Папка с набором по умолчанию (`--dir` меняет): `assets/emoji` от рабочей папки.
pub fn default_dir() -> PathBuf {
    PathBuf::from("assets/emoji")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ж-2: стикер узнаётся по содержимому файла, а не по месту в наборе — значки, вставленные
    /// в середину манифеста, не сдвигают роли; чужой файл — без ключа.
    #[test]
    fn stickers_are_matched_to_icons_by_file_content() {
        let ours: std::collections::HashMap<Vec<u8>, String> = [
            (b"done".to_vec(), "color/done".to_owned()),
            (b"down".to_vec(), "color/down".to_owned()),
            (b"wait".to_vec(), "color/wait".to_owned()),
        ]
        .into_iter()
        .collect();
        let set = vec![b"wait".to_vec(), b"done".to_vec(), b"alien".to_vec()];
        assert_eq!(
            keys_by_content(&set, &ours),
            vec![
                Some("color/wait".to_owned()),
                Some("color/done".to_owned()),
                None
            ]
        );
    }

    #[test]
    fn set_names_follow_telegram_rules() {
        let plans = plans("@exch_ex_bot");
        assert_eq!(plans.len(), 4);
        for p in &plans {
            assert!(p.name.len() <= 64, "{}", p.name);
            assert!(!p.name.contains("__"), "{}", p.name);
            assert!(p.name.ends_with("_by_exch_ex_bot"), "{}", p.name);
            assert!(
                p.name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "{}",
                p.name
            );
            assert!(p.title.chars().count() <= 64);
            assert_eq!(p.repaint, p.kind == "mono");
        }
        let names: std::collections::BTreeSet<_> = plans.iter().map(|p| &p.name).collect();
        assert_eq!(names.len(), 4);
    }

    #[test]
    fn the_manifest_covers_every_role_and_has_both_files() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/emoji");
        let manifest = read_manifest(&dir).unwrap();
        assert_eq!(
            manifest.icons.len(),
            127,
            "+ стрелка «вниз» (8.11), пламя для текста (9.11), инвестиции — плитка и линия (13.11), \
             концепция «где деньги» — 5 плиток и «Ответить» (15.11)"
        );
        for icon in &manifest.icons {
            assert!(dir.join(&icon.animated).is_file(), "{}", icon.animated);
            assert!(dir.join(&icon.still).is_file(), "{}", icon.still);
            assert!(!icon.emoji.is_empty());
        }
        for kind in ["color/", "mono/"] {
            // В наборе Telegram — не больше 200 эмодзи.
            assert!(
                manifest
                    .icons
                    .iter()
                    .filter(|i| i.key.starts_with(kind))
                    .count()
                    <= 200
            );
        }
        for role in Emoji::ALL {
            for icon in [role.text_icon(), role.button_icon()] {
                assert!(manifest.icons.iter().any(|i| i.key == icon), "{icon}");
            }
        }
    }

    #[test]
    fn description_lists_roles_and_sources() {
        let mut s = EmojiSettings::default();
        s.custom.insert("ok".into(), "123".into());
        let text = describe(&s);
        assert!(text.contains("Вариация: animated"));
        assert!(text.contains("свой эмодзи 123"));
        assert!(text.contains("набор: color/wait / mono/wait"));
        assert!(parse_role("wiat").is_err());
        assert_eq!(parse_role("ok").unwrap(), Emoji::Ok);
    }
}
