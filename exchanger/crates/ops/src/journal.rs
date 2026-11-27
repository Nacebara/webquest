//! Журнал действий словами (П-31, макет «Журнал»: «взял(а) K7M2…», «комиссия CB → xR
//! 2.0 → 2.4», «бан 48812211 · спам»). В базе действие — код (`order.take`), здесь — фраза из
//! раздела `[journal]` в `ru.toml`; заявка — публичным номером, клиент и сотрудник — по имени.

use std::collections::HashMap;

use domain::{Decimal, Direction};
use serde_json::Value;
use storage::audit::AuditRow;

use crate::cb::{Cb, Flag};
use crate::nav::Links;
use crate::texts::{Texts, esc};

/// Имена целей для страницы журнала: публичные номера заявок, ярлыки аккаунтов, имена
/// сотрудников (по одному запросу на страницу).
#[derive(Debug, Default, Clone)]
pub struct Names {
    pub orders: HashMap<i64, String>,
    pub userbots: HashMap<i64, String>,
    pub staff: HashMap<i64, String>,
}

/// Действия, у которых есть фраза (`journal.<действие с _ вместо .>`). Новое действие журнала —
/// сюда и в `ru.toml`; тест проверяет, что фраза есть.
#[cfg(test)]
pub(crate) const ACTIONS: [&str; 41] = [
    "order.take",
    "order.call_admin",
    "order.retry_payout",
    "order.retry_invoice_payment",
    "order.refund",
    "order.close_as_paid",
    "order.close_without_payout",
    "order.confirm_intake",
    "userbot.end_session",
    "userbot.pause",
    "userbot.resume",
    "wallet.move",
    "direction.set",
    "direction.open",
    "direction.close",
    "direction.clear_auto_pause",
    "promo.settings",
    "promo.switch",
    "promo.broadcast",
    "reserve.target",
    "user.terms",
    "user.daily_limit",
    "user.note",
    "user.ban",
    "user.unban",
    "settings.flag",
    "settings.emoji_style",
    "settings.custom_emoji",
    "settings.maint_text",
    "settings.passcode_free_limit",
    "staff.add",
    "staff.role",
    "staff.limit",
    "staff.disable",
    "capital.in",
    "capital.out",
    "capital.loss",
    "invest.approve",
    "invest.reject",
    "invest.resolve",
    "invest.terms",
];

/// Поля направления в порядке показа: «комиссия 2.0 → 2.4, мин. сумма 10 → 20».
const DIRECTION_FIELDS: [&str; 10] = [
    "fee_pct",
    "min_fee",
    "min_amount",
    "max_amount",
    "new_user_max_amount",
    "fee_mode",
    "skew_k_pct",
    "fee_floor_pct",
    "fee_cap_pct",
    "fiat_buffer_pct",
];

const TERM_FIELDS: [&str; 5] = [
    "fee_pct",
    "min_fee",
    "min_amount",
    "max_amount",
    "newcomer_exempt",
];

/// Число как в макете: `2.0`, `2.4`, `5000`.
fn num(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => return Some(if *b { "да" } else { "нет" }.to_owned()),
        Value::Null => return Some("—".to_owned()),
        _ => return None,
    };
    Some(match s.parse::<Decimal>() {
        Ok(d) => {
            let n = d.normalize().to_string();
            if n.contains('.') || n.len() > 2 {
                n
            } else {
                format!("{n}.0")
            }
        }
        Err(_) => s,
    })
}

