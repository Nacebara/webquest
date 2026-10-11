//! Асинхронные блокировки по ключу (id заявки): приём ссылки и событие движка правят одно
//! и то же сообщение заявки по очереди, иначе старое состояние могло бы затереть новое, а
//! сообщение — уйти дважды. Запись удаляется, когда блокировку больше никто не держит и не ждёт.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::OwnedMutexGuard;

#[derive(Debug, Default)]
pub(crate) struct KeyedLocks {
    map: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

pub(crate) struct KeyedGuard<'a> {
    guard: Option<OwnedMutexGuard<()>>,
    key: i64,
    locks: &'a KeyedLocks,
}

impl KeyedLocks {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Arc<tokio::sync::Mutex<()>>>> {
        match self.map.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(crate) async fn lock(&self, key: i64) -> KeyedGuard<'_> {
        let mutex = self.map().entry(key).or_default().clone();
        let guard = mutex.lock_owned().await;
        KeyedGuard {
            guard: Some(guard),
            key,
            locks: self,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map().len()
    }
}

impl Drop for KeyedGuard<'_> {
    fn drop(&mut self) {
        drop(self.guard.take());
        let mut map = self.locks.map();
        // Только запись в карте: никто не держит и не ждёт.
        if map
            .get(&self.key)
            .is_some_and(|m| Arc::strong_count(m) == 1)
        {
            map.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn same_key_is_serialized_and_entries_are_cleaned() {
        let locks = Arc::new(KeyedLocks::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let first = locks.lock(1).await;
        let task = {
            let locks = locks.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let _g = locks.lock(1).await;
                log.lock().unwrap_or_else(|p| p.into_inner()).push("second");
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let other = locks.lock(2).await;
        log.lock().unwrap_or_else(|p| p.into_inner()).push("first");
        drop(first);
        task.await.unwrap_or_default();
        assert_eq!(
            *log.lock().unwrap_or_else(|p| p.into_inner()),
            vec!["first", "second"]
        );
        drop(other);
        assert_eq!(locks.len(), 0);
    }
}
