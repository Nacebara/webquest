//! Списки пульта с нажимаемыми строками и страницами ([`crate::nav`]): «Заявки», «Клиенты»,
//! заявки клиента.

use chrono::{DateTime, Utc};
use domain::fsm::OrderState;
use domain::{Decimal, Direction};
use storage::staff::{Role, StaffMember};

use crate::dash::amount;
use crate::nav::{self, List};
use crate::reserves::short;
use crate::texts::{Texts, code, esc};
use crate::{Cb, Inner, OpsError, Screen, btn};

/// Строка «Заявок».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRow {
    pub id: i64,
    pub public_id: String,
    pub direction: Direction,
    pub state: OrderState,
    pub amount: Option<Decimal>,
    pub created_at: DateTime<Utc>,
}

/// Точка состояния заявки.
pub(crate) fn state_dot(s: OrderState) -> &'static str {
    match s {
        OrderState::Completed | OrderState::ClosedManual => "{e:dot_ok}",
        OrderState::ManualReview => "{e:dot_act}",
        s if s.is_terminal() => "{e:dot_closed}",
        _ => "{e:dot_work}",
    }
}

/// «14:04» сегодня, «вчера», «5 ноя» раньше (по Москве).
pub(crate) fn day_or_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let clock = ui::Clock::msk();
    let months = [
        "янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
    ]
    .map(String::from);
    clock.day_label(at, now, "вчера", &months)
}

/// Таблица заявок: «Заявка · Что · Статус», номер — ссылка на карточку.
pub(crate) fn orders_table(
    links: &nav::Links,
    t: &Texts,
    rows: &[OrderRow],
    now: DateTime<Utc>,
) -> (String, String) {
    let mut rich = format!(
        "<table striped compact><tr><th>{}</th><th>{}</th><th align=\"right\">{}</th></tr>",
        t.t("orders.c_order"),
        t.t("orders.c_what"),
        t.t("orders.c_state"),
    );
    let mut plain = Vec::new();
    for r in rows {
        let num = links.link(&esc(&r.public_id), &Cb::Order(r.id));
        let what = match r.amount {
            Some(a) => format!("{} · {}", short(r.direction), amount(a)),
            None => short(r.direction).to_owned(),
        };
        let state = format!(
            "{} {} · {}",
            state_dot(r.state),
            t.t(&format!("states.{}", r.state.as_db_str())),
            day_or_time(r.created_at, now)
        );
        rich.push_str(&format!(
            "<tr><td>{num}</td><td>{what}</td><td align=\"right\">{state}</td></tr>"
        ));
        plain.push(format!("{num} — {what} · {state}"));
    }
    rich.push_str("</table>");
    (
        rich,
        format!("<blockquote>{}</blockquote>", plain.join("\n")),
    )
}

/// Строка заявки из базы: id, номер, направление, состояние, сумма, когда.
type RawOrder = (i64, String, String, String, Option<Decimal>, DateTime<Utc>);

async fn order_rows(
    conn: &mut sqlx::PgConnection,
    user: Option<i64>,
    limit: usize,
    offset: usize,
) -> Result<(Vec<OrderRow>, usize), OpsError> {
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM orders WHERE ($1::BIGINT IS NULL OR user_id = $1)",
    )
    .bind(user)
    .fetch_one(&mut *conn)
    .await?;
    let raw: Vec<RawOrder> = sqlx::query_as(
        "SELECT id, public_id, direction, state, \
                COALESCE(intake_amount, invoice_amount, quote_amount_in), created_at \
           FROM orders WHERE ($1::BIGINT IS NULL OR user_id = $1) \
          ORDER BY id DESC LIMIT $2 OFFSET $3",
    )
    .bind(user)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .bind(i64::try_from(offset).unwrap_or(0))
    .fetch_all(&mut *conn)
    .await?;
    let rows = raw
        .into_iter()
        .filter_map(|(id, public_id, direction, state, amount, created_at)| {
            Some(OrderRow {
                id,
                public_id,
                direction: direction.parse().ok()?,
                state: OrderState::ALL
                    .into_iter()
                    .find(|s| s.as_db_str() == state)?,
                amount,
                created_at,
            })
        })
        .collect();
    Ok((rows, usize::try_from(total).unwrap_or(0)))
}

