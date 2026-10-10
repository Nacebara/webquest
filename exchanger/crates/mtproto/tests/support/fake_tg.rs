//! Поддельный сервер Telegram для тестов `GrammersTransport` без сети (LOVEC-PORTING §7).
//!
//! Порт идеи lovec `benches/support/fake_tg.rs` под grammers 0.10 (слой 227) и tokio:
//! MTProto 2.0 со стороны сервера поверх транспорта Full и заранее известный auth key (обмена
//! ключами нет — сессия клиента засевается этим ключом). Умеет ровно то, что нужно тестам:
//! ответить на `initConnection` и `ping`, передать остальные запросы обработчику теста
//! (результат, RPC-ошибка, молчание или обрыв), отправить клиенту `Updates`, разорвать соединения.
//!
//! Логику ботов кошельков не знает — это задача симулятора `userbot::sim`.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use grammers_crypto::DequeBuffer;
use grammers_crypto::aes::{ige_decrypt, ige_encrypt};
use grammers_mtproto::transport::{Error as TransportError, Full, Transport};
use grammers_tl_types::{self as tl, Deserializable, Identifiable, Serializable};
use sha1::Sha1;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const MSG_CONTAINER: u32 = 0x73f1_f8dc;
const RPC_RESULT: u32 = 0xf35c_6d01;
const MSGS_ACK: u32 = 0x62d6_b459;

/// Запрос клиента после расшифровки и разбора контейнера.
#[derive(Debug, Clone)]
pub struct Request {
    pub msg_id: i64,
    /// Тело с id конструктора.
    pub body: Vec<u8>,
}

impl Request {
    pub fn constructor(&self) -> u32 {
        u32::from_le_bytes(self.body[..4].try_into().unwrap())
    }

    pub fn is<F: Identifiable>(&self) -> bool {
        self.constructor() == F::CONSTRUCTOR_ID
    }

    /// Разобрать как функцию TL `F`, если это она.
    pub fn parse<F: Identifiable + Deserializable>(&self) -> Option<F> {
        if !self.is::<F>() {
            return None;
        }
        Some(F::from_bytes(&self.body[4..]).expect("malformed request body"))
    }
}

/// Что сервер делает с запросом.
pub enum Reply {
    /// `rpc_result` с этим объектом TL.
    Result(Vec<u8>),
    /// `rpc_result` с `rpc_error`.
    Error(i32, String),
    /// Ничего не отвечать (запрос выполнен, ответ потерян, или сервер завис).
    Silence,
    /// Разорвать соединение, не отвечая.
    Close,
}

impl Reply {
    pub fn ok<T: Serializable>(value: &T) -> Self {
        Reply::Result(value.to_bytes())
    }

    pub fn error(code: i32, name: &str) -> Self {
        Reply::Error(code, name.to_owned())
    }
}

type Handler = Box<dyn FnMut(&Request) -> Reply + Send>;

struct Shared {
    auth_key: [u8; 256],
    handler: Mutex<Handler>,
    log: Mutex<Vec<Request>>,
    push: broadcast::Sender<Vec<u8>>,
    kick: broadcast::Sender<()>,
    connections: AtomicUsize,
}

pub struct FakeTelegram {
    pub addr: SocketAddrV4,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl Drop for FakeTelegram {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.shared.kick.send(());
    }
}

