//! Разговор одной операции с ботом кошелька.
//!
//! [`Conv`] держит ворота аккаунта на всё время операции и подписан на поток сообщений
//! с самого начала — ответ, пришедший раньше, чем вернулся вызов отправки, не теряется.
//! Всё, что приходит, сначала уходит в журнал, потом разбирается.
//!
//! Ответ на команду — сообщение бота (не наше) в том же чате с id **больше** id команды:
//! новое или правка такого сообщения. Правка более старого сообщения ответом не считается
//! (это может быть поздний ответ на прошлую операцию). Если за `reply_timeout` ответа нет,
//! смотрим историю чата после id команды, а чат уходит в карантин: следующая команда в нём
//! ждёт, пока поздний ответ на эту не перестанет быть возможным.

use tokio::sync::MutexGuard;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::time::Instant;

use super::UserbotWallet;
use super::gate::{GateState, Pace};
use crate::transport::{CallbackAnswer, Chat, RawMessage, Transport, TransportError};
use crate::wallet::BotError;

/// Отправка не удалась.
#[derive(Debug)]
pub(crate) enum SendError {
    /// Точно не отправлено: деньги не двигались.
    NotSent(BotError),
    /// Могло уйти, но подтвердить и найти своё сообщение не удалось.
    Unknown(String),
}

/// Отправленная команда.
#[derive(Debug, Clone)]
pub(crate) struct Sent {
    pub msg: RawMessage,
    /// Команда найдена в истории после `RANDOM_ID_DUPLICATE` (ушла раньше, в прошлом вызове
    /// или до сбоя): ответ мог прийти до подписки — сначала смотрим историю.
    pub located: bool,
}

pub(crate) struct Conv<'a, T: Transport> {
    w: &'a UserbotWallet<T>,
    gate: MutexGuard<'a, GateState>,
    rx: Receiver<RawMessage>,
    /// Всё, что пришло в поток за время операции, в порядке прихода (версии правок — отдельно).
    seen: Vec<RawMessage>,
    op: Option<String>,
}

impl<'a, T: Transport> Conv<'a, T> {
    /// Взять ворота аккаунта и подписаться на поток. `op` — ключ операции для журнала.
    pub async fn open(w: &'a UserbotWallet<T>, op: Option<&str>) -> Self {
        let gate = w.gate.lock().await;
        let rx = w.transport.subscribe();
        Self {
            w,
            gate,
            rx,
            seen: Vec::new(),
            op: op.map(str::to_owned),
        }
    }

    pub fn transport(&self) -> &T {
        &self.w.transport
    }

    pub fn platform(&self) -> domain::Platform {
        self.w.platform
    }

    pub fn deadline(&self) -> Instant {
        Instant::now() + self.w.cfg.reply_timeout
    }

    /// Записать сообщение в журнал. `ours` — наша команда или признанный ответ на неё.
    pub async fn record(&self, msg: &RawMessage, ours: bool) {
        let op = if ours { self.op.as_deref() } else { None };
        self.w.log.record(self.w.gate.label(), msg, op).await;
    }

    async fn push(&mut self, msg: RawMessage) {
        self.record(&msg, false).await;
        self.seen.push(msg);
    }

