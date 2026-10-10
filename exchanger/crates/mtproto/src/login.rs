//! Вход в аккаунт юзербота: телефон → код → пароль 2FA (порт lovec `account.rs:1085-1167`).
//! Для будущей команды `exch login`: сессия создаётся в памяти, шифруется и сохраняется в
//! [`SessionStore`]; открытый текст на диск не попадает.
//!
//! Телефон, код и пароль — `SecretString`, в логи не пишутся. Вызовы grammers идут в отдельных
//! задачах с таймаутом: в grammers 0.10 вход может запаниковать на неожиданном ответе сервера
//! (`unimplemented!` для `auth.sentCodePaymentRequired`, `panic!` на плохих параметрах SRP).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use grammers_client::client::{LoginToken, PasswordToken};
use grammers_client::{Client, SignInError};
use grammers_mtsender::InvocationError;
use secrecy::{ExposeSecret, SecretString};
use userbot::TransportError;
use zeroize::Zeroizing;

use crate::config::MtprotoConfig;
use crate::crypto::{SealedSession, SessionKey};
use crate::errors::map_invocation_error;
use crate::session::{EncryptedSession, SessionError, SessionSnapshot, SessionStore, StoredUser};
use crate::transport::Pool;

const MAX_CODE_ATTEMPTS: u32 = 3;
const MAX_PASSWORD_ATTEMPTS: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("invalid MTProto configuration: {0}")]
    InvalidConfig(String),
    /// Сохранённую сессию не удалось расшифровать — она не перезаписывается молча.
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("login input: {0}")]
    Prompt(String),
    #[error("Telegram rejected {what}: {error}")]
    Rpc {
        what: &'static str,
        error: TransportError,
    },
    #[error("login code is invalid")]
    InvalidCode,
    #[error("2FA password is invalid")]
    InvalidPassword,
    #[error("this phone number has no Telegram account: sign up with an official app first")]
    SignUpRequired,
    #[error("timed out during {0}")]
    Timeout(&'static str),
    #[error("{0}")]
    Internal(String),
}

impl LoginError {
    fn rpc(what: &'static str, e: &InvocationError) -> Self {
        Self::Rpc {
            what,
            error: map_invocation_error(e),
        }
    }
}

/// Откуда брать телефон, код и пароль (терминал, тест, ops-бот).
#[async_trait]
pub trait LoginPrompt: Send {
    async fn phone(&mut self) -> Result<SecretString, LoginError>;
    /// `attempt` — с 1.
    async fn code(&mut self, attempt: u32) -> Result<SecretString, LoginError>;
    async fn password(
        &mut self,
        hint: Option<&str>,
        attempt: u32,
    ) -> Result<SecretString, LoginError>;
    /// Сообщение человеку («код отправлен», «неверный код»).
    async fn notice(&mut self, message: &str);
}

/// Итог входа: что записать в `userbot_accounts`.
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    pub user_id: i64,
    pub username: Option<String>,
    /// `+31******42` — полный номер не храним.
    pub phone_masked: String,
    /// То же, что сохранено в хранилище.
    pub sealed: SealedSession,
    /// Сохранённая сессия уже была рабочей: вход не понадобился.
    pub already_authorized: bool,
}

/// Маска телефона для БД и логов: `+` и две первые и две последние цифры.
pub fn mask_phone(phone: &str) -> String {
    let digits: Vec<char> = phone.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 5 {
        return "***".to_owned();
    }
    let head: String = digits[..2].iter().collect();
    let tail: String = digits[digits.len() - 2..].iter().collect();
    format!("+{head}{}{tail}", "*".repeat(digits.len() - 4))
}

/// Вызов grammers в отдельной задаче: таймаут и перехват паники.
async fn guarded<T: Send + 'static>(
    what: &'static str,
    limit: Duration,
    fut: impl Future<Output = T> + Send + 'static,
) -> Result<T, LoginError> {
    let mut task = tokio::spawn(fut);
    match tokio::time::timeout(limit, &mut task).await {
        Err(_) => {
            task.abort();
            Err(LoginError::Timeout(what))
        }
        Ok(Err(e)) if e.is_panic() => Err(LoginError::Internal(format!(
            "{what}: unexpected server response (grammers panicked)"
        ))),
        Ok(Err(_)) => Err(LoginError::Internal(format!("{what}: task was cancelled"))),
        Ok(Ok(v)) => Ok(v),
    }
}

/// Войти в аккаунт `cfg.account` и сохранить зашифрованную сессию.
///
/// Если в хранилище уже есть рабочая сессия, вход не выполняется. Если сессия есть, но не
/// расшифровывается (другой ключ или аккаунт), — ошибка: её нельзя затирать без решения человека.
pub async fn login(
    cfg: &MtprotoConfig,
    key: &SessionKey,
    store: Arc<dyn SessionStore>,
    prompt: &mut dyn LoginPrompt,
) -> Result<LoginOutcome, LoginError> {
    cfg.validate().map_err(LoginError::InvalidConfig)?;
    let allowed: Vec<i64> = cfg.bots.iter().map(|b| b.id).collect();
    let session = match store.load().await.map_err(SessionError::from)? {
        Some(sealed) => EncryptedSession::open(key, &cfg.account, &sealed, allowed)?,
        None => EncryptedSession::new(SessionSnapshot::default(), allowed),
    };
    let session = Arc::new(session);
    let pool = Pool::start(cfg, Arc::clone(&session));
    let result = run_login(cfg, key, store.as_ref(), &session, &pool.action, prompt).await;
    Pool::stop(&pool.handle, pool.runner).await;
    result
}

