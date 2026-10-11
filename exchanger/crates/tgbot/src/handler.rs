//! Разбор одного обновления: сообщение (команда, кнопка меню, ссылка) или нажатие кнопки.
//!
//! Порядок для каждого обновления: клиент в `users` → активный бан (тогда только экран бана)
//! → действие. Бот работает только в личке; группы и каналы игнорируются.

use std::sync::Arc;
use std::time::Instant;

use botapi::{CallbackQuery, ChatType, Message, Update, User};
use domain::Money;
use parsers::links::{extract_from_message, parse_bare_code};
use ui::{Callback, Command, HistoryRow, MenuAction, Screen, UiError};

use crate::terms::TermsSnapshot;
use crate::{Inner, Notice, RateLimits, TgBotError, intake};

/// Строк истории на странице (SPEC S10).
const HISTORY_PAGE: i64 = 10;

impl Inner {
    /// Экран обычным HTML.
    pub(crate) async fn reply(&self, chat: i64, screen: &Screen) -> Result<(), TgBotError> {
        self.api.send_html(chat, screen).await?;
        Ok(())
    }

    pub(crate) async fn reply_error(&self, chat: i64, error: &UiError) -> Result<(), TgBotError> {
        self.reply(chat, &self.ui.error(error)).await
    }

    /// Справочный экран: rich, если он есть и клиент не просил простой вид; иначе HTML.
    async fn reply_doc(&self, chat: i64, user: i64, screen: &Screen) -> Result<(), TgBotError> {
        let rich = match &screen.rich_html {
            Some(rich) if !self.prefers_plain(user) => rich,
            _ => return self.reply(chat, screen).await,
        };
        let keyboard = self.ui.rich_keyboard(screen);
        let (message, sent_rich) = self.api.send_rich(chat, rich, &keyboard, screen).await?;
        if sent_rich {
            self.remember_rich(message.chat.id, message.message_id, screen);
        }
        Ok(())
    }

