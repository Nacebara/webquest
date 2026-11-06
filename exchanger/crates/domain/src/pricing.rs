//! Цены и котировки (SPEC §2.3, §3.4, §4 П6). Все проценты — в процентах: `2.5` = 2,5 %.

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

use crate::money::Step;

const HUNDRED: Decimal = Decimal::ONE_HUNDRED;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PricingError {
    #[error("amount {amount} is below the minimum {min}")]
    BelowMinimum { amount: Decimal, min: Decimal },
    #[error("amount {amount} is above the maximum {max}")]
    AboveMaximum { amount: Decimal, max: Decimal },
    #[error("nothing left to pay out after the fee")]
    NothingToPayOut,
    #[error("invalid pricing parameters: {0}")]
    InvalidParams(&'static str),
    #[error("arithmetic overflow")]
    Overflow,
}

/// Комиссия направления на момент котировки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeePolicy {
    /// Процент комиссии, `2.5` = 2,5 %.
    pub fee_pct: Decimal,
    /// Минимальная комиссия в активе заявки.
    pub min_fee: Decimal,
    /// Выгодный курс встречного потока на часть суммы (`crate::fees`); `None` — нет.
    #[serde(default)]
    pub discount: Option<Discount>,
}

/// Выгодный курс на первые `room` единиц суммы: на них — `fee_pct`, на остаток — обычная
/// ставка. Дробить заявку бессмысленно, а перегнуть резерв в другую сторону нельзя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discount {
    pub fee_pct: Decimal,
    pub room: Decimal,
}

impl FeePolicy {
    /// Один процент на всю сумму.
    pub const fn flat(fee_pct: Decimal, min_fee: Decimal) -> Self {
        Self {
            fee_pct,
            min_fee,
            discount: None,
        }
    }

    fn validate(&self) -> Result<(), PricingError> {
        if self.fee_pct.is_sign_negative() || self.fee_pct >= HUNDRED {
            return Err(PricingError::InvalidParams("fee_pct must be in [0, 100)"));
        }
        if self.min_fee.is_sign_negative() {
            return Err(PricingError::InvalidParams("min_fee must not be negative"));
        }
        if let Some(d) = self.discount {
            if d.fee_pct.is_sign_negative() || d.fee_pct > self.fee_pct {
                return Err(PricingError::InvalidParams(
                    "discount fee_pct must be in [0, fee_pct]",
                ));
            }
            if d.room.is_sign_negative() {
                return Err(PricingError::InvalidParams(
                    "discount room must not be negative",
                ));
            }
        }
        Ok(())
    }

    /// Процентная часть комиссии с суммы `amount` (без минимальной комиссии).
    pub fn pct_fee(&self, amount: Decimal) -> Result<Decimal, PricingError> {
        let (cheap, rest) = match self.discount {
            Some(d) => {
                let cheap = amount.min(d.room).max(Decimal::ZERO);
                (Some((cheap, d.fee_pct)), amount - cheap)
            }
            None => (None, amount),
        };
        let part = |base: Decimal, pct: Decimal| {
            base.checked_mul(pct)
                .and_then(|x| x.checked_div(HUNDRED))
                .ok_or(PricingError::Overflow)
        };
        let mut fee = part(rest, self.fee_pct)?;
        if let Some((cheap, pct)) = cheap {
            fee = fee
                .checked_add(part(cheap, pct)?)
                .ok_or(PricingError::Overflow)?;
        }
        Ok(fee)
    }

    /// Средний процент с суммы — для показа клиенту и записи в заявку (`fee_pct_applied`),
    /// вверх до 0,01 п.п.
    pub fn average_pct(&self, amount: Decimal) -> Result<Decimal, PricingError> {
        if amount <= Decimal::ZERO {
            return Ok(self.fee_pct);
        }
        let fee = self.pct_fee(amount)?;
        let pct = fee
            .checked_mul(HUNDRED)
            .and_then(|x| x.checked_div(amount))
            .ok_or(PricingError::Overflow)?;
        Ok(pct
            .round_dp_with_strategy(2, RoundingStrategy::ToPositiveInfinity)
            .normalize())
    }
}

/// Ценовой рычаг балансировки: `fee = clamp(base + k · s, floor, cap)`,
/// `s = clamp(1 − available / target, −1, 1)` по стороне выплаты (SPEC §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lever {
    pub base_pct: Decimal,
    pub k_pct: Decimal,
    pub floor_pct: Decimal,
    pub cap_pct: Decimal,
}