    /// Забрать из потока всё, что уже пришло.
    async fn pump(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(msg) => self.push(msg).await,
                Err(TryRecvError::Lagged(n)) => {
                    tracing::warn!(
                        account = self.w.gate.label(),
                        lost = n,
                        "update stream lagged; history will be checked"
                    );
                }
                Err(TryRecvError::Empty | TryRecvError::Closed) => return,
            }
        }
    }

    /// Ждать следующее сообщение потока до `deadline`. `false` — время вышло или поток закрыт.
    async fn recv_until(&mut self, deadline: Instant) -> bool {
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Ok(msg)) => {
                    self.push(msg).await;
                    return true;
                }
                Ok(Err(RecvError::Lagged(n))) => {
                    tracing::warn!(
                        account = self.w.gate.label(),
                        lost = n,
                        "update stream lagged; history will be checked"
                    );
                }
                Ok(Err(RecvError::Closed)) => {
                    tokio::time::sleep_until(deadline).await;
                    return false;
                }
                Err(_) => return false,
            }
        }
    }

    /// Отметка «с этого места» для [`Conv::wait`]: ставить до действия, ответ на которое ждём.
    pub async fn mark(&mut self) -> usize {
        self.pump().await;
        self.seen.len()
    }

    /// Дождаться права на действие; на паузе FLOOD_WAIT — ошибка «не выполнялось».
    pub async fn pace(&mut self, kind: Pace) -> Result<(), BotError> {
        let cfg = self.w.cfg.clone();
        self.gate
            .pace(&cfg, kind)
            .await
            .map_err(BotError::FloodWait)
    }

    /// Ошибка транспорта, при которой запрос точно не выполнился.
    pub fn not_sent(&mut self, e: TransportError) -> BotError {
        match e {
            TransportError::FloodWait(d) => {
                self.gate.flood(d);
                tracing::warn!(account = self.w.gate.label(), wait = ?d, "FLOOD_WAIT");
                BotError::FloodWait(d)
            }
            TransportError::Rpc { ref name, .. }
                if name == "PEER_FLOOD" || name.starts_with("USER_RESTRICTED") =>
            {
                tracing::error!(account = self.w.gate.label(), rpc = %name, "account restricted");
                BotError::AccountRestricted
            }
            TransportError::NotAuthorized => {
                BotError::Unavailable("account is not authorized".to_owned())
            }
            other => BotError::Transport(other),
        }
    }

    /// Чат в карантине после команды без ответа: дождаться конца, записывая всё, что придёт.
    pub async fn wait_quarantine(&mut self, chat: Chat) {
        if let Some(until) = self.gate.quarantine_until(chat) {
            tracing::info!(
                account = self.w.gate.label(),
                ?chat,
                "waiting for late replies"
            );
            while self.recv_until(until).await {}
        }
    }

    /// Команда в `chat` осталась без ответа: поздний ответ не должен достаться следующей.
    pub fn quarantine(&mut self, chat: Chat) {
        let wait = self.w.cfg.late_reply_quarantine;
        self.gate.set_quarantine(chat, wait);
    }

    /// Отправить текст. Повтор после «исход неизвестен» — только с тем же `random_id`;
    /// `RANDOM_ID_DUPLICATE` значит «уже отправлено», и своё сообщение ищем в истории.
    pub async fn send(
        &mut self,
        chat: Chat,
        text: &str,
        random_id: i64,
        kind: Pace,
    ) -> Result<Sent, SendError> {
        self.pace(kind).await.map_err(SendError::NotSent)?;
        let first = self.w.transport.send_text(chat, text, random_id).await;
        let result = match first {
            Err(e) if e.outcome_unknown() => {
                tracing::warn!(account = self.w.gate.label(), error = %e, "send outcome unknown, resending with the same random_id");
                if let Err(flood) = self.pace(kind).await {
                    return Err(SendError::Unknown(format!(
                        "{e}; resend impossible: {flood}"
                    )));
                }
                match self.w.transport.send_text(chat, text, random_id).await {
                    Err(e2) if e2 != TransportError::RandomIdDuplicate => {
                        if let TransportError::FloodWait(d) = e2 {
                            self.gate.flood(d);
                        }
                        return Err(SendError::Unknown(format!("{e}; resend: {e2}")));
                    }
                    other => other,
                }
            }
            other => other,
        };
        match result {
            Ok(msg) => {
                self.record(&msg, true).await;
                Ok(Sent {
                    msg,
                    located: false,
                })
            }
            Err(TransportError::RandomIdDuplicate) => self.locate(chat, text).await,
            Err(e) => Err(SendError::NotSent(self.not_sent(e))),
        }
    }

    /// Найти своё уже отправленное сообщение: последнее наше с этим текстом.
    async fn locate(&mut self, chat: Chat, text: &str) -> Result<Sent, SendError> {
        let recent = self
            .recent(chat)
            .await
            .map_err(|e| SendError::Unknown(format!("RANDOM_ID_DUPLICATE; history: {e}")))?;
        match recent.into_iter().rev().find(|m| m.out && m.text == text) {
            Some(msg) => {
                self.record(&msg, true).await;
                Ok(Sent { msg, located: true })
            }
            None => Err(SendError::Unknown(
                "RANDOM_ID_DUPLICATE, but the message is not in recent history".to_owned(),
            )),
        }
    }

    /// Нажать callback-кнопку. Без повторов: второе нажатие могло бы второй раз заплатить.
    pub async fn press(
        &mut self,
        chat: Chat,
        msg_id: i32,
        data: &[u8],
    ) -> Result<CallbackAnswer, SendError> {
        self.pace(Pace::Bot).await.map_err(SendError::NotSent)?;
        match self.w.transport.press(chat, msg_id, data).await {
            Ok(answer) => Ok(answer),
            Err(e) if e.outcome_unknown() => Err(SendError::Unknown(e.to_string())),
            Err(e) => Err(SendError::NotSent(self.not_sent(e))),
        }
    }

    /// Последние сообщения чата (без темпа: чтение ботам не пишет).
    pub async fn recent(&self, chat: Chat) -> Result<Vec<RawMessage>, TransportError> {
        let msgs = self.w.transport.recent(chat, self.w.cfg.scan_depth).await?;
        for m in &msgs {
            self.record(m, false).await;
        }
        Ok(msgs)
    }

    async fn history_after(&self, chat: Chat, after_id: i32) -> Vec<RawMessage> {
        match self
            .w
            .transport
            .history(chat, after_id, self.w.cfg.scan_depth)
            .await
        {
            Ok(msgs) => {
                for m in &msgs {
                    self.record(m, false).await;
                }
                msgs
            }
            Err(e) => {
                tracing::warn!(account = self.w.gate.label(), error = %e, "history unavailable");
                Vec::new()
            }
        }
    }

    /// Ждать сообщение бота в `chat` с id > `after_id`, пришедшее после отметки `mark`,
    /// для которого `accept` вернёт `true`. До `deadline`, затем — по истории чата.
    /// `history_first` — сначала посмотреть историю (команда ушла раньше подписки).
    pub async fn wait(
        &mut self,
        chat: Chat,
        after_id: i32,
        mark: usize,
        deadline: Instant,
        history_first: bool,
        accept: impl FnMut(&RawMessage) -> bool,
    ) -> Option<RawMessage> {
        let candidate = move |m: &RawMessage| m.chat == chat && !m.out && m.id > after_id;
        self.wait_where(
            chat,
            after_id,
            mark,
            deadline,
            history_first,
            candidate,
            accept,
        )
        .await
    }

    /// Ждать правку сообщения `msg_id` (в том числе нашего: инлайн-чек в «Избранном»).
    pub async fn wait_edit(
        &mut self,
        chat: Chat,
        msg_id: i32,
        mark: usize,
        deadline: Instant,
        accept: impl FnMut(&RawMessage) -> bool,
    ) -> Option<RawMessage> {
        let candidate = move |m: &RawMessage| m.chat == chat && m.id == msg_id;
        self.wait_where(chat, msg_id - 1, mark, deadline, false, candidate, accept)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn wait_where(
        &mut self,
        chat: Chat,
        history_after: i32,
        mark: usize,
        deadline: Instant,
        history_first: bool,
        candidate: impl Fn(&RawMessage) -> bool,
        mut accept: impl FnMut(&RawMessage) -> bool,
    ) -> Option<RawMessage> {
        if history_first {
            for m in self.history_after(chat, history_after).await {
                if candidate(&m) && accept(&m) {
                    return Some(self.accepted(m).await);
                }
            }
        }
        let mut pos = mark.min(self.seen.len());
        loop {
            while pos < self.seen.len() {
                let m = self.seen[pos].clone();
                pos += 1;
                if candidate(&m) && accept(&m) {
                    return Some(self.accepted(m).await);
                }
            }
            if !self.recv_until(deadline).await {
                break;
            }
        }
        for m in self.history_after(chat, history_after).await {
            if candidate(&m) && accept(&m) {
                return Some(self.accepted(m).await);
            }
        }
        None
    }

    async fn accepted(&self, m: RawMessage) -> RawMessage {
        self.record(&m, true).await;
        m
    }
}
