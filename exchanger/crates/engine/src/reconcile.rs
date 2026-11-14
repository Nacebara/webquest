//! Сверка балансов (SPEC П14) и самопроверка на живых кошельках (`exch selftest`, пульт).
//!
//! **Сверка.** Раз в `reconcile_every` движок читает баланс USDT каждого кошелька через юзербота
//! и сравнивает с леджером. Пока у кошелька есть операция прямо в работе (`dispatched`) или идёт
//! самопроверка, замер не делается: деньги в пути. Операции с неясным исходом (`unknown`) замер
//! не останавливают: сверка считает оба варианта каждой — «выполнена» и «не выполнена» — и
//! сравнивает баланс со всеми сочетаниями (`domain::reconcile`, находка М-6). Сходится с одним —
//! подсказка оператору, какие операции, судя по балансу, прошли. Сверка не проходит дольше
//! `stall_minutes` — CRITICAL «сверка не проходит». Допуски — `settings.reconcile_tolerance`
//! (`{"warn": "0.01", "pause": "1.00", "stall_minutes": 30}`), сравнение — с ближайшим вариантом:
//! - в пределах `warn` — порядок, открытое событие о расхождении закрывается;
//! - до `pause` — событие WARN;
//! - больше `pause` и **денег меньше**, чем по леджеру, — выплаты с этого кошелька на паузу и
//!   CRITICAL: где-то ушли деньги, которых леджер не видел;
//! - больше `pause` и **денег больше** — WARN с кнопкой «Это мой взнос — провести» в пульте
//!   (владелец пополнил кошелёк): паузы нет, лишние деньги не мешают выплатам.
//!
//! **Самопроверка.** На каждом кошельке: баланс → чек через меню на `amount` → активировать
//! свой чек (третья запись владельца: свой чек зачисляется) → баланс тот же. Деньги никуда не
//! уходят, леджер не трогается; сверка на это время молчит. Чек не активировался — его ссылка
//! в отчёте: забрать руками.

use std::sync::atomic::Ordering;

use domain::reconcile::{MAX_UNSETTLED, Outcome, judge};
use domain::{Asset, Decimal, Money, Platform};
use rust_decimal::prelude::ToPrimitive;
use serde_json::json;
use storage::alerts::{Level, NewAlert, Topic};
use storage::directions::Side;
use userbot::{ActivationOutcome, IssueOutcome, OpTag};

use crate::{Engine, EngineError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Warn,
    Pause,
    SkippedInflight,
    /// Кошелёк не ответил: замера нет.
    Unavailable,
}

impl Verdict {
    fn as_db_str(self) -> Option<&'static str> {
        match self {
            Verdict::Ok => Some("ok"),
            Verdict::Warn => Some("warn"),
            Verdict::Pause => Some("pause"),
            Verdict::SkippedInflight => Some("skipped_inflight"),
            Verdict::Unavailable => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub platform: Platform,
    /// Баланс по леджеру (без неясных операций).
    pub expected: Decimal,
    pub actual: Option<Decimal>,
    pub verdict: Verdict,
    /// Почему замера нет (`Unavailable`): ошибка кошелька или «не подключён».
    pub error: Option<String>,
    /// Операции с неясным исходом по кошельку на момент замера.
    pub unsettled: Vec<i64>,
    /// Те из них, что, судя по балансу, выполнились (пусто — ни одна); `None` — подсказки нет.
    pub matched: Option<Vec<i64>>,
}

/// Ключ события «баланс кошелька не читается».
pub fn unreadable_key(platform: Platform) -> String {
    format!("balance_unreadable:{}", platform.db_name())
}

/// Ключ события «баланс не сходится».
pub fn mismatch_key(platform: Platform) -> String {
    format!("balance_mismatch:{}", platform.db_name())
}

/// Ключ события «сверка кошелька не проходит дольше `stall_minutes`».
pub fn stalled_key(platform: Platform) -> String {
    format!("reconcile_stalled:{}", platform.db_name())
}

impl Reconciliation {
    pub fn diff(&self) -> Option<Decimal> {
        self.actual.map(|a| a - self.expected)
    }
}

/// Шаг самопроверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelftestStep {
    pub platform: Platform,
    pub step: &'static str,
    pub ok: bool,
    pub detail: String,
}

fn usdt_of(balances: &[Money]) -> Decimal {
    balances
        .iter()
        .find(|m| m.asset() == Asset::Usdt)
        .map_or(Decimal::ZERO, Money::amount)
}

