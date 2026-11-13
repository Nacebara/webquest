//! События для персонала (`alerts`, SPEC §8): заявки на ручном разборе, подсказки и
//! переключения встречного потока, прочее от движка.
//!
//! Каждые `alert_every`:
//! 1. у каждой заявки на разборе есть открытое событие `manual_review:<id>`; заявка ушла с
//!    разбора — событие закрывается само;
//! 2. новые и изменившиеся события уходят в личку всему действующему персоналу; повтор того же
//!    события правит уже отправленное сообщение («×N»);
//! 3. «Принял» закрывает событие у всех.

use botapi::{EditMessageText, ReplyMarkup, SendMessage};
use domain::Direction;
use serde_json::{Value, json};
use storage::alerts::{AlertRow, Level, NewAlert, Topic};
use storage::staff::StaffMember;

use crate::cb::Cb;
use crate::texts::esc;
use crate::views::{pct, usdt};
use crate::{Inner, OpsError, Screen, btn};

fn s(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(x) => x.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn dec(v: &Value, key: &str) -> domain::Decimal {
    s(v, key).parse().unwrap_or_default()
}

impl Inner {
    pub(crate) async fn alerts_once(&self) -> Result<(), OpsError> {
        let mut conn = self.pool.acquire().await?;
        // 1. Ручной разбор: событие на каждую заявку в очереди.
        let queue = storage::admin::review_queue(&mut conn).await?;
        for item in &queue {
            let key = format!("manual_review:{}", item.id);
            storage::alerts::ensure(
                &mut conn,
                &NewAlert {
                    level: Level::Critical,
                    topic: Topic::Errors,
                    dedup_key: &key,
                    title: "manual_review",
                    payload: json!({
                        "id": item.id,
                        "direction": item.direction.code(),
                        "amount": item.intake_amount.or(item.payout_amount).map(|a| a.to_string()),
                        "reason": item.manual_reason,
                    }),
                },
            )
            .await?;
        }
        for alert in storage::alerts::open(&mut conn, 500).await? {
            if alert.title != "manual_review" {
                continue;
            }
            let id = alert.payload["id"].as_i64();
            if id.is_some_and(|id| !queue.iter().any(|q| q.id == id)) {
                storage::alerts::resolve(&mut conn, &alert.dedup_key).await?;
                self.state(|st| st.delivered.remove(&alert.id));
            }
        }
        // 2. Доставка.
        let pending = storage::alerts::pending(&mut conn, 20).await?;
        if pending.is_empty() {
            return Ok(());
        }
        let staff = storage::staff::list(&mut conn).await?;
        let recipients: Vec<&StaffMember> = staff.iter().filter(|m| m.active()).collect();
        drop(conn);
        for alert in pending {
            let screen = self.look().screen(&self.render_alert(&alert));
            let known = self.state(|st| st.delivered.get(&alert.id).cloned().unwrap_or_default());
            let mut sent = Vec::new();
            for m in &recipients {
                let chat = m.tg_user_id;
                let previous = known.iter().find(|(c, _)| *c == chat).map(|(_, id)| *id);
                let result = match previous {
                    Some(message_id) => {
                        let mut request =
                            EditMessageText::html(chat, message_id, screen.text.clone());
                        request.reply_markup = screen.markup();
                        self.api
                            .edit_message_text(&request)
                            .await
                            .map(|_| message_id)
                    }
                    None => {
                        let mut request = SendMessage::html(chat, screen.text.clone());
                        request.reply_markup = screen.markup().map(ReplyMarkup::Inline);
                        // Важное — со звуком, остальное — тихо.
                        request.disable_notification = Some(alert.level < Level::Warn);
                        self.api.send_message(&request).await.map(|m| m.message_id)
                    }
                };
                match result {
                    Ok(id) => sent.push((chat, id)),
                    Err(e) if e.is_not_modified() => {
                        if let Some(id) = previous {
                            sent.push((chat, id));
                        }
                    }
                    // Сотрудник не начинал диалог с пультом или заблокировал его.
                    Err(e) => tracing::info!(chat, error = %e, "alert not delivered"),
                }
            }
            let first = sent.first().map(|(_, id)| *id);
            self.state(|st| {
                st.delivered.insert(alert.id, sent);
            });
            let mut conn = self.pool.acquire().await?;
            storage::alerts::mark_delivered(&mut conn, alert.id, first).await?;
        }
        // Новое событие — главный экран правится сразу (макет: «и сразу при событии»).
        if let Err(e) = self.refresh_dashes().await {
            tracing::warn!(error = %e, "ops dash refresh failed");
        }
        Ok(())
    }

    pub(crate) fn render_alert(&self, a: &AlertRow) -> Screen {
        let t = &self.texts;
        let p = &a.payload;
        let dir_name = |key: &str| {
            s(p, key)
                .parse::<Direction>()
                .map_or_else(|_| esc(&s(p, key)), |d| self.dir_name(d))
        };
        let mut screen = match a.title.as_str() {
            "manual_review" => {
                let id = p["id"].as_i64().unwrap_or_default();
                let amount = s(p, "amount");
                Screen::new(t.f(
                    "alert.manual_review",
                    &[
                        ("id", &id),
                        ("direction", &dir_name("direction")),
                        (
                            "amount",
                            &if amount.is_empty() {
                                t.t("common.none")
                            } else {
                                usdt(dec(p, "amount"))
                            },
                        ),
                        ("reason", &esc(&s(p, "reason"))),
                    ],
                ))
                .row(vec![
                    btn(t.t("alert.b_open"), &Cb::Order(id)),
                    btn(t.t("alert.b_ack"), &Cb::Ack(a.id)),
                ])
            }
            "promo_hint" => {
                let d = s(p, "direction").parse::<Direction>().ok();
                let (source, target) = d.map_or((String::new(), String::new()), |d| {
                    (
                        self.platform_name(d.source()),
                        self.platform_name(d.target()),
                    )
                });
                let mut sc = Screen::new(t.f(
                    "alert.promo_hint",
                    &[
                        ("source", &source),
                        ("source_available", &usdt(dec(p, "source_available"))),
                        ("source_target", &usdt(dec(p, "source_target"))),
                        ("deficit", &esc(&s(p, "deficit_pct"))),
                        ("target", &target),
                        ("target_available", &usdt(dec(p, "target_available"))),
                        ("target_target", &usdt(dec(p, "target_target"))),
                        ("name", &dir_name("direction")),
                        ("fee", &pct(dec(p, "fee_pct"))),
                        ("room", &usdt(dec(p, "room"))),
                    ],
                ));
                let mut row = Vec::new();
                if let Some(d) = d {
                    row.push(btn(t.t("alert.b_promo_on"), &Cb::PromoSwitch(d, true)));
                }
                row.push(btn(t.t("alert.b_ack"), &Cb::Ack(a.id)));
                sc = sc.row(row);
                sc
            }
            "promo_started" | "promo_stopped" => Screen::new(t.f(
                if a.title == "promo_started" {
                    "alert.promo_started"
                } else {
                    "alert.promo_stopped"
                },
                &[
                    ("name", &dir_name("direction")),
                    ("fee", &pct(dec(p, "fee_pct"))),
                    ("room", &usdt(dec(p, "room"))),
                ],
            ))
            .row(vec![
                btn(t.t("menu.promo"), &Cb::Promo),
                btn(t.t("alert.b_ack"), &Cb::Ack(a.id)),
            ]),
            "balance_mismatch" => mismatch_screen(t, p, a.id),
            "reconcile_stalled" => stalled_screen(t, p, a.id),
            "balance_unreadable" => {
                let name = p["platform"]
                    .as_str()
                    .and_then(domain::Platform::from_db_name)
                    .map_or_else(|| esc(&s(p, "platform")), |x| self.platform_name(x));
                Screen::new(t.f(
                    "alert.balance_unreadable",
                    &[("name", &name), ("error", &esc(&s(p, "error")))],
                ))
                .row(vec![
                    btn(t.t("alert.b_recheck"), &Cb::Reconcile),
                    btn(t.t("alert.b_ack"), &Cb::Ack(a.id)),
                ])
            }
            "invest_withdrawal" | "invest_manual" | "invest_failed" => {
                let id = p["move_id"].as_i64().unwrap_or_default();
                let kind = t.t(if s(p, "kind") == "deposit" {
                    "invest.kind_deposit"
                } else {
                    "invest.kind_withdrawal"
                });
                let wallet = p["platform"]
                    .as_str()
                    .and_then(domain::Platform::from_db_name)
                    .map_or_else(|| esc(&s(p, "platform")), |x| self.platform_name(x));
                let amount = p["amount"]
                    .as_str()
                    .and_then(|a| a.parse().ok())
                    .map_or_else(|| "—".to_owned(), usdt);
                let key = match a.title.as_str() {
                    "invest_withdrawal" => "alert.invest_withdrawal",
                    "invest_manual" => "alert.invest_manual",
                    _ => "alert.invest_failed",
                };
                let mut sc = Screen::new(t.f(
                    key,
                    &[
                        ("id", &id),
                        ("user", &p["user_id"].as_i64().unwrap_or_default()),
                        ("kind", &kind),
                        ("wallet", &wallet),
                        ("amount", &amount),
                        ("note", &esc(&s(p, "note"))),
                    ],
                ));
                if a.title == "invest_withdrawal" {
                    sc = sc.row(vec![
                        btn(t.t("invest.b_approve"), &Cb::InvestApprove(id)),
                        btn(t.t("invest.b_reject"), &Cb::InvestReject(id)),
                    ]);
                }
                sc.row(vec![
                    btn(t.t("alert.b_open"), &Cb::InvestMove(id)),
                    btn(t.t("alert.b_ack"), &Cb::Ack(a.id)),
                ])
            }
            _ => Screen::new(t.f(
                "alert.generic",
                &[
                    ("title", &esc(&a.title)),
                    ("payload", &esc(&ui::html::clip(&p.to_string(), 600))),
                ],
            ))
            .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(a.id))]),
        };
        if a.count > 1 {
            screen.text.push_str(&t.f(
                "alert.repeat",
                &[("n", &a.count), ("at", &crate::views::when(a.last_at))],
            ));
        }
        screen
    }

    /// «Принял»: событие закрыто, у всех копий — отметка и без кнопки «Принял».
    pub(crate) async fn ack(&self, staff: &StaffMember, id: i64) -> Result<(), OpsError> {
        let alert = {
            let mut conn = self.pool.acquire().await?;
            storage::alerts::ack(&mut conn, id, staff.tg_user_id).await?;
            storage::alerts::get(&mut conn, id).await?
        };
        let Some(alert) = alert else {
            return Ok(());
        };
        let mut screen = self.render_alert(&alert);
        screen.text.push_str(
            &self
                .texts
                .f("alert.acked", &[("name", &esc(&staff.display_name))]),
        );
        for row in &mut screen.keyboard {
            row.retain(|b| {
                !matches!(&b.action, botapi::InlineButtonAction::CallbackData(d) if d.starts_with("ack:"))
            });
        }
        screen.keyboard.retain(|r| !r.is_empty());
        let copies = self.state(|st| st.delivered.remove(&id).unwrap_or_default());
        for (chat, message_id) in copies {
            let mut request = EditMessageText::html(chat, message_id, screen.text.clone());
            request.reply_markup = screen.markup();
            if let Err(e) = self.api.edit_message_text(&request).await
                && !e.is_not_modified()
            {
                tracing::debug!(error = %e, "ack edit failed");
            }
        }
        Ok(())
    }
}

