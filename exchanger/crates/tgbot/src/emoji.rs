//! Премиум-эмодзи из настроек (`emoji_style`, `emoji_packs`, `custom_emoji`): чтение и подхват
//! изменений из пульта без перезапуска — раз в `settings_ttl` бот сверяет отпечаток настроек и,
//! если он сменился, пересобирает интерфейс с новым набором.

use std::collections::BTreeMap;
use std::sync::Arc;

use sqlx::PgConnection;
use tokio_util::sync::CancellationToken;
use ui::{EmojiPacks, EmojiSet, EmojiSettings, EmojiStyle};

use crate::Inner;

pub const STYLE_KEY: &str = "emoji_style";
pub const PACKS_KEY: &str = "emoji_packs";
pub const CUSTOM_KEY: &str = "custom_emoji";

#[derive(Debug, thiserror::Error)]
pub enum EmojiSettingsError {
    #[error(transparent)]
    Storage(#[from] storage::StorageError),
    #[error("{0}")]
    Malformed(String),
}

/// Настройки эмодзи и их отпечаток (сырые значения трёх ключей).
pub async fn load(conn: &mut PgConnection) -> Result<(EmojiSettings, String), EmojiSettingsError> {
    let style_raw = storage::settings::get(&mut *conn, STYLE_KEY).await?;
    let packs_raw = storage::settings::get(&mut *conn, PACKS_KEY).await?;
    let custom_raw = storage::settings::get(&mut *conn, CUSTOM_KEY).await?;
    let fingerprint = format!("{style_raw:?}|{packs_raw:?}|{custom_raw:?}");
    let style = match &style_raw {
        Some(v) => {
            let key = v.as_str().ok_or_else(|| {
                EmojiSettingsError::Malformed("emoji_style must be a string".into())
            })?;
            EmojiStyle::from_key(key).ok_or_else(|| {
                EmojiSettingsError::Malformed(format!("unknown emoji_style {key:?}"))
            })?
        }
        None => EmojiStyle::default(),
    };
    let packs: EmojiPacks = match packs_raw {
        Some(v) => serde_json::from_value(v)
            .map_err(|e| EmojiSettingsError::Malformed(format!("emoji_packs: {e}")))?,
        None => EmojiPacks::default(),
    };
    let custom: BTreeMap<String, String> = match custom_raw {
        Some(v) => serde_json::from_value(v)
            .map_err(|e| EmojiSettingsError::Malformed(format!("custom_emoji: {e}")))?,
        None => BTreeMap::new(),
    };
    Ok((
        EmojiSettings {
            style,
            packs,
            custom,
        },
        fingerprint,
    ))
}

/// Набор эмодзи из настроек; «off» — обычные символы.
pub fn set_from(settings: &EmojiSettings) -> Result<EmojiSet, ui::EmojiError> {
    EmojiSet::from_settings(settings.style != EmojiStyle::Off, settings)
}

pub(crate) async fn watch(inner: Arc<Inner>, stop: CancellationToken) {
    let mut tick = tokio::time::interval(inner.cfg.settings_ttl);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = tick.tick() => {}
        }
        let loaded = async {
            let mut conn = inner
                .pool
                .acquire()
                .await
                .map_err(storage::StorageError::from)?;
            load(&mut conn).await
        }
        .await;
        let (settings, fingerprint) = match loaded {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(error = %e, "emoji settings cannot be read, keeping the current set");
                continue;
            }
        };
        let changed = {
            let mut seen = match inner.emoji_seen.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            // Первый проход только запоминает: набор при старте собран из тех же настроек.
            let first = seen.is_none();
            let changed = seen.as_deref() != Some(fingerprint.as_str());
            *seen = Some(fingerprint);
            changed && !first
        };
        if !changed {
            continue;
        }
        match set_from(&settings) {
            Ok(set) => {
                let ui = inner.ui().with_emoji(set);
                inner.set_ui(ui);
                tracing::info!(style = settings.style.key(), "emoji settings reloaded");
            }
            Err(e) => {
                tracing::warn!(error = %e, "emoji settings are invalid, keeping the current set")
            }
        }
    }
}
