//! Лимиты частоты на клиента (`settings.user_rate_limit`, SPEC §11): заявок за 10 минут и
//! сообщений со ссылками за минуту. Счётчики — в памяти процесса (скользящее окно), после
//! рестарта начинаются заново: это защита от флуда, а не денежное правило.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

const ORDERS_WINDOW: Duration = Duration::from_secs(600);
const LINKS_WINDOW: Duration = Duration::from_secs(60);

/// Значения `settings.user_rate_limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimits {
    /// Принятых заявок за 10 минут.
    pub orders_per_10min: u32,
    /// Сообщений со ссылками за минуту.
    pub links_per_min: u32,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            orders_per_10min: 5,
            links_per_min: 10,
        }
    }
}

impl RateLimits {
    /// Из JSON настройки; отсутствующее или кривое поле — значение по умолчанию.
    pub fn from_json(value: &Value) -> Self {
        let default = Self::default();
        let field = |name: &str, fallback: u32| {
            value
                .get(name)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(fallback)
        };
        Self {
            orders_per_10min: field("orders_per_10min", default.orders_per_10min),
            links_per_min: field("links_per_min", default.links_per_min),
        }
    }
}

/// Скользящие окна по клиентам.
#[derive(Debug, Default)]
pub(crate) struct RateLimiter {
    links: Mutex<HashMap<i64, VecDeque<Instant>>>,
    orders: Mutex<HashMap<i64, VecDeque<Instant>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Сколько ждать, если окно заполнено; иначе `None`.
fn wait_in(
    hits: &mut VecDeque<Instant>,
    limit: u32,
    window: Duration,
    now: Instant,
) -> Option<Duration> {
    while hits
        .front()
        .is_some_and(|at| now.duration_since(*at) >= window)
    {
        hits.pop_front();
    }
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if hits.len() < limit {
        return None;
    }
    // Освободится, когда выйдет из окна самое старое из последних `limit` событий.
    let oldest = hits
        .len()
        .checked_sub(limit)
        .and_then(|i| hits.get(i))
        .copied()
        .unwrap_or(now);
    Some(window.saturating_sub(now.duration_since(oldest)))
}

impl RateLimiter {
    /// Сообщение со ссылками: учесть или сказать, сколько ждать.
    pub(crate) fn hit_link(&self, user: i64, limit: u32, now: Instant) -> Result<(), Duration> {
        let mut map = lock(&self.links);
        let hits = map.entry(user).or_default();
        if let Some(wait) = wait_in(hits, limit, LINKS_WINDOW, now) {
            return Err(wait);
        }
        hits.push_back(now);
        Ok(())
    }

    /// Можно ли принять ещё заявку (не учитывает её).
    pub(crate) fn order_allowed(
        &self,
        user: i64,
        limit: u32,
        now: Instant,
    ) -> Result<(), Duration> {
        let mut map = lock(&self.orders);
        let hits = map.entry(user).or_default();
        match wait_in(hits, limit, ORDERS_WINDOW, now) {
            Some(wait) => Err(wait),
            None => Ok(()),
        }
    }

    /// Заявка принята.
    pub(crate) fn record_order(&self, user: i64, now: Instant) {
        lock(&self.orders).entry(user).or_default().push_back(now);
    }

    /// Забыть клиентов, у которых окна опустели.
    pub(crate) fn purge(&self, now: Instant) {
        for (map, window) in [(&self.links, LINKS_WINDOW), (&self.orders, ORDERS_WINDOW)] {
            lock(map).retain(|_, hits| {
                hits.retain(|at| now.duration_since(*at) < window);
                !hits.is_empty()
            });
        }
    }
}

/// Секунды для экрана «подождите N с»: вверх, минимум 1.
pub(crate) fn wait_secs(wait: Duration) -> u64 {
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    secs.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_settings_with_fallbacks() {
        let v = serde_json::json!({"orders_per_10min": 2, "links_per_min": 3});
        assert_eq!(
            RateLimits::from_json(&v),
            RateLimits {
                orders_per_10min: 2,
                links_per_min: 3
            }
        );
        let v = serde_json::json!({"orders_per_10min": "x"});
        assert_eq!(RateLimits::from_json(&v), RateLimits::default());
        assert_eq!(RateLimits::from_json(&Value::Null), RateLimits::default());
    }

    #[test]
    fn link_window_slides() {
        let l = RateLimiter::default();
        let t0 = Instant::now();
        for i in 0..3 {
            assert!(l.hit_link(1, 3, t0 + Duration::from_secs(i)).is_ok());
        }
        let wait = l.hit_link(1, 3, t0 + Duration::from_secs(10)).unwrap_err();
        assert_eq!(wait, Duration::from_secs(50));
        assert!(l.hit_link(2, 3, t0).is_ok(), "другой клиент не затронут");
        assert!(l.hit_link(1, 3, t0 + Duration::from_secs(60)).is_ok());
        assert_eq!(wait_secs(Duration::from_millis(1)), 1);
        assert_eq!(wait_secs(Duration::from_millis(49_001)), 50);
    }

    #[test]
    fn orders_are_counted_only_when_recorded() {
        let l = RateLimiter::default();
        let t0 = Instant::now();
        assert!(l.order_allowed(1, 1, t0).is_ok());
        assert!(l.order_allowed(1, 1, t0).is_ok(), "проверка не учитывает");
        l.record_order(1, t0);
        assert!(l.order_allowed(1, 1, t0 + Duration::from_secs(1)).is_err());
        assert!(l.order_allowed(1, 1, t0 + ORDERS_WINDOW).is_ok());
        assert!(
            l.order_allowed(1, 0, t0).is_err(),
            "лимит 0 — ничего нельзя"
        );
        l.purge(t0 + ORDERS_WINDOW);
        assert!(lock(&l.orders).is_empty());
    }
}
