//! `random_id` для отправок Telegram.
//!
//! Денежная операция получает свой `random_id` заранее ([`new_random_id`], хранится
//! в `operations.request`). Шаги операции, которым нужна своя отправка (метка в «Избранном»,
//! команда меню, ввод суммы), получают `random_id`, выведенный из него детерминированно
//! ([`derive`]): повтор шага после сбоя идёт с тем же числом, и Telegram не создаст второе
//! сообщение.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Шаги операции, которым нужна своя отправка.
pub(crate) const MARKER: u64 = 1;
pub(crate) const MENU_COMMAND: u64 = 2;
pub(crate) const MENU_AMOUNT: u64 = 3;
pub(crate) const XROCKET_RETRY: u64 = 4;
pub(crate) const INVOICE_OPEN: u64 = 5;

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(GOLDEN);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn to_id(x: u64) -> i64 {
    let id = i64::from_ne_bytes(x.to_ne_bytes());
    // Ноль Telegram не принимает как random_id.
    if id == 0 { 1 } else { id }
}

/// `random_id` шага `step` операции с `base`. Один и тот же для одних и тех же аргументов.
pub fn derive(base: i64, step: u64) -> i64 {
    let base = u64::from_ne_bytes(base.to_ne_bytes());
    to_id(splitmix64(base ^ step.wrapping_mul(GOLDEN)))
}

/// Новый случайный `random_id` (для операций и для команд без денег).
pub fn new_random_id() -> i64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| {
        u64::try_from(d.as_nanos() & u128::from(u64::MAX)).unwrap_or(0)
    });
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    to_id(splitmix64(nanos ^ splitmix64(n)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn derived_ids_are_stable_distinct_and_nonzero() {
        let base = 0x1234_5678_9abc_def0;
        assert_eq!(derive(base, MARKER), derive(base, MARKER));
        let steps = [
            MARKER,
            MENU_COMMAND,
            MENU_AMOUNT,
            XROCKET_RETRY,
            INVOICE_OPEN,
        ];
        let ids: HashSet<i64> = steps.iter().map(|s| derive(base, *s)).collect();
        assert_eq!(ids.len(), steps.len());
        assert!(!ids.contains(&base));
        assert!(ids.iter().all(|id| *id != 0));
        assert_ne!(derive(base, MARKER), derive(base + 1, MARKER));
    }

    #[test]
    fn fresh_ids_do_not_repeat() {
        let ids: HashSet<i64> = (0..10_000).map(|_| new_random_id()).collect();
        assert_eq!(ids.len(), 10_000);
    }
}
