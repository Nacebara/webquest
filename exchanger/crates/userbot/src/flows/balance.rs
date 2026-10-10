//! Балансы кошелька: экран `/wallet` бота (сверка с леджером, SPEC §3).

use domain::Money;
use parsers::replies::parse_balance;

use super::conv::{Conv, SendError};
use super::gate::Pace;
use super::{UserbotWallet, rid};
use crate::transport::{Chat, Transport};
use crate::wallet::BotError;

pub(crate) async fn run<T: Transport>(w: &UserbotWallet<T>) -> Result<Vec<Money>, BotError> {
    let platform = w.platform;
    let chat = Chat::WalletBot(platform);
    let mut conv = Conv::open(w, None).await;
    conv.wait_quarantine(chat).await;
    let mark = conv.mark().await;
    let sent = match conv
        .send(chat, "/wallet", rid::new_random_id(), Pace::Bot)
        .await
    {
        Ok(sent) => sent,
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            conv.quarantine(chat);
            return Err(BotError::Unavailable(format!("/wallet: {detail}")));
        }
    };
    let deadline = conv.deadline();
    let screen = conv
        .wait(chat, sent.msg.id, mark, deadline, sent.located, |m| {
            !parse_balance(platform, &m.text).is_empty()
        })
        .await;
    let Some(screen) = screen else {
        conv.quarantine(chat);
        return Err(BotError::Unavailable(
            "the bot did not show the wallet".to_owned(),
        ));
    };
    parse_balance(platform, &screen.text)
        .into_iter()
        .map(|line| {
            Money::new(line.amount, line.asset)
                .map_err(|e| BotError::Unavailable(format!("wallet screen: {e}")))
        })
        .collect()
}
