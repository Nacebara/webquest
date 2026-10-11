//! Клиентский бот exch (HANDOVER §5, M4; DESIGN-v0.2 §6).
//!
//! Обвязка поверх `ui` (экраны), `botapi` (Bot API 10.3), `parsers` (ссылки) и `engine`
//! (заявки):
//!
//! - **приём обновлений** — long polling `getUpdates`, `offset` в памяти; обновления одного
//!   клиента — по порядку, разных клиентов — параллельно; каждое обновление — в своей задаче,
//!   ошибка или паника в одном не останавливает бота ([`poll`]);
//! - **клиент** — `users` обновляется на каждом обновлении; при активном бане — только экран
//!   бана ([`handler`]);
//! - **команды и меню** — `/start`, `/help`, `/terms`, `/history`, `/privacy`, кнопки главного
//!   меню, инлайн-кнопки; на нажатие кнопки отвечаем всегда;
//! - **приём ссылок** — текст, `text_link`, кнопки пересланного сообщения → первая ссылка на
//!   чек → `Engine::submit_check`; лимиты `settings.user_rate_limit` ([`intake`]);
//! - **одно сообщение на заявку** — отправляется при приёме и правится по событиям движка
//!   (`Engine::subscribe`); его id хранится в `orders.progress_*`, после рестарта сообщения
//!   догоняют текущее состояние ([`orders`]).
//!
//! Деньги бот не двигает: все решения о деньгах — в движке. Бот только показывает, где деньги
//! клиента и что делать (CLAUDE.md, правило 19). Ссылки на чеки в логи не пишутся.

mod api;
mod handler;
mod intake;
mod limits;
mod locks;
mod orders;
mod poll;
mod terms;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use botapi::BotApi;
use domain::Decimal;
use engine::Engine;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use ui::{Screen, Ui};

pub use limits::RateLimits;

/// Настройки клиентского бота.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientBotConfig {
    /// Сколько секунд Telegram держит запрос `getUpdates`.
    pub poll_timeout_secs: u32,
    /// `@username` оператора для экрана «Поддержка».
    pub support_contact: Option<String>,
    /// `@username` аккаунта, к которому можно привязать чек (экран условий, ошибка E05).
    pub bind_username: Option<String>,
    /// За сколько оператор обещает разобрать заявку на ручной проверке (SPEC §7: 30 мин).
    pub manual_review_sla: Duration,
    /// Сообщения заявок, закончившихся за это время, сверяются с состоянием после рестарта.
    pub resync_window: Duration,
    /// Как часто досылать сообщения заявок, которые не удалось отправить.
    pub sweep_interval: Duration,
    /// Часть резерва, которую движок не отдаёт под заявки (`EngineConfig::reserve_safety`).
    pub reserve_safety: Decimal,
    /// Сколько держать в памяти условия направлений между запросами.
    pub terms_ttl: Duration,
    /// Сколько держать в памяти `settings.user_rate_limit`.
    pub settings_ttl: Duration,
    /// Справочные экраны — rich-сообщением (`sendRichMessage`) с HTML-фолбэком.
    pub rich_messages: bool,
    /// Не чаще этого — повтор экрана бана или «слишком часто» одному клиенту.
    pub notice_interval: Duration,
    /// Сколько ждать незаконченные правки сообщений при остановке.
    pub shutdown_grace: Duration,
}

