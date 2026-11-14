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
mod checkwait;
pub mod emoji;
mod handler;
mod intake;
mod invest;
mod limits;
mod locks;
mod orders;
mod poll;
mod promo;
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

/// Обработчик, который видит каждое обновление раньше клиентского бота: админ-панель
/// персонала живёт в этом же боте (решение владельца 7 ноября 2026: один бот, как гарант).
#[async_trait::async_trait]
pub trait UpdateHook: Send + Sync {
    /// `true` — обновление обработано (команда или кнопка пульта, ответ на его вопрос),
    /// клиентский бот его не трогает.
    async fn try_handle(&self, update: &botapi::Update) -> bool;
    /// Дополнительная кнопка главного меню для пользователя (персоналу — вход в пульт).
    async fn menu_button(&self, user: i64) -> Option<String>;
    /// Вопрос клиента в поддержку (макет B1 «Поддержка»): передать персоналу в пульт.
    /// `false` — передать некому.
    async fn support_question(
        &self,
        _from: &botapi::User,
        _text: &str,
        _order: Option<&str>,
    ) -> bool {
        false
    }
}

/// Настройки клиентского бота.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientBotConfig {
    /// Сколько секунд Telegram держит запрос `getUpdates`.
    pub poll_timeout_secs: u32,
    /// `@username` оператора для экрана «Поддержка».
    pub support_contact: Option<String>,
    /// `@username` аккаунта, к которому можно привязать чек (экран условий, ошибка E05).
    pub bind_username: Option<String>,
    /// За сколько оператор обещает разобрать заявку на ручной проверке: «Обычно до 15 минут»
    /// (утверждённый макет «Ручная проверка»; шаг 6.3, К-4; раньше было 30 по SPEC §7).
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
    /// Чек через инлайн-режим кошелька без ссылки: как часто перечитывать сообщение.
    pub check_link_step: Duration,
    /// …и сколько всего ждать ссылку.
    pub check_link_wait: Duration,
    /// …и на каких моментах ожидания пробовать служебную пересылку.
    pub check_link_forward_at: Vec<Duration>,
}

impl Default for ClientBotConfig {
    fn default() -> Self {
        Self {
            poll_timeout_secs: 30,
            support_contact: None,
            bind_username: None,
            manual_review_sla: Duration::from_secs(15 * 60),
            resync_window: Duration::from_secs(24 * 3600),
            sweep_interval: Duration::from_secs(5 * 60),
            reserve_safety: Decimal::ZERO,
            terms_ttl: Duration::from_secs(10),
            settings_ttl: Duration::from_secs(60),
            rich_messages: true,
            notice_interval: Duration::from_secs(20),
            shutdown_grace: Duration::from_secs(10),
            check_link_step: Duration::from_secs(2),
            check_link_wait: Duration::from_secs(60),
            check_link_forward_at: vec![
                Duration::from_secs(8),
                Duration::from_secs(24),
                Duration::from_secs(44),
            ],
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
                ui: std::sync::RwLock::new(Arc::new(ui)),
                emoji_seen: Mutex::new(None),
                hook: std::sync::RwLock::new(None),
                engine,
                pool,
                cfg,
                bot_username: OnceLock::new(),
                limiter: limits::RateLimiter::default(),
                limits_cache: Mutex::new(None),
                terms_cache: Mutex::new(None),
                locks: locks::KeyedLocks::default(),
                memory: Mutex::new(Memory::default()),
                poll_beat: std::sync::atomic::AtomicI64::new(0),
                poll_attempt: std::sync::atomic::AtomicI64::new(0),
                background: Mutex::new(tokio::task::JoinSet::new()),
                invoice_slots: Arc::new(tokio::sync::Semaphore::new(INVOICE_SLOTS)),
            }),
        }
    }

    /// Когда опрос обновлений последний раз прошёл круг (удачно или с ошибкой); `None` — ещё ни
    /// разу. Для сторожа зависания.
    pub fn last_poll_attempt(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        match self
            .inner
            .poll_attempt
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            0 => None,
            t => chrono::DateTime::from_timestamp(t, 0),
        }
    }

    /// Когда Telegram последний раз ответил на опрос обновлений; `None` — ещё ни разу.
    pub fn last_poll(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        match self
            .inner
            .poll_beat
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            0 => None,
            t => chrono::DateTime::from_timestamp(t, 0),
        }
    }

    /// Подключить админ-панель (или другой обработчик), который видит обновления первым.
    pub fn with_hook(self, hook: Arc<dyn UpdateHook>) -> Self {
        if let Ok(mut slot) = self.inner.hook.write() {
            *slot = Some(hook);
        }
        self
    }

    /// Работать до отмены `cancel`: приём обновлений и правка сообщений заявок. Возвращается
    /// после мягкой остановки: текущая порция обновлений доработана, обработанные обновления
    /// подтверждены Telegram, незаконченные правки сообщений ждали до `shutdown_grace`.
    pub async fn run(&self, cancel: CancellationToken) -> Result<(), TgBotError> {
        let inner = self.inner.clone();
        inner.startup().await;
        // Подписка — до пересинхронизации: изменение между ними не потеряется.
        let updates = inner.engine.subscribe();
        let invest_updates = inner.engine.subscribe_invest();
        let stop = cancel.child_token();
        let listener = tokio::spawn(orders::listen(inner.clone(), updates, stop.clone()));
        let investors = tokio::spawn(invest::listen(inner.clone(), invest_updates, stop.clone()));
        let reload = tokio::spawn(emoji::watch(inner.clone(), stop.clone()));
        let broadcast = tokio::spawn(promo::run(inner.clone(), stop.clone()));
        poll::run(inner.clone(), cancel).await;
        inner.finish_background().await;
        stop.cancel();
        let _ = broadcast.await;
        let _ = reload.await;
        let _ = investors.await;
        if let Err(e) = listener.await {
            tracing::error!(error = %e, "order message listener crashed");
        }
        Ok(())
    }
}

