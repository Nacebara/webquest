//! Переход заявки и его эффекты — в одной транзакции (CLAUDE.md п. 3–4, 7).
//!
//! `domain::fsm` говорит, что сделать (удержать резерв, провести, создать операцию), а суммы
//! берутся из заявки после патча этого же перехода.

use domain::fsm::{Effect, Event, HoldAction, MoneyRole, OpKind, PostingKind};
use domain::{Asset, Decimal, ledger};
use sqlx::PgConnection;
use storage::ledger::PostingRefs;
use storage::operations::NewOperation;
use storage::orders::{Actor, OrderDetails, OrderPatch, OrderRow};

use crate::EngineError;

/// Что нужно эффектам, кроме данных заявки.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EffectCtx {
    /// Операция, итог которой вызвал переход: проводка привязывается к ней
    /// (`ledger_tx_once_per_operation` — повторная обработка не задвоит леджер).
    pub cause_op: Option<i64>,
    /// `Hold(Place)`: кошелёк и сумма удержания.
    pub hold: Option<(i16, Decimal)>,
    /// Актив, в котором реально пришли деньги (если не совпал с активом заявки).
    pub intake_asset: Option<Asset>,
}

fn need(value: Option<Decimal>, what: &str, order: &OrderRow) -> Result<Decimal, EngineError> {
    value.ok_or_else(|| EngineError::Invariant(format!("order {}: no {what}", order.id)))
}

/// Применить событие и все эффекты перехода. Возвращает заявку после перехода и id новых
/// операций (их исполнит воркер после коммита).
pub(crate) async fn transit(
    conn: &mut PgConnection,
    order: &OrderRow,
    event: Event,
    patch: &OrderPatch<'_>,
    reason: Option<&str>,
    ctx: EffectCtx,
) -> Result<(OrderRow, Vec<i64>), EngineError> {
    let (updated, transition) =
        storage::orders::apply_event(&mut *conn, order, event, patch, Actor::System, reason)
            .await?;
    let details = storage::orders::details(&mut *conn, order.id).await?;
    let mut ops = Vec::new();
    for effect in transition.effects {
        match effect {
            Effect::Hold(HoldAction::Place) => {
                let (wallet, amount) = ctx.hold.ok_or_else(|| {
                    EngineError::Invariant(format!("order {}: hold amount not given", order.id))
                })?;
                storage::holds::lock_wallet(&mut *conn, wallet).await?;
                storage::holds::place(&mut *conn, order.id, wallet, order.asset, amount).await?;
            }
            Effect::Hold(HoldAction::ShrinkToPayout) => {
                let payout = need(details.payout_amount, "payout amount", order)?;
                storage::holds::shrink(&mut *conn, order.id, payout).await?;
            }
            Effect::Hold(HoldAction::Release) => {
                storage::holds::close(&mut *conn, order.id, false).await?;
            }
            Effect::Hold(HoldAction::Consume) => {
                storage::holds::close(&mut *conn, order.id, true).await?;
            }
            Effect::Post(kind) => post(&mut *conn, &updated, &details, kind, ctx).await?,
            Effect::Operation { kind, role } => {
                ops.push(create_op(&mut *conn, &updated, &details, kind, role).await?);
            }
        }
    }
    Ok((updated, ops))
}

async fn post(
    conn: &mut PgConnection,
    order: &OrderRow,
    d: &OrderDetails,
    kind: PostingKind,
    ctx: EffectCtx,
) -> Result<(), EngineError> {
    let source = order.direction.source();
    let target = order.direction.target();
    let asset = order.asset;
    let posting = match kind {
        PostingKind::Intake => ledger::intake(
            source,
            ctx.intake_asset.unwrap_or(asset),
            need(d.intake_amount, "intake amount", order)?,
        )?,
        PostingKind::Payout => ledger::payout(
            target,
            asset,
            need(d.intake_amount, "intake amount", order)?,
            need(d.payout_amount, "payout amount", order)?,
            need(d.fee_amount, "fee amount", order)?,
        )?,
        PostingKind::Refund => ledger::refund(
            source,
            asset,
            need(d.refund_amount, "refund amount", order)?,
        )?,
        PostingKind::InvoicePaid => ledger::invoice_paid(
            target,
            asset,
            need(d.intake_amount, "intake amount", order)?,
            need(d.invoice_amount, "invoice amount", order)?,
        )?,
        PostingKind::ManualClose => {
            return Err(EngineError::Invariant(format!(
                "order {}: manual close posting is done by the operator tool",
                order.id
            )));
        }
    };
    storage::ledger::post(
        conn,
        &posting,
        PostingRefs {
            order_id: Some(order.id),
            operation_id: ctx.cause_op,
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

async fn create_op(
    conn: &mut PgConnection,
    order: &OrderRow,
    d: &OrderDetails,
    kind: OpKind,
    role: MoneyRole,
) -> Result<i64, EngineError> {
    let source = order.direction.source();
    let target = order.direction.target();
    let (platform, start_param, amount, label) = match kind {
        OpKind::ActivateCheck => (source, d.intake_start_param.as_deref(), None, "intake"),
        OpKind::CreatePayoutCheck => (
            target,
            None,
            Some(need(d.payout_amount, "payout amount", order)?),
            "payout",
        ),
        OpKind::CreateRefundCheck => (
            source,
            None,
            Some(need(d.refund_amount, "refund amount", order)?),
            "refund",
        ),
        OpKind::PayInvoice => (
            target,
            d.invoice_start_param.as_deref(),
            Some(need(d.invoice_amount, "invoice amount", order)?),
            "pay",
        ),
    };
    if kind == OpKind::ActivateCheck && start_param.is_none() {
        return Err(EngineError::Invariant(format!(
            "order {}: no check to activate",
            order.id
        )));
    }
    let (wallet, _) = storage::wallets::active(&mut *conn, platform)
        .await?
        .ok_or(EngineError::NoWalletAccount(platform))?;
    let n = storage::operations::count_for_order(&mut *conn, order.id, kind).await? + 1;
    let key = format!("ord-{}-{label}-{n}", order.id);
    let id = storage::operations::insert(
        conn,
        &NewOperation {
            order_id: order.id,
            kind,
            role,
            platform,
            wallet_account_id: wallet,
            idempotency_key: &key,
            start_param,
            amount,
            asset: order.asset,
            random_id: userbot::flows::rid::new_random_id(),
        },
    )
    .await?;
    Ok(id)
}
