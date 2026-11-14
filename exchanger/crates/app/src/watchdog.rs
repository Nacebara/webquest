//! Сторож зависания (находка Н-9, шаг 2.7 плана).
//!
//! Движок и опрос Telegram отмечают каждый свой круг ([`crate::health::Probe`]). Если какой-то из
//! них молчит дольше [`LIMIT`] — цикл завис (ожидание без таймаута, взаимная блокировка), и
//! перезапуск задачи изнутри не поможет: сторож пишет в журнал, что именно молчит, и завершает
//! процесс кодом [`EXIT_STUCK`]. Служба Windows поднимает exch заново, восстановление доводит
//! заявки (что было в полёте — к человеку, как после любого падения).
//!
//! Сторож работает на отдельном потоке ОС, а не в tokio: если завис сам рантайм, задача внутри
//! него не проснулась бы. Сбой базы или сети его не будит: круг считается и тогда, когда
//! супервизор пережидает сбой (шаг 2.4), и когда опрос Telegram вернулся с ошибкой сети — процесс
//! не завис, он ждёт. Предел — 15 минут: дольше любой операции юзербота.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::health::Probe;

/// Сколько цикл может молчать, прежде чем сторож перезапустит exch.
pub const LIMIT: Duration = Duration::from_secs(15 * 60);
/// Как часто сторож смотрит на часы.
pub const CHECK_EVERY: Duration = Duration::from_secs(30);
/// Код выхода «завис»: служба перезапускает exch по любому ненулевому коду, а в журнале службы
/// этот код отличает зависание от ошибки старта (1).
pub const EXIT_STUCK: i32 = 3;

/// Какие циклы молчат дольше `limit`. Цикл, который ещё ни разу не отозвался, считается от
/// `started` — запуска сторожа. Нет ни одного цикла (бот не запущен) — сторожить нечего.
pub fn stuck(
    probes: &[Probe],
    now: DateTime<Utc>,
    started: DateTime<Utc>,
    limit: Duration,
) -> Vec<(&'static str, Duration)> {
    probes
        .iter()
        .filter_map(|p| {
            let since = (p.last)().unwrap_or(started).max(started);
            let silent = (now - since).to_std().unwrap_or_default();
            (silent > limit).then_some((p.name, silent))
        })
        .collect()
}

/// Смотреть раз в `every`; что-то молчит дольше `limit` — вызвать `on_stuck` и выйти из цикла.
/// В программе `on_stuck` завершает процесс, в тестах — записывает.
pub fn watch(
    probes: Arc<Vec<Probe>>,
    limit: Duration,
    every: Duration,
    on_stuck: impl FnOnce(&[(&'static str, Duration)]),
) {
    let started = Utc::now();
    loop {
        std::thread::sleep(every);
        let stuck = stuck(&probes, Utc::now(), started, limit);
        if !stuck.is_empty() {
            on_stuck(&stuck);
            return;
        }
    }
}

/// Запустить сторожа на своём потоке ОС. Сработал — журнал и выход с кодом [`EXIT_STUCK`].
pub fn spawn(probes: Arc<Vec<Probe>>) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("exch-watchdog".into())
        .spawn(move || {
            watch(probes, LIMIT, CHECK_EVERY, |stuck| {
                for (name, silent) in stuck {
                    tracing::error!(
                        loop_name = name,
                        silent_secs = silent.as_secs(),
                        "watchdog: the loop is stuck; exiting so that the service restarts exch"
                    );
                }
                // Строка журнала уже в файле (пишется сразу); окно — на случай запуска руками.
                eprintln!(
                    "exch завис ({}): выхожу с кодом {EXIT_STUCK}, служба запустит заново",
                    stuck.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
                );
                std::process::exit(EXIT_STUCK);
            });
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn ago(secs: i64) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::seconds(secs)
    }

    #[test]
    fn a_loop_silent_longer_than_the_limit_is_stuck() {
        let start = ago(3600);
        let probes = vec![
            Probe::new("engine", || Some(ago(5))),
            Probe::new("telegram polling", || Some(ago(20 * 60))),
        ];
        let s = stuck(&probes, Utc::now(), start, LIMIT);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0, "telegram polling");
    }

    #[test]
    fn a_loop_that_never_beat_is_counted_from_the_start() {
        let never = vec![Probe::new("engine", || None)];
        assert!(stuck(&never, Utc::now(), ago(60), LIMIT).is_empty());
        assert_eq!(stuck(&never, Utc::now(), ago(16 * 60), LIMIT).len(), 1);
        // Старый круг до запуска сторожа не в счёт: считаем от запуска.
        let before = vec![Probe::new("engine", || Some(ago(3600)))];
        assert!(stuck(&before, Utc::now(), ago(60), LIMIT).is_empty());
    }

    #[test]
    fn nothing_to_watch_never_fires() {
        assert!(stuck(&[], Utc::now(), ago(3600), LIMIT).is_empty());
    }

    /// Критерий шага 2.7: зависший цикл — сторож срабатывает (в программе — выход с кодом 3).
    #[test]
    fn the_watchdog_fires_when_a_loop_hangs_and_not_before() {
        let beat = Arc::new(AtomicI64::new(Utc::now().timestamp_millis()));
        let b = beat.clone();
        let probes = Arc::new(vec![Probe::new("engine", move || {
            DateTime::from_timestamp_millis(b.load(Ordering::SeqCst))
        })]);
        let fired = Arc::new(Mutex::new(None));
        let f = fired.clone();
        let handle = std::thread::spawn(move || {
            watch(
                probes,
                Duration::from_millis(400),
                Duration::from_millis(50),
                |s| {
                    *f.lock().unwrap() = Some(s.iter().map(|(n, _)| *n).collect::<Vec<_>>());
                },
            );
        });
        // Пока цикл отмечается — тишина.
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(100));
            beat.store(Utc::now().timestamp_millis(), Ordering::SeqCst);
        }
        assert!(
            fired.lock().unwrap().is_none(),
            "fired while the loop was alive"
        );
        // Цикл завис.
        handle.join().unwrap();
        assert_eq!(fired.lock().unwrap().clone(), Some(vec!["engine"]));
    }
}
