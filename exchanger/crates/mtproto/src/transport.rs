//! `GrammersTransport` — реализация `userbot::Transport` на grammers 0.10.
//!
//! Устройство (LOVEC-PORTING §5.4, §9.2):
//! - один `SenderPool` на аккаунт, два клиента: `action` (`NoRetries`: `sendMessage`, нажатия,
//!   инлайн, web view, пинг) и `reads` (`AutoSleep` до 5 с: история, апдейты, `getUsers`).
//!   grammers по умолчанию сам повторяет запрос после `Io`, и этот повтор невидим для outbox —
//!   для денежных действий так нельзя;
//! - каждый вызов в `tokio::time::timeout`; таймаут = исход неизвестен и пересоздание пула:
//!   новый поток апдейтов начинает с `getDifference` от сохранённого pts, поэтому ответ бота,
//!   пришедший, пока соединение было мёртвым, не теряется;
//! - супервизор соединения: поток апдейтов + сторож (`Ping` / `updates.getState`) + задача
//!   `runner`. Падение любой из них (в т. ч. паника runner на странных данных сервера) —
//!   пересоздание пула с отступом 1 → 2 → … → 60 с. Потеря авторизации — остановка и
//!   `NotAuthorized` на все вызовы;
//! - апдейты: `catch_up = true` с сохранённым pts, поэтому ответы ботов, пришедшие во время
//!   простоя, догоняются через `getDifference`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use domain::Platform;
use grammers_client::Client;
use grammers_client::client::{
    AutoSleep, ClientConfiguration, NoRetries, UpdateStream, UpdatesConfiguration,
};
use grammers_mtsender::{ConnectionParams, InvocationError, SenderPool, SenderPoolHandle};
use grammers_tl_types::{self as tl, RemoteCall};
use tokio::sync::{Notify, broadcast, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;
use userbot::transport::{
    CallbackAnswer, Chat, InlineResults, RawMessage, Transport, TransportError,
};

use crate::config::{MtprotoConfig, PinnedBot};
use crate::convert::{self, BotIdentityError, ChatContext, SentMessage, WebAppKind};
use crate::crypto::SessionKey;
use crate::errors::{is_auth_error, map_invocation_error};
use crate::requests::{self, BotPeer, BotPeers, WebAppTarget};
use crate::session::{EncryptedSession, SessionError, SessionPersister, SessionStore, StoredUser};

/// Ёмкость канала `subscribe()`: отставший подписчик получит `Lagged` и догонит историей.
const BROADCAST_CAPACITY: usize = 1024;
/// Предел очереди апдейтов grammers (по умолчанию 100 — мало для догона после простоя).
const UPDATE_QUEUE_LIMIT: usize = 10_000;
const MAX_WEBAPP_CACHE: usize = 512;
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(60);
const UPDATE_ERRORS_BEFORE_RECONNECT: u32 = 10;
/// Каждый какой пинг заменять на `updates.getState` (проверка, что сессию не отозвали).
const AUTH_CHECK_EVERY_PINGS: u64 = 4;

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("invalid MTProto configuration: {0}")]
    InvalidConfig(String),
    #[error("no saved session for this account: run `exch login` first")]
    NoSession,
    #[error("account is not authorized: run `exch login` again")]
    NotAuthorized,
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("wallet bot identity check failed: {0}")]
    BotIdentity(#[from] BotIdentityError),
    #[error("timed out during {0}")]
    Timeout(&'static str),
    #[error("{what} failed: {error}")]
    Network {
        what: &'static str,
        error: TransportError,
    },
}

impl ConnectError {
    fn from_invocation(what: &'static str, e: &InvocationError) -> Self {
        if is_auth_error(e) {
            return Self::NotAuthorized;
        }
        Self::Network {
            what,
            error: map_invocation_error(e),
        }
    }
}

/// Какой клиент и таймаут использовать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    /// Действия: без автоповторов, таймаут `timeouts.action`.
    Action,
    /// Чтение: автоповтор после `Io` и короткого FLOOD_WAIT, таймаут `timeouts.read`.
    Read,
}

#[derive(Clone)]
struct Conn {
    action: Client,
    reads: Client,
    handle: SenderPoolHandle,
    /// Номер пула: таймаут запроса к старому пулу не должен ронять новый.
    generation: u64,
}

/// Живое соединение: пул, его runner и поток апдейтов.
struct Live {
    conn: Conn,
    runner: JoinHandle<()>,
    stream: UpdateStream,
}

