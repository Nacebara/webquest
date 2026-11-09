//! Рассылка «🔥 Выгодно» клиентам (решение владельца 9 ноября 2026).
//!
//! Движок заводит рассылку в журнале (`storage::promo`), когда на направлении включился
//! выгодный курс и рассылка включена в «Ребалансировке». Здесь она уходит: по клиенту за раз,
//! с паузой — не больше 20 сообщений в секунду (у Telegram — 30). Клиент помечается до
//! отправки, поэтому после перезапуска второй раз сообщение не придёт; кому писали меньше
//! 3 часов назад — пропускается. Выгодный курс выключился, пока шла рассылка, — остаток не
//! рассылаем: сообщение говорило бы неправду.

use std::sync::Arc;
use std::time::Duration;

use domain::Direction;
use tokio_util::sync::CancellationToken;

use crate::{Inner, TgBotError};

/// Как часто проверять, не завелась ли рассылка.
const EVERY: Duration = Duration::from_secs(5);
/// Пауза между сообщениями: не больше 20 в секунду.
const GAP: Duration = Duration::from_millis(50);
/// Клиентов за один заход (потом — снова свежие условия).
const BATCH: usize = 200;

pub(crate) async fn run(inner: Arc<Inner>, cancel: CancellationToken) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(EVERY) => {}
        }
        if let Err(e) = tick(&inner, &cancel).await {
            tracing::warn!(error = %e, "promo broadcast step failed");
        }
    }
}

/// Один заход: до [`BATCH`] клиентов идущей рассылки.
pub(crate) async fn tick(inner: &Arc<Inner>, cancel: &CancellationToken) -> Result<(), TgBotError> {
    let broadcast = {
        let mut conn = inner.pool.acquire().await?;
        storage::promo::running(&mut conn).await?
    };
    let Some(b) = broadcast else {
        return Ok(());
    };
    // Свежие условия: выгодного курса уже нет — рассылку закрываем.
    let terms = inner.terms().await?;
    let screen = b
        .direction
        .parse::<Direction>()
        .ok()
        .and_then(|d| inner.ui().promo_broadcast(&terms.terms, d));
    let Some(screen) = screen else {
        let mut conn = inner.pool.acquire().await?;
        storage::promo::finish(&mut conn, b.id).await?;
        tracing::info!(broadcast = b.id, "promo is off, broadcast stopped");
        return Ok(());
    };
    for _ in 0..BATCH {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let next = {
            let mut conn = inner.pool.acquire().await?;
            storage::promo::claim_next(&mut conn, b.id).await?
        };
        let Some((user, fresh)) = next else {
            let mut conn = inner.pool.acquire().await?;
            storage::promo::finish(&mut conn, b.id).await?;
            tracing::info!(broadcast = b.id, "promo broadcast finished");
            return Ok(());
        };
        if !fresh {
            continue;
        }
        // Бот работает только в личке: чат клиента = его id.
        let sent = inner.send_flow(user, user, &screen).await;
        if let Err(e) = &sent
            && !e.is_forbidden()
        {
            tracing::debug!(user, error = %e, "promo message not delivered");
        }
        let mut conn = inner.pool.acquire().await?;
        storage::promo::count(&mut conn, b.id, sent.is_ok()).await?;
        drop(conn);
        tokio::time::sleep(GAP).await;
    }
    Ok(())
}
