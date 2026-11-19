//! «Обращения» и «Система» пульта.
//!
//! Обращение клиента (макет «Обращение клиента») хранится событием `support:<клиент>` в
//! `alerts`: так его видит счётчик «Обращения · N» в меню, переживает перезапуск, а повторный
//! вопрос того же клиента не плодит строк. Ответ или «Закрыть» закрывает его.

use chrono::Utc;
use serde_json::json;
use sqlx::PgConnection;
use storage::alerts::{AlertRow, Level, NewAlert, Topic};
use storage::staff::StaffMember;

use crate::nav::{self, List};
use crate::texts::esc;
use crate::{Cb, Inner, OpsError, Screen, btn};

const TITLE: &str = "support_question";

fn key(client: i64) -> String {
    format!("support:{client}")
}

/// Открытые обращения, новые сверху.
pub(crate) async fn open_questions(conn: &mut PgConnection) -> Result<Vec<AlertRow>, OpsError> {
    let mut list: Vec<AlertRow> = storage::alerts::open(conn, 500)
        .await?
        .into_iter()
        .filter(|a| a.title == TITLE)
        .collect();
    list.sort_by_key(|a| std::cmp::Reverse(a.last_at));
    Ok(list)
}

/// «12 мин», «3 ч», «2 дн».
pub(crate) fn age(minutes: i64, t: &crate::Texts) -> String {
    if minutes >= 48 * 60 {
        t.f("system.up_days", &[("n", &(minutes / (24 * 60)))])
    } else if minutes >= 120 {
        t.f("system.up_hours", &[("n", &(minutes / 60))])
    } else {
        t.f("system.up_min", &[("n", &minutes.max(0))])
    }
}

