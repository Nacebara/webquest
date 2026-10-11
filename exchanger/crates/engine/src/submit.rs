//! Приём чека клиента (SPEC §4, П1–П2): проверки → заявка → удержание резерва целевого
//! кошелька → операция активации. Всё одной транзакцией: если что-то не прошло, нет ни
//! заявки в работе, ни удержания, ни операции.

use domain::fsm::Event;
use domain::pricing::{Limits, max_order_effective};
use domain::{Asset, Direction, Platform};
use parsers::{LinkKind, WalletLink};
use storage::StorageError;
use storage::orders::{NewOrder, OrderPatch, OrderRow};

use crate::effects::{EffectCtx, transit};
use crate::{Engine, EngineError};

/// Почему чек не взят в работу. Деньги клиента остались в чеке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitReject {
    /// Ссылка на счёт, а не на чек.
    NotACheck,
    /// Этот чек уже в работе в другой заявке.
    AlreadyInWork,
    /// Направление закрыто или на автопаузе.
    DirectionClosed,
    /// Резерва на стороне выплаты не хватает даже на минимальную заявку.
    NoReserve,
    /// Сервис на обслуживании или выплаты остановлены стоп-краном.
    ServicePaused,
    /// Суточный лимит клиента исчерпан (`settings.daily_limit_usd`).
    DailyLimit,
    /// Чек пришёл для оплаты счёта, но котировка уже истекла: чек не открывали.
    QuoteExpired,
    /// По счёту клиента ещё проверяется предыдущий чек: этот не открывали, чтобы он не ушёл
    /// в обмен вместо оплаты счёта.
    InvoiceBusy,
    /// Мульти-чек xRocket: активируется только в приложении xRocket (DESIGN-v0.2 Р8).
    MultiCheck,
    /// Этот чек мы уже открывали (другая заявка — живая или закрытая). Второй `/start` не
    /// отправляем: у мульти-чека CryptoBot наша доля уже получена.
    CheckUsed,
}

