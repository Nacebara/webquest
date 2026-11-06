//! Выбор комиссии заявки и выгодный курс встречного потока (решения владельца 6 ноября 2026,
//! DESIGN-v0.2 Р15).
//!
//! Порядок:
//! 1. **база** — личная ставка клиента; иначе в режиме «ступени» — ступень по сумме заявки;
//!    иначе фикс направления;
//! 2. **рычаг резерва** поверх базы: полоса пола и потолка сдвигается вместе с базой, поэтому
//!    рычаг не съедает ни личную ставку, ни ступень;
//! 3. **выгодный курс** встречного потока, если он сейчас действует и дешевле: только на объём,
//!    который возвращает оба кошелька к цели резерва (`room`), на остаток — ставка шага 2;
//! 4. **минимальная комиссия**; при выгодном курсе — меньшая из двух.
//!
//! Выгодный курс бывает только у направления, которое везёт деньги на кошелёк с нехваткой и
//! забирает их с кошелька с излишком. У встречного ему направления условия противоположные,
//! поэтому оба со скидкой одновременно не бывают: прокрутка по кругу всегда стоит не меньше
//! обычной ставки основного направления.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::pricing::{Discount, FeePolicy, Lever, PricingError};

const HUNDRED: Decimal = Decimal::ONE_HUNDRED;
/// Верхняя граница процента комиссии (как `CHECK fee_pct < 20` в схеме).
pub const MAX_FEE_PCT: Decimal = Decimal::from_parts(20, 0, 0, false, 0);
/// Больше ступеней на направление не держим: клиенту их надо прочитать.
pub const MAX_TIERS: usize = 8;

/// Как считать базовую комиссию направления.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeMode {
    /// Один процент на любую сумму (`directions.fee_pct`).
    Flat,
    /// Процент по ступени, в которую попадает сумма заявки.
    Tiers,
}

impl FeeMode {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            FeeMode::Flat => "flat",
            FeeMode::Tiers => "tiers",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "flat" => Some(FeeMode::Flat),
            "tiers" => Some(FeeMode::Tiers),
            _ => None,
        }
    }
}

/// Ступень: «от суммы `from` — `fee_pct` на всю сумму».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeeTier {
    #[serde(with = "rust_decimal::serde::str")]
    pub from: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub fee_pct: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleError {
    #[error("no tiers")]
    Empty,
    #[error("more than {MAX_TIERS} tiers")]
    TooMany,
    #[error("the first tier must start at 0")]
    FirstNotZero,
    #[error("tier starts must strictly increase")]
    NotIncreasing,
    #[error("tier fee must be in [0, {MAX_FEE_PCT})")]
    BadFee,
}

/// Ступени комиссии: начала строго растут, первая — с нуля.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<FeeTier>", into = "Vec<FeeTier>")]
pub struct FeeSchedule {
    tiers: Vec<FeeTier>,
}

impl TryFrom<Vec<FeeTier>> for FeeSchedule {
    type Error = ScheduleError;

    fn try_from(tiers: Vec<FeeTier>) -> Result<Self, ScheduleError> {
        Self::new(tiers)
    }
}

impl From<FeeSchedule> for Vec<FeeTier> {
    fn from(s: FeeSchedule) -> Self {
        s.tiers
    }
}

impl FeeSchedule {
    pub fn new(tiers: Vec<FeeTier>) -> Result<Self, ScheduleError> {
        let first = tiers.first().ok_or(ScheduleError::Empty)?;
        if tiers.len() > MAX_TIERS {
            return Err(ScheduleError::TooMany);
        }
        if !first.from.is_zero() {
            return Err(ScheduleError::FirstNotZero);
        }
        if tiers.windows(2).any(|w| w[1].from <= w[0].from) {
            return Err(ScheduleError::NotIncreasing);
        }
        if tiers
            .iter()
            .any(|t| t.fee_pct.is_sign_negative() || t.fee_pct >= MAX_FEE_PCT)
        {
            return Err(ScheduleError::BadFee);
        }
        Ok(Self {
            tiers: tiers
                .into_iter()
                .map(|t| FeeTier {
                    from: t.from.normalize(),
                    fee_pct: t.fee_pct.normalize(),
                })
                .collect(),
        })
    }