impl Inner {
    /// «Заявки»: последние заявки всех клиентов, по [`nav::PAGE`], номер — ссылка.
    pub(crate) async fn orders(&self, page: u32) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM orders")
            .fetch_one(&mut *conn)
            .await?;
        let total = usize::try_from(total).unwrap_or(0);
        let (page, range) = nav::window(total, page);
        let (rows, _) = order_rows(&mut conn, None, nav::PAGE, range.start).await?;
        let title = t.f("orders.title", &[("n", &total)]);
        let mut s = if rows.is_empty() {
            Screen::card(
                format!("<h3>{title}</h3><p>{}</p>", t.t("orders.empty")),
                format!("<b>{title}</b>\n{}", t.t("orders.empty")),
            )
        } else {
            let (table, plain) = orders_table(&self.links(), t, &rows, Utc::now());
            Screen::card(
                format!(
                    "<h3>{title}</h3>{table}<footer>{}</footer>",
                    t.t("orders.hint")
                ),
                format!("<b>{title}</b>\n{plain}\n<i>{}</i>", t.t("orders.hint")),
            )
        };
        s = s.row(nav::pager(t, List::Orders, page, total));
        Ok(s.row(vec![
            btn(t.t("orders.b_find"), &Cb::OrderAsk),
            btn(t.t("nav.refresh"), &Cb::Page(List::Orders, page)),
        ])
        .row(nav::back(t, &Cb::Menu)))
    }

    /// «Клиенты»: последние клиенты по последней заявке, ID — ссылка на карточку.
    pub(crate) async fn users(&self, staff: &StaffMember, page: u32) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(&mut *conn)
            .await?;
        let total = usize::try_from(total).unwrap_or(0);
        let (page, range) = nav::window(total, page);
        type Raw = (i64, Option<String>, i64, Option<DateTime<Utc>>, bool);
        let rows: Vec<Raw> = sqlx::query_as(
            "SELECT u.tg_user_id, u.username, \
                    (SELECT count(*) FROM orders o WHERE o.user_id = u.tg_user_id AND o.state = 'COMPLETED'), \
                    (SELECT max(o.created_at) FROM orders o WHERE o.user_id = u.tg_user_id), \
                    EXISTS (SELECT 1 FROM bans b WHERE b.user_id = u.tg_user_id AND b.lifted_at IS NULL \
                              AND (b.expires_at IS NULL OR b.expires_at > now())) \
               FROM users u \
              ORDER BY 4 DESC NULLS LAST, u.last_seen_at DESC \
              LIMIT $1 OFFSET $2",
        )
        .bind(i64::try_from(nav::PAGE).unwrap_or(8))
        .bind(i64::try_from(range.start).unwrap_or(0))
        .fetch_all(&mut *conn)
        .await?;
        let title = t.f("users.title", &[("n", &total)]);
        let now = Utc::now();
        let mut s = if rows.is_empty() {
            Screen::card(
                format!("<h3>{title}</h3><p>{}</p>", t.t("users.empty")),
                format!("<b>{title}</b>\n{}", t.t("users.empty")),
            )
        } else {
            let mut rich = format!(
                "<h3>{title}</h3><table striped compact><tr><th>{}</th><th align=\"right\">{}</th><th align=\"right\">{}</th></tr>",
                t.t("users.c_client"),
                t.t("users.c_done"),
                t.t("users.c_last"),
            );
            let mut plain = Vec::new();
            for (id, username, done, last, banned) in &rows {
                let mut who = self.link(&id.to_string(), &Cb::User(*id));
                if let Some(u) = username {
                    who.push_str(&format!(" @{}", esc(u)));
                }
                if *banned {
                    who.push_str(" {e:ban}");
                }
                let last = last.map_or_else(|| t.t("users.never"), |at| day_or_time(at, now));
                rich.push_str(&format!(
                    "<tr><td>{who}</td><td align=\"right\">{done}</td><td align=\"right\">{last}</td></tr>"
                ));
                plain.push(format!("{who} — {done} · {last}"));
            }
            rich.push_str(&format!("</table><footer>{}</footer>", t.t("users.hint")));
            Screen::card(
                rich,
                format!(
                    "<b>{title}</b>\n<blockquote>{}</blockquote>\n<i>{}</i>",
                    plain.join("\n"),
                    t.t("users.hint")
                ),
            )
        };
        s = s.row(nav::pager(t, List::Users, page, total));
        let mut row = vec![btn(t.t("users.b_find"), &Cb::UserAsk)];
        if staff.role >= Role::Admin {
            row.push(btn(t.t("menu.investors"), &Cb::Investors));
        }
        Ok(s.row(row).row(nav::back(t, &Cb::Menu)))
    }

    /// Заявки клиента, по страницам.
    pub(crate) async fn user_orders(&self, user: i64, page: u32) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let (_, total) = order_rows(&mut conn, Some(user), 0, 0).await?;
        let (page, range) = nav::window(total, page);
        let (rows, _) = order_rows(&mut conn, Some(user), nav::PAGE, range.start).await?;
        let title = t.f(
            "orders.user_title",
            &[("user", &code(&user.to_string())), ("n", &total)],
        );
        let mut s = if rows.is_empty() {
            Screen::card(
                format!("<h3>{title}</h3><p>{}</p>", t.t("orders.empty")),
                format!("<b>{title}</b>\n{}", t.t("orders.empty")),
            )
        } else {
            let (table, plain) = orders_table(&self.links(), t, &rows, Utc::now());
            Screen::card(
                format!("<h3>{title}</h3>{table}"),
                format!("<b>{title}</b>\n{plain}"),
            )
        };
        s = s.row(nav::pager_with(t, page, total, |p| Cb::UserOrders(user, p)));
        Ok(s.row(nav::back(t, &Cb::User(user))))
    }

    /// Последние заявки клиента для его карточки (до 5) и сколько всего.
    pub(crate) async fn user_recent(&self, user: i64) -> Result<(String, usize), OpsError> {
        let mut conn = self.pool.acquire().await?;
        let (rows, total) = order_rows(&mut conn, Some(user), 5, 0).await?;
        if rows.is_empty() {
            return Ok((String::new(), 0));
        }
        let lines: Vec<String> = rows
            .iter()
            .map(|r| {
                format!(
                    "{} {} · {} · {}",
                    state_dot(r.state),
                    self.link(&esc(&r.public_id), &Cb::Order(r.id)),
                    short(r.direction),
                    r.amount.map_or_else(String::new, amount)
                )
            })
            .collect();
        Ok((lines.join("\n"), total))
    }
}
