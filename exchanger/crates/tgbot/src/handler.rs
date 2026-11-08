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
        let screen = self.with_menu_button(chat, screen).await;
        self.api.send_html(chat, &screen).await?;
        Ok(())
    }

    /// Главное меню инвестора — с «💼 Инвестиции», персонала — с входом в админ-панель
    /// (чат лички = id пользователя).
    async fn with_menu_button(&self, chat: i64, screen: &Screen) -> Screen {
        let mut screen = screen.clone();
        if let ui::Keyboard::Reply(markup) = &mut screen.keyboard {
            if self.is_investor(chat).await {
                markup.keyboard.push(vec![self.ui().invest_menu_button()]);
            }
            if let Some(hook) = self.hook()
                && let Some(label) = hook.menu_button(chat).await
            {
                markup
                    .keyboard
                    .push(vec![botapi::KeyboardButton::new(label)]);
            }
        }
        screen
    }

    pub(crate) async fn reply_error(&self, chat: i64, error: &UiError) -> Result<(), TgBotError> {
        self.reply(chat, &self.ui().error(error)).await
    }

    /// Справочный экран: rich, если он есть и клиент не просил простой вид; иначе HTML.
    pub(crate) async fn reply_doc(
        &self,
        chat: i64,
        user: i64,
        screen: &Screen,
    ) -> Result<(), TgBotError> {
        let rich = match &screen.rich_html {
            Some(rich) if !self.prefers_plain(user) => rich,
            _ => return self.reply(chat, screen).await,
        };
        let screen = &self.with_menu_button(chat, screen).await;
        let keyboard = self.ui().rich_keyboard(screen);
        let (message, sent_rich) = self.api.send_rich(chat, rich, &keyboard, screen).await?;
        if sent_rich {
            self.remember_rich(message.chat.id, message.message_id, screen);
        }
        Ok(())
    }

    /// Заменить экран навигации (выбор направления, инструкция) в том же сообщении. Сообщение
    /// заявки не трогаем никогда: в нём может быть ссылка на чек выплаты.
    pub(crate) async fn replace(
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

    /// Условия направлений для клиента: с личными условиями — свои (без кэша), иначе общие.
    pub(crate) async fn terms_for(&self, user: i64) -> Result<TermsSnapshot, TgBotError> {
        let personal = {
            let mut conn = self.pool.acquire().await?;
            storage::user_terms::has_any(&mut conn, user).await?
        };
        if !personal {
            return self.terms().await;
        }
        let mut conn = self.pool.acquire().await?;
        crate::terms::load(
            &mut conn,
            self.cfg.reserve_safety,
            self.cfg.bind_username.as_deref(),
            Some(user),
        )
        .await
    }

    /// Общие условия направлений (кэш на `terms_ttl`).
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
                None,
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
                payout: e
                    .invoice_amount
                    .or(e.payout_amount)
                    .and_then(|a| Money::new(a, e.asset).ok()),
                state: e.state,
            })
            .collect();
        let next = more.then(|| page.saturating_add(1));
        let summary = {
            let mut conn = self.pool.acquire().await?;
            storage::client::history_totals(&mut conn, user).await?
        };
        let summary = ui::HistorySummary {
            completed: summary.completed,
            volume: summary.volume,
        };
        let screen = self
            .ui()
            .history(&rows, next, Some(&summary), chrono::Utc::now());
        self.reply_doc(chat, user, &screen).await
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
        self.reply_doc(chat, user, &self.ui().order_card(&card, chrono::Utc::now()))
            .await
    }

    async fn menu(&self, chat: i64, user: i64, action: MenuAction) -> Result<(), TgBotError> {
        // Любая кнопка меню отменяет ожидание чека пополнения или суммы вывода.
        self.invest_forget(user);
        match action {
            MenuAction::Invest => self.invest_open(chat, user, None).await,
            MenuAction::Exchange => {
                let terms = self.terms_for(user).await?;
                self.reply(chat, &self.ui().exchange_menu(&terms.terms))
                    .await
            }
            MenuAction::PayInvoice => {
                let terms = self.terms_for(user).await?;
                self.reply(chat, &self.ui().invoice_prompt(&terms.terms))
                    .await
            }
            MenuAction::Rates => {
                let terms = self.terms_for(user).await?;
                self.reply_doc(chat, user, &self.ui().terms(&terms.terms))
                    .await
            }
            MenuAction::History => self.history(chat, user, 0).await,
            MenuAction::Support => {
                self.reply(
                    chat,
                    &self.ui().support(self.cfg.support_contact.as_deref()),
                )
                .await
            }
            MenuAction::Help => {
                let terms = self.terms_for(user).await?;
                self.reply(chat, &self.ui().help(&terms.terms)).await
            }
        }
    }

    async fn command(&self, chat: i64, user: i64, command: Command) -> Result<(), TgBotError> {
        self.invest_forget(user);
        match command {
            Command::Start(_) => {
                let terms = self.terms_for(user).await?;
                self.reply_doc(chat, user, &self.ui().welcome(&terms.terms))
                    .await
            }
            Command::Terms => {
                let terms = self.terms_for(user).await?;
                self.reply_doc(chat, user, &self.ui().terms(&terms.terms))
                    .await
            }
            Command::History => self.history(chat, user, 0).await,
            Command::Help => {
                let terms = self.terms_for(user).await?;
                self.reply(chat, &self.ui().help(&terms.terms)).await
            }
            Command::Privacy => self.reply_doc(chat, user, &self.ui().privacy()).await,
        }
    }
}