/// Сколько счетов юзербот открывает одновременно в фоне.
const INVOICE_SLOTS: usize = 4;

impl Inner {
    /// Запустить работу в фоне: опрос Telegram её не ждёт (шаг 2.9, находка Н-7). При остановке
    /// бот дожидается её до `shutdown_grace`.
    pub(crate) fn spawn_background(
        &self,
        task: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        if let Ok(mut set) = self.background.lock() {
            // Закончившиеся задачи — убрать, чтобы набор не рос.
            while set.try_join_next().is_some() {}
            set.spawn(task);
        }
    }

    async fn finish_background(&self) {
        let mut set = match self.background.lock() {
            Ok(mut s) => std::mem::take(&mut *s),
            Err(_) => return,
        };
        let wait = async { while set.join_next().await.is_some() {} };
        if tokio::time::timeout(self.cfg.shutdown_grace, wait)
            .await
            .is_err()
        {
            tracing::warn!("background invoice checks did not finish before the stop");
        }
    }
}

/// Что бот помнит в памяти между обновлениями. Не переживает рестарт — кроме «Простого вида» и
/// ожидания вопроса в поддержку: они ещё и в базе (`client_prefs`, шаг 6.3) и читаются при запуске.
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
    /// Инвестор нажал «Пополнить» или выбрал кошелёк вывода: чего ждём и с какого момента.
    invest_wait: HashMap<i64, (invest::Wait, Instant)>,
    /// Ссылки, уже взятые из сообщения (чат, сообщение) — чтобы правка того же сообщения
    /// не обработала их второй раз.
    link_messages: HashMap<(i64, i32), HashSet<String>>,
    link_order: VecDeque<(i64, i32)>,
    /// Заявки, у которых клиент развернул «Детали» (макет B1).
    details_open: HashSet<i64>,
    /// Клиент нажал «Поддержка»: следующий текст — вопрос оператору (заявка, с какого момента).
    support_wait: HashMap<i64, (Option<String>, Instant)>,
}

/// Сколько ждать вопрос после «Поддержка».
pub(crate) const SUPPORT_WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Сколько сообщений со ссылками помнить для правок.
const LINK_MEMORY: usize = 4096;

/// Сколько rich-сообщений помнить для «Не отображается?».
const RICH_MEMORY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Notice {
    Banned,
    RateLimited,
}

pub(crate) struct Inner {
    api: api::Api,
    /// Интерфейс; набор эмодзи меняется из пульта без перезапуска ([`emoji`]).
    ui: std::sync::RwLock<Arc<Ui>>,
    /// Последние прочитанные настройки эмодзи (отпечаток), чтобы не пересобирать зря.
    emoji_seen: Mutex<Option<String>>,
    /// Админ-панель: видит обновления раньше клиентского бота.
    hook: std::sync::RwLock<Option<Arc<dyn UpdateHook>>>,
    engine: Arc<Engine>,
    pool: PgPool,
    cfg: ClientBotConfig,
    bot_username: OnceLock<String>,
    limiter: limits::RateLimiter,
    limits_cache: Mutex<Option<(Instant, RateLimits)>>,
    terms_cache: Mutex<Option<(Instant, terms::TermsSnapshot)>>,
    locks: locks::KeyedLocks,
    memory: Mutex<Memory>,
    /// Последний удачный ответ getUpdates (секунды Unix, 0 — ещё ни разу): по нему внешний пинг
    /// понимает, что опрос Telegram жив (шаг 2.5).
    poll_beat: std::sync::atomic::AtomicI64,
    /// Последний круг опроса — удачный или нет (секунды Unix): по нему сторож зависания видит,
    /// что опрос не завис, а пережидает сеть (шаг 2.7).
    poll_attempt: std::sync::atomic::AtomicI64,
    /// Счета, которые юзербот открывает в фоне (шаг 2.9): опрос Telegram их не ждёт.
    background: Mutex<tokio::task::JoinSet<()>>,
    /// Сколько счетов открывается одновременно: остальные ждут своей очереди.
    invoice_slots: Arc<tokio::sync::Semaphore>,
}