/// Операции кошелька, исход которых сейчас не известен.
struct Unsettled {
    /// Есть операция прямо в работе (`dispatched`): деньги в пути, замер не делается.
    busy: bool,
    /// Операции `unknown`: id и на сколько изменится баланс USDT, если операция выполнилась;
    /// `None` — неизвестно (сумма или актив ещё не известны, как у чека до активации).
    ops: Vec<(i64, Option<Decimal>)>,
}

impl Unsettled {
    fn ids(&self) -> Vec<i64> {
        self.ops.iter().map(|(id, _)| *id).collect()
    }

    /// Изменения всех неясных операций, если каждое известно.
    fn deltas(&self) -> Option<Vec<(i64, Decimal)>> {
        self.ops.iter().map(|(id, d)| d.map(|d| (*id, d))).collect()
    }

    /// Почему замер не сделан (в тревогу «сверка не проходит»).
    fn why(&self, selftest: bool) -> &'static str {
        match self.deltas() {
            _ if selftest => "selftest",
            _ if self.busy => "in_progress",
            None => "unknown_amount",
            Some(d) if d.iter().filter(|(_, x)| !x.is_zero()).count() > MAX_UNSETTLED => "too_many",
            Some(_) => "in_progress",
        }
    }
}

/// На сколько операция меняет баланс USDT кошелька, если выполнилась.
fn usdt_delta(kind: &str, amount: Option<Decimal>, asset: Option<&str>) -> Option<Decimal> {
    let sign = match kind {
        "create_payout_check"
        | "create_refund_check"
        | "create_excess_check"
        | "pay_invoice"
        | "withdrawal" => Decimal::NEGATIVE_ONE,
        "activate_check" => Decimal::ONE,
        // Просмотр счёта и чтение баланса деньги не двигают.
        _ => return Some(Decimal::ZERO),
    };
    match asset {
        Some("USDT") => amount.map(|a| a * sign),
        // Другой актив баланс USDT не трогает.
        Some(_) => Some(Decimal::ZERO),
        None => None,
    }
}

/// Операции в полёте по кошельку для событий пульта (не больше пяти): заявка и её публичный
/// номер, вид, статус, сумма. `only_unknown` — только операции с неясным исходом.
async fn ops_brief(
    conn: &mut sqlx::PgConnection,
    wallet: i16,
    only_unknown: bool,
) -> Result<Vec<serde_json::Value>, EngineError> {
    type Brief = (
        i64,
        Option<i64>,
        Option<String>,
        String,
        String,
        Option<Decimal>,
    );
    let rows: Vec<Brief> = sqlx::query_as(
        "SELECT op.id, op.order_id, o.public_id, op.kind, op.status, op.amount FROM operations op \
           LEFT JOIN orders o ON o.id = op.order_id \
          WHERE op.wallet_account_id = $1 AND op.status IN ('dispatched', 'unknown') \
            AND (NOT $2 OR op.status = 'unknown') \
          ORDER BY op.id LIMIT 5",
    )
    .bind(wallet)
    .bind(only_unknown)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, order, public, kind, status, amount)| {
            json!({
                "id": id,
                "order_id": order,
                "public": public,
                "kind": kind,
                "status": status,
                "amount": amount.map(|a| a.normalize().to_string()),
            })
        })
        .collect())
}

/// Операция в полёте: id, вид, статус, сумма, актив.
type OpRow = (i64, String, String, Option<Decimal>, Option<String>);

async fn unsettled(conn: &mut sqlx::PgConnection, wallet: i16) -> Result<Unsettled, EngineError> {
    let rows: Vec<OpRow> = sqlx::query_as(
        "SELECT id, kind, status, amount, asset FROM operations WHERE wallet_account_id = $1 AND status IN ('dispatched', 'unknown') ORDER BY id",
    )
    .bind(wallet)
    .fetch_all(conn)
    .await?;
    let mut out = Unsettled {
        busy: false,
        ops: Vec::new(),
    };
    for (id, kind, status, amount, asset) in rows {
        if status == "dispatched" {
            out.busy = true;
        } else {
            out.ops
                .push((id, usdt_delta(&kind, amount, asset.as_deref())));
        }
    }
    Ok(out)
}