impl Inner {
    /// Записать обращение (одно на клиента; повтор — новый текст и счётчик). Сразу помечено
    /// доставленным: персоналу его шлёт [`Inner::support_question`], не рассылка событий.
    pub(crate) async fn store_question(
        &self,
        from: &botapi::User,
        text: &str,
        order: Option<&str>,
    ) -> Result<i64, OpsError> {
        let mut tx = self.pool.begin().await?;
        let key = key(from.id);
        let id = storage::alerts::raise(
            &mut tx,
            &NewAlert {
                level: Level::Notice,
                topic: Topic::Admin,
                dedup_key: &key,
                title: TITLE,
                payload: json!({
                    "client": from.id,
                    "username": from.username,
                    "text": text,
                    "order": order,
                }),
            },
        )
        .await?;
        storage::alerts::mark_delivered(&mut tx, id, None).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Закрыть обращение клиента (ответили или закрыли без ответа).
    /// Заявка открытого вопроса клиента (публичный номер), если вопрос был по заявке.
    pub(crate) async fn question_order(&self, client: i64) -> Result<Option<String>, OpsError> {
        let mut conn = self.pool.acquire().await?;
        Ok(storage::alerts::open_by_key(&mut conn, &key(client))
            .await?
            .and_then(|a| a.payload["order"].as_str().map(str::to_owned)))
    }

    /// П-8: кто закрыл и что ответил — в данных обращения; копии у всех сотрудников правятся:
    /// «Ответил(а) Анна · 14:09» с ответом, без «Ответить».
    pub(crate) async fn close_question(
        &self,
        client: i64,
        staff: &StaffMember,
        answer: Option<&str>,
    ) -> Result<(), OpsError> {
        let closed = {
            let mut conn = self.pool.acquire().await?;
            match storage::alerts::open_by_key(&mut conn, &key(client)).await? {
                Some(a) => {
                    let by = json!({
                        "by": staff.display_name,
                        "at": Utc::now().to_rfc3339(),
                        "text": answer,
                    });
                    let field = if answer.is_some() { "answer" } else { "closed" };
                    storage::alerts::merge_payload(&mut conn, a.id, &json!({ field: by })).await?;
                    storage::alerts::ack(&mut conn, a.id, staff.tg_user_id).await?;
                    storage::alerts::get(&mut conn, a.id).await?
                }
                None => None,
            }
        };
        let Some(a) = closed else {
            return Ok(());
        };
        let screen = self.look().screen(&self.question_done(&a).await?);
        let copies = {
            let mut conn = self.pool.acquire().await?;
            storage::alerts::copies(&mut conn, a.id).await?
        };
        for c in copies.iter().filter(|c| c.message_id != 0) {
            let mut request =
                botapi::EditMessageText::html(c.chat_id, c.message_id, screen.text.clone());
            request.reply_markup = screen.markup();
            if let Err(e) = self.api.edit_message_text(&request).await
                && !e.is_not_modified()
            {
                tracing::debug!(error = %e, "support copy edit failed");
            }
        }
        Ok(())
    }

    /// Закрытое обращение (П-8, картинка принята 18.11): вопрос, заявка, кто ответил и что.
    pub(crate) async fn question_done(&self, a: &AlertRow) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let p = &a.payload;
        let client = p["client"].as_i64().unwrap_or_default();
        let who = match p["username"].as_str() {
            Some(u) => format!("{client} · @{}", esc(u)),
            None => client.to_string(),
        };
        let question: String = p["text"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(1500)
            .collect();
        let mut body = t.f(
            "support.question_done",
            &[("who", &who), ("text", &esc(&question))],
        );
        let mut order_id = None;
        if let Some(public) = p["order"].as_str() {
            body.push('\n');
            body.push_str(&t.f("support.order", &[("order", &esc(public))]));
            let mut conn = self.pool.acquire().await?;
            order_id = storage::admin::find_order(&mut conn, public).await?;
        }
        body.push_str(&self.closed_line(p));
        let mut screen = Screen::new(body);
        if let Some(id) = order_id {
            screen = screen.row(vec![btn(t.t("support.b_order"), &Cb::Order(id))]);
        }
        Ok(screen)
    }

    /// «Ответил(а) Анна · 14:09 + ответ» или «Закрыл(а) без ответа Анна · 14:09».
    fn closed_line(&self, p: &serde_json::Value) -> String {
        let t = &self.texts;
        let at = |v: &serde_json::Value| {
            v["at"]
                .as_str()
                .and_then(|a| a.parse::<chrono::DateTime<Utc>>().ok())
                .map(crate::dash::hhmm)
                .unwrap_or_default()
        };
        if p["answer"].is_object() {
            let a = &p["answer"];
            t.f(
                "support.answered",
                &[
                    ("name", &esc(a["by"].as_str().unwrap_or_default())),
                    ("at", &at(a)),
                    ("answer", &esc(a["text"].as_str().unwrap_or_default())),
                ],
            )
        } else if p["closed"].is_object() {
            let c = &p["closed"];
            t.f(
                "support.closed_by",
                &[
                    ("name", &esc(c["by"].as_str().unwrap_or_default())),
                    ("at", &at(c)),
                ],
            )
        } else {
            String::new()
        }
    }

    /// «Ответить» (П-8): обращение открыто — вопрос; уже ответили или закрыли — показать, кто и
    /// что, и спросить «Ответить ещё раз?». Клиенту ничего не уходит, пока сотрудник не решит.
    pub(crate) async fn support_reply_entry(
        &self,
        staff: &StaffMember,
        client: i64,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let last = {
            let mut conn = self.pool.acquire().await?;
            storage::alerts::latest_by_key(&mut conn, &key(client)).await?
        };
        if let Some(a) = last.filter(|a| a.acked_at.is_some()) {
            let p = &a.payload;
            let at = |v: &serde_json::Value| {
                v["at"]
                    .as_str()
                    .and_then(|a| a.parse::<chrono::DateTime<Utc>>().ok())
                    .map(crate::dash::hhmm)
                    .unwrap_or_default()
            };
            let text = if p["answer"].is_object() {
                let a = &p["answer"];
                Some(t.f(
                    "support.already",
                    &[
                        ("client", &client),
                        ("name", &esc(a["by"].as_str().unwrap_or_default())),
                        ("at", &at(a)),
                        ("answer", &esc(a["text"].as_str().unwrap_or_default())),
                    ],
                ))
            } else if p["closed"].is_object() {
                let c = &p["closed"];
                Some(t.f(
                    "support.closed_before",
                    &[
                        ("client", &client),
                        ("name", &esc(c["by"].as_str().unwrap_or_default())),
                        ("at", &at(c)),
                    ],
                ))
            } else {
                None
            };
            if let Some(text) = text {
                return Ok(Screen::new(text).row(vec![
                    btn(t.t("support.b_reply_again"), &Cb::SupportReplyAgain(client))
                        .style(botapi::ButtonStyle::Primary),
                    btn(t.t("common.cancel"), &Cb::Cancel),
                ]));
            }
        }
        Ok(self.support_reply_prompt(staff, client))
    }

    /// Вопрос «Ответ клиенту … — напишите одним сообщением».
    pub(crate) fn support_reply_prompt(&self, staff: &StaffMember, client: i64) -> Screen {
        self.prompt_with(
            staff,
            crate::actions::Input::SupportReply(client),
            self.texts
                .f("support.reply_prompt", &[("client", &client.to_string())]),
        )
    }

    /// Карточка обращения — макет: кто, вопрос, заявка рядом; «Ответить», «Заявка», «Закрыть».
    pub(crate) async fn question_card(
        &self,
        client: i64,
        username: Option<&str>,
        text: &str,
        order: Option<&str>,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let who = match username {
            Some(u) => format!("{client} · @{}", esc(u)),
            None => client.to_string(),
        };
        let question: String = text.chars().take(1500).collect();
        let mut body = t.f(
            "support.question",
            &[("who", &who), ("text", &esc(&question))],
        );
        let mut order_id = None;
        let mut foreign = false;
        if let Some(public) = order {
            let mut conn = self.pool.acquire().await?;
            order_id = storage::admin::find_order(&mut conn, public).await?;
            // Б-1 (аудит 11.11): номер приходит из кнопки клиента — прикладываем только его
            // собственную заявку. Чужой номер (увиденный на скриншоте) — вопрос без заявки.
            if let Some(id) = order_id {
                let owner: i64 = sqlx::query_scalar("SELECT user_id FROM orders WHERE id = $1")
                    .bind(id)
                    .fetch_one(&mut *conn)
                    .await?;
                if owner != client {
                    tracing::warn!(
                        client,
                        "support question names someone else's order: not attached"
                    );
                    order_id = None;
                    foreign = true;
                }
            }
        }
        if let Some(public) = order.filter(|_| !foreign) {
            let mut conn = self.pool.acquire().await?;
            let line = match order_id {
                Some(id) => {
                    let (state, updated): (String, chrono::DateTime<Utc>) =
                        sqlx::query_as("SELECT state, updated_at FROM orders WHERE id = $1")
                            .bind(id)
                            .fetch_one(&mut *conn)
                            .await?;
                    let minutes = (Utc::now() - updated).num_minutes();
                    t.f(
                        "support.order_state",
                        &[
                            ("order", &esc(public)),
                            ("state", &self.state_name(&state)),
                            ("age", &age(minutes, t)),
                        ],
                    )
                }
                None => t.f("support.order", &[("order", &esc(public))]),
            };
            body.push_str(&line);
        }
        let mut first = vec![
            btn(t.t("support.b_reply"), &Cb::SupportReply(client))
                .style(botapi::ButtonStyle::Primary),
        ];
        if let Some(id) = order_id {
            first.push(btn(t.t("support.b_order"), &Cb::Order(id)));
        }
        Ok(Screen::new(body)
            .row(first)
            .row(vec![btn(t.t("support.b_close"), &Cb::SupportClose(client))]))
    }

    /// «Обращения»: кто ждёт ответа, что спросил, сколько ждёт; клиент — ссылка на карточку
    /// обращения («Ответить», «Заявка», «Закрыть»). По [`nav::PAGE`] на странице.
    pub(crate) async fn inbox(&self, page: u32) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let list = {
            let mut conn = self.pool.acquire().await?;
            open_questions(&mut conn).await?
        };
        if list.is_empty() {
            let rich = format!(
                "<h3>{}</h3><p>{}</p>",
                t.t("support.inbox_empty_title"),
                t.t("support.inbox_empty")
            );
            let plain = format!(
                "<b>{}</b>\n{}",
                t.t("support.inbox_empty_title"),
                t.t("support.inbox_empty")
            );
            return Ok(Screen::card(rich, plain)
                .row(vec![btn(t.t("nav.refresh"), &Cb::Inbox)])
                .row(nav::back(t, &Cb::Menu)));
        }
        let (page, range) = nav::window(list.len(), page);
        let title = t.f("support.inbox_title", &[("n", &list.len())]);
        let mut rich = format!(
            "<h3>{title}</h3><table striped compact><tr><th>{}</th><th>{}</th><th align=\"right\">{}</th></tr>",
            t.t("support.c_client"),
            t.t("support.c_text"),
            t.t("support.c_wait"),
        );
        let mut lines = Vec::new();
        for a in &list[range] {
            let client = a.payload["client"].as_i64().unwrap_or_default();
            let text: String = a.payload["text"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(40)
                .collect();
            let wait = age((Utc::now() - a.first_at).num_minutes(), t);
            let who = self.link(&client.to_string(), &Cb::Question(client));
            rich.push_str(&format!(
                "<tr><td>{who}</td><td>{}</td><td align=\"right\">{wait}</td></tr>",
                esc(&text)
            ));
            lines.push(t.f(
                "support.inbox_plain",
                &[("client", &who), ("text", &esc(&text)), ("wait", &wait)],
            ));
        }
        rich.push_str(&format!(
            "</table><footer>{}</footer>",
            t.t("support.inbox_hint")
        ));
        let plain = format!(
            "<b>{title}</b>\n<blockquote>{}</blockquote>\n<i>{}</i>",
            lines.join("\n"),
            t.t("support.inbox_hint")
        );
        Ok(Screen::card(rich, plain)
            .row(nav::pager(t, List::Inbox, page, list.len()))
            .row(vec![btn(t.t("nav.refresh"), &Cb::Page(List::Inbox, page))])
            .row(nav::back(t, &Cb::Menu)))
    }

    /// Карточка обращения из хранилища (нажали строку в «Обращениях»).
    pub(crate) async fn question_from_store(&self, client: i64) -> Result<Screen, OpsError> {
        let a = {
            let mut conn = self.pool.acquire().await?;
            storage::alerts::open_by_key(&mut conn, &key(client)).await?
        };
        let Some(a) = a else {
            return Ok(self
                .inbox(0)
                .await?
                .notice(&self.texts.t("support.already_closed")));
        };
        let p = &a.payload;
        let card = self
            .question_card(
                client,
                p["username"].as_str(),
                p["text"].as_str().unwrap_or_default(),
                p["order"].as_str(),
            )
            .await?;
        Ok(card.row(nav::back(&self.texts, &Cb::Inbox)))
    }

    /// «Закрыть» обращение без ответа.
    pub(crate) async fn support_close(
        &self,
        staff: &StaffMember,
        client: i64,
    ) -> Result<Screen, OpsError> {
        self.close_question(client, staff, None).await?;
        Ok(self.inbox(0).await?.notice(
            &self
                .texts
                .f("support.closed", &[("name", &esc(&staff.display_name))]),
        ))
    }

    /// «Система» — макет: версия и время работы, база, бэкап, очередь операций, Bot API,
    /// аккаунты юзерботов.
    pub(crate) async fn system(&self) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let started = std::time::Instant::now();
        let mut conn = self.pool.acquire().await?;
        let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&mut *conn)
            .await
            .is_ok();
        let db_ms = started.elapsed().as_millis();
        let (active, unknown): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE status IN ('pending', 'dispatched')), count(*) FILTER (WHERE status = 'unknown') FROM operations",
        )
        .fetch_one(&mut *conn)
        .await?;
        let bots = storage::admin::userbots(&mut conn).await?;
        drop(conn);
        let api_started = std::time::Instant::now();
        let api_ok = self.api.get_me().await.is_ok();
        let api_ms = api_started.elapsed().as_millis();
        let uptime = age(
            self.started
                .elapsed()
                .as_secs()
                .div_ceil(60)
                .try_into()
                .unwrap_or(0),
            t,
        );

        let mut rows: Vec<(&str, String, String)> = vec![
            (
                "{e:dot_ok}",
                t.t("system.version"),
                t.f(
                    "system.v_version",
                    &[("version", &env!("CARGO_PKG_VERSION")), ("uptime", &uptime)],
                ),
            ),
            (
                if db_ok { "{e:dot_ok}" } else { "{e:dot_act}" },
                t.t("system.db"),
                if db_ok {
                    t.f("system.v_db", &[("ms", &db_ms)])
                } else {
                    t.t("system.v_down")
                },
            ),
            ("{e:dot_wait}", t.t("system.backup"), t.t("system.v_backup")),
            (
                if unknown > 0 {
                    "{e:dot_act}"
                } else if active > 0 {
                    "{e:dot_work}"
                } else {
                    "{e:dot_ok}"
                },
                t.t("system.queue"),
                if active + unknown == 0 {
                    t.t("system.v_queue_empty")
                } else {
                    t.f("system.v_queue", &[("n", &active), ("unknown", &unknown)])
                },
            ),
            (
                if api_ok { "{e:dot_ok}" } else { "{e:dot_act}" },
                t.t("system.botapi"),
                if api_ok {
                    t.f("system.v_botapi", &[("ms", &api_ms)])
                } else {
                    t.t("system.v_down")
                },
            ),
        ];
        for b in &bots {
            let flood = b.flood_wait_until.filter(|u| *u > Utc::now());
            let (dot, value) = match (b.status.as_str(), flood) {
                (_, Some(until)) => (
                    "{e:dot_wait}",
                    t.f("system.bot_flood", &[("until", &crate::dash::hhmm(until))]),
                ),
                ("active", None) => ("{e:dot_ok}", t.t("system.bot_ok")),
                (status, None) => ("{e:dot_act}", self.state_name_bot(status)),
            };
            rows.push((dot, esc(&b.label), value));
        }
        let title = t.t("system.title");
        let mut rich = format!("<h3>{title}</h3><table compact>");
        let mut plain = format!("<b>{title}</b>\n<blockquote>");
        let mut lines = Vec::new();
        for (dot, label, value) in &rows {
            rich.push_str(&format!(
                "<tr><td>{dot} {label}</td><td align=\"right\">{value}</td></tr>"
            ));
            lines.push(format!("{dot} {label} — {value}"));
        }
        let footer = t.f(
            "system.updated",
            &[("time", &crate::dash::hhmm(Utc::now()))],
        );
        rich.push_str(&format!("</table><footer>{footer}</footer>"));
        plain.push_str(&lines.join("\n"));
        plain.push_str(&format!("</blockquote>\n<i>{footer}</i>"));
        Ok(Screen::card(rich, plain)
            .row(vec![btn(t.t("system.b_selftest"), &Cb::Selftest)])
            .row(vec![btn(t.t("system.b_preview"), &Cb::Preview)])
            .row(vec![btn(t.t("nav.refresh"), &Cb::System)])
            .row(nav::back(t, &Cb::Menu)))
    }

    /// Состояние аккаунта юзербота словами.
    fn state_name_bot(&self, status: &str) -> String {
        let key = format!("bots.status_{status}");
        if self.texts.has(&key) {
            self.texts.t(&key)
        } else {
            esc(status)
        }
    }

    /// «Рассылка» — пока не сделана: говорим прямо.
    pub(crate) fn broadcast(&self) -> Screen {
        let t = &self.texts;
        let rich = format!(
            "<h3>{}</h3><p>{}</p>",
            t.t("broadcast.title"),
            t.t("broadcast.soon")
        );
        let plain = format!(
            "<b>{}</b>\n{}",
            t.t("broadcast.title"),
            t.t("broadcast.soon")
        );
        Screen::card(rich, plain).row(nav::back(t, &Cb::Menu))
    }
}
