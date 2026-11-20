//! Критичные тревоги о кошельках и аккаунтах юзерботов (шаг 2.8 плана, находка Ю-3; макеты
//! «Вход сброшен», «Незнакомый ответ» и новые от 14 ноября 2026 — «аккаунт ограничен», «Telegram
//! просит паузу»). Раньше о беде с аккаунтом персонал узнавал косвенно — по заявке в разборе.
//!
//! Тревоги — состояния ([`storage::alerts::raise_condition`]): «Принял» гасит их, пока аккаунт
//! снова не заработает. Первый ответ кошелька через этот аккаунт закрывает их сами.

use std::time::Duration;

use chrono::Utc;
use domain::Platform;
use serde_json::json;
use sqlx::PgConnection;
use storage::alerts::{Level, NewAlert, Topic};
use storage::operations::OperationRow;
use userbot::BotError;

use crate::EngineError;

/// Короче этого паузу Telegram бот пережидает молча.
pub const LONG_FLOOD: Duration = Duration::from_secs(5 * 60);

/// Аккаунт юзербота операции: метка (`ub-1`), id и платформы его активных кошельков.
async fn account_of(
    conn: &mut PgConnection,
    op: &OperationRow,
) -> Result<Option<(String, i16, Vec<Platform>)>, EngineError> {
    let wallets = storage::wallets::list(&mut *conn).await?;
    let Some(own) = wallets
        .iter()
        .find(|w| Some(w.wallet_id) == op.wallet_account_id)
        .or_else(|| {
            wallets
                .iter()
                .find(|w| w.is_active && w.platform == op.platform)
        })
    else {
        return Ok(None);
    };
    let platforms = wallets
        .iter()
        .filter(|w| w.is_active && w.userbot_id == own.userbot_id)
        .map(|w| w.platform)
        .collect();
    Ok(Some((own.userbot_label.clone(), own.userbot_id, platforms)))
}

fn platforms_json(p: &[Platform]) -> serde_json::Value {
    json!(p.iter().map(|p| p.db_name()).collect::<Vec<_>>())
}

/// Кошелёк не дал аккаунту действовать: вход сброшен, аккаунт ограничен или Telegram просит
/// долгую паузу. Остальные ошибки — не тревога.
pub(crate) async fn on_bot_error(
    conn: &mut PgConnection,
    op: &OperationRow,
    e: &BotError,
) -> Result<(), EngineError> {
    let (title, extra) = match e {
        BotError::NotAuthorized => ("login_lost", json!({})),
        BotError::AccountRestricted => ("account_restricted", json!({})),
        BotError::FloodWait(d) if *d >= LONG_FLOOD => {
            let until = Utc::now() + chrono::Duration::from_std(*d).unwrap_or_default();
            (
                "flood_long",
                json!({ "wait_secs": d.as_secs(), "until": until.to_rfc3339() }),
            )
        }
        _ => return Ok(()),
    };
    let Some((label, userbot_id, platforms)) = account_of(&mut *conn, op).await? else {
        return Ok(());
    };
    let mut payload = json!({
        "account": label,
        "userbot_id": userbot_id,
        "platforms": platforms_json(&platforms),
    });
    if let (Some(p), Some(x)) = (payload.as_object_mut(), extra.as_object()) {
        p.extend(x.clone());
    }
    if title == "login_lost" {
        storage::sessions::mark_logged_out(&mut *conn, &label).await?;
    }
    let key = format!("{title}:{label}");
    storage::alerts::raise_condition(
        conn,
        &NewAlert {
            level: Level::Critical,
            topic: Topic::Errors,
            dedup_key: &key,
            title,
            payload,
        },
    )
    .await?;
    tracing::error!(account = %label, title, "wallet account cannot act: staff alerted");
    Ok(())
}

/// Кошелёк ответил через аккаунт операции: вход, ограничение и долгая пауза позади.
pub(crate) async fn on_answer(
    conn: &mut PgConnection,
    op: &OperationRow,
) -> Result<(), EngineError> {
    let Some((label, _, _)) = account_of(&mut *conn, op).await? else {
        return Ok(());
    };
    storage::sessions::mark_answering(&mut *conn, &label).await?;
    for title in ["login_lost", "account_restricted", "flood_long"] {
        storage::alerts::resolve(&mut *conn, &format!("{title}:{label}")).await?;
    }
    Ok(())
}

