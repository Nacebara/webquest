//! «Очередь разбора» и «Карточка заявки» — макет «Каждый день».
//!
//! Очередь: таблица по возрасту (старейшая — первой и синей кнопкой), «Взять» закрепляет
//! заявку — остальные видят, кто ей занят. Карточка (rich): состояние и возраст, клиент,
//! суммы, ход заявки по шагам, операции, ответ кошелька, проводки; кнопки — по правам
//! сотрудника.

use chrono::{DateTime, Duration, TimeZone as _, Utc};
use domain::fsm::{Flow, OrderState};
use domain::{Asset, Decimal, Direction, Platform};
use storage::admin::OrderCard;
use storage::staff::{Role, StaffMember};

use crate::cb::DecisionKind;
use crate::dash::amount;
use crate::nav::{self, Links, List};
use crate::reserves::short;
use crate::texts::{Texts, code, esc};
use crate::{Cb, Inner, OpsError, Screen, btn};

/// Строка очереди.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueRow {
    pub id: i64,
    pub public_id: String,
    /// Что случилось — коротко.
    pub what: String,
    pub minutes: i64,
    /// Кто взял (имя сотрудника) и взял ли сам смотрящий.
    pub taken_by: Option<String>,
    pub mine: bool,
}

/// Очередь разбора.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueueData {
    pub items: Vec<QueueRow>,
    /// Счета, которые нужно оплатить вручную (появятся с ручной оплатой счёта).
    pub invoices: Vec<QueueRow>,
}

fn age(t: &Texts, minutes: i64) -> String {
    if minutes >= 120 {
        t.f("queue.age_hours", &[("n", &(minutes / 60))])
    } else {
        t.f("queue.age_min", &[("n", &minutes.max(0))])
    }
}

/// Причина коротко: до 32 символов.
fn brief(reason: &str) -> String {
    let chars: Vec<char> = reason.chars().collect();
    if chars.len() <= 32 {
        reason.to_owned()
    } else {
        let head: String = chars[..31].iter().collect();
        format!("{}…", head.trim_end())
    }
}

impl QueueData {
    /// Очередь, страница `page`: номер заявки — ссылка на карточку (решение владельца
    /// 8 ноября: нажимать по номеру в таблице, а не кнопками; «Взять» — в карточке).
    pub fn screen(&self, t: &Texts, links: &Links, page: u32) -> Screen {
        let n = self.items.len() + self.invoices.len();
        if n == 0 {
            let title = t.t("queue.title_empty");
            return Screen::card(
                format!("<h3>{title}</h3><p>{}</p>", t.t("queue.empty")),
                format!("<b>{title}</b>\n{}", t.t("queue.empty")),
            )
            .row(vec![btn(t.t("queue.b_refresh"), &Cb::Queue)])
            .row(nav::back(t, &Cb::Menu));
        }
        let (page, range) = nav::window(self.items.len(), page);
        let title = t.f("queue.title", &[("n", &self.items.len())]);
        let table = |rows: &[QueueRow], head: bool| {
            let mut html = String::from("<table striped compact>");
            if head {
                html.push_str(&format!(
                    "<tr><th>{}</th><th>{}</th><th align=\"right\">{}</th></tr>",
                    t.t("queue.c_order"),
                    t.t("queue.c_what"),
                    t.t("queue.c_wait")
                ));
            }
            let mut plain = Vec::new();
            for r in rows {
                let what = match &r.taken_by {
                    Some(name) => format!("{} · {{m:take}} {}", esc(&r.what), esc(name)),
                    None => esc(&r.what),
                };
                let num = links.link(&esc(&r.public_id), &Cb::Order(r.id));
                html.push_str(&format!(
                    "<tr><td>{num}</td><td>{what}</td><td align=\"right\">{}</td></tr>",
                    age(t, r.minutes)
                ));
                plain.push(format!("{num} — {what} · {}", age(t, r.minutes)));
            }
            html.push_str("</table>");
            (html, plain.join("\n"))
        };
        let (items_html, items_plain) = table(&self.items[range], true);
        let mut rich = format!("<h3>{title}</h3>{items_html}");
        let mut plain = format!("<b>{title}</b>\n<blockquote>{items_plain}</blockquote>");
        if !self.invoices.is_empty() {
            let sub = t.f("queue.invoices", &[("n", &self.invoices.len())]);
            let (html, text) = table(&self.invoices, false);
            rich.push_str(&format!("<h4>{sub}</h4>{html}"));
            plain.push_str(&format!("\n<b>{sub}</b>\n<blockquote>{text}</blockquote>"));
        }
        rich.push_str(&format!("<footer>{}</footer>", t.t("queue.hint")));
        plain.push_str(&format!("\n<i>{}</i>", t.t("queue.hint")));
        Screen::card(rich, plain)
            .row(nav::pager(t, List::Queue, page, self.items.len()))
            .row(vec![btn(
                t.t("queue.b_refresh"),
                &Cb::Page(List::Queue, page),
            )])
            .row(nav::back(t, &Cb::Menu))
    }
}

