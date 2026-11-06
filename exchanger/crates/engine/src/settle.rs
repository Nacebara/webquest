//! Чем закрыть обмен после зачисления чека (`RECEIVED`, SPEC §2.3, §3.4).
//!
//! - стоп-кран выплат или чужой актив — человеку;
//! - сумма вне лимитов направления (для нового клиента — его лимит) или больше удержанного
//!   резерва — возврат всей суммы чеком на платформе входа;
//! - иначе выплата: комиссия по рычагу резерва, выплата вниз до шага платформы.

use domain::pricing::{self, PricingError};
use domain::{Decimal, Money, Step};
use sqlx::PgConnection;
use storage::orders::OrderRow;

use crate::EngineError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Payout {
        fee_pct: Decimal,
        fee: Decimal,
        payout: Decimal,
    },
    /// Возврат `amount` (сумма прихода вниз до шага платформы входа; остаток — в доход).
    Refund {
        code: &'static str,
        amount: Decimal,
    },
    Manual {
        reason: String,
    },
}

pub(crate) async fn decide(
    conn: &mut PgConnection,
    order: &OrderRow,
    received: &Money,
) -> Result<Decision, EngineError> {
    if storage::settings::flag_on(&mut *conn, "kill_switch_payouts").await? {
        return Ok(Decision::Manual {
            reason: "стоп-кран: выплаты остановлены".to_owned(),
        });
    }
    if received.asset() != order.asset {
        return Ok(Decision::Manual {
            reason: format!(
                "чек в {}, а заявка в {}",
                received.asset().code(),
                order.asset.code()
            ),
        });
    }
    let amount = received.amount();
    let eff = storage::user_terms::effective(&mut *conn, order.direction, order.user_id).await?;
    let dir = &eff.dir;
    let max = if storage::orders::is_new_user(&mut *conn, order.user_id).await? {
        dir.new_user_max_amount
    } else {
        dir.max_amount
    };
    if amount < dir.min_amount {
        return refund(&mut *conn, order, received, "below_minimum").await;
    }
    if amount > max {
        return refund(&mut *conn, order, received, "above_maximum").await;
    }

    let target = order.direction.target();
    let (wallet, _) = storage::wallets::active(&mut *conn, target)
        .await?
        .ok_or(EngineError::NoWalletAccount(target))?;
    let held = storage::holds::active(&mut *conn, order.id)
        .await?
        .map_or(Decimal::ZERO, |(_, _, amount)| amount);
    // Чек уже зачислен на кошелёк входа: объём встречного потока — как до заявки.
    let fee =
        crate::fees::fee_decision(&mut *conn, &eff, wallet, order.asset, amount, amount, held)
            .await?;
    let fee_pct = fee.policy.average_pct(amount)?;
    let step = Step::from_decimal(
        storage::directions::payout_step(&mut *conn, target, order.asset).await?,
    )?;
    let quote = match pricing::quote_check_exchange(amount, fee.policy, step) {
        Ok(q) => q,
        Err(PricingError::NothingToPayOut) => {
            return refund(&mut *conn, order, received, "nothing_to_pay_out").await;
        }
        Err(e) => return Err(e.into()),
    };
    if quote.payout > held {
        return refund(&mut *conn, order, received, "above_reserved_amount").await;
    }
    Ok(Decision::Payout {
        fee_pct,
        fee: quote.fee,
        payout: quote.payout,
    })
}

/// Возврат всей суммы чеком на платформе входа: вниз до шага платформы (остаток — в доход,
/// проводка `excess_kept` при подтверждении возврата). Меньше минимального чека платформы —
/// создать нечего, к человеку.
pub(crate) async fn refund(
    conn: &mut PgConnection,
    order: &OrderRow,
    received: &Money,
    code: &'static str,
) -> Result<Decision, EngineError> {
    let source = order.direction.source();
    let asset = received.asset();
    let step =
        Step::from_decimal(storage::directions::payout_step(&mut *conn, source, asset).await?)?;
    let amount = step.floor(received.amount());
    let min = storage::directions::min_check(&mut *conn, source, asset).await?;
    if amount <= Decimal::ZERO || amount < min {
        return Ok(Decision::Manual {
            reason: format!(
                "возврат {} {} меньше минимального чека {min} ({code})",
                received.amount(),
                asset.code()
            ),
        });
    }
    Ok(Decision::Refund { code, amount })
}
