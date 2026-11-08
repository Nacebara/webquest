//! Почему направление сейчас принимает или не принимает заявки — одна правда для пульта и
//! клиента. Те же проверки, что у приёма чека ([`crate::submit`]): обслуживание, стоп-кран,
//! направление открыто и не на автопаузе, кошельки обеих сторон подключены и не на паузе,
//! на стороне выплаты хватает резерва хотя бы на минимальную заявку (с запасом и «лимитом
//! без код-пароля» CryptoBot).

use domain::pricing::{Limits, max_order_effective};
use domain::{Asset, Decimal, Direction, Platform};

use crate::{Engine, EngineError};

/// Что мешает направлению принимать заявки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// Владелец закрыл приём.
    Closed,
    /// Автопауза (сбой кошелька, незнакомый ответ, недостача по сверке) — причина из базы.
    AutoPaused { reason: Option<String> },
    /// Режим обслуживания.
    Maintenance,
    /// Стоп-кран: выплаты остановлены.
    KillSwitch,
    /// Кошелёк платформы не подключён (нет входа юзербота).
    WalletMissing { platform: Platform },
    /// Аккаунт юзербота этого кошелька на паузе (пульт).
    UserbotPaused { platform: Platform },
    /// На кошельке выплаты свободно меньше минимальной заявки.
    NoReserve {
        platform: Platform,
        available: Decimal,
        needed: Decimal,
    },
}

/// Состояние направления сейчас.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    pub direction: Direction,
    /// Пусто — принимает.
    pub blockers: Vec<Blocker>,
    /// Сколько можно принять одной заявкой (общие условия, без личных и суточных лимитов).
    pub max_now: Option<Decimal>,
}

impl Availability {
    pub fn accepting(&self) -> bool {
        self.blockers.is_empty()
    }
}

impl Engine {
    /// Состояние всех направлений.
    pub async fn availability(&self) -> Result<Vec<Availability>, EngineError> {
        let mut conn = self.pool.acquire().await?;
        let maintenance = storage::settings::flag_on(&mut conn, "maintenance").await?;
        let kill = storage::settings::flag_on(&mut conn, "kill_switch_payouts").await?;
        let mut out = Vec::new();
        for direction in Direction::ALL {
            let row = storage::directions::get(&mut conn, direction).await?;
            let mut blockers = Vec::new();
            if !row.is_open {
                blockers.push(Blocker::Closed);
            }
            if row.auto_paused {
                blockers.push(Blocker::AutoPaused {
                    reason: row.pause_reason.clone(),
                });
            }
            if maintenance {
                blockers.push(Blocker::Maintenance);
            }
            if kill {
                blockers.push(Blocker::KillSwitch);
            }
            let mut target_wallet = None;
            for platform in [direction.source(), direction.target()] {
                match storage::wallets::active(&mut conn, platform).await? {
                    None => {
                        let b = Blocker::WalletMissing { platform };
                        if !blockers.contains(&b) {
                            blockers.push(b);
                        }
                    }
                    Some((wallet, _)) => {
                        if storage::wallets::userbot_paused(&mut conn, wallet).await? {
                            let b = Blocker::UserbotPaused { platform };
                            if !blockers.contains(&b) {
                                blockers.push(b);
                            }
                        }
                        if platform == direction.target() {
                            target_wallet = Some(wallet);
                        }
                    }
                }
            }
            let mut max_now = None;
            if let Some(wallet) = target_wallet {
                let target = direction.target();
                let spendable =
                    crate::submit::spendable(&mut conn, &self.cfg, target, wallet, Asset::Usdt)
                        .await?;
                let limits = Limits {
                    min_amount: row.min_amount,
                    max_amount: row.max_amount,
                };
                max_now = max_order_effective(limits, spendable, self.cfg.reserve_safety);
                if max_now.is_none() {
                    blockers.push(Blocker::NoReserve {
                        platform: target,
                        available: (spendable - self.cfg.reserve_safety).max(Decimal::ZERO),
                        needed: row.min_amount,
                    });
                }
            }
            out.push(Availability {
                direction,
                blockers,
                max_now,
            });
        }
        Ok(out)
    }
}
