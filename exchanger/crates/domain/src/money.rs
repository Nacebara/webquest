//! Деньги: только `Decimal`, шаги округления — степени десяти (SPEC §10.4).

use std::fmt;

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

use crate::asset::Asset;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("amount must not be negative: {0}")]
    Negative(Decimal),
    #[error("step must be a power of ten not greater than 1, got {0}")]
    BadStep(Decimal),
    #[error("asset mismatch: {0} vs {1}")]
    AssetMismatch(Asset, Asset),
    #[error("arithmetic overflow")]
    Overflow,
}

/// Сумма в активе. Отрицательные суммы запрещены — знак есть только в проводке леджера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    amount: Decimal,
    asset: Asset,
}

impl Money {
    pub fn new(amount: Decimal, asset: Asset) -> Result<Self, MoneyError> {
        if amount.is_sign_negative() && !amount.is_zero() {
            return Err(MoneyError::Negative(amount));
        }
        Ok(Self {
            amount: amount.normalize(),
            asset,
        })
    }

    pub fn zero(asset: Asset) -> Self {
        Self {
            amount: Decimal::ZERO,
            asset,
        }
    }

    pub const fn amount(&self) -> Decimal {
        self.amount
    }

    pub const fn asset(&self) -> Asset {
        self.asset
    }

    pub fn checked_add(self, other: Money) -> Result<Money, MoneyError> {
        self.same_asset(other)?;
        let sum = self
            .amount
            .checked_add(other.amount)
            .ok_or(MoneyError::Overflow)?;
        Money::new(sum, self.asset)
    }

    /// Вычитание; результат не может быть отрицательным.
    pub fn checked_sub(self, other: Money) -> Result<Money, MoneyError> {
        self.same_asset(other)?;
        let diff = self
            .amount
            .checked_sub(other.amount)
            .ok_or(MoneyError::Overflow)?;
        Money::new(diff, self.asset)
    }

    fn same_asset(self, other: Money) -> Result<(), MoneyError> {
        if self.asset == other.asset {
            Ok(())
        } else {
            Err(MoneyError::AssetMismatch(self.asset, other.asset))
        }
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.amount, self.asset)
    }
}

/// Шаг округления выплат: 10^-dp (USDT — 0.01, TON — 0.001). Произвольные шаги
/// (0.05 и т. п.) не поддерживаем: округление до знака после запятой точно и без деления.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Step {
    dp: u32,
}

impl Step {
    /// Максимальная точность шага; больше 18 знаков нет в `NUMERIC(38,18)`.
    pub const MAX_DP: u32 = 18;

    pub const fn from_dp(dp: u32) -> Option<Self> {
        if dp <= Self::MAX_DP {
            Some(Self { dp })
        } else {
            None
        }
    }

    /// Из значения `platform_assets.payout_step` (0.01 → 2 знака).
    pub fn from_decimal(step: Decimal) -> Result<Self, MoneyError> {
        let normalized = step.normalize();
        let dp = normalized.scale();
        let is_power_of_ten = normalized.mantissa() == 1 && !normalized.is_sign_negative();
        match (is_power_of_ten, Self::from_dp(dp)) {
            (true, Some(s)) => Ok(s),
            _ => Err(MoneyError::BadStep(step)),
        }
    }

    pub const fn dp(self) -> u32 {
        self.dp
    }

    pub fn as_decimal(self) -> Decimal {
        Decimal::new(1, self.dp)
    }

    /// Вниз до шага (для неотрицательных — к нулю). Так считаем выплату клиенту.
    pub fn floor(self, x: Decimal) -> Decimal {
        x.round_dp_with_strategy(self.dp, RoundingStrategy::ToNegativeInfinity)
            .normalize()
    }

    /// Вверх до шага. Так считаем сумму «к оплате» в котировке.
    pub fn ceil(self, x: Decimal) -> Decimal {
        x.round_dp_with_strategy(self.dp, RoundingStrategy::ToPositiveInfinity)
            .normalize()
    }

    pub fn is_multiple(self, x: Decimal) -> bool {
        x.normalize().scale() <= self.dp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    #[test]
    fn step_from_decimal_accepts_powers_of_ten_only() {
        assert_eq!(Step::from_decimal(dec!(0.01)).map(Step::dp), Ok(2));
        assert_eq!(Step::from_decimal(dec!(0.010)).map(Step::dp), Ok(2));
        assert_eq!(Step::from_decimal(dec!(1)).map(Step::dp), Ok(0));
        assert!(Step::from_decimal(dec!(0.05)).is_err());
        assert!(Step::from_decimal(dec!(10)).is_err());
        assert!(Step::from_decimal(dec!(-0.01)).is_err());
        assert!(Step::from_decimal(dec!(0)).is_err());
    }

    #[test]
    fn floor_and_ceil_examples() {
        let s = Step::from_dp(2).unwrap();
        assert_eq!(s.floor(dec!(32.49675)), dec!(32.49));
        assert_eq!(s.ceil(dec!(10.3000001)), dec!(10.31));
        assert_eq!(s.ceil(dec!(10.30)), dec!(10.3));
        assert_eq!(s.floor(dec!(5)), dec!(5));
    }

    #[test]
    fn money_rejects_negative_and_mixed_assets() {
        assert!(Money::new(dec!(-1), Asset::Usdt).is_err());
        let a = Money::new(dec!(1), Asset::Usdt).unwrap();
        let b = Money::new(dec!(1), Asset::Ton).unwrap();
        assert_eq!(
            a.checked_add(b),
            Err(MoneyError::AssetMismatch(Asset::Usdt, Asset::Ton))
        );
        let two = Money::new(dec!(2), Asset::Usdt).unwrap();
        assert!(a.checked_sub(two).is_err());
    }

    pub(crate) fn amount() -> impl Strategy<Value = Decimal> {
        (0i64..=10_000_000_000, 0u32..=8).prop_map(|(m, s)| Decimal::new(m, s))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        #[test]
        fn floor_is_largest_multiple_not_above(x in amount(), dp in 0u32..=6) {
            let s = Step::from_dp(dp).unwrap();
            let f = s.floor(x);
            prop_assert!(s.is_multiple(f));
            prop_assert!(f <= x);
            prop_assert!(x - f < s.as_decimal());
        }

        #[test]
        fn ceil_is_smallest_multiple_not_below(x in amount(), dp in 0u32..=6) {
            let s = Step::from_dp(dp).unwrap();
            let c = s.ceil(x);
            prop_assert!(s.is_multiple(c));
            prop_assert!(c >= x);
            prop_assert!(c - x < s.as_decimal());
        }
    }
}
