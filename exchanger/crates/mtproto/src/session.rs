//! Сессия MTProto в памяти с зашифрованным сохранением (LOVEC-PORTING §5.5).
//!
//! В grammers 0.10 у `MemorySession` нет экспорта, а `SqliteSession` пишет auth key на диск
//! открытым текстом, поэтому здесь своя реализация трейта `grammers_session::Session`:
//!
//! - состояние ([`SessionSnapshot`]): домашний DC, адреса и auth key DC, peer'ы — **только**
//!   свой аккаунт и закреплённые боты кошельков, состояние апдейтов (pts/qts/date/seq);
//! - сериализация — свой компактный бинарный формат в `Zeroizing<Vec<u8>>` с точной ёмкостью
//!   (без промежуточных строк с hex auth key, которые остались бы в куче);
//! - сохранение — через [`SessionStore`] только в виде [`SealedSession`]; смена DC, auth key и
//!   peer'ов сохраняется сразу, состояние апдейтов — с задержкой ([`SessionPersister`]).

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use grammers_session::types::{
    ChannelState, DcOption, PeerId, PeerInfo, PeerKind, UpdateState, UpdatesState,
};
use grammers_session::{BoxFuture, Session, SessionData};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, Zeroizing};

use crate::crypto::{self, CryptoError, SealedSession, SessionKey};

const SNAPSHOT_MAGIC: &[u8; 4] = b"EXSD";
const SNAPSHOT_VERSION: u8 = 1;
const AUTH_KEY_LEN: usize = 256;
const MAX_DC_OPTIONS: usize = 64;
const MAX_PEERS: usize = 64;
const MAX_CHANNELS: usize = 100_000;

/// Пользователь, сохранённый в сессии (свой аккаунт или бот кошелька).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredUser {
    pub id: i64,
    pub access_hash: Option<i64>,
    pub bot: Option<bool>,
    pub is_self: Option<bool>,
}

impl StoredUser {
    fn to_info(self) -> PeerInfo {
        PeerInfo::User {
            id: self.id,
            auth: self
                .access_hash
                .map(grammers_session::types::PeerAuth::from_hash),
            bot: self.bot,
            is_self: self.is_self,
        }
    }
}

/// Полное состояние сессии. Auth key стираются из памяти при drop.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub home_dc: i32,
    pub dc_options: BTreeMap<i32, DcOption>,
    pub users: BTreeMap<i64, StoredUser>,
    pub updates: UpdatesState,
}

impl std::fmt::Debug for SessionSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSnapshot")
            .field("home_dc", &self.home_dc)
            .field(
                "dcs_with_auth_key",
                &self
                    .dc_options
                    .values()
                    .filter(|o| o.auth_key.is_some())
                    .map(|o| o.id)
                    .collect::<Vec<_>>(),
            )
            .field("users", &self.users.len())
            .finish_non_exhaustive()
    }
}

impl Drop for SessionSnapshot {
    fn drop(&mut self) {
        for option in self.dc_options.values_mut() {
            if let Some(key) = option.auth_key.as_mut() {
                key.zeroize();
            }
        }
    }
}