/// Макет «Недостача»: «xRocket: меньше на 12.00 · Леджер … · реально … · ub-1 · Выплаты в
/// xRocket на паузе. Это вы выводили?» — «Да, это я» (провести вывод владельца) и красная
/// «Нет, разбираться». Излишек — то же с «Это вы пополняли?».
pub fn mismatch_screen(t: &crate::Texts, p: &serde_json::Value, id: i64) -> Screen {
    let name = p["platform"]
        .as_str()
        .and_then(domain::Platform::from_db_name)
        .map_or_else(
            || esc(p["platform"].as_str().unwrap_or_default()),
            |x| t.t(&format!("names.{}", x.db_name())),
        );
    let num = |k: &str| -> domain::Decimal {
        p[k].as_str()
            .and_then(|v| v.parse().ok())
            .unwrap_or_default()
    };
    let diff = num("diff");
    let account = p["account"]
        .as_str()
        .map(|l| t.f("alert.account", &[("label", &esc(l))]))
        .unwrap_or_default();
    let paused = if p["paused"].as_bool() == Some(true) {
        t.f("alert.paused", &[("name", &name)])
    } else {
        String::new()
    };
    let short = diff < domain::Decimal::ZERO;
    let args: [(&str, &dyn std::fmt::Display); 6] = [
        ("name", &name),
        ("amount", &crate::dash::amount(diff.abs())),
        ("expected", &crate::dash::amount(num("expected"))),
        ("actual", &crate::dash::amount(num("actual"))),
        ("account", &account),
        ("paused", &paused),
    ];
    let text = match p["unsettled"].as_array().filter(|a| !a.is_empty()) {
        None => t.f(
            if short {
                "alert.balance_short"
            } else {
                "alert.balance_extra"
            },
            &args,
        ),
        // Сверка учла операции с неясным исходом (шаг 1.5): «Ожидалось» — их вариант.
        Some(ops) => {
            let line = unsettled_line(t, ops, short, p["below_all"].as_bool() == Some(true));
            let mut all: Vec<(&str, &dyn std::fmt::Display)> = args.to_vec();
            all.push(("unsettled", &line));
            t.f(
                if short {
                    "alert.balance_short_unsettled"
                } else {
                    "alert.balance_extra_unsettled"
                },
                &all,
            )
        }
    };
    let no = btn(t.t("alert.b_no"), &Cb::Investigate(id));
    Screen::new(text)
        .row(vec![btn(t.t("alert.b_yes"), &Cb::Capital(id))])
        .row(vec![if short {
            no.style(botapi::ButtonStyle::Danger)
        } else {
            no
        }])
}