/// Незнакомый ответ кошелька на активацию: направление на автопаузе, заявка в разборе
/// (макет «Незнакомый ответ»). Своя тревога на каждый ответ: каждый надо посмотреть.
pub(crate) async fn unknown_reply(
    conn: &mut PgConnection,
    op: &OperationRow,
    text: &str,
    order_public: &str,
    direction: &str,
) -> Result<(), EngineError> {
    let key = format!("unknown_reply:{}:{}", op.platform.db_name(), op.id);
    storage::alerts::raise(
        conn,
        &NewAlert {
            level: Level::Critical,
            topic: Topic::Errors,
            dedup_key: &key,
            title: "unknown_reply",
            payload: json!({
                "platform": op.platform.db_name(),
                "text": mask(text),
                "order": op.order_id,
                "public": order_public,
                "direction": direction,
                "at": Utc::now().to_rfc3339(),
            }),
        },
    )
    .await?;
    Ok(())
}

/// Ответ кошелька в тревогу: длинные коды (чеки, счета) — начало и конец, не больше 300 знаков.
/// `CQ12…9F`: первые четыре и последние два знака кода.
fn short(code: &str) -> String {
    let head: String = code.chars().take(4).collect();
    let tail: String = code
        .chars()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

pub fn mask(text: &str) -> String {
    let masked: Vec<String> = text
        .split(' ')
        .map(|w| {
            // Ссылка на чек или счёт: код после `start=` — маской, хвост (знаки) — как есть (Д-19.6).
            if let Some((head, rest)) = w.split_once("start=") {
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || "_-".contains(c)))
                    .unwrap_or(rest.len());
                let (code, tail) = rest.split_at(end);
                if code.chars().count() > 8 {
                    return format!("{head}start={}{tail}", short(code));
                }
            }
            let alnum = w.chars().filter(char::is_ascii_alphanumeric).count();
            if alnum >= 12
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
            {
                short(w)
            } else {
                w.to_owned()
            }
        })
        .collect();
    let joined = masked.join(" ");
    match joined.char_indices().nth(300) {
        Some((i, _)) => format!("{}…", &joined[..i]),
        None => joined,
    }
}

/// Э-19.1 (проверка 19.11): аккаунт `label` не вошёл при запуске exch — сессии нет или Telegram
/// её отозвал. exch работает без него: аккаунт помечен «вход сброшен», направления его кошельков
/// на автопаузе ([`crate::LOGIN_LOST_PAUSE`]), персоналу — та же тревога «Вход сброшен», что и при
/// потере входа на ходу.
pub async fn account_not_connected(
    conn: &mut PgConnection,
    label: &str,
    userbot_id: i16,
    platforms: &[Platform],
) -> Result<(), EngineError> {
    storage::sessions::mark_logged_out(&mut *conn, label).await?;
    for platform in platforms {
        for side in [
            storage::directions::Side::Source,
            storage::directions::Side::Target,
        ] {
            storage::directions::auto_pause_platform(
                &mut *conn,
                *platform,
                side,
                crate::LOGIN_LOST_PAUSE,
            )
            .await?;
        }
    }
    let key = format!("login_lost:{label}");
    storage::alerts::raise_condition(
        conn,
        &NewAlert {
            level: Level::Critical,
            topic: Topic::Errors,
            dedup_key: &key,
            title: "login_lost",
            payload: json!({
                "account": label,
                "userbot_id": userbot_id,
                "platforms": platforms_json(platforms),
            }),
        },
    )
    .await?;
    tracing::error!(account = %label, "wallet account did not log in at start: it is skipped, its directions are paused");
    Ok(())
}

/// Аккаунт снова подключён при запуске: снять паузу «вход сброшен» с направлений его кошельков.
/// Тревогу закроет первый ответ кошелька ([`on_answer`]).
pub async fn account_connected(
    conn: &mut PgConnection,
    platforms: &[Platform],
) -> Result<(), EngineError> {
    for platform in platforms {
        storage::directions::clear_auto_pause_reason(
            &mut *conn,
            *platform,
            crate::LOGIN_LOST_PAUSE,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_in_wallet_replies_are_masked() {
        assert_eq!(
            mask("Чек CQ12ABCDEFGH9F временно недоступен"),
            "Чек CQ12…9F временно недоступен"
        );
        assert_eq!(mask("short text"), "short text");
        assert_eq!(mask(&"а".repeat(400)).chars().count(), 301);
        // Д-19.6 (проверка 19.11): ссылка на чек целиком — тоже маской (правило 16).
        assert_eq!(
            mask("Чек https://t.me/send?start=CQ12ABCDEFGH9F уже активирован."),
            "Чек https://t.me/send?start=CQ12…9F уже активирован."
        );
        assert_eq!(
            mask("Ссылка: t.me/xrocket?start=t_vF5RwLYE49f36gz,"),
            "Ссылка: t.me/xrocket?start=t_vF…gz,"
        );
    }
}
