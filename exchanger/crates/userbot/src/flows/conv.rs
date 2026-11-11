//! Разговор одной операции с ботом кошелька.
//!
//! [`Conv`] держит ворота аккаунта на всё время операции и подписан на поток сообщений
//! с самого начала — ответ, пришедший раньше, чем вернулся вызов отправки, не теряется.
//! Всё, что приходит, сначала уходит в журнал, потом разбирается.
//!
//! Ответ на команду — сообщение бота (не наше) в том же чате **в окне команды**: id больше id
//! команды и меньше id нашего следующего сообщения в этом чате. Новое сообщение или правка
//! такого. Окна разных команд не пересекаются, поэтому один ответ не может достаться двум
//! операциям: ни поздний ответ на прошлую команду, ни ответ на следующую.
//!
//! Меню ботов отвечают и правкой своего экрана, который старше последней нашей команды:
//! xRocket на введённую сумму чека правит сообщение меню (оно пришло в ответ на `/cheques`).
//! Для этого [`Conv::wait_screen`]: окно открывает команда, с которой начался экран, а
//! закрывает наше следующее сообщение после последней команды разговора.
//!
//! «Чистый чат»: первая команда операции в чат бота уходит, только если на нашу прошлую
//! команду в этом чате уже есть ответ (по истории Telegram, а не по таймеру в памяти — это
//! переживает рестарт). Иначе ждём `late_reply_quarantine`, затем операция возвращается
//! движку (`BotError::Unavailable`, ничего не выполнялось). Команда активации чека без ответа
//! держит чат, пока ответ не придёт или заявку не разберёт человек
//! ([`AccountGate::acknowledge_unanswered`](super::AccountGate::acknowledge_unanswered)): её
//! поздний ответ несёт сумму, и следующая заявка получила бы чужие деньги (аудит 11.11.2026,
//! Ю-2). Для остальных команд — через `unanswered_block` чат
//! считается свободным. Исключение — повтор той же команды (тот же текст): это повтор
//! операции с тем же `random_id`, её ответ ищется в её же окне.

use tokio::sync::MutexGuard;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::time::Instant;

use std::collections::HashSet;

use parsers::ActivationReply;
use parsers::replies::classify_activation;

use super::UserbotWallet;
use super::gate::{GateState, Pace};
use crate::transport::{CallbackAnswer, Chat, RawMessage, Transport, TransportError};
use crate::wallet::BotError;

/// Команды меню чеков: их экран бот правит на месте, в том числе в ответ на сумму.
const MENU_COMMANDS: [&str; 2] = ["/checks", "/cheques"];

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
    /// `RANDOM_ID_DUPLICATE` на самой первой отправке этого вызова: команду отправил ПРОШЛЫЙ
    /// вызов с тем же `random_id`, то есть операцию уже начинали. Для выдачи чека и оплаты
    /// счёта это значит «ничего не создавать, только сверить».
    pub earlier_call: bool,
}

/// Команда, ответ на которую несёт деньги: `/start <код чека>`. Её поздний ответ нельзя отдать
/// следующей заявке, поэтому по времени чат после неё не освобождается.
fn carries_money(platform: domain::Platform, text: &str) -> bool {
    text.strip_prefix("/start ").is_some_and(|param| {
        parsers::links::classify_start_param(platform, param)
            == Some(parsers::links::LinkKind::Check)
    })
}

