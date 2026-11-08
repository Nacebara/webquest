//! Сверка балансов (SPEC П14) и самопроверка на живых кошельках (`exch selftest`, пульт).
//!
//! **Сверка.** Раз в `reconcile_every` движок читает баланс USDT каждого кошелька через юзербота
//! и сравнивает с леджером. Пока у кошелька есть операции в полёте (`dispatched` / `unknown`)
//! или идёт самопроверка, замер не делается: деньги могут быть в пути. Допуски —
//! `settings.reconcile_tolerance` (`{"warn": "0.01", "pause": "1.00"}`):
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

use domain::{Asset, Decimal, Money, Platform};
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
    pub expected: Decimal,
    pub actual: Option<Decimal>,
    pub verdict: Verdict,
    /// Почему замера нет (`Unavailable`): ошибка кошелька или «не подключён».
    pub error: Option<String>,
}

/// Ключ события «баланс кошелька не читается».
pub fn unreadable_key(platform: Platform) -> String {
    format!("balance_unreadable:{}", platform.db_name())
}

/// Ключ события «баланс не сходится».
pub fn mismatch_key(platform: Platform) -> String {
    format!("balance_mismatch:{}", platform.db_name())
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

async fn in_flight(conn: &mut sqlx::PgConnection, wallet: i16) -> Result<i64, EngineError> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM operations WHERE wallet_account_id = $1 AND status IN ('dispatched', 'unknown')",
    )
    .bind(wallet)
    .fetch_one(conn)
    .await?)
}

impl Engine {
    /// Сверить балансы всех кошельков с леджером. Ошибка кошелька — `Unavailable`, не ошибка.
    pub async fn reconcile_balances(&self) -> Result<Vec<Reconciliation>, EngineError> {
        let mut out = Vec::new();
        let (warn, pause) = {
            let mut conn = self.pool.acquire().await?;
            let v = storage::settings::get(&mut conn, "reconcile_tolerance").await?;
            let get = |k: &str, d: Decimal| {
                v.as_ref()
                    .and_then(|v| v.get(k))
                    .and_then(crate::submit::decimal_of)
                    .unwrap_or(d)
            };
            (get("warn", Decimal::new(1, 2)), get("pause", Decimal::ONE))
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
            let skip = self.selftest_running.load(Ordering::SeqCst)
                || in_flight(&mut conn, wallet).await? > 0;
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
                        });
                        continue;
                    }
                }
            };
            let mut tx = self.pool.begin().await?;
            // Пока читали баланс, могла начаться операция: такой замер не считаем.
            let raced = in_flight(&mut tx, wallet).await? > 0
                || self.selftest_running.load(Ordering::SeqCst);
            let verdict = match actual {
                None => Verdict::SkippedInflight,
                Some(_) if raced => Verdict::SkippedInflight,
                Some(a) => {
                    let d = (a - expected).abs();
                    if d <= warn {
                        Verdict::Ok
                    } else if d <= pause || a > expected {
                        Verdict::Warn
                    } else {
                        Verdict::Pause
                    }
                }
            };
            if let Some(v) = verdict.as_db_str() {
                sqlx::query(
                    "INSERT INTO reconciliations (wallet_account_id, asset, source, expected, actual, inflight_ops, verdict) VALUES ($1, 'USDT', 'userbot', $2, $3, 0, $4)",
                )
                .bind(wallet)
                .bind(expected)
                .bind(actual.unwrap_or(expected))
                .bind(v)
                .execute(&mut *tx)
                .await?;
            }
            let key = mismatch_key(*platform);
            if actual.is_some() {
                // Баланс прочитан — событие «не читается» закрыто.
                storage::alerts::resolve(&mut tx, &unreadable_key(*platform)).await?;
            }
            match (verdict, actual) {
                (Verdict::Ok, _) => {
                    storage::alerts::resolve(&mut tx, &key).await?;
                }
                (Verdict::Warn | Verdict::Pause, Some(a)) => {
                    if verdict == Verdict::Pause {
                        let reason = format!(
                            "сверка: в кошельке {a} USDT, по леджеру {expected} — не хватает {}",
                            expected - a
                        );
                        storage::directions::auto_pause_platform(
                            &mut tx,
                            *platform,
                            Side::Target,
                            &reason,
                        )
                        .await?;
                        tracing::error!(platform = platform.db_name(), %expected, actual = %a, "reconciliation: money missing, payouts paused");
                    }
                    storage::alerts::raise(
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
                                "expected": expected.normalize().to_string(),
                                "actual": a.normalize().to_string(),
                                "diff": (a - expected).normalize().to_string(),
                                "paused": verdict == Verdict::Pause,
                                "account": account,
                            }),
                        },
                    )
                    .await?;
                }
                _ => {}
            }
            tx.commit().await?;
            out.push(Reconciliation {
                platform: *platform,
                expected,
                actual,
                verdict,
                error: None,
            });
        }
        out.sort_by_key(|r| r.platform.db_name());
        Ok(out)
    }

    /// Баланс кошелька не читается или кошелёк не подключён: событие владельцу с причиной.
    /// Иначе резерв тихо остаётся нулём, а направления — на паузе без объяснений.
    async fn unreadable(
        &self,
        conn: &mut sqlx::PgConnection,
        platform: Platform,
        error: &str,
    ) -> Result<(), EngineError> {
        storage::alerts::raise(
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
