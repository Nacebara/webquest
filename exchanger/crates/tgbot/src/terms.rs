//! Условия направлений для экранов (`ui::Terms`): комиссия по рычагу резерва, лимиты с учётом
//! резерва целевого кошелька, статус. Те же формулы, что у движка при приёме чека
//! (`domain::pricing::max_order_effective`, `Lever`) — экран не обещает больше, чем движок примет.

use std::collections::HashMap;

use domain::pricing::{Lever, Limits, max_order_effective};
use domain::{Asset, Decimal, Direction};
use rust_decimal::RoundingStrategy;
use sqlx::PgConnection;
use ui::{DirectionStatus, DirectionTerms, Terms};

use crate::TgBotError;

/// Условия и свободный резерв по направлениям.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TermsSnapshot {
    pub terms: Terms,
    /// Сколько резерва целевого кошелька свободно под новую заявку (после запаса).
    pub room: HashMap<Direction, Decimal>,
}

/// Вниз до центов: показываем не больше, чем можно принять.
fn floor_cents(value: Decimal) -> Decimal {
    value
        .max(Decimal::ZERO)
        .round_dp_with_strategy(2, RoundingStrategy::ToZero)
}

pub(crate) async fn load(
    conn: &mut PgConnection,
    safety: Decimal,
    bind_username: Option<&str>,
) -> Result<TermsSnapshot, TgBotError> {
    let asset = Asset::Usdt;
    let mut directions = Vec::new();
    let mut room = HashMap::new();
    // Основной спрос — CryptoBot → xRocket — первым (DESIGN-v0.2 Р1).
    for direction in [
        Direction::CbToXrCheck,
        Direction::XrToCbCheck,
        Direction::PayCbInvoice,
        Direction::PayXrInvoice,
    ] {
        let row = storage::directions::get(&mut *conn, direction).await?;
        let wallet = storage::wallets::active(&mut *conn, direction.target()).await?;
        let (fee_pct, effective, free) = match wallet {
            Some((wallet, _)) => {
                let available = storage::holds::available(&mut *conn, wallet, asset).await?;
                let target = storage::directions::reserve_target(&mut *conn, wallet, asset).await?;
                let fee = match target {
                    Some(target) => Lever {
                        base_pct: row.fee_pct,
                        k_pct: row.skew_k_pct,
                        floor_pct: row.fee_floor_pct,
                        cap_pct: row.fee_cap_pct,
                    }
                    .effective_fee_pct(available, target)
                    .unwrap_or(row.fee_pct),
                    None => row.fee_pct,
                };
                let limits = Limits {
                    min_amount: row.min_amount,
                    max_amount: row.max_amount,
                };
                (
                    fee,
                    max_order_effective(limits, available, safety),
                    available - safety,
                )
            }
            None => (row.fee_pct, None, Decimal::ZERO),
        };
        let status = if !row.is_open {
            DirectionStatus::Off
        } else if row.auto_paused || effective.is_none() {
            DirectionStatus::Paused
        } else {
            DirectionStatus::Open
        };
        room.insert(direction, floor_cents(free));
        directions.push(DirectionTerms {
            direction,
            fee_pct: fee_pct.normalize(),
            min_fee: row.min_fee.normalize(),
            min: row.min_amount.normalize(),
            max: floor_cents(effective.unwrap_or(row.max_amount)).normalize(),
            status,
        });
    }
    Ok(TermsSnapshot {
        terms: Terms {
            directions,
            bind_username: bind_username.map(str::to_owned),
        },
        room,
    })
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn cents_are_floored_and_never_negative() {
        assert_eq!(floor_cents(dec!(12.349)), dec!(12.34));
        assert_eq!(floor_cents(dec!(-3)), dec!(0));
        assert_eq!(floor_cents(dec!(500)), dec!(500));
    }
}