/// «комиссия 2.0 → 2.4, мин. сумма 10 → 20»: изменившиеся поля из `before`/`after`.
fn changes(t: &Texts, prefix: &str, fields: &[&str], r: &AuditRow) -> String {
    let (Some(before), Some(after)) = (&r.before, &r.after) else {
        return String::new();
    };
    fields
        .iter()
        .filter(|f| before.get(**f) != after.get(**f))
        .filter_map(|f| {
            let a = num(before.get(*f).unwrap_or(&Value::Null))?;
            let b = num(after.get(*f).unwrap_or(&Value::Null))?;
            Some(t.f(
                "journal.change",
                &[
                    ("field", &t.t(&format!("journal.{prefix}_{f}"))),
                    ("before", &esc(&a)),
                    ("after", &esc(&b)),
                ],
            ))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn direction_of(id: &str) -> Option<Direction> {
    id.split(':').next_back().and_then(|d| d.parse().ok())
}

/// Что сделано — словами, со ссылками на заявку и клиента.
pub fn what(t: &Texts, links: &Links, names: &Names, r: &AuditRow) -> String {
    let id = r.target_id.as_deref().unwrap_or_default();
    let num_id = id.split(':').next().and_then(|x| x.parse::<i64>().ok());
    let order = match num_id {
        Some(n) if r.target_type.as_deref() == Some("order") => {
            let public = names
                .orders
                .get(&n)
                .map_or_else(|| n.to_string(), |p| p.clone());
            links.link(&format!("<code>{}</code>", esc(&public)), &Cb::Order(n))
        }
        _ => esc(id),
    };
    let user = match num_id {
        Some(n) if r.target_type.as_deref() == Some("user") => {
            links.link(&esc(&n.to_string()), &Cb::User(n))
        }
        _ => esc(id),
    };
    let direction =
        direction_of(id).map_or_else(|| esc(id), |d| t.t(&format!("dash.short_{}", d.code())));
    let after = r.after.as_ref();
    let field = |k: &str| {
        after
            .and_then(|a| a.get(k))
            .and_then(num)
            .unwrap_or_default()
    };
    let target = match (r.target_type.as_deref(), num_id) {
        (Some("userbot"), Some(n)) => names.userbots.get(&n).map_or_else(|| esc(id), |l| esc(l)),
        (Some("staff"), Some(n)) => names.staff.get(&n).map_or_else(|| esc(id), |s| esc(s)),
        (Some("wallet"), _) => domain::Platform::from_db_name(id)
            .map_or_else(|| esc(id), |p| t.t(&format!("names.{}", p.db_name()))),
        _ => esc(id),
    };
    let state = match r.action.as_str() {
        "settings.flag" | "promo.switch" => {
            let on = after
                .and_then(|a| a.get("on"))
                .and_then(Value::as_bool)
                .unwrap_or_default();
            let until = after
                .and_then(|a| a.get("until"))
                .and_then(Value::as_str)
                .and_then(|u| u.parse::<chrono::DateTime<chrono::Utc>>().ok());
            match until {
                Some(u) if on => t.f("maint.state_until", &[("time", &crate::dash::hhmm(u))]),
                _ => t.t(if on { "common.on" } else { "common.off" }),
            }
        }
        _ => String::new(),
    };
    let flag = [Flag::Newcomer, Flag::Maintenance, Flag::KillSwitch]
        .into_iter()
        .find(|f| f.key() == id)
        .map_or_else(
            || esc(id),
            |f| {
                t.t(match f {
                    Flag::Newcomer => "settings.n_newcomer",
                    Flag::Maintenance => "settings.n_maint",
                    Flag::KillSwitch => "settings.n_kill",
                })
            },
        );
    let role = |v: Option<&Value>| {
        v.and_then(|v| v.get("role").or(Some(v)))
            .and_then(Value::as_str)
            .map_or_else(String::new, |r| t.t(&format!("roles.{r}")))
    };
    let changes = match r.action.as_str() {
        "direction.set" => changes(t, "f_dir", &DIRECTION_FIELDS, r),
        "user.terms" => changes(t, "f_terms", &TERM_FIELDS, r),
        "reserve.target" => changes(t, "f_reserve", &["target"], r),
        "settings.passcode_free_limit" => changes(t, "f_passcode", &["limit"], r),
        "user.daily_limit" => changes(t, "f_user", &["limit"], r),
        "staff.limit" => changes(t, "f_staff", &["limit"], r),
        _ => String::new(),
    };
    let key = format!("journal.{}", r.action.replace('.', "_"));
    if !t.has(&key) {
        return t.f(
            "journal.other",
            &[
                ("action", &crate::texts::code(&r.action)),
                ("target", &esc(id)),
            ],
        );
    }
    t.f(
        &key,
        &[
            ("order", &order),
            ("user", &user),
            ("direction", &direction),
            ("target", &target),
            ("flag", &flag),
            ("state", &state),
            ("changes", &changes),
            ("amount", &field("amount")),
            ("account", &esc(&field("account"))),
            (
                "was",
                &esc(&r
                    .before
                    .as_ref()
                    .and_then(|b| b.get("account"))
                    .and_then(num)
                    .unwrap_or_default()),
            ),
            ("role", &role(after)),
            ("was_role", &role(r.before.as_ref())),
            ("name", &esc(&field("name"))),
        ],
    )
}

/// Причина действия: колонка `reason` или `reason` в `after` (бан, разблокировка, потеря).
pub fn reason(r: &AuditRow) -> Option<String> {
    r.reason.clone().or_else(|| {
        r.after
            .as_ref()
            .and_then(|a| a.get("reason"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// У каждого действия журнала есть фраза, и в ней нет кода действия.
    #[test]
    fn every_action_has_words() {
        let t = Texts::builtin().unwrap();
        for a in ACTIONS {
            let key = format!("journal.{}", a.replace('.', "_"));
            assert!(t.has(&key), "ops/ru.toml has no {key}");
            assert!(!t.t(&key).contains(a), "{key}");
        }
        for f in DIRECTION_FIELDS {
            assert!(t.has(&format!("journal.f_dir_{f}")), "{f}");
        }
        for f in TERM_FIELDS {
            assert!(t.has(&format!("journal.f_terms_{f}")), "{f}");
        }
    }

    fn row(action: &str, ty: &str, id: &str, before: Value, after: Value) -> AuditRow {
        AuditRow {
            id: 1,
            staff_id: 501,
            staff_name: Some("Олег".into()),
            action: action.into(),
            target_type: Some(ty.into()),
            target_id: Some(id.into()),
            before: Some(before),
            after: Some(after),
            reason: None,
            double_confirmed: true,
            at: chrono::Utc::now(),
        }
    }

    /// Макет «Журнал»: «взял(а) K7M2…», «комиссия CB → xR 2.0 → 2.4», «бан 48812211 · спам».
    #[test]
    fn mockup_lines() {
        let t = Texts::builtin().unwrap();
        let links = Links::new(None);
        let mut names = Names::default();
        names.orders.insert(15, "K7M2Q9XR4T".into());
        let take = what(
            &t,
            &links,
            &names,
            &row("order.take", "order", "15", Value::Null, Value::Null),
        );
        assert!(
            take.contains("взял(а)") && take.contains("K7M2Q9XR4T") && !take.contains("№15"),
            "{take}"
        );
        let fee = what(
            &t,
            &links,
            &names,
            &row(
                "direction.set",
                "direction",
                "cb_to_xr_check",
                serde_json::json!({"fee_pct": "2.0", "min_fee": "0.1"}),
                serde_json::json!({"fee_pct": "2.4", "min_fee": "0.1"}),
            ),
        );
        assert_eq!(fee, "CB → xR: комиссия <code>2.0 → 2.4</code>");
        let ban = row(
            "user.ban",
            "user",
            "48812211",
            Value::Null,
            serde_json::json!({"days": 0, "reason": "спам"}),
        );
        assert!(what(&t, &links, &names, &ban).starts_with("бан "));
        assert_eq!(reason(&ban).as_deref(), Some("спам"));
        let kill = what(
            &t,
            &links,
            &names,
            &row(
                "settings.flag",
                "settings",
                "kill_switch_payouts",
                serde_json::json!({"on": false}),
                serde_json::json!({"on": true}),
            ),
        );
        assert_eq!(kill, "Стоп-кран выплат: вкл");
    }
}
