//! Пульт exch — ops-бот (SPEC §7–§8, DESIGN-v0.2 Р15–Р16).
//!
//! Живёт в том же боте, что и клиенты (решение владельца 7 ноября 2026: один бот, как гарант):
//! клиентский бот отдаёт пульту каждое обновление первым ([`tgbot::UpdateHook`]). Персоналу в
//! главном меню — кнопка «🛠 Админ-панель» и команда `/admin`; кнопки пульта — с префиксом `a:`;
//! остальное (ссылки на чеки, `/start`) идёт обычным клиентским путём. Посторонним пульт не
//! виден и не отвечает. Разделы: сводка, направления (комиссия, ступени,
//! фикс, рычаг, лимиты, пауза), встречный поток (режим, курс, пороги, цели резерва, включение
//! по направлению), ручной разбор (решения по заявке через `engine::operator`), клиенты
//! (личные условия, суточный лимит, бан, заметка), настройки (лимит новичка, обслуживание,
//! стоп-кран, премиум-эмодзи), юзерботы, персонал, журнал; события (`alerts`) — в личку
//! персоналу.
//!
//! Правила:
//! - кнопка не меняет ничего сама: она открывает ввод или предпросмотр «было → станет»;
//!   изменение — только кнопкой «Подтвердить» с одноразовым nonce (живёт `confirm_ttl`, привязан
//!   к сотруднику);
//! - права проверяются и при показе кнопки, и при подтверждении (роль могли сменить);
//! - каждое изменение — запись в `admin_audit` в той же транзакции; решения по заявке пишет
//!   движок;
//! - деньги заявки двигает только движок (`Engine::operator_decide`), пульт — нет.

mod actions;
mod alerts;
pub use alerts::mismatch_screen;
mod cb;
pub mod chart;
mod dash;
mod inbox;
mod invest;
mod look;
pub mod preview;
mod reserves;
pub mod stats;
mod texts;
mod views;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use botapi::{
    AnswerCallbackQuery, BotApi, EditMessageText, InlineKeyboardButton, InlineKeyboardMarkup,
    Message, ReplyMarkup, SendMessage, Update,
};
use engine::Engine;
use sqlx::PgPool;
use storage::staff::{Role, StaffMember};
use tokio_util::sync::CancellationToken;

pub use cb::Cb;
pub use dash::{DashData, sample_dash};
pub use look::Look;
pub use reserves::{ReservesData, sample_reserves};
pub use texts::Texts;
pub use views::{MenuCounts, menu_screen};

use crate::actions::{Action, Input};

/// Как часто главный экран правится сам (макет: «раз в минуту и сразу при событии»).
const DASH_EVERY: Duration = Duration::from_secs(60);

/// Настройки пульта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsConfig {
    /// Как часто проверять и рассылать события.
    pub alert_every: Duration,
    /// Сколько живёт кнопка «Подтвердить» (SPEC §7.4: 60 с).
    pub confirm_ttl: Duration,
    /// Сколько ждать ответа на вопрос пульта.
    pub input_ttl: Duration,
    /// Владелец из окружения (`OWNER_TG_ID`): создаётся при старте, если владельца ещё нет.
    pub owner_id: Option<i64>,
    /// Часовой пояс владельца (IANA, `TZ_OWNER`): дни и часы «Статистики».
    pub tz: String,
}

impl Default for OpsConfig {
    fn default() -> Self {
        Self {
            alert_every: Duration::from_secs(5),
            confirm_ttl: Duration::from_secs(60),
            input_ttl: Duration::from_secs(600),
            owner_id: None,
            tz: "Europe/Moscow".to_owned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error(transparent)]
    Api(#[from] botapi::Error),
    #[error(transparent)]
    Storage(#[from] storage::StorageError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Engine(#[from] engine::EngineError),
    #[error("ops texts: {0}")]
    Texts(String),
}

/// Экран пульта: HTML и инлайн-клавиатура.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Screen {
    /// Обычный HTML; у карточки — запасной вид.
    pub text: String,
    /// Карточка (rich-HTML, `sendRichMessage`); `None` — экран обычным HTML.
    pub rich: Option<String>,
    pub keyboard: Vec<Vec<InlineKeyboardButton>>,
}

impl Screen {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            rich: None,
            keyboard: Vec::new(),
        }
    }