/// Что последняя сверка говорит о неясной операции заявки (шаг 1.5, утверждено 13.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileHint {
    pub at: DateTime<Utc>,
    /// Кошелёк операции.
    pub platform: Platform,
    /// Вид операции (`create_payout_check` …).
    pub kind: String,
    /// `Some(true)` — по балансу прошла, `Some(false)` — не прошла, `None` — не понять.
    pub done: Option<bool>,
}

impl ReconcileHint {
    fn line(&self, t: &Texts) -> String {
        let group = match self.kind.as_str() {
            "create_refund_check" | "create_excess_check" => "refund",
            "pay_invoice" => "invoice",
            "activate_check" => "intake",
            _ => "payout",
        };
        let key = match self.done {
            Some(true) => format!("order.hint_done_{group}"),
            Some(false) => format!("order.hint_not_{group}"),
            None => "order.hint_unclear".to_owned(),
        };
        t.f(
            &key,
            &[
                ("time", &crate::dash::hhmm(self.at)),
                ("wallet", &wallet(self.platform)),
            ],
        )
    }
}

/// Карточка заявки: данные из базы ([`Inner::order_view`]) или из макета ([`sample_order`]).
#[derive(Debug, Clone, PartialEq)]
pub struct OrderView {
    pub card: OrderCard,
    /// Обменов у клиента (завершённых).
    pub completed: i64,
    /// Аккаунт юзербота кошелька заявки.
    pub account: Option<String>,
    pub taken_by: Option<String>,
    /// Проводки: сумма и счёт леджера.
    pub entries: Vec<(Decimal, String)>,
    /// Удержано под заявку: сумма и кошелёк.
    pub held: Option<(Decimal, Platform)>,
    /// Подсказка сверки по неясной операции заявки.
    pub reconcile: Option<ReconcileHint>,
}

fn wallet(p: Platform) -> &'static str {
    match p {
        Platform::CryptoBot => "CryptoBot",
        Platform::XRocket => "xRocket",
    }
}

/// Время шага: `14:04:02` по Москве (технический ход заявки).
fn hms(at: DateTime<Utc>) -> String {
    (at + Duration::hours(3)).format("%H:%M:%S").to_string()
}

fn state_icon(s: OrderState) -> &'static str {
    match s {
        OrderState::ManualReview => "{e:manual}",
        OrderState::Completed | OrderState::ClosedManual => "{e:done}",
        OrderState::Refunded => "{e:refund}",
        OrderState::Rejected
        | OrderState::IntakeFailed
        | OrderState::Expired
        | OrderState::Cancelled => "{e:dot_closed}",
        _ => "{e:process}",
    }
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