impl Lever {
    /// Текущая комиссия, округлённая вверх до 0,01 п.п. (в пользу сервиса).
    pub fn effective_fee_pct(
        &self,
        available: Decimal,
        target: Decimal,
    ) -> Result<Decimal, PricingError> {
        if self.floor_pct > self.cap_pct {
            return Err(PricingError::InvalidParams("floor_pct > cap_pct"));
        }
        if target <= Decimal::ZERO {
            return Err(PricingError::InvalidParams(
                "target reserve must be positive",
            ));
        }
        let ratio = available
            .checked_div(target)
            .ok_or(PricingError::Overflow)?;
        let skew = (Decimal::ONE - ratio).clamp(-Decimal::ONE, Decimal::ONE);
        let raw = self
            .k_pct
            .checked_mul(skew)
            .and_then(|adj| self.base_pct.checked_add(adj))
            .ok_or(PricingError::Overflow)?;
        let rounded = raw.round_dp_with_strategy(2, RoundingStrategy::ToPositiveInfinity);
        Ok(rounded.clamp(self.floor_pct, self.cap_pct).normalize())
    }
}

/// Котировка обмена чека: сколько пришло, сколько комиссия, сколько выплатим.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeQuote {
    pub amount_in: Decimal,
    pub fee: Decimal,
    pub payout: Decimal,
}

/// Обмен чека: `fee_raw = max(amount_in · fee%, min_fee)`, `payout = floor_step(amount_in − fee_raw)`,
/// `fee = amount_in − payout` — остаток округления уходит в комиссию.
pub fn quote_check_exchange(
    amount_in: Decimal,
    fee: FeePolicy,
    step: Step,
) -> Result<ExchangeQuote, PricingError> {
    fee.validate()?;
    if amount_in <= Decimal::ZERO {
        return Err(PricingError::NothingToPayOut);
    }
    let fee_raw = fee.pct_fee(amount_in)?.max(fee.min_fee);
    let payout = step.floor(amount_in - fee_raw);
    if payout <= Decimal::ZERO {
        return Err(PricingError::NothingToPayOut);
    }
    Ok(ExchangeQuote {
        amount_in: amount_in.normalize(),
        fee: (amount_in - payout).normalize(),
        payout,
    })
}

/// Котировка оплаты чужого счёта: клиент присылает `client_pays`, мы платим счёт `invoice_amount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceQuote {
    pub invoice_amount: Decimal,
    pub client_pays: Decimal,
    pub fee: Decimal,
}

/// `X = max(ceil_step(A · (1 + fee%)), ceil_step(A + min_fee))`, комиссия = `X − A`.
pub fn quote_invoice_payment(
    invoice_amount: Decimal,
    fee: FeePolicy,
    step: Step,
) -> Result<InvoiceQuote, PricingError> {
    fee.validate()?;
    if invoice_amount <= Decimal::ZERO {
        return Err(PricingError::InvalidParams(
            "invoice amount must be positive",
        ));
    }
    let by_pct = invoice_amount
        .checked_add(fee.pct_fee(invoice_amount)?)
        .ok_or(PricingError::Overflow)?;
    let by_min = invoice_amount
        .checked_add(fee.min_fee)
        .ok_or(PricingError::Overflow)?;
    let client_pays = step.ceil(by_pct).max(step.ceil(by_min));
    Ok(InvoiceQuote {
        invoice_amount: invoice_amount.normalize(),
        client_pays,
        fee: (client_pays - invoice_amount).normalize(),
    })
}

/// Сумма счёта в фиате → сумма в активе с буфером на движение курса (SPEC §1.4, П5).
/// `rate` — сколько единиц фиата стоит 1 единица актива.
pub fn fiat_to_asset(
    fiat_amount: Decimal,
    rate: Decimal,
    buffer_pct: Decimal,
    step: Step,
) -> Result<Decimal, PricingError> {
    if rate <= Decimal::ZERO || fiat_amount <= Decimal::ZERO || buffer_pct.is_sign_negative() {
        return Err(PricingError::InvalidParams(
            "fiat amount and rate must be positive, buffer non-negative",
        ));
    }
    let raw = fiat_amount
        .checked_mul(HUNDRED + buffer_pct)
        .and_then(|x| x.checked_div(HUNDRED))
        .and_then(|x| x.checked_div(rate))
        .ok_or(PricingError::Overflow)?;
    Ok(step.ceil(raw))
}