    /// Карточка: rich-HTML и запасной обычный HTML (CLAUDE.md п. 18: все экраны пульта —
    /// карточки).
    pub fn card(rich: impl Into<String>, plain: impl Into<String>) -> Self {
        Self {
            text: plain.into(),
            rich: Some(rich.into()),
            keyboard: Vec::new(),
        }
    }

    pub fn row(mut self, row: Vec<InlineKeyboardButton>) -> Self {
        if !row.is_empty() {
            self.keyboard.push(row);
        }
        self
    }

    /// Строка-уведомление над экраном («Сохранено», итог решения) — и в карточке, и в
    /// запасном тексте.
    pub fn notice(mut self, html: &str) -> Self {
        self.text = format!("{html}\n\n{}", self.text);
        if let Some(rich) = &mut self.rich {
            *rich = format!("<p>{html}</p>{rich}");
        }
        self
    }

    fn markup(&self) -> Option<InlineKeyboardMarkup> {
        (!self.keyboard.is_empty()).then(|| InlineKeyboardMarkup::new(self.keyboard.clone()))
    }
}

pub(crate) fn btn(text: impl Into<String>, cb: &Cb) -> InlineKeyboardButton {
    InlineKeyboardButton::callback(text, cb.encode())
}

/// Ждём ответа сотрудника.
#[derive(Debug, Clone)]
pub(crate) struct Awaiting {
    pub input: Input,
    pub until: Instant,
}

/// Предпросмотр, ждущий подтверждения.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    pub staff: i64,
    pub until: Instant,
    pub action: Action,
}

#[derive(Default)]
pub(crate) struct State {
    awaiting: HashMap<i64, Awaiting>,
    pending: HashMap<String, Pending>,
    /// Сообщения событий у персонала: id события → (чат, сообщение).
    delivered: HashMap<i64, Vec<(i64, i32)>>,
}

/// Пульт. Дешёвый `Clone`.
#[derive(Clone)]
pub struct OpsBot {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for OpsBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpsBot")
            .field("cfg", &self.inner.cfg)
            .finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    api: BotApi,
    engine: Arc<Engine>,
    pool: PgPool,
    texts: Texts,
    cfg: OpsConfig,
    state: Mutex<State>,
    /// Вид (эмодзи) и отпечаток настроек, по которому он собран.
    look: std::sync::RwLock<(Arc<Look>, String)>,
    /// Когда пульт запущен («Система»: «работает 3 дн»).
    started: Instant,
}

impl OpsBot {
    pub fn new(
        api: BotApi,
        engine: Arc<Engine>,
        pool: PgPool,
        cfg: OpsConfig,
    ) -> Result<Self, OpsError> {
        let texts = Texts::builtin().map_err(|e| OpsError::Texts(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                api,
                engine,
                pool,
                texts,
                cfg,
                state: Mutex::new(State::default()),
                look: std::sync::RwLock::new((Arc::new(Look::plain()), String::new())),
                started: Instant::now(),
            }),
        })
    }

    /// Работать до отмены: владелец из окружения и рассылка событий персоналу. Обновления
    /// пульт получает через клиентский бот ([`tgbot::UpdateHook`]): бот один.
    pub async fn run(&self, cancel: CancellationToken) -> Result<(), OpsError> {
        let inner = self.inner.clone();
        inner.startup().await?;
        inner.refresh_look().await;
        inner.alert_loop(cancel).await;
        Ok(())
    }

    /// Обработать обновление, если оно для пульта. `false` — не для пульта: пусть его
    /// обработает клиентский бот.
    pub async fn handle_update(&self, update: &Update) -> Result<bool, OpsError> {
        self.inner.handle(update).await
    }

    /// Один проход рассылки событий (тесты и ручной вызов).
    pub async fn deliver_alerts(&self) -> Result<(), OpsError> {
        self.inner.alerts_once().await
    }
}

