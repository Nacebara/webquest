//! [`SimTransport`] — [`Transport`] одного аккаунта поверх общего [`SimWorld`].

use async_trait::async_trait;
use domain::Platform;
use tokio::sync::broadcast;

use super::SimWorld;
use super::bots;
use super::model::{AccountId, Call, Fault};
use super::state::Draft;
use crate::transport::{
    CallbackAnswer, Chat, InlineResults, RawMessage, Transport, TransportError,
};

/// Транспорт одного аккаунта Telegram в мире симулятора.
#[derive(Clone)]
pub struct SimTransport {
    world: SimWorld,
    account: AccountId,
    tx: broadcast::Sender<RawMessage>,
}

impl std::fmt::Debug for SimTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimTransport")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl SimTransport {
    pub(crate) fn new(
        world: SimWorld,
        account: AccountId,
        tx: Option<broadcast::Sender<RawMessage>>,
    ) -> Self {
        // Неизвестный аккаунт: поток-пустышка, вызовы — `NotAuthorized`.
        let tx = tx.unwrap_or_else(|| broadcast::channel(1).0);
        Self { world, account, tx }
    }

    pub fn account(&self) -> AccountId {
        self.account
    }

    pub fn world(&self) -> &SimWorld {
        &self.world
    }
}

/// Сбой, при котором запрос не дошёл до сервера: ничего не сделано.
fn before_effect(fault: Option<&Fault>) -> Option<TransportError> {
    match fault? {
        Fault::TimeoutBeforeEffect => Some(TransportError::Timeout),
        Fault::Disconnected => Some(TransportError::Disconnected),
        Fault::FloodWait(d) => Some(TransportError::FloodWait(*d)),
        Fault::Rpc { code, name } => Some(TransportError::Rpc {
            code: *code,
            name: name.clone(),
        }),
        _ => None,
    }
}

/// Сбой, при котором запрос выполнен, но ответ потерян.
fn after_effect(fault: Option<&Fault>) -> Option<TransportError> {
    match fault? {
        Fault::TimeoutAfterEffect => Some(TransportError::Timeout),
        Fault::DisconnectedAfterEffect => Some(TransportError::Disconnected),
        _ => None,
    }
}

fn finish<T>(value: T, fault: Option<&Fault>) -> Result<T, TransportError> {
    after_effect(fault).map_or(Ok(value), Err)
}

#[async_trait]
impl Transport for SimTransport {
    async fn send_text(
        &self,
        chat: Chat,
        text: &str,
        random_id: i64,
    ) -> Result<RawMessage, TransportError> {
        let acc = self.account;
        let (result, delayed) = {
            let mut st = self.world.lock();
            if !st.accounts.contains_key(&acc) {
                return Err(TransportError::NotAuthorized);
            }
            st.tick();
            let fault = st.take_fault(acc, Call::SendText);
            if let Some(e) = before_effect(fault.as_ref()) {
                return Err(e);
            }
            let fresh = st
                .account_mut(acc)
                .map_err(|_| TransportError::NotAuthorized)?
                .random_ids
                .insert(random_id);
            if !fresh {
                return Err(TransportError::RandomIdDuplicate);
            }
            let sent = st
                .store_outgoing(acc, chat, Draft::text(text))
                .ok_or(TransportError::NotAuthorized)?;
            let outs = match chat {
                Chat::WalletBot(platform) => bots::on_text(&mut st, acc, platform, text, random_id),
                Chat::SavedMessages => Vec::new(),
            };
            let delayed = st.deliver((acc, chat), outs, fault.as_ref());
            (finish(sent, fault.as_ref()), delayed)
        };
        self.world.spawn_delayed(delayed);
        result
    }

    async fn press(
        &self,
        chat: Chat,
        msg_id: i32,
        data: &[u8],
    ) -> Result<CallbackAnswer, TransportError> {
        let acc = self.account;
        let (result, delayed) = {
            let mut st = self.world.lock();
            if !st.accounts.contains_key(&acc) {
                return Err(TransportError::NotAuthorized);
            }
            st.tick();
            let fault = st.take_fault(acc, Call::Press);
            if let Some(e) = before_effect(fault.as_ref()) {
                return Err(e);
            }
            let (answer, outs) = bots::on_press(&mut st, acc, chat, msg_id, data)?;
            let delayed = st.deliver((acc, chat), outs, fault.as_ref());
            (finish(answer, fault.as_ref()), delayed)
        };
        self.world.spawn_delayed(delayed);
        result
    }