impl Default for SessionSnapshot {
    /// Новая сессия: статическая таблица DC grammers, без auth key и без входа.
    fn default() -> Self {
        let data = SessionData::default();
        Self {
            home_dc: data.home_dc,
            dc_options: data
                .dc_options
                .values()
                .map(|o| (o.id, o.clone()))
                .collect(),
            users: BTreeMap::new(),
            updates: UpdatesState::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("session snapshot is truncated")]
    Truncated,
    #[error("not a session snapshot or unsupported version")]
    BadHeader,
    #[error("session snapshot has invalid field: {0}")]
    Invalid(&'static str),
    #[error("session snapshot has trailing bytes")]
    TrailingBytes,
}

fn opt_bool_to_u8(v: Option<bool>) -> u8 {
    match v {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }
}

fn u8_to_opt_bool(v: u8) -> Result<Option<bool>, SnapshotError> {
    match v {
        0 => Ok(None),
        1 => Ok(Some(false)),
        2 => Ok(Some(true)),
        _ => Err(SnapshotError::Invalid("optional bool")),
    }
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        if self.buf.len() < n {
            return Err(SnapshotError::Truncated);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], SnapshotError> {
        self.take(N)?
            .try_into()
            .map_err(|_| SnapshotError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, SnapshotError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, SnapshotError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, SnapshotError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    fn count(&mut self, max: usize, what: &'static str) -> Result<usize, SnapshotError> {
        let n = usize::from(self.u16()?);
        if n > max {
            return Err(SnapshotError::Invalid(what));
        }
        Ok(n)
    }

    fn count32(&mut self, max: usize, what: &'static str) -> Result<usize, SnapshotError> {
        let n = usize::try_from(self.i32()?).map_err(|_| SnapshotError::Invalid(what))?;
        if n > max {
            return Err(SnapshotError::Invalid(what));
        }
        Ok(n)
    }
}

impl SessionSnapshot {
    fn encoded_len(&self) -> usize {
        let dcs: usize = self
            .dc_options
            .values()
            .map(|o| {
                4 + 6
                    + 18
                    + 1
                    + if o.auth_key.is_some() {
                        AUTH_KEY_LEN
                    } else {
                        0
                    }
            })
            .sum();
        4 + 1
            + 4
            + 2
            + dcs
            + 2
            + self.users.len() * (8 + 1 + 8 + 1 + 1)
            + 16
            + 4
            + self.updates.channels.len() * 12
    }

    /// Сериализовать в буфер, который стирается при drop. Ёмкость считается заранее, чтобы
    /// вектор не перевыделялся (иначе копия auth key осталась бы в освобождённой памяти).
    pub fn encode(&self) -> Result<Zeroizing<Vec<u8>>, SnapshotError> {
        if self.dc_options.len() > MAX_DC_OPTIONS {
            return Err(SnapshotError::Invalid("too many dc options"));
        }
        if self.users.len() > MAX_PEERS {
            return Err(SnapshotError::Invalid("too many peers"));
        }
        if self.updates.channels.len() > MAX_CHANNELS {
            return Err(SnapshotError::Invalid("too many channel states"));
        }
        let len = self.encoded_len();
        let mut out = Zeroizing::new(Vec::with_capacity(len));
        out.extend_from_slice(SNAPSHOT_MAGIC);
        out.push(SNAPSHOT_VERSION);
        out.extend_from_slice(&self.home_dc.to_le_bytes());

        let count = |n: usize| u16::try_from(n).map_err(|_| SnapshotError::Invalid("count"));
        out.extend_from_slice(&count(self.dc_options.len())?.to_le_bytes());
        for o in self.dc_options.values() {
            out.extend_from_slice(&o.id.to_le_bytes());
            out.extend_from_slice(&o.ipv4.ip().octets());
            out.extend_from_slice(&o.ipv4.port().to_le_bytes());
            out.extend_from_slice(&o.ipv6.ip().octets());
            out.extend_from_slice(&o.ipv6.port().to_le_bytes());
            match &o.auth_key {
                Some(key) => {
                    out.push(1);
                    out.extend_from_slice(key);
                }
                None => out.push(0),
            }
        }

        out.extend_from_slice(&count(self.users.len())?.to_le_bytes());
        for u in self.users.values() {
            out.extend_from_slice(&u.id.to_le_bytes());
            out.push(u8::from(u.access_hash.is_some()));
            out.extend_from_slice(&u.access_hash.unwrap_or(0).to_le_bytes());
            out.push(opt_bool_to_u8(u.bot));
            out.push(opt_bool_to_u8(u.is_self));
        }

        let s = &self.updates;
        out.extend_from_slice(&s.pts.to_le_bytes());
        out.extend_from_slice(&s.qts.to_le_bytes());
        out.extend_from_slice(&s.date.to_le_bytes());
        out.extend_from_slice(&s.seq.to_le_bytes());
        let channels =
            i32::try_from(s.channels.len()).map_err(|_| SnapshotError::Invalid("channels"))?;
        out.extend_from_slice(&channels.to_le_bytes());
        for c in &s.channels {
            out.extend_from_slice(&c.id.to_le_bytes());
            out.extend_from_slice(&c.pts.to_le_bytes());
        }
        debug_assert_eq!(out.len(), len);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let mut r = Reader { buf: bytes };
        if r.take(4)? != SNAPSHOT_MAGIC.as_slice() || r.u8()? != SNAPSHOT_VERSION {
            return Err(SnapshotError::BadHeader);
        }
        let mut snap = SessionSnapshot {
            home_dc: r.i32()?,
            dc_options: BTreeMap::new(),
            users: BTreeMap::new(),
            updates: UpdatesState::default(),
        };

        for _ in 0..r.count(MAX_DC_OPTIONS, "dc option count")? {
            let id = r.i32()?;
            let v4 = r.array::<4>()?;
            let v4_port = r.u16()?;
            let v6 = r.array::<16>()?;
            let v6_port = r.u16()?;
            let auth_key = match r.u8()? {
                0 => None,
                1 => {
                    let mut key = [0u8; AUTH_KEY_LEN];
                    key.copy_from_slice(r.take(AUTH_KEY_LEN)?);
                    Some(key)
                }
                _ => return Err(SnapshotError::Invalid("auth key flag")),
            };
            let option = DcOption {
                id,
                ipv4: SocketAddrV4::new(Ipv4Addr::from(v4), v4_port),
                ipv6: SocketAddrV6::new(Ipv6Addr::from(v6), v6_port, 0, 0),
                auth_key,
            };
            if snap.dc_options.insert(id, option).is_some() {
                return Err(SnapshotError::Invalid("duplicate dc option"));
            }
        }
        if !snap.dc_options.contains_key(&snap.home_dc) {
            return Err(SnapshotError::Invalid("home dc has no option"));
        }

        for _ in 0..r.count(MAX_PEERS, "peer count")? {
            let id = r.i64()?;
            let has_hash = r.u8()?;
            let hash = r.i64()?;
            let user = StoredUser {
                id,
                access_hash: match has_hash {
                    0 => None,
                    1 => Some(hash),
                    _ => return Err(SnapshotError::Invalid("access hash flag")),
                },
                bot: u8_to_opt_bool(r.u8()?)?,
                is_self: u8_to_opt_bool(r.u8()?)?,
            };
            if PeerId::user(id).is_none() {
                return Err(SnapshotError::Invalid("user id out of range"));
            }
            if snap.users.insert(id, user).is_some() {
                return Err(SnapshotError::Invalid("duplicate peer"));
            }
        }

        snap.updates.pts = r.i32()?;
        snap.updates.qts = r.i32()?;
        snap.updates.date = r.i32()?;
        snap.updates.seq = r.i32()?;
        let channels = r.count32(MAX_CHANNELS, "channel count")?;
        snap.updates.channels.reserve(channels);
        for _ in 0..channels {
            snap.updates.channels.push(ChannelState {
                id: r.i64()?,
                pts: r.i32()?,
            });
        }
        if !r.buf.is_empty() {
            return Err(SnapshotError::TrailingBytes);
        }
        Ok(snap)
    }

    /// Свой user id, если сессия авторизована (после входа grammers кэширует себя с `is_self`).
    pub fn self_user(&self) -> Option<StoredUser> {
        self.users
            .values()
            .find(|u| u.is_self == Some(true))
            .copied()
    }

    /// Есть ли auth key для домашнего DC.
    pub fn has_home_auth_key(&self) -> bool {
        self.dc_options
            .get(&self.home_dc)
            .is_some_and(|o| o.auth_key.is_some())
    }
}

/// Ошибка хранилища сессии (БД). Текст не должен содержать секретов.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("session store: {0}")]
pub struct StoreError(pub String);

/// Где лежит зашифрованная сессия: в бою — `userbot_accounts` (реализация в `app`/`storage`),
/// в тестах — [`MemoryStore`]. Открытый текст сюда никогда не попадает.
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn load(&self) -> Result<Option<SealedSession>, StoreError>;
    async fn save(&self, sealed: &SealedSession) -> Result<(), StoreError>;
}

/// Хранилище в памяти: тесты и `exch login`, который сам решает, куда записать результат.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<Option<SealedSession>>,
    saves: AtomicU8,
}

impl MemoryStore {
    pub fn new(initial: Option<SealedSession>) -> Self {
        Self {
            inner: Mutex::new(initial),
            saves: AtomicU8::new(0),
        }
    }

    pub fn get(&self) -> Option<SealedSession> {
        self.inner.lock().ok().and_then(|g| g.clone())
    }

    /// Сколько раз вызывали `save` (насыщается на 255).
    pub fn save_count(&self) -> u8 {
        self.saves.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn load(&self) -> Result<Option<SealedSession>, StoreError> {
        Ok(self.get())
    }

    async fn save(&self, sealed: &SealedSession) -> Result<(), StoreError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| StoreError("memory store lock poisoned".into()))?;
        *guard = Some(sealed.clone());
        let _ = self
            .saves
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_add(1));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("session lock is poisoned")]
    Poisoned,
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Слить сведения о пользователе («или» по полям, новое значение важнее); `true` — изменилось.
fn merge_user(state: &mut SessionSnapshot, user: StoredUser) -> bool {
    match state.users.get_mut(&user.id) {
        Some(existing) => {
            let before = *existing;
            existing.access_hash = user.access_hash.or(existing.access_hash);
            existing.bot = user.bot.or(existing.bot);
            existing.is_self = user.is_self.or(existing.is_self);
            before != *existing
        }
        None => {
            state.users.insert(user.id, user);
            true
        }
    }
}

const CLEAN: u8 = 0;
/// Изменилось только состояние апдейтов: сохранить с задержкой.
const DIRTY_LAZY: u8 = 1;
/// Изменились DC, auth key или peer'ы: сохранить сразу.
const DIRTY_URGENT: u8 = 2;

/// Сессия для `SenderPool` и `Client` grammers. Хранит только нужных peer'ов: себя и
/// закреплённых ботов — остальные (каналы, группы, люди) не кэшируются (CLAUDE.md п. 10).
pub struct EncryptedSession {
    state: Mutex<SessionSnapshot>,
    allowed_users: Vec<i64>,
    dirty: AtomicU8,
    changed: Notify,
}

impl std::fmt::Debug for EncryptedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedSession")
            .field("allowed_users", &self.allowed_users)
            .finish_non_exhaustive()
    }
}