    /// Одна ступень с нуля — то же, что фикс.
    pub fn single(fee_pct: Decimal) -> Result<Self, ScheduleError> {
        Self::new(vec![FeeTier {
            from: Decimal::ZERO,
            fee_pct,
        }])
    }

    pub fn tiers(&self) -> &[FeeTier] {
        &self.tiers
    }

    /// Номер ступени для суммы.
    pub fn index_for(&self, amount: Decimal) -> usize {
        self.tiers
            .iter()
            .rposition(|t| t.from <= amount)
            .unwrap_or(0)
    }

    pub fn pct_for(&self, amount: Decimal) -> Decimal {
        self.tiers[self.index_for(amount)].fee_pct
    }

    /// Ступени дороже предыдущей: так бывает по ошибке, пульт спросит ещё раз.
    pub fn dearer_than_previous(&self) -> Vec<usize> {
        self.tiers
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[1].fee_pct > w[0].fee_pct)
            .map(|(i, _)| i + 1)
            .collect()
    }
}

/// Резерв кошелька: свободно и цель.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reserve {
    pub available: Decimal,
    pub target: Decimal,
}

impl Reserve {
    /// Нехватка в процентах цели: `> 0` — меньше цели, `< 0` — излишек. Цель не задана или
    /// нулевая — `None`.
    pub fn deficit_pct(&self) -> Option<Decimal> {
        if self.target <= Decimal::ZERO {
            return None;
        }
        (self.target - self.available)
            .checked_mul(HUNDRED)?
            .checked_div(self.target)
    }

    fn deficit(&self) -> Decimal {
        (self.target - self.available).max(Decimal::ZERO)
    }

    fn excess(&self) -> Decimal {
        (self.available - self.target).max(Decimal::ZERO)
    }
}

/// Сколько можно провести встречным потоком, чтобы оба кошелька приблизились к цели:
/// `min(нехватка кошелька входа, излишек кошелька выплаты)`. Без целей — ноль.
pub fn rebalance_room(source: Option<Reserve>, target: Option<Reserve>) -> Decimal {
    match (source, target) {
        (Some(s), Some(t)) if s.target > Decimal::ZERO && t.target > Decimal::ZERO => {
            s.deficit().min(t.excess())
        }
        _ => Decimal::ZERO,
    }
}

/// Пороги автоматики: включить при нехватке кошелька входа `≥ trigger_pct` цели, выключить при
/// `≤ release_pct` (разные пороги — чтобы не мигало).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoThresholds {
    pub trigger_pct: Decimal,
    pub release_pct: Decimal,
}

impl PromoThresholds {
    pub fn validate(&self) -> Result<(), PricingError> {
        if self.release_pct.is_sign_negative()
            || self.release_pct >= self.trigger_pct
            || self.trigger_pct > HUNDRED
        {
            return Err(PricingError::InvalidParams(
                "need 0 <= release_pct < trigger_pct <= 100",
            ));
        }
        Ok(())
    }

    /// Пора ли включить выгодный курс (для автоматики и подсказки).
    pub fn should_start(&self, source: Reserve, target: Reserve) -> bool {
        rebalance_room(Some(source), Some(target)) > Decimal::ZERO
            && source.deficit_pct().is_some_and(|d| d >= self.trigger_pct)
    }

    /// Держать ли включённым.
    pub fn should_keep(&self, source: Reserve, target: Reserve) -> bool {
        rebalance_room(Some(source), Some(target)) > Decimal::ZERO
            && source.deficit_pct().is_some_and(|d| d > self.release_pct)
    }

    /// Итог автоматики: было включено — держим до порога выключения, иначе — порог включения.
    pub fn next_state(&self, active: bool, source: Reserve, target: Reserve) -> bool {
        if active {
            self.should_keep(source, target)
        } else {
            self.should_start(source, target)
        }
    }
}

/// Выгодный курс, который сейчас действует на направлении.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Promo {
    pub fee_pct: Decimal,
    pub min_fee: Decimal,
    /// Объём, на который он действует ([`rebalance_room`]).
    pub room: Decimal,
}