impl OrderView {
    /// Карточка для роли `role`; `now` — для возраста.
    pub fn screen(&self, t: &Texts, role: Role, now: DateTime<Utc>, links: &Links) -> Screen {
        let c = &self.card;
        let state_name = |s: &str| t.t(&format!("states.{s}"));
        let since = c.transitions.last().map_or(c.created_at, |s| s.at);
        let mut head = vec![
            format!(
                "{} <b>{}</b>",
                state_icon(c.state),
                capitalized(&state_name(c.state.as_db_str()))
            ),
            age(t, (now - since).num_minutes()),
            short(c.direction).to_owned(),
        ];
        if let Some(a) = &self.account {
            head.push(esc(a));
        }
        let head = head.join(" · ");
        let title = t.f("order.card_title", &[("public", &code(&c.public_id))]);

        // Ключ — значение.
        let target = c.direction.target();
        let mut rows: Vec<(String, String)> = vec![(
            t.t("order.r_client"),
            format!(
                "{} · {} {}",
                links.link(&c.user_id.to_string(), &Cb::User(c.user_id)),
                self.completed,
                ui::fmt::plural(self.completed, ["обмен", "обмена", "обменов"])
            ),
        )];
        if let Some(a) = c.intake_amount {
            let mut v = format!("{} {}", amount(a), wallet(c.intake_platform));
            if !c.intake_posted() {
                v.push_str(&t.t("order.not_posted"));
            }
            rows.push((t.t("order.r_intake"), v));
        }
        match c.direction.flow() {
            Flow::CheckExchange => {
                if let Some(p) = c.payout_amount {
                    rows.push((
                        t.t("order.r_payout"),
                        format!("{} {}", amount(p), wallet(target)),
                    ));
                }
            }
            Flow::InvoicePayment => {
                if let Some(a) = c.invoice_amount {
                    rows.push((
                        t.t("order.r_invoice"),
                        format!("{} {}", amount(a), wallet(target)),
                    ));
                }
            }
        }
        if let Some(f) = c.fee_amount {
            let pct = c
                .fee_pct_applied
                .map_or_else(String::new, |p| format!(" · {}", ui::fmt::percent(p)));
            rows.push((t.t("order.r_fee"), format!("{}{pct}", amount(f))));
        }
        if let Some(r) = c.refund_amount {
            rows.push((t.t("order.r_refund"), amount(r)));
        }
        if let Some(r) = &c.manual_reason {
            rows.push((t.t("order.r_reason"), esc(r)));
        }

        let mut rich = format!("<h3>{title}</h3><p>{head}</p>");
        let mut plain = format!("<b>{title}</b>\n{head}\n");
        if let Some(h) = &self.reconcile {
            let line = h.line(t);
            rich.push_str(&format!("<blockquote>{line}</blockquote>"));
            plain.push_str(&format!("{line}\n"));
        }
        rich.push_str("<table compact>");
        for (k, v) in &rows {
            rich.push_str(&format!(
                "<tr><td>{k}</td><td align=\"right\">{v}</td></tr>"
            ));
            plain.push_str(&format!("{k}: {v}\n"));
        }
        rich.push_str("</table>");

        // Ход по шагам.
        if !c.transitions.is_empty() {
            let last = c.transitions.len() - 1;
            let steps: Vec<String> = c
                .transitions
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    // Сбой — шаг с причиной прямо перед ручным разбором (макет: красная точка).
                    let failed = s.reason.as_deref().is_some_and(|r| !r.is_empty())
                        && c.transitions
                            .get(i + 1)
                            .is_some_and(|n| n.to == "MANUAL_REVIEW");
                    let icon = if s.to == "MANUAL_REVIEW" {
                        "{e:manual}"
                    } else if failed {
                        "{e:dot_act}"
                    } else if i == last && !is_final(&s.to) {
                        "{e:step_now}"
                    } else {
                        "{e:step_done}"
                    };
                    let what = match s.reason.as_deref().filter(|r| !r.is_empty()) {
                        Some(r) => esc(r),
                        None => state_name(&s.to),
                    };
                    format!("{icon} {} · {what}", hms(s.at))
                })
                .collect();
            let sum = t.f("order.d_steps", &[("n", &steps.len())]);
            rich.push_str(&format!(
                "<details open><summary>{sum}</summary><ul>{}</ul></details>",
                steps
                    .iter()
                    .map(|s| format!("<li>{s}</li>"))
                    .collect::<String>()
            ));
            plain.push_str(&format!(
                "<blockquote expandable><b>{sum}</b>\n{}</blockquote>\n",
                steps.join("\n")
            ));
        }

        // Операции.
        if !c.operations.is_empty() {
            let prefix = format!("ord-{}-", c.id);
            let lines: Vec<String> = c
                .operations
                .iter()
                .map(|op| {
                    let key = op.key.replacen(&prefix, "ord-…-", 1);
                    format!("{:<16} {}", esc(&key), esc(&op.status))
                })
                .collect();
            let sum = t.f("order.d_ops", &[("n", &lines.len())]);
            rich.push_str(&format!(
                "<details><summary>{sum}</summary><pre>{}</pre></details>",
                lines.join("\n")
            ));
            plain.push_str(&format!(
                "<blockquote expandable><b>{sum}</b>\n<code>{}</code></blockquote>\n",
                lines.join("\n")
            ));
        }

        // Ответ кошелька — последний входящий.
        if let Some(m) = c.messages.iter().rev().find(|m| !m.outgoing) {
            let who = m.platform.as_deref().unwrap_or(wallet(target));
            let text = crate::views::mask_links(&ui::html::clip(&m.text, 300));
            let line = format!("{}, {} — {}", esc(who), hms(m.at), esc(&text));
            let sum = t.t("order.d_reply");
            rich.push_str(&format!(
                "<details><summary>{sum}</summary><pre>{line}</pre></details>"
            ));
            plain.push_str(&format!(
                "<blockquote expandable><b>{sum}</b>\n{line}</blockquote>\n"
            ));
        }

        // Проводки и удержание.
        if !self.entries.is_empty() || self.held.is_some() {
            let mut lines: Vec<String> = self
                .entries
                .iter()
                .map(|(a, acc)| {
                    let sign = if *a < Decimal::ZERO { "−" } else { "+" };
                    format!("{sign}{}  {}", amount(a.abs()), esc(acc))
                })
                .collect();
            if let Some((h, p)) = self.held {
                lines.push(t.f(
                    "order.held",
                    &[("amount", &amount(h)), ("wallet", &wallet(p))],
                ));
            }
            let sum = t.f("order.d_ledger", &[("n", &self.entries.len())]);
            rich.push_str(&format!(
                "<details><summary>{sum}</summary><pre>{}</pre></details>",
                lines.join("\n")
            ));
            plain.push_str(&format!(
                "<blockquote expandable><b>{sum}</b>\n{}</blockquote>\n",
                lines.join("\n")
            ));
        }

        let mut s = Screen::card(rich, plain.trim_end().to_owned());
        let write = btn(t.t("order.b_write"), &Cb::SupportReply(c.user_id));
        if c.state != OrderState::ManualReview {
            return s.row(vec![write, btn(t.t("order.b_refresh"), &Cb::Order(c.id))]);
        }
        // Ручной разбор: решения по правам (макет — для владельца).
        let take = btn(t.t("order.b_take"), &Cb::Take(c.id));
        s = s.row(vec![
            if self.taken_by.is_none() {
                take.style(botapi::ButtonStyle::Primary)
            } else {
                take
            },
            write,
        ]);
        let posted = c.intake_posted();
        let decide = |k: DecisionKind, key: &str| btn(t.t(key), &Cb::Decide(c.id, k));
        if !posted && role >= Role::Admin {
            s = s.row(vec![decide(DecisionKind::Intake, "order.b_intake")]);
        }
        if posted {
            s = s.row(vec![match c.direction.flow() {
                Flow::CheckExchange => decide(DecisionKind::RetryPayout, "order.b_retry"),
                Flow::InvoicePayment => decide(DecisionKind::RetryInvoice, "order.b_retry_invoice"),
            }]);
            let mut row = vec![decide(DecisionKind::Refund, "order.b_refund")];
            if role >= Role::Admin {
                row.push(decide(DecisionKind::ClosePaid, "order.b_paid"));
            }
            s = s.row(row);
        }
        if role >= Role::Owner {
            s = s.row(vec![
                decide(DecisionKind::Close, "order.b_close").style(botapi::ButtonStyle::Danger),
            ]);
        }
        s
    }
}