/// Одно обновление.
pub(crate) async fn handle(inner: &Arc<Inner>, update: Update) -> Result<(), TgBotError> {
    // Админ-панель первой: её кнопки, команды и ответы на её вопросы.
    if let Some(hook) = inner.hook()
        && hook.try_handle(&update).await
    {
        return Ok(());
    }
    if let Some(query) = update.callback_query {
        return callback(inner, query).await;
    }
    // Правка — тоже приход ссылки: чек, созданный через `@send` / `@xrocket` прямо в этом
    // чате, сначала приходит заглушкой «Создание чека…», а ссылка появляется кнопкой в
    // правке того же сообщения (docs/LOVEC-PORTING.md §8.1, шаг 5).
    match (update.message, update.edited_message) {
        (Some(message), _) => message_update(inner, &message, Arrival::New).await,
        (None, Some(message)) => message_update(inner, &message, Arrival::Edited).await,
        (None, None) => Ok(()),
    }
}

/// Как пришло сообщение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
    New,
    /// Правка ранее присланного сообщения: берём из неё только новые ссылки на чек или счёт,
    /// без команд, кнопок меню и подсказок.
    Edited,
}

/// Кошелёк, через инлайн-режим которого отправлено сообщение (`via_bot`).
fn wallet_via_bot(message: &Message) -> Option<domain::Platform> {
    message
        .via_bot
        .as_ref()
        .and_then(|b| b.username.as_deref())
        .and_then(parsers::links::official_platform)
}

