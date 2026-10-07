//! Помощник входа (`mtproto::login`) против поддельного сервера: телефон → код, повтор кода,
//! уже рабочая сессия, недействительная сессия, регистрация. Путь с паролем 2FA (SRP) здесь
//! не проверяется: поддельный сервер не умеет SRP — только живой прогон `exch login`.

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use grammers_tl_types as tl;
use mtproto::session::{EncryptedSession, SessionStore};
use mtproto::{GrammersTransport, LoginError, LoginPrompt, login};
use secrecy::SecretString;
use support::fake_tg::{FakeTelegram, Reply, Request};
use support::*;

const PHONE: &str = "+31612345642";

#[derive(Default)]
struct Scripted {
    codes: VecDeque<&'static str>,
    code_attempts: Vec<u32>,
    phones_asked: u32,
    notices: Vec<String>,
}

#[async_trait]
impl LoginPrompt for Scripted {
    async fn phone(&mut self) -> Result<SecretString, LoginError> {
        self.phones_asked += 1;
        Ok(SecretString::from(PHONE))
    }

    async fn code(&mut self, attempt: u32) -> Result<SecretString, LoginError> {
        self.code_attempts.push(attempt);
        self.codes
            .pop_front()
            .map(SecretString::from)
            .ok_or_else(|| LoginError::Prompt("no more codes".into()))
    }

    async fn password(&mut self, _: Option<&str>, _: u32) -> Result<SecretString, LoginError> {
        Err(LoginError::Prompt("2FA is not scripted".into()))
    }

    async fn notice(&mut self, message: &str) {
        self.notices.push(message.to_owned());
    }
}

fn sent_code() -> Reply {
    let sent: tl::enums::auth::SentCode = tl::types::auth::SentCode {
        r#type: tl::types::auth::SentCodeTypeApp { length: 5 }.into(),
        phone_code_hash: "phone-code-hash".into(),
        next_type: None,
        timeout: None,
    }
    .into();
    Reply::ok(&sent)
}

fn authorization(user_id: i64) -> Reply {
    let auth: tl::enums::auth::Authorization = tl::types::auth::Authorization {
        setup_password_required: false,
        otherwise_relogin_days: None,
        tmp_sessions: None,
        future_auth_token: None,
        user: user(user_id, 5, false, Some("exch_test")),
    }
    .into();
    Reply::ok(&auth)
}

/// Сервер входа: `sendCode`, `signIn` с правильным кодом `22222`, остальное — `bootstrap`.
fn login_server(req: &Request) -> Option<Reply> {
    if let Some(r) = req.parse::<tl::functions::auth::SendCode>() {
        assert_eq!(r.phone_number, PHONE);
        assert_eq!(r.api_id, 4242);
        assert_eq!(r.api_hash, "api-hash");
        return Some(sent_code());
    }
    if let Some(r) = req.parse::<tl::functions::auth::SignIn>() {
        assert_eq!(r.phone_code_hash, "phone-code-hash");
        return Some(match r.phone_code.as_deref() {
            Some("22222") => authorization(SELF_ID),
            _ => Reply::error(400, "PHONE_CODE_INVALID"),
        });
    }
    None
}

async fn fake(mut handler: impl FnMut(&Request) -> Option<Reply> + Send + 'static) -> FakeTelegram {
    FakeTelegram::start(move |req| {
        handler(req)
            .or_else(|| bootstrap(req))
            .unwrap_or_else(|| Reply::error(400, "METHOD_NOT_EXPECTED_IN_TEST"))
    })
    .await
}

#[tokio::test]
async fn login_with_code_retry_saves_a_working_session() {
    let fake = fake(login_server).await;
    let store = store_without_login(&fake);
    let mut prompt = Scripted {
        codes: VecDeque::from(["11111", "22222"]),
        ..Scripted::default()
    };
    let outcome = login(&config(), &key(), Arc::clone(&store) as _, &mut prompt)
        .await
        .unwrap();
    assert_eq!(outcome.user_id, SELF_ID);
    assert_eq!(outcome.username.as_deref(), Some("exch_test"));
    assert_eq!(outcome.phone_masked, "+31*******42");
    assert!(!outcome.already_authorized);
    assert_eq!(prompt.code_attempts, vec![1, 2]);
    assert!(prompt.notices.iter().any(|n| n.contains("Неверный код")));
    let codes: Vec<Option<String>> = fake
        .parsed::<tl::functions::auth::SignIn>()
        .into_iter()
        .map(|r| r.phone_code)
        .collect();
    assert_eq!(codes, vec![Some("11111".into()), Some("22222".into())]);

    // Сохранено ровно то, что вернул вход, и только в зашифрованном виде.
    let sealed = store.load().await.unwrap().unwrap();
    assert_eq!(sealed, outcome.sealed);
    assert!(
        !sealed
            .ciphertext
            .windows(PHONE.len())
            .any(|w| w == PHONE.as_bytes())
    );
    let session = EncryptedSession::open(&key(), ACCOUNT, &sealed, [CB, XR]).unwrap();
    assert_eq!(session.self_user().map(|u| u.id), Some(SELF_ID));

    // С этой сессией транспорт подключается (боты — через resolve по username из конфига).
    let transport = GrammersTransport::connect(config(), key(), store as _)
        .await
        .unwrap();
    assert_eq!(transport.self_id(), SELF_ID);
    transport.shutdown().await;
}