    /// Заменить экран навигации (выбор направления, инструкция) в том же сообщении. Сообщение
    /// заявки не трогаем никогда: в нём может быть ссылка на чек выплаты.
    async fn replace(
        &self,
        chat: i64,
        message: Option<&Message>,
        screen: &Screen,
    ) -> Result<(), TgBotError> {
        if let Some(message) = message.filter(|m| !m.is_inaccessible()) {
            let progress = {
                let mut conn = self.pool.acquire().await?;
                storage::client::is_progress_message(&mut conn, chat, message.message_id).await?
            };
            if !progress {
                match self.api.edit_html(chat, message.message_id, screen).await {
                    Ok(crate::api::Edited::Done | crate::api::Edited::NotModified) => {
                        return Ok(());
                    }
                    Ok(crate::api::Edited::Gone) => {}
                    Err(e) if e.is_rejection() => {
                        tracing::debug!(error = %e, "cannot edit in place, sending a new message");
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
        self.reply(chat, screen).await
    }

    /// Условия направлений (кэш на `terms_ttl`).
    pub(crate) async fn terms(&self) -> Result<TermsSnapshot, TgBotError> {
        let ttl = self.cfg.terms_ttl;
        let cached = {
            let guard = match self.terms_cache.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            guard
                .as_ref()
                .filter(|(at, _)| at.elapsed() < ttl)
                .map(|(_, t)| t.clone())
        };
        if let Some(terms) = cached {
            return Ok(terms);
        }
        let terms = {
            let mut conn = self.pool.acquire().await?;
            crate::terms::load(
                &mut conn,
                self.cfg.reserve_safety,
                self.cfg.bind_username.as_deref(),
            )
            .await?
        };
        if let Ok(mut guard) = self.terms_cache.lock() {
            *guard = Some((Instant::now(), terms.clone()));
        }
        Ok(terms)
    }

    /// `settings.user_rate_limit` (кэш на `settings_ttl`). БД недоступна — значения по
    /// умолчанию: лимит не должен отключаться из-за сбоя.
    pub(crate) async fn rate_limits(&self) -> RateLimits {
        let ttl = self.cfg.settings_ttl;
        let cached = match self.limits_cache.lock() {
            Ok(g) => g.filter(|(at, _)| at.elapsed() < ttl).map(|(_, l)| l),
            Err(p) => p.into_inner().map(|(_, l)| l),
        };
        if let Some(limits) = cached {
            return limits;
        }
        let loaded = async {
            let mut conn = self.pool.acquire().await?;
            storage::settings::get(&mut conn, "user_rate_limit").await
        }
        .await;
        let limits = match loaded {
            Ok(Some(value)) => RateLimits::from_json(&value),
            Ok(None) => RateLimits::default(),
            Err(e) => {
                tracing::warn!(error = %e, "user_rate_limit not loaded, using defaults");
                return RateLimits::default();
            }
        };
        if let Ok(mut guard) = self.limits_cache.lock() {
            *guard = Some((Instant::now(), limits));
        }
        limits
    }

    /// Клиент в `users` и его активный бан.
    async fn touch_user(&self, user: &User) -> Result<Option<UiError>, TgBotError> {
        let mut conn = self.pool.acquire().await?;
        storage::users::upsert(
            &mut conn,
            user.id,
            &storage::users::Profile {
                username: user.username.as_deref(),
                first_name: Some(user.first_name.as_str()).filter(|s| !s.is_empty()),
                language_code: user.language_code.as_deref(),
            },
        )
        .await?;
        let ban = storage::client::active_ban(&mut conn, user.id).await?;
        Ok(ban.map(|b| UiError::Banned {
            until: b.expires_at,
            reason: b.reason,
        }))
    }

    async fn banned(&self, chat: i64, user: i64, ban: &UiError) -> Result<(), TgBotError> {
        tracing::info!(user, "update from a banned client ignored");
        if self.should_notify(user, Notice::Banned) {
            self.reply_error(chat, ban).await?;
        }
        Ok(())
    }

    async fn history(&self, chat: i64, user: i64, page: u32) -> Result<(), TgBotError> {
        let entries = {
            let mut conn = self.pool.acquire().await?;
            storage::client::history(
                &mut conn,
                user,
                HISTORY_PAGE + 1,
                i64::from(page) * HISTORY_PAGE,
            )
            .await?
        };
        let more = usize::try_from(HISTORY_PAGE).is_ok_and(|n| entries.len() > n);
        let rows: Vec<HistoryRow> = entries
            .into_iter()
            .take(usize::try_from(HISTORY_PAGE).unwrap_or(10))
            .map(|e| HistoryRow {
                order: e.public_id,
                created_at: e.created_at,
                direction: e.direction,
                amount: e.intake_amount.and_then(|a| Money::new(a, e.asset).ok()),
                state: e.state,
            })
            .collect();
        let next = more.then(|| page.saturating_add(1));
        self.reply_doc(chat, user, &self.ui.history(&rows, next))
            .await
    }

    async fn order_card(&self, chat: i64, user: i64, public_id: &str) -> Result<(), TgBotError> {
        let view = {
            let mut conn = self.pool.acquire().await?;
            storage::client::order_view_for_user(&mut conn, user, public_id).await?
        };
        // Чужая или несуществующая заявка — молча: номер мог подобрать кто угодно.
        let Some(view) = view else {
            return Ok(());
        };
        let card = crate::orders::card(&view);
        self.reply_doc(chat, user, &self.ui.order_card(&card)).await
    }

    async fn menu(&self, chat: i64, user: i64, action: MenuAction) -> Result<(), TgBotError> {
        match action {
            MenuAction::Exchange => {
                let terms = self.terms().await?;
                self.reply(chat, &self.ui.exchange_menu(&terms.terms)).await
            }
            MenuAction::PayInvoice => {
                let terms = self.terms().await?;
                self.reply(chat, &self.ui.invoice_prompt(&terms.terms))
                    .await
            }
            MenuAction::Rates => {
                let terms = self.terms().await?;
                self.reply_doc(chat, user, &self.ui.terms(&terms.terms))
                    .await
            }
            MenuAction::History => self.history(chat, user, 0).await,
            MenuAction::Support => {
                self.reply(chat, &self.ui.support(self.cfg.support_contact.as_deref()))
                    .await
            }
        }
    }

    async fn command(&self, chat: i64, user: i64, command: Command) -> Result<(), TgBotError> {
        match command {
            Command::Start(_) => {
                let terms = self.terms().await?;
                self.reply_doc(chat, user, &self.ui.welcome(&terms.terms))
                    .await
            }
            Command::Terms => {
                let terms = self.terms().await?;
                self.reply_doc(chat, user, &self.ui.terms(&terms.terms))
                    .await
            }
            Command::History => self.history(chat, user, 0).await,
            Command::Help => {
                let terms = self.terms().await?;
                self.reply(chat, &self.ui.help(&terms.terms)).await
            }
            Command::Privacy => self.reply_doc(chat, user, &self.ui.privacy()).await,
        }
    }
}

/// Одно обновление.
pub(crate) async fn handle(inner: &Arc<Inner>, update: Update) -> Result<(), TgBotError> {
    if let Some(query) = update.callback_query {
        return callback(inner, query).await;
    }
    // Правки сообщений не обрабатываем: исправленная ссылка — это новое сообщение.
    match update.message {
        Some(message) => message_update(inner, &message).await,
        None => Ok(()),
    }
}

async fn message_update(inner: &Arc<Inner>, message: &Message) -> Result<(), TgBotError> {
    if message.chat.kind != ChatType::Private {
        return Ok(());
    }
    let Some(from) = message.from.as_ref().filter(|u| !u.is_bot) else {
        return Ok(());
    };
    let chat = message.chat.id;
    if let Some(ban) = inner.touch_user(from).await? {
        return inner.banned(chat, from.id, &ban).await;
    }
    let text = message.text_and_entities().map_or("", |(text, _)| text);

    if let Some(command) = Command::parse(text, inner.bot_username()) {
        // Диплинк `t.me/<бот>?start=CQ…` — это чек.
        if let Command::Start(Some(payload)) = &command
            && let Some(link) = parse_bare_code(payload)
        {
            return intake::process(inner, chat, from.id, &[Ok(link)]).await;
        }
        return inner.command(chat, from.id, command).await;
    }
    if message.forward_origin.is_none()
        && let Some(action) = inner.ui.parse_menu(text)
    {
        return inner.menu(chat, from.id, action).await;
    }

    let mut found = extract_from_message(text, &message.entity_urls(), &message.button_urls());
    if found.is_empty()
        && let Some(link) = parse_bare_code(text)
    {
        found.push(Ok(link));
    }
    if found.is_empty() {
        return inner.reply(chat, &inner.ui.no_link()).await;
    }
    intake::process(inner, chat, from.id, &found).await
}

/// Нажатие кнопки. Ответ на нажатие — всегда, даже если обработка упала.
async fn callback(inner: &Arc<Inner>, query: CallbackQuery) -> Result<(), TgBotError> {
    let result = callback_action(inner, &query).await;
    inner.api.answer(&query.id).await;
    result
}

async fn callback_action(inner: &Arc<Inner>, query: &CallbackQuery) -> Result<(), TgBotError> {
    let from = &query.from;
    if from.is_bot {
        return Ok(());
    }
    let message = query.message.as_ref();
    if message.is_some_and(|m| m.chat.kind != ChatType::Private) {
        return Ok(());
    }
    // Личка: чат клиента = его id (сообщение кнопки могло стать недоступным).
    let chat = message.map_or(from.id, |m| m.chat.id);
    if let Some(ban) = inner.touch_user(from).await? {
        return inner.banned(chat, from.id, &ban).await;
    }
    let Some(action) = query.data.as_deref().and_then(Callback::parse) else {
        return Ok(());
    };
    let user = from.id;
    match action {
        Callback::Direction(direction) => {
            let terms = inner.terms().await?;
            let screen = inner.ui.send_check(&terms.terms, direction);
            inner.replace(chat, message, &screen).await
        }
        Callback::HowTo(direction) => {
            let terms = inner.terms().await?;
            let screen = inner.ui.check_howto(&terms.terms, direction);
            inner.replace(chat, message, &screen).await
        }
        Callback::Exchange => inner.menu(chat, user, MenuAction::Exchange).await,
        Callback::Terms => inner.menu(chat, user, MenuAction::Rates).await,
        Callback::Privacy => inner.command(chat, user, Command::Privacy).await,
        Callback::History { page } => inner.history(chat, user, page).await,
        Callback::Order(id) => inner.order_card(chat, user, &id).await,
        Callback::Support(_) => inner.menu(chat, user, MenuAction::Support).await,
        Callback::Cancel(None) => {
            inner
                .replace(chat, message, &inner.ui.cancelled(None))
                .await
        }
        Callback::PlainView => plain_view(inner, chat, user, message).await,
        // Кнопки потока оплаты счёта: сам поток ещё не подключён, таких сообщений бот не
        // отправляет. Подписки «сообщить, когда заработает» пока нет — только ответ на нажатие.
        Callback::Cancel(Some(_))
        | Callback::PayByCheck(_)
        | Callback::Refund(_)
        | Callback::Notify(_) => Ok(()),
    }
}

/// «Не отображается?»: дальше этому клиенту — простой вид, а этот экран — сразу им.
async fn plain_view(
    inner: &Arc<Inner>,
    chat: i64,
    user: i64,
    message: Option<&Message>,
) -> Result<(), TgBotError> {
    let screen = inner.memory(|m| {
        m.plain_view.insert(user);
        message.and_then(|msg| m.rich_sent.get(&(chat, msg.message_id)).cloned())
    });
    match screen {
        Some(screen) => inner.reply(chat, &screen).await,
        None => Ok(()),
    }
}
