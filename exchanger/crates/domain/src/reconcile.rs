//! Сверка баланса кошелька с леджером, когда по кошельку висят операции с неясным исходом
//! (находка М-6, шаг 1.5 плана).
//!
//! Операция `unknown` могла выполниться, а могла нет; леджер её ещё не провёл. Поэтому
//! ожидаемых балансов несколько: леджер плюс изменения от любого сочетания таких операций
//! («эти выполнились, остальные — нет»). Реальный баланс сравнивается со всеми вариантами:
//! - сходится с каким-то (в пределах `warn`) — порядок; если вариант один — это подсказка
//!   оператору, какие операции, судя по балансу, выполнились;
//! - больше самого большого варианта — «денег больше» (как взнос владельца): предупреждение,
//!   без паузы;
//! - иначе — не хватает относительно ближайшего варианта сверху: до `pause` — предупреждение,
//!   больше — пауза выплат. Недостача при висящей неясной операции видна за один цикл.
//!
//! Без неясных операций вариант один — леджер, и правило совпадает с прежним.

use rust_decimal::Decimal;

/// Больше стольких неясных операций на кошелёк не перебираем (2^N вариантов): сверка
/// пропускается, а движок поднимет тревогу «сверка не проходит».
pub const MAX_UNSETTLED: usize = 8;

/// Итог сверки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Warn,
    Pause,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgement {
    pub outcome: Outcome,
    /// Ожидаемый баланс варианта, с которым сравнивали.
    pub reference: Decimal,
    /// Операции, которые, судя по балансу, выполнились (пусто — ни одна). `None` — подсказки
    /// нет: баланс не сходится ни с одним вариантом или сходится сразу с несколькими.
    pub matched: Option<Vec<i64>>,
}