/// Название операции в тексте события: `prefix` — `op` («выплата») или `opc` («чек выплаты»).
fn op_noun(t: &crate::Texts, prefix: &str, kind: &str) -> String {
    let key = format!("alert.{prefix}_{kind}");
    if t.has(&key) {
        t.t(&key)
    } else {
        t.t(&format!("alert.{prefix}_other"))
    }
}

/// Строка «Учтена неясная выплата K7M2… (97.50): …» под тревогой о недостаче.
fn unsettled_line(t: &crate::Texts, ops: &[Value], short: bool, below_all: bool) -> String {
    let one = |o: &Value| {
        t.f(
            "alert.unsettled_op",
            &[
                ("what", &op_noun(t, "op", &s(o, "kind"))),
                ("public", &esc(&s(o, "public"))),
                (
                    "sum",
                    &o["amount"]
                        .as_str()
                        .and_then(|a| a.parse().ok())
                        .map_or_else(|| "—".to_owned(), crate::dash::amount),
                ),
            ],
        )
    };
    if let [o] = ops
        && short
        && below_all
        && s(o, "kind") == "create_payout_check"
    {
        // Текст макета: одна неясная выплата, денег меньше обоих исходов.
        let op = format!(
            "<code>{}</code> ({})",
            esc(&s(o, "public")),
            o["amount"]
                .as_str()
                .and_then(|a| a.parse().ok())
                .map_or_else(|| "—".to_owned(), crate::dash::amount)
        );
        return t.f("alert.unsettled_payout_short", &[("op", &op)]);
    }
    let list = ops.iter().map(one).collect::<Vec<_>>().join(", ");
    let head = t.f(
        if ops.len() == 1 {
            "alert.unsettled_one"
        } else {
            "alert.unsettled_many"
        },
        &[("op", &list)],
    );
    let tail = t.t(match (short, below_all) {
        (true, true) => "alert.unsettled_short_all",
        (true, false) => "alert.unsettled_short_between",
        (false, _) => "alert.unsettled_extra",
    });
    format!("{head}{tail}")
}

