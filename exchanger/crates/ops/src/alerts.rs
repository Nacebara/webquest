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
use storage::staff::{Role, StaffMember};

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

/// П-10 (картинка принята владельцем 18.11): кому тревога и со звуком ли. `None` — роли не
/// приходит вовсе: решить её она не может (П-11). Правила в коде, без настроек; экран «Алерты»
/// из макета (тихие часы, темы) — потом.
pub(crate) fn audience(title: &str, level: Level, role: Role) -> Option<bool> {
    let admin = role >= Role::Admin;
    match title {
        // Разбор и то, что останавливает всё: всем, со звуком.
        "manual_review" | "unknown_reply" | "engine_down" | "kill_switch" | "second_instance" => {
            Some(true)
        }
        // Деньги и аккаунты: админу и владельцу, со звуком.
        "balance_mismatch" | "reconcile_stalled" | "balance_unreadable" | "login_lost"
        | "account_restricted" | "flood_long" | "new_login" | "backup_failed" => {
            admin.then_some(true)
        }
        t if t.starts_with("invest_") => admin.then_some(true),
        // Подсказка разработчику, решать нечего (П-19.2, картинка link-missing.png): владельцу, тихо.
        "check_link_missing" => (role == Role::Owner).then_some(false),
        // Выгодный курс и перекос резерва: админу и владельцу, тихо.
        "promo_hint" | "promo_started" | "promo_stopped" => admin.then_some(false),
        _ => Some(level >= Level::Warn),
    }
}

/// Б-19.3: кому можно показать тревогу по номеру — тем, кому она приходит. Недостачу видит каждая
/// роль: она на главном экране и в «Резервах» у всех (утверждённые экраны «Нужно решить» и
/// «Резервы и сверка»), кнопка «Недостача» ведёт сюда.
pub(crate) fn may_see(a: &AlertRow, role: Role) -> bool {
    audience(&a.title, a.level, role).is_some() || a.title == "balance_mismatch"
}

