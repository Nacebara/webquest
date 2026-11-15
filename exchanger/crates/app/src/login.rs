//! `exch login --label <метка> --platform cryptobot|xrocket` (HANDOVER §5, M2.2).
//!
//! Порядок:
//! 1. [`prepare`] — проверить метку и что кошелёк платформы не привязан к другому аккаунту;
//!    достать сохранённую сессию метки (если она рабочая, вход не понадобится);
//! 2. вход ([`Authenticator`]; в бою — `mtproto::login` с `TerminalPrompt`) в хранилище
//!    **в памяти** (`MemoryStore`): до проверок ничего не пишется в БД;
//! 3. [`register`] — одной транзакцией: аккаунт с маской телефона, свой user id (метка уже
//!    привязана к другому аккаунту Telegram — отказ), зашифрованная сессия, статус `active`,
//!    кошелёк платформы.
//!
//! Один аккаунт Telegram может обслуживать обе платформы: второй `exch login` той же метки с
//! другой `--platform` возьмёт рабочую сессию без кода. Телефон, код и пароль 2FA — только
//! в `SecretString` внутри `mtproto`; в БД и логи попадает маска телефона.

use std::sync::Arc;

use async_trait::async_trait;
use domain::{Platform, wallet_label};
use mtproto::{
    LoginError, LoginOutcome, LoginPrompt, MemoryStore, MtprotoConfig, SealedSession, SessionKey,
    SessionStore,
};
use sqlx::PgConnection;
use storage::sessions::{self, UserbotAccount, UserbotStatus};
use storage::{PgPool, StorageError, wallets};

use crate::session_store::{from_stored, to_stored};

/// Какую метку и какой кошелёк подключаем.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginTarget {
    pub label: String,
    pub platform: Platform,
    /// Перенести кошелёк на этот аккаунт с другого (прежний заблокирован или вход в него
    /// потерян, веха 4.2). Учёт кошелька не меняется: остаток на прежнем аккаунте покажет сверка.
    pub move_wallet: bool,
}

impl LoginTarget {
    pub fn new(label: String, platform: Platform) -> Self {
        Self {
            label,
            platform,
            move_wallet: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoginCmdError {
    #[error("invalid userbot label {0:?}: use 1-32 characters a-z, 0-9, '-' or '_'")]
    BadLabel(String),
    #[error(
        "wallet {wallet} ({}) is already bound to userbot {bound}: switching the account of a wallet with money on it is a manual procedure (HANDOVER §8), nothing was changed",
        .platform.db_name()
    )]
    WalletBoundElsewhere {
        platform: Platform,
        wallet: &'static str,
        bound: String,
    },
    #[error(
        "wallet {} still has money in flight: {}; settle these orders in the panel first, nothing was changed",
        .platform.db_name(),
        .blockers.join(", ")
    )]
    WalletBusy {
        platform: Platform,
        blockers: Vec<String>,
    },
    #[error(
        "userbot {label} belongs to Telegram account {bound}, but this login is account {got}: use a new label for another account, nothing was saved"
    )]
    LabelBoundToOtherAccount { label: String, bound: i64, got: i64 },
    #[error(
        "Telegram account {tg_user_id} is already registered as userbot {other}: run `exch login --label {other}` instead, nothing was saved"
    )]
    AccountUnderOtherLabel { tg_user_id: i64, other: String },
    #[error("stored session of {label} is malformed: {error}")]
    BadStoredSession {
        label: String,
        error: mtproto::CryptoError,
    },
    #[error(transparent)]
    Login(#[from] LoginError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Метка — ключ строки в БД, часть AAD шифра сессии и поле логов: только простые символы.
pub fn validate_label(label: &str) -> Result<(), LoginCmdError> {
    let ok = (1..=32).contains(&label.len())
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(LoginCmdError::BadLabel(label.to_owned()))
    }
}