fn is_final(state: &str) -> bool {
    OrderState::ALL
        .iter()
        .find(|s| s.as_db_str() == state)
        .is_some_and(|s| s.is_terminal())
}

/// Москва, 5 ноября 2026, `h:m:s` (как в макете).
fn msk(h: u32, m: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 5, h, m, s)
        .single()
        .unwrap_or_default()
        - Duration::hours(3)
}

/// «Сейчас» для данных макета: 14:10:36 — заявке на разборе 6 минут.
pub fn sample_now() -> DateTime<Utc> {
    msk(14, 10, 36)
}

/// Очередь на данных макета.
pub fn sample_queue() -> QueueData {
    let row = |id, public: &str, what: &str, minutes| QueueRow {
        id,
        public_id: public.to_owned(),
        what: what.to_owned(),
        minutes,
        taken_by: None,
        mine: false,
    };
    QueueData {
        items: vec![
            row(15, "K7M2Q9XR4T", "выплата: нет ответа", 6),
            row(16, "7F3A1C9D2B", "незнакомый ответ", 2),
        ],
        // Строка ручной оплаты счёта — в модели уже есть, из базы придёт вместе с ручной
        // оплатой счёта.
        invoices: vec![row(17, "Q1W2E3R4T5", "счёт CryptoBot 12.00", 4)],
    }
}

