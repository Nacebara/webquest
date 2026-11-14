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
pub fn mask(text: &str) -> String {
    let masked: Vec<String> = text
        .split(' ')
        .map(|w| {
            let alnum = w.chars().filter(char::is_ascii_alphanumeric).count();
            if alnum >= 12
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
            {
                let head: String = w.chars().take(4).collect();
                let tail: String = w
                    .chars()
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("{head}…{tail}")
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
    }
}
