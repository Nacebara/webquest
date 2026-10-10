//! Ворота аккаунта Telegram: одна операция в полёте, темп, FLOOD_WAIT, карантин чатов.
//!
//! Операция берёт ворота на всё время (`Conv` держит замок), поэтому сообщения ботов в её
//! окне относятся только к ней. Темп (LOVEC-PORTING §6, F1): не чаще одного действия в
//! `min_interval` и не больше `bot_actions_per_minute` обращений к ботам за 60 с — lovec
//! получал FLOOD_WAIT уже после 10 команд в минуту. FLOOD_WAIT останавливает весь аккаунт.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use tokio::sync::{Mutex, MutexGuard};
use tokio::time::Instant;

use super::FlowConfig;
use crate::transport::Chat;

const WINDOW: Duration = Duration::from_secs(60);

/// Ворота одного аккаунта Telegram. Общие для кошельков обеих платформ на этом аккаунте.
#[derive(Debug)]
pub struct AccountGate {
    label: String,
    state: Mutex<GateState>,
}

/// Что считается в темпе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pace {
    /// Обращение к боту: команда, нажатие, инлайн-запрос и его отправка.
    Bot,
    /// Своё сообщение в «Избранное».
    Own,
}

#[derive(Debug, Default)]
pub(crate) struct GateState {
    last_action: Option<Instant>,
    bot_actions: VecDeque<Instant>,
    flood_until: Option<Instant>,
    quarantine: HashMap<Chat, Instant>,
}

impl AccountGate {
    /// `label` — метка аккаунта (`userbot_accounts.label`), идёт в журнал сообщений.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            state: Mutex::new(GateState::default()),
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Сколько ещё длится пауза FLOOD_WAIT (для ops-бота и сохранения в БД).
    pub async fn flood_remaining(&self) -> Option<Duration> {
        self.state.lock().await.flood_left(Instant::now())
    }

    /// Поставить паузу FLOOD_WAIT, например восстановленную из БД после рестарта.
    pub async fn set_flood(&self, wait: Duration) {
        self.state.lock().await.flood(wait);
    }

    pub(crate) async fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().await
    }
}

impl GateState {
    fn flood_left(&self, now: Instant) -> Option<Duration> {
        self.flood_until
            .filter(|until| *until > now)
            .map(|until| until - now)
    }

    pub(crate) fn flood(&mut self, wait: Duration) {
        let until = Instant::now() + wait;
        self.flood_until = Some(self.flood_until.map_or(until, |u| u.max(until)));
    }

    /// Дождаться права на действие. `Err` — аккаунт на паузе FLOOD_WAIT (ждать не будем:
    /// операция возвращается движку и встаёт в очередь).
    pub(crate) async fn pace(&mut self, cfg: &FlowConfig, kind: Pace) -> Result<(), Duration> {
        loop {
            let now = Instant::now();
            if let Some(left) = self.flood_left(now) {
                return Err(left);
            }
            while self
                .bot_actions
                .front()
                .is_some_and(|t| now.duration_since(*t) >= WINDOW)
            {
                self.bot_actions.pop_front();
            }
            let mut ready = self
                .last_action
                .map_or(now, |t| (t + cfg.min_interval).max(now));
            let limit = usize::try_from(cfg.bot_actions_per_minute.max(1)).unwrap_or(usize::MAX);
            if kind == Pace::Bot
                && self.bot_actions.len() >= limit
                && let Some(oldest) = self.bot_actions.front()
            {
                ready = ready.max(*oldest + WINDOW);
            }
            if ready > now {
                tokio::time::sleep_until(ready).await;
                continue;
            }
            self.last_action = Some(now);
            if kind == Pace::Bot {
                self.bot_actions.push_back(now);
            }
            return Ok(());
        }
    }

    pub(crate) fn quarantine_until(&self, chat: Chat) -> Option<Instant> {
        self.quarantine
            .get(&chat)
            .copied()
            .filter(|until| *until > Instant::now())
    }

    pub(crate) fn set_quarantine(&mut self, chat: Chat, wait: Duration) {
        self.quarantine.insert(chat, Instant::now() + wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> FlowConfig {
        FlowConfig {
            min_interval: Duration::from_millis(1500),
            bot_actions_per_minute: 3,
            ..FlowConfig::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pace_keeps_interval_and_minute_budget() {
        let gate = AccountGate::new("ub");
        let mut st = gate.lock().await;
        let start = Instant::now();
        for _ in 0..3 {
            st.pace(&cfg(), Pace::Bot).await.unwrap();
        }
        assert_eq!(start.elapsed(), Duration::from_millis(3000));
        // Четвёртое обращение к боту — только через 60 с после первого.
        st.pace(&cfg(), Pace::Bot).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(60));
        // Своё сообщение бюджет ботов не тратит, но интервал соблюдает.
        st.pace(&cfg(), Pace::Own).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_millis(61_500));
    }

    #[tokio::test(start_paused = true)]
    async fn flood_wait_stops_the_account() {
        let gate = AccountGate::new("ub");
        gate.set_flood(Duration::from_secs(29)).await;
        assert_eq!(gate.flood_remaining().await, Some(Duration::from_secs(29)));
        let mut st = gate.lock().await;
        assert_eq!(
            st.pace(&cfg(), Pace::Own).await,
            Err(Duration::from_secs(29))
        );
        // Более короткая пауза не сокращает текущую.
        st.flood(Duration::from_secs(5));
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(st.flood_left(Instant::now()), Some(Duration::from_secs(19)));
        tokio::time::advance(Duration::from_secs(19)).await;
        assert!(st.pace(&cfg(), Pace::Bot).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn quarantine_expires() {
        let gate = AccountGate::new("ub");
        let chat = Chat::SavedMessages;
        let mut st = gate.lock().await;
        assert!(st.quarantine_until(chat).is_none());
        st.set_quarantine(chat, Duration::from_secs(30));
        assert!(st.quarantine_until(chat).is_some());
        tokio::time::advance(Duration::from_secs(30)).await;
        assert!(st.quarantine_until(chat).is_none());
    }
}