async fn fetch_me(
    client: &Client,
    limit: Duration,
) -> Result<(StoredUser, Option<String>, Option<String>), LoginError> {
    let c = client.clone();
    let users = guarded("users.getUsers(self)", limit, async move {
        c.invoke(&grammers_tl_types::functions::users::GetUsers {
            id: vec![grammers_tl_types::enums::InputUser::UserSelf],
        })
        .await
    })
    .await?
    .map_err(|e| LoginError::rpc("users.getUsers(self)", &e))?;
    match users.into_iter().next() {
        Some(grammers_tl_types::enums::User::User(u)) => Ok((
            StoredUser {
                id: u.id,
                access_hash: u.access_hash,
                bot: Some(u.bot),
                is_self: Some(true),
            },
            u.username,
            u.phone,
        )),
        _ => Err(LoginError::Internal(
            "users.getUsers(self) returned no user".into(),
        )),
    }
}

async fn run_login(
    cfg: &MtprotoConfig,
    key: &SessionKey,
    store: &dyn SessionStore,
    session: &EncryptedSession,
    client: &Client,
    prompt: &mut dyn LoginPrompt,
) -> Result<LoginOutcome, LoginError> {
    let limit = cfg.timeouts.startup;
    let mut phone_masked = None;

    let mut already_authorized = false;
    if session.self_user().is_some() {
        let c = client.clone();
        match guarded(
            "updates.getState",
            limit,
            async move { c.is_authorized().await },
        )
        .await?
        {
            Ok(true) => already_authorized = true,
            Ok(false) => {
                session.forget_users()?;
                prompt
                    .notice("Сохранённая сессия больше не действует — нужен новый вход.")
                    .await;
            }
            Err(e) => return Err(LoginError::rpc("updates.getState", &e)),
        }
    }

    if !already_authorized {
        let phone = prompt.phone().await?;
        phone_masked = Some(mask_phone(phone.expose_secret()));
        let token: Arc<LoginToken> = {
            let c = client.clone();
            let phone = Zeroizing::new(phone.expose_secret().to_owned());
            let api_hash = Zeroizing::new(cfg.api_hash.expose_secret().to_owned());
            let token = guarded("auth.sendCode", limit, async move {
                c.request_login_code(&phone, &api_hash).await
            })
            .await?
            .map_err(|e| LoginError::rpc("auth.sendCode", &e))?;
            Arc::new(token)
        };
        prompt.notice("Код отправлен в Telegram.").await;

        let mut attempt = 0;
        loop {
            attempt += 1;
            let code = prompt.code(attempt).await?;
            let c = client.clone();
            let t = Arc::clone(&token);
            let code = Zeroizing::new(code.expose_secret().trim().to_owned());
            let result = guarded(
                "auth.signIn",
                limit,
                async move { c.sign_in(&t, &code).await },
            )
            .await?;
            match result {
                Ok(_) => break,
                Err(SignInError::InvalidCode) if attempt < MAX_CODE_ATTEMPTS => {
                    prompt.notice("Неверный код, попробуйте ещё раз.").await;
                }
                Err(SignInError::InvalidCode) => return Err(LoginError::InvalidCode),
                Err(SignInError::PasswordRequired(password_token)) => {
                    check_password(client, password_token, prompt, limit).await?;
                    break;
                }
                Err(SignInError::SignUpRequired) => return Err(LoginError::SignUpRequired),
                Err(SignInError::InvalidPassword(_)) => {
                    return Err(LoginError::Internal(
                        "unexpected password error on sign in".into(),
                    ));
                }
                Err(SignInError::Other(e)) => return Err(LoginError::rpc("auth.signIn", &e)),
            }
        }
    }

    let (me, username, phone) = fetch_me(client, limit).await?;
    session.remember_user(me)?;
    let sealed = session.seal(key, &cfg.account)?;
    store.save(&sealed).await.map_err(SessionError::from)?;
    tracing::info!(account = %cfg.account, user_id = me.id, already_authorized, "MTProto session saved");
    Ok(LoginOutcome {
        user_id: me.id,
        username,
        phone_masked: phone_masked
            .or_else(|| phone.as_deref().map(mask_phone))
            .unwrap_or_else(|| "***".to_owned()),
        sealed,
        already_authorized,
    })
}