pub(crate) struct Conv<'a, T: Transport> {
    w: &'a UserbotWallet<T>,
    gate: MutexGuard<'a, GateState>,
    rx: Receiver<RawMessage>,
    /// Всё, что пришло в поток за время операции, в порядке прихода (версии правок — отдельно).
    seen: Vec<RawMessage>,
    op: Option<String>,
    /// Чаты, проверенные на «чистоту» в этой операции.
    cleared: HashSet<Chat>,
    /// Чаты, открытые решением человека при неотвеченной активации: её поздний ответ ещё
    /// может прийти.
    after_ack: HashSet<Chat>,
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
            cleared: HashSet::new(),
            after_ack: HashSet::new(),
        }
    }

    pub fn platform(&self) -> domain::Platform {
        self.w.platform
    }

    pub fn deadline(&self) -> Instant {
        Instant::now() + self.w.cfg.reply_timeout
    }

    /// До какого момента ждать поздний ответ на прошлую команду.
    pub fn late_deadline(&self) -> Instant {
        Instant::now() + self.w.cfg.late_reply_quarantine
    }

    /// Чат открыт решением человека, хотя прошлая активация чека осталась без ответа.
    pub fn after_ack(&self, chat: Chat) -> bool {
        self.after_ack.contains(&chat)
    }

    /// Команда `cmd_id` в `chat` осталась без ответа: ворота запоминают её, чтобы человек мог
    /// признать её разобранной.
    pub fn unanswered(&mut self, chat: Chat, cmd_id: i32) {
        self.gate.unanswered_since(chat, cmd_id);
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
            TransportError::NotAuthorized => BotError::NotAuthorized,
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

    /// Сообщение бота, которое отвечает на команду (не уведомление, не перевод, не «обработка»).
    fn answers(platform: domain::Platform, m: &RawMessage) -> bool {
        !m.out
            && !matches!(
                classify_activation(platform, &m.text),
                ActivationReply::ClaimNotice
                    | ActivationReply::IncomingTransfer { .. }
                    | ActivationReply::IncomingTransferOther
                    | ActivationReply::InProgress
            )
    }

    /// Проверка «чистого чата» перед первой командой операции в чат бота. `next_text` —
    /// текст, который собираемся отправить: если прошлая неотвеченная команда — та же самая,
    /// это повтор операции, и отправка разрешена.
    pub async fn ensure_clear(
        &mut self,
        chat: Chat,
        next_text: Option<&str>,
    ) -> Result<(), BotError> {
        let Chat::WalletBot(platform) = chat else {
            return Ok(());
        };
        if self.cleared.contains(&chat) {
            return Ok(());
        }
        self.wait_quarantine(chat).await;
        loop {
            let msgs = self
                .recent(chat)
                .await
                .map_err(|e| BotError::Unavailable(format!("history: {e}")))?;
            let Some(i) = msgs.iter().rposition(|m| m.out) else {
                break;
            };
            let last = &msgs[i];
            // Ответ — новое сообщение бота после команды или (только в меню чеков) правка
            // экрана меню, пришедшего на нашу предыдущую команду `/checks` / `/cheques`,
            // сделанная не раньше этой: xRocket отвечает на сумму чека правкой меню.
            let menu_cmd = msgs[..i]
                .iter()
                .rposition(|m| m.out)
                .map(|j| &msgs[j])
                .filter(|p| MENU_COMMANDS.contains(&p.text.as_str()))
                .map(|p| p.id);
            let edited_screen = |m: &RawMessage| {
                !m.out
                    && m.id < last.id
                    && menu_cmd.is_some_and(|p| m.id > p)
                    && m.edit_date.is_some_and(|d| d >= last.date)
                    && Self::answers(platform, m)
            };
            let answered = msgs[i + 1..].iter().any(|m| Self::answers(platform, m))
                || msgs[..i].iter().any(edited_screen);
            if answered || next_text == Some(last.text.as_str()) {
                break;
            }
            if self.gate.acknowledged(chat, last.id) {
                self.after_ack.insert(chat);
                break;
            }
            let since = self.gate.unanswered_since(chat, last.id);
            let waited = since.elapsed();
            let block = self.w.cfg.unanswered_block;
            if waited >= block && carries_money(platform, &last.text) {
                tracing::error!(
                    account = self.w.gate.label(),
                    ?chat,
                    command = last.id,
                    "check activation has no reply: the chat stays blocked until a human clears it"
                );
                return Err(BotError::ChatBlocked);
            }
            if waited >= block {
                tracing::warn!(
                    account = self.w.gate.label(),
                    ?chat,
                    command = last.id,
                    "previous command has no reply for too long; the chat is used again"
                );
                break;
            }
            let wait = self.w.cfg.late_reply_quarantine.min(block - waited);
            let deadline = Instant::now() + wait;
            let mut pos = self.mark().await;
            let mut got = false;
            'wait: loop {
                while pos < self.seen.len() {
                    let m = &self.seen[pos];
                    pos += 1;
                    if m.chat == chat && m.id > last.id && Self::answers(platform, m) {
                        got = true;
                        break 'wait;
                    }
                }
                if !self.recv_until(deadline).await {
                    break;
                }
            }
            if !got {
                tracing::info!(
                    account = self.w.gate.label(),
                    ?chat,
                    command = last.id,
                    "previous command still has no reply"
                );
                return Err(BotError::Unavailable(
                    "previous command in this chat has no reply yet".to_owned(),
                ));
            }
        }
        self.gate.clear_unanswered(chat);
        self.cleared.insert(chat);
        Ok(())
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
        self.ensure_clear(chat, Some(text))
            .await
            .map_err(SendError::NotSent)?;
        self.pace(kind).await.map_err(SendError::NotSent)?;
        let first = self.w.transport.send_text(chat, text, random_id).await;
        let earlier_call = matches!(first, Err(TransportError::RandomIdDuplicate));
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
                    earlier_call: false,
                })
            }
            Err(TransportError::RandomIdDuplicate) => {
                let mut sent = self.locate(chat, text).await?;
                sent.earlier_call = earlier_call;
                Ok(sent)
            }
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
                Ok(Sent {
                    msg,
                    located: true,
                    earlier_call: false,
                })
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
            after_id,
            mark,
            deadline,
            history_first,
            candidate,
            accept,
        )
        .await
    }

    /// Ждать экран меню: новое сообщение бота или правку его экрана в окне от команды
    /// `opened_by` (с неё начался экран, например `/cheques`) до нашего следующего сообщения
    /// после `last_out` (последнее наше сообщение разговора, например сумма чека).
    /// Принимаются только сообщения, пришедшие после отметки `mark`, и — если не дождались —
    /// текущие версии по истории.
    #[allow(clippy::too_many_arguments)]
    pub async fn wait_screen(
        &mut self,
        chat: Chat,
        opened_by: i32,
        last_out: i32,
        mark: usize,
        deadline: Instant,
        history_first: bool,
        accept: impl FnMut(&RawMessage) -> bool,
    ) -> Option<RawMessage> {
        let candidate = move |m: &RawMessage| m.chat == chat && !m.out && m.id > opened_by;
        self.wait_where(
            chat,
            opened_by,
            last_out,
            mark,
            deadline,
            history_first,
            candidate,
            accept,
        )
        .await
    }

    /// Окно команды: сообщения после нашего следующего сообщения в этом чате (первого нашего
    /// с id больше `bound_after`) не рассматриваются — ответы на следующую команду и поздние
    /// ответы не наши. История читается с `history_after`.
    #[allow(clippy::too_many_arguments)]
    async fn wait_where(
        &mut self,
        chat: Chat,
        history_after: i32,
        bound_after: i32,
        mark: usize,
        deadline: Instant,
        history_first: bool,
        candidate: impl Fn(&RawMessage) -> bool,
        mut accept: impl FnMut(&RawMessage) -> bool,
    ) -> Option<RawMessage> {
        let next_out = |msgs: &[RawMessage]| {
            msgs.iter()
                .filter(|m| m.chat == chat && m.out && m.id > bound_after)
                .map(|m| m.id)
                .min()
        };
        if history_first {
            let msgs = self.history_after(chat, history_after).await;
            let upper = next_out(&msgs);
            for m in msgs {
                if upper.is_none_or(|u| m.id < u) && candidate(&m) && accept(&m) {
                    return Some(self.accepted(m).await);
                }
            }
        }
        let mut upper: Option<i32> = None;
        let mut pos = mark.min(self.seen.len());
        loop {
            while pos < self.seen.len() {
                let m = self.seen[pos].clone();
                pos += 1;
                if m.chat == chat && m.out && m.id > bound_after {
                    upper = Some(upper.map_or(m.id, |u| u.min(m.id)));
                    continue;
                }
                if upper.is_none_or(|u| m.id < u) && candidate(&m) && accept(&m) {
                    return Some(self.accepted(m).await);
                }
            }
            if !self.recv_until(deadline).await {
                break;
            }
        }
        let msgs = self.history_after(chat, history_after).await;
        let upper = match (upper, next_out(&msgs)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        for m in msgs {
            if upper.is_none_or(|u| m.id < u) && candidate(&m) && accept(&m) {
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
