//! Комиссия заявки и встречный поток (DESIGN-v0.2 Р15).
//!
//! Одна функция [`fee_decision`] для обмена (при зачислении чека) и для оплаты счёта (при
//! котировке): личная ставка → ступень → рычаг резерва → выгодный курс → минимальная комиссия
//! (`domain::fees::decide`).
//!
//! **Встречный поток.** Направление, которое везёт деньги на кошелёк с нехваткой резерва и
//! забирает их с кошелька с излишком, может идти по выгодному курсу — это бесплатная для нас
//! ребалансировка вместо вывода через сеть. Курс действует только на объём, который приближает
//! оба кошелька к цели (`domain::fees::rebalance_room`). Настройки — `settings.rebalance_promo`:
//! - `off` — выгодного курса нет;
//! - `hint` — включает и выключает человек; пульт подсказывает, когда пора ([`Engine::promo_tick`]);
//! - `auto` — система включает при нехватке `≥ trigger_pct` цели и выключает при `≤ release_pct`.
//!
//! Где курс включён — `settings.rebalance_promo_active`. Включённый вручную сам выключается,
//! когда резерв выровнялся; выключенный вручную в режиме `auto` не включится снова, пока перекос
//! не пройдёт.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use domain::fees::{
    FeeDecision, FeeInputs, LeverInput, Promo, PromoThresholds, Reserve, rebalance_room,
};
use domain::{Asset, Decimal, Direction, Platform};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use storage::alerts::{Level, NewAlert, Topic};
use storage::user_terms::Effective;

use crate::{Engine, EngineError};

pub const PROMO_SETTINGS: &str = "rebalance_promo";
pub const PROMO_ACTIVE: &str = "rebalance_promo_active";
/// Рассылка «🔥 Выгодно» клиентам: `{"on": true}` — включена (включает владелец).
pub const PROMO_BROADCAST: &str = "promo_broadcast";