fn now_secs() -> i32 {
    i32::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

impl FakeTelegram {
    pub async fn start(handler: impl FnMut(&Request) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let SocketAddr::V4(addr) = listener.local_addr().unwrap() else {
            unreachable!("bound to IPv4")
        };
        let mut auth_key = [0u8; 256];
        let mut x = 0x9e37_79b9_7f4a_7c15_u64;
        for b in &mut auth_key {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x.to_le_bytes()[0];
        }
        let shared = Arc::new(Shared {
            auth_key,
            handler: Mutex::new(Box::new(handler)),
            log: Mutex::new(Vec::new()),
            push: broadcast::channel(64).0,
            kick: broadcast::channel(4).0,
            connections: AtomicUsize::new(0),
        });
        let s = Arc::clone(&shared);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                s.connections.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(serve(stream, Arc::clone(&s)));
            }
        });
        Self { addr, shared, task }
    }

    pub fn auth_key(&self) -> [u8; 256] {
        self.shared.auth_key
    }

    /// Все запросы клиента, кроме служебных (`initConnection`, `ping`, `msgs_ack`).
    pub fn requests(&self) -> Vec<Request> {
        self.shared.log.lock().unwrap().clone()
    }

    pub fn count<F: Identifiable>(&self) -> usize {
        self.requests().iter().filter(|r| r.is::<F>()).count()
    }

    pub fn parsed<F: Identifiable + Deserializable>(&self) -> Vec<F> {
        self.requests()
            .iter()
            .filter_map(Request::parse::<F>)
            .collect()
    }

    /// Сколько TCP-соединений принял сервер.
    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// Отправить `Updates` во все соединения (как пуш сервера).
    pub fn push(&self, updates: &tl::enums::Updates) {
        let _ = self.shared.push.send(updates.to_bytes());
    }

    /// Разорвать все соединения.
    pub fn kick(&self) {
        let _ = self.shared.kick.send(());
    }

    /// Дождаться условия по журналу запросов.
    pub async fn wait_for(&self, what: &str, mut cond: impl FnMut(&[Request]) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !cond(&self.requests()) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "fake telegram: timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Серверная половина MTProto 2.0: расшифровывает с x = 0, шифрует с x = 8.
struct ServerCrypto {
    key: [u8; 256],
    key_id: [u8; 8],
    session_id: Option<i64>,
    counter: i64,
    seq: i32,
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn calc_key(key: &[u8; 256], msg_key: &[u8; 16], x: usize) -> ([u8; 32], [u8; 32]) {
    let a = sha256(&[msg_key, &key[x..x + 36]]);
    let b = sha256(&[&key[40 + x..40 + x + 36], msg_key]);
    let mut aes_key = [0u8; 32];
    aes_key[..8].copy_from_slice(&a[..8]);
    aes_key[8..24].copy_from_slice(&b[8..24]);
    aes_key[24..].copy_from_slice(&a[24..]);
    let mut aes_iv = [0u8; 32];
    aes_iv[..8].copy_from_slice(&b[..8]);
    aes_iv[8..24].copy_from_slice(&a[8..24]);
    aes_iv[24..].copy_from_slice(&b[24..]);
    (aes_key, aes_iv)
}

impl ServerCrypto {
    fn new(key: [u8; 256]) -> Self {
        let sha = Sha1::digest(key);
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&sha[12..20]);
        Self {
            key,
            key_id,
            session_id: None,
            counter: 0,
            seq: 0,
        }
    }

    fn next_msg_id(&mut self, response: bool) -> i64 {
        self.counter += 1;
        (i64::from(now_secs()) << 32) | (self.counter << 2) | if response { 1 } else { 3 }
    }

    fn seal(&mut self, body: &[u8]) -> Vec<u8> {
        let mut plain = Vec::with_capacity(64 + body.len());
        0i64.serialize(&mut plain); // salt
        self.session_id.unwrap_or_default().serialize(&mut plain);
        let response = u32::from_le_bytes(body[..4].try_into().unwrap()) == RPC_RESULT;
        self.next_msg_id(response).serialize(&mut plain);
        self.seq += 1;
        (self.seq * 2 - 1).serialize(&mut plain);
        i32::try_from(body.len()).unwrap().serialize(&mut plain);
        plain.extend_from_slice(body);
        let pad = 16 + (16 - plain.len() % 16);
        plain.extend(std::iter::repeat_n(0x5a_u8, pad));
        let large = sha256(&[&self.key[88 + 8..88 + 8 + 32], &plain]);
        let mut msg_key = [0u8; 16];
        msg_key.copy_from_slice(&large[8..24]);
        let (k, iv) = calc_key(&self.key, &msg_key, 8);
        ige_encrypt(&mut plain, &k, &iv);
        let mut out = Vec::with_capacity(24 + plain.len());
        out.extend_from_slice(&self.key_id);
        out.extend_from_slice(&msg_key);
        out.extend_from_slice(&plain);
        out
    }

    fn open(&mut self, mut payload: Vec<u8>) -> Vec<Request> {
        assert_eq!(&payload[..8], &self.key_id, "client used another auth key");
        let mut msg_key = [0u8; 16];
        msg_key.copy_from_slice(&payload[8..24]);
        let (k, iv) = calc_key(&self.key, &msg_key, 0);
        let plain = &mut payload[24..];
        ige_decrypt(plain, &k, &iv);
        self.session_id = Some(i64::from_le_bytes(plain[8..16].try_into().unwrap()));
        let mut cur = &plain[16..];
        let (msg_id, body) = take_msg(&mut cur);
        let mut out = Vec::new();
        push_msg(&mut out, msg_id, body);
        out
    }
}

fn take_msg<'a>(cur: &mut &'a [u8]) -> (i64, &'a [u8]) {
    let msg_id = i64::from_le_bytes(cur[..8].try_into().unwrap());
    let len = usize::try_from(i32::from_le_bytes(cur[12..16].try_into().unwrap())).unwrap();
    let body = &cur[16..16 + len];
    *cur = &cur[16 + len..];
    (msg_id, body)
}

