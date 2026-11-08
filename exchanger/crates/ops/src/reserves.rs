//! «Резервы и сверка» — макет: леджер против реальных балансов, деньги в полёте, удержанные,
//! цель и порог по каждому кошельку; строка сверки (когда была, расхождения, когда следующая);
//! строки порога — кошелёк у порога и что сделал рычаг комиссии.
//!
//! «Недостача» — сообщение события «не сходится» ([`crate::alerts`]): «Это вы выводили?» —
//! «Да, это я» проводит вывод владельца (после подтверждения), «Нет, разбираться» оставляет
//! выплаты на паузе и закрывает повтор события.

use chrono::{DateTime, Duration, TimeZone as _, Utc};
use domain::{Asset, Decimal, Direction, Platform};
use storage::staff::StaffMember;

use crate::dash::{amount, hhmm};
use crate::texts::{Texts, esc};
use crate::{Cb, Inner, OpsError, Screen, btn};

/// Резерв кошелька.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletReserve {
    pub platform: Platform,
    pub ledger: Decimal,
    /// По последней сверке.
    pub actual: Option<Decimal>,
    /// Выплаты, которые уходят прямо сейчас.
    pub in_flight: Decimal,
    /// Удержано под принятые заявки.
    pub held: Decimal,
    /// Цель и порог «мало».
    pub target: Option<(Decimal, Decimal)>,
}

/// Расхождение по открытому событию сверки: кошелёк, разница (реально − леджер), событие.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub platform: Platform,
    pub diff: Decimal,
    pub alert: i64,
}

/// Всё для карточки — из базы ([`Inner::reserves_data`]) или из макета ([`sample_reserves`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservesData {
    pub wallets: Vec<WalletReserve>,
    pub reconciled_at: Option<DateTime<Utc>>,
    pub next_at: Option<DateTime<Utc>>,
    pub mismatches: Vec<Mismatch>,
    /// Строки под сверкой: точка и текст (у порога, ниже критичного, баланс не читается).
    pub notes: Vec<(&'static str, String)>,
}

fn name(p: Platform) -> &'static str {
    match p {
        Platform::CryptoBot => "CryptoBot",
        Platform::XRocket => "xRocket",
    }
}

/// «CB → xR» — коротко, как в макете.
pub(crate) fn short(d: Direction) -> &'static str {
    match d {
        Direction::CbToXrCheck => "CB → xR",
        Direction::XrToCbCheck => "xR → CB",
        Direction::PayCbInvoice => "счёт CB",
        Direction::PayXrInvoice => "счёт xR",
    }
}