impl Engine {
    /// Сверить балансы всех кошельков с леджером. Ошибка кошелька — `Unavailable`, не ошибка.
    pub async fn reconcile_balances(&self) -> Result<Vec<Reconciliation>, EngineError> {
        let mut out = Vec::new();
        let (warn, pause, stall_minutes) = {
            let mut conn = self.pool.acquire().await?;
            let v = storage::settings::get(&mut conn, "reconcile_tolerance").await?;
            let get = |k: &str, d: Decimal| {
                v.as_ref()
                    .and_then(|v| v.get(k))
                    .and_then(crate::submit::decimal_of)
                    .unwrap_or(d)
            };
            let stall = get("stall_minutes", Decimal::from(30))
                .trunc()
                .to_i64()
                .unwrap_or(30);
            (
                get("warn", Decimal::new(1, 2)),
                get("pause", Decimal::ONE),
                stall,
            )
        };
        for (platform, wallet_bot) in &self.wallets {
            let mut conn = self.pool.acquire().await?;
            let Some((wallet, _)) = storage::wallets::active(&mut conn, *platform).await? else {
                let error = "кошелёк не подключён: войдите юзерботом в этот кошелёк".to_owned();
                self.unreadable(&mut conn, *platform, &error).await?;
                out.push(Reconciliation {
                    platform: *platform,
                    expected: Decimal::ZERO,
                    actual: None,
                    verdict: Verdict::Unavailable,
                    error: Some(error),
                    unsettled: Vec::new(),
                    matched: None,
                });
                continue;
            };
            // Аккаунт юзербота кошелька — в событие «не сходится» (макет: «… · ub-1»).
            let account = storage::wallets::list(&mut conn)
                .await?
                .into_iter()
                .find(|w| w.wallet_id == wallet)
                .map(|w| w.userbot_label);
            let expected = storage::ledger::wallet_balances(&mut conn)
                .await?
                .into_iter()
                .find(|b| b.wallet_account_id == wallet && b.asset == "USDT")
                .map_or(Decimal::ZERO, |b| b.ledger_balance);
            let pending = unsettled(&mut conn, wallet).await?;
            let selftest = self.selftest_running.load(Ordering::SeqCst);
            // Баланс читать имеет смысл, если деньги не в пути. Вердикт — если ещё и каждый
            // вариант неясных операций известен. Активация с неизвестной суммой вердикт не даёт,
            // но замер нужен: по нему движок ограничивает «Деньги пришли» (М-5, шаг 1.6).
            let readable = |p: &Unsettled, selftest: bool| !selftest && !p.busy;
            let measurable = |p: &Unsettled| {
                p.deltas().is_some_and(|d| {
                    d.iter().filter(|(_, x)| !x.is_zero()).count() <= MAX_UNSETTLED
                })
            };
            let skip = !readable(&pending, selftest);
            drop(conn);
            let actual = if skip {
                None
            } else {
                match wallet_bot.balances().await {
                    Ok(b) => Some(usdt_of(&b)),
                    Err(e) => {
                        tracing::warn!(platform = platform.db_name(), error = %e, "balance read failed");
                        let error = e.to_string();
                        let mut conn = self.pool.acquire().await?;
                        self.unreadable(&mut conn, *platform, &error).await?;
                        out.push(Reconciliation {
                            platform: *platform,
                            expected,
                            actual: None,
                            verdict: Verdict::Unavailable,
                            error: Some(error),
                            unsettled: pending.ids(),
                            matched: None,
                        });
                        continue;
                    }
                }
            };
            let mut tx = self.pool.begin().await?;
            // Пока читали баланс, могла начаться или решиться операция: такой замер не считаем.
            let now_pending = unsettled(&mut tx, wallet).await?;
            let raced = !readable(&now_pending, self.selftest_running.load(Ordering::SeqCst))
                || now_pending.ids() != pending.ids();
            let balance_read = actual.is_some() && !raced;
            let judgement = match actual {
                Some(a) if balance_read && measurable(&pending) => pending
                    .deltas()
                    .and_then(|d| judge(expected, &d, a, warn, pause)),
                _ => None,
            };
            let verdict = match &judgement {
                None => Verdict::SkippedInflight,
                Some(j) => match j.outcome {
                    Outcome::Ok => Verdict::Ok,
                    Outcome::Warn => Verdict::Warn,
                    Outcome::Pause => Verdict::Pause,
                },
            };
            let unsettled_ids = pending.ids();
            let matched = judgement.as_ref().and_then(|j| j.matched.clone());
            if let Some(v) = verdict.as_db_str() {
                sqlx::query(
                    "INSERT INTO reconciliations (wallet_account_id, asset, source, expected, actual, inflight_ops, verdict, unsettled_ops, matched_ops, balance_read) VALUES ($1, 'USDT', 'userbot', $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(wallet)
                .bind(expected)
                .bind(actual.filter(|_| balance_read).unwrap_or(expected))
                .bind(i32::try_from(unsettled_ids.len()).unwrap_or(i32::MAX))
                .bind(v)
                .bind(&unsettled_ids)
                .bind(&matched)
                .bind(balance_read)
                .execute(&mut *tx)
                .await?;
            }
            let key = mismatch_key(*platform);
            if actual.is_some() {
                // Баланс прочитан — событие «не читается» закрыто.
                storage::alerts::resolve(&mut tx, &unreadable_key(*platform)).await?;
            }
            match (verdict, actual, &judgement) {
                (Verdict::Ok, _, _) => {
                    storage::alerts::resolve(&mut tx, &key).await?;
                }
                (Verdict::Warn | Verdict::Pause, Some(a), Some(j)) => {
                    // Сравнивали с ближайшим вариантом неясных операций; без них это леджер.
                    let reference = j.reference;
                    let lowest = expected
                        + pending
                            .deltas()
                            .unwrap_or_default()
                            .iter()
                            .map(|(_, d)| (*d).min(Decimal::ZERO))
                            .sum::<Decimal>();
                    let unsettled_brief = if unsettled_ids.is_empty() {
                        Vec::new()
                    } else {
                        ops_brief(&mut tx, wallet, true).await?
                    };
                    if verdict == Verdict::Pause {
                        let reason = format!(
                            "сверка: в кошельке {a} USDT, ожидалось {reference} — не хватает {}",
                            reference - a
                        );
                        storage::directions::auto_pause_platform(
                            &mut tx,
                            *platform,
                            Side::Target,
                            &reason,
                        )
                        .await?;
                        tracing::error!(platform = platform.db_name(), %expected, %reference, actual = %a, "reconciliation: money missing, payouts paused");
                    }
                    storage::alerts::raise_condition(
                        &mut tx,
                        &NewAlert {
                            level: if verdict == Verdict::Pause {
                                Level::Critical
                            } else {
                                Level::Warn
                            },
                            topic: Topic::Reserves,
                            dedup_key: &key,
                            title: "balance_mismatch",
                            payload: json!({
                                "platform": platform.db_name(),
                                "expected": reference.normalize().to_string(),
                                "ledger": expected.normalize().to_string(),
                                "actual": a.normalize().to_string(),
                                "diff": (a - reference).normalize().to_string(),
                                "paused": verdict == Verdict::Pause,
                                "account": account,
                                "unsettled": unsettled_brief,
                                // Меньше любого исхода неясных операций: не хватает при любом.
                                "below_all": a < lowest,
                            }),
                        },
                    )
                    .await?;
                }
                _ => {}
            }
            // Сверка прошла — тревога «не проходит» закрыта; пропущена — сколько уже не проходит.
            let stalled = stalled_key(*platform);
            if verdict == Verdict::SkippedInflight {
                self.stall_check(
                    &mut tx,
                    *platform,
                    wallet,
                    stall_minutes,
                    pending.why(selftest),
                )
                .await?;
            } else {
                storage::alerts::resolve(&mut tx, &stalled).await?;
            }
            tx.commit().await?;
            out.push(Reconciliation {
                platform: *platform,
                expected,
                actual,
                verdict,
                error: None,
                unsettled: unsettled_ids,
                matched,
            });
        }
        out.sort_by_key(|r| r.platform.db_name());
        Ok(out)
    }

    /// Сверка кошелька пропускается с `since` (первый пропуск после последнего замера) дольше
    /// `stall_minutes` — CRITICAL: всё это время недостачу не видно. В событии — операции, которые
    /// держат сверку, с заявками.
    async fn stall_check(
        &self,
        conn: &mut sqlx::PgConnection,
        platform: Platform,
        wallet: i16,
        stall_minutes: i64,
        why: &str,
    ) -> Result<(), EngineError> {
        let (since, minutes): (Option<chrono::DateTime<chrono::Utc>>, Option<i64>) = sqlx::query_as(
            "SELECT min(at), (EXTRACT(EPOCH FROM now() - min(at)) / 60)::bigint FROM reconciliations \
              WHERE wallet_account_id = $1 AND verdict = 'skipped_inflight' \
                AND at > COALESCE((SELECT max(at) FROM reconciliations WHERE wallet_account_id = $1 AND verdict <> 'skipped_inflight'), '-infinity')",
        )
        .bind(wallet)
        .fetch_one(&mut *conn)
        .await?;
        let (Some(since), Some(minutes)) = (since, minutes) else {
            return Ok(());
        };
        if minutes < stall_minutes {
            return Ok(());
        }
        let ops = ops_brief(&mut *conn, wallet, false).await?;
        tracing::error!(
            platform = platform.db_name(),
            minutes,
            why,
            "reconciliation stalled"
        );
        storage::alerts::raise_condition(
            conn,
            &NewAlert {
                level: Level::Critical,
                topic: Topic::Reserves,
                dedup_key: &stalled_key(platform),
                title: "reconcile_stalled",
                payload: json!({
                    "platform": platform.db_name(),
                    "since": since.to_rfc3339(),
                    "minutes": minutes,
                    "reason": why,
                    "ops": ops,
                }),
            },
        )
        .await?;
        Ok(())
    }

    /// Баланс кошелька не читается или кошелёк не подключён: событие владельцу с причиной.
    /// Иначе резерв тихо остаётся нулём, а направления — на паузе без объяснений.
    async fn unreadable(
        &self,
        conn: &mut sqlx::PgConnection,
        platform: Platform,
        error: &str,
    ) -> Result<(), EngineError> {
        storage::alerts::raise_condition(
            conn,
            &NewAlert {
                level: Level::Warn,
                topic: Topic::Reserves,
                dedup_key: &unreadable_key(platform),
                title: "balance_unreadable",
                payload: json!({
                    "platform": platform.db_name(),
                    "error": ui_safe(error),
                }),
            },
        )
        .await?;
        Ok(())
    }

    /// Самопроверка на живых кошельках: чек на `amount` через меню и его же активация.
    pub async fn selftest(&self, amount: Decimal) -> Result<Vec<SelftestStep>, EngineError> {
        if self.selftest_running.swap(true, Ordering::SeqCst) {
            return Err(EngineError::Invariant("selftest is already running".into()));
        }
        let result = self.selftest_inner(amount).await;
        self.selftest_running.store(false, Ordering::SeqCst);
        result
    }

    async fn selftest_inner(&self, amount: Decimal) -> Result<Vec<SelftestStep>, EngineError> {
        let mut steps = Vec::new();
        let money = Money::new(amount, Asset::Usdt)?;
        let stamp = chrono::Utc::now().timestamp();
        let mut platforms: Vec<_> = self.wallets.iter().collect();
        platforms.sort_by_key(|(p, _)| p.db_name());
        for (platform, wallet) in platforms {
            let platform = *platform;
            let mut push = |step: &'static str, ok: bool, detail: String| {
                steps.push(SelftestStep {
                    platform,
                    step,
                    ok,
                    detail,
                });
            };
            let before = match wallet.balances().await {
                Ok(b) => usdt_of(&b),
                Err(e) => {
                    push("balance", false, e.to_string());
                    continue;
                }
            };
            push("balance", true, format!("{before} USDT"));
            if before < amount {
                push("issue", false, format!("на кошельке меньше {amount} USDT"));
                continue;
            }
            let tag = OpTag {
                key: format!("selftest-{}-{stamp}-issue", platform.db_name()),
                random_id: userbot::flows::rid::new_random_id(),
            };
            let check = match wallet.issue_check(&money, &tag).await {
                Ok(IssueOutcome::Issued { check, .. }) => {
                    push(
                        "issue",
                        true,
                        format!("{} · {}", check.amount.amount(), mask(&check.param)),
                    );
                    check
                }
                Ok(other) => {
                    push("issue", false, format!("{other:?}"));
                    continue;
                }
                Err(e) => {
                    push("issue", false, e.to_string());
                    continue;
                }
            };
            let tag = OpTag {
                key: format!("selftest-{}-{stamp}-claim", platform.db_name()),
                random_id: userbot::flows::rid::new_random_id(),
            };
            match wallet.activate_check(&check.param, &tag).await {
                Ok(ActivationOutcome::Received(m)) if m == money => {
                    push("claim", true, format!("{} USDT", m.amount()));
                }
                Ok(other) => {
                    push(
                        "claim",
                        false,
                        format!("{other:?} — заберите чек вручную: {}", check.url),
                    );
                    continue;
                }
                Err(e) => {
                    push(
                        "claim",
                        false,
                        format!("{e} — заберите чек вручную: {}", check.url),
                    );
                    continue;
                }
            }
            match wallet.balances().await {
                Ok(b) => {
                    let after = usdt_of(&b);
                    push("balance_after", after == before, format!("{after} USDT"));
                }
                Err(e) => push("balance_after", false, e.to_string()),
            }
        }
        Ok(steps)
    }
}

fn mask(param: &str) -> String {
    let chars: Vec<char> = param.chars().collect();
    if chars.len() <= 8 {
        return param.to_owned();
    }
    format!(
        "{}…{}",
        chars[..4].iter().collect::<String>(),
        chars[chars.len() - 2..].iter().collect::<String>()
    )
}

/// Текст ошибки для события: короткий, без переводов строк.
fn ui_safe(error: &str) -> String {
    let one_line: String = error.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(300).collect()
}