impl SubmitReject {
    pub const fn code(self) -> &'static str {
        match self {
            SubmitReject::NotACheck => "not_a_check",
            SubmitReject::AlreadyInWork => "already_in_work",
            SubmitReject::DirectionClosed => "direction_closed",
            SubmitReject::NoReserve => "no_reserve",
            SubmitReject::ServicePaused => "service_paused",
            SubmitReject::DailyLimit => "daily_limit",
            SubmitReject::QuoteExpired => "quote_expired",
            SubmitReject::InvoiceBusy => "invoice_busy",
            SubmitReject::MultiCheck => "xrocket_multi_check",
            SubmitReject::CheckUsed => "check_used",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submitted {
    /// Заявка в работе: чек уходит на активацию.
    Accepted(OrderRow),
    /// Отказ. `order` — запись отказа для истории (если заявка успела создаться).
    Rejected {
        reason: SubmitReject,
        order: Option<OrderRow>,
    },
}

/// Направление обмена по платформе чека: чек CryptoBot меняем на чек xRocket и наоборот.
pub fn direction_for_check(platform: Platform) -> Direction {
    match platform {
        Platform::CryptoBot => Direction::CbToXrCheck,
        Platform::XRocket => Direction::XrToCbCheck,
    }
}

impl Engine {
    /// Клиент `user_id` (уже есть в `users`) прислал ссылку на чек. Если у клиента есть счёт,
    /// который ждёт чек этой платформы, чек идёт в оплату счёта (SPEC §4, П3), иначе — обмен.
    pub async fn submit_check(
        &self,
        user_id: i64,
        link: &WalletLink,
    ) -> Result<Submitted, EngineError> {
        let rejected = |reason| {
            Ok(Submitted::Rejected {
                reason,
                order: None,
            })
        };
        if link.kind != LinkKind::Check {
            return rejected(SubmitReject::NotACheck);
        }
        // Парсер такие ссылки чеком не отдаёт; здесь — вторая линия: `/start` мульти-чека
        // xRocket боту не уходит никогда.
        if link.platform == Platform::XRocket && parsers::links::is_xrocket_multi_check(&link.param)
        {
            return rejected(SubmitReject::MultiCheck);
        }
        let (live, used, awaiting, busy) = {
            let mut conn = self.pool.acquire().await?;
            (
                storage::client::live_order_for_check(&mut conn, link.platform, &link.param)
                    .await?,
                storage::operations::check_activated_before(&mut conn, link.platform, &link.param)
                    .await?,
                storage::orders::awaiting_funds(&mut conn, user_id, Some(link.platform)).await?,
                storage::orders::invoice_checking(&mut conn, user_id, link.platform).await?,
            )
        };
        // Чек сейчас в живой заявке — «уже в работе» (с её номером); открывали раньше —
        // второй раз не открываем.
        if live.is_some() {
            return rejected(SubmitReject::AlreadyInWork);
        }
        if used {
            return rejected(SubmitReject::CheckUsed);
        }
        if let Some(order) = awaiting {
            return self.fund_invoice(order.id, link).await;
        }
        if busy.is_some() {
            return Ok(Submitted::Rejected {
                reason: SubmitReject::InvoiceBusy,
                order: None,
            });
        }
        let direction = direction_for_check(link.platform);
        let asset = Asset::Usdt;
        let mut tx = self.pool.begin().await?;
        let order = match storage::orders::insert(
            &mut tx,
            &NewOrder {
                user_id,
                direction,
                asset,
                intake_start_param: Some(&link.param),
                invoice_start_param: None,
            },
        )
        .await
        {
            Ok(order) => order,
            Err(StorageError::DuplicateLiveCheck) => {
                return Ok(Submitted::Rejected {
                    reason: SubmitReject::AlreadyInWork,
                    order: None,
                });
            }
            Err(e) => return Err(e.into()),
        };

        let dir = storage::directions::get(&mut tx, direction).await?;
        let target = direction.target();
        let paused = storage::settings::flag_on(&mut tx, "maintenance").await?
            || storage::settings::flag_on(&mut tx, "kill_switch_payouts").await?;
        let hold = if paused {
            Err(SubmitReject::ServicePaused)
        } else if dir.accepting() {
            let (wallet, _) = storage::wallets::active(&mut tx, target)
                .await?
                .ok_or(EngineError::NoWalletAccount(target))?;
            storage::holds::lock_wallet(&mut tx, wallet).await?;
            let available = storage::holds::available(&mut tx, wallet, asset).await?;
            let new_user = storage::orders::is_new_user(&mut tx, user_id).await?;
            let per_order = if new_user {
                dir.new_user_max_amount
            } else {
                dir.max_amount
            };
            // Суточный лимит клиента: сколько уже пришло и удержано за 24 часа (USDT ≈ USD).
            let daily = daily_limit(&mut tx, new_user).await?;
            let used = storage::orders::user_volume_24h(&mut tx, user_id).await?;
            let left = daily.map(|d| d - used);
            let max_amount = left.map_or(per_order, |l| per_order.min(l));
            let limits = Limits {
                min_amount: dir.min_amount,
                max_amount,
            };
            if max_amount < dir.min_amount {
                Err(SubmitReject::DailyLimit)
            } else {
                match max_order_effective(limits, available, self.cfg.reserve_safety) {
                    Some(amount) => Ok((wallet, amount)),
                    None => Err(SubmitReject::NoReserve),
                }
            }
        } else {
            Err(SubmitReject::DirectionClosed)
        };

        let result = match hold {
            Ok(hold) => {
                let accepted = transit(
                    &mut tx,
                    &order,
                    Event::LinkAccepted,
                    &OrderPatch::default(),
                    None,
                    EffectCtx {
                        hold: Some(hold),
                        ..EffectCtx::default()
                    },
                )
                .await;
                match accepted {
                    Ok((order, _)) => Submitted::Accepted(order),
                    // Тот же чек одновременно открыли в другой заявке: транзакция откатится,
                    // этой заявки не будет.
                    Err(EngineError::Storage(StorageError::CheckActivatedBefore)) => {
                        return rejected(SubmitReject::CheckUsed);
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(reason) => {
                let patch = OrderPatch {
                    failure_code: Some(reason.code()),
                    ..OrderPatch::default()
                };
                let (order, _) = transit(
                    &mut tx,
                    &order,
                    Event::Rejected,
                    &patch,
                    Some(reason.code()),
                    EffectCtx::default(),
                )
                .await?;
                Submitted::Rejected {
                    reason,
                    order: Some(order),
                }
            }
        };
        tx.commit().await?;
        match &result {
            Submitted::Accepted(order)
            | Submitted::Rejected {
                order: Some(order), ..
            } => self.notify(order),
            Submitted::Rejected { order: None, .. } => {}
        }
        Ok(result)
    }
}

/// Суточный лимит из `settings.daily_limit_usd` (`{"default": 1000, "new_user": 200}`);
/// `None` — не задан.
pub(crate) async fn daily_limit(
    conn: &mut sqlx::PgConnection,
    new_user: bool,
) -> Result<Option<domain::Decimal>, EngineError> {
    let Some(value) = storage::settings::get(conn, "daily_limit_usd").await? else {
        return Ok(None);
    };
    let key = if new_user { "new_user" } else { "default" };
    Ok(value.get(key).and_then(decimal_of))
}

/// Число из настройки: JSON-число или строка.
pub(crate) fn decimal_of(v: &serde_json::Value) -> Option<domain::Decimal> {
    match v {
        serde_json::Value::Number(n) => n.to_string().parse().ok(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Числовая настройка (`quote_ttl_seconds`, `min_refund_amount`); `None` — не задана.
pub(crate) async fn setting_decimal(
    conn: &mut sqlx::PgConnection,
    key: &str,
) -> Result<Option<domain::Decimal>, EngineError> {
    Ok(storage::settings::get(conn, key)
        .await?
        .as_ref()
        .and_then(decimal_of))
}