/// Что известно о метке до входа.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub existing: Option<UserbotAccount>,
    /// Сохранённая сессия: с неё начнётся вход.
    pub sealed: Option<SealedSession>,
}

/// Кошелёк свободен для этой метки. Возвращает прежний аккаунт, если кошелёк переносится.
async fn check_wallet_free(
    conn: &mut PgConnection,
    target: &LoginTarget,
) -> Result<Option<String>, LoginCmdError> {
    let Some(binding) = wallets::wallet_binding(&mut *conn, target.platform).await? else {
        return Ok(None);
    };
    if binding.userbot_label == target.label {
        return Ok(None);
    }
    if !target.move_wallet {
        return Err(LoginCmdError::WalletBoundElsewhere {
            platform: target.platform,
            wallet: wallet_label(target.platform),
            bound: binding.userbot_label,
        });
    }
    let blockers = wallets::move_blockers(conn, target.platform).await?;
    if !blockers.is_empty() {
        return Err(LoginCmdError::WalletBusy {
            platform: target.platform,
            blockers,
        });
    }
    Ok(Some(binding.userbot_label))
}

/// Шаг 1: проверки до сети.
pub async fn prepare(pool: &PgPool, target: &LoginTarget) -> Result<Prepared, LoginCmdError> {
    validate_label(&target.label)?;
    let mut conn = pool.acquire().await?;
    check_wallet_free(&mut conn, target).await?;
    let existing = sessions::find(&mut conn, &target.label).await?;
    let sealed = sessions::load_session(&mut conn, &target.label)
        .await?
        .map(from_stored)
        .transpose()
        .map_err(|error| LoginCmdError::BadStoredSession {
            label: target.label.clone(),
            error,
        })?;
    Ok(Prepared { existing, sealed })
}

/// Что записано в БД после входа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub label: String,
    pub userbot_id: i16,
    pub tg_user_id: i64,
    pub phone_masked: String,
    pub platform: Platform,
    pub wallet_id: i16,
    /// Сохранённая сессия была рабочей: код не понадобился.
    pub already_authorized: bool,
    /// Кошелёк перенесён с этого аккаунта (веха 4.2).
    pub moved_from: Option<String>,
}

/// Маска для БД: при входе по рабочей сессии телефон может быть неизвестен (`***`) — тогда
/// остаётся прежняя маска.
fn phone_mask<'a>(existing: Option<&'a UserbotAccount>, outcome: &'a LoginOutcome) -> &'a str {
    match existing {
        Some(acc) if outcome.phone_masked == "***" => &acc.phone_masked,
        _ => &outcome.phone_masked,
    }
}

