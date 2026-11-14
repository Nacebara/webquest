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

fn platform_of(name: &str) -> Option<domain::Platform> {
    match name {
        "cryptobot" => Some(domain::Platform::CryptoBot),
        "xrocket" => Some(domain::Platform::XRocket),
        _ => None,
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
            storage::alerts::ensure_condition(
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
            }
        }
        // Заявка ушла с разбора после «Принял» — состояние ушло: снова попадёт — новая тревога.
        let acked: Vec<(String, i64)> = sqlx::query_as(
            "SELECT dedup_key, (payload->>'id')::bigint FROM alerts \
              WHERE title = 'manual_review' AND acked_by IS NOT NULL AND resolved_at IS NULL",
        )
        .fetch_all(&mut *conn)
        .await?;
        for (key, id) in acked {
            if !queue.iter().any(|q| q.id == id) {
                storage::alerts::resolve(&mut conn, &key).await?;
            }
        }
        // 1б. Движок не проходит круг дольше `engine_down_after` (шаг 2.8): тревога, пока не
        // заработает. Минуты считаются при показе — от `since`.
        let after = self.cfg.engine_down_after;
        let quiet = match self.engine.last_beat() {
            Some(t) => (chrono::Utc::now() - t).to_std().unwrap_or_default(),
            None => self.started.elapsed(),
        };
        if quiet >= after && self.started.elapsed() >= after {
            let since = chrono::Utc::now() - chrono::Duration::from_std(quiet).unwrap_or_default();
            storage::alerts::ensure_condition(
                &mut conn,
                &NewAlert {
                    level: Level::Critical,
                    topic: Topic::Errors,
                    dedup_key: "engine_down",
                    title: "engine_down",
                    payload: json!({
                        "since": since.to_rfc3339(),
                        "error": self.engine.last_error(),
                    }),
                },
            )
            .await?;
        } else if quiet < after {
            storage::alerts::resolve(&mut conn, "engine_down").await?;
        }
        // 2. Доставка: новые и изменившиеся — новым сообщением или правкой копии; критичные,
        // которые никто не принял 15 минут, — повтор новым сообщением со звуком вместо прежнего.
        let pending = storage::alerts::pending(&mut conn, 20).await?;
        let repeats = storage::alerts::due_repeats(&mut conn, self.cfg.repeat_every, 20).await?;
        if pending.is_empty() && repeats.is_empty() {
            return Ok(());
        }
        let staff = storage::staff::list(&mut conn).await?;
        let recipients: Vec<&StaffMember> = staff.iter().filter(|m| m.active()).collect();
        drop(conn);
        for alert in pending {
            self.deliver(alert, &recipients, false).await?;
        }
        for alert in repeats {
            self.deliver(alert, &recipients, true).await?;
        }
        // Новое событие — главный экран правится сразу (макет: «и сразу при событии»).
        if let Err(e) = self.refresh_dashes().await {
            tracing::warn!(error = %e, "ops dash refresh failed");
        }
        Ok(())
    }

    /// Показать тревогу каждому сотруднику. Копия уже есть — правка (тихо); нет — новое
    /// сообщение (важное — со звуком). `repeat` — повтор критичной: новое сообщение со звуком,
    /// прежнее удаляется, в чате всегда одна копия. Копии — в базе: переживают перезапуск.
    async fn deliver(
        &self,
        mut alert: AlertRow,
        recipients: &[&StaffMember],
        repeat: bool,
    ) -> Result<(), OpsError> {
        let copies = {
            let mut conn = self.pool.acquire().await?;
            if repeat {
                storage::alerts::bump_repeat(&mut conn, alert.id).await?;
                alert.repeats += 1;
            }
            storage::alerts::copies(&mut conn, alert.id).await?
        };
        let screen = self.look().screen(&self.render_alert(&alert));
        let mut first = None;
        for m in recipients {
            let chat = m.tg_user_id;
            let previous = copies
                .iter()
                .find(|c| c.chat_id == chat)
                .map(|c| c.message_id);
            if let (Some(message_id), false) = (previous, repeat) {
                let mut request = EditMessageText::html(chat, message_id, screen.text.clone());
                request.reply_markup = screen.markup();
                match self.api.edit_message_text(&request).await {
                    Ok(_) => {
                        first.get_or_insert(message_id);
                        continue;
                    }
                    Err(e) if e.is_not_modified() => {
                        first.get_or_insert(message_id);
                        continue;
                    }
                    // Сотрудник удалил сообщение — пришлём новое.
                    Err(e) if e.is_message_gone() => {}
                    Err(e) => {
                        tracing::info!(chat, error = %e, "alert not updated");
                        continue;
                    }
                }
            }
            let mut request = SendMessage::html(chat, screen.text.clone());
            request.reply_markup = screen.markup().map(ReplyMarkup::Inline);
            // Важное — со звуком, остальное — тихо.
            request.disable_notification = Some(alert.level < Level::Warn);
            match self.api.send_message(&request).await {
                Ok(sent) => {
                    first.get_or_insert(sent.message_id);
                    let mut conn = self.pool.acquire().await?;
                    storage::alerts::save_copy(&mut conn, alert.id, chat, sent.message_id).await?;
                    drop(conn);
                    if let (Some(old), true) = (previous, repeat)
                        && let Err(e) = self.api.delete_message(chat, old).await
                    {
                        tracing::debug!(chat, error = %e, "previous alert copy not deleted");
                    }
                }
                // Сотрудник не начинал диалог с пультом или заблокировал его.
                Err(e) => tracing::info!(chat, error = %e, "alert not delivered"),
            }
        }
        let mut conn = self.pool.acquire().await?;
        storage::alerts::mark_delivered(&mut conn, alert.id, first).await?;
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
        let critical = critical_screen(t, &a.title, p, a.id, a.first_at);
        let mut screen = match a.title.as_str() {
            _ if critical.is_some() => critical.unwrap_or_default(),
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
        if a.repeats > 0 && a.acked_at.is_none() {
            screen.text.push_str(&t.f(
                "alert.unacked",
                &[("n", &a.repeats), ("since", &crate::dash::hhmm(a.first_at))],
            ));
        }
        screen
    }

    /// «Сохранить фикстуру»: ответ кошелька из тревоги «Незнакомый ответ» — готовой строкой для
    /// `crates/parsers/tests/fixtures` (CLAUDE.md п. 13: словарь не подгоняют, добавляют
    /// фикстуру). Отдельным сообщением: тревога остаётся на месте.
    pub(crate) async fn fixture(&self, chat: i64, id: i64) -> Result<(), OpsError> {
        let alert = {
            let mut conn = self.pool.acquire().await?;
            storage::alerts::get(&mut conn, id).await?
        };
        let Some(alert) = alert.filter(|a| a.title == "unknown_reply") else {
            return Ok(());
        };
        let case = json!({
            "platform": alert.payload["platform"],
            "text": alert.payload["text"],
            "class": "?",
        });
        let text = self
            .texts
            .f("alert.fixture", &[("json", &esc(&case.to_string()))]);
        self.api
            .send_message(&SendMessage::html(chat, text))
            .await?;
        Ok(())
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
        // Макет «Алерт и «Принял»»: первая строка тревоги с серой точкой и «Принял Анна · 14:47 —
        // повторы остановлены»; остальные кнопки остаются, «Принял» уходит.
        let full = self.render_alert(&alert);
        let head = full.text.lines().next().unwrap_or_default();
        let head = head
            .strip_prefix("{e:")
            .and_then(|r| r.split_once("} "))
            .map_or(head, |(_, rest)| rest);
        let mut screen = full.clone();
        screen.text = self.texts.f(
            "alert.acked",
            &[
                ("head", &head),
                ("name", &esc(&staff.display_name)),
                (
                    "at",
                    &crate::dash::hhmm(alert.acked_at.unwrap_or_else(chrono::Utc::now)),
                ),
            ],
        );
        let ack_data = Cb::Ack(id).encode();
        for row in &mut screen.keyboard {
            row.retain(|b| {
                !matches!(&b.action, botapi::InlineButtonAction::CallbackData(d) if *d == ack_data)
            });
        }
        screen.keyboard.retain(|r| !r.is_empty());
        let screen = self.look().screen(&screen);
        let copies = {
            let mut conn = self.pool.acquire().await?;
            storage::alerts::copies(&mut conn, id).await?
        };
        for c in copies {
            let mut request = EditMessageText::html(c.chat_id, c.message_id, screen.text.clone());
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

/// Критичные тревоги шага 2.8 (утверждены владельцем 14.11 и в макете пульта): вход сброшен,
/// аккаунт ограничен, долгая пауза Telegram, движок не работает, бот запущен дважды, незнакомый
/// ответ, стоп-кран. `None` — тревога другого вида.
pub fn critical_screen(
    t: &crate::Texts,
    title: &str,
    p: &Value,
    id: i64,
    first_at: chrono::DateTime<chrono::Utc>,
) -> Option<Screen> {
    let screen = match title {
        "login_lost" | "account_restricted" | "flood_long" => {
            let account = esc(&s(p, "account"));
            let platforms = p["platforms"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().and_then(platform_of))
                        .map(|x| t.t(&format!("names.{}", x.db_name())))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let route = t.f("alert.route_closed", &[("platforms", &platforms)]);
            let ub = p["userbot_id"].as_i64().and_then(|x| i16::try_from(x).ok());
            match title {
                "login_lost" => {
                    let mut sc = Screen::new(t.f(
                        "alert.login_lost",
                        &[("account", &account), ("route", &route)],
                    ))
                    .row(vec![
                        btn(t.t("alert.b_relogin"), &Cb::Bots).style(botapi::ButtonStyle::Primary),
                    ]);
                    let mut second = vec![btn(t.t("alert.b_replace"), &Cb::Bots)];
                    if let Some(ub) = ub {
                        second.push(btn(t.t("alert.b_disable"), &Cb::BotPause(ub, true)));
                    }
                    sc = sc.row(second);
                    sc.row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
                }
                "account_restricted" => Screen::new(t.f(
                    "alert.account_restricted",
                    &[("account", &account), ("route", &route)],
                ))
                .row(vec![
                    btn(t.t("alert.b_replace_account"), &Cb::Bots)
                        .style(botapi::ButtonStyle::Primary),
                    btn(t.t("alert.b_accounts"), &Cb::Bots),
                ])
                .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))]),
                _ => {
                    let minutes = p["wait_secs"].as_u64().unwrap_or_default().div_ceil(60);
                    let until = s(p, "until")
                        .parse::<chrono::DateTime<chrono::Utc>>()
                        .map(crate::dash::hhmm)
                        .unwrap_or_default();
                    Screen::new(t.f(
                        "alert.flood_long",
                        &[
                            ("account", &account),
                            ("minutes", &minutes),
                            ("until", &until),
                            ("platforms", &platforms),
                        ],
                    ))
                    .row(vec![btn(t.t("alert.b_accounts"), &Cb::Bots)])
                    .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
                }
            }
        }
        "engine_down" => {
            let since = s(p, "since")
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap_or(first_at);
            let minutes = (chrono::Utc::now() - since).num_minutes().max(1);
            let error = match p["error"].as_str() {
                Some(e) if !e.is_empty() => t.f(
                    "alert.engine_error",
                    &[("error", &esc(&ui::html::clip(e, 300)))],
                ),
                _ => String::new(),
            };
            Screen::new(t.f(
                "alert.engine_down",
                &[("minutes", &minutes), ("error", &error)],
            ))
            .row(vec![
                btn(t.t("alert.b_system"), &Cb::System).style(botapi::ButtonStyle::Primary),
            ])
            .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
        }
        "second_instance" => Screen::new(t.t("alert.second_instance"))
            .row(vec![btn(t.t("alert.b_system"), &Cb::System)])
            .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))]),
        "unknown_reply" => {
            let platform = s(p, "platform");
            let name = platform_of(&platform).map_or_else(
                || esc(&platform),
                |x| t.t(&format!("names.{}", x.db_name())),
            );
            let at = s(p, "at")
                .parse::<chrono::DateTime<chrono::Utc>>()
                .map(crate::dash::hhmm)
                .unwrap_or_default();
            let direction = s(p, "direction").parse::<Direction>().ok();
            let mut sc = Screen::new(t.f(
                "alert.unknown_reply",
                &[
                    ("platform", &name),
                    ("at", &at),
                    ("text", &esc(&s(p, "text"))),
                    (
                        "direction",
                        &direction.map_or_else(
                            || esc(&s(p, "direction")),
                            |d| t.t(&format!("names.{}", d.code())),
                        ),
                    ),
                    ("public", &esc(&s(p, "public"))),
                ],
            ))
            .row(vec![btn(t.t("alert.b_fixture"), &Cb::Fixture(id))]);
            let mut second = Vec::new();
            if let Some(d) = direction {
                second.push(btn(t.t("alert.b_unpause"), &Cb::DirClearAuto(d)));
            }
            if let Some(order) = p["order"].as_i64() {
                second.push(btn(t.t("alert.b_order"), &Cb::Order(order)));
            }
            if !second.is_empty() {
                sc = sc.row(second);
            }
            sc.row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
        }
        "kill_switch" => {
            let at = s(p, "at")
                .parse::<chrono::DateTime<chrono::Utc>>()
                .map(crate::dash::hhmm)
                .unwrap_or_default();
            Screen::new(t.f(
                "alert.kill_switch",
                &[("by", &esc(&s(p, "by"))), ("at", &at)],
            ))
            .row(vec![btn(
                t.t("alert.b_maint"),
                &Cb::Toggle(crate::cb::Flag::Maintenance),
            )])
            .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
        }
        _ => return None,
    };
    Some(screen)
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