fn push_msg(out: &mut Vec<Request>, msg_id: i64, body: &[u8]) {
    if u32::from_le_bytes(body[..4].try_into().unwrap()) == MSG_CONTAINER {
        let count = i32::from_le_bytes(body[4..8].try_into().unwrap());
        let mut cur = &body[8..];
        for _ in 0..count {
            let (m, b) = take_msg(&mut cur);
            push_msg(out, m, b);
        }
    } else {
        out.push(Request {
            msg_id,
            body: body.to_vec(),
        });
    }
}

struct Conn {
    crypto: ServerCrypto,
    tout: Full,
}

impl Conn {
    fn frame(&mut self, body: &[u8]) -> Vec<u8> {
        let sealed = self.crypto.seal(body);
        let mut buf = DequeBuffer::with_capacity(sealed.len() + 64, 64);
        buf.extend(sealed);
        self.tout.pack(&mut buf);
        buf.as_ref().to_vec()
    }
}

fn rpc_result(req_msg_id: i64, result: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(12 + result.len());
    RPC_RESULT.serialize(&mut body);
    req_msg_id.serialize(&mut body);
    body.extend_from_slice(result);
    body
}

fn rpc_error(code: i32, message: &str) -> Vec<u8> {
    tl::enums::RpcError::Error(tl::types::RpcError {
        error_code: code,
        error_message: message.to_owned(),
    })
    .to_bytes()
}

/// Минимальный `Config` слоя 227 — его ждёт `SenderPoolRunner::connect_sender`.
fn config_bytes() -> Vec<u8> {
    let now = now_secs();
    tl::enums::Config::Config(tl::types::Config {
        default_p2p_contacts: false,
        preload_featured_stickers: false,
        revoke_pm_inbox: false,
        blocked_mode: false,
        force_try_ipv6: false,
        date: now,
        expires: now + 3600,
        test_mode: false,
        this_dc: 2,
        dc_options: Vec::new(),
        dc_txt_domain_name: String::new(),
        chat_size_max: 200,
        megagroup_size_max: 200_000,
        forwarded_count_max: 100,
        online_update_period_ms: 210_000,
        offline_blur_timeout_ms: 5_000,
        offline_idle_timeout_ms: 30_000,
        online_cloud_timeout_ms: 300_000,
        notify_cloud_delay_ms: 30_000,
        notify_default_delay_ms: 1_500,
        push_chat_period_ms: 60_000,
        push_chat_limit: 2,
        edit_time_limit: 172_800,
        revoke_time_limit: i32::MAX,
        revoke_pm_time_limit: i32::MAX,
        rating_e_decay: 2_419_200,
        stickers_recent_limit: 200,
        channels_read_media_period: 604_800,
        tmp_sessions: None,
        call_receive_timeout_ms: 20_000,
        call_ring_timeout_ms: 90_000,
        call_connect_timeout_ms: 30_000,
        call_packet_timeout_ms: 10_000,
        me_url_prefix: "https://t.me/".into(),
        autoupdate_url_prefix: None,
        gif_search_username: None,
        venue_search_username: None,
        img_search_username: None,
        static_maps_provider: None,
        caption_length_max: 1024,
        message_length_max: 4096,
        webfile_dc_id: 4,
        suggested_lang_code: None,
        lang_pack_version: None,
        base_lang_pack_version: None,
        reactions_default: None,
        autologin_token: None,
    })
    .to_bytes()
}