/// Лимиты направления на одну заявку.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub min_amount: Decimal,
    pub max_amount: Decimal,
}

impl Limits {
    pub fn check(&self, amount: Decimal) -> Result<(), PricingError> {
        if amount < self.min_amount {
            Err(PricingError::BelowMinimum {
                amount,
                min: self.min_amount,
            })
        } else if amount > self.max_amount {
            Err(PricingError::AboveMaximum {
                amount,
                max: self.max_amount,
            })
        } else {
            Ok(())
        }
    }
}

/// Сколько можно удержать под чек, сумма которого ещё неизвестна (SPEC §3.4):
/// `min(max_amount, available − safety)`; `None`, если меньше минимальной заявки — чек не активируем.
pub fn max_order_effective(limits: Limits, available: Decimal, safety: Decimal) -> Option<Decimal> {
    let room = available - safety;
    let eff = limits.max_amount.min(room);
    (eff >= limits.min_amount).then_some(eff)
}

/// Доля «лимита без код-пароля» CryptoBot, которую оставляем про запас: траты владельца руками
/// в том же аккаунте и расхождение курса USD/USDT (DESIGN-v0.2 Р11).
pub const PASSCODE_MARGIN: Decimal = Decimal::from_parts(5, 0, 0, false, 2);

/// Сколько ещё можно потратить с аккаунта CryptoBot за скользящие 24 часа, чтобы бот не спросил
/// код-пароль: `limit · (1 − PASSCODE_MARGIN) − committed`, не меньше нуля. `committed` — уже
/// потрачено и обещано (удержания под выплаты) за 24 часа.
pub fn passcode_free_headroom(limit: Decimal, committed: Decimal) -> Decimal {
    let cap = limit * (Decimal::ONE - PASSCODE_MARGIN);
    (cap - committed).max(Decimal::ZERO)
}

/// Сколько пришло относительно требуемой суммы (SPEC §4, П6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Funding {
    Exact,
    /// Недоплата: ждём ещё `missing`.
    Under {
        missing: Decimal,
    },
    /// Переплата: `refund = true` — вернуть излишек, иначе оставить в доходе.
    Over {
        excess: Decimal,
        refund: bool,
    },
}