/// Почему цикл соединения закончился.
enum Ended {
    Shutdown,
    AuthLost,
    Reconnect(&'static str),
}

struct Shared {
    cfg: MtprotoConfig,
    ctx: ChatContext,
    bots: BotPeers,
    session: Arc<EncryptedSession>,
    persister: Arc<SessionPersister>,
    conn: watch::Sender<Option<Conn>>,
    tx: broadcast::Sender<RawMessage>,
    webapps: Mutex<HashMap<String, WebAppKind>>,
    auth_lost: AtomicBool,
    cancel: CancellationToken,
    /// Разбудить сторожа: проверить соединение сразу (после обрыва во время запроса).
    nudge: Notify,
    /// Пересоздать пул (запрос к текущему пулу завис).
    rebuild: Notify,
    generation: AtomicU64,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Shared {
    fn remember_webapps(&self, markup: Option<&tl::enums::ReplyMarkup>) {
        let found = convert::webapp_buttons(markup);
        if found.is_empty() {
            return;
        }
        if let Ok(mut cache) = self.webapps.lock() {
            if cache.len() + found.len() > MAX_WEBAPP_CACHE {
                cache.clear();
            }
            cache.extend(found);
        }
    }

    fn webapp_kind(&self, url: &str) -> Option<WebAppKind> {
        self.webapps.lock().ok().and_then(|c| c.get(url).copied())
    }

    fn mark_auth_lost(&self) {
        if !self.auth_lost.swap(true, Ordering::SeqCst) {
            tracing::error!(
                account = %self.cfg.account,
                "Telegram session is no longer authorized: run `exch login` for this account"
            );
            self.conn.send_replace(None);
        }
    }
}

/// Настоящий аккаунт Telegram через MTProto.
pub struct GrammersTransport {
    shared: Arc<Shared>,
}

fn connection_params(cfg: &MtprotoConfig) -> ConnectionParams {
    ConnectionParams {
        device_model: cfg.device.device_model.clone(),
        system_version: cfg.device.system_version.clone(),
        app_version: cfg.device.app_version.clone(),
        system_lang_code: cfg.device.system_lang_code.clone(),
        lang_code: cfg.device.lang_code.clone(),
        use_ipv6: false,
        ..ConnectionParams::default()
    }
}

/// Пул соединений и два клиента над ним. Runner запускается отдельной задачей.
pub(crate) struct Pool {
    pub(crate) action: Client,
    pub(crate) reads: Client,
    pub(crate) handle: SenderPoolHandle,
    pub(crate) runner: JoinHandle<()>,
    pub(crate) updates:
        tokio::sync::mpsc::UnboundedReceiver<grammers_session::updates::UpdatesLike>,
}

impl Pool {
    pub(crate) fn start(cfg: &MtprotoConfig, session: Arc<EncryptedSession>) -> Self {
        let pool = SenderPool::with_configuration(session, cfg.api_id, connection_params(cfg));
        let handle = pool.handle.thin.clone();
        let action = Client::with_configuration(
            pool.handle.clone(),
            ClientConfiguration {
                retry_policy: Box::new(NoRetries),
                auto_cache_peers: false,
            },
        );
        let reads = Client::with_configuration(
            pool.handle,
            ClientConfiguration {
                retry_policy: Box::new(AutoSleep {
                    threshold: Duration::from_secs(5),
                    io_errors_as_flood_of: Some(Duration::from_secs(1)),
                }),
                auto_cache_peers: true,
            },
        );
        let runner = tokio::spawn(pool.runner.run());
        Self {
            action,
            reads,
            handle,
            runner,
            updates: pool.updates,
        }
    }

