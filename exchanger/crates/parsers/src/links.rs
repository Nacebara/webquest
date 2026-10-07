//! Ссылки на чеки и счета (SPEC §1.2; форматы — lovec `bots.rs`, `links.rs`).
//!
//! Коды: CryptoBot чек `CQ` + 10 = 12 символов, счёт `IV…`; xRocket чек `t_` + 15 = 17,
//! мульти-чек `mci_` + 15 = 19 и `mc_` + 15 = 18, счёт `inv_…`. Боты: `send` = `CryptoBot`,
//! `xrocket` = `tonRocketBot`; тестнет `CryptoTestnetBot`, `xrocket_testnet_bot` — не принимаем.
//! Формы: `https://t.me/<bot>?start=<p>`, без схемы, `telegram.me`, `telegram.dog`, `www.`,
//! `https://<bot>.t.me/?start=`, `t.me/<bot>/app?startapp=<p>`, `tg://resolve?domain=<bot>&start=<p>`
//! (и с `start` раньше `domain`), `?ref=…&start=<p>`. Имя бота — без учёта регистра, только ASCII.

use domain::Platform;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LinkKind {
    Check,
    Invoice,
}

/// Распознанная ссылка на чек или счёт официального бота кошелька.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WalletLink {
    pub platform: Platform,
    pub kind: LinkKind,
    /// Параметр `start`: `CQxxxxxxxxxx`, `t_…`, `mci_…`, `mc_…`, `IV…`, `inv_…`.
    pub param: String,
}

impl WalletLink {
    /// Каноническая ссылка, которую можно показать клиенту.
    pub fn canonical_url(&self) -> String {
        let bot = match self.platform {
            Platform::CryptoBot => "send",
            Platform::XRocket => "xrocket",
        };
        format!("https://t.me/{bot}?start={}", self.param)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("not a link to a wallet bot")]
    NotWalletLink,
    /// Ссылка на похожего, но не официального бота (фишинг, омоглифы).
    #[error("link points to a non-official bot: {0}")]
    ForeignBot(String),
    #[error("testnet links are not accepted")]
    Testnet,
    /// Ссылка на бота кошелька, но не чек и не счёт (реферальная, меню и т. п.).
    #[error("wallet bot link is neither a check nor an invoice")]
    NotCheckOrInvoice,
}

/// Разобрать одну ссылку. Реализация — в вехе parsers (SPEC §1.2).
pub fn parse_url(url: &str) -> Result<WalletLink, LinkError> {
    let _ = url;
    Err(LinkError::NotWalletLink)
}

/// Голый код без ссылки (`CQ…`, `t_…`, `mci_…`, `mc_…`): клиент мог прислать только его.
pub fn parse_bare_code(text: &str) -> Option<WalletLink> {
    let _ = text;
    None
}

/// Все ссылки из сообщения клиента в порядке появления, без повторов: текст, URL из сущностей
/// `text_link` и URL кнопок (пересланный чек несёт ссылку в кнопке).
pub fn extract_from_message(
    text: &str,
    entity_urls: &[String],
    button_urls: &[String],
) -> Vec<Result<WalletLink, LinkError>> {
    let _ = (text, entity_urls, button_urls);
    Vec::new()
}