/// Карточка заявки на данных макета: CB → xR, 100.00 → 97.50, ручной разбор 6 минут.
pub fn sample_order() -> OrderView {
    use storage::admin::{OpLine, PostingLine, TransitionRow, WalletMessageLine};
    let step = |to: &str, reason: Option<&str>, at| TransitionRow {
        from: None,
        to: to.to_owned(),
        actor: "system".into(),
        reason: reason.map(str::to_owned),
        at,
    };
    let op = |id, key: &str, status: &str| OpLine {
        id,
        kind: String::new(),
        status: status.to_owned(),
        key: key.to_owned(),
        amount: None,
        asset: None,
        external_id: None,
        last_error: None,
        response: None,
        created_at: msk(14, 4, 2),
    };
    let card = OrderCard {
        id: 15,
        public_id: "K7M2Q9XR4T".into(),
        user_id: 51_234_567,
        direction: Direction::CbToXrCheck,
        asset: Asset::Usdt,
        state: OrderState::ManualReview,
        version: 5,
        intake_platform: Platform::CryptoBot,
        intake_start_param: None,
        intake_amount: Some(Decimal::from(100)),
        intake_asset: Some("USDT".into()),
        invoice_start_param: None,
        invoice_amount: None,
        invoice_fiat: None,
        invoice_fiat_amount: None,
        quote_amount_in: None,
        fee_pct_applied: Some(Decimal::new(25, 1)),
        fee_amount: Some(Decimal::new(250, 2)),
        payout_amount: Some(Decimal::new(9750, 2)),
        refund_amount: None,
        payout_check_url: None,
        failure_code: None,
        manual_reason: None,
        created_at: msk(14, 4, 1),
        finished_at: None,
        transitions: vec![
            step("RECEIVED", Some("чек принят"), msk(14, 4, 2)),
            step("PAYOUT_PENDING", Some("резерв удержан"), msk(14, 4, 3)),
            step(
                "PAYOUT_PENDING",
                Some("чек xRocket: нет ответа 30 с"),
                msk(14, 4, 4),
            ),
            step(
                "MANUAL_REVIEW",
                Some("сверка: чек не найден"),
                msk(14, 4, 36),
            ),
        ],
        operations: vec![
            op(1, "ord-15-intake-1", "done"),
            op(2, "ord-15-payout", "failed"),
        ],
        postings: vec![PostingLine {
            kind: "intake".into(),
            memo: None,
            created_by: "system".into(),
            at: msk(14, 4, 2),
        }],
        messages: vec![WalletMessageLine {
            platform: Some("xRocket v3".into()),
            outgoing: false,
            text: "нет ответа".into(),
            at: msk(14, 4, 4),
        }],
    };
    OrderView {
        card,
        completed: 23,
        account: Some("ub-1".into()),
        taken_by: None,
        entries: vec![
            (Decimal::from(100), "wallet:cb:ub-1".into()),
            (Decimal::from(-100), "client:51234567".into()),
        ],
        held: Some((Decimal::new(9750, 2), Platform::XRocket)),
        reconcile: None,
    }
}

impl Inner {
    /// Очередь разбора из базы; `viewer` — чтобы отличить «свои» заявки.
    pub(crate) async fn queue_data(&self, viewer: Option<i64>) -> Result<QueueData, OpsError> {
        let mut conn = self.pool.acquire().await?;
        let items = storage::admin::review_queue(&mut conn).await?;
        let mut rows = Vec::new();
        for item in &items {
            let taken_by = match item.taken_by {
                Some(id) => Some(
                    storage::staff::get(&mut conn, id)
                        .await?
                        .map_or_else(|| id.to_string(), |m| m.display_name),
                ),
                None => None,
            };
            rows.push(QueueRow {
                id: item.id,
                public_id: item.public_id.clone(),
                what: brief(item.manual_reason.as_deref().unwrap_or("")),
                minutes: (Utc::now() - item.since).num_minutes().max(0),
                taken_by,
                mine: item.taken_by.is_some() && item.taken_by == viewer,
            });
        }
        Ok(QueueData {
            items: rows,
            invoices: Vec::new(),
        })
    }

