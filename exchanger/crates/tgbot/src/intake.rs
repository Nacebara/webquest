//! Приём ссылок (HANDOVER §1.2, шаги 1–3): что нашли в сообщении → лимиты → движок → ответ.
//!
//! - первая корректная ссылка на **чек** уходит в `Engine::submit_check`; принято — одно
//!   сообщение заявки, отказ — экран ошибки (где деньги и что делать);
//! - ссылка на **счёт** уходит в `Engine::submit_invoice`: котировка — одно сообщение заявки
//!   (дальше клиент присылает чек, и он идёт в эту заявку), отказ — экран ошибки;
//! - чужой бот, тестнет, ссылка не на чек — экран ошибки, движок не вызывается.

use std::sync::Arc;
use std::time::Instant;

use engine::{InvoiceIssue, InvoiceReject, InvoiceSubmitted, SubmitReject, Submitted};
use parsers::{LinkError, LinkKind, WalletLink};
use ui::UiError;
use ui::{InvoiceProblem, InvoiceRefusal};

use crate::limits::wait_secs;
use crate::orders::{Create, ensure_message};
use crate::{Inner, Notice, TgBotError};

/// Направление обмена по платформе чека (как у движка: CryptoBot → xRocket и наоборот).
fn check_direction(platform: domain::Platform) -> domain::Direction {
    match platform {
        domain::Platform::CryptoBot => domain::Direction::CbToXrCheck,
        domain::Platform::XRocket => domain::Direction::XrToCbCheck,
    }
}

/// Ошибка ссылки → экран.
fn link_error(e: &LinkError) -> UiError {
    match e {
        LinkError::ForeignBot(bot) => UiError::ForeignBot { bot: bot.clone() },
        LinkError::Testnet => UiError::Testnet,
        LinkError::NotCheckOrInvoice | LinkError::NotWalletLink => UiError::NotCheckOrInvoice,
        LinkError::MultiCheck => UiError::MultiCheck,
    }
}

/// Сообщение клиента со ссылками (`found` не пуст).
pub(crate) async fn process(
    inner: &Arc<Inner>,
    chat: i64,
    user: i64,
    found: &[Result<WalletLink, LinkError>],
) -> Result<(), TgBotError> {
    let limits = inner.rate_limits().await;
    let now = Instant::now();
    if let Err(wait) = inner.limiter.hit_link(user, limits.links_per_min, now) {
        return rate_limited(inner, chat, user, wait).await;
    }
    let maintenance = {
        let mut conn = inner.pool.acquire().await?;
        storage::settings::flag_on(&mut conn, "maintenance").await?
    };
    if maintenance {
        return inner
            .reply_error(chat, &UiError::Maintenance { until: None })
            .await;
    }

    let check = found
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .find(|l| l.kind == LinkKind::Check);
    if let Some(link) = check {
        if let Err(wait) = inner
            .limiter
            .order_allowed(user, limits.orders_per_10min, now)
        {
            return rate_limited(inner, chat, user, wait).await;
        }
        return submit(inner, chat, user, link).await;
    }
    let invoice = found
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .find(|l| l.kind == LinkKind::Invoice);
    if let Some(link) = invoice {
        if let Err(wait) = inner
            .limiter
            .order_allowed(user, limits.orders_per_10min, now)
        {
            return rate_limited(inner, chat, user, wait).await;
        }
        return submit_invoice(inner, chat, user, link).await;
    }
    let error = found
        .iter()
        .find_map(|r| r.as_ref().err())
        .map_or(UiError::NotCheckOrInvoice, link_error);
    inner.reply_error(chat, &error).await
}

/// Отказ по счёту → экран. Денег клиента у нас нет.
fn invoice_error(reason: &InvoiceReject) -> UiError {
    let usdt = |x: domain::Decimal| {
        domain::Money::new(x, domain::Asset::Usdt)
            .unwrap_or_else(|_| domain::Money::zero(domain::Asset::Usdt))
    };
    let order = |o: &str| {
        if o.is_empty() {
            "—".to_owned()
        } else {
            o.to_owned()
        }
    };
    match reason {
        InvoiceReject::NotAnInvoice => UiError::NotCheckOrInvoice,
        InvoiceReject::AlreadyInWork { order: o } => UiError::InvoiceInWork { order: order(o) },
        InvoiceReject::AlreadyPaid { .. } => {
            UiError::InvoiceUnsuitable(InvoiceProblem::AlreadyPaid)
        }
        InvoiceReject::PendingInvoice { order: o } => UiError::InvoicePending { order: order(o) },
        InvoiceReject::DirectionClosed => UiError::InvoiceRefused(InvoiceRefusal::Paused),
        InvoiceReject::ServicePaused => UiError::InvoiceRefused(InvoiceRefusal::Maintenance),
        InvoiceReject::NoReserve { max } => {
            UiError::InvoiceRefused(InvoiceRefusal::NoReserve { max: usdt(*max) })
        }
        InvoiceReject::DailyLimit => UiError::InvoiceRefused(InvoiceRefusal::DailyLimit),
        InvoiceReject::BelowMinimum { min } => {
            UiError::InvoiceRefused(InvoiceRefusal::BelowMinimum { min: usdt(*min) })
        }
        InvoiceReject::AboveMaximum { max } => {
            UiError::InvoiceRefused(InvoiceRefusal::AboveMaximum { max: usdt(*max) })
        }
        InvoiceReject::Unsuitable(issue) => UiError::InvoiceUnsuitable(match issue {
            InvoiceIssue::Reusable => InvoiceProblem::Reusable,
            InvoiceIssue::AlreadyPaid => InvoiceProblem::AlreadyPaid,
            InvoiceIssue::Expired => InvoiceProblem::Expired,
            InvoiceIssue::NoAmount => InvoiceProblem::NoAmount,
            InvoiceIssue::UnsupportedAsset => InvoiceProblem::UnsupportedAsset,
            InvoiceIssue::Fiat => InvoiceProblem::Fiat,
            InvoiceIssue::Unreadable => InvoiceProblem::Unreadable,
        }),
        InvoiceReject::WalletUnavailable => UiError::InvoiceUnsuitable(InvoiceProblem::Unavailable),
    }
}