/// Шаг 3: записать итог входа одной транзакцией. Любой отказ — без изменений в БД.
pub async fn register(
    pool: &PgPool,
    target: &LoginTarget,
    outcome: &LoginOutcome,
) -> Result<Registered, LoginCmdError> {
    validate_label(&target.label)?;
    let label = target.label.as_str();
    let mut tx = pool.begin().await?;
    // Входы по одному: проверка привязок и запись не должны перемежаться с другим входом.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('exch:login'))")
        .execute(&mut *tx)
        .await?;

    let accounts = sessions::list(&mut tx).await?;
    let existing = accounts.iter().find(|a| a.label == label);
    if let Some(bound) = existing.and_then(|a| a.tg_user_id)
        && bound != outcome.user_id
    {
        return Err(LoginCmdError::LabelBoundToOtherAccount {
            label: label.to_owned(),
            bound,
            got: outcome.user_id,
        });
    }
    if let Some(other) = accounts
        .iter()
        .find(|a| a.label != label && a.tg_user_id == Some(outcome.user_id))
    {
        return Err(LoginCmdError::AccountUnderOtherLabel {
            tg_user_id: outcome.user_id,
            other: other.label.clone(),
        });
    }
    let moved_from = check_wallet_free(&mut tx, target).await?;

    let phone_masked = phone_mask(existing, outcome).to_owned();
    let userbot_id = wallets::ensure_userbot(&mut tx, label, &phone_masked).await?;
    sessions::set_tg_user(&mut tx, label, outcome.user_id).await?;
    sessions::save_session(&mut tx, label, &to_stored(&outcome.sealed)).await?;
    sessions::set_status(&mut tx, label, UserbotStatus::Active).await?;
    let wallet_id = wallets::ensure_wallet(&mut tx, target.platform, userbot_id).await?;
    if let Some(old) = &moved_from {
        // Журнал пульта: перенос кошелька — решение владельца, оно должно быть видно.
        let owner: Option<i64> =
            sqlx::query_scalar("SELECT tg_user_id FROM staff WHERE role = 'owner' AND disabled_at IS NULL ORDER BY created_at LIMIT 1")
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(owner) = owner {
            storage::audit::record(
                &mut tx,
                &storage::audit::NewAudit {
                    staff_id: owner,
                    action: "wallet.move",
                    target_type: Some("wallet"),
                    target_id: Some(wallet_label(target.platform)),
                    before: Some(serde_json::json!({ "account": old })),
                    after: Some(serde_json::json!({ "account": label })),
                    reason: Some("exch setup: прежний аккаунт не работает"),
                    double_confirmed: true,
                },
            )
            .await?;
        }
    }
    tx.commit().await?;

    Ok(Registered {
        label: label.to_owned(),
        userbot_id,
        tg_user_id: outcome.user_id,
        phone_masked,
        platform: target.platform,
        wallet_id,
        already_authorized: outcome.already_authorized,
        moved_from,
    })
}

/// Сетевая часть входа. Начинает с сессии из `store` и сохраняет туда результат.
#[async_trait]
pub trait Authenticator: Send {
    async fn authenticate(&mut self, store: Arc<MemoryStore>) -> Result<LoginOutcome, LoginError>;
}

/// Настоящий вход через `mtproto::login`.
pub struct MtprotoAuthenticator<P: LoginPrompt> {
    pub cfg: MtprotoConfig,
    pub key: SessionKey,
    pub prompt: P,
}

#[async_trait]
impl<P: LoginPrompt> Authenticator for MtprotoAuthenticator<P> {
    async fn authenticate(&mut self, store: Arc<MemoryStore>) -> Result<LoginOutcome, LoginError> {
        mtproto::login(
            &self.cfg,
            &self.key,
            store as Arc<dyn SessionStore>,
            &mut self.prompt,
        )
        .await
    }
}

/// Вся команда: проверки → вход в памяти → запись в БД.
pub async fn run_login(
    pool: &PgPool,
    target: &LoginTarget,
    auth: &mut dyn Authenticator,
) -> Result<Registered, LoginCmdError> {
    let prepared = prepare(pool, target).await?;
    match &prepared.existing {
        Some(acc) => tracing::info!(
            account = %acc.label,
            status = acc.status.db_name(),
            has_session = acc.has_session,
            "userbot label exists: its saved session is tried first"
        ),
        None => tracing::info!(account = %target.label, "new userbot label"),
    }
    let store = Arc::new(MemoryStore::new(prepared.sealed));
    let outcome = auth.authenticate(store).await?;
    let registered = register(pool, target, &outcome).await?;
    tracing::info!(
        account = %registered.label,
        tg_user_id = registered.tg_user_id,
        phone = %registered.phone_masked,
        platform = registered.platform.db_name(),
        wallet = wallet_label(registered.platform),
        already_authorized = registered.already_authorized,
        "userbot logged in: session saved encrypted, wallet bound"
    );
    Ok(registered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_simple() {
        for ok in ["ub-cb-1", "ub_both", "a", "x".repeat(32).as_str()] {
            assert!(validate_label(ok).is_ok(), "{ok}");
        }
        for bad in ["", "UB-1", "ub 1", "ub/1", "юб", &"x".repeat(33)] {
            assert!(validate_label(bad).is_err(), "{bad}");
        }
    }
}
