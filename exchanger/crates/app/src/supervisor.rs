//! Супервизор задач (SPEC §10.1): упавшая задача перезапускается с backoff;
//! больше 5 перезапусков за 10 минут — супервизор сдаётся, процесс выходит с кодом 1,
//! Docker перезапускает контейнер, рекавери доводит заявки.

use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

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
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let outcome = tokio::spawn(make(cancel.child_token())).await;
        if cancel.is_cancelled() {
            return Ok(());
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