#[tokio::test]
async fn working_session_needs_no_login() {
    let fake = fake(login_server).await;
    let store = store_for(&fake, true);
    let mut prompt = Scripted::default();
    let outcome = login(&config(), &key(), store, &mut prompt).await.unwrap();
    assert!(outcome.already_authorized);
    assert_eq!(outcome.user_id, SELF_ID);
    assert_eq!(prompt.phones_asked, 0);
    assert_eq!(fake.count::<tl::functions::auth::SendCode>(), 0);
}

#[tokio::test]
async fn revoked_session_is_replaced_by_a_new_login() {
    const NEW_ID: i64 = 888_000;
    let state_calls = Arc::new(Mutex::new(0u32));
    let calls = Arc::clone(&state_calls);
    let fake = fake(move |req| {
        if req.is::<tl::functions::updates::GetState>() {
            let mut n = calls.lock().unwrap();
            *n += 1;
            if *n == 1 {
                return Some(Reply::error(401, "AUTH_KEY_UNREGISTERED"));
            }
        }
        if req.is::<tl::functions::auth::SignIn>() {
            return Some(authorization(NEW_ID));
        }
        if let Some(r) = req.parse::<tl::functions::users::GetUsers>()
            && matches!(r.id.first(), Some(tl::enums::InputUser::UserSelf))
        {
            return Some(Reply::ok(&vec![user(NEW_ID, 9, false, None)]));
        }
        login_server(req)
    })
    .await;
    let store = store_for(&fake, true);
    let mut prompt = Scripted {
        codes: VecDeque::from(["33333"]),
        ..Scripted::default()
    };
    let outcome = login(&config(), &key(), Arc::clone(&store) as _, &mut prompt)
        .await
        .unwrap();
    assert!(!outcome.already_authorized);
    assert_eq!(outcome.user_id, NEW_ID);
    let sealed = store.load().await.unwrap().unwrap();
    let session = EncryptedSession::open(&key(), ACCOUNT, &sealed, [CB, XR]).unwrap();
    let snapshot = session.snapshot().unwrap();
    let selves: Vec<i64> = snapshot
        .users
        .values()
        .filter(|u| u.is_self == Some(true))
        .map(|u| u.id)
        .collect();
    assert_eq!(selves, vec![NEW_ID], "the old account is forgotten");
}

#[tokio::test]
async fn sign_up_is_required_for_unknown_numbers() {
    let fake = fake(|req| {
        req.is::<tl::functions::auth::SignIn>()
            .then(|| {
                let auth: tl::enums::auth::Authorization =
                    tl::types::auth::AuthorizationSignUpRequired {
                        terms_of_service: None,
                    }
                    .into();
                Reply::ok(&auth)
            })
            .or_else(|| login_server(req))
    })
    .await;
    let store = store_without_login(&fake);
    let mut prompt = Scripted {
        codes: VecDeque::from(["22222"]),
        ..Scripted::default()
    };
    let err = login(&config(), &key(), Arc::clone(&store) as _, &mut prompt)
        .await
        .unwrap_err();
    assert!(matches!(err, LoginError::SignUpRequired), "{err:?}");
    // Неудачный вход ничего не записал поверх прежней сессии.
    assert_eq!(store.save_count(), 0);
}

#[tokio::test]
async fn phone_rejected_by_telegram() {
    let fake = fake(|req| {
        req.is::<tl::functions::auth::SendCode>()
            .then(|| Reply::error(400, "PHONE_NUMBER_INVALID"))
    })
    .await;
    let mut prompt = Scripted::default();
    let err = login(&config(), &key(), store_without_login(&fake), &mut prompt)
        .await
        .unwrap_err();
    match err {
        LoginError::Rpc { what, error } => {
            assert_eq!(what, "auth.sendCode");
            assert!(error.to_string().contains("PHONE_NUMBER_INVALID"));
        }
        other => panic!("unexpected {other:?}"),
    }
}
