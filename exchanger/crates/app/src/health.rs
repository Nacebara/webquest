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

/// Какие проверки молчат дольше `limit` (или ещё ни разу не отозвались). Пусто — всё живо.
/// Нет ни одной проверки — бот не работает (движок или бот не запущены): тоже «молчит».
pub fn silent(probes: &[Probe], now: DateTime<Utc>, limit: Duration) -> Vec<&'static str> {
    if probes.is_empty() {
        return vec!["nothing to watch: the engine or the bot is not running"];
    }
    let limit = chrono::Duration::from_std(limit).unwrap_or(chrono::Duration::MAX);
    probes
        .iter()
        .filter(|p| (p.last)().is_none_or(|t| now - t > limit))
        .map(|p| p.name)
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

    fn at(secs_ago: i64) -> Probe {
        Probe::new("engine", move || {
            Some(Utc::now() - chrono::Duration::seconds(secs_ago))
        })
    }

    #[test]
    fn fresh_loops_are_alive_silent_ones_are_named() {
        let now = Utc::now();
        assert!(silent(&[at(5), at(60)], now, STALE).is_empty());
        let stuck = Probe::new("telegram polling", move || {
            Some(Utc::now() - chrono::Duration::seconds(600))
        });
        assert_eq!(silent(&[at(5), stuck], now, STALE), ["telegram polling"]);
        let never = Probe::new("engine", || None);
        assert_eq!(silent(&[never], now, STALE), ["engine"]);
    }

    #[test]
    fn nothing_running_is_not_alive() {
        assert_eq!(silent(&[], Utc::now(), STALE).len(), 1);
    }
}