/// Выгодный курс включился: если рассылка включена — завести её в журнале (отправит клиентский
/// бот). Уже идёт другая — новой не будет. Решение владельца 9 ноября 2026.
pub async fn announce_promo(
    conn: &mut PgConnection,
    direction: Direction,
) -> Result<Option<i64>, EngineError> {
    if !storage::settings::flag(&mut *conn, PROMO_BROADCAST, false).await? {
        return Ok(None);
    }
    let id = storage::promo::start(&mut *conn, direction.code()).await?;
    if let Some(id) = id {
        tracing::info!(
            direction = direction.code(),
            broadcast = id,
            "promo broadcast queued"
        );
    }
    Ok(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromoMode {
    Off,
    Hint,
    Auto,
}

/// Настройки встречного потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoSettings {
    pub mode: PromoMode,
    #[serde(with = "rust_decimal::serde::str")]
    pub fee_pct: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub min_fee: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub trigger_pct: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub release_pct: Decimal,
}

impl Default for PromoSettings {
    fn default() -> Self {
        Self {
            mode: PromoMode::Hint,
            fee_pct: Decimal::new(3, 1),
            min_fee: Decimal::new(5, 2),
            trigger_pct: Decimal::from(30),
            release_pct: Decimal::from(10),
        }
    }
}

impl PromoSettings {
    pub fn thresholds(&self) -> PromoThresholds {
        PromoThresholds {
            trigger_pct: self.trigger_pct,
            release_pct: self.release_pct,
        }
    }

    /// Проверка перед сохранением (пульт).
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.fee_pct.is_sign_negative() || self.fee_pct >= domain::fees::MAX_FEE_PCT {
            return Err("fee_pct");
        }
        if self.min_fee.is_sign_negative() {
            return Err("min_fee");
        }
        self.thresholds().validate().map_err(|_| "thresholds")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchedBy {
    Auto,
    Manual,
}

/// Состояние выгодного курса на направлении.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoState {
    pub on: bool,
    pub by: SwitchedBy,
    pub since: DateTime<Utc>,
    #[serde(default)]
    pub staff: Option<i64>,
}

pub type PromoStates = BTreeMap<String, PromoState>;

/// Настройки; нет или испорчены — по умолчанию (с предупреждением: испорченное поправит пульт).
pub async fn promo_settings(conn: &mut PgConnection) -> Result<PromoSettings, EngineError> {
    let Some(value) = storage::settings::get(conn, PROMO_SETTINGS).await? else {
        return Ok(PromoSettings::default());
    };
    match serde_json::from_value::<PromoSettings>(value) {
        Ok(s) if s.validate().is_ok() => Ok(s),
        _ => {
            tracing::warn!("settings.rebalance_promo is invalid, using defaults");
            Ok(PromoSettings::default())
        }
    }
}

pub async fn save_promo_settings(
    conn: &mut PgConnection,
    s: &PromoSettings,
    staff: i64,
) -> Result<(), EngineError> {
    let value = serde_json::to_value(s)
        .map_err(|e| EngineError::Invariant(format!("promo settings: {e}")))?;
    storage::settings::set(conn, PROMO_SETTINGS, &value, Some(staff)).await?;
    Ok(())
}

pub async fn promo_states(conn: &mut PgConnection) -> Result<PromoStates, EngineError> {
    Ok(storage::settings::get(conn, PROMO_ACTIVE)
        .await?
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

async fn save_states(
    conn: &mut PgConnection,
    states: &PromoStates,
    staff: Option<i64>,
) -> Result<(), EngineError> {
    let value = serde_json::to_value(states)
        .map_err(|e| EngineError::Invariant(format!("promo states: {e}")))?;
    storage::settings::set(conn, PROMO_ACTIVE, &value, staff).await?;
    Ok(())
}

/// Включить или выключить выгодный курс на направлении вручную (пульт).
pub async fn switch_promo(
    conn: &mut PgConnection,
    direction: Direction,
    on: bool,
    staff: i64,
) -> Result<(), EngineError> {
    let mut states = promo_states(&mut *conn).await?;
    states.insert(
        direction.code().to_owned(),
        PromoState {
            on,
            by: SwitchedBy::Manual,
            since: Utc::now(),
            staff: Some(staff),
        },
    );
    save_states(conn, &states, Some(staff)).await
}

/// Резерв активного кошелька платформы: свободно и цель (без цели — `None`).
pub async fn reserve_of(
    conn: &mut PgConnection,
    platform: Platform,
    asset: Asset,
) -> Result<Option<Reserve>, EngineError> {
    let Some((wallet, _)) = storage::wallets::active(&mut *conn, platform).await? else {
        return Ok(None);
    };
    let Some(target) = storage::directions::reserve_target(&mut *conn, wallet, asset).await? else {
        return Ok(None);
    };
    let available = storage::holds::available(&mut *conn, wallet, asset).await?;
    Ok(Some(Reserve { available, target }))
}

/// Резервы направления (вход, выплата) и объём встречного потока. `source_received` — сколько
/// уже зачислено на кошелёк входа по самой заявке, `own_hold` — удержано под неё на кошельке
/// выплаты: объём считаем как до заявки.
pub async fn direction_reserves(
    conn: &mut PgConnection,
    direction: Direction,
    asset: Asset,
    source_received: Decimal,
    own_hold: Decimal,
) -> Result<(Option<Reserve>, Option<Reserve>, Decimal), EngineError> {
    let source = reserve_of(&mut *conn, direction.source(), asset)
        .await?
        .map(|r| Reserve {
            available: r.available - source_received,
            ..r
        });
    let target = reserve_of(&mut *conn, direction.target(), asset)
        .await?
        .map(|r| Reserve {
            available: r.available + own_hold,
            ..r
        });
    Ok((source, target, rebalance_room(source, target)))
}

/// Выгодный курс, который действует на направлении сейчас (с объёмом); нет — `None`.
pub async fn active_promo(
    conn: &mut PgConnection,
    direction: Direction,
    asset: Asset,
    source_received: Decimal,
    own_hold: Decimal,
) -> Result<Option<Promo>, EngineError> {
    let settings = promo_settings(&mut *conn).await?;
    if settings.mode == PromoMode::Off {
        return Ok(None);
    }
    let on = promo_states(&mut *conn)
        .await?
        .get(direction.code())
        .is_some_and(|s| s.on);
    if !on {
        return Ok(None);
    }
    let (_, _, room) =
        direction_reserves(&mut *conn, direction, asset, source_received, own_hold).await?;
    Ok((room > Decimal::ZERO).then_some(Promo {
        fee_pct: settings.fee_pct,
        min_fee: settings.min_fee,
        room,
    }))
}

/// Комиссия заявки на сумму `amount`. `wallet` — кошелёк выплаты.
pub async fn fee_decision(
    conn: &mut PgConnection,
    eff: &Effective,
    wallet: i16,
    asset: Asset,
    amount: Decimal,
    source_received: Decimal,
    own_hold: Decimal,
) -> Result<FeeDecision, EngineError> {
    let dir = &eff.dir;
    let lever = match storage::directions::reserve_target(&mut *conn, wallet, asset).await? {
        Some(target) => Some(LeverInput {
            k_pct: dir.skew_k_pct,
            floor_pct: dir.fee_floor_pct,
            cap_pct: dir.fee_cap_pct,
            available: storage::holds::available(&mut *conn, wallet, asset).await? + own_hold,
            target,
        }),
        None => None,
    };
    let promo = active_promo(&mut *conn, dir.direction, asset, source_received, own_hold).await?;
    let inputs = FeeInputs {
        direction_pct: dir.fee_pct,
        min_fee: dir.min_fee,
        mode: dir.fee_mode,
        schedule: dir.fee_tiers.as_ref(),
        personal_pct: eff.personal.fee_pct,
        lever,
        promo,
    };
    Ok(domain::fees::decide(amount, &inputs)?)
}

/// Что поменял проход автоматики (для логов и тестов).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromoEvent {
    Started { direction: Direction, room: Decimal },
    Stopped { direction: Direction },
    Hint { direction: Direction, room: Decimal },
}

impl Engine {
    /// Проход автоматики встречного потока: подсказки, включение и выключение по порогам.
    /// Зовётся из [`Engine::run`] раз в `housekeeping_every`.
    pub async fn promo_tick(&self) -> Result<Vec<PromoEvent>, EngineError> {
        let asset = Asset::Usdt;
        let mut tx = self.pool.begin().await?;
        let settings = promo_settings(&mut tx).await?;
        let th = settings.thresholds();
        let mut states = promo_states(&mut tx).await?;
        let mut events = Vec::new();
        let mut changed = false;
        for direction in Direction::ALL {
            let code = direction.code();
            let (source, target, room) =
                direction_reserves(&mut tx, direction, asset, Decimal::ZERO, Decimal::ZERO).await?;
            let (Some(s), Some(t)) = (source, target) else {
                continue;
            };
            let state = states.get(code).copied();
            let on = state.is_some_and(|st| st.on);
            let hint_key = format!("promo_hint:{code}");
            let payload = json!({
                "direction": code,
                "room": room.to_string(),
                "fee_pct": settings.fee_pct.to_string(),
                "source_available": s.available.to_string(),
                "source_target": s.target.to_string(),
                "target_available": t.available.to_string(),
                "target_target": t.target.to_string(),
                "deficit_pct": s.deficit_pct().map(|d| d.round_dp(1).to_string()),
            });
            let next = match settings.mode {
                PromoMode::Off => on,
                // Подсказка и ручное включение: выключаем только когда выровнялось.
                PromoMode::Hint => {
                    if !on && th.should_start(s, t) {
                        storage::alerts::raise(
                            &mut tx,
                            &NewAlert {
                                level: Level::Notice,
                                topic: Topic::Reserves,
                                dedup_key: &hint_key,
                                title: "promo_hint",
                                payload: payload.clone(),
                            },
                        )
                        .await?;
                        events.push(PromoEvent::Hint { direction, room });
                    } else if !th.should_start(s, t) {
                        storage::alerts::resolve(&mut tx, &hint_key).await?;
                    }
                    on && th.should_keep(s, t)
                }
                PromoMode::Auto => {
                    storage::alerts::resolve(&mut tx, &hint_key).await?;
                    match state {
                        // Выключили руками: ждём, пока перекос пройдёт, потом снова автоматика.
                        Some(st) if st.by == SwitchedBy::Manual && !st.on => {
                            if !th.should_keep(s, t) {
                                states.remove(code);
                                changed = true;
                            }
                            false
                        }
                        _ => th.next_state(on, s, t),
                    }
                }
            };
            if next != on {
                states.insert(
                    code.to_owned(),
                    PromoState {
                        on: next,
                        by: SwitchedBy::Auto,
                        since: Utc::now(),
                        staff: None,
                    },
                );
                changed = true;
                let key = format!("promo_switched:{code}:{}", Utc::now().timestamp());
                storage::alerts::raise(
                    &mut tx,
                    &NewAlert {
                        level: Level::Notice,
                        topic: Topic::Reserves,
                        dedup_key: &key,
                        title: if next {
                            "promo_started"
                        } else {
                            "promo_stopped"
                        },
                        payload,
                    },
                )
                .await?;
                if next {
                    announce_promo(&mut tx, direction).await?;
                }
                events.push(if next {
                    PromoEvent::Started { direction, room }
                } else {
                    PromoEvent::Stopped { direction }
                });
                tracing::info!(direction = code, on = next, %room, "rebalance promo switched");
            }
        }
        if changed {
            save_states(&mut tx, &states, None).await?;
        }
        tx.commit().await?;
        Ok(events)
    }
}
