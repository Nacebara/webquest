//! Условия направлений для экранов (`ui::Terms`): комиссия по рычагу резерва (одна ставка или
//! ступени), выгодный курс встречного потока, лимиты с учётом резерва целевого кошелька, статус,
//! личные условия клиента. Те же формулы, что у движка при приёме чека
//! (`domain::fees`, `domain::pricing::max_order_effective`) — экран не обещает больше, чем движок
//! примет.

use std::collections::HashMap;

use domain::fees::{FeeMode, LeverInput};
use domain::pricing::{Limits, max_order_effective};
use domain::{Asset, Decimal, Direction};
use rust_decimal::RoundingStrategy;
use sqlx::PgConnection;
use ui::{DirectionStatus, DirectionTerms, PauseReason, PromoTerms, Terms, TierTerms};

use crate::TgBotError;

/// Условия и свободный резерв по направлениям.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TermsSnapshot {
    pub terms: Terms,
    /// Сколько резерва целевого кошелька свободно под новую заявку (после запаса).
    pub room: HashMap<Direction, Decimal>,
}

/// Аккаунт юзербота кошелька на паузе (пульт): новые заявки не принимаются.
async fn userbot_paused(
    conn: &mut PgConnection,
    wallet: Option<(i16, i16)>,
) -> Result<bool, TgBotError> {
    Ok(match wallet {
        Some((w, _)) => storage::wallets::userbot_paused(conn, w).await?,
        None => false,
    })
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
    user: Option<i64>,
) -> Result<TermsSnapshot, TgBotError> {
    let asset = Asset::Usdt;
    let maintenance = storage::settings::flag_on(&mut *conn, "maintenance").await?
        || storage::settings::flag_on(&mut *conn, "kill_switch_payouts").await?;
    let mut directions = Vec::new();
    let mut room = HashMap::new();
    // Основной спрос — CryptoBot → xRocket — первым (DESIGN-v0.2 Р1).
    for direction in [
        Direction::CbToXrCheck,
        Direction::XrToCbCheck,
        Direction::PayCbInvoice,
        Direction::PayXrInvoice,
    ] {
        // Условия этого клиента (личная ставка, лимиты) или общие.
        let (row, personal) = match user {
            Some(user) => {
                let e = storage::user_terms::effective(&mut *conn, direction, user).await?;
                (e.dir, Some(e.personal))
            }
            None => (storage::directions::get(&mut *conn, direction).await?, None),
        };
        let wallet = storage::wallets::active(&mut *conn, direction.target()).await?;
        let (lever, effective, free) = match wallet {
            Some((wallet, _)) => {
                let available = storage::holds::available(&mut *conn, wallet, asset).await?;
                let target = storage::directions::reserve_target(&mut *conn, wallet, asset).await?;
                let lever = target.map(|target| LeverInput {
                    k_pct: row.skew_k_pct,
                    floor_pct: row.fee_floor_pct,
                    cap_pct: row.fee_cap_pct,
                    available,
                    target,
                });
                let limits = Limits {
                    min_amount: row.min_amount,
                    max_amount: row.max_amount,
                };
                (
                    lever,
                    max_order_effective(limits, available, safety),
                    available - safety,
                )
            }
            None => (None, None, Decimal::ZERO),
        };
        // Ставка с рычагом для базы; ошибка настройки рычага — базовая (экран не падает).
        let levered = |base: Decimal| {
            domain::fees::levered_pct(row.fee_pct, base, lever)
                .unwrap_or(base)
                .normalize()
        };
        let personal_pct = personal.as_ref().and_then(|p| p.fee_pct);
        let tiers: Vec<TierTerms> = match (personal_pct, row.fee_mode, row.fee_tiers.as_ref()) {
            (None, FeeMode::Tiers, Some(schedule)) if schedule.tiers().len() > 1 => schedule
                .tiers()
                .iter()
                .map(|t| TierTerms {
                    from: t.from.max(row.min_amount),
                    fee_pct: levered(t.fee_pct),
                })
                .collect(),
            _ => Vec::new(),
        };
        let fee_pct = match (personal_pct, tiers.first()) {
            (Some(p), _) => levered(p),
            (None, Some(first)) => first.fee_pct,
            (None, None) => levered(row.fee_pct),
        };
        let cheapest = tiers.iter().map(|t| t.fee_pct).fold(fee_pct, Decimal::min);
        let promo =
            engine::fees::active_promo(&mut *conn, direction, asset, Decimal::ZERO, Decimal::ZERO)
                .await
                .map_err(|e| match e {
                    engine::EngineError::Storage(s) => TgBotError::Storage(s),
                    other => TgBotError::Engine(other),
                })?
                .filter(|p| p.fee_pct < cheapest)
                .map(|p| PromoTerms {
                    fee_pct: p.fee_pct.normalize(),
                    up_to: floor_cents(p.room).normalize(),
                })
                .filter(|p| p.up_to > Decimal::ZERO);
        let source_wallet = storage::wallets::active(&mut *conn, direction.source()).await?;
        let technical = row.auto_paused
            || wallet.is_none()
            || source_wallet.is_none()
            || userbot_paused(&mut *conn, wallet).await?
            || userbot_paused(&mut *conn, source_wallet).await?;
        let (status, pause) = if !row.is_open {
            (DirectionStatus::Off, None)
        } else if maintenance {
            (DirectionStatus::Paused, Some(PauseReason::Maintenance))
        } else if technical {
            (DirectionStatus::Paused, Some(PauseReason::Technical))
        } else if effective.is_none() {
            (
                DirectionStatus::Paused,
                Some(PauseReason::NoReserve {
                    wallet: direction.target(),
                }),
            )
        } else {
            (DirectionStatus::Open, None)
        };
        room.insert(direction, floor_cents(free));
        directions.push(DirectionTerms {
            direction,
            fee_pct,
            min_fee: row.min_fee.normalize(),
            min: row.min_amount.normalize(),
            max: floor_cents(effective.unwrap_or(row.max_amount)).normalize(),
            status,
            tiers,
            promo,
            personal: personal.is_some_and(|p| p.is_personal()),
            pause,
            new_user_max: (row.new_user_max_amount < row.max_amount)
                .then(|| row.new_user_max_amount.normalize()),
        });
    }
    Ok(TermsSnapshot {
        terms: Terms {
            directions,
            bind_username: bind_username.map(str::to_owned),
            updated_at: Some(chrono::Utc::now()),
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
