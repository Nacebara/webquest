//! Пульс процесса и пинг наружу (находка Н-5, шаг 2.5 плана).
//!
//! Раз в минуту: база отвечает? — и, если задан адрес пинг-сервиса (`HEALTHCHECK_URL`,
//! например healthchecks.io), GET на него. Пинг уходит, **только когда движок и опрос Telegram
//! живы** — каждый недавно прошёл свой круг. Завис цикл, упал опрос, лежит база, процесс убит —
//! пинги прекращаются, и пинг-сервис сам пишет владельцу. Поэтому «процесс жив» здесь не
//! считается: важно, работает ли бот.

use std::time::Duration;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

/// Как часто проверять и пинговать.
pub const EVERY: Duration = Duration::from_secs(60);
/// Сколько может молчать цикл, прежде чем считаться зависшим. Опрос Telegram отвечает не
/// реже чем раз в таймаут long polling (до минуты), движок — раз в секунду между операциями.
pub const STALE: Duration = Duration::from_secs(180);
pub const ENGINE: &str = "engine";
pub const TELEGRAM_POLLING: &str = "telegram polling";

/// Что должно быть живо, чтобы пинговать: имя для журнала и «когда последний раз прошёл круг».
pub struct Probe {
    pub name: &'static str,
    pub last: Box<dyn Fn() -> Option<DateTime<Utc>> + Send + Sync>,
}

impl Probe {
    pub fn new(
        name: &'static str,
        last: impl Fn() -> Option<DateTime<Utc>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            last: Box::new(last),
        }
    }
}

/// Какие обязательные циклы не запущены, ещё не отозвались или молчат дольше `limit`.
pub fn silent(probes: &[Probe], now: DateTime<Utc>, limit: Duration) -> Vec<&'static str> {
    let limit = chrono::Duration::from_std(limit).unwrap_or(chrono::Duration::MAX);
    [ENGINE, TELEGRAM_POLLING]
        .into_iter()
        .filter(|name| {
            probes
                .iter()
                .find(|probe| probe.name == *name)
                .and_then(|probe| (probe.last)())
                .is_none_or(|last| now - last > limit)
        })
        .collect()
}

/// Пульс: база, проверки, пинг. База не отвечает — ошибка (супервизор подождёт её возврата, а
/// пинги тем временем не идут). Пинг-сервис не ответил — только предупреждение в журнал.
pub async fn run(
    pool: storage::PgPool,
    pinger: Option<std::sync::Arc<botapi::Pinger>>,
    probes: std::sync::Arc<Vec<Probe>>,
    every: Duration,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut tick = tokio::time::interval(every);
    // Первый круг — через `every`: движок и опрос успеют отозваться после старта.
    tick.tick().await;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            _ = tick.tick() => {}
        }
        let mut conn = pool
            .acquire()
            .await
            .context("heartbeat: no database connection")?;
        let balances = storage::ledger::wallet_balances(&mut conn).await?;
        drop(conn);
        let Some(pinger) = &pinger else {
            tracing::info!(wallets = balances.len(), "alive");
            continue;
        };
        let silent = silent(&probes, Utc::now(), STALE.min(every * 3));
        if !silent.is_empty() {
            tracing::warn!(
                ?silent,
                "not pinging the health check: the bot is not working"
            );
            continue;
        }
        match pinger.ping().await {
            Ok(()) => tracing::info!(wallets = balances.len(), "alive, health check pinged"),
            Err(e) => tracing::warn!(error = %e, "health check ping failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(name: &'static str, secs_ago: i64) -> Probe {
        Probe::new(name, move || {
            Some(Utc::now() - chrono::Duration::seconds(secs_ago))
        })
    }

    #[test]
    fn fresh_loops_are_alive_silent_ones_are_named() {
        let now = Utc::now();
        assert!(silent(&[at(ENGINE, 5), at(TELEGRAM_POLLING, 60)], now, STALE).is_empty());
        assert_eq!(
            silent(&[at(ENGINE, 5), at(TELEGRAM_POLLING, 600)], now, STALE),
            [TELEGRAM_POLLING]
        );
        let never = Probe::new(ENGINE, || None);
        assert_eq!(
            silent(&[never, at(TELEGRAM_POLLING, 5)], now, STALE),
            [ENGINE]
        );
    }

    #[test]
    fn nothing_running_is_not_alive() {
        assert_eq!(silent(&[], Utc::now(), STALE), [ENGINE, TELEGRAM_POLLING]);
    }
}