    /// Остановить пул: запросы в полёте получат `Dropped` (исход неизвестен).
    pub(crate) async fn stop(handle: &SenderPoolHandle, mut runner: JoinHandle<()>) {
        handle.quit();
        // Завершённый JoinHandle повторно не опрашиваем (tokio паникует).
        if runner.is_finished() {
            return;
        }
        if tokio::time::timeout(Duration::from_secs(3), &mut runner)
            .await
            .is_err()
        {
            runner.abort();
        }
    }
}

async fn timed<T>(
    limit: Duration,
    what: &'static str,
    fut: impl Future<Output = Result<T, InvocationError>>,
) -> Result<T, ConnectError> {
    match tokio::time::timeout(limit, fut).await {
        Err(_) => Err(ConnectError::Timeout(what)),
        Ok(Err(e)) => Err(ConnectError::from_invocation(what, &e)),
        Ok(Ok(v)) => Ok(v),
    }
}

/// Свой аккаунт: id и access hash (`users.getUsers([inputUserSelf])`).
async fn fetch_self(reads: &Client, limit: Duration) -> Result<StoredUser, ConnectError> {
    let users = timed(
        limit,
        "users.getUsers(self)",
        reads.invoke(&tl::functions::users::GetUsers {
            id: vec![tl::enums::InputUser::UserSelf],
        }),
    )
    .await?;
    match users.into_iter().next() {
        Some(tl::enums::User::User(u)) => Ok(StoredUser {
            id: u.id,
            access_hash: u.access_hash,
            bot: Some(u.bot),
            is_self: Some(true),
        }),
        _ => Err(ConnectError::NotAuthorized),
    }
}

/// Найти и сверить закреплённого бота: сначала по сохранённому access hash, иначе — один
/// `contacts.resolveUsername` по username **из конфига** и проверка, что id совпал с закреплённым.
async fn resolve_bot(
    reads: &Client,
    session: &EncryptedSession,
    pinned: &PinnedBot,
    limit: Duration,
) -> Result<BotPeer, ConnectError> {
    let platform = pinned.platform;
    let remember = |access_hash: i64| -> Result<BotPeer, ConnectError> {
        session.remember_user(StoredUser {
            id: pinned.id,
            access_hash: Some(access_hash),
            bot: Some(true),
            is_self: Some(false),
        })?;
        Ok(BotPeer {
            platform,
            id: pinned.id,
            access_hash,
        })
    };

    if let Some(hash) = session.user(pinned.id).and_then(|u| u.access_hash) {
        let request = tl::functions::users::GetUsers {
            id: vec![
                tl::types::InputUser {
                    user_id: pinned.id,
                    access_hash: hash,
                }
                .into(),
            ],
        };
        match tokio::time::timeout(limit, reads.invoke(&request)).await {
            Err(_) => return Err(ConnectError::Timeout("users.getUsers(wallet bot)")),
            Ok(Ok(users)) => match users.first() {
                Some(user @ tl::enums::User::User(_)) => {
                    let hash = convert::verify_bot_user(user, pinned)?;
                    return remember(hash);
                }
                _ => {
                    tracing::warn!(
                        ?platform,
                        "cached wallet bot peer not returned, resolving again"
                    );
                }
            },
            // Устаревший access hash — пробуем resolve; прочие ошибки — наверх.
            Ok(Err(InvocationError::Rpc(rpc))) if rpc.code == 400 => {
                tracing::warn!(?platform, error = %rpc, "cached wallet bot peer rejected, resolving again");
            }
            Ok(Err(e)) => {
                return Err(ConnectError::from_invocation(
                    "users.getUsers(wallet bot)",
                    &e,
                ));
            }
        }
    }

    let resolved = timed(
        limit,
        "contacts.resolveUsername(wallet bot)",
        reads.invoke(&tl::functions::contacts::ResolveUsername {
            username: pinned.username.trim_start_matches('@').to_owned(),
            referer: None,
        }),
    )
    .await?;
    let tl::enums::contacts::ResolvedPeer::Peer(resolved) = resolved;
    let got = match resolved.peer {
        tl::enums::Peer::User(u) => u.user_id,
        tl::enums::Peer::Chat(c) => -c.chat_id,
        tl::enums::Peer::Channel(c) => -c.channel_id,
    };
    if got != pinned.id {
        return Err(BotIdentityError::IdMismatch {
            platform,
            expected: pinned.id,
            got,
        }
        .into());
    }
    let user =
        resolved
            .users
            .iter()
            .find(|u| u.id() == pinned.id)
            .ok_or(BotIdentityError::NotFound {
                platform,
                expected: pinned.id,
            })?;
    let hash = convert::verify_bot_user(user, pinned)?;
    remember(hash)
}

impl GrammersTransport {
    /// Подключить аккаунт: расшифровать сессию, проверить авторизацию, сверить ботов
    /// кошельков по закреплённым id и username, запустить поток апдейтов и сторожа.
    pub async fn connect(
        cfg: MtprotoConfig,
        key: SessionKey,
        store: Arc<dyn SessionStore>,
    ) -> Result<Self, ConnectError> {
        cfg.validate().map_err(ConnectError::InvalidConfig)?;
        let span = tracing::info_span!("mtproto", account = %cfg.account);
        async move {
            let sealed = store
                .load()
                .await
                .map_err(SessionError::from)?
                .ok_or(ConnectError::NoSession)?;
            let allowed: Vec<i64> = cfg.bots.iter().map(|b| b.id).collect();
            let session = Arc::new(EncryptedSession::open(
                &key,
                &cfg.account,
                &sealed,
                allowed,
            )?);
            let snapshot = session.snapshot()?;
            if snapshot.self_user().is_none() || !snapshot.has_home_auth_key() {
                return Err(ConnectError::NotAuthorized);
            }
            drop(snapshot);
            let key = Arc::new(key);
            let persister = Arc::new(SessionPersister::new(
                Arc::clone(&session),
                store,
                key,
                cfg.account.clone(),
            ));

            let pool = Pool::start(&cfg, Arc::clone(&session));
            match Self::bootstrap(cfg, session, persister, pool).await {
                Ok(t) => Ok(t),
                Err((e, handle, runner)) => {
                    Pool::stop(&handle, runner).await;
                    Err(e)
                }
            }
        }
        .instrument(span)
        .await
    }