/// `unsettled` — неясные операции: (id, на сколько изменится баланс, если операция
/// выполнилась: выплата — минус сумма, приход — плюс). Операции с нулевым изменением на
/// баланс не влияют и в подсказку не попадают. Больше [`MAX_UNSETTLED`] значимых операций —
/// `None`.
pub fn judge(
    ledger: Decimal,
    unsettled: &[(i64, Decimal)],
    actual: Decimal,
    warn: Decimal,
    pause: Decimal,
) -> Option<Judgement> {
    let ops: Vec<(i64, Decimal)> = unsettled
        .iter()
        .copied()
        .filter(|(_, d)| !d.is_zero())
        .collect();
    if ops.len() > MAX_UNSETTLED {
        return None;
    }
    // Все сочетания «эти выполнились»: бит i маски — операция i.
    let variants: Vec<(Decimal, u32)> = (0u32..1 << ops.len())
        .map(|mask| {
            let sum = ops
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, (_, d))| *d)
                .sum::<Decimal>();
            (ledger + sum, mask)
        })
        .collect();
    let ids = |mask: u32| -> Vec<i64> {
        ops.iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, (id, _))| *id)
            .collect()
    };

    let mut matches: Vec<(Decimal, u32)> = variants
        .iter()
        .copied()
        .filter(|(v, _)| (actual - *v).abs() <= warn)
        .collect();
    if !matches.is_empty() {
        // Эталон — ближайший вариант; при равенстве — где выполнилось меньше операций.
        matches.sort_by_key(|(v, mask)| ((actual - *v).abs(), mask.count_ones()));
        let (reference, mask) = matches[0];
        return Some(Judgement {
            outcome: Outcome::Ok,
            reference,
            matched: (matches.len() == 1).then(|| ids(mask)),
        });
    }

    let highest = variants.iter().map(|(v, _)| *v).max().unwrap_or(ledger);
    if actual > highest {
        // Денег больше любого варианта — пополнение или приход, которого леджер не видел.
        return Some(Judgement {
            outcome: Outcome::Warn,
            reference: highest,
            matched: None,
        });
    }
    // Не хватает: сравниваем с ближайшим вариантом сверху — самая маленькая недостача из
    // возможных. Если и она больше `pause`, деньги ушли при любом исходе неясных операций.
    let reference = variants
        .iter()
        .map(|(v, _)| *v)
        .filter(|v| *v > actual)
        .min()
        .unwrap_or(highest);
    let outcome = if reference - actual <= pause {
        Outcome::Warn
    } else {
        Outcome::Pause
    };
    Some(Judgement {
        outcome,
        reference,
        matched: None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    const WARN: Decimal = dec!(0.01);
    const PAUSE: Decimal = dec!(1.00);

    fn j(ledger: Decimal, ops: &[(i64, Decimal)], actual: Decimal) -> Judgement {
        judge(ledger, ops, actual, WARN, PAUSE).unwrap()
    }

    #[test]
    fn executed_payout_matches_its_variant() {
        let r = j(dec!(1000), &[(7, dec!(-97.50))], dec!(902.50));
        assert_eq!(r.outcome, Outcome::Ok);
        assert_eq!(r.matched, Some(vec![7]));
        assert_eq!(r.reference, dec!(902.50));
    }

    #[test]
    fn payout_that_did_not_go_matches_the_ledger() {
        let r = j(dec!(1000), &[(7, dec!(-97.50))], dec!(1000));
        assert_eq!(r.outcome, Outcome::Ok);
        assert_eq!(r.matched, Some(vec![]));
    }

    /// Аудит М-6: висит неясная выплата 97.50, а с кошелька пропали 12.00. Раньше сверка
    /// молчала; теперь баланс не сходится ни с одним вариантом — пауза за один цикл.
    #[test]
    fn missing_money_is_caught_while_a_payout_is_unknown() {
        let not_done = j(dec!(1000), &[(7, dec!(-97.50))], dec!(988));
        assert_eq!(not_done.outcome, Outcome::Pause);
        assert_eq!(not_done.reference, dec!(1000));
        assert_eq!(not_done.matched, None);
        let done = j(dec!(1000), &[(7, dec!(-97.50))], dec!(890.50));
        assert_eq!(done.outcome, Outcome::Pause);
        assert_eq!(done.reference, dec!(902.50));
    }

    #[test]
    fn a_small_gap_is_a_warning_and_extra_money_never_pauses() {
        assert_eq!(
            j(dec!(1000), &[(7, dec!(-97.50))], dec!(902)).outcome,
            Outcome::Warn
        );
        assert_eq!(
            j(dec!(1000), &[(7, dec!(-97.50))], dec!(1500)).outcome,
            Outcome::Warn
        );
    }

    /// Неясный приход (активация чека без ответа) — вариант «пришло» больше леджера.
    #[test]
    fn unknown_intake_counts_upwards() {
        let r = j(dec!(1000), &[(3, dec!(100))], dec!(1100));
        assert_eq!((r.outcome, r.matched), (Outcome::Ok, Some(vec![3])));
    }

    /// Две неясные выплаты по 10.00: баланс −10 сходится с двумя сочетаниями — порядок, но
    /// подсказки нет (неизвестно, какая из двух).
    #[test]
    fn two_equal_payouts_give_no_hint() {
        let r = j(dec!(100), &[(1, dec!(-10)), (2, dec!(-10))], dec!(90));
        assert_eq!((r.outcome, r.matched), (Outcome::Ok, None));
        let both = j(dec!(100), &[(1, dec!(-10)), (2, dec!(-10))], dec!(80));
        assert_eq!(both.matched, Some(vec![1, 2]));
    }

    #[test]
    fn zero_change_operations_do_not_count() {
        let r = j(dec!(100), &[(1, dec!(0)), (2, dec!(-10))], dec!(90));
        assert_eq!(r.matched, Some(vec![2]));
    }

    #[test]
    fn too_many_operations_are_not_brute_forced() {
        let ops: Vec<_> = (0..=MAX_UNSETTLED as i64).map(|i| (i, dec!(-1))).collect();
        assert!(judge(dec!(100), &ops, dec!(100), WARN, PAUSE).is_none());
    }

    fn cents() -> impl Strategy<Value = Decimal> {
        (-1_000_000i64..1_000_000).prop_map(|c| Decimal::new(c, 2))
    }

    /// Прежнее правило — до шага 1.5: без неясных операций оно не меняется.
    fn old_rule(expected: Decimal, actual: Decimal) -> Outcome {
        let d = (actual - expected).abs();
        if d <= WARN {
            Outcome::Ok
        } else if d <= PAUSE || actual > expected {
            Outcome::Warn
        } else {
            Outcome::Pause
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(5_000))]

        #[test]
        fn without_unknown_ops_the_rule_is_unchanged(l in cents(), a in cents()) {
            let r = j(l, &[], a);
            prop_assert_eq!(r.outcome, old_rule(l, a));
            prop_assert_eq!(r.reference, l);
        }

        /// Баланс ровно равен одному из вариантов — всегда порядок; эталон — этот вариант.
        #[test]
        fn every_variant_is_ok(l in cents(), deltas in prop::collection::vec(cents(), 0..5), mask in 0u32..32) {
            let ops: Vec<(i64, Decimal)> = deltas.iter().enumerate().map(|(i, d)| (i as i64, *d)).collect();
            let v = l + ops.iter().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, d)| *d).sum::<Decimal>();
            let r = j(l, &ops, v);
            prop_assert_eq!(r.outcome, Outcome::Ok);
            if let Some(m) = &r.matched {
                // Подсказка, если есть, указывает на сочетание с тем же балансом.
                let mv = l + ops.iter().filter(|(i, _)| m.contains(i)).map(|(_, d)| *d).sum::<Decimal>();
                prop_assert!((mv - v).abs() <= WARN);
            }
        }

        /// Меньше самого маленького варианта больше чем на `pause` — всегда пауза.
        #[test]
        fn below_every_variant_pauses(l in cents(), deltas in prop::collection::vec(cents(), 0..5), gap in 101i64..100_000) {
            let ops: Vec<(i64, Decimal)> = deltas.iter().enumerate().map(|(i, d)| (i as i64, *d)).collect();
            let lowest = l + deltas.iter().filter(|d| d.is_sign_negative()).copied().sum::<Decimal>();
            let r = j(l, &ops, lowest - Decimal::new(gap, 2));
            prop_assert_eq!(r.outcome, Outcome::Pause);
        }

        /// Денег больше любого варианта — предупреждение, никогда не пауза.
        #[test]
        fn above_every_variant_never_pauses(l in cents(), deltas in prop::collection::vec(cents(), 0..5), gap in 2i64..100_000) {
            let ops: Vec<(i64, Decimal)> = deltas.iter().enumerate().map(|(i, d)| (i as i64, *d)).collect();
            let highest = l + deltas.iter().filter(|d| d.is_sign_positive()).copied().sum::<Decimal>();
            let r = j(l, &ops, highest + Decimal::new(gap, 2));
            prop_assert_eq!(r.outcome, Outcome::Warn);
        }
    }
}