/// «Сверка не проходит» (шаг 1.5): сверка кошелька пропускается дольше порога; какие заявки её
/// держат. Кнопки — «Разбор», «Сверить снова», «Принял».
pub fn stalled_screen(t: &crate::Texts, p: &Value, id: i64) -> Screen {
    let name = p["platform"]
        .as_str()
        .and_then(domain::Platform::from_db_name)
        .map_or_else(
            || esc(p["platform"].as_str().unwrap_or_default()),
            |x| t.t(&format!("names.{}", x.db_name())),
        );
    let m = p["minutes"].as_i64().unwrap_or_default();
    let minutes = if m < 60 {
        t.f("alert.minutes", &[("n", &m)])
    } else {
        t.f(
            "alert.hours",
            &[("h", &(m / 60)), ("m", &format!("{:02}", m % 60))],
        )
    };
    let since = p["since"]
        .as_str()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map_or_else(
            || "—".to_owned(),
            |at| crate::dash::hhmm(at.with_timezone(&chrono::Utc)),
        );
    let ops = p["ops"].as_array().cloned().unwrap_or_default();
    let reason = s(p, "reason");
    let mut why = if reason == "selftest" {
        t.t("alert.stalled_selftest")
    } else if reason == "too_many" {
        t.t("alert.stalled_too_many")
    } else if ops.len() == 1 {
        t.t("alert.stalled_hold_one")
    } else {
        t.t("alert.stalled_hold_many")
    };
    if reason != "selftest" && !ops.is_empty() {
        for o in &ops {
            let state = t.t(if s(o, "status") == "dispatched" {
                "alert.stalled_in_progress"
            } else if o["amount"].is_null() {
                "alert.stalled_no_amount"
            } else {
                "alert.stalled_unknown"
            });
            let what = op_noun(t, "opc", &s(o, "kind"));
            why.push('\n');
            why.push_str(&match o["public"].as_str() {
                Some(public) => t.f(
                    "alert.stalled_op",
                    &[("public", &esc(public)), ("what", &what), ("state", &state)],
                ),
                None => t.f(
                    "alert.stalled_op_no_order",
                    &[
                        ("id", &o["id"].as_i64().unwrap_or_default()),
                        ("what", &what),
                        ("state", &state),
                    ],
                ),
            });
        }
        why.push('\n');
        why.push_str(&t.t(if ops.len() == 1 {
            "alert.stalled_end_one"
        } else {
            "alert.stalled_end_many"
        }));
    }
    let text = t.f(
        "alert.reconcile_stalled",
        &[
            ("name", &name),
            ("minutes", &minutes),
            ("since", &since),
            ("why", &why),
        ],
    );
    Screen::new(text)
        .row(vec![
            btn(t.t("alert.b_queue"), &Cb::Queue).style(botapi::ButtonStyle::Primary),
            btn(t.t("alert.b_recheck"), &Cb::Reconcile),
        ])
        .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
}