impl Inner {
    pub(crate) async fn alerts_once(&self) -> Result<(), OpsError> {
        let mut conn = self.pool.acquire().await?;
        // 1. Ручной разбор: событие на каждую заявку в очереди.
        let queue = storage::admin::review_queue(&mut conn).await?;
        let ids: Vec<i64> = queue.iter().map(|q| q.id).collect();
        let replies = storage::admin::last_wallet_replies(&mut conn, &ids).await?;
        for item in &queue {
            let key = format!("manual_review:{}", item.id);
            let reply = replies.iter().find(|(id, _)| *id == item.id).map(
                |(_, m)| json!({ "who": m.platform, "at": m.at.to_rfc3339(), "text": m.text }),
            );
            storage::alerts::ensure_condition(
                &mut conn,
                &NewAlert {
                    level: Level::Critical,
                    topic: Topic::Errors,
                    dedup_key: &key,
                    title: "manual_review",
                    payload: json!({
                        "id": item.id,
                        "public": item.public_id,
                        "user": item.user_id,
                        "direction": item.direction.code(),
                        "amount": item.intake_amount.or(item.payout_amount).map(|a| a.to_string()),
                        "amount_in": item.intake_amount.map(|a| a.to_string()),
                        "amount_out": item.payout_amount.map(|a| a.to_string()),
                        "reason": item.manual_reason,
                        "excess": item.excess.map(|a| a.to_string()),
                        "since": item.since.to_rfc3339(),
                        "reply": reply,
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
        drop(conn);
        // 1в. П-10: решённое — правкой всех копий («Решено: Игнат — вернул(а) деньги»).
        if let Err(e) = self.close_resolved().await {
            tracing::warn!(error = %e, "resolved alert copies not updated");
        }
        let mut conn = self.pool.acquire().await?;
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
        let _turn = self.alert_turn.lock().await;
        let copies = {
            let mut conn = self.pool.acquire().await?;
            if repeat {
                // Повтор засчитывается, только если тревогу ещё никто не принял.
                match storage::alerts::bump_repeat(&mut conn, alert.id).await? {
                    Some(n) => alert.repeats = n,
                    None => return Ok(()),
                }
            }
            storage::alerts::copies(&mut conn, alert.id).await?
        };
        let screen = self.look().screen(&self.render_alert(&alert));
        let mut first = None;
        // Отправка не удалась по временной причине (связь, 5xx, 429): повторить на следующем
        // круге. Сотрудник заблокировал бота (403) — так и останется, ждать его незачем (С-19.1).
        let mut transient = false;
        for m in recipients {
            let chat = m.tg_user_id;
            let Some(sound) = audience(&alert.title, alert.level, m.role) else {
                continue;
            };
            // Позвал админа (П-3): его копию не трогаем и не повторяем.
            if copies
                .iter()
                .any(|c| c.chat_id == chat && c.muted_at.is_some())
            {
                continue;
            }
            let previous = copies
                .iter()
                .find(|c| c.chat_id == chat && c.message_id != 0)
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
                        transient |= !e.is_forbidden();
                        continue;
                    }
                }
            }
            let mut request = SendMessage::html(chat, screen.text.clone());
            request.reply_markup = screen.markup().map(ReplyMarkup::Inline);
            // Звук — по роли (П-10).
            request.disable_notification = Some(!sound);
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
                Err(e) => {
                    tracing::info!(chat, error = %e, "alert not delivered");
                    transient |= !e.is_forbidden();
                }
            }
        }
        // Ни одной копии из-за сбоя связи: тревога остаётся в очереди и уйдёт на следующем
        // круге (С-19.1). Иначе она считалась бы доставленной и не пришла бы никогда.
        if first.is_none() && transient {
            tracing::warn!(
                alert = alert.id,
                title = %alert.title,
                "alert reached nobody, will retry"
            );
            return Ok(());
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
            "manual_review" => review_alert_screen(t, p, a.id),
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
        let Some(alert) =
            alert.filter(|a| matches!(a.title.as_str(), "unknown_reply" | "check_link_missing"))
        else {
            return Ok(());
        };
        let text = if alert.title == "check_link_missing" {
            // Устройство сообщения (виды кнопок, хосты, длина текста) — без ссылок и кодов (П-19.2).
            let case = json!({
                "platform": alert.payload["platform"],
                "shape": alert.payload["shape"],
            });
            self.texts
                .f("alert.fixture_shape", &[("json", &esc(&case.to_string()))])
        } else {
            let case = json!({
                "platform": alert.payload["platform"],
                "text": alert.payload["text"],
                "class": "?",
            });
            self.texts
                .f("alert.fixture", &[("json", &esc(&case.to_string()))])
        };
        self.api
            .send_message(&SendMessage::html(chat, text))
            .await?;
        Ok(())
    }

    /// Первая строка тревоги без значка — заголовок серой копии («Принял», «Решено»).
    fn alert_head(&self, a: &AlertRow) -> String {
        if a.title == "manual_review"
            && let Some(public) = a.payload["public"].as_str()
        {
            return self
                .texts
                .f("alert.review_head", &[("public", &esc(public))]);
        }
        let full = self.render_alert(a);
        let head = full.text.lines().next().unwrap_or_default();
        head.strip_prefix("{e:")
            .and_then(|r| r.split_once("} "))
            .map_or(head, |(_, rest)| rest)
            .to_owned()
    }

    /// Кто решил тревогу: последнее действие персонала по её предмету после её появления
    /// (заявка — решение по ней, кошелёк — взнос, вывод или списание).
    async fn resolution(
        &self,
        conn: &mut sqlx::PgConnection,
        a: &AlertRow,
    ) -> Result<Option<storage::audit::AuditRow>, OpsError> {
        let (target, prefix) = match a.title.as_str() {
            "manual_review" => match a.payload["id"].as_i64() {
                Some(id) => (("order", id.to_string()), "order."),
                None => return Ok(None),
            },
            "balance_mismatch" => match a.payload["platform"].as_str() {
                Some(p) => (("wallet", p.to_owned()), "capital."),
                None => return Ok(None),
            },
            "kill_switch" => (("settings", "kill_switch_payouts".to_owned()), "settings."),
            "new_login" => match a.payload["account"].as_str() {
                Some(account) => (("userbot", account.to_owned()), "userbot."),
                None => return Ok(None),
            },
            _ => return Ok(None),
        };
        Ok(
            storage::audit::recent(conn, 20, None, Some((target.0, &target.1)))
                .await?
                .into_iter()
                .find(|r| {
                    r.at >= a.first_at
                        && r.action.starts_with(prefix)
                        && !matches!(r.action.as_str(), "order.take" | "order.call_admin")
                }),
        )
    }

    /// П-10: копии решённых тревог — серые, без кнопок, с тем, кто и что сделал. Каждая копия
    /// правится один раз (`alert_copies.closed_at`).
    pub(crate) async fn close_resolved(&self) -> Result<(), OpsError> {
        let _turn = self.alert_turn.lock().await;
        let mut conn = self.pool.acquire().await?;
        let copies = storage::alerts::resolved_open_copies(&mut conn, 50).await?;
        let mut current: Option<(i64, Screen)> = None;
        let names: std::collections::HashMap<i64, String> = storage::admin::userbots(&mut conn)
            .await?
            .into_iter()
            .map(|b| (i64::from(b.id), b.label))
            .collect();
        for (alert_id, chat, message_id) in copies {
            if current.as_ref().is_none_or(|(id, _)| *id != alert_id) {
                let Some(a) = storage::alerts::get(&mut conn, alert_id).await? else {
                    continue;
                };
                let t = &self.texts;
                let line = match self.resolution(&mut conn, &a).await? {
                    Some(r) => {
                        let mut journal = crate::journal::Names {
                            userbots: names.clone(),
                            ..Default::default()
                        };
                        if let Some(public) = a.payload["public"].as_str()
                            && let Some(id) = a.payload["id"].as_i64()
                        {
                            journal.orders.insert(id, public.to_owned());
                        }
                        t.f(
                            "alert.resolved_by",
                            &[
                                (
                                    "who",
                                    &esc(r
                                        .staff_name
                                        .as_deref()
                                        .unwrap_or(&r.staff_id.to_string())),
                                ),
                                (
                                    "what",
                                    &crate::journal::what(t, &self.links(), &journal, &r),
                                ),
                                ("at", &crate::dash::hhmm(r.at)),
                            ],
                        )
                    }
                    None => t.f(
                        "alert.resolved_at",
                        &[("at", &crate::dash::hhmm(chrono::Utc::now()))],
                    ),
                };
                let screen = Screen::new(t.f(
                    "alert.resolved",
                    &[("head", &self.alert_head(&a)), ("line", &line)],
                ));
                current = Some((alert_id, self.look().screen(&screen)));
            }
            let Some((_, screen)) = &current else {
                continue;
            };
            let mut request = EditMessageText::html(chat, message_id, screen.text.clone());
            request.reply_markup = Some(botapi::InlineKeyboardMarkup::new(Vec::new()));
            match self.api.edit_message_text(&request).await {
                Ok(_) => {}
                Err(e) if e.is_not_modified() || e.is_message_gone() => {}
                // П-19.3: Telegram отказал насовсем (сотрудник заблокировал бота — 403, чата нет) —
                // повтор не поможет, а копия держала бы очередь «Решено» для всех.
                Err(e) if e.is_rejection() => {
                    tracing::info!(chat, error = %e, "resolved alert copy cannot be updated, closed");
                }
                Err(e) => {
                    tracing::info!(chat, error = %e, "resolved alert copy not updated");
                    continue;
                }
            }
            storage::alerts::close_copy(&mut conn, alert_id, chat).await?;
        }
        Ok(())
    }

    /// «Принял»: событие закрыто, у всех копий — отметка и без кнопки «Принял».
    pub(crate) async fn ack(&self, staff: &StaffMember, id: i64) -> Result<(), OpsError> {
        // «Принял» на «Нужен разбор» от того, кто решить не может, — «Позвать админа» (П-3):
        // повторы стихают у него, а не у всех.
        let peek = {
            let mut conn = self.pool.acquire().await?;
            storage::alerts::get(&mut conn, id).await?
        };
        if let Some(a) = peek
            .as_ref()
            .filter(|a| a.title == "manual_review" && staff.role < Role::Admin)
            && let Some(order) = a.payload["id"].as_i64()
        {
            self.call_admin(staff, order).await?;
            return Ok(());
        }
        // Б-19.3: «Принял» — только тем, кому тревога приходит. Кнопки у других нет; данные кнопки
        // можно подделать — тогда ничего не меняется.
        if let Some(a) = &peek
            && audience(&a.title, a.level, staff.role).is_none()
        {
            tracing::warn!(
                staff = staff.tg_user_id,
                alert = id,
                title = %a.title,
                "ack of an alert this role does not receive, ignored"
            );
            return Ok(());
        }
        let _turn = self.alert_turn.lock().await;
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
        // «Разбор · K7M2Q9XR4T» и одна «Открыть» (макет): взять заявку — из её карточки.
        let review_head = (alert.title == "manual_review").then(|| {
            self.texts.f(
                "alert.review_head",
                &[(
                    "public",
                    &esc(alert.payload["public"].as_str().unwrap_or_default()),
                )],
            )
        });
        let head = match &review_head {
            Some(h) if alert.payload["public"].is_string() => h.as_str(),
            _ => head,
        };
        let mut screen = full.clone();
        if alert.title == "manual_review"
            && let Some(order) = alert.payload["id"].as_i64()
        {
            screen.keyboard = vec![vec![btn(self.texts.t("alert.b_open"), &Cb::Order(order))]];
        }
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
        for c in copies
            .iter()
            .filter(|c| c.message_id != 0 && c.muted_at.is_none())
        {
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

    /// «Завершить сессию» из тревоги «Новый вход» (веха 10): выкинуть чужое устройство и
    /// закрыть тревогу. Админ и выше.
    pub(crate) async fn end_session(
        &self,
        staff: &StaffMember,
        id: i64,
    ) -> Result<String, OpsError> {
        let t = &self.texts;
        if staff.role < Role::Admin {
            return Ok(t.f(
                "common.not_allowed",
                &[("role", &self.role_name(Role::Admin))],
            ));
        }
        let alert = {
            let mut conn = self.pool.acquire().await?;
            storage::alerts::get(&mut conn, id).await?
        };
        let Some(a) = alert.filter(|a| a.title == "new_login") else {
            return Ok(t.t("alert.session_gone"));
        };
        let account = s(&a.payload, "account");
        let Ok(hash) = s(&a.payload, "hash").parse::<i64>() else {
            return Ok(t.t("alert.session_gone"));
        };
        if let Err(e) = self.engine.end_session(&account, hash).await {
            tracing::warn!(error = %e, "foreign session not ended");
            return Ok(t.f(
                "alert.session_not_ended",
                &[("error", &esc(&e.to_string()))],
            ));
        }
        {
            let mut conn = self.pool.acquire().await?;
            let target = account.clone();
            storage::audit::record(
                &mut conn,
                &storage::audit::NewAudit {
                    staff_id: staff.tg_user_id,
                    action: "userbot.end_session",
                    target_type: Some("userbot"),
                    target_id: Some(&target),
                    before: Some(a.payload.clone()),
                    after: None,
                    reason: None,
                    double_confirmed: false,
                },
            )
            .await?;
        }
        self.ack(staff, id).await?;
        Ok(t.f("alert.session_ended", &[("account", &esc(&account))]))
    }

    /// «Позвать админа» (П-3, аудит 16.11): сотрудник без права решить заявку на разборе. Его
    /// копия тревоги затихает, админам и владельцу — сразу сообщение со звуком; сама тревога
    /// остаётся открытой и повторяется у них, пока её не примет тот, кто решает. Возвращает имена
    /// тех, кого позвали.
    pub(crate) async fn call_admin(
        &self,
        staff: &StaffMember,
        order: i64,
    ) -> Result<Vec<String>, OpsError> {
        let _turn = self.alert_turn.lock().await;
        let (alert, deciders, card) = {
            let mut conn = self.pool.acquire().await?;
            let alert =
                storage::alerts::open_by_key(&mut conn, &format!("manual_review:{order}")).await?;
            let deciders: Vec<StaffMember> = storage::staff::list(&mut conn)
                .await?
                .into_iter()
                .filter(|m| m.active() && m.role >= Role::Admin)
                .collect();
            let card = storage::admin::order_card(&mut conn, order).await?;
            let target = order.to_string();
            storage::audit::record(
                &mut conn,
                &storage::audit::NewAudit {
                    staff_id: staff.tg_user_id,
                    action: "order.call_admin",
                    target_type: Some("order"),
                    target_id: Some(&target),
                    before: None,
                    after: None,
                    reason: None,
                    double_confirmed: false,
                },
            )
            .await?;
            if let Some(a) = &alert {
                storage::alerts::mute_copy(&mut conn, a.id, staff.tg_user_id).await?;
            }
            (alert, deciders, card)
        };
        let t = &self.texts;
        // Его копия тревоги: «Позвал(а) админа · время — повторы для вас остановлены».
        if let Some(a) = &alert {
            let copies = {
                let mut conn = self.pool.acquire().await?;
                storage::alerts::copies(&mut conn, a.id).await?
            };
            if let Some(c) = copies
                .iter()
                .find(|c| c.chat_id == staff.tg_user_id && c.message_id != 0)
            {
                let full = self.render_alert(a);
                let head = full.text.lines().next().unwrap_or_default();
                let head = head
                    .strip_prefix("{e:")
                    .and_then(|r| r.split_once("} "))
                    .map_or(head, |(_, rest)| rest);
                let mut screen = Screen::new(t.f(
                    "alert.called",
                    &[
                        ("head", &head),
                        ("name", &esc(&staff.display_name)),
                        ("at", &crate::dash::hhmm(chrono::Utc::now())),
                    ],
                ));
                screen = screen.row(vec![btn(t.t("alert.b_open"), &Cb::Order(order))]);
                let screen = self.look().screen(&screen);
                let mut request =
                    EditMessageText::html(c.chat_id, c.message_id, screen.text.clone());
                request.reply_markup = screen.markup();
                if let Err(e) = self.api.edit_message_text(&request).await
                    && !e.is_not_modified()
                {
                    tracing::debug!(error = %e, "call-admin edit failed");
                }
            }
        }
        let reason = card
            .manual_reason
            .as_deref()
            .map_or_else(|| t.t("common.none"), esc);
        let text = t.f(
            "order.call_admin_notice",
            &[
                ("name", &esc(&staff.display_name)),
                ("public", &esc(&card.public_id)),
                ("reason", &reason),
            ],
        );
        let screen = self.look().screen(&Screen::new(text).row(vec![
            btn(t.t("alert.b_open"), &Cb::Order(order)).style(botapi::ButtonStyle::Primary),
        ]));
        let mut names = Vec::new();
        for m in &deciders {
            let mut request = SendMessage::html(m.tg_user_id, screen.text.clone());
            request.reply_markup = screen.markup().map(ReplyMarkup::Inline);
            match self.api.send_message(&request).await {
                Ok(_) => names.push(m.display_name.clone()),
                Err(e) => {
                    tracing::info!(chat = m.tg_user_id, error = %e, "call-admin not delivered")
                }
            }
        }
        tracing::info!(
            order,
            staff = staff.tg_user_id,
            called = names.len(),
            "staff called an admin"
        );
        Ok(names)
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
        // Макет «Новая авторизация»: «Это я» закрывает тихо, «Завершить» выкидывает устройство.
        "new_login" => {
            let at = s(p, "at")
                .parse::<chrono::DateTime<chrono::Utc>>()
                .map(crate::dash::hhmm)
                .unwrap_or_default();
            let place: Vec<String> = ["device", "app", "country", "ip"]
                .iter()
                .map(|k| s(p, k))
                .filter(|v| !v.is_empty())
                .map(|v| esc(&v))
                .chain(std::iter::once(at))
                .collect();
            Screen::new(t.f(
                "alert.new_login",
                &[
                    ("account", &esc(&s(p, "account"))),
                    ("where", &place.join(" · ")),
                ],
            ))
            .row(vec![
                btn(t.t("alert.b_end_session"), &Cb::EndSession(id))
                    .style(botapi::ButtonStyle::Danger),
            ])
            .row(vec![btn(t.t("alert.b_its_me"), &Cb::Ack(id))])
        }
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
                // Снимает только паузу «незнакомый ответ» — недостача или другая причина у того же
                // направления остаётся (Д-19.9).
                second.push(btn(t.t("alert.b_unpause"), &Cb::DirClearUnknown(d)));
            }
            if let Some(order) = p["order"].as_i64() {
                second.push(btn(t.t("alert.b_order"), &Cb::Order(order)));
            }
            if !second.is_empty() {
                sc = sc.row(second);
            }
            sc.row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
        }
        // П-19.2 (картинка link-missing.png принята 20.11): «Нет ссылки на чек» — устройство
        // сообщения свёрнутой цитатой, где деньги — строкой ниже.
        "check_link_missing" => {
            let platform = s(p, "platform");
            let name = platform_of(&platform).map_or_else(
                || esc(&platform),
                |x| t.t(&format!("names.{}", x.db_name())),
            );
            let shape = &p["shape"];
            let mut parts = Vec::new();
            if let Some(bot) = shape["via_bot"].as_str() {
                parts.push(esc(&format!("@{bot}")));
            }
            parts.push(t.t(if shape["edited"].as_bool() == Some(true) {
                "alert.shape_edited"
            } else {
                "alert.shape_not_edited"
            }));
            let len = shape["text_len"].as_i64().unwrap_or(0);
            parts.push(t.f(
                "alert.shape_text",
                &[
                    ("n", &len),
                    ("chars", &ui::fmt::plural(len, ["знак", "знака", "знаков"])),
                ],
            ));
            let buttons: Vec<String> = shape["buttons"]
                .as_array()
                .map(|b| b.iter().filter_map(|x| x.as_str()).map(esc).collect())
                .unwrap_or_default();
            parts.push(if buttons.is_empty() {
                t.t("alert.shape_no_buttons")
            } else {
                t.f("alert.shape_buttons", &[("list", &buttons.join(", "))])
            });
            let user = p["user_id"].as_i64().unwrap_or(0);
            let mut sc = Screen::new(t.f(
                "alert.link_missing",
                &[
                    ("platform", &name),
                    ("at", &crate::dash::hhmm(first_at)),
                    ("shape", &parts.join(" · ")),
                    ("user", &user),
                ],
            ))
            .row(vec![btn(t.t("alert.b_fixture"), &Cb::Fixture(id))]);
            if user != 0 {
                sc = sc.row(vec![btn(t.t("alert.b_client"), &Cb::User(user))]);
            }
            sc
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
            .row(vec![btn(t.t("alert.b_maint"), &Cb::MaintFor(30))])
            .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
        }
        _ => return None,
    };
    Some(screen)
}

/// Макет «Алерт и „Принял“» (П-6, П-7): «Нужен разбор · K7M2Q9XR4T / CB → xR · 100.00 → 97.50 ·
/// клиент 51234567 / причина / ответ кошелька цитатой», [Открыть] [Взять] / [Принял]. Старые
/// тревоги без новых полей — с тем, что есть.
pub fn review_alert_screen(t: &crate::Texts, p: &Value, id: i64) -> Screen {
    let order = p["id"].as_i64().unwrap_or_default();
    let direction = s(p, "direction").parse::<Direction>().ok();
    let route = direction.map_or_else(
        || esc(&s(p, "direction")),
        |d| t.t(&format!("dash.short_{}", d.code())),
    );
    let money = |k: &str| {
        p[k].as_str()
            .and_then(|v| v.parse::<domain::Decimal>().ok())
    };
    let amounts = match (money("amount_in").or(money("amount")), money("amount_out")) {
        (Some(a), Some(b)) => format!("{} → {}", crate::dash::amount(a), crate::dash::amount(b)),
        (Some(a), None) | (None, Some(a)) => crate::dash::amount(a),
        (None, None) => t.t("common.none"),
    };
    let public = p["public"]
        .as_str()
        .map_or_else(|| order.to_string(), str::to_owned);
    let reason = s(p, "reason");
    // Застрявшая переплата (Д-19.1, макет «Нужен разбор · переплата»): «Переплата 8.75: …».
    let reason = match money("excess") {
        Some(excess) => t.f(
            "alert.review_excess",
            &[
                ("amount", &crate::dash::amount(excess)),
                ("reason", &esc(&reason)),
            ],
        ),
        None if reason.is_empty() => String::new(),
        None => t.f("alert.review_reason", &[("reason", &esc(&reason))]),
    };
    // Чей ответ ждём: выплата и счёт — кошелёк выплаты, чек на переплату — кошелёк входа.
    let wallet = direction.map(|d| {
        crate::review::wallet(if money("excess").is_some() {
            d.source()
        } else {
            d.target()
        })
    });
    let reply = match &p["reply"] {
        Value::Object(m) => {
            let at = m
                .get("at")
                .and_then(Value::as_str)
                .and_then(|a| a.parse::<chrono::DateTime<chrono::Utc>>().ok())
                .map(crate::review::hms)
                .unwrap_or_default();
            let who = m
                .get("who")
                .and_then(Value::as_str)
                .or(wallet)
                .unwrap_or_default();
            let text = m.get("text").and_then(Value::as_str).unwrap_or_default();
            let text = crate::views::mask_links(&ui::html::clip(text, 300));
            t.f(
                "alert.review_reply",
                &[("who", &esc(who)), ("at", &at), ("text", &esc(&text))],
            )
        }
        _ => match (
            wallet,
            s(p, "since").parse::<chrono::DateTime<chrono::Utc>>(),
        ) {
            (Some(w), Ok(since)) => t.f(
                "alert.review_reply",
                &[
                    ("who", &w),
                    ("at", &crate::review::hms(since)),
                    ("text", &t.t("alert.review_no_reply")),
                ],
            ),
            _ => String::new(),
        },
    };
    Screen::new(
        t.f(
            "alert.manual_review",
            &[
                ("public", &esc(&public)),
                ("route", &route),
                ("amounts", &amounts),
                (
                    "user",
                    &p["user"]
                        .as_i64()
                        .map_or_else(String::new, |u| u.to_string()),
                ),
                ("reason", &reason),
                ("reply", &reply),
            ],
        ),
    )
    .row(vec![
        btn(t.t("alert.b_open"), &Cb::Order(order)),
        btn(t.t("alert.b_take"), &Cb::Take(order)).style(botapi::ButtonStyle::Primary),
    ])
    .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
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
    let screen = Screen::new(text)
        .row(vec![btn(t.t("alert.b_yes"), &Cb::Capital(id))])
        .row(vec![if short {
            no.style(botapi::ButtonStyle::Danger)
        } else {
            no
        }]);
    // Недостачу, которую не объяснить, владелец признаёт потерей (решение 17.11).
    if short {
        screen.row(vec![btn(t.t("alert.b_write_off"), &Cb::WriteOff(id))])
    } else {
        screen
    }
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

#[cfg(test)]
mod audience_tests {
    use super::*;

    /// Таблица «звук по роли» с картинки П-10 (принята 18.11).
    #[test]
    fn sound_by_role_table() {
        let all = [Role::Operator, Role::Admin, Role::Owner];
        for title in [
            "manual_review",
            "unknown_reply",
            "engine_down",
            "kill_switch",
            "second_instance",
        ] {
            for r in all {
                assert_eq!(audience(title, Level::Critical, r), Some(true), "{title}");
            }
        }
        for title in [
            "balance_mismatch",
            "reconcile_stalled",
            "login_lost",
            "new_login",
            "invest_withdrawal",
            "invest_manual",
            "backup_failed",
        ] {
            assert_eq!(
                audience(title, Level::Critical, Role::Operator),
                None,
                "{title}"
            );
            assert_eq!(audience(title, Level::Critical, Role::Admin), Some(true));
            assert_eq!(audience(title, Level::Critical, Role::Owner), Some(true));
        }
        for title in ["promo_hint", "promo_started", "promo_stopped"] {
            assert_eq!(audience(title, Level::Warn, Role::Operator), None);
            assert_eq!(audience(title, Level::Warn, Role::Owner), Some(false));
        }
        // Остальное — как раньше: звук от Warn.
        assert_eq!(
            audience("reserve_low", Level::Notice, Role::Operator),
            Some(false)
        );
        assert_eq!(
            audience("reserve_low", Level::Warn, Role::Operator),
            Some(true)
        );
        // П-19.2 (картинка link-missing.png принята 20.11): «Нет ссылки на чек» — подсказка
        // разработчику, решать нечего: только владельцу и без звука.
        for role in [Role::Operator, Role::Admin] {
            assert_eq!(audience("check_link_missing", Level::Warn, role), None);
        }
        assert_eq!(
            audience("check_link_missing", Level::Warn, Role::Owner),
            Some(false)
        );
    }
}