impl EncryptedSession {
    pub fn new(snapshot: SessionSnapshot, allowed_users: impl IntoIterator<Item = i64>) -> Self {
        Self {
            state: Mutex::new(snapshot),
            allowed_users: allowed_users.into_iter().collect(),
            dirty: AtomicU8::new(CLEAN),
            changed: Notify::new(),
        }
    }

    /// Расшифровать и разобрать сохранённую сессию.
    pub fn open(
        key: &SessionKey,
        account: &str,
        sealed: &SealedSession,
        allowed_users: impl IntoIterator<Item = i64>,
    ) -> Result<Self, SessionError> {
        let plain = crypto::open(key, account, sealed)?;
        let snapshot = SessionSnapshot::decode(&plain)?;
        Ok(Self::new(snapshot, allowed_users))
    }

    fn lock(&self) -> Result<MutexGuard<'_, SessionSnapshot>, SessionError> {
        self.state.lock().map_err(|_| SessionError::Poisoned)
    }

    pub fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        Ok(self.lock()?.clone())
    }

    /// Зашифровать текущее состояние.
    pub fn seal(&self, key: &SessionKey, account: &str) -> Result<SealedSession, SessionError> {
        let plain = self.lock()?.encode()?;
        Ok(crypto::seal(key, account, &plain)?)
    }

    pub fn self_user(&self) -> Option<StoredUser> {
        self.lock().ok().and_then(|s| s.self_user())
    }

    pub fn user(&self, id: i64) -> Option<StoredUser> {
        self.lock().ok().and_then(|s| s.users.get(&id).copied())
    }

    /// Забыть всех пользователей и состояние апдейтов (перед новым входом в недействительную
    /// сессию: иначе в ней останутся два «своих» аккаунта). Auth key и DC сохраняются.
    pub fn forget_users(&self) -> Result<(), SessionError> {
        {
            let mut state = self.lock()?;
            state.users.clear();
            state.updates = UpdatesState::default();
        }
        self.mark(DIRTY_URGENT);
        Ok(())
    }

    /// Записать пользователя напрямую (бот после сверки при старте, себя после `get_me`).
    pub fn remember_user(&self, user: StoredUser) -> Result<(), SessionError> {
        let changed = merge_user(&mut *self.lock()?, user);
        if changed {
            self.mark(DIRTY_URGENT);
        }
        Ok(())
    }

    fn is_allowed(&self, state: &SessionSnapshot, id: i64, is_self: Option<bool>) -> bool {
        is_self == Some(true)
            || self.allowed_users.contains(&id)
            || state.self_user().is_some_and(|me| me.id == id)
    }

    fn mark(&self, level: u8) {
        self.dirty.fetch_max(level, Ordering::SeqCst);
        self.changed.notify_one();
    }

    fn take_dirty(&self) -> u8 {
        self.dirty.swap(CLEAN, Ordering::SeqCst)
    }

    fn restore_dirty(&self, level: u8) {
        self.dirty.fetch_max(level, Ordering::SeqCst);
    }

    /// Есть ли несохранённые изменения.
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::SeqCst) != CLEAN
    }
}

