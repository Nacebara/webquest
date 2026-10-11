//! Long polling `getUpdates` (вебхук — позже, HANDOVER §5 M4).
//!
//! - `offset` — в памяти: следующий запрос подтверждает Telegram всё обработанное;
//! - порция обновлений разбирается по клиентам: обновления одного клиента — строго по
//!   порядку, разные клиенты — параллельно; следующая порция — после всей текущей;
//! - каждое обновление — в своей задаче: ошибка или паника в нём попадает в лог, а бот
//!   продолжает (битое обновление Bot API отдаёт как пустое — оно просто сдвигает `offset`);
//! - сбой `getUpdates` — пауза (`retry_after` или нарастающая до 30 с) и снова;
//! - отмена — дорабатываем текущую порцию, подтверждаем её и выходим.

use std::sync::Arc;
use std::time::Duration;

use botapi::{GetUpdates, Update, UpdateKind};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::{Inner, handler};

const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Сколько ждать финального подтверждения порции при остановке.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn run(inner: Arc<Inner>, cancel: CancellationToken) {
    let mut last: Option<i64> = None;
    let mut backoff = Duration::from_secs(1);
    tracing::info!("client bot: polling for updates");
    loop {
        let request = GetUpdates::after(last, inner.cfg.poll_timeout_secs)
            .allowed(vec![UpdateKind::Message, UpdateKind::CallbackQuery]);
        let result = tokio::select! {
            () = cancel.cancelled() => break,
            result = inner.api.bot().get_updates(&request) => result,
        };
        match result {
            Ok(updates) => {
                backoff = Duration::from_secs(1);
                if let Some(max) = updates.iter().map(|u| u.update_id).max() {
                    last = Some(last.map_or(max, |l| l.max(max)));
                }
                if !updates.is_empty() {
                    process_batch(&inner, updates).await;
                }
            }
            Err(e) => {
                let wait = e.retry_after().unwrap_or(backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
                tracing::warn!(error = %e, wait_ms = wait.as_millis(), "getUpdates failed");
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
    // Подтвердить обработанное: обновления с id < offset Telegram больше не пришлёт. То, что
    // вернёт этот запрос, не подтверждено и придёт после рестарта.
    if let Some(last) = last {
        let confirm = GetUpdates {
            offset: Some(last.saturating_add(1)),
            limit: Some(1),
            timeout: Some(0),
            allowed_updates: None,
        };
        match tokio::time::timeout(CONFIRM_TIMEOUT, inner.api.bot().get_updates(&confirm)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "final getUpdates confirmation failed"),
            Err(_) => tracing::warn!("final getUpdates confirmation timed out"),
        }
    }
    tracing::info!("client bot: polling stopped");
}

/// Порция: по клиентам параллельно, у клиента — по порядку, каждое обновление изолировано.
async fn process_batch(inner: &Arc<Inner>, updates: Vec<Update>) {
    let mut groups: Vec<(Option<i64>, Vec<Update>)> = Vec::new();
    for update in updates {
        let user = update.from().map(|u| u.id);
        match groups
            .iter_mut()
            .find(|(key, _)| key.is_some() && *key == user)
        {
            Some((_, group)) => group.push(update),
            None => groups.push((user, vec![update])),
        }
    }
    let mut tasks = JoinSet::new();
    for (_, group) in groups {
        let inner = inner.clone();
        tasks.spawn(async move {
            for update in group {
                let id = update.update_id;
                let inner = inner.clone();
                let handled = tokio::spawn(async move { handler::handle(&inner, update).await });
                match handled.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(update = id, error = %e, "update handling failed"),
                    Err(e) => tracing::error!(update = id, error = %e, "update handler panicked"),
                }
            }
        });
    }
    while let Some(done) = tasks.join_next().await {
        if let Err(e) = done {
            tracing::error!(error = %e, "update group task crashed");
        }
    }
}