impl ReservesData {
    pub fn screen(&self, t: &Texts) -> Screen {
        let title = t.t("reserves.title");
        let mut rich = format!("<h3>{title}</h3><table striped compact><tr><th></th>");
        for w in &self.wallets {
            rich.push_str(&format!("<th align=\"right\">{}</th>", name(w.platform)));
        }
        rich.push_str("</tr>");
        let cell = |v: &dyn Fn(&WalletReserve) -> String| -> Vec<String> {
            self.wallets.iter().map(v).collect()
        };
        let rows: Vec<(String, Vec<String>)> = vec![
            (t.t("reserves.r_ledger"), cell(&|w| amount(w.ledger))),
            (
                t.t("reserves.r_actual"),
                cell(&|w| match w.actual {
                    Some(a) if a < w.ledger => format!("{{e:dot_act}} {}", amount(a)),
                    Some(a) => amount(a),
                    None => t.t("reserves.unknown"),
                }),
            ),
            (t.t("reserves.r_in_flight"), cell(&|w| amount(w.in_flight))),
            (t.t("reserves.r_held"), cell(&|w| amount(w.held))),
            (
                t.t("reserves.r_target"),
                cell(&|w| match w.target {
                    Some((target, low)) => {
                        format!("{} · {}", ui::fmt::grouped(target), ui::fmt::grouped(low))
                    }
                    None => t.t("reserves.unknown"),
                }),
            ),
        ];
        let mut plain = format!("<b>{title}</b>\n");
        for w in &self.wallets {
            plain.push_str(&format!(
                "<blockquote><b>{}</b>\n{}</blockquote>\n",
                name(w.platform),
                rows.iter()
                    .map(|(label, values)| {
                        let i = self
                            .wallets
                            .iter()
                            .position(|x| x.platform == w.platform)
                            .unwrap_or(0);
                        format!("{label}: {}", values.get(i).cloned().unwrap_or_default())
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        for (label, values) in &rows {
            rich.push_str(&format!("<tr><td>{label}</td>"));
            for v in values {
                rich.push_str(&format!("<td align=\"right\">{v}</td>"));
            }
            rich.push_str("</tr>");
        }
        rich.push_str("</table>");

        // Сверка: когда, расхождения, когда следующая.
        let mut parts = Vec::new();
        let dot = if self.mismatches.is_empty() {
            "{e:dot_ok}"
        } else {
            "{e:dot_act}"
        };
        parts.push(match self.reconciled_at {
            Some(at) => t.f("reserves.reconciled", &[("time", &hhmm(at))]),
            None => t.t("reserves.never"),
        });
        if self.mismatches.is_empty() {
            if self.reconciled_at.is_some() {
                parts.push(t.t("reserves.no_diff"));
            }
        } else {
            for m in &self.mismatches {
                let sign = if m.diff > Decimal::ZERO { "+" } else { "−" };
                parts.push(t.f(
                    "reserves.diff",
                    &[
                        ("name", &name(m.platform)),
                        ("diff", &format!("{sign}{}", amount(m.diff.abs()))),
                    ],
                ));
            }
        }
        if let Some(next) = self.next_at {
            parts.push(t.f("reserves.next", &[("time", &hhmm(next))]));
        }
        let line = format!("{dot} {}", parts.join(" · "));
        rich.push_str(&format!("<p>{line}</p>"));
        plain.push_str(&format!("{line}\n"));
        for (dot, note) in &self.notes {
            rich.push_str(&format!("<p>{dot} {note}</p>"));
            plain.push_str(&format!("{dot} {note}\n"));
        }

        let mut s = Screen::card(rich, plain.trim_end().to_owned()).row(vec![
            btn(t.t("reserves.b_reconcile"), &Cb::Reconcile).style(botapi::ButtonStyle::Primary),
        ]);
        for m in &self.mismatches {
            let (key, style) = if m.diff < Decimal::ZERO {
                ("reserves.b_short", Some(botapi::ButtonStyle::Danger))
            } else {
                ("reserves.b_extra", None)
            };
            let b = btn(
                t.f(key, &[("name", &name(m.platform))]),
                &Cb::AlertShow(m.alert),
            );
            s = s.row(vec![match style {
                Some(st) => b.style(st),
                None => b,
            }]);
        }
        s.row(vec![btn(t.t("reserves.b_rebalance"), &Cb::Promo)])
            .row(vec![
                btn(t.t("reserves.b_profit"), &Cb::Profit),
                btn(t.t("reserves.b_thresholds"), &Cb::Thresholds),
            ])
    }
}

/// Москва, 5 ноября 2026 (как в макете).
fn msk(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 5, h, m, 0)
        .single()
        .unwrap_or_default()
        - Duration::hours(3)
}

/// Карточка на данных макета «Резервы и сверка».
pub fn sample_reserves(t: &Texts) -> ReservesData {
    let w = |platform, ledger: i64, in_flight: Decimal, held: i64| WalletReserve {
        platform,
        ledger: Decimal::from(ledger),
        actual: Some(Decimal::from(ledger)),
        in_flight,
        held: Decimal::from(held),
        target: Some((Decimal::from(1500), Decimal::from(600))),
    };
    ReservesData {
        wallets: vec![
            w(Platform::CryptoBot, 2140, Decimal::ZERO, 300),
            w(Platform::XRocket, 640, Decimal::new(9750, 2), 120),
        ],
        reconciled_at: Some(msk(14, 0)),
        next_at: Some(msk(14, 15)),
        mismatches: Vec::new(),
        notes: vec![(
            "{e:dot_wait}",
            t.f(
                "reserves.near_low_lever",
                &[
                    ("name", &"xRocket"),
                    ("dir", &short(Direction::CbToXrCheck)),
                    ("fee", &ui::fmt::percent(Decimal::new(25, 1))),
                ],
            ),
        )],
    }
}

impl Inner {
    /// Данные карточки из базы.
    pub(crate) async fn reserves_data(&self) -> Result<ReservesData, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let wallets = storage::wallets::list(&mut conn).await?;
        let balances = storage::ledger::wallet_balances(&mut conn).await?;
        let recon = storage::admin::last_reconciliations(&mut conn).await?;
        let flying: Vec<(i16, Decimal)> = sqlx::query_as(
            "SELECT wallet_account_id, COALESCE(sum(amount), 0) FROM operations \
              WHERE status IN ('pending', 'dispatched', 'unknown') AND wallet_account_id IS NOT NULL \
                AND kind IN ('create_payout_check', 'create_refund_check', 'create_excess_check', 'pay_invoice', 'withdrawal') \
              GROUP BY wallet_account_id",
        )
        .fetch_all(&mut *conn)
        .await?;
        let alerts = storage::alerts::open(&mut conn, 500).await?;
        let mut out = Vec::new();
        let mut mismatches = Vec::new();
        let mut notes = Vec::new();
        for w in wallets.iter().filter(|w| w.is_active) {
            let Some(b) = balances
                .iter()
                .find(|b| b.wallet_account_id == w.wallet_id && b.asset == "USDT")
            else {
                continue;
            };
            let th = storage::admin::thresholds(&mut conn, w.wallet_id, Asset::Usdt).await?;
            out.push(WalletReserve {
                platform: w.platform,
                ledger: b.ledger_balance,
                actual: recon
                    .iter()
                    .find(|r| r.wallet_account_id == w.wallet_id)
                    .map(|r| r.actual),
                in_flight: flying
                    .iter()
                    .find(|(id, _)| *id == w.wallet_id)
                    .map_or(Decimal::ZERO, |(_, v)| *v),
                held: b.held,
                target: th.map(|th| (th.target, th.low)),
            });
            let key = engine::reconcile::mismatch_key(w.platform);
            if let Some(a) = alerts.iter().find(|a| a.dedup_key == key) {
                let diff: Decimal = a.payload["diff"]
                    .as_str()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or_default();
                mismatches.push(Mismatch {
                    platform: w.platform,
                    diff,
                    alert: a.id,
                });
            }
            let key = engine::reconcile::unreadable_key(w.platform);
            if let Some(a) = alerts.iter().find(|a| a.dedup_key == key) {
                notes.push((
                    "{e:dot_act}",
                    t.f(
                        "reserves.unreadable",
                        &[
                            ("name", &name(w.platform)),
                            (
                                "error",
                                &esc(a.payload["error"].as_str().unwrap_or_default()),
                            ),
                        ],
                    ),
                ));
            }
            // Порог: доступно меньше «мало» — что сделал рычаг комиссии.
            if let Some(th) = th {
                if b.available < th.critical {
                    notes.push((
                        "{e:dot_act}",
                        t.f(
                            "reserves.below_critical",
                            &[
                                ("name", &name(w.platform)),
                                ("available", &amount(b.available)),
                            ],
                        ),
                    ));
                } else if b.available < th.low {
                    let mut raised = None;
                    for d in crate::views::DIRECTIONS {
                        if d.target() != w.platform {
                            continue;
                        }
                        let row = storage::directions::get(&mut conn, d).await?;
                        let now = self.fee_now(&mut conn, &row).await?;
                        if now != ui::fmt::percent(row.fee_pct) {
                            raised = Some((d, now));
                            break;
                        }
                    }
                    notes.push((
                        "{e:dot_wait}",
                        match raised {
                            Some((d, fee)) => t.f(
                                "reserves.near_low_lever",
                                &[
                                    ("name", &name(w.platform)),
                                    ("dir", &short(d)),
                                    ("fee", &fee),
                                ],
                            ),
                            None => t.f(
                                "reserves.near_low",
                                &[
                                    ("name", &name(w.platform)),
                                    ("available", &amount(b.available)),
                                    ("low", &amount(th.low)),
                                ],
                            ),
                        },
                    ));
                }
            }
        }
        let reconciled_at = recon.iter().map(|r| r.at).max();
        let every = chrono::Duration::from_std(self.engine.config().reconcile_every)
            .unwrap_or_else(|_| Duration::minutes(10));
        Ok(ReservesData {
            wallets: out,
            reconciled_at,
            next_at: reconciled_at.map(|at| at + every),
            mismatches,
            notes,
        })
    }

    /// «Резервы и сверка».
    pub(crate) async fn reserves(&self) -> Result<Screen, OpsError> {
        Ok(self.reserves_data().await?.screen(&self.texts))
    }

    /// «Сверить сейчас»: сверка и та же карточка со свежими цифрами; что не прочиталось —
    /// строкой над ней.
    pub(crate) async fn reconcile_now(&self) -> Result<Screen, OpsError> {
        let report = self.engine.reconcile_balances().await?;
        let t = &self.texts;
        let mut lines = Vec::new();
        for r in &report {
            match r.verdict {
                engine::reconcile::Verdict::SkippedInflight => {
                    lines.push(t.f("reserves.skipped", &[("name", &name(r.platform))]))
                }
                engine::reconcile::Verdict::Unavailable => lines.push(t.f(
                    "reserves.unreadable",
                    &[
                        ("name", &name(r.platform)),
                        ("error", &esc(r.error.as_deref().unwrap_or(""))),
                    ],
                )),
                _ => {}
            }
        }
        let card = self.reserves().await?;
        let head = if report.is_empty() {
            t.t("reserves.no_wallets")
        } else if lines.is_empty() {
            t.f("reserves.done", &[("time", &hhmm(Utc::now()))])
        } else {
            lines.join("\n")
        };
        Ok(card.notice(&head))
    }

    /// Событие пульта по id — например «Недостача» с главного экрана или из «Резервов».
    pub(crate) async fn alert_screen(&self, id: i64) -> Result<Screen, OpsError> {
        let a = {
            let mut conn = self.pool.acquire().await?;
            storage::alerts::get(&mut conn, id).await?
        };
        Ok(match a {
            Some(a) => self.render_alert(&a),
            None => self.reserves().await?,
        })
    }

    /// «Нет, разбираться»: выплаты остаются на паузе, событие больше не повторяется; что
    /// проверить — строкой над «Резервами».
    pub(crate) async fn investigate(
        &self,
        staff: &StaffMember,
        id: i64,
    ) -> Result<Screen, OpsError> {
        let a = {
            let mut conn = self.pool.acquire().await?;
            let a = storage::alerts::get(&mut conn, id).await?;
            storage::alerts::ack(&mut conn, id, staff.tg_user_id).await?;
            a
        };
        let platform = a
            .as_ref()
            .and_then(|a| a.payload["platform"].as_str())
            .and_then(Platform::from_db_name);
        let account = a
            .as_ref()
            .and_then(|a| a.payload["account"].as_str().map(esc))
            .unwrap_or_else(|| self.texts.t("reserves.account_unknown"));
        let head = self.texts.f(
            "reserves.investigating",
            &[
                ("name", &platform.map_or("кошелёк", name)),
                ("account", &account),
            ],
        );
        Ok(self.reserves().await?.notice(&head))
    }

    /// «Пороги»: цель и порог «мало» по кошелькам, изменить цель.
    pub(crate) async fn thresholds_screen(&self) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let data = self.reserves_data().await?;
        let title = t.t("reserves.th_title");
        let mut rich = format!(
            "<h3>{title}</h3><table striped compact><tr><th></th><th align=\"right\">{}</th><th align=\"right\">{}</th></tr>",
            t.t("reserves.th_target"),
            t.t("reserves.th_low"),
        );
        let mut plain = format!("<b>{title}</b>\n");
        let mut s = Screen::default();
        for w in &data.wallets {
            let (target, low) = w.target.map_or_else(
                || (t.t("reserves.unknown"), t.t("reserves.unknown")),
                |(a, b)| (ui::fmt::grouped(a), ui::fmt::grouped(b)),
            );
            rich.push_str(&format!(
                "<tr><td>{}</td><td align=\"right\">{target}</td><td align=\"right\">{low}</td></tr>",
                name(w.platform)
            ));
            plain.push_str(&format!("{} — {target} · {low}\n", name(w.platform)));
            s = s.row(vec![btn(
                t.f("reserves.b_target", &[("name", &name(w.platform))]),
                &Cb::PromoTarget(w.platform),
            )]);
        }
        rich.push_str(&format!("</table><p>{}</p>", t.t("reserves.th_note")));
        plain.push_str(&t.t("reserves.th_note"));
        s.text = plain;
        s.rich = Some(rich);
        Ok(s.row(vec![btn(t.t("reserves.b_back"), &Cb::Reserves)]))
    }

    /// «Прибыль» — вывода прибыли ещё нет: говорим прямо.
    pub(crate) fn profit(&self) -> Screen {
        let t = &self.texts;
        let rich = format!(
            "<h3>{}</h3><p>{}</p>",
            t.t("reserves.profit_title"),
            t.t("reserves.profit_soon")
        );
        let plain = format!(
            "<b>{}</b>\n{}",
            t.t("reserves.profit_title"),
            t.t("reserves.profit_soon")
        );
        Screen::card(rich, plain).row(vec![btn(t.t("reserves.b_back"), &Cb::Reserves)])
    }
}