    #[allow(clippy::type_complexity)]
    async fn bootstrap(
        cfg: MtprotoConfig,
        session: Arc<EncryptedSession>,
        persister: Arc<SessionPersister>,
        pool: Pool,
    ) -> Result<Self, (ConnectError, SenderPoolHandle, JoinHandle<()>)> {
        let Pool {
            action,
            reads,
            handle,
            runner,
            updates,
        } = pool;
        let limit = cfg.timeouts.startup;
        let setup = async {
            let me = fetch_self(&reads, limit).await?;
            session.remember_user(me)?;
            let bots = BotPeers {
                cryptobot: resolve_bot(&reads, &session, &cfg.bots.cryptobot, limit).await?,
                xrocket: resolve_bot(&reads, &session, &cfg.bots.xrocket, limit).await?,
            };
            let home_dc = grammers_session::Session::home_dc_id(session.as_ref())?;
            let stream = reads
                .stream_updates(
                    updates,
                    UpdatesConfiguration {
                        catch_up: true,
                        update_queue_limit: Some(UPDATE_QUEUE_LIMIT),
                    },
                )
                .await
                .map_err(|e| ConnectError::Network {
                    what: "stream_updates",
                    error: TransportError::Other(e.to_string()),
                })?;
            Ok::<_, ConnectError>((me, bots, home_dc, stream))
        };
        let (me, bots, home_dc, stream) = match setup.await {
            Ok(v) => v,
            Err(e) => return Err((e, handle, runner)),
        };
        tracing::info!(
            self_id = me.id,
            home_dc,
            "MTProto account connected, wallet bots verified"
        );

        let conn = Conn {
            action,
            reads,
            handle,
            generation: 0,
        };
        let (conn_tx, _) = watch::channel(Some(conn.clone()));
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let shared = Arc::new(Shared {
            ctx: ChatContext::new(me.id, &cfg.bots),
            bots,
            session,
            persister,
            conn: conn_tx,
            tx,
            webapps: Mutex::new(HashMap::new()),
            auth_lost: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            nudge: Notify::new(),
            rebuild: Notify::new(),
            generation: AtomicU64::new(0),
            tasks: Mutex::new(Vec::new()),
            cfg,
        });

        let span = tracing::Span::current();
        let persist_task = tokio::spawn(
            Arc::clone(&shared.persister)
                .run(
                    shared.cancel.clone(),
                    shared.cfg.timeouts.update_state_sync_every,
                )
                .instrument(span.clone()),
        );
        let live = Live {
            conn,
            runner,
            stream,
        };
        let supervisor = tokio::spawn(supervise(Arc::clone(&shared), live).instrument(span));
        if let Ok(mut tasks) = shared.tasks.lock() {
            tasks.push(persist_task);
            tasks.push(supervisor);
        }
        Ok(Self { shared })
    }

    /// Id аккаунта юзербота.
    pub fn self_id(&self) -> i64 {
        self.shared.ctx.self_id
    }

    /// Сверенные при старте боты кошельков.
    pub fn bot_peers(&self) -> BotPeers {
        self.shared.bots
    }

    /// Авторизация не потеряна (иначе все вызовы — `NotAuthorized`).
    pub fn is_authorized(&self) -> bool {
        !self.shared.auth_lost.load(Ordering::SeqCst)
    }

    /// Есть ли сейчас живое соединение.
    pub fn is_connected(&self) -> bool {
        self.shared.conn.borrow().is_some()
    }

    /// Остановить соединение и сохранить сессию.
    pub async fn shutdown(&self) {
        self.shared.cancel.cancel();
        let tasks = self
            .shared
            .tasks
            .lock()
            .map(|mut t| std::mem::take(&mut *t))
            .unwrap_or_default();
        for task in tasks {
            if let Err(e) = task.await
                && e.is_panic()
            {
                tracing::error!(account = %self.shared.cfg.account, "MTProto task panicked during shutdown");
            }
        }
    }

    /// Перечитать сообщения по id (например, ответ, пришедший `UpdateShortMessage` без кнопок).
    pub async fn refetch(
        &self,
        chat: Chat,
        ids: &[i32],
    ) -> Result<Vec<RawMessage>, TransportError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let result = self
            .call(
                Lane::Read,
                &requests::get_messages(ids),
                "messages.getMessages",
            )
            .await?;
        let messages = convert::messages_of(result);
        Ok(self.to_raw_messages(&messages, chat))
    }

    fn to_raw_messages(&self, messages: &[tl::enums::Message], chat: Chat) -> Vec<RawMessage> {
        messages
            .iter()
            .filter_map(|m| {
                if let tl::enums::Message::Message(inner) = m {
                    self.shared.remember_webapps(inner.reply_markup.as_ref());
                }
                convert::message_to_raw(m, &self.shared.ctx)
            })
            .filter(|m| m.chat == chat)
            .collect()
    }

    async fn current_conn(&self) -> Result<Conn, TransportError> {
        if self.shared.auth_lost.load(Ordering::SeqCst) {
            return Err(TransportError::NotAuthorized);
        }
        let mut rx = self.shared.conn.subscribe();
        let wait = async {
            loop {
                let current = rx.borrow_and_update().clone();
                if let Some(conn) = current {
                    return Some(conn);
                }
                if rx.changed().await.is_err() {
                    return None;
                }
            }
        };
        match tokio::time::timeout(self.shared.cfg.timeouts.reconnect_wait, wait).await {
            Ok(Some(conn)) => Ok(conn),
            _ if self.shared.auth_lost.load(Ordering::SeqCst) => Err(TransportError::NotAuthorized),
            _ => Err(TransportError::Other(
                "not connected to Telegram; request was not sent".into(),
            )),
        }
    }