/// Ссылка на чужой счёт: юзербот открывает его (несколько секунд), котировка — сообщение
/// заявки.
async fn submit_invoice(
    inner: &Arc<Inner>,
    chat: i64,
    user: i64,
    link: &WalletLink,
) -> Result<(), TgBotError> {
    // Каждая ссылка на счёт — действие юзербота (он открывает счёт): считается в лимите заявок
    // и при отказе, иначе потоком неподходящих счетов можно занять аккаунт кошелька.
    inner.limiter.record_order(user, Instant::now());
    let submitted = match inner.engine.submit_invoice(user, link).await {
        Ok(submitted) => submitted,
        Err(e) => {
            tracing::error!(user, error = %e, "invoice submission failed");
            return inner
                .reply_error(
                    chat,
                    &UiError::InvoiceUnsuitable(InvoiceProblem::Unavailable),
                )
                .await;
        }
    };
    match submitted {
        InvoiceSubmitted::Quoted(order) => {
            tracing::info!(
                order = order.id,
                direction = order.direction.code(),
                "invoice quoted"
            );
            if let Err(e) = ensure_message(inner, order.id, Create::Always).await {
                tracing::warn!(order = order.id, error = %e, "order message not sent yet");
            }
            Ok(())
        }
        InvoiceSubmitted::Rejected(reason) => {
            tracing::info!(user, reason = reason.code(), "invoice rejected");
            inner.reply_error(chat, &invoice_error(&reason)).await
        }
    }
}

async fn rate_limited(
    inner: &Arc<Inner>,
    chat: i64,
    user: i64,
    wait: std::time::Duration,
) -> Result<(), TgBotError> {
    tracing::info!(user, wait_secs = wait.as_secs(), "client rate limited");
    if !inner.should_notify(user, Notice::RateLimited) {
        return Ok(());
    }
    inner
        .reply_error(
            chat,
            &UiError::RateLimited {
                retry_after_secs: wait_secs(wait),
            },
        )
        .await
}

async fn submit(
    inner: &Arc<Inner>,
    chat: i64,
    user: i64,
    link: &WalletLink,
) -> Result<(), TgBotError> {
    let submitted = match inner.engine.submit_check(user, link).await {
        Ok(submitted) => submitted,
        Err(e) => {
            // Транзакция приёма откатилась: чек не открывали, деньги в нём.
            tracing::error!(user, error = %e, "check submission failed");
            inner.reply_error(chat, &UiError::Internal).await?;
            return Ok(());
        }
    };
    match submitted {
        Submitted::Accepted(order) => {
            inner.limiter.record_order(user, Instant::now());
            tracing::info!(
                order = order.id,
                direction = order.direction.code(),
                "check accepted"
            );
            // Не вышло отправить сейчас — дошлёт подписка на движок или фоновая досылка.
            if let Err(e) = ensure_message(inner, order.id, Create::Always).await {
                tracing::warn!(order = order.id, error = %e, "order message not sent yet");
            }
            Ok(())
        }
        Submitted::Rejected {
            reason: SubmitReject::AlreadyInWork,
            ..
        } => {
            let live = {
                let mut conn = inner.pool.acquire().await?;
                storage::client::live_order_for_check(&mut conn, link.platform, &link.param).await?
            };
            // Заявка успела закончиться между проверками — номер неизвестен.
            let order = live.map_or_else(|| "—".to_owned(), |o| o.public_id);
            inner.reply_error(chat, &UiError::Duplicate { order }).await
        }
        Submitted::Rejected {
            reason,
            order: Some(order),
        } => {
            tracing::info!(
                order = order.id,
                reason = reason.code(),
                "check rejected at intake"
            );
            ensure_message(inner, order.id, Create::Always).await
        }
        Submitted::Rejected {
            reason,
            order: None,
        } => {
            let error = match reason {
                SubmitReject::DirectionClosed => UiError::DirectionPaused {
                    direction: check_direction(link.platform),
                    until: None,
                },
                SubmitReject::NoReserve => UiError::NoReserve {
                    max: domain::Money::zero(domain::Asset::Usdt),
                },
                SubmitReject::ServicePaused => UiError::Maintenance { until: None },
                SubmitReject::DailyLimit => UiError::DailyLimit,
                SubmitReject::QuoteExpired => UiError::QuoteExpired,
                SubmitReject::InvoiceBusy => UiError::InvoiceCheckBusy,
                SubmitReject::MultiCheck => UiError::MultiCheck,
                SubmitReject::CheckUsed => UiError::CheckUsed,
                SubmitReject::NotACheck | SubmitReject::AlreadyInWork => UiError::NotCheckOrInvoice,
            };
            inner.reply_error(chat, &error).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_errors_map_to_screens() {
        assert_eq!(
            link_error(&LinkError::ForeignBot("CryptoBotX".into())),
            UiError::ForeignBot {
                bot: "CryptoBotX".into()
            }
        );
        assert_eq!(link_error(&LinkError::Testnet), UiError::Testnet);
        assert_eq!(
            link_error(&LinkError::NotCheckOrInvoice),
            UiError::NotCheckOrInvoice
        );
    }
}
