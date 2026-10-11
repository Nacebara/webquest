//! Чем закрыть обмен после зачисления чека (`RECEIVED`, SPEC §2.3, §3.4).
//!
//! - стоп-кран выплат или чужой актив — человеку;
//! - сумма вне лимитов направления (для нового клиента — его лимит) или больше удержанного
//!   резерва — возврат всей суммы чеком на платформе входа;
//! - иначе выплата: комиссия по рычагу резерва, выплата вниз до шага платформы.

use domain::pricing::{self, FeePolicy, Lever, PricingError};
use domain::{Asset, Decimal, Money, Step};
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
    Refund {
        code: &'static str,
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
            reason: "kill switch: payouts are stopped".to_owned(),
        });
    }
    if received.asset() != order.asset {
        return Ok(Decision::Manual {
            reason: format!(
                "check in {} for a {} order",
                received.asset().code(),
                order.asset.code()
            ),
        });
    }
    let amount = received.amount();
    let dir = storage::directions::get(&mut *conn, order.direction).await?;
    let max = if storage::orders::is_new_user(&mut *conn, order.user_id).await? {
        dir.new_user_max_amount
    } else {
        dir.max_amount
    };
    if amount < dir.min_amount {
        return Ok(Decision::Refund {
            code: "below_minimum",
        });
    }
    if amount > max {
        return Ok(Decision::Refund {
            code: "above_maximum",
        });
    }

    let target = order.direction.target();
    let (wallet, _) = storage::wallets::active(&mut *conn, target)
        .await?
        .ok_or(EngineError::NoWalletAccount(target))?;
    let held = storage::holds::active(&mut *conn, order.id)
        .await?
        .map_or(Decimal::ZERO, |(_, _, amount)| amount);
    let fee_pct = fee_pct(&mut *conn, &dir, wallet, order.asset).await?;
    let step = Step::from_decimal(
        storage::directions::payout_step(&mut *conn, target, order.asset).await?,
    )?;
    let quote = match pricing::quote_check_exchange(
        amount,
        FeePolicy {
            fee_pct,
            min_fee: dir.min_fee,
        },
        step,
    ) {
        Ok(q) => q,
        Err(PricingError::NothingToPayOut) => {
            return Ok(Decision::Refund {
                code: "nothing_to_pay_out",
            });
        }
        Err(e) => return Err(e.into()),
    };
    if quote.payout > held {
        return Ok(Decision::Refund {
            code: "above_reserved_amount",
        });
    }
    Ok(Decision::Payout {
        fee_pct,
        fee: quote.fee,
        payout: quote.payout,
    })
}

/// Комиссия по рычагу резерва целевого кошелька; без заданного целевого резерва — базовая.
async fn fee_pct(
    conn: &mut PgConnection,
    dir: &storage::directions::DirectionRow,
    wallet: i16,
    asset: Asset,
) -> Result<Decimal, EngineError> {
    let Some(target) = storage::directions::reserve_target(&mut *conn, wallet, asset).await? else {
        return Ok(dir.fee_pct);
    };
    let available = storage::holds::available(&mut *conn, wallet, asset).await?;
    let lever = Lever {
        base_pct: dir.fee_pct,
        k_pct: dir.skew_k_pct,
        floor_pct: dir.fee_floor_pct,
        cap_pct: dir.fee_cap_pct,
    };
    Ok(lever.effective_fee_pct(available, target)?)
}