    async fn call<R: RemoteCall>(
        &self,
        lane: Lane,
        request: &R,
        what: &'static str,
    ) -> Result<R::Return, TransportError> {
        let conn = self.current_conn().await?;
        let (client, limit) = match lane {
            Lane::Action => (&conn.action, self.shared.cfg.timeouts.action),
            Lane::Read => (&conn.reads, self.shared.cfg.timeouts.read),
        };
        match tokio::time::timeout(limit, client.invoke(request)).await {
            Ok(Ok(result)) => Ok(result),
            Err(_) => {
                tracing::warn!(
                    what,
                    "MTProto request timed out, outcome unknown; recreating the connection"
                );
                if conn.generation == self.shared.generation.load(Ordering::SeqCst) {
                    self.shared.rebuild.notify_one();
                }
                Err(TransportError::Timeout)
            }
            Ok(Err(e)) => {
                let mapped = map_invocation_error(&e);
                match &mapped {
                    TransportError::NotAuthorized => self.shared.mark_auth_lost(),
                    TransportError::FloodWait(wait) => {
                        tracing::warn!(what, wait_s = wait.as_secs(), "FLOOD_WAIT");
                    }
                    m if m.outcome_unknown() => {
                        tracing::warn!(what, error = %e, "MTProto request failed, outcome unknown");
                        self.shared.nudge.notify_one();
                    }
                    _ => tracing::debug!(what, error = %e, "MTProto request rejected"),
                }
                Err(mapped)
            }
        }
    }

    /// Наше только что отправленное сообщение из ответа сервера.
    async fn sent_message(
        &self,
        updates: &tl::enums::Updates,
        random_id: i64,
        chat: Chat,
        text: Option<&str>,
    ) -> Result<RawMessage, TransportError> {
        if let tl::enums::Updates::Updates(tl::types::Updates { updates: list, .. })
        | tl::enums::Updates::Combined(tl::types::UpdatesCombined { updates: list, .. }) =
            updates
        {
            for u in list {
                if let Some(m) = convert::update_message(u) {
                    self.shared.remember_webapps(m.reply_markup.as_ref());
                }
            }
        }
        match convert::find_sent_message(updates, random_id, chat, &self.shared.ctx) {
            SentMessage::Message(m) => Ok(m),
            SentMessage::Short { id, date, entities } => Ok(convert::sent_text_message(
                chat,
                id,
                date,
                text.unwrap_or_default(),
                entities.as_deref(),
            )),
            SentMessage::IdOnly { id, date } => {
                if let Some(text) = text {
                    return Ok(convert::sent_text_message(chat, id, date, text, None));
                }
                // Инлайн-сообщение без тела в ответе: перечитать, чтобы получить кнопки.
                match self.refetch(chat, &[id]).await {
                    Ok(mut found) if !found.is_empty() => Ok(found.remove(0)),
                    _ => Ok(convert::sent_text_message(chat, id, date, "", None)),
                }
            }
            SentMessage::NotFound => {
                // Сервер принял запрос, но сообщения с нашим random_id в ответе нет. Считаем
                // исход неизвестным: повтор с тем же random_id безопасен и прояснит его.
                tracing::warn!(?chat, "sent message not found in the server response");
                Err(TransportError::Timeout)
            }
        }
    }
}

impl Drop for GrammersTransport {
    fn drop(&mut self) {
        // Фоновые задачи завершатся сами и сохранят сессию.
        self.shared.cancel.cancel();
    }
}

#[async_trait]
impl Transport for GrammersTransport {
    async fn send_text(
        &self,
        chat: Chat,
        text: &str,
        random_id: i64,
    ) -> Result<RawMessage, TransportError> {
        let request = requests::send_message(self.shared.bots.input_peer(chat), text, random_id);
        let updates = self
            .call(Lane::Action, &request, "messages.sendMessage")
            .await?;
        self.sent_message(&updates, random_id, chat, Some(text))
            .await
    }

    async fn press(
        &self,
        chat: Chat,
        msg_id: i32,
        data: &[u8],
    ) -> Result<CallbackAnswer, TransportError> {
        let request = requests::press_callback(self.shared.bots.input_peer(chat), msg_id, data);
        let answer = self
            .call(Lane::Action, &request, "messages.getBotCallbackAnswer")
            .await?;
        Ok(convert::callback_answer(&answer))
    }

    async fn inline_query(
        &self,
        bot: Platform,
        query: &str,
    ) -> Result<InlineResults, TransportError> {
        // Через клиент без автоповторов: не проверено, не создаёт ли бот чек уже на запросе
        // (LOVEC-PORTING §8.1, I1).
        let request = requests::inline_query(self.shared.bots.get(bot).input_user(), query);
        let results = self
            .call(Lane::Action, &request, "messages.getInlineBotResults")
            .await?;
        Ok(convert::inline_results(&results))
    }