async fn message_update(
    inner: &Arc<Inner>,
    message: &Message,
    arrival: Arrival,
) -> Result<(), TgBotError> {
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

    if arrival == Arrival::Edited {
        let found = extract_from_message(text, &message.entity_urls(), &message.button_urls());
        // Только то, чего в этом сообщении ещё не было: правка без новой ссылки — молча.
        if !inner.first_seen_links(chat, message.message_id, &found) {
            return Ok(());
        }
        tracing::info!(
            user = from.id,
            via = ?wallet_via_bot(message),
            "check link arrived in an edited message"
        );
        if inner.invest_message(chat, from.id, text, &found).await? {
            return Ok(());
        }
        return intake::process(inner, chat, from.id, &found).await;
    }

    if let Some(command) = Command::parse(text, inner.bot_username()) {
        // Диплинк `t.me/<бот>?start=CQ…` — это чек.
        if let Command::Start(Some(payload)) = &command
            && let Some(found) = parse_bare_code(payload)
        {
            return intake::process(inner, chat, from.id, &[found]).await;
        }
        return inner.command(chat, from.id, command).await;
    }
    if message.forward_origin.is_none()
        && let Some(action) = inner.ui().parse_menu(text)
    {
        return inner.menu(chat, from.id, action).await;
    }

    let mut found = extract_from_message(text, &message.entity_urls(), &message.button_urls());
    if found.is_empty()
        && let Some(bare) = parse_bare_code(text)
    {
        found.push(bare);
    }
    // Запомнить ссылки этого сообщения: правка без новых ссылок не станет второй заявкой.
    inner.first_seen_links(chat, message.message_id, &found);
    // Чек через инлайн-режим кошелька ещё создаётся: ссылка придёт правкой — подождать.
    if found.is_empty()
        && let Some(platform) = wallet_via_bot(message)
    {
        crate::checkwait::start(inner.clone(), chat, from.id, message.message_id, platform);
        return Ok(());
    }
    // Инвестор нажал «Пополнить» или выбрал кошелёк вывода: чек или сумма — туда.
    if (message.forward_origin.is_none() || !found.is_empty())
        && inner.invest_message(chat, from.id, text, &found).await?
    {
        return Ok(());
    }
    if found.is_empty() {
        return inner.reply(chat, &inner.ui().no_link()).await;
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
    match action.clone() {
        Callback::Direction(direction) => {
            let terms = inner.terms_for(user).await?;
            let screen = inner.ui().send_check(&terms.terms, direction);
            inner.replace(chat, message, &screen).await
        }
        Callback::HowTo(direction) => {
            let terms = inner.terms_for(user).await?;
            let screen = inner.ui().check_howto(&terms.terms, direction);
            inner.replace(chat, message, &screen).await
        }
        Callback::Exchange => inner.menu(chat, user, MenuAction::Exchange).await,
        Callback::PayInvoice => inner.menu(chat, user, MenuAction::PayInvoice).await,
        Callback::Terms => inner.menu(chat, user, MenuAction::Rates).await,
        Callback::Privacy => inner.command(chat, user, Command::Privacy).await,
        Callback::History { page } => inner.history(chat, user, page).await,
        Callback::Order(id) => inner.order_card(chat, user, &id).await,
        Callback::Details(id) | Callback::HideDetails(id) => {
            let open = matches!(action, Callback::Details(_));
            match inner.own_order(user, &id).await? {
                Some(view) => {
                    inner.memory(|m| {
                        if open {
                            m.details_open.insert(view.id);
                        } else {
                            m.details_open.remove(&view.id);
                        }
                    });
                    crate::orders::ensure_message(
                        inner,
                        view.id,
                        crate::orders::Create::UnlessRejected,
                    )
                    .await
                }
                None => Ok(()),
            }
        }
        Callback::Support(_) => inner.menu(chat, user, MenuAction::Support).await,
        Callback::Cancel(None) => {
            inner
                .replace(chat, message, &inner.ui().cancelled(None))
                .await
        }
        Callback::PlainView => plain_view(inner, chat, user, message).await,
        Callback::PayByCheck(id) => match inner.own_order(user, &id).await? {
            Some(view) => crate::orders::show_awaiting(inner, &view).await,
            None => Ok(()),
        },
        // «Отмена» по котировке и «Вернуть деньги» при недоплате: движок закрывает заявку
        // (денег нет — отмена, часть пришла — возврат), сообщение заявки обновит подписка.
        Callback::Cancel(Some(id)) | Callback::Refund(id) => {
            if let Some(view) = inner.own_order(user, &id).await? {
                let closed = inner.engine.cancel_invoice(user, view.id).await?;
                tracing::info!(order = view.id, closed, "client asked to cancel");
                if !closed {
                    // Отменять поздно: показать, что с заявкой сейчас.
                    crate::orders::ensure_message(
                        inner,
                        view.id,
                        crate::orders::Create::UnlessRejected,
                    )
                    .await?;
                }
            }
            Ok(())
        }
        // Подписки «сообщить, когда заработает» пока нет — только ответ на нажатие.
        Callback::Notify(_) => Ok(()),
        Callback::Invest(action) => inner.invest_callback(chat, user, message, action).await,
    }
}

impl Inner {
    /// Заявка клиента по публичному номеру из кнопки. Чужую не отдаёт.
    async fn own_order(
        &self,
        user: i64,
        public_id: &str,
    ) -> Result<Option<storage::client::OrderView>, TgBotError> {
        let mut conn = self.pool.acquire().await?;
        Ok(storage::client::order_view_for_user(&mut conn, user, public_id).await?)
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