impl Default for ClientBotConfig {
    fn default() -> Self {
        Self {
            poll_timeout_secs: 30,
            support_contact: None,
            bind_username: None,
            manual_review_sla: Duration::from_secs(30 * 60),
            resync_window: Duration::from_secs(24 * 3600),
            sweep_interval: Duration::from_secs(5 * 60),
            reserve_safety: Decimal::ZERO,
            terms_ttl: Duration::from_secs(10),
            settings_ttl: Duration::from_secs(60),
            rich_messages: true,
            notice_interval: Duration::from_secs(20),
            shutdown_grace: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TgBotError {
    #[error(transparent)]
    Api(#[from] botapi::Error),
    #[error(transparent)]
    Storage(#[from] storage::StorageError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Engine(#[from] engine::EngineError),
}

/// Клиентский бот. Дешёвый `Clone`.
#[derive(Clone)]
pub struct ClientBot {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ClientBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientBot")
            .field("api", &self.inner.api)
            .field("cfg", &self.inner.cfg)
            .finish_non_exhaustive()
    }
}

impl ClientBot {
    /// `engine` — тот же движок, который крутит `Engine::run`: бот подаёт в него чеки и
    /// подписывается на изменения заявок.
    pub fn new(
        api: BotApi,
        ui: Ui,
        engine: Arc<Engine>,
        pool: PgPool,
        cfg: ClientBotConfig,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                api: api::Api::new(api),
                ui,
                engine,
                pool,
                cfg,
                bot_username: OnceLock::new(),
                limiter: limits::RateLimiter::default(),
                limits_cache: Mutex::new(None),
                terms_cache: Mutex::new(None),
                locks: locks::KeyedLocks::default(),
                memory: Mutex::new(Memory::default()),
            }),
        }
    }

    /// Работать до отмены `cancel`: приём обновлений и правка сообщений заявок. Возвращается
    /// после мягкой остановки: текущая порция обновлений доработана, обработанные обновления
    /// подтверждены Telegram, незаконченные правки сообщений ждали до `shutdown_grace`.
    pub async fn run(&self, cancel: CancellationToken) -> Result<(), TgBotError> {
        let inner = self.inner.clone();
        inner.startup().await;
        // Подписка — до пересинхронизации: изменение между ними не потеряется.
        let updates = inner.engine.subscribe();
        let stop = cancel.child_token();
        let listener = tokio::spawn(orders::listen(inner.clone(), updates, stop.clone()));
        poll::run(inner, cancel).await;
        stop.cancel();
        if let Err(e) = listener.await {
            tracing::error!(error = %e, "order message listener crashed");
        }
        Ok(())
    }
}

/// Что бот помнит в памяти между обновлениями (не переживает рестарт — и не должно).
#[derive(Default)]
pub(crate) struct Memory {
    /// Клиенты, которые попросили простой вид вместо rich.
    plain_view: HashSet<i64>,
    /// Последние rich-сообщения: (чат, сообщение) → экран, чтобы прислать его простым видом.
    rich_sent: HashMap<(i64, i32), Screen>,
    rich_order: VecDeque<(i64, i32)>,
    /// Последний показанный вид сообщения заявки (хеш), чтобы не править одинаковым.
    rendered: HashMap<i64, u64>,
    /// Заявки, сообщение которых доставить нельзя (клиент заблокировал бота).
    unreachable: HashSet<i64>,
    /// Когда клиенту последний раз показали уведомление (бан, лимит).
    notices: HashMap<(i64, Notice), Instant>,
}

/// Сколько rich-сообщений помнить для «Не отображается?».
const RICH_MEMORY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Notice {
    Banned,
    RateLimited,
}

pub(crate) struct Inner {
    api: api::Api,
    ui: Ui,
    engine: Arc<Engine>,
    pool: PgPool,
    cfg: ClientBotConfig,
    bot_username: OnceLock<String>,
    limiter: limits::RateLimiter,
    limits_cache: Mutex<Option<(Instant, RateLimits)>>,
    terms_cache: Mutex<Option<(Instant, terms::TermsSnapshot)>>,
    locks: locks::KeyedLocks,
    memory: Mutex<Memory>,
}

impl Inner {
    /// Имя бота (для `/cmd@bot`) и меню команд. Сбой не мешает работать.
    async fn startup(&self) {
        match self.api.bot().get_me().await {
            Ok(me) => {
                if let Some(name) = me.username {
                    let _ = self.bot_username.set(name);
                }
            }
            Err(e) => tracing::warn!(error = %e, "getMe failed, commands with @mention are accepted for any bot"),
        }
        if let Err(e) = self.api.bot().set_my_commands(self.ui.bot_commands()).await {
            tracing::warn!(error = %e, "setMyCommands failed");
        }
    }

    fn bot_username(&self) -> Option<&str> {
        self.bot_username.get().map(String::as_str)
    }

    /// Поработать с памятью. Блокировка короткая и без `await` внутри.
    fn memory<R>(&self, f: impl FnOnce(&mut Memory) -> R) -> R {
        let mut guard = match self.memory.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard)
    }

    /// Показать уведомление этого вида клиенту, если давно не показывали.
    fn should_notify(&self, user: i64, kind: Notice) -> bool {
        let now = Instant::now();
        let every = self.cfg.notice_interval;
        self.memory(|m| {
            m.notices.retain(|_, at| now.duration_since(*at) < every);
            match m.notices.entry((user, kind)) {
                std::collections::hash_map::Entry::Occupied(_) => false,
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(now);
                    true
                }
            }
        })
    }

    fn remember_rich(&self, chat: i64, message_id: i32, screen: &Screen) {
        self.memory(|m| {
            m.rich_sent.insert((chat, message_id), screen.clone());
            m.rich_order.push_back((chat, message_id));
            while m.rich_order.len() > RICH_MEMORY {
                if let Some(old) = m.rich_order.pop_front() {
                    m.rich_sent.remove(&old);
                }
            }
        });
    }

    fn prefers_plain(&self, user: i64) -> bool {
        !self.cfg.rich_messages || self.memory(|m| m.plain_view.contains(&user))
    }
}