    async fn send_inline(
        &self,
        to: Chat,
        bot: Platform,
        results: &InlineResults,
        result_id: &str,
        random_id: i64,
    ) -> Result<RawMessage, TransportError> {
        if !results.results.iter().any(|r| r.id == result_id) {
            return Err(TransportError::Other(
                "result id is not among the inline results; nothing was sent".into(),
            ));
        }
        let request = requests::send_inline_result(
            self.shared.bots.input_peer(to),
            results.query_id,
            result_id,
            random_id,
        );
        let updates = self
            .call(Lane::Action, &request, "messages.sendInlineBotResult")
            .await?;
        let message = self.sent_message(&updates, random_id, to, None).await?;
        if message.via_bot.is_some_and(|via| via != bot) {
            tracing::warn!(?bot, via = ?message.via_bot, "inline message came via another wallet bot");
        }
        Ok(message)
    }

    async fn open_webapp(&self, bot: Platform, button_url: &str) -> Result<String, TransportError> {
        let peer = *self.shared.bots.get(bot);
        let pinned = self.shared.cfg.bots.get(bot);
        let platform = self.shared.cfg.webapp_platform.as_str();
        let target = requests::webapp_target(
            button_url,
            self.shared.webapp_kind(button_url),
            &pinned.username,
        )
        .map_err(|e| TransportError::Other(format!("{e}; nothing was sent")))?;
        // URL ответа содержит подписанные initData: в логи не пишем.
        let result = match target {
            WebAppTarget::WebView { url } => {
                self.call(
                    Lane::Action,
                    &requests::request_webview(&peer, &url, platform),
                    "messages.requestWebView",
                )
                .await?
            }
            WebAppTarget::Simple { url } => {
                self.call(
                    Lane::Action,
                    &requests::request_simple_webview(&peer, &url, platform),
                    "messages.requestSimpleWebView",
                )
                .await?
            }
            WebAppTarget::App {
                short_name,
                start_param,
            } => {
                self.call(
                    Lane::Action,
                    &requests::request_app_webview(
                        &peer,
                        &short_name,
                        start_param.as_deref(),
                        platform,
                    ),
                    "messages.requestAppWebView",
                )
                .await?
            }
        };
        Ok(convert::webview_url(&result))
    }

    async fn history(
        &self,
        chat: Chat,
        after_id: i32,
        limit: u32,
    ) -> Result<Vec<RawMessage>, TransportError> {
        let request = requests::get_history(self.shared.bots.input_peer(chat), after_id, limit);
        let result = self
            .call(Lane::Read, &request, "messages.getHistory")
            .await?;
        let messages = convert::messages_of(result);
        for m in &messages {
            if let tl::enums::Message::Message(inner) = m {
                self.shared.remember_webapps(inner.reply_markup.as_ref());
            }
        }
        let limit = usize::try_from(limit.clamp(1, requests::MAX_HISTORY_LIMIT)).unwrap_or(1);
        Ok(convert::history_page(
            &messages,
            chat,
            after_id,
            limit,
            &self.shared.ctx,
        ))
    }