enum Action {
    Send(Vec<u8>),
    Nothing,
    Close,
}

fn respond(shared: &Shared, conn: &mut Conn, msg: &Request) -> Action {
    let c = msg.constructor();
    if c == MSGS_ACK {
        return Action::Nothing;
    }
    if c == tl::functions::Ping::CONSTRUCTOR_ID
        || c == tl::functions::PingDelayDisconnect::CONSTRUCTOR_ID
    {
        let ping_id = i64::from_le_bytes(msg.body[4..12].try_into().unwrap());
        let pong = tl::enums::Pong::Pong(tl::types::Pong {
            msg_id: msg.msg_id,
            ping_id,
        });
        return Action::Send(conn.frame(&pong.to_bytes()));
    }
    if c == <tl::functions::InvokeWithLayer<tl::functions::help::GetConfig> as Identifiable>::CONSTRUCTOR_ID
    {
        return Action::Send(conn.frame(&rpc_result(msg.msg_id, &config_bytes())));
    }
    shared.log.lock().unwrap().push(msg.clone());
    let reply = (shared.handler.lock().unwrap())(msg);
    match reply {
        Reply::Result(bytes) => Action::Send(conn.frame(&rpc_result(msg.msg_id, &bytes))),
        Reply::Error(code, name) => {
            Action::Send(conn.frame(&rpc_result(msg.msg_id, &rpc_error(code, &name))))
        }
        Reply::Silence => Action::Nothing,
        Reply::Close => Action::Close,
    }
}

async fn serve(stream: TcpStream, shared: Arc<Shared>) {
    let _ = stream.set_nodelay(true);
    let (mut rd, mut wr) = stream.into_split();
    let mut conn = Conn {
        crypto: ServerCrypto::new(shared.auth_key),
        tout: Full::new(),
    };
    let mut tin = Full::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 1 << 16];
    let mut push_rx = shared.push.subscribe();
    let mut kick_rx = shared.kick.subscribe();
    // Пуши до первого сообщения клиента ждут: без его session_id клиент их отвергнет.
    let mut pending: Vec<Vec<u8>> = Vec::new();
    loop {
        tokio::select! {
            read = rd.read(&mut chunk) => {
                let n = match read {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                loop {
                    let off = match tin.unpack(&mut buf) {
                        Ok(off) => off,
                        Err(TransportError::MissingBytes) => break,
                        Err(_) => return,
                    };
                    let payload = buf[off.data_range.clone()].to_vec();
                    buf.drain(..off.next_offset);
                    for msg in conn.crypto.open(payload) {
                        match respond(&shared, &mut conn, &msg) {
                            Action::Send(bytes) => {
                                if wr.write_all(&bytes).await.is_err() {
                                    return;
                                }
                            }
                            Action::Nothing => {}
                            Action::Close => return,
                        }
                    }
                    for body in pending.drain(..) {
                        let bytes = conn.frame(&body);
                        if wr.write_all(&bytes).await.is_err() {
                            return;
                        }
                    }
                }
            }
            pushed = push_rx.recv() => match pushed {
                Ok(body) if conn.crypto.session_id.is_some() => {
                    let bytes = conn.frame(&body);
                    if wr.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                Ok(body) => pending.push(body),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = kick_rx.recv() => return,
        }
    }
}