/// Рычаг резерва направления: полоса вокруг базовой ставки направления и резерв выплаты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeverInput {
    pub k_pct: Decimal,
    pub floor_pct: Decimal,
    pub cap_pct: Decimal,
    pub available: Decimal,
    pub target: Decimal,
}

/// Всё, из чего складывается комиссия.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeInputs<'a> {
    /// Фикс направления (`directions.fee_pct`) — от него отсчитана полоса рычага.
    pub direction_pct: Decimal,
    pub min_fee: Decimal,
    pub mode: FeeMode,
    pub schedule: Option<&'a FeeSchedule>,
    /// Личная ставка клиента.
    pub personal_pct: Option<Decimal>,
    pub lever: Option<LeverInput>,
    pub promo: Option<Promo>,
}

/// Откуда взялась база.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaseSource {
    Flat,
    /// Номер ступени.
    Tier(usize),
    Personal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeDecision {
    /// Политика для котировки (`quote_check_exchange`, `quote_invoice_payment`).
    pub policy: FeePolicy,
    pub base_pct: Decimal,
    pub source: BaseSource,
    /// Ставка после рычага, без выгодного курса.
    pub lever_pct: Decimal,
    /// Выгодный курс применён.
    pub promo: bool,
}

/// Ставка после рычага для базы `base`: полоса направления сдвигается на `base − direction_pct`.
pub fn levered_pct(
    direction_pct: Decimal,
    base: Decimal,
    lever: Option<LeverInput>,
) -> Result<Decimal, PricingError> {
    let Some(l) = lever else {
        return Ok(base);
    };
    // Нулевая цель резерва — ошибка настройки: `Lever` откажет, заявка уйдёт человеку.
    let shift = base - direction_pct;
    Lever {
        base_pct: base,
        k_pct: l.k_pct,
        floor_pct: (l.floor_pct + shift).max(Decimal::ZERO).min(base),
        cap_pct: (l.cap_pct + shift).max(base),
    }
    .effective_fee_pct(l.available, l.target)
}