    fn subscribe(&self) -> broadcast::Receiver<RawMessage> {
        self.shared.tx.subscribe()
    }
}

/// Цикл соединения: держит текущий пул, пересоздаёт его при сбоях.
async fn supervise(shared: Arc<Shared>, mut live: Live) {
    loop {
        let reason = drive(&shared, &mut live).await;
        shared.conn.send_replace(None);
        if let Err(e) = live.stream.sync_update_state().await {
            tracing::warn!(error = %e, "cannot sync update state");
        }
        let Live {
            conn,
            runner,
            stream,
        } = live;
        drop(stream);
        Pool::stop(&conn.handle, runner).await;
        match reason {
            Ended::Shutdown => break,
            Ended::AuthLost => {
                shared.mark_auth_lost();
                break;
            }
            Ended::Reconnect(why) => tracing::warn!(why, "MTProto connection lost, reconnecting"),
        }

        // Первая попытка — сразу, дальше с отступом 1 → 2 → … → 60 с.
        let mut backoff = Duration::ZERO;
        live = loop {
            tokio::select! {
                _ = shared.cancel.cancelled() => {
                    persist_on_exit(&shared).await;
                    return;
                }
                attempt = async {
                    tokio::time::sleep(backoff).await;
                    establish(&shared).await
                } => match attempt {
                    Ok(next) => {
                        tracing::info!("MTProto connection re-established");
                        break next;
                    }
                    Err(ConnectError::NotAuthorized) => {
                        shared.mark_auth_lost();
                        persist_on_exit(&shared).await;
                        return;
                    }
                    Err(e) => {
                        backoff = (backoff * 2).clamp(Duration::from_secs(1), MAX_RECONNECT_BACKOFF);
                        tracing::warn!(error = %e, retry_in_s = backoff.as_secs(), "MTProto reconnect failed");
                    }
                },
            }
        };
        shared.conn.send_replace(Some(live.conn.clone()));
    }
    persist_on_exit(&shared).await;
}

async fn persist_on_exit(shared: &Shared) {
    if let Err(e) = shared.persister.flush().await {
        tracing::error!(error = %e, "cannot save MTProto session");
    }
}

/// Новый пул после сбоя: проверка связи и авторизации (`updates.getState`), поток апдейтов
/// с `catch_up` — он начнёт с `getDifference` от сохранённого pts.
async fn establish(shared: &Shared) -> Result<Live, ConnectError> {
    let Pool {
        action,
        reads,
        handle,
        runner,
        updates,
    } = Pool::start(&shared.cfg, Arc::clone(&shared.session));
    let setup = async {
        timed(
            shared.cfg.timeouts.startup,
            "updates.getState",
            action.invoke(&tl::functions::updates::GetState {}),
        )
        .await?;
        let stream = reads
            .stream_updates(
                updates,
                UpdatesConfiguration {
                    catch_up: true,
                    update_queue_limit: Some(UPDATE_QUEUE_LIMIT),
                },
            )
            .await
            .map_err(|e| ConnectError::Network {
                what: "stream_updates",
                error: TransportError::Other(e.to_string()),
            })?;
        Ok::<_, ConnectError>(stream)
    };
    match setup.await {
        Ok(stream) => Ok(Live {
            conn: Conn {
                action,
                reads,
                handle,
                generation: shared.generation.fetch_add(1, Ordering::SeqCst) + 1,
            },
            runner,
            stream,
        }),
        Err(e) => {
            Pool::stop(&handle, runner).await;
            Err(e)
        }
    }
}

async fn drive(shared: &Shared, live: &mut Live) -> Ended {
    let Live {
        conn,
        runner,
        stream,
    } = live;
    tokio::select! {
        _ = shared.cancel.cancelled() => Ended::Shutdown,
        _ = shared.rebuild.notified() => Ended::Reconnect("a request timed out"),
        result = runner => {
            match result {
                Err(e) if e.is_panic() => {
                    tracing::error!("grammers runner panicked; recreating the connection pool");
                }
                _ => tracing::warn!("grammers runner stopped"),
            }
            Ended::Reconnect("runner stopped")
        }
        ended = update_loop(shared, stream) => ended,
        ended = watchdog(shared, conn) => ended,
    }
}

/// Поток апдейтов → `RawMessage` для чатов с ботами и «Избранного» → broadcast.
async fn update_loop(shared: &Shared, stream: &mut UpdateStream) -> Ended {
    let mut errors = 0u32;
    let mut last_sync = tokio::time::Instant::now();
    loop {
        match stream.next_raw().await {
            Ok((update, _state, _peers)) => {
                errors = 0;
                if let Some(m) = convert::update_message(&update) {
                    shared.remember_webapps(m.reply_markup.as_ref());
                }
                if let Some(raw) = convert::update_to_raw(&update, &shared.ctx) {
                    tracing::debug!(chat = ?raw.chat, id = raw.id, edited = raw.edit_date.is_some(), "wallet chat message");
                    // Нет подписчиков — не ошибка: сообщение догонится историей.
                    let _ = shared.tx.send(raw);
                }
                if last_sync.elapsed() >= shared.cfg.timeouts.update_state_sync_every {
                    if let Err(e) = stream.sync_update_state().await {
                        tracing::warn!(error = %e, "cannot sync update state");
                    }
                    last_sync = tokio::time::Instant::now();
                }
            }
            Err(InvocationError::Dropped) => return Ended::Reconnect("update channel closed"),
            Err(e) if is_auth_error(&e) => return Ended::AuthLost,
            Err(e) => {
                errors += 1;
                tracing::warn!(error = %e, errors, "update stream error");
                if errors >= UPDATE_ERRORS_BEFORE_RECONNECT {
                    return Ended::Reconnect("update stream keeps failing");
                }
                let pause = Duration::from_millis(100)
                    .saturating_mul(1 << errors.min(6))
                    .min(Duration::from_secs(5));
                tokio::time::sleep(pause).await;
            }
        }
    }
}

/// Сторож: `Ping` (и каждый 4-й раз `updates.getState`) с таймаутом; несколько сбоев подряд —
/// пересоздание пула. Обрыв (`Io`) grammers лечит сам на следующем запросе, а «тихое»
/// соединение (TCP жив, сервер молчит) без пересоздания не лечится: в 0.10 нет pong-таймаута.
async fn watchdog(shared: &Shared, conn: &Conn) -> Ended {
    let t = &shared.cfg.timeouts;
    let mut failures = 0u32;
    let mut tick = 0u64;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(t.ping_every) => {}
            _ = shared.nudge.notified() => {}
        }
        tick = tick.wrapping_add(1);
        let result = if tick.is_multiple_of(AUTH_CHECK_EVERY_PINGS) {
            tokio::time::timeout(
                t.ping_timeout,
                conn.action.invoke(&tl::functions::updates::GetState {}),
            )
            .await
            .map(|r| r.map(drop))
        } else {
            tokio::time::timeout(
                t.ping_timeout,
                conn.action.invoke(&tl::functions::Ping {
                    ping_id: i64::try_from(tick).unwrap_or_default(),
                }),
            )
            .await
            .map(|r| r.map(drop))
        };
        match result {
            Ok(Ok(())) => failures = 0,
            Ok(Err(e)) if is_auth_error(&e) => return Ended::AuthLost,
            // Сервер ответил ошибкой — соединение живо.
            Ok(Err(InvocationError::Rpc(_))) => failures = 0,
            other => {
                failures += 1;
                let error = match other {
                    Ok(Err(e)) => e.to_string(),
                    _ => "timeout".to_owned(),
                };
                tracing::warn!(failures, %error, "MTProto ping failed");
                if failures >= t.ping_failures_before_reconnect {
                    return Ended::Reconnect("ping keeps failing");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddrV4;

    use secrecy::SecretString;

    use super::*;
    use crate::crypto::seal;
    use crate::session::{MemoryStore, SessionSnapshot};

    fn cfg() -> MtprotoConfig {
        MtprotoConfig::new("ub-test", 12345, SecretString::from("hash"))
    }

    fn key() -> SessionKey {
        SessionKey::from_bytes(1, &[4; 32])
    }

    async fn connect_with(store: MemoryStore) -> Result<GrammersTransport, ConnectError> {
        GrammersTransport::connect(cfg(), key(), Arc::new(store)).await
    }

    #[tokio::test]
    async fn connect_without_session_asks_for_login() {
        let err = connect_with(MemoryStore::default()).await.err().unwrap();
        assert!(matches!(err, ConnectError::NoSession), "{err:?}");
    }

    #[tokio::test]
    async fn connect_with_unauthorized_session_fails_before_network() {
        // Сессия без входа (нет своего пользователя и auth key) — до сети не доходим.
        let plain = SessionSnapshot::default().encode().unwrap();
        let sealed = seal(&key(), "ub-test", &plain).unwrap();
        let err = connect_with(MemoryStore::new(Some(sealed)))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, ConnectError::NotAuthorized), "{err:?}");
    }

    #[tokio::test]
    async fn connect_with_foreign_session_is_rejected() {
        let plain = SessionSnapshot::default().encode().unwrap();
        let sealed = seal(&key(), "ub-other", &plain).unwrap();
        let err = connect_with(MemoryStore::new(Some(sealed)))
            .await
            .err()
            .unwrap();
        assert!(
            matches!(err, ConnectError::Session(SessionError::Crypto(_))),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn connect_rejects_bad_config() {
        let mut bad = cfg();
        bad.bots.xrocket.id = bad.bots.cryptobot.id;
        let err = GrammersTransport::connect(bad, key(), Arc::new(MemoryStore::default()))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, ConnectError::InvalidConfig(_)), "{err:?}");
    }

    fn session_pointing_to(addr: std::net::SocketAddr) -> Arc<EncryptedSession> {
        let std::net::SocketAddr::V4(v4) = addr else {
            panic!("ipv4 expected");
        };
        let mut snap = SessionSnapshot::default();
        let home = snap.home_dc;
        let dc = snap.dc_options.get_mut(&home).unwrap();
        dc.ipv4 = SocketAddrV4::new(*v4.ip(), v4.port());
        // С готовым auth key grammers не делает обмен ключами: сразу шлёт запрос.
        dc.auth_key = Some([1; 256]);
        Arc::new(EncryptedSession::new(snap, []))
    }

    /// «Тихий» DC (TCP принят, ответа нет): запрос не висит дольше нашего таймаута, а пул
    /// останавливается, хотя runner grammers застрял в подключении без таймаута
    /// (LOVEC-PORTING B3).
    #[tokio::test]
    async fn silent_dc_times_out_and_pool_still_stops() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hold = tokio::spawn(async move {
            let mut sockets = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                sockets.push(socket);
            }
        });
        let pool = Pool::start(&cfg(), session_pointing_to(addr));
        let started = tokio::time::Instant::now();
        let result = timed(
            Duration::from_millis(300),
            "ping",
            pool.action.invoke(&tl::functions::Ping { ping_id: 1 }),
        )
        .await;
        assert!(
            matches!(result, Err(ConnectError::Timeout("ping"))),
            "{result:?}"
        );
        Pool::stop(&pool.handle, pool.runner).await;
        assert!(started.elapsed() < Duration::from_secs(10));
        hold.abort();
    }

    /// Закрытый порт: ошибка подключения считается «исход неизвестен» (консервативно).
    #[tokio::test]
    async fn refused_connection_is_outcome_unknown() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let pool = Pool::start(&cfg(), session_pointing_to(addr));
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            pool.action.invoke(&tl::functions::Ping { ping_id: 1 }),
        )
        .await
        .unwrap();
        let mapped = map_invocation_error(&result.unwrap_err());
        assert!(mapped.outcome_unknown(), "{mapped:?}");
        Pool::stop(&pool.handle, pool.runner).await;
    }
}
