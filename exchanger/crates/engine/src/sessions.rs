//! Чужие входы в аккаунты юзерботов (веха 10, пункт «наблюдение за аккаунтами»; утверждённый
//! макет «Новая авторизация»): раз в `sessions_every` движок читает сеансы каждого аккаунта
//! (Настройки → Устройства). Первый раз — запоминает, что есть (телефон владельца, сам exch).
//! Потом каждый новый сеанс, кроме самого exch, — критичная тревога «Новый вход» в пульт:
//! «Это я» закрывает её, «Завершить сессию» выкидывает чужое устройство.

use serde_json::json;
use storage::alerts::{Level, NewAlert, Topic};

use crate::{Engine, EngineError};

/// Ключ `settings` со списком известных сеансов аккаунта.
fn known_key(label: &str) -> String {
    format!("ub_sessions:{label}")
}

/// IP для тревоги: первая и последняя части, середина скрыта — как в макете «185.…41».
pub fn mask_ip(ip: &str) -> String {
    let parts: Vec<&str> = ip.split(['.', ':']).filter(|p| !p.is_empty()).collect();
    match (parts.first(), parts.last()) {
        (Some(first), Some(last)) if parts.len() > 2 => format!("{first}.…{last}"),
        _ => ip.to_owned(),
    }
}

impl Engine {
    /// Прочитать сеансы аккаунтов и поднять тревогу на каждый новый чужой вход.
    pub async fn watch_sessions(&self) -> Result<usize, EngineError> {
        let mut raised = 0;
        for (label, transport) in &self.accounts {
            let list = match transport.sessions().await {
                Ok(list) if !list.is_empty() => list,
                Ok(_) => continue,
                Err(e) => {
                    tracing::debug!(account = %label, error = %e, "sessions not read");
                    continue;
                }
            };
            let mut conn = self.pool.acquire().await?;
            let known: Option<Vec<String>> = storage::settings::get(&mut conn, &known_key(label))
                .await?
                .and_then(|v| serde_json::from_value(v).ok());
            if let Some(known) = &known {
                for s in list.iter().filter(|s| !s.current) {
                    let hash = s.hash.to_string();
                    if known.contains(&hash) {
                        continue;
                    }
                    let at = chrono::DateTime::from_timestamp(s.created, 0)
                        .unwrap_or_else(chrono::Utc::now);
                    storage::alerts::raise(
                        &mut conn,
                        &NewAlert {
                            level: Level::Critical,
                            topic: Topic::Security,
                            dedup_key: &format!("new_login:{label}:{hash}"),
                            title: "new_login",
                            payload: json!({
                                "account": label,
                                "hash": hash,
                                "device": s.device,
                                "app": s.app,
                                "country": s.country,
                                "ip": mask_ip(&s.ip),
                                "at": at.to_rfc3339(),
                            }),
                        },
                    )
                    .await?;
                    tracing::error!(account = %label, device = %s.device, "new login to a userbot account: staff alerted");
                    raised += 1;
                }
            }
            let now: Vec<String> = list.iter().map(|s| s.hash.to_string()).collect();
            storage::settings::set(&mut conn, &known_key(label), &json!(now), None).await?;
        }
        Ok(raised)
    }

    /// «Завершить сессию» из тревоги «Новый вход»: выкинуть чужое устройство.
    pub async fn end_session(&self, label: &str, hash: i64) -> Result<(), EngineError> {
        let (_, transport) = self
            .accounts
            .iter()
            .find(|(l, _)| l == label)
            .ok_or_else(|| EngineError::Invariant(format!("no userbot account {label}")))?;
        transport
            .end_session(hash)
            .await
            .map_err(|e| EngineError::Invariant(format!("session not ended: {e}")))?;
        tracing::warn!(account = %label, "foreign session ended by staff");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_is_masked_like_the_mockup() {
        assert_eq!(mask_ip("185.12.34.41"), "185.…41");
        assert_eq!(mask_ip("2a01:4f8:0:1::41"), "2a01.…41");
        assert_eq!(mask_ip("10.0"), "10.0");
    }
}
