//! Шаблоны проводок двойной записи (SPEC §10.4, v0.2). Знак: дебет > 0, кредит < 0.
//!
//! Кошелёк сервиса на платформе один — личный баланс аккаунта юзербота (`asset:cb:USDT`,
//! `asset:xr:USDT`). Выплата и возврат — чек, который юзербот создаёт в боте кошелька:
//! деньги списываются с нашего баланса в момент создания чека, тогда же делаем проводку.
//! Комиссия признаётся в момент выплаты; возврат — сторно `payable` без отмены дохода.

use std::collections::BTreeMap;
use std::fmt;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::asset::{Asset, Platform, wallet_label};

/// Код счёта леджера, как в `ledger_accounts.code`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccountCode(String);

impl AccountCode {
    /// Баланс аккаунта юзербота на платформе: `asset:cb:USDT`.
    pub fn wallet(platform: Platform, asset: Asset) -> Self {
        Self(format!("asset:{}:{}", wallet_label(platform), asset.code()))
    }

    /// Деньги в пути между платформами при ребалансировке.
    pub fn transit(asset: Asset) -> Self {
        Self(format!("asset:transit:{}", asset.code()))
    }

    /// Деньги клиентов в незавершённых заявках.
    pub fn payable(asset: Asset) -> Self {
        Self(format!("liability:payable:{}", asset.code()))
    }

    pub fn revenue(kind: Revenue, asset: Asset) -> Self {
        Self(format!("revenue:{}:{}", kind.as_str(), asset.code()))
    }

    pub fn expense(kind: Expense, asset: Asset) -> Self {
        Self(format!("expense:{}:{}", kind.as_str(), asset.code()))
    }

