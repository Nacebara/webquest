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
            "balance_mismatch" => {
                let platform = p["platform"]
                    .as_str()
                    .and_then(domain::Platform::from_db_name);
                let name =
                    platform.map_or_else(|| esc(&s(p, "platform")), |x| self.platform_name(x));
                let diff = dec(p, "diff");
                let paused = if p["paused"].as_bool() == Some(true) {
                    t.t("alert.paused")
                } else {
                    String::new()
                };
                let mut sc = Screen::new(t.f(
                    "alert.balance_mismatch",
                    &[
                        ("name", &name),
                        ("actual", &usdt(dec(p, "actual"))),
                        ("expected", &usdt(dec(p, "expected"))),
                        (
                            "diff",
                            &format!(
                                "{}{}",
                                if diff > domain::Decimal::ZERO {
                                    "+"
                                } else {
                                    ""
                                },
                                usdt(diff)
                            ),
                        ),
                        ("paused", &paused),
                    ],
                ));
                let capital = if diff > domain::Decimal::ZERO {
                    t.f("alert.b_capital_in", &[("amount", &usdt(diff))])
                } else {
                    t.f("alert.b_capital_out", &[("amount", &usdt(-diff))])
                };
                sc = sc.row(vec![btn(capital, &Cb::Capital(a.id))]).row(vec![
                    btn(t.t("alert.b_recheck"), &Cb::Reconcile),
                    btn(t.t("alert.b_ack"), &Cb::Ack(a.id)),
                ]);
                sc
            }
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
