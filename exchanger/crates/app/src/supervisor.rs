//! Супервизор задач (SPEC §10.1): упавшая задача перезапускается с backoff;
//! больше 5 перезапусков за 10 минут — супервизор сдаётся, процесс выходит с кодом 1,
//! служба Windows перезапускает его, рекавери доводит заявки.
//!
//! Сбой базы или сети ([`crate::startup::transient`]) к «сдаться» не считается (находка Н-4,
//! шаг 2.4): минута без PostgreSQL не должна выключать exch до прихода человека. Такая задача
//! перезапускается с паузой 1, 2, 4 … 60 с, пока база и сеть не вернутся.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Когда задача последний раз перезапускалась после временного сбоя (секунды Unix). Сторож
/// зависания (шаг 2.7) по этому видит, что задача не зависла, а пережидает базу или сеть.
static TRANSIENT_RETRY: LazyLock<Mutex<HashMap<&'static str, i64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn note_transient_retry(task: &'static str) {
    if let Ok(mut m) = TRANSIENT_RETRY.lock() {
        m.insert(task, chrono::Utc::now().timestamp());
    }
}

/// Когда задача `task` последний раз пережидала сбой базы или сети; `None` — не пережидала.
pub fn last_transient_retry(task: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let t = *TRANSIENT_RETRY.lock().ok()?.get(task)?;
    chrono::DateTime::from_timestamp(t, 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    pub max_restarts: usize,
    pub window: Duration,
    pub min_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            window: Duration::from_secs(600),
            min_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Restart { after: Duration },
    GiveUp,
}

/// Учёт перезапусков в скользящем окне. Чистая логика — тестируется без рантайма.
#[derive(Debug)]
pub struct RestartTracker {
    policy: RestartPolicy,
    restarts: VecDeque<Instant>,
}

impl RestartTracker {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            restarts: VecDeque::new(),
        }
    }

    /// Задача завершилась аварийно в момент `now`: перезапускать ли и через сколько.
    pub fn on_failure(&mut self, now: Instant) -> Decision {
        while let Some(&first) = self.restarts.front() {
            if now.duration_since(first) > self.policy.window {
                self.restarts.pop_front();
            } else {
                break;
            }
        }
        if self.restarts.len() >= self.policy.max_restarts {
            return Decision::GiveUp;
        }
        let exp = u32::try_from(self.restarts.len())
            .unwrap_or(u32::MAX)
            .min(16);
        let after = self
            .policy
            .min_backoff
            .saturating_mul(1u32 << exp)
            .min(self.policy.max_backoff);
        self.restarts.push_back(now);
        Decision::Restart { after }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("task {task} kept failing: more than {max} restarts within {window:?}")]
pub struct GaveUp {
    pub task: &'static str,
    pub max: usize,
    pub window: Duration,
}