    pub fn equity(kind: Equity, asset: Asset) -> Self {
        Self(format!("equity:{}:{}", kind.as_str(), asset.code()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Тип счёта для `ledger_accounts.type`.
    pub fn account_type(&self) -> &str {
        self.0.split(':').next().unwrap_or_default()
    }
}

impl fmt::Display for AccountCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revenue {
    Fee,
    Excess,
    FxBuffer,
}

impl Revenue {
    const fn as_str(self) -> &'static str {
        match self {
            Revenue::Fee => "fee",
            Revenue::Excess => "excess",
            Revenue::FxBuffer => "fx_buffer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expense {
    NetworkFee,
    PlatformFee,
    Loss,
    Referral,
}

impl Expense {
    const fn as_str(self) -> &'static str {
        match self {
            Expense::NetworkFee => "network_fee",
            Expense::PlatformFee => "platform_fee",
            Expense::Loss => "loss",
            Expense::Referral => "referral",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Equity {
    Capital,
    OwnerWithdrawals,
}

impl Equity {
    const fn as_str(self) -> &'static str {
        match self {
            Equity::Capital => "capital",
            Equity::OwnerWithdrawals => "owner_withdrawals",
        }
    }
}

/// Вид проводки — значение `ledger_transactions.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TxKind {
    Intake,
    Payout,
    Refund,
    ExcessRefund,
    ExcessKept,
    InvoicePaid,
    CapitalIn,
    ManualClose,
}

impl TxKind {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            TxKind::Intake => "intake",
            TxKind::Payout => "payout",
            TxKind::Refund => "refund",
            TxKind::ExcessRefund => "excess_refund",
            TxKind::ExcessKept => "excess_kept",
            TxKind::InvoicePaid => "invoice_paid",
            TxKind::CapitalIn => "capital_in",
            TxKind::ManualClose => "manual_close",
        }
    }
}

/// Строка проводки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub account: AccountCode,
    pub asset: Asset,
    pub amount: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PostingError {
    #[error("posting must have at least two lines")]
    TooFewLines,
    #[error("posting line for {0} has zero amount")]
    ZeroLine(AccountCode),
    #[error("posting is unbalanced for {asset}: {sum}")]
    Unbalanced { asset: Asset, sum: Decimal },
    #[error("amount must be positive: {0}")]
    NonPositive(Decimal),
    #[error("parts do not add up: {0}")]
    Mismatch(&'static str),
}

/// Сбалансированная проводка. Создаётся только через шаблоны или [`Posting::new`] с проверкой —
/// ту же проверку повторяет триггер `ledger_entries_balanced` в БД.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    kind: TxKind,
    lines: Vec<Line>,
}

impl Posting {
    pub fn new(kind: TxKind, lines: Vec<Line>) -> Result<Self, PostingError> {
        if lines.len() < 2 {
            return Err(PostingError::TooFewLines);
        }
        let mut sums: BTreeMap<Asset, Decimal> = BTreeMap::new();
        for line in &lines {
            if line.amount.is_zero() {
                return Err(PostingError::ZeroLine(line.account.clone()));
            }
            *sums.entry(line.asset).or_default() += line.amount;
        }
        if let Some((asset, sum)) = sums.into_iter().find(|(_, s)| !s.is_zero()) {
            return Err(PostingError::Unbalanced { asset, sum });
        }
        Ok(Self { kind, lines })
    }

    pub const fn kind(&self) -> TxKind {
        self.kind
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Сумма по счёту в этой проводке (для проверок и сверки).
    pub fn net(&self, account: &AccountCode) -> Decimal {
        self.lines
            .iter()
            .filter(|l| &l.account == account)
            .map(|l| l.amount)
            .sum()
    }
}

fn positive(x: Decimal) -> Result<Decimal, PostingError> {
    if x > Decimal::ZERO {
        Ok(x)
    } else {
        Err(PostingError::NonPositive(x))
    }
}

fn line(account: AccountCode, asset: Asset, amount: Decimal) -> Line {
    Line {
        account,
        asset,
        amount,
    }
}

/// Чек клиента активирован: Дт кошелёк платформы входа / Кт payable на всю сумму.
pub fn intake(platform: Platform, asset: Asset, amount: Decimal) -> Result<Posting, PostingError> {
    let a = positive(amount)?;
    Posting::new(
        TxKind::Intake,
        vec![
            line(AccountCode::wallet(platform, asset), asset, a),
            line(AccountCode::payable(asset), asset, -a),
        ],
    )
}

/// Чек выплаты создан: Дт payable (всё принятое) / Кт кошелёк выплаты / Кт revenue:fee.
pub fn payout(
    platform: Platform,
    asset: Asset,
    amount_in: Decimal,
    payout: Decimal,
    fee: Decimal,
) -> Result<Posting, PostingError> {
    let a = positive(amount_in)?;
    let p = positive(payout)?;
    if fee.is_sign_negative() || p + fee != a {
        return Err(PostingError::Mismatch("payout + fee must equal amount_in"));
    }
    let mut lines = vec![
        line(AccountCode::payable(asset), asset, a),
        line(AccountCode::wallet(platform, asset), asset, -p),
    ];
    if !fee.is_zero() {
        lines.push(line(AccountCode::revenue(Revenue::Fee, asset), asset, -fee));
    }
    Posting::new(TxKind::Payout, lines)
}

/// Чужой счёт оплачен: Дт payable X / Кт кошелёк платформы счёта A / Кт revenue:fee X−A.
pub fn invoice_paid(
    platform: Platform,
    asset: Asset,
    client_paid: Decimal,
    invoice_amount: Decimal,
) -> Result<Posting, PostingError> {
    let x = positive(client_paid)?;
    let a = positive(invoice_amount)?;
    if a > x {
        return Err(PostingError::Mismatch(
            "invoice amount exceeds client payment",
        ));
    }
    let mut lines = vec![
        line(AccountCode::payable(asset), asset, x),
        line(AccountCode::wallet(platform, asset), asset, -a),
    ];
    if x > a {
        lines.push(line(
            AccountCode::revenue(Revenue::Fee, asset),
            asset,
            a - x,
        ));
    }
    Posting::new(TxKind::InvoicePaid, lines)
}

/// Чек возврата создан на платформе входа: Дт payable / Кт кошелёк.
pub fn refund(platform: Platform, asset: Asset, amount: Decimal) -> Result<Posting, PostingError> {
    refund_kind(TxKind::Refund, platform, asset, amount)
}

/// Возврат переплаты: то же, но отдельный вид, чтобы не путать с возвратом всей заявки.
pub fn excess_refund(
    platform: Platform,
    asset: Asset,
    amount: Decimal,
) -> Result<Posting, PostingError> {
    refund_kind(TxKind::ExcessRefund, platform, asset, amount)
}

fn refund_kind(
    kind: TxKind,
    platform: Platform,
    asset: Asset,
    amount: Decimal,
) -> Result<Posting, PostingError> {
    let a = positive(amount)?;
    Posting::new(
        kind,
        vec![
            line(AccountCode::payable(asset), asset, a),
            line(AccountCode::wallet(platform, asset), asset, -a),
        ],
    )
}

/// Мелкая переплата оставлена в доходе: Дт payable / Кт revenue:excess.
pub fn excess_kept(asset: Asset, amount: Decimal) -> Result<Posting, PostingError> {
    let a = positive(amount)?;
    Posting::new(
        TxKind::ExcessKept,
        vec![
            line(AccountCode::payable(asset), asset, a),
            line(AccountCode::revenue(Revenue::Excess, asset), asset, -a),
        ],
    )
}

/// Взнос оборотного капитала: Дт кошелёк / Кт equity:capital.
pub fn capital_in(
    platform: Platform,
    asset: Asset,
    amount: Decimal,
) -> Result<Posting, PostingError> {
    let a = positive(amount)?;
    Posting::new(
        TxKind::CapitalIn,
        vec![
            line(AccountCode::wallet(platform, asset), asset, a),
            line(AccountCode::equity(Equity::Capital, asset), asset, -a),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    const CB: Platform = Platform::CryptoBot;
    const XR: Platform = Platform::XRocket;

    #[test]
    fn account_codes_match_schema_conventions() {
        assert_eq!(
            AccountCode::wallet(CB, Asset::Usdt).as_str(),
            "asset:cb:USDT"
        );
        assert_eq!(AccountCode::wallet(XR, Asset::Ton).as_str(), "asset:xr:TON");
        assert_eq!(
            AccountCode::payable(Asset::Ton).as_str(),
            "liability:payable:TON"
        );
        assert_eq!(
            AccountCode::revenue(Revenue::Fee, Asset::Usdt).account_type(),
            "revenue"
        );
        assert_eq!(
            AccountCode::transit(Asset::Usdt).as_str(),
            "asset:transit:USDT"
        );
    }

    /// Основной поток v0.2: чек CryptoBot 100 USDT → чек xRocket 97,50 USDT, комиссия 2,50.
    #[test]
    fn main_flow_cryptobot_to_xrocket() {
        let i = intake(CB, Asset::Usdt, dec!(100)).unwrap();
        let p = payout(XR, Asset::Usdt, dec!(100), dec!(97.50), dec!(2.50)).unwrap();
        let payable = AccountCode::payable(Asset::Usdt);
        assert_eq!(
            i.net(&payable) + p.net(&payable),
            Decimal::ZERO,
            "payable closes to zero"
        );
        assert_eq!(
            p.net(&AccountCode::revenue(Revenue::Fee, Asset::Usdt)),
            dec!(-2.50)
        );
        assert_eq!(p.net(&AccountCode::wallet(XR, Asset::Usdt)), dec!(-97.50));
        assert_eq!(i.net(&AccountCode::wallet(CB, Asset::Usdt)), dec!(100));
    }

    #[test]
    fn invoice_payment_from_xrocket_funds() {
        let i = intake(XR, Asset::Usdt, dec!(10.30)).unwrap();
        let p = invoice_paid(CB, Asset::Usdt, dec!(10.30), dec!(10)).unwrap();
        let payable = AccountCode::payable(Asset::Usdt);
        assert_eq!(i.net(&payable) + p.net(&payable), Decimal::ZERO);
        assert_eq!(
            p.net(&AccountCode::revenue(Revenue::Fee, Asset::Usdt)),
            dec!(-0.30)
        );
    }

    #[test]
    fn refund_reverses_intake_without_touching_revenue() {
        let i = intake(CB, Asset::Usdt, dec!(100)).unwrap();
        let r = refund(CB, Asset::Usdt, dec!(100)).unwrap();
        let payable = AccountCode::payable(Asset::Usdt);
        assert_eq!(i.net(&payable) + r.net(&payable), Decimal::ZERO);
        assert!(
            r.lines()
                .iter()
                .all(|l| l.account.account_type() != "revenue")
        );
    }

    #[test]
    fn invalid_postings_are_rejected() {
        let a = AccountCode::payable(Asset::Usdt);
        let b = AccountCode::wallet(XR, Asset::Usdt);
        assert_eq!(
            Posting::new(TxKind::Intake, vec![line(a.clone(), Asset::Usdt, dec!(1))]),
            Err(PostingError::TooFewLines)
        );
        assert!(matches!(
            Posting::new(
                TxKind::Intake,
                vec![
                    line(a.clone(), Asset::Usdt, dec!(1)),
                    line(b.clone(), Asset::Usdt, dec!(-0.99))
                ]
            ),
            Err(PostingError::Unbalanced { .. })
        ));
        assert!(matches!(
            Posting::new(
                TxKind::Intake,
                vec![line(a, Asset::Usdt, dec!(1)), line(b, Asset::Ton, dec!(-1))]
            ),
            Err(PostingError::Unbalanced { .. })
        ));
        assert!(payout(XR, Asset::Usdt, dec!(100), dec!(97.5), dec!(2.4)).is_err());
        assert!(intake(CB, Asset::Usdt, dec!(0)).is_err());
    }

    fn amt() -> impl Strategy<Value = Decimal> {
        (1i64..=1_000_000_000_000, 0u32..=8).prop_map(|(m, s)| Decimal::new(m, s))
    }

    fn sum_by_asset(p: &Posting) -> BTreeMap<Asset, Decimal> {
        let mut m = BTreeMap::new();
        for l in p.lines() {
            *m.entry(l.asset).or_insert(Decimal::ZERO) += l.amount;
        }
        m
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        #[test]
        fn templates_are_always_balanced(a in amt(), b in amt(), pi in 0usize..2) {
            let p = Platform::ALL[pi];
            let (big, small) = if a >= b { (a, b) } else { (b, a) };
            let postings = [
                intake(p, Asset::Usdt, a),
                refund(p, Asset::Usdt, a),
                excess_refund(p, Asset::Usdt, a),
                excess_kept(Asset::Usdt, a),
                capital_in(p, Asset::Usdt, a),
                invoice_paid(p, Asset::Usdt, big, small),
                payout(p, Asset::Usdt, big + small, big, small),
            ];
            for posting in postings {
                let posting = posting.unwrap();
                prop_assert!(sum_by_asset(&posting).values().all(|s| s.is_zero()));
                prop_assert!(posting.lines().iter().all(|l| !l.amount.is_zero()));
            }
        }

        #[test]
        fn exchange_lifecycle_closes_payable(amount_in in amt(), fee_part in 0i64..=1_000) {
            let fee = (amount_in * Decimal::new(fee_part, 4)).round_dp(8);
            prop_assume!(fee < amount_in);
            let i = intake(CB, Asset::Usdt, amount_in).unwrap();
            let p = payout(XR, Asset::Usdt, amount_in, amount_in - fee, fee).unwrap();
            let payable = AccountCode::payable(Asset::Usdt);
            prop_assert_eq!(i.net(&payable) + p.net(&payable), Decimal::ZERO);
        }
    }
}