impl Inner {
    fn state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let mut guard = match self.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        f(&mut guard)
    }

    /// Действующий вид пульта.
    pub(crate) fn look(&self) -> Arc<Look> {
        match self.look.read() {
            Ok(g) => g.0.clone(),
            Err(p) => p.into_inner().0.clone(),
        }
    }

    /// Перечитать эмодзи из базы (те же настройки, что у клиентского бота), если изменились.
    async fn refresh_look(&self) {
        let loaded = match self.pool.acquire().await {
            Ok(mut conn) => tgbot::emoji::load(&mut conn).await.ok(),
            Err(_) => None,
        };
        let Some((settings, fingerprint)) = loaded else {
            return;
        };
        let same = match self.look.read() {
            Ok(g) => g.1 == fingerprint,
            Err(p) => p.into_inner().1 == fingerprint,
        };
        if same {
            return;
        }
        match tgbot::emoji::set_from(&settings) {
            Ok(set) => {
                let mut g = match self.look.write() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                *g = (Arc::new(Look::new(set)), fingerprint);
            }
            Err(e) => tracing::warn!(error = %e, "ops: emoji settings are invalid"),
        }
    }

    async fn startup(&self) -> Result<(), OpsError> {
        if let Some(owner) = self.cfg.owner_id {
            let mut conn = self.pool.acquire().await?;
            let o = storage::staff::ensure_owner(&mut conn, owner, "Владелец").await?;
            if o.tg_user_id != owner {
                tracing::warn!(
                    "OWNER_TG_ID differs from the owner in the database; the database wins"
                );
            }
        }
        Ok(())
    }

    async fn alert_loop(&self, cancel: CancellationToken) {
        let mut tick = tokio::time::interval(self.cfg.alert_every);
        let mut last_minute = Instant::now();
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = tick.tick() => {}
            }
            if let Err(e) = self.alerts_once().await {
                tracing::warn!(error = %e, "ops alerts failed");
            }
            if last_minute.elapsed() >= DASH_EVERY {
                last_minute = Instant::now();
                self.refresh_look().await;
                if let Err(e) = self.refresh_dashes().await {
                    tracing::warn!(error = %e, "ops dash refresh failed");
                }
            }
        }
    }

    /// Действующий сотрудник или `None` (посторонний — молча).
    async fn staff(&self, user: i64) -> Result<Option<StaffMember>, OpsError> {
        let mut conn = self.pool.acquire().await?;
        Ok(storage::staff::active(&mut conn, user).await?)
    }

    /// Команды пульта в общем боте. Остальные команды (`/start`, `/help` …) — клиентские.
    const COMMANDS: [&'static str; 15] = [
        "admin", "panel", "menu", "dash", "queue", "order", "find", "user", "dir", "promo",
        "settings", "bots", "staff", "audit", "system",
    ];

    fn has_awaiting(&self, staff: i64) -> bool {
        self.state(|s| {
            s.awaiting
                .get(&staff)
                .is_some_and(|a| a.until > Instant::now())
        })
    }

    /// Обновление для пульта? Да — обработать и вернуть `true`.
    async fn handle(self: &Arc<Self>, update: &Update) -> Result<bool, OpsError> {
        if let Some(query) = &update.callback_query {
            // Кнопки пульта — только с префиксом `a:`; клиентские не трогаем.
            let Some(cb) = query.data.as_deref().and_then(Cb::parse) else {
                return Ok(false);
            };
            let _ = self
                .api
                .answer_callback_query(&AnswerCallbackQuery::ack(query.id.clone()))
                .await;
            let Some(staff) = self.staff(query.from.id).await? else {
                tracing::info!(
                    user = query.from.id,
                    "ops button from a non-staff user, ignored"
                );
                return Ok(true);
            };
            let Some(message) = &query.message else {
                return Ok(true);
            };
            let chat = message.chat.id;
            let dash = cb == Cb::Dash;
            if let Some(screen) = self.on_callback(&staff, chat, message, cb).await? {
                self.edit(chat, message.message_id, &screen).await?;
                if dash {
                    self.remember_dash(chat, message.message_id).await?;
                }
            }
            return Ok(true);
        }
        let Some(message) = &update.message else {
            return Ok(false);
        };
        if message.chat.kind != botapi::ChatType::Private {
            return Ok(false);
        }
        let Some(from) = message.from.as_ref() else {
            return Ok(false);
        };
        let Some(staff) = self.staff(from.id).await? else {
            return Ok(false);
        };
        let text = message.text.as_deref().unwrap_or("").trim();
        let command = text
            .strip_prefix('/')
            .map(|c| c.split([' ', '@']).next().unwrap_or(c));
        let ours = match command {
            Some(name) => Self::COMMANDS.contains(&name),
            None => text == self.texts.t("menu.open") || self.has_awaiting(staff.tg_user_id),
        };
        if !ours {
            return Ok(false);
        }
        let screen = self.on_message(&staff, message).await?;
        let sent = self.send(message.chat.id, &screen).await?;
        let opened_dash = matches!(command, Some("dash" | "admin" | "panel"))
            || (command.is_none() && text == self.texts.t("menu.open"));
        if opened_dash {
            self.remember_dash(sent.chat.id, sent.message_id).await?;
        }
        Ok(true)
    }

    /// Отправить экран: карточку — `sendRichMessage` (Telegram отказал — обычным HTML).
    pub(crate) async fn send(&self, chat: i64, screen: &Screen) -> Result<Message, OpsError> {
        let screen = self.look().screen(screen);
        if let Some(rich) = &screen.rich {
            let mut request = botapi::SendRichMessage::new(chat, rich.clone());
            request.reply_markup = screen.markup().map(ReplyMarkup::Inline);
            match self.api.send_rich_message(&request).await {
                Ok(m) => return Ok(m),
                Err(e) if e.is_rejection() => {
                    tracing::info!(error = %e, "ops card rejected, sending plain HTML");
                }
                Err(e) => return Err(e.into()),
            }
        }
        let mut request = SendMessage::html(chat, screen.text.clone());
        request.reply_markup = screen.markup().map(ReplyMarkup::Inline);
        Ok(self.api.send_message(&request).await?)
    }

    /// Править экран на месте (карточка ⇄ обычный HTML — тоже правкой); не вышло (старое
    /// сообщение) — новым сообщением.
    pub(crate) async fn edit(
        &self,
        chat: i64,
        message_id: i32,
        screen: &Screen,
    ) -> Result<(), OpsError> {
        let look = self.look().screen(screen);
        if let Some(rich) = &look.rich {
            match self
                .api
                .edit_message_rich(chat, message_id, rich.clone(), look.markup())
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) if e.is_not_modified() => return Ok(()),
                Err(e) if e.is_message_gone() => {
                    self.send(chat, screen).await?;
                    return Ok(());
                }
                Err(e) if e.is_rejection() => {
                    tracing::info!(error = %e, "ops card edit rejected, trying plain HTML");
                }
                Err(e) => return Err(e.into()),
            }
        }
        let mut request = EditMessageText::html(chat, message_id, look.text.clone());
        request.reply_markup = look.markup();
        match self.api.edit_message_text(&request).await {
            Ok(_) => Ok(()),
            Err(e) if e.is_not_modified() => Ok(()),
            Err(e) if e.is_rejection() || e.is_message_gone() => {
                self.send(chat, screen).await?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn role_name(&self, role: Role) -> String {
        self.texts.t(&format!("roles.{}", role.as_db_str()))
    }

    /// Нет прав — экран с отказом.
    pub(crate) fn deny(&self, staff: &StaffMember, need: Role) -> Option<Screen> {
        (staff.role < need).then(|| {
            Screen::new(
                self.texts
                    .f("common.not_allowed", &[("role", &self.role_name(need))]),
            )
            .row(vec![btn(self.texts.t("common.home"), &Cb::Menu)])
        })
    }

    pub(crate) fn ask(&self, staff: i64, input: Input) {
        let until = Instant::now() + self.cfg.input_ttl;
        self.state(|s| {
            s.awaiting.insert(staff, Awaiting { input, until });
        });
    }

    fn take_awaiting(&self, staff: i64) -> Option<Input> {
        self.state(|s| {
            let a = s.awaiting.remove(&staff)?;
            (a.until > Instant::now()).then_some(a.input)
        })
    }

    pub(crate) fn clear_awaiting(&self, staff: i64) {
        self.state(|s| {
            s.awaiting.remove(&staff);
        });
    }

    /// Положить действие на подтверждение: одноразовый nonce, привязан к сотруднику.
    pub(crate) fn hold(&self, staff: i64, action: Action) -> String {
        let nonce = nonce();
        let until = Instant::now() + self.cfg.confirm_ttl;
        self.state(|s| {
            let now = Instant::now();
            s.pending.retain(|_, p| p.until > now);
            s.pending.insert(
                nonce.clone(),
                Pending {
                    staff,
                    until,
                    action,
                },
            );
        });
        nonce
    }

    /// Забрать действие по nonce: только свой, только живой, только один раз.
    fn take_pending(&self, staff: i64, nonce: &str) -> Option<Action> {
        self.state(|s| {
            // Чужое нажатие не трогает подтверждение: иначе любой сотрудник мог бы его сжечь.
            if s.pending.get(nonce).is_none_or(|p| p.staff != staff) {
                return None;
            }
            let p = s.pending.remove(nonce)?;
            (p.until > Instant::now()).then_some(p.action)
        })
    }

    async fn on_message(
        self: &Arc<Self>,
        staff: &StaffMember,
        message: &Message,
    ) -> Result<Screen, OpsError> {
        let text = message.text.as_deref().unwrap_or("").trim();
        // Вход в пульт — главный экран (макет: «Первая строка отвечает „надо ли мне что-то
        // делать“»); меню — кнопкой «Меню» или /menu.
        if text == self.texts.t("menu.open") {
            self.clear_awaiting(staff.tg_user_id);
            return self.dash().await;
        }
        if let Some(command) = text.strip_prefix('/') {
            self.clear_awaiting(staff.tg_user_id);
            let (name, arg) = command.split_once(' ').unwrap_or((command, ""));
            let name = name.split('@').next().unwrap_or(name);
            let arg = arg.trim();
            return match name {
                "dash" | "admin" | "panel" => self.dash().await,
                "menu" => self.menu(staff).await,
                "system" => self.system().await,
                "queue" => self.queue().await,
                "dir" => self.dirs().await,
                "promo" => self.promo().await,
                "settings" => self.settings().await,
                "bots" => self.bots().await,
                "staff" => self.staff_list(staff).await,
                "audit" => self.audit().await,
                "order" | "find" if !arg.is_empty() => self.find_order(arg).await,
                "order" | "find" => Ok(self.prompt(staff, Input::OrderSearch, "order.ask")),
                "user" if !arg.is_empty() => self.find_user(arg).await,
                "user" => Ok(self.prompt(staff, Input::UserSearch, "user.ask")),
                _ => self.menu(staff).await,
            };
        }
        match self.take_awaiting(staff.tg_user_id) {
            Some(input) => self.on_input(staff, input, message).await,
            None => self.menu(staff).await,
        }
    }

    /// Вопрос сотруднику с кнопкой «Отмена».
    pub(crate) fn prompt(&self, staff: &StaffMember, input: Input, key: &str) -> Screen {
        self.ask(staff.tg_user_id, input);
        let t = &self.texts;
        Screen::new(format!("{}\n\n{}", t.t(key), t.t("common.input_hint")))
            .row(vec![btn(t.t("common.cancel"), &Cb::Cancel)])
    }

    pub(crate) fn prompt_with(&self, staff: &StaffMember, input: Input, text: String) -> Screen {
        self.ask(staff.tg_user_id, input);
        let t = &self.texts;
        Screen::new(format!("{text}\n\n{}", t.t("common.input_hint")))
            .row(vec![btn(t.t("common.cancel"), &Cb::Cancel)])
    }

    async fn on_callback(
        self: &Arc<Self>,
        staff: &StaffMember,
        chat: i64,
        _message: &Message,
        cb: Cb,
    ) -> Result<Option<Screen>, OpsError> {
        let screen = match cb {
            Cb::Stats(days) => {
                self.send_stats(chat, days).await?;
                return Ok(None);
            }
            Cb::Menu => self.menu(staff).await?,
            Cb::Cancel => {
                self.clear_awaiting(staff.tg_user_id);
                Screen::new(self.texts.t("common.cancelled"))
                    .row(vec![btn(self.texts.t("common.home"), &Cb::Menu)])
            }
            Cb::Confirm(nonce) => self.confirm(staff, &nonce, false).await?,
            Cb::ConfirmUnknown(nonce) => self.confirm(staff, &nonce, true).await?,
            Cb::Ack(id) => {
                self.ack(staff, id).await?;
                return Ok(None);
            }
            Cb::Inbox => self.inbox().await?,
            Cb::SupportClose(client) => self.support_close(staff, client).await?,
            Cb::System => self.system().await?,
            Cb::Broadcast => self.broadcast(),
            Cb::Preview => {
                let api = self.api.clone();
                let look = self.look();
                tokio::spawn(async move {
                    crate::preview::send_preview(&api, &look, chat).await;
                });
                Screen::new(self.texts.t("system.preview_sent"))
                    .row(vec![btn(self.texts.t("common.home"), &Cb::Menu)])
            }
            Cb::SupportReply(client) => self.prompt_with(
                staff,
                actions::Input::SupportReply(client),
                self.texts
                    .f("support.reply_prompt", &[("client", &client.to_string())]),
            ),
            other => self.navigate(staff, other).await?,
        };
        Ok(Some(screen))
    }

    async fn confirm(
        &self,
        staff: &StaffMember,
        nonce: &str,
        previous_failed: bool,
    ) -> Result<Screen, OpsError> {
        let Some(action) = self.take_pending(staff.tg_user_id, nonce) else {
            return Ok(Screen::new(self.texts.t("common.expired"))
                .row(vec![btn(self.texts.t("common.home"), &Cb::Menu)]));
        };
        // Права — заново: роль могли сменить, пока висел предпросмотр.
        let staff = match self.staff(staff.tg_user_id).await? {
            Some(s) => s,
            None => return Ok(Screen::new(self.texts.t("common.expired"))),
        };
        if let Some(denied) = self.deny(&staff, action.min_role()) {
            return Ok(denied);
        }
        self.apply(&staff, action, previous_failed).await
    }
}

#[async_trait::async_trait]
impl tgbot::UpdateHook for OpsBot {
    async fn try_handle(&self, update: &Update) -> bool {
        match self.inner.handle(update).await {
            Ok(handled) => handled,
            Err(e) => {
                // Решили, что это пульту, и упали посередине: клиентскому боту не отдаём.
                tracing::warn!(error = %e, "ops update failed");
                true
            }
        }
    }

    async fn menu_button(&self, user: i64) -> Option<String> {
        match self.inner.staff(user).await {
            Ok(Some(_)) => Some(self.inner.texts.t("menu.open")),
            _ => None,
        }
    }

    async fn support_question(&self, from: &botapi::User, text: &str, order: Option<&str>) -> bool {
        match self.inner.support_question(from, text, order).await {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!(error = %e, "support question not delivered");
                false
            }
        }
    }
}