/// Запускать задачу, пока не отменён `cancel`. Задача получает дочерний токен и должна
/// завершиться `Ok(())` после его отмены. Паника и `Err` считаются падением.
pub async fn supervise<F, Fut>(
    name: &'static str,
    cancel: CancellationToken,
    policy: RestartPolicy,
    mut make: F,
) -> Result<(), GaveUp>
where
    F: FnMut(CancellationToken) -> Fut,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let mut tracker = RestartTracker::new(policy);
    // Подряд идущие временные сбои (база, сеть): пауза растёт, к «сдаться» не считаются.
    let mut transient_streak = 0u32;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let started = Instant::now();
        let outcome = tokio::spawn(make(cancel.child_token())).await;
        if cancel.is_cancelled() {
            return Ok(());
        }
        // Задача проработала дольше самой длинной паузы — прежние сбои закончились.
        if started.elapsed() >= policy.max_backoff {
            transient_streak = 0;
        }
        if let Ok(Err(e)) = &outcome
            && crate::startup::transient(e)
        {
            let after = policy
                .min_backoff
                .saturating_mul(1u32 << transient_streak.min(16))
                .min(policy.max_backoff);
            transient_streak = transient_streak.saturating_add(1);
            tracing::error!(
                task = name,
                error = %e,
                failures = transient_streak,
                delay_ms = after.as_millis() as u64,
                "task failed: the database or the network is unavailable; waiting, not giving up"
            );
            note_transient_retry(name);
            tokio::select! {
                () = tokio::time::sleep(after) => {}
                () = cancel.cancelled() => return Ok(()),
            }
            note_transient_retry(name);
            continue;
        }
        match &outcome {
            Ok(Ok(())) => tracing::error!(task = name, "task exited without cancellation"),
            Ok(Err(e)) => tracing::error!(task = name, error = %e, "task failed"),
            Err(join) => tracing::error!(task = name, error = %join, "task panicked"),
        }
        match tracker.on_failure(Instant::now()) {
            Decision::Restart { after } => {
                tracing::warn!(
                    task = name,
                    delay_ms = after.as_millis() as u64,
                    "restarting task"
                );
                tokio::select! {
                    () = tokio::time::sleep(after) => {}
                    () = cancel.cancelled() => return Ok(()),
                }
            }
            Decision::GiveUp => {
                return Err(GaveUp {
                    task: name,
                    max: policy.max_restarts,
                    window: policy.window,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn backoff_doubles_and_caps_then_gives_up() {
        let mut t = RestartTracker::new(RestartPolicy::default());
        let start = Instant::now();
        let delays: Vec<Decision> = (0..6)
            .map(|i| t.on_failure(start + Duration::from_secs(i)))
            .collect();
        assert_eq!(
            delays,
            vec![
                Decision::Restart {
                    after: Duration::from_secs(1)
                },
                Decision::Restart {
                    after: Duration::from_secs(2)
                },
                Decision::Restart {
                    after: Duration::from_secs(4)
                },
                Decision::Restart {
                    after: Duration::from_secs(8)
                },
                Decision::Restart {
                    after: Duration::from_secs(16)
                },
                Decision::GiveUp,
            ]
        );
    }

    #[test]
    fn old_failures_leave_the_window() {
        let policy = RestartPolicy {
            max_restarts: 2,
            ..RestartPolicy::default()
        };
        let mut t = RestartTracker::new(policy);
        let start = Instant::now();
        assert!(matches!(t.on_failure(start), Decision::Restart { .. }));
        assert!(matches!(
            t.on_failure(start + Duration::from_secs(1)),
            Decision::Restart { .. }
        ));
        assert_eq!(
            t.on_failure(start + Duration::from_secs(2)),
            Decision::GiveUp
        );
        // Через 11 минут прежние падения вышли из окна.
        assert_eq!(
            t.on_failure(start + Duration::from_secs(661)),
            Decision::Restart {
                after: Duration::from_secs(1)
            }
        );
    }

    /// Шаг 2.4 (Н-4): минута без базы — задача падает снова и снова, супервизор не сдаётся;
    /// база вернулась — задача работает.
    #[tokio::test(start_paused = true)]
    async fn a_minute_without_the_database_does_not_stop_the_process() {
        let runs = Arc::new(AtomicUsize::new(0));
        let up = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let start = Instant::now();
        let handle = {
            let runs = runs.clone();
            let up = up.clone();
            let cancel = cancel.clone();
            tokio::spawn(supervise(
                "engine",
                cancel,
                RestartPolicy::default(),
                move |token| {
                    let runs = runs.clone();
                    let up = up.clone();
                    async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        if start.elapsed() < Duration::from_secs(60) {
                            return Err(anyhow::Error::from(engine::EngineError::Db(
                                sqlx::Error::PoolTimedOut,
                            )));
                        }
                        up.store(true, Ordering::SeqCst);
                        token.cancelled().await;
                        Ok(())
                    }
                },
            ))
        };
        tokio::time::sleep(Duration::from_secs(150)).await;
        assert!(
            !handle.is_finished(),
            "supervisor gave up on a database outage"
        );
        assert!(
            up.load(Ordering::SeqCst),
            "task works again after the database came back"
        );
        assert!(
            runs.load(Ordering::SeqCst) > 5,
            "more failures than the give-up limit"
        );
        // Сторож зависания видит, что задача пережидала сбой, а не висела (шаг 2.7).
        assert!(last_transient_retry("engine").is_some());
        cancel.cancel();
        assert!(handle.await.unwrap().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn failing_task_is_restarted_then_supervisor_gives_up() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        let result = supervise(
            "always-fails",
            CancellationToken::new(),
            RestartPolicy::default(),
            move |_| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    anyhow::bail!("boom")
                }
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(runs.load(Ordering::SeqCst), 6, "1 start + 5 restarts");
    }

    #[tokio::test(start_paused = true)]
    async fn panics_count_as_failures() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        let policy = RestartPolicy {
            max_restarts: 1,
            ..RestartPolicy::default()
        };
        let result = supervise("panics", CancellationToken::new(), policy, move |_| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                panic!("bug");
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_stops_a_healthy_task_cleanly() {
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(supervise(
            "healthy",
            cancel.clone(),
            RestartPolicy::default(),
            |token| async move {
                token.cancelled().await;
                Ok(())
            },
        ));
        tokio::time::sleep(Duration::from_secs(5)).await;
        cancel.cancel();
        assert!(handle.await.unwrap().is_ok());
    }
}