async fn check_password(
    client: &Client,
    mut token: PasswordToken,
    prompt: &mut dyn LoginPrompt,
    limit: Duration,
) -> Result<(), LoginError> {
    for attempt in 1..=MAX_PASSWORD_ATTEMPTS {
        let hint = token.hint().map(str::to_owned);
        let password = prompt.password(hint.as_deref(), attempt).await?;
        let c = client.clone();
        let password = Zeroizing::new(password.expose_secret().as_bytes().to_vec());
        let result = guarded("auth.checkPassword", limit, async move {
            c.check_password(token, password.as_slice()).await
        })
        .await?;
        match result {
            Ok(_) => return Ok(()),
            Err(SignInError::InvalidPassword(next)) => {
                if attempt < MAX_PASSWORD_ATTEMPTS {
                    prompt.notice("Неверный пароль, попробуйте ещё раз.").await;
                }
                token = next;
            }
            Err(SignInError::Other(e)) => return Err(LoginError::rpc("auth.checkPassword", &e)),
            Err(_) => return Err(LoginError::Internal("unexpected sign-in state".into())),
        }
    }
    Err(LoginError::InvalidPassword)
}

/// Ввод в терминале: телефон и код — строкой, пароль — без эха.
#[derive(Debug, Default)]
pub struct TerminalPrompt;

async fn read_line(prompt: &'static str) -> Result<SecretString, LoginError> {
    tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        let mut err = std::io::stderr();
        let _ = write!(err, "{prompt}");
        let _ = err.flush();
        let mut line = Zeroizing::new(String::new());
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => Err(LoginError::Prompt("input closed".into())),
            Ok(_) => Ok(SecretString::from(line.trim().to_owned())),
            Err(e) => Err(LoginError::Prompt(e.to_string())),
        }
    })
    .await
    .map_err(|e| LoginError::Prompt(e.to_string()))?
}

#[async_trait]
impl LoginPrompt for TerminalPrompt {
    async fn phone(&mut self) -> Result<SecretString, LoginError> {
        read_line("Телефон аккаунта в международном формате (+…): ").await
    }

    async fn code(&mut self, _attempt: u32) -> Result<SecretString, LoginError> {
        read_line("Код из Telegram: ").await
    }

    async fn password(
        &mut self,
        hint: Option<&str>,
        _attempt: u32,
    ) -> Result<SecretString, LoginError> {
        let prompt = match hint {
            Some(h) if !h.is_empty() => format!("Пароль 2FA (подсказка: {h}): "),
            _ => "Пароль 2FA: ".to_owned(),
        };
        tokio::task::spawn_blocking(move || {
            rpassword::prompt_password(prompt)
                .map(|p| {
                    let p = Zeroizing::new(p);
                    SecretString::from(p.as_str().to_owned())
                })
                .map_err(|e| LoginError::Prompt(e.to_string()))
        })
        .await
        .map_err(|e| LoginError::Prompt(e.to_string()))?
    }

    async fn notice(&mut self, message: &str) {
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::seal;
    use crate::session::MemoryStore;

    #[test]
    fn phone_is_masked() {
        assert_eq!(mask_phone("+31 6 1234 5642"), "+31*******42");
        assert_eq!(mask_phone("79991234567"), "+79*******67");
        assert_eq!(mask_phone("+1234"), "***");
        assert!(!mask_phone("+31612345642").contains("612345"));
    }

    struct NoInput;

    #[async_trait]
    impl LoginPrompt for NoInput {
        async fn phone(&mut self) -> Result<SecretString, LoginError> {
            Err(LoginError::Prompt("no input in tests".into()))
        }
        async fn code(&mut self, _: u32) -> Result<SecretString, LoginError> {
            Err(LoginError::Prompt("no input in tests".into()))
        }
        async fn password(&mut self, _: Option<&str>, _: u32) -> Result<SecretString, LoginError> {
            Err(LoginError::Prompt("no input in tests".into()))
        }
        async fn notice(&mut self, _: &str) {}
    }

    #[tokio::test]
    async fn undecryptable_session_is_never_overwritten() {
        let cfg = MtprotoConfig::new("ub-1", 1, SecretString::from("hash"));
        let other_key = SessionKey::from_bytes(1, &[9; 32]);
        let sealed = seal(&other_key, "ub-1", b"whatever").unwrap();
        let store = Arc::new(MemoryStore::new(Some(sealed.clone())));
        let key = SessionKey::from_bytes(1, &[1; 32]);
        let err = login(
            &cfg,
            &key,
            Arc::clone(&store) as Arc<dyn SessionStore>,
            &mut NoInput,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, LoginError::Session(SessionError::Crypto(_))),
            "{err:?}"
        );
        assert_eq!(store.get(), Some(sealed));
        assert_eq!(store.save_count(), 0);
    }

    #[tokio::test]
    async fn invalid_config_is_rejected_before_network() {
        let cfg = MtprotoConfig::new("", 1, SecretString::from("hash"));
        let store = Arc::new(MemoryStore::default());
        let key = SessionKey::from_bytes(1, &[1; 32]);
        assert!(matches!(
            login(&cfg, &key, store, &mut NoInput).await,
            Err(LoginError::InvalidConfig(_))
        ));
    }
}