    pub(crate) async fn queue_for(
        &self,
        viewer: Option<i64>,
        page: u32,
    ) -> Result<Screen, OpsError> {
        Ok(self
            .queue_data(viewer)
            .await?
            .screen(&self.texts, &self.links(), page))
    }

    /// Данные карточки заявки из базы.
    pub(crate) async fn order_view(&self, id: i64) -> Result<Option<OrderView>, OpsError> {
        let mut conn = self.pool.acquire().await?;
        let card = match storage::admin::order_card(&mut conn, id).await {
            Ok(c) => c,
            Err(storage::StorageError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let completed = storage::client::history_totals(&mut conn, card.user_id)
            .await?
            .completed;
        let entries: Vec<(Decimal, String)> = sqlx::query_as(
            "SELECT e.amount, a.code FROM ledger_entries e \
               JOIN ledger_accounts a ON a.id = e.account_id \
               JOIN ledger_transactions t ON t.id = e.tx_id \
              WHERE t.order_id = $1 ORDER BY e.id",
        )
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
        let held: Option<(Decimal, i16)> = sqlx::query_as(
            "SELECT amount, wallet_account_id FROM holds WHERE order_id = $1 AND state = 'active'",
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
        let wallets = storage::wallets::list(&mut conn).await?;
        let held = held.and_then(|(a, w)| {
            wallets
                .iter()
                .find(|x| x.wallet_id == w)
                .map(|x| (a, x.platform))
        });
        let account = wallets
            .iter()
            .find(|w| w.is_active && w.platform == card.direction.target())
            .map(|w| w.userbot_label.clone());
        let taken_by = match storage::admin::review_queue(&mut conn)
            .await?
            .into_iter()
            .find(|q| q.id == id)
            .and_then(|q| q.taken_by)
        {
            Some(sid) => Some(
                storage::staff::get(&mut conn, sid)
                    .await?
                    .map_or_else(|| sid.to_string(), |m| m.display_name),
            ),
            None => None,
        };
        // Последняя сверка, учитывавшая неясную операцию заявки (шаг 1.5).
        // Время сверки, операции «прошли» по балансу, операция, её вид и кошелёк.
        type HintRow = (DateTime<Utc>, Option<Vec<i64>>, i64, String, String);
        let hint: Option<HintRow> = sqlx::query_as(
            "SELECT r.at, r.matched_ops, op.id, op.kind, op.platform FROM operations op \
               JOIN reconciliations r ON op.id = ANY(r.unsettled_ops) AND r.verdict = 'ok' \
              WHERE op.order_id = $1 AND op.status = 'unknown' \
              ORDER BY r.at DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
        let reconcile = hint.and_then(|(at, matched, op, kind, platform)| {
            Some(ReconcileHint {
                at,
                platform: Platform::from_db_name(&platform)?,
                kind,
                done: matched.map(|m| m.contains(&op)),
            })
        });
        Ok(Some(OrderView {
            card,
            completed,
            account,
            taken_by,
            entries,
            held,
            reconcile,
        }))
    }

    /// Карточка заявки; `staff` — кнопки решений по его роли.
    pub(crate) async fn order(
        &self,
        staff: Option<&StaffMember>,
        id: i64,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        match self.order_view(id).await? {
            Some(v) => {
                let role = staff.map_or(Role::Operator, |m| m.role);
                let screen = v.screen(t, role, Utc::now(), &self.links());
                // «‹ Назад» — в список, откуда открыли; иначе — в разбор или «Заявки».
                let default = if v.card.state == OrderState::ManualReview {
                    Cb::Queue
                } else {
                    Cb::Page(List::Orders, 0)
                };
                let back = staff.map_or(default.clone(), |m| self.list_or(m.tg_user_id, default));
                Ok(screen.row(nav::back(t, &back)))
            }
            None => {
                Ok(Screen::new(t.t("order.not_found"))
                    .row(vec![btn(t.t("common.home"), &Cb::Menu)]))
            }
        }
    }
}