impl Session for EncryptedSession {
    type Error = SessionError;

    fn home_dc_id(&self) -> Result<i32, SessionError> {
        Ok(self.lock()?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), SessionError>> {
        Box::pin(async move {
            let changed = {
                let mut state = self.lock()?;
                let changed = state.home_dc != dc_id;
                state.home_dc = dc_id;
                changed
            };
            if changed {
                self.mark(DIRTY_URGENT);
            }
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, SessionError> {
        Ok(self.lock()?.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), SessionError>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            let changed = {
                let mut state = self.lock()?;
                let changed = state.dc_options.get(&dc_option.id) != Some(&dc_option);
                if let Some(mut old) = state.dc_options.insert(dc_option.id, dc_option)
                    && let Some(key) = old.auth_key.as_mut()
                {
                    key.zeroize();
                }
                changed
            };
            if changed {
                self.mark(DIRTY_URGENT);
            }
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, SessionError>> {
        Box::pin(async move {
            let state = self.lock()?;
            if peer.kind() != PeerKind::User {
                return Ok(None);
            }
            let user = match peer.bare_id() {
                // PeerId::self_user(): свой аккаунт без известного id.
                None => state.self_user(),
                Some(id) => state.users.get(&id).copied(),
            };
            Ok(user.map(StoredUser::to_info))
        })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), SessionError>> {
        let peer = peer.clone();
        Box::pin(async move {
            let PeerInfo::User {
                id,
                auth,
                bot,
                is_self,
            } = peer
            else {
                return Ok(());
            };
            let changed = {
                let mut state = self.lock()?;
                if !self.is_allowed(&state, id, is_self) {
                    return Ok(());
                }
                merge_user(
                    &mut state,
                    StoredUser {
                        id,
                        access_hash: auth.map(|a| a.hash()),
                        bot,
                        is_self,
                    },
                )
            };
            if changed {
                self.mark(DIRTY_URGENT);
            }
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, SessionError>> {
        Box::pin(async move { Ok(self.lock()?.updates.clone()) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), SessionError>> {
        Box::pin(async move {
            {
                let mut state = self.lock()?;
                let s = &mut state.updates;
                match update {
                    UpdateState::All(all) => *s = all,
                    UpdateState::Primary { pts, date, seq } => {
                        s.pts = pts;
                        s.date = date;
                        s.seq = seq;
                    }
                    UpdateState::Secondary { qts } => s.qts = qts,
                    UpdateState::Channel { id, pts } => {
                        s.channels.retain(|c| c.id != id);
                        s.channels.push(ChannelState { id, pts });
                    }
                }
            }
            self.mark(DIRTY_LAZY);
            Ok(())
        })
    }
}

/// Сохраняет сессию в [`SessionStore`]: сразу после важных изменений и не чаще раза в
/// `lazy_every` для состояния апдейтов. Ошибка хранилища не роняет соединение: изменения
/// остаются «грязными» и сохраняются следующей попыткой, в лог — `error` (нужен человек).
pub struct SessionPersister {
    session: Arc<EncryptedSession>,
    store: Arc<dyn SessionStore>,
    key: Arc<SessionKey>,
    account: String,
    /// Сохранения по одному: иначе старый снимок мог бы записаться поверх нового.
    flushing: tokio::sync::Mutex<()>,
}

impl SessionPersister {
    pub fn new(
        session: Arc<EncryptedSession>,
        store: Arc<dyn SessionStore>,
        key: Arc<SessionKey>,
        account: impl Into<String>,
    ) -> Self {
        Self {
            session,
            store,
            key,
            account: account.into(),
            flushing: tokio::sync::Mutex::new(()),
        }
    }

    /// Сохранить текущее состояние, если есть изменения.
    pub async fn flush(&self) -> Result<(), SessionError> {
        let _one_at_a_time = self.flushing.lock().await;
        let level = self.session.take_dirty();
        if level == CLEAN {
            return Ok(());
        }
        let result = async {
            let sealed = self.session.seal(&self.key, &self.account)?;
            self.store.save(&sealed).await?;
            Ok::<(), SessionError>(())
        }
        .await;
        if result.is_err() {
            self.session.restore_dirty(level);
        }
        result
    }

    /// Сохранить безусловно (после входа).
    pub async fn save_now(&self) -> Result<(), SessionError> {
        self.session.mark(DIRTY_URGENT);
        self.flush().await
    }

    /// Фоновый цикл до отмены; при отмене — последнее сохранение.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken, lazy_every: Duration) {
        let mut last_save = tokio::time::Instant::now();
        let mut backoff = Duration::from_secs(1);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = self.session.changed.notified() => {}
                _ = tokio::time::sleep(lazy_every) => {}
            }
            let level = self.session.dirty.load(Ordering::SeqCst);
            let due =
                level == DIRTY_URGENT || (level == DIRTY_LAZY && last_save.elapsed() >= lazy_every);
            if !due {
                continue;
            }
            match self.flush().await {
                Ok(()) => {
                    last_save = tokio::time::Instant::now();
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    tracing::error!(account = %self.account, error = %e, "cannot save MTProto session");
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
        if let Err(e) = self.flush().await {
            tracing::error!(account = %self.account, error = %e, "cannot save MTProto session on shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammers_session::types::PeerAuth;

    fn sample() -> SessionSnapshot {
        let mut snap = SessionSnapshot::default();
        snap.home_dc = 2;
        if let Some(dc) = snap.dc_options.get_mut(&2) {
            dc.auth_key = Some([0x5A; AUTH_KEY_LEN]);
        }
        snap.users.insert(
            777,
            StoredUser {
                id: 777,
                access_hash: Some(-123_456_789),
                bot: Some(false),
                is_self: Some(true),
            },
        );
        snap.users.insert(
            1_559_501_630,
            StoredUser {
                id: 1_559_501_630,
                access_hash: Some(42),
                bot: Some(true),
                is_self: None,
            },
        );
        snap.updates = UpdatesState {
            pts: 100,
            qts: 7,
            date: 1_760_000_000,
            seq: 3,
            channels: vec![ChannelState { id: 99, pts: 5 }],
        };
        snap
    }

    #[test]
    fn snapshot_round_trip() {
        let snap = sample();
        let bytes = snap.encode().unwrap();
        assert_eq!(bytes.len(), bytes.capacity(), "exact capacity, no realloc");
        let back = SessionSnapshot::decode(&bytes).unwrap();
        assert_eq!(back, snap);
        assert_eq!(back.self_user().map(|u| u.id), Some(777));
        assert!(back.has_home_auth_key());
    }

    #[test]
    fn default_snapshot_has_static_dcs_and_no_login() {
        let snap = SessionSnapshot::default();
        assert_eq!(snap.dc_options.len(), 5);
        assert!(snap.self_user().is_none());
        assert!(!snap.has_home_auth_key());
        let back = SessionSnapshot::decode(&snap.encode().unwrap()).unwrap();
        assert_eq!(back, snap);
    }

    #[test]
    fn snapshot_rejects_garbage() {
        let bytes = sample().encode().unwrap();
        for cut in [0, 3, 5, 20, bytes.len() - 1] {
            assert!(
                SessionSnapshot::decode(&bytes[..cut]).is_err(),
                "cut at {cut}"
            );
        }
        let mut extra = bytes.to_vec();
        extra.push(0);
        assert_eq!(
            SessionSnapshot::decode(&extra),
            Err(SnapshotError::TrailingBytes)
        );
        let mut bad = bytes.to_vec();
        bad[4] = 99;
        assert_eq!(SessionSnapshot::decode(&bad), Err(SnapshotError::BadHeader));
    }

    #[test]
    fn debug_does_not_print_auth_key() {
        let dbg = format!("{:?}", sample());
        assert!(dbg.contains("dcs_with_auth_key: [2]"), "{dbg}");
        assert!(!dbg.contains("90"), "{dbg}"); // 0x5A
    }

    #[tokio::test]
    async fn session_caches_only_self_and_pinned_bots() {
        let session = EncryptedSession::new(SessionSnapshot::default(), [1_559_501_630]);
        let user = |id, is_self| PeerInfo::User {
            id,
            auth: Some(PeerAuth::from_hash(id * 10)),
            bot: Some(false),
            is_self,
        };
        session.cache_peer(&user(31337, None)).await.unwrap();
        session
            .cache_peer(&PeerInfo::Channel {
                id: 5,
                auth: Some(PeerAuth::from_hash(1)),
                kind: None,
            })
            .await
            .unwrap();
        assert!(!session.is_dirty(), "foreign peers are not cached");
        assert_eq!(
            session.peer(PeerId::user(31337).unwrap()).await.unwrap(),
            None
        );

        session
            .cache_peer(&user(1_559_501_630, None))
            .await
            .unwrap();
        session.cache_peer(&user(777, Some(true))).await.unwrap();
        assert!(session.is_dirty());
        let me = session.peer(PeerId::self_user()).await.unwrap();
        assert!(matches!(me, Some(PeerInfo::User { id: 777, .. })));
        let bot = session
            .peer_ref(PeerId::user(1_559_501_630).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bot.auth.hash(), 15_595_016_300);
        // Свой аккаунт без флага is_self (из апдейтов) тоже допускается после входа.
        session.cache_peer(&user(777, None)).await.unwrap();
        assert_eq!(session.user(777).and_then(|u| u.is_self), Some(true));
    }

    #[tokio::test]
    async fn persister_saves_sealed_state_and_reopens() {
        let key = Arc::new(SessionKey::from_bytes(1, &[3; 32]));
        let store = Arc::new(MemoryStore::default());
        let session = Arc::new(EncryptedSession::new(SessionSnapshot::default(), []));
        let persister = SessionPersister::new(
            Arc::clone(&session),
            Arc::clone(&store) as Arc<dyn SessionStore>,
            Arc::clone(&key),
            "ub-1",
        );

        persister.flush().await.unwrap();
        assert_eq!(store.save_count(), 0, "nothing to save");

        session.set_home_dc_id(4).await.unwrap();
        let mut dc = session.dc_option(4).unwrap().unwrap();
        dc.auth_key = Some([7; AUTH_KEY_LEN]);
        session.set_dc_option(&dc).await.unwrap();
        persister.flush().await.unwrap();
        assert_eq!(store.save_count(), 1);
        assert!(!session.is_dirty());

        let sealed = store.get().unwrap();
        assert!(
            !sealed.ciphertext.windows(16).any(|w| w == [7u8; 16]),
            "auth key must not be stored in clear"
        );
        let reopened = EncryptedSession::open(&key, "ub-1", &sealed, []).unwrap();
        assert_eq!(reopened.home_dc_id().unwrap(), 4);
        assert_eq!(
            reopened.dc_option(4).unwrap().unwrap().auth_key,
            Some([7; AUTH_KEY_LEN])
        );
        assert!(EncryptedSession::open(&key, "ub-2", &sealed, []).is_err());
    }

    struct FailingStore;

    #[async_trait]
    impl SessionStore for FailingStore {
        async fn load(&self) -> Result<Option<SealedSession>, StoreError> {
            Ok(None)
        }
        async fn save(&self, _: &SealedSession) -> Result<(), StoreError> {
            Err(StoreError("db down".into()))
        }
    }

    #[tokio::test]
    async fn failed_save_keeps_changes_dirty() {
        let session = Arc::new(EncryptedSession::new(SessionSnapshot::default(), []));
        let persister = SessionPersister::new(
            Arc::clone(&session),
            Arc::new(FailingStore),
            Arc::new(SessionKey::from_bytes(1, &[1; 32])),
            "ub-1",
        );
        session.set_home_dc_id(1).await.unwrap();
        assert!(persister.flush().await.is_err());
        assert!(session.is_dirty(), "will be retried");
    }

    #[tokio::test]
    async fn forget_users_keeps_auth_key() {
        let session = EncryptedSession::open(
            &SessionKey::from_bytes(1, &[2; 32]),
            "ub-1",
            &crypto::seal(
                &SessionKey::from_bytes(1, &[2; 32]),
                "ub-1",
                &sample().encode().unwrap(),
            )
            .unwrap(),
            [],
        )
        .unwrap();
        assert!(session.self_user().is_some());
        session.forget_users().unwrap();
        assert!(session.self_user().is_none());
        assert!(session.user(1_559_501_630).is_none());
        assert!(session.snapshot().unwrap().has_home_auth_key());
        assert_eq!(
            session.updates_state().await.unwrap(),
            UpdatesState::default()
        );
        assert!(session.is_dirty());
    }

    #[tokio::test]
    async fn update_state_is_lazy() {
        let session = EncryptedSession::new(SessionSnapshot::default(), []);
        session
            .set_update_state(UpdateState::Primary {
                pts: 5,
                date: 6,
                seq: 7,
            })
            .await
            .unwrap();
        assert_eq!(session.dirty.load(Ordering::SeqCst), DIRTY_LAZY);
        session
            .set_update_state(UpdateState::Channel { id: 1, pts: 2 })
            .await
            .unwrap();
        session
            .set_update_state(UpdateState::Channel { id: 1, pts: 3 })
            .await
            .unwrap();
        let state = session.updates_state().await.unwrap();
        assert_eq!((state.pts, state.date, state.seq), (5, 6, 7));
        assert_eq!(state.channels, vec![ChannelState { id: 1, pts: 3 }]);
    }
}