    async fn inline_query(
        &self,
        bot: Platform,
        query: &str,
    ) -> Result<InlineResults, TransportError> {
        let acc = self.account;
        let mut st = self.world.lock();
        if !st.accounts.contains_key(&acc) {
            return Err(TransportError::NotAuthorized);
        }
        st.tick();
        let fault = st.take_fault(acc, Call::InlineQuery);
        if let Some(e) = before_effect(fault.as_ref()) {
            return Err(e);
        }
        let results = bots::on_inline_query(&mut st, acc, bot, query);
        finish(results, fault.as_ref())
    }

    async fn send_inline(
        &self,
        to: Chat,
        bot: Platform,
        results: &InlineResults,
        result_id: &str,
        random_id: i64,
    ) -> Result<RawMessage, TransportError> {
        let acc = self.account;
        let mut st = self.world.lock();
        if !st.accounts.contains_key(&acc) {
            return Err(TransportError::NotAuthorized);
        }
        st.tick();
        let fault = st.take_fault(acc, Call::SendInline);
        if let Some(e) = before_effect(fault.as_ref()) {
            return Err(e);
        }
        let duplicate = st
            .account(acc)
            .map_err(|_| TransportError::NotAuthorized)?
            .random_ids
            .contains(&random_id);
        if duplicate {
            return Err(TransportError::RandomIdDuplicate);
        }
        // Ошибка разбора запроса (`QUERY_ID_INVALID`, …) не занимает `random_id`.
        let draft =
            bots::on_send_inline(&mut st, acc, bot, results.query_id, result_id, random_id)?;
        st.account_mut(acc)
            .map_err(|_| TransportError::NotAuthorized)?
            .random_ids
            .insert(random_id);
        let sent = st
            .store_outgoing(acc, to, draft)
            .ok_or(TransportError::NotAuthorized)?;
        finish(sent, fault.as_ref())
    }

    async fn open_webapp(&self, bot: Platform, button_url: &str) -> Result<String, TransportError> {
        let acc = self.account;
        let mut st = self.world.lock();
        if !st.accounts.contains_key(&acc) {
            return Err(TransportError::NotAuthorized);
        }
        st.tick();
        let fault = st.take_fault(acc, Call::OpenWebApp);
        if let Some(e) = before_effect(fault.as_ref()) {
            return Err(e);
        }
        let url = bots::open_webapp(&mut st, acc, bot, button_url)?;
        finish(url, fault.as_ref())
    }

    async fn history(
        &self,
        chat: Chat,
        after_id: i32,
        limit: u32,
    ) -> Result<Vec<RawMessage>, TransportError> {
        let acc = self.account;
        let mut st = self.world.lock();
        let account = st.account(acc).map_err(|_| TransportError::NotAuthorized)?;
        let messages: Vec<RawMessage> = account
            .chats
            .get(&chat)
            .map(|msgs| {
                msgs.iter()
                    .filter(|m| m.id > after_id)
                    .take(usize::try_from(limit).unwrap_or(usize::MAX))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let fault = st.take_fault(acc, Call::History);
        if let Some(e) = before_effect(fault.as_ref()).or_else(|| after_effect(fault.as_ref())) {
            return Err(e);
        }
        Ok(messages)
    }

    async fn recent(&self, chat: Chat, limit: u32) -> Result<Vec<RawMessage>, TransportError> {
        let acc = self.account;
        let mut st = self.world.lock();
        let account = st.account(acc).map_err(|_| TransportError::NotAuthorized)?;
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let messages: Vec<RawMessage> = account
            .chats
            .get(&chat)
            .map(|msgs| msgs[msgs.len().saturating_sub(limit)..].to_vec())
            .unwrap_or_default();
        let fault = st.take_fault(acc, Call::History);
        if let Some(e) = before_effect(fault.as_ref()).or_else(|| after_effect(fault.as_ref())) {
            return Err(e);
        }
        Ok(messages)
    }

    fn subscribe(&self) -> broadcast::Receiver<RawMessage> {
        self.tx.subscribe()
    }
}