impl Inner {
    /// Вопрос клиента — всем действующим сотрудникам (макет: «Обращение · клиент · @ник» и
    /// кнопка «Ответить»). Сколько сотрудников получили.
    async fn support_question(
        &self,
        from: &botapi::User,
        text: &str,
        order: Option<&str>,
    ) -> Result<usize, OpsError> {
        let staff = {
            let mut conn = self.pool.acquire().await?;
            storage::staff::list(&mut conn).await?
        };
        self.store_question(from, text, order).await?;
        let screen = self
            .question_card(from.id, from.username.as_deref(), text, order)
            .await?;
        let mut delivered = 0;
        for member in staff.iter().filter(|m| m.disabled_at.is_none()) {
            match self.send(member.tg_user_id, &screen).await {
                Ok(_) => delivered += 1,
                Err(e) => {
                    tracing::warn!(staff = member.tg_user_id, error = %e, "support question not sent")
                }
            }
        }
        Ok(delivered)
    }

    /// Ответ сотрудника — клиенту от имени бота («Оператор Анна: …»).
    pub(crate) async fn support_reply(
        &self,
        staff: &StaffMember,
        client: i64,
        text: &str,
    ) -> Result<Screen, OpsError> {
        let home = || btn(self.texts.t("common.home"), &Cb::Menu);
        if text.is_empty() {
            return Ok(Screen::new(self.texts.t("support.empty")).row(vec![home()]));
        }
        let answer = self.texts.f(
            "support.answer",
            &[
                ("name", &texts::esc(&staff.display_name)),
                ("text", &texts::esc(text)),
            ],
        );
        match self
            .api
            .send_message(&SendMessage::html(client, answer))
            .await
        {
            Ok(_) => {
                self.close_question(client, staff.tg_user_id).await?;
                Ok(Screen::new(self.texts.f(
                    "support.sent",
                    &[("name", &texts::esc(&staff.display_name))],
                ))
                .row(vec![btn(self.texts.t("menu.b_inbox"), &Cb::Inbox), home()]))
            }
            Err(e) => Ok(Screen::new(
                self.texts
                    .f("support.failed", &[("error", &texts::esc(&e.to_string()))]),
            )
            .row(vec![home()])),
        }
    }
}

