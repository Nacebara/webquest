//! Суммы из текстов ботов кошельков: `100 USDT`, `1 234,56 USDT`, `0.5 TON`, `10 TONCOIN`,
//! `$5.00` (фиат игнорируем). Только `Decimal`, без промежуточных `f64`.

use domain::{Asset, Decimal, Platform};

/// Первая сумма с кодом поддерживаемого актива в тексте.
pub fn parse_money(text: &str, platform: Platform) -> Option<(Decimal, Asset)> {
    let _ = (text, platform);
    None
}