pub fn classify_funding(received: Decimal, required: Decimal, min_refund: Decimal) -> Funding {
    use std::cmp::Ordering;
    match received.cmp(&required) {
        Ordering::Equal => Funding::Exact,
        Ordering::Less => Funding::Under {
            missing: (required - received).normalize(),
        },
        Ordering::Greater => {
            let excess = (received - required).normalize();
            Funding::Over {
                excess,
                refund: excess >= min_refund,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn passcode_headroom_keeps_a_margin_and_never_goes_negative() {
        assert_eq!(PASSCODE_MARGIN, dec!(0.05));
        assert_eq!(passcode_free_headroom(dec!(500), dec!(0)), dec!(475));
        assert_eq!(passcode_free_headroom(dec!(500), dec!(400)), dec!(75));
        assert_eq!(passcode_free_headroom(dec!(500), dec!(475)), dec!(0));
        assert_eq!(passcode_free_headroom(dec!(500), dec!(900)), dec!(0));
        assert_eq!(passcode_free_headroom(dec!(0), dec!(0)), dec!(0));
        assert_eq!(passcode_free_headroom(dec!(30000), dec!(1000)), dec!(27500));
    }

    use super::*;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    fn usdt_step() -> Step {
        Step::from_dp(2).unwrap()
    }

    fn policy(pct: Decimal, min: Decimal) -> FeePolicy {
        FeePolicy::flat(pct, min)
    }

    #[test]
    fn spec_example_check_exchange() {
        // SPEC §2.3: 33,33 USDT при 2,5 % → выплата 32,49, комиссия 0,84.
        let q =
            quote_check_exchange(dec!(33.33), policy(dec!(2.5), dec!(0.10)), usdt_step()).unwrap();
        assert_eq!(q.payout, dec!(32.49));
        assert_eq!(q.fee, dec!(0.84));
    }

    #[test]
    fn spec_example_hundred_usdt() {
        let q =
            quote_check_exchange(dec!(100), policy(dec!(2.5), dec!(0.10)), usdt_step()).unwrap();
        assert_eq!((q.payout, q.fee), (dec!(97.5), dec!(2.5)));
    }

    #[test]
    fn min_fee_applies_to_small_amounts() {
        let q = quote_check_exchange(dec!(2), policy(dec!(1.5), dec!(0.10)), usdt_step()).unwrap();
        assert_eq!((q.payout, q.fee), (dec!(1.9), dec!(0.1)));
    }

    #[test]
    fn tiny_amount_yields_nothing() {
        let r = quote_check_exchange(dec!(0.10), policy(dec!(2.5), dec!(0.10)), usdt_step());
        assert_eq!(r, Err(PricingError::NothingToPayOut));
    }

    #[test]
    fn spec_example_invoice_quote() {
        // SPEC §2.3: счёт 10 USDT при 3 % → клиент присылает 10,30.
        let q = quote_invoice_payment(dec!(10), policy(dec!(3), dec!(0.20)), usdt_step()).unwrap();
        assert_eq!((q.client_pays, q.fee), (dec!(10.3), dec!(0.3)));
        let small =
            quote_invoice_payment(dec!(2), policy(dec!(3), dec!(0.20)), usdt_step()).unwrap();
        assert_eq!(
            small.client_pays,
            dec!(2.2),
            "min fee wins for small invoices"
        );
    }

    #[test]
    fn spec_example_lever() {
        // SPEC §2.3: сторона CryptoBot на 40 % цели → 2,95 %; xRocket на 160 % → 1,05 %.
        let xc = Lever {
            base_pct: dec!(2.5),
            k_pct: dec!(0.75),
            floor_pct: dec!(1.5),
            cap_pct: dec!(3.5),
        };
        assert_eq!(
            xc.effective_fee_pct(dec!(360), dec!(900)).unwrap(),
            dec!(2.95)
        );
        let cx = Lever {
            base_pct: dec!(1.5),
            k_pct: dec!(0.75),
            floor_pct: dec!(0.5),
            cap_pct: dec!(3.0),
        };
        assert_eq!(
            cx.effective_fee_pct(dec!(1200), dec!(750)).unwrap(),
            dec!(1.05)
        );
    }

    #[test]
    fn fiat_conversion_rounds_up_with_buffer() {
        // 1000 RUB при курсе 95 RUB за USDT и буфере 1 % → 10,6315… → 10,64.
        assert_eq!(
            fiat_to_asset(dec!(1000), dec!(95), dec!(1), usdt_step()).unwrap(),
            dec!(10.64)
        );
    }

    #[test]
    fn hold_sizing_follows_reserve() {
        let limits = Limits {
            min_amount: dec!(2),
            max_amount: dec!(300),
        };
        assert_eq!(
            max_order_effective(limits, dec!(900), dec!(20)),
            Some(dec!(300))
        );
        assert_eq!(
            max_order_effective(limits, dec!(120), dec!(20)),
            Some(dec!(100))
        );
        assert_eq!(max_order_effective(limits, dec!(21), dec!(20)), None);
    }

    #[test]
    fn funding_classification() {
        assert_eq!(
            classify_funding(dec!(10.30), dec!(10.3), dec!(0.5)),
            Funding::Exact
        );
        assert_eq!(
            classify_funding(dec!(7), dec!(10.3), dec!(0.5)),
            Funding::Under { missing: dec!(3.3) }
        );
        assert_eq!(
            classify_funding(dec!(10.5), dec!(10.3), dec!(0.5)),
            Funding::Over {
                excess: dec!(0.2),
                refund: false
            }
        );
        assert_eq!(
            classify_funding(dec!(11), dec!(10.3), dec!(0.5)),
            Funding::Over {
                excess: dec!(0.7),
                refund: true
            }
        );
    }

    fn amount() -> impl Strategy<Value = Decimal> {
        // 0.01 … 1 000 000 с точностью до 6 знаков.
        (1i64..=1_000_000_000_000, 0u32..=6)
            .prop_map(|(m, s)| Decimal::new(m, s))
            .prop_filter("≥ 0.01", |d| *d >= dec!(0.01))
    }

    fn pct() -> impl Strategy<Value = Decimal> {
        (0i64..=1_000).prop_map(|m| Decimal::new(m, 2)) // 0.00 … 10.00 %
    }

    fn min_fee() -> impl Strategy<Value = Decimal> {
        (0i64..=100).prop_map(|m| Decimal::new(m, 2)) // 0.00 … 1.00
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        #[test]
        fn exchange_invariants(a in amount(), p in pct(), m in min_fee(), dp in 0u32..=6) {
            let step = Step::from_dp(dp).unwrap();
            let policy = policy(p, m);
            match quote_check_exchange(a, policy, step) {
                Ok(q) => {
                    prop_assert_eq!(q.payout + q.fee, a);
                    prop_assert!(step.is_multiple(q.payout));
                    prop_assert!(q.payout > Decimal::ZERO && q.payout <= a);
                    prop_assert!(q.fee >= m);
                    prop_assert!(q.fee >= a * p / HUNDRED);
                    // Сервис не берёт больше одного шага сверх расчётной комиссии.
                    prop_assert!(q.fee - (a * p / HUNDRED).max(m) < step.as_decimal());
                }
                Err(PricingError::NothingToPayOut) => {
                    prop_assert!(step.floor(a - (a * p / HUNDRED).max(m)) <= Decimal::ZERO);
                }
                Err(e) => prop_assert!(false, "unexpected error {e:?}"),
            }
        }

        #[test]
        fn exchange_payout_is_monotonic(a in amount(), b in amount(), p in pct(), m in min_fee()) {
            let step = Step::from_dp(2).unwrap();
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            let policy = policy(p, m);
            let payout = |x| quote_check_exchange(x, policy, step).map(|q| q.payout).unwrap_or(Decimal::ZERO);
            prop_assert!(payout(lo) <= payout(hi));
        }

        #[test]
        fn invoice_quote_invariants(a in amount(), p in pct(), m in min_fee(), dp in 0u32..=6) {
            let step = Step::from_dp(dp).unwrap();
            let q = quote_invoice_payment(a, policy(p, m), step).unwrap();
            let required = (a * (HUNDRED + p) / HUNDRED).max(a + m);
            prop_assert!(step.is_multiple(q.client_pays));
            prop_assert!(q.client_pays >= required);
            prop_assert!(q.client_pays - required < step.as_decimal(), "X is the smallest step multiple");
            prop_assert!(q.fee >= m);
            prop_assert_eq!(q.client_pays - q.fee, a);
        }

        #[test]
        fn discount_is_never_dearer_and_splitting_gains_nothing(
            a in amount(), b in amount(), p in pct(), cheap in pct(), room in amount(),
        ) {
            let cheap = cheap.min(p);
            let with = |room: Decimal| FeePolicy {
                discount: Some(Discount { fee_pct: cheap, room }),
                ..policy(p, Decimal::ZERO)
            };
            let flat = policy(p, Decimal::ZERO).pct_fee(a).unwrap();
            let all_cheap = policy(cheap, Decimal::ZERO).pct_fee(a).unwrap();
            let fee = with(room).pct_fee(a).unwrap();
            prop_assert!(fee <= flat && fee >= all_cheap);
            // Две заявки подряд (вторая видит остаток объёма) стоят ровно как одна.
            let one = with(room).pct_fee(a + b).unwrap();
            let two = with(room).pct_fee(a).unwrap()
                + with((room - a).max(Decimal::ZERO)).pct_fee(b).unwrap();
            prop_assert_eq!(one, two);
            // Котировка с выгодным курсом сходится так же, как без него.
            if let Ok(q) = quote_check_exchange(a, with(room), Step::from_dp(2).unwrap()) {
                prop_assert_eq!(q.payout + q.fee, a);
                prop_assert!(q.fee >= fee);
            }
        }

        #[test]
        fn lever_stays_within_bounds_and_is_monotonic(
            avail_a in 0i64..=10_000, avail_b in 0i64..=10_000, target in 1i64..=5_000,
            base in 0i64..=500, k in 0i64..=200, floor in 0i64..=200, span in 0i64..=500,
        ) {
            let lever = Lever {
                base_pct: Decimal::new(base, 2),
                k_pct: Decimal::new(k, 2),
                floor_pct: Decimal::new(floor, 2),
                cap_pct: Decimal::new(floor + span, 2),
            };
            let t = Decimal::from(target);
            let fa = lever.effective_fee_pct(Decimal::from(avail_a), t).unwrap();
            let fb = lever.effective_fee_pct(Decimal::from(avail_b), t).unwrap();
            prop_assert!(fa >= lever.floor_pct && fa <= lever.cap_pct);
            if avail_a <= avail_b {
                prop_assert!(fa >= fb, "more reserve never makes the direction more expensive");
            }
        }
    }
}