/// Одноразовый ключ подтверждения: 12 символов из времени и счётчика, перемешанных
/// `RandomState` (свой ключ на процесс) — подобрать чужой нельзя, а угадывать незачем: nonce
/// привязан к сотруднику.
fn nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(n);
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    let mut x = h.finish();
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut out = String::with_capacity(12);
    for _ in 0..12 {
        out.push(char::from(ALPHABET[(x % ALPHABET.len() as u64) as usize]));
        x /= ALPHABET.len() as u64;
        if x == 0 {
            x = h.finish().rotate_left(17) ^ n;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use domain::Direction;
    use domain::fsm::OrderState;
    use ui::Emoji;

    use super::*;

    /// Ключи вида `раздел.ключ` в кавычках из исходников пульта.
    fn used_keys() -> Vec<String> {
        let sources = [
            include_str!("lib.rs"),
            include_str!("views.rs"),
            include_str!("actions.rs"),
            include_str!("alerts.rs"),
            include_str!("invest.rs"),
        ];
        let sections = [
            "common",
            "roles",
            "names",
            "states",
            "op_status",
            "commands",
            "menu",
            "dash",
            "status",
            "dir",
            "promo",
            "queue",
            "order",
            "refusal",
            "user",
            "settings",
            "emoji_roles",
            "bots",
            "staff",
            "audit",
            "alert",
            "recon",
            "selftest",
            "capital",
            "invest",
        ];
        // Действия журнала (`admin_audit.action`) похожи на ключи, но это не тексты.
        let audit = [
            "order.take",
            "direction.set",
            "direction.open",
            "direction.close",
            "direction.clear_auto_pause",
            "promo.settings",
            "promo.switch",
            "reserve.target",
            "user.terms",
            "user.daily_limit",
            "user.note",
            "user.ban",
            "user.unban",
            "settings.flag",
            "settings.emoji_style",
            "settings.custom_emoji",
            "userbot.pause",
            "userbot.resume",
            "staff.add",
            "staff.role",
            "staff.disable",
            "capital.in",
            "capital.out",
        ];
        let mut keys = Vec::new();
        for src in sources {
            for piece in src.split('"').skip(1).step_by(2) {
                let Some((section, key)) = piece.split_once('.') else {
                    continue;
                };
                // `include_str!("invest.rs")` выше — имя файла, а не ключ.
                if sections.contains(&section)
                    && !audit.contains(&piece)
                    && !piece.ends_with(".rs")
                    && !key.is_empty()
                    && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    keys.push(piece.to_owned());
                }
            }
        }
        keys
    }

    #[test]
    fn every_used_text_exists() {
        let t = Texts::builtin().unwrap();
        let keys = used_keys();
        assert!(keys.len() > 150, "{}", keys.len());
        for key in keys {
            assert!(t.has(&key), "ops/ru.toml has no {key}");
        }
        // Ключи, которые собираются в коде.
        for d in Direction::ALL {
            assert!(t.has(&format!("names.{}", d.code())));
        }
        for p in domain::Platform::ALL {
            assert!(t.has(&format!("names.{}", p.db_name())));
        }
        for s in OrderState::ALL {
            assert!(t.has(&format!("states.{}", s.as_db_str())), "{s:?}");
        }
        for s in [
            "pending",
            "dispatched",
            "succeeded",
            "failed",
            "unknown",
            "cancelled",
        ] {
            assert!(t.has(&format!("op_status.{s}")));
        }
        for r in Role::ALL {
            assert!(t.has(&format!("roles.{}", r.as_db_str())));
        }
        for e in Emoji::ALL {
            assert!(t.has(&format!("emoji_roles.{}", e.key())), "{e:?}");
        }
        for step in ["balance", "issue", "claim", "balance_after"] {
            assert!(t.has(&format!("selftest.s_{step}")));
        }
    }

    #[test]
    fn nonces_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            let n = nonce();
            assert_eq!(n.len(), 12);
            assert!(seen.insert(n));
        }
    }
}