/// Комиссия заявки на сумму `amount` (обмен — сумма чека, оплата счёта — сумма счёта).
pub fn decide(amount: Decimal, inputs: &FeeInputs<'_>) -> Result<FeeDecision, PricingError> {
    let (base_pct, source) = match (inputs.personal_pct, inputs.mode, inputs.schedule) {
        (Some(p), _, _) => (p, BaseSource::Personal),
        (None, FeeMode::Tiers, Some(s)) => {
            let i = s.index_for(amount);
            (s.tiers()[i].fee_pct, BaseSource::Tier(i))
        }
        _ => (inputs.direction_pct, BaseSource::Flat),
    };
    let lever_pct = levered_pct(inputs.direction_pct, base_pct, inputs.lever)?;
    let mut policy = FeePolicy::flat(lever_pct, inputs.min_fee);
    let mut promo = false;
    if let Some(p) = inputs.promo
        && p.room > Decimal::ZERO
        && p.fee_pct < lever_pct
    {
        policy.discount = Some(Discount {
            fee_pct: p.fee_pct.max(Decimal::ZERO),
            room: p.room,
        });
        policy.min_fee = inputs.min_fee.min(p.min_fee);
        promo = true;
    }
    Ok(FeeDecision {
        policy,
        base_pct,
        source,
        lever_pct,
        promo,
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    use super::*;

    fn tiers() -> FeeSchedule {
        FeeSchedule::new(vec![
            FeeTier {
                from: dec!(0),
                fee_pct: dec!(2.5),
            },
            FeeTier {
                from: dec!(100),
                fee_pct: dec!(1.8),
            },
            FeeTier {
                from: dec!(250),
                fee_pct: dec!(1.2),
            },
        ])
        .unwrap()
    }

    fn inputs(schedule: &FeeSchedule) -> FeeInputs<'_> {
        FeeInputs {
            direction_pct: dec!(2.5),
            min_fee: dec!(0.10),
            mode: FeeMode::Tiers,
            schedule: Some(schedule),
            personal_pct: None,
            lever: None,
            promo: None,
        }
    }

    #[test]
    fn schedule_rules() {
        assert_eq!(FeeSchedule::new(vec![]), Err(ScheduleError::Empty));
        let t = |from, fee_pct| FeeTier { from, fee_pct };
        assert_eq!(
            FeeSchedule::new(vec![t(dec!(1), dec!(1))]),
            Err(ScheduleError::FirstNotZero)
        );
        assert_eq!(
            FeeSchedule::new(vec![t(dec!(0), dec!(1)), t(dec!(0), dec!(1))]),
            Err(ScheduleError::NotIncreasing)
        );
        assert_eq!(
            FeeSchedule::new(vec![t(dec!(0), dec!(20))]),
            Err(ScheduleError::BadFee)
        );
        let s = tiers();
        assert_eq!(s.pct_for(dec!(2)), dec!(2.5));
        assert_eq!(s.pct_for(dec!(99.99)), dec!(2.5));
        assert_eq!(s.pct_for(dec!(100)), dec!(1.8));
        assert_eq!(s.pct_for(dec!(5000)), dec!(1.2));
        assert!(s.dearer_than_previous().is_empty());
        let odd = FeeSchedule::new(vec![t(dec!(0), dec!(1)), t(dec!(50), dec!(2))]).unwrap();
        assert_eq!(odd.dearer_than_previous(), vec![1]);
    }

    #[test]
    fn base_comes_from_personal_then_tier_then_flat() {
        let s = tiers();
        let mut i = inputs(&s);
        let d = decide(dec!(300), &i).unwrap();
        assert_eq!((d.base_pct, d.source), (dec!(1.2), BaseSource::Tier(2)));
        i.mode = FeeMode::Flat;
        let d = decide(dec!(300), &i).unwrap();
        assert_eq!((d.base_pct, d.source), (dec!(2.5), BaseSource::Flat));
        i.personal_pct = Some(dec!(0.9));
        let d = decide(dec!(300), &i).unwrap();
        assert_eq!((d.base_pct, d.source), (dec!(0.9), BaseSource::Personal));
    }

    #[test]
    fn lever_band_moves_with_the_base() {
        let lever = LeverInput {
            k_pct: dec!(0.75),
            floor_pct: dec!(1.5),
            cap_pct: dec!(3.5),
            available: dec!(0),
            target: dec!(1000),
        };
        // Резерв пуст: +0.75 к любой базе, а не потолок направления.
        assert_eq!(
            levered_pct(dec!(2.5), dec!(1.2), Some(lever)).unwrap(),
            dec!(1.95)
        );
        // Резерв вдвое больше цели: −0.75, но не ниже сдвинутого пола (1.5 − 1.3 = 0.2).
        let rich = LeverInput {
            available: dec!(2000),
            ..lever
        };
        assert_eq!(
            levered_pct(dec!(2.5), dec!(1.2), Some(rich)).unwrap(),
            dec!(0.45)
        );
        assert_eq!(
            levered_pct(dec!(2.5), dec!(0.1), Some(rich)).unwrap(),
            dec!(0)
        );
    }

    #[test]
    fn promo_applies_only_when_cheaper_and_only_to_its_room() {
        let s = tiers();
        let mut i = inputs(&s);
        i.mode = FeeMode::Flat;
        i.promo = Some(Promo {
            fee_pct: dec!(0.3),
            min_fee: dec!(0.05),
            room: dec!(450),
        });
        let d = decide(dec!(500), &i).unwrap();
        assert!(d.promo);
        // 450 · 0.3 % + 50 · 2.5 % = 1.35 + 1.25 = 2.60.
        assert_eq!(d.policy.pct_fee(dec!(500)).unwrap(), dec!(2.60));
        assert_eq!(d.policy.average_pct(dec!(500)).unwrap(), dec!(0.52));
        assert_eq!(d.policy.min_fee, dec!(0.05));
        // Личная ставка ниже выгодного курса — выгодный курс не нужен.
        i.personal_pct = Some(dec!(0.2));
        assert!(!decide(dec!(500), &i).unwrap().promo);
        // Нет объёма — нет курса.
        i.personal_pct = None;
        i.promo = Some(Promo {
            room: dec!(0),
            ..i.promo.unwrap()
        });
        assert!(!decide(dec!(500), &i).unwrap().promo);
    }

    #[test]
    fn rebalance_room_and_thresholds() {
        let xr = Reserve {
            available: dec!(150),
            target: dec!(600),
        };
        let cb = Reserve {
            available: dec!(1300),
            target: dec!(600),
        };
        // Рокет → КБ: вход Рокет (нехватка 450), выплата КБ (излишек 700).
        assert_eq!(rebalance_room(Some(xr), Some(cb)), dec!(450));
        // КБ → Рокет: условия противоположные — объёма нет.
        assert_eq!(rebalance_room(Some(cb), Some(xr)), dec!(0));
        assert_eq!(rebalance_room(None, Some(cb)), dec!(0));
        assert_eq!(xr.deficit_pct(), Some(dec!(75)));
        let th = PromoThresholds {
            trigger_pct: dec!(30),
            release_pct: dec!(10),
        };
        th.validate().unwrap();
        assert!(th.should_start(xr, cb));
        assert!(!th.should_start(cb, xr));
        let mild = Reserve {
            available: dec!(500),
            target: dec!(600),
        };
        // Нехватка 16.7 %: включать рано, но включённое держим.
        assert!(!th.next_state(false, mild, cb));
        assert!(th.next_state(true, mild, cb));
        let fine = Reserve {
            available: dec!(560),
            target: dec!(600),
        };
        assert!(!th.next_state(true, fine, cb));
        assert!(
            PromoThresholds {
                trigger_pct: dec!(10),
                release_pct: dec!(10)
            }
            .validate()
            .is_err()
        );
    }

    fn pct() -> impl Strategy<Value = Decimal> {
        (0i64..=1_000).prop_map(|m| Decimal::new(m, 2))
    }

    fn amount() -> impl Strategy<Value = Decimal> {
        (1i64..=100_000_000).prop_map(|m| Decimal::new(m, 2))
    }

    fn reserve() -> impl Strategy<Value = Reserve> {
        (0i64..=100_000, 1i64..=50_000).prop_map(|(a, t)| Reserve {
            available: Decimal::from(a),
            target: Decimal::from(t),
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// Встречные направления пары никогда не получают выгодный курс одновременно.
        #[test]
        fn opposite_directions_never_both_discounted(a in reserve(), b in reserve()) {
            let one = rebalance_room(Some(a), Some(b));
            let other = rebalance_room(Some(b), Some(a));
            prop_assert!(one.is_zero() || other.is_zero());
        }

        /// Выгодный курс не дороже обычной ставки и не дешевле себя самого на всю сумму.
        #[test]
        fn decision_is_bounded(amount in amount(), base in pct(), cheap in pct(), room in amount()) {
            let i = FeeInputs {
                direction_pct: base,
                min_fee: Decimal::ZERO,
                mode: FeeMode::Flat,
                schedule: None,
                personal_pct: None,
                lever: None,
                promo: Some(Promo { fee_pct: cheap, min_fee: Decimal::ZERO, room }),
            };
            let d = decide(amount, &i).unwrap();
            let fee = d.policy.pct_fee(amount).unwrap();
            prop_assert!(fee <= amount * base / HUNDRED);
            prop_assert!(fee >= amount * cheap.min(base) / HUNDRED);
            prop_assert_eq!(d.promo, cheap < base && room > Decimal::ZERO);
        }

        /// Рычаг поверх ступени: чем больше резерва выплаты, тем не дороже, и не ниже нуля.
        #[test]
        fn levered_tier_is_monotonic(
            base in pct(), dir in pct(), a in 0i64..=10_000, b in 0i64..=10_000, t in 1i64..=5_000,
        ) {
            let l = |available: i64| LeverInput {
                k_pct: dec!(0.75),
                floor_pct: dir.min(dec!(1.5)),
                cap_pct: dir + dec!(1),
                available: Decimal::from(available),
                target: Decimal::from(t),
            };
            let fa = levered_pct(dir, base, Some(l(a))).unwrap();
            let fb = levered_pct(dir, base, Some(l(b))).unwrap();
            prop_assert!(fa >= Decimal::ZERO && fb >= Decimal::ZERO);
            if a <= b {
                prop_assert!(fa >= fb);
            }
        }
    }
}