impl Inner {
    /// Текущий интерфейс.
    pub(crate) fn ui(&self) -> Arc<Ui> {
        match self.ui.read() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    pub(crate) fn hook(&self) -> Option<Arc<dyn UpdateHook>> {
        match self.hook.read() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    pub(crate) fn set_ui(&self, ui: Ui) {
        match self.ui.write() {
            Ok(mut g) => *g = Arc::new(ui),
            Err(p) => *p.into_inner() = Arc::new(ui),
        }
    }

    /// Имя бота (для `/cmd@bot`) и меню команд. Сбой не мешает работать.
    /// «Простой вид» и «Поддержка нажата» — из базы в память (шаг 6.3, К-4).
    async fn load_client_prefs(&self) {
        let since =
            chrono::Utc::now() - chrono::Duration::from_std(SUPPORT_WAIT).unwrap_or_default();
        let saved = match self.pool.acquire().await {
            Ok(mut conn) => storage::client_prefs::load(&mut conn, since).await,
            Err(e) => Err(e.into()),
        };
        let saved = match saved {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "client preferences not loaded");
                return;
            }
        };
        let now = chrono::Utc::now();
        self.memory(|m| {
            m.plain_view.extend(saved.plain_view.iter().copied());
            for (user, order, at) in &saved.support_waits {
                let age = (now - *at).to_std().unwrap_or_default();
                let since = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
                m.support_wait.insert(*user, (order.clone(), since));
            }
        });
    }

    /// Записать «Простой вид» в базу. Не записалось — не беда: в памяти он есть.
    pub(crate) async fn save_plain_view(&self, user: i64, plain: bool) {
        let saved = match self.pool.acquire().await {
            Ok(mut conn) => storage::client_prefs::set_plain_view(&mut conn, user, plain).await,
            Err(e) => Err(e.into()),
        };
        if let Err(e) = saved {
            tracing::warn!(error = %e, "plain view not saved");
        }
    }

    /// Записать ожидание вопроса в поддержку (`Some`) или снять его (`None`).
    pub(crate) async fn save_support_wait(&self, user: i64, wait: Option<Option<String>>) {
        let saved = match self.pool.acquire().await {
            Ok(mut conn) => match &wait {
                Some(order) => {
                    storage::client_prefs::set_support_wait(&mut conn, user, order.as_deref()).await
                }
                None => storage::client_prefs::clear_support_wait(&mut conn, user).await,
            },
            Err(e) => Err(e.into()),
        };
        if let Err(e) = saved {
            tracing::warn!(error = %e, "support wait not saved");
        }
    }

    async fn startup(&self) {
        self.load_client_prefs().await;
        match self.api.bot().get_me().await {
            Ok(me) => {
                if let Some(name) = me.username {
                    let _ = self.bot_username.set(name);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "getMe failed, commands with @mention are accepted for any bot")
            }
        }
        if let Err(e) = self
            .api
            .bot()
            .set_my_commands(self.ui().bot_commands())
            .await
        {
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

    /// Отметить ссылки сообщения как взятые. `true` — среди них есть новая (ещё не взятая
    /// из этого же сообщения), её надо обработать.
    pub(crate) fn first_seen_links(
        &self,
        chat: i64,
        message_id: i32,
        found: &[Result<parsers::WalletLink, parsers::LinkError>],
    ) -> bool {
        let params: Vec<String> = found
            .iter()
            .filter_map(|f| f.as_ref().ok())
            .map(|l| format!("{}:{}", l.platform.db_name(), l.param))
            .collect();
        if params.is_empty() {
            return false;
        }
        self.memory(|m| {
            let key = (chat, message_id);
            if !m.link_messages.contains_key(&key) {
                m.link_order.push_back(key);
                while m.link_order.len() > LINK_MEMORY {
                    if let Some(old) = m.link_order.pop_front() {
                        m.link_messages.remove(&old);
                    }
                }
            }
            let seen = m.link_messages.entry(key).or_default();
            let mut fresh = false;
            for p in params {
                fresh |= seen.insert(p);
            }
            fresh
        })
    }

    /// Из сообщения уже взята ссылка (правкой или ответом-цитатой).
    pub(crate) fn links_taken(&self, chat: i64, message_id: i32) -> bool {
        self.memory(|m| {
            m.link_messages
                .get(&(chat, message_id))
                .is_some_and(|s| !s.is_empty())
        })
    }

    pub(crate) fn prefers_plain(&self, user: i64) -> bool {
        !self.cfg.rich_messages || self.memory(|m| m.plain_view.contains(&user))
    }
}
