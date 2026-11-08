//! Главный экран пульта `/dash` — макет «Главный экран · вариант А»: одна карточка, которая сама
//! меняет вид по обстановке.
//!
//! - **Обычный день** — «Нужно решить · N» с цветными точками, плитки «Обменов / Оборот /
//!   Прибыль» со сравнением к вчера, резерв «Доступно · от цели», подвал, кнопки со
//!   счётчиками (синие — где ждут человека).
//! - **Пожар** — недостача, пауза выплат, сброшенный вход: наверху и красным, резерв
//!   «Леджер · Реально», красная кнопка — только сейчас.
//! - **Тишина** — «Всё спокойно», цифры дня и резервы.
//!
//! Экран правится сам раз в минуту и сразу при новом событии ([`Inner::refresh_dashes`]):
//! сообщения главного экрана у персонала помнятся в базе (`settings.ops_dash_messages`).

use chrono::{DateTime, Duration, TimeZone as _, Utc};
use domain::{Asset, Decimal, Platform};
use storage::admin::Thresholds;

use crate::texts::{Texts, esc};
use crate::{Cb, Inner, OpsError, Screen, btn};

/// Где помнятся сообщения главного экрана: `[[chat, message_id], …]`.
const DASH_MESSAGES: &str = "ops_dash_messages";

/// Что ждёт человека — строка «Нужно решить».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// Сверка: в кошельке меньше, чем по леджеру, выплаты с него на паузе.
    Shortfall { platform: Platform, amount: Decimal },
    /// Заявки на ручном разборе; возраст старейшей, минуты.
    Review { n: i64, oldest_mins: i64 },
    /// Аккаунт юзербота потерял вход (`logged_out`) или заблокирован (`banned`).
    Session { label: String, banned: bool },
    /// Счета, которые нужно оплатить руками, и их сумма.
    ManualInvoice { n: i64, amount: Decimal },
    /// Незнакомый ответ кошелька: приём на платформе на автопаузе.
    Unknown { n: i64, platform: Platform },
    /// Прочие автопаузы направлений — ждут решения человека.
    AutoPause { n: i64 },
    /// Баланс кошелька не читается — сверки нет.
    Unreadable { platform: Platform },
    /// Движения инвесторов ждут решения.
    Invest { n: i64 },
}

impl Need {
    /// Пожар: из-за этого деньги могут не дойти до клиентов.
    fn fire(&self) -> bool {
        matches!(self, Need::Shortfall { .. } | Need::Session { .. })
    }

    /// Сколько дел в строке — для заголовка «Нужно решить · N».
    fn count(&self) -> i64 {
        match self {
            Need::Review { n, .. }
            | Need::ManualInvoice { n, .. }
            | Need::Unknown { n, .. }
            | Need::AutoPause { n }
            | Need::Invest { n } => *n,
            Need::Shortfall { .. } | Need::Session { .. } | Need::Unreadable { .. } => 1,
        }
    }

    /// Порядок строк: сначала деньги, потом люди и кошельки.
    fn rank(&self) -> u8 {
        match self {
            Need::Shortfall { .. } => 0,
            Need::Review { .. } => 1,
            Need::Session { .. } => 2,
            Need::ManualInvoice { .. } => 3,
            Need::Unknown { .. } => 4,
            Need::AutoPause { .. } => 5,
            Need::Unreadable { .. } => 6,
            Need::Invest { .. } => 7,
        }
    }

    /// Красная точка — деньги клиента ждут человека; оранжевая — остальное.
    fn dot(&self) -> &'static str {
        match self {
            Need::Shortfall { .. } | Need::Review { .. } | Need::Session { .. } => "{e:dot_act}",
            _ => "{e:dot_wait}",
        }
    }
}

/// Цифры дня: сегодня и вчера к этому же часу.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kpis {
    pub completed: i64,
    pub turnover: Decimal,
    pub profit: Decimal,
    /// Вчера к этому же часу: обменов, оборот, прибыль.
    pub prev: Option<(i64, Decimal, Decimal)>,
}

/// Резерв кошелька.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reserve {
    pub platform: Platform,
    /// Можно выплатить: леджер минус удержания.
    pub available: Decimal,
    pub ledger: Decimal,
    /// Реально в кошельке по последней сверке.
    pub actual: Option<Decimal>,
    pub thresholds: Option<Thresholds>,
}

/// Всё для главного экрана — из базы ([`Inner::dash_data`]) или из макета ([`sample_dash`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashData {
    /// Стоп-кран: все выплаты на паузе.
    pub kill_switch: bool,
    /// Выплаты с кошелька на паузе по сверке: платформа и с какого времени.
    pub paused: Vec<(Platform, Option<DateTime<Utc>>)>,
    pub needs: Vec<Need>,
    pub kpis: Kpis,
    pub reserves: Vec<Reserve>,
    /// Аккаунты юзерботов: на связи / всего.
    pub accounts: (usize, usize),
    /// Последняя сверка и было ли в ней расхождение.
    pub reconciled_at: Option<DateTime<Utc>>,
    pub reconcile_bad: bool,
}

/// Вид главного экрана.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashState {
    Normal,
    Fire,
    Calm,
}

fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::CryptoBot => "CryptoBot",
        Platform::XRocket => "xRocket",
    }
}

fn platform_icon(p: Platform) -> &'static str {
    match p {
        Platform::CryptoBot => "{e:cb}",
        Platform::XRocket => "{e:xr}",
    }
}

/// «2 140.00»: разряды через неразрывный пробел, два знака.
fn amount(v: Decimal) -> String {
    let s = ui::fmt::amount(v, Asset::Usdt);
    let (sign, s) = match s.strip_prefix('-') {
        Some(rest) => ("−", rest.to_owned()),
        None => ("", s),
    };
    let (int, frac) = s.split_once('.').unwrap_or((&s, "00"));
    let int: Decimal = int.parse().unwrap_or_default();
    format!("{sign}{}.{frac}", ui::fmt::grouped(int))
}

/// «143 %» — доля от цели, целым.
fn share(part: Decimal, whole: Decimal) -> Option<i64> {
    if whole <= Decimal::ZERO {
        return None;
    }
    let pct = (part * Decimal::ONE_HUNDRED / whole).round();
    pct.to_string().parse().ok()
}

fn pct(n: i64) -> String {
    format!("{n}\u{a0}%")
}

/// Изменение к вчера, целым процентом; вчера нуль — сравнивать не с чем.
fn delta(cur: Decimal, prev: Decimal) -> Option<i64> {
    share(cur - prev, prev)
}

/// Время `14:15`: в карточке — в поясе читателя (`<tg-time>`), в запасном тексте — по Москве.
fn hhmm(at: DateTime<Utc>) -> String {
    let clock = ui::Clock::new(3 * 3600, "").unwrap_or_else(ui::Clock::msk);
    format!(
        "<tg-time unix=\"{}\" format=\"t\">{}</tg-time>",
        at.timestamp(),
        clock.text(at, ui::TimeFormat::Time)
    )
}

/// Точка резерва: не меньше порога «мало» — зелёная, ниже критичного — красная.
fn reserve_dot(r: &Reserve) -> &'static str {
    match &r.thresholds {
        Some(th) if r.available >= th.low => "{e:dot_ok}",
        Some(th) if r.available >= th.critical => "{e:dot_wait}",
        Some(_) => "{e:dot_act}",
        None => "",
    }
}

impl DashData {
    pub fn state(&self) -> DashState {
        if self.kill_switch || !self.paused.is_empty() || self.needs.iter().any(Need::fire) {
            DashState::Fire
        } else if self.needs.is_empty() {
            DashState::Calm
        } else {
            DashState::Normal
        }
    }

    fn need_label(&self, t: &Texts, n: &Need) -> (String, String) {
        match n {
            Need::Shortfall {
                platform,
                amount: a,
            } => (
                t.f("dash.n_shortfall", &[("name", &platform_name(*platform))]),
                t.f("dash.v_shortfall", &[("amount", &amount(*a))]),
            ),
            Need::Review { n, oldest_mins } => {
                let age = if *oldest_mins >= 120 {
                    t.f("dash.age_hours", &[("n", &(oldest_mins / 60))])
                } else {
                    t.f("dash.age_min", &[("n", oldest_mins)])
                };
                (
                    t.t("dash.n_review"),
                    t.f("dash.v_review", &[("n", n), ("age", &age)]),
                )
            }
            Need::Session { label, banned } => (
                t.f(
                    if *banned {
                        "dash.n_banned"
                    } else {
                        "dash.n_session"
                    },
                    &[("label", &esc(label))],
                ),
                t.t("dash.v_session"),
            ),
            Need::ManualInvoice { n, amount: a } => (
                t.t("dash.n_invoice"),
                t.f("dash.v_invoice", &[("n", n), ("amount", &amount(*a))]),
            ),
            Need::Unknown { n, platform } => (
                t.t("dash.n_unknown"),
                t.f(
                    "dash.v_unknown",
                    &[("n", n), ("name", &platform_name(*platform))],
                ),
            ),
            Need::AutoPause { n } => (
                t.t("dash.n_autopause"),
                t.f("dash.v_autopause", &[("n", n)]),
            ),
            Need::Unreadable { platform } => (
                t.f("dash.n_unreadable", &[("name", &platform_name(*platform))]),
                t.t("dash.v_unreadable"),
            ),
            Need::Invest { n } => (t.t("dash.n_invest"), t.f("dash.v_invest", &[("n", n)])),
        }
    }

    fn kpi_cells(&self, t: &Texts) -> Vec<(String, String, String)> {
        let k = &self.kpis;
        let fmt_delta = |d: Option<i64>| match d {
            Some(d) if d < 0 => t.f("dash.down", &[("pct", &pct(-d))]),
            Some(d) => t.f("dash.up", &[("pct", &pct(d))]),
            None => String::new(),
        };
        let prev = k.prev;
        vec![
            (
                t.t("dash.k_completed"),
                k.completed.to_string(),
                fmt_delta(
                    prev.and_then(|(c, _, _)| delta(Decimal::from(k.completed), Decimal::from(c))),
                ),
            ),
            (
                t.t("dash.k_turnover"),
                ui::fmt::grouped(k.turnover),
                fmt_delta(prev.and_then(|(_, tv, _)| delta(k.turnover, tv))),
            ),
            (
                t.t("dash.k_profit"),
                amount(k.profit),
                fmt_delta(prev.and_then(|(_, _, p)| delta(k.profit, p))),
            ),
        ]
    }

    fn footer(&self, t: &Texts, state: DashState) -> String {
        let dot = match state {
            DashState::Normal => "{e:live}",
            DashState::Fire => "{e:dot_act}",
            DashState::Calm => "{e:dot_ok}",
        };
        let mut parts = vec![t.f(
            "dash.f_accounts",
            &[("up", &self.accounts.0), ("total", &self.accounts.1)],
        )];
        parts.push(match self.reconciled_at {
            Some(at) if self.reconcile_bad => t.f("dash.f_reconcile_bad", &[("time", &hhmm(at))]),
            Some(at) => t.f("dash.f_reconcile", &[("time", &hhmm(at))]),
            None => t.t("dash.f_no_reconcile"),
        });
        if state == DashState::Normal {
            parts.push(t.t("dash.f_payouts"));
        }
        let compared = self.kpi_cells(t).iter().any(|(_, _, d)| !d.is_empty());
        if state != DashState::Fire && compared {
            parts.push(t.t("dash.f_vs"));
        }
        format!("{dot} {}", parts.join(" · "))
    }

    fn buttons(&self, t: &Texts, state: DashState) -> Vec<botapi::InlineKeyboardButton> {
        use botapi::ButtonStyle::{Danger, Primary};
        let mut out = Vec::new();
        if state == DashState::Calm {
            out.push(btn(t.t("dash.b_stats"), &Cb::Stats(1)));
        }
        let mut needs = self.needs.clone();
        needs.sort_by_key(Need::rank);
        let mut shortfall = false;
        for n in &needs {
            match n {
                Need::Shortfall { .. } if !shortfall => {
                    shortfall = true;
                    out.push(btn(t.t("dash.b_shortfall"), &Cb::Reconcile).style(Danger));
                }
                Need::Review { n, .. } => {
                    out.push(btn(t.f("dash.b_review", &[("n", n)]), &Cb::Queue).style(Primary));
                }
                Need::Session { label, .. } => out.push(
                    btn(t.f("dash.b_login", &[("label", &esc(label))]), &Cb::Bots).style(Primary),
                ),
                Need::ManualInvoice { n, .. } => {
                    out.push(btn(t.f("dash.b_invoices", &[("n", n)]), &Cb::Queue).style(Primary))
                }
                Need::Unknown { n, .. } => {
                    out.push(btn(t.f("dash.b_unknown", &[("n", n)]), &Cb::Dirs));
                }
                Need::AutoPause { n } => {
                    out.push(btn(t.f("dash.b_autopause", &[("n", n)]), &Cb::Dirs));
                }
                Need::Invest { n } => {
                    out.push(btn(t.f("dash.b_invest", &[("n", n)]), &Cb::Investors));
                }
                Need::Shortfall { .. } | Need::Unreadable { .. } => {}
            }
        }
        out.push(btn(t.t("dash.b_reserves"), &Cb::Reconcile));
        if state != DashState::Fire {
            out.push(btn(t.t("dash.b_accounts"), &Cb::Bots));
        }
        out.push(btn(t.t("dash.b_menu"), &Cb::Menu));
        out
    }

    /// Карточка главного экрана (rich и запасной HTML) с кнопками. Значки — токенами
    /// `{e:…}` / `{m:…}`: их меняет [`crate::Look`] при отправке.
    pub fn screen(&self, t: &Texts) -> Screen {
        let state = self.state();
        let mut needs = self.needs.clone();
        needs.sort_by_key(Need::rank);
        let total: i64 = needs.iter().map(Need::count).sum();
        let title = if state == DashState::Calm {
            t.t("dash.title_calm")
        } else {
            t.f("dash.title_need", &[("n", &total)])
        };
        let mut rich = format!("<h3>{title}</h3>");
        let mut plain = format!("<b>{title}</b>\n");

        // Пожар: что горит — первой строкой.
        let mut heads = Vec::new();
        if self.kill_switch {
            heads.push(t.t("dash.kill_switch"));
        }
        for (p, since) in &self.paused {
            let since = since
                .map(|at| t.f("dash.since", &[("time", &hhmm(at))]))
                .unwrap_or_default();
            heads.push(t.f(
                "dash.paused_payouts",
                &[("name", &platform_name(*p)), ("since", &since)],
            ));
        }
        for h in &heads {
            rich.push_str(&format!("<p>{h}</p>"));
            plain.push_str(&format!("{h}\n"));
        }

        if state == DashState::Calm {
            let calm = t.t("dash.calm");
            rich.push_str(&format!("<p>{calm}</p>"));
            plain.push_str(&format!("{calm}\n"));
        } else if !needs.is_empty() {
            rich.push_str("<table compact>");
            let mut lines = Vec::new();
            for n in &needs {
                let (label, value) = self.need_label(t, n);
                rich.push_str(&format!(
                    "<tr><td>{}</td><td>{label}</td><td align=\"right\">{value}</td></tr>",
                    n.dot()
                ));
                lines.push(t.f(
                    "dash.row",
                    &[("dot", &n.dot()), ("label", &label), ("value", &value)],
                ));
            }
            rich.push_str("</table><hr/>");
            plain.push_str(&format!("<blockquote>{}</blockquote>\n", lines.join("\n")));
        }

        // Цифры дня — кроме пожара.
        if state != DashState::Fire {
            let cells = self.kpi_cells(t);
            rich.push_str("<table bordered><tr>");
            for (label, value, d) in &cells {
                let d = if d.is_empty() {
                    String::new()
                } else {
                    format!(" {d}")
                };
                rich.push_str(&format!("<td>{label}<br/><b>{value}</b>{d}</td>"));
            }
            rich.push_str("</tr></table>");
            let line: Vec<String> = cells
                .iter()
                .map(|(label, value, d)| {
                    t.f(
                        "dash.kpi_plain",
                        &[("label", label), ("value", value), ("delta", d)],
                    )
                    .trim_end()
                    .to_owned()
                })
                .collect();
            plain.push_str(&format!("{}\n", line.join(" · ")));
        }

        // Резерв: обычно — «Доступно · от цели», в пожар — «Леджер · Реально».
        if !self.reserves.is_empty() {
            let fire = state == DashState::Fire;
            let (c2, c3) = if fire {
                (t.t("dash.c_ledger"), t.t("dash.c_actual"))
            } else {
                (t.t("dash.c_available"), t.t("dash.c_target"))
            };
            rich.push_str(&format!(
                "<table striped compact><tr><th>{}</th><th align=\"right\">{c2}</th><th align=\"right\">{c3}</th></tr>",
                t.t("dash.c_reserve")
            ));
            let mut lines = Vec::new();
            for r in &self.reserves {
                let name = format!(
                    "{} {}",
                    platform_icon(r.platform),
                    platform_name(r.platform)
                );
                if fire {
                    let actual = match r.actual {
                        Some(a) if a < r.ledger => format!("{{e:dot_act}} {}", amount(a)),
                        Some(a) => amount(a),
                        None => t.t("dash.no_target"),
                    };
                    rich.push_str(&format!(
                        "<tr><td>{name}</td><td align=\"right\">{}</td><td align=\"right\">{actual}</td></tr>",
                        amount(r.ledger)
                    ));
                    lines.push(t.f(
                        "dash.fire_plain",
                        &[
                            ("icon", &platform_icon(r.platform)),
                            ("name", &platform_name(r.platform)),
                            ("ledger", &amount(r.ledger)),
                            ("actual", &actual),
                        ],
                    ));
                } else {
                    let share = r
                        .thresholds
                        .as_ref()
                        .and_then(|th| share(r.available, th.target));
                    let target = share.map_or_else(
                        || t.t("dash.no_target"),
                        |s| format!("{} {}", reserve_dot(r), pct(s)),
                    );
                    rich.push_str(&format!(
                        "<tr><td>{name}</td><td align=\"right\">{}</td><td align=\"right\">{target}</td></tr>",
                        amount(r.available)
                    ));
                    lines.push(match share {
                        Some(s) => t.f(
                            "dash.reserve_plain",
                            &[
                                ("icon", &platform_icon(r.platform)),
                                ("name", &platform_name(r.platform)),
                                ("available", &amount(r.available)),
                                ("dot", &reserve_dot(r)),
                                ("pct", &pct(s)),
                            ],
                        ),
                        None => t.f(
                            "dash.reserve_plain_no_target",
                            &[
                                ("icon", &platform_icon(r.platform)),
                                ("name", &platform_name(r.platform)),
                                ("available", &amount(r.available)),
                            ],
                        ),
                    });
                }
            }
            rich.push_str("</table>");
            plain.push_str(&format!("<blockquote>{}</blockquote>\n", lines.join("\n")));
        }

        let footer = self.footer(t, state);
        rich.push_str(&format!("<footer>{footer}</footer>"));
        plain.push_str(&format!("<i>{footer}</i>"));

        let mut screen = Screen::card(rich, plain);
        let buttons = self.buttons(t, state);
        for pair in buttons.chunks(2) {
            screen = screen.row(pair.to_vec());
        }
        screen
    }
}

/// Москва, 5 ноября 2026 (как в макете).
fn msk(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 5, h, m, 0)
        .single()
        .unwrap_or_default()
        - Duration::hours(3)
}

/// Три состояния главного экрана на данных макета: обычный день, пожар, тишина.
pub fn sample_dash() -> [(&'static str, DashData); 3] {
    let th = Thresholds::from_target(Decimal::from(1500));
    let reserve = |platform, available: i64, actual: Option<i64>| Reserve {
        platform,
        available: Decimal::from(available),
        ledger: Decimal::from(available),
        actual: actual.map(Decimal::from),
        thresholds: Some(th),
    };
    let kpis = Kpis {
        completed: 148,
        turnover: Decimal::from(12_480),
        profit: Decimal::new(29_140, 2),
        prev: Some((136, Decimal::from(11_886), Decimal::from(275))),
    };
    let ok = DashData {
        kill_switch: false,
        paused: Vec::new(),
        needs: vec![
            Need::Review {
                n: 2,
                oldest_mins: 6,
            },
            Need::ManualInvoice {
                n: 1,
                amount: Decimal::from(12),
            },
            Need::Unknown {
                n: 1,
                platform: Platform::XRocket,
            },
        ],
        kpis: kpis.clone(),
        reserves: vec![
            reserve(Platform::CryptoBot, 2140, None),
            reserve(Platform::XRocket, 640, None),
        ],
        accounts: (2, 2),
        reconciled_at: Some(msk(14, 0)),
        reconcile_bad: false,
    };
    let fire = DashData {
        kill_switch: false,
        paused: vec![(Platform::XRocket, Some(msk(14, 15)))],
        needs: vec![
            Need::Shortfall {
                platform: Platform::XRocket,
                amount: Decimal::from(12),
            },
            Need::Review {
                n: 4,
                oldest_mins: 18,
            },
            Need::Session {
                label: "ub-2".into(),
                banned: false,
            },
        ],
        kpis: kpis.clone(),
        reserves: vec![
            reserve(Platform::CryptoBot, 2140, Some(2140)),
            reserve(Platform::XRocket, 640, Some(628)),
        ],
        accounts: (1, 2),
        reconciled_at: Some(msk(14, 15)),
        reconcile_bad: true,
    };
    let calm = DashData {
        kill_switch: false,
        paused: Vec::new(),
        needs: Vec::new(),
        kpis,
        reserves: vec![
            reserve(Platform::CryptoBot, 1480, None),
            reserve(Platform::XRocket, 1300, None),
        ],
        accounts: (2, 2),
        reconciled_at: Some(msk(14, 0)),
        reconcile_bad: false,
    };
    [("Обычный день", ok), ("Пожар", fire), ("Тишина", calm)]
}

impl Inner {
    /// Данные главного экрана из базы.
    pub(crate) async fn dash_data(&self) -> Result<DashData, OpsError> {
        let mut conn = self.pool.acquire().await?;
        let now = Utc::now();
        let mut needs = Vec::new();

        // Сверка: недостача по открытым событиям «не сходится» с паузой выплат.
        let mut paused = Vec::new();
        let alerts = storage::alerts::open(&mut conn, 500).await?;
        for p in [Platform::CryptoBot, Platform::XRocket] {
            let key = engine::reconcile::mismatch_key(p);
            if let Some(a) = alerts.iter().find(|a| a.dedup_key == key)
                && a.payload["paused"].as_bool() == Some(true)
            {
                let diff: Decimal = a.payload["diff"]
                    .as_str()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or_default();
                needs.push(Need::Shortfall {
                    platform: p,
                    amount: -diff,
                });
                paused.push((p, Some(a.first_at)));
            }
            let key = engine::reconcile::unreadable_key(p);
            if alerts.iter().any(|a| a.dedup_key == key) {
                needs.push(Need::Unreadable { platform: p });
            }
        }
        let kill_switch =
            storage::settings::flag(&mut conn, crate::cb::Flag::KillSwitch.key(), false).await?;

        // Ручной разбор.
        let queue = storage::admin::review_queue(&mut conn).await?;
        if let Some(oldest) = queue.iter().map(|q| q.since).min() {
            needs.push(Need::Review {
                n: i64::try_from(queue.len()).unwrap_or(i64::MAX),
                oldest_mins: (now - oldest).num_minutes().max(0),
            });
        }

        // Аккаунты юзерботов.
        let bots = storage::admin::userbots(&mut conn).await?;
        let up = bots
            .iter()
            .filter(|b| matches!(b.status.as_str(), "active" | "flood_wait"))
            .count();
        for b in &bots {
            if matches!(b.status.as_str(), "logged_out" | "banned") {
                needs.push(Need::Session {
                    label: b.label.clone(),
                    banned: b.status == "banned",
                });
            }
        }

        // Автопаузы направлений: незнакомый ответ — отдельной строкой; паузы из-за недостачи
        // уже наверху.
        let short: Vec<Platform> = paused.iter().map(|(p, _)| *p).collect();
        let mut unknown: Vec<Platform> = Vec::new();
        let mut other = 0;
        for d in crate::views::DIRECTIONS {
            let row = storage::directions::get(&mut conn, d).await?;
            if !row.auto_paused {
                continue;
            }
            if row.pause_reason.as_deref() == Some(engine::UNKNOWN_REPLY_PAUSE) {
                if !unknown.contains(&d.source()) {
                    unknown.push(d.source());
                }
            } else if !short.contains(&d.target()) {
                other += 1;
            }
        }
        for platform in unknown {
            needs.push(Need::Unknown { n: 1, platform });
        }
        if other > 0 {
            needs.push(Need::AutoPause { n: other });
        }

        // Инвесторы.
        let invest = storage::invest::totals(&mut conn).await?;
        let waiting = invest.requested + invest.manual;
        if waiting > 0 {
            needs.push(Need::Invest { n: waiting });
        }

        // Цифры дня: сегодня с полуночи и вчера к этому же часу.
        let tz = self.cfg.tz.as_str();
        let (_, midnight) = storage::admin::local_midnight(&mut conn, tz).await?;
        let sum = |pts: &[storage::admin::DayPoint]| {
            pts.iter()
                .fold((0, Decimal::ZERO, Decimal::ZERO), |(c, tv, f), p| {
                    (c + p.completed, tv + p.turnover, f + p.fees)
                })
        };
        let today = storage::admin::daily_stats(&mut conn, midnight, now, tz, "day").await?;
        let yesterday = storage::admin::daily_stats(
            &mut conn,
            midnight - Duration::days(1),
            now - Duration::days(1),
            tz,
            "day",
        )
        .await?;
        let (completed, turnover, profit) = sum(&today);
        let prev = sum(&yesterday);
        let kpis = Kpis {
            completed,
            turnover,
            profit,
            prev: Some(prev),
        };

        // Резерв и последняя сверка.
        let wallets = storage::wallets::list(&mut conn).await?;
        let balances = storage::ledger::wallet_balances(&mut conn).await?;
        let recon = storage::admin::last_reconciliations(&mut conn).await?;
        let mut reserves = Vec::new();
        for w in wallets.iter().filter(|w| w.is_active) {
            let Some(b) = balances
                .iter()
                .find(|b| b.wallet_account_id == w.wallet_id && b.asset == "USDT")
            else {
                continue;
            };
            let thresholds =
                storage::admin::thresholds(&mut conn, w.wallet_id, Asset::Usdt).await?;
            reserves.push(Reserve {
                platform: w.platform,
                available: b.available,
                ledger: b.ledger_balance,
                actual: recon
                    .iter()
                    .find(|r| r.wallet_account_id == w.wallet_id)
                    .map(|r| r.actual),
                thresholds,
            });
        }
        let reconciled_at = recon.iter().map(|r| r.at).max();
        let reconcile_bad = recon.iter().any(|r| r.verdict != "ok");

        Ok(DashData {
            kill_switch,
            paused,
            needs,
            kpis,
            reserves,
            accounts: (up, bots.len()),
            reconciled_at,
            reconcile_bad,
        })
    }

    /// Главный экран.
    pub(crate) async fn dash(&self) -> Result<Screen, OpsError> {
        Ok(self.dash_data().await?.screen(&self.texts))
    }

    /// Запомнить сообщение главного экрана в чате (одно на чат — последнее).
    pub(crate) async fn remember_dash(&self, chat: i64, message_id: i32) -> Result<(), OpsError> {
        let mut conn = self.pool.acquire().await?;
        let mut list = dash_messages(&mut conn).await?;
        list.retain(|(c, _)| *c != chat);
        list.push((chat, message_id));
        storage::settings::set(&mut conn, DASH_MESSAGES, &serde_json::json!(list), None).await?;
        Ok(())
    }

    /// Поправить все главные экраны (раз в минуту и после нового события). Сообщение удалено
    /// или слишком старое для правки — забыть его; нового не присылать.
    pub(crate) async fn refresh_dashes(&self) -> Result<(), OpsError> {
        let list = {
            let mut conn = self.pool.acquire().await?;
            dash_messages(&mut conn).await?
        };
        if list.is_empty() {
            return Ok(());
        }
        let screen = self.look().screen(&self.dash().await?);
        let mut alive = Vec::new();
        for (chat, message_id) in &list {
            let rich = screen.rich.clone().unwrap_or_default();
            match self
                .api
                .edit_message_rich(*chat, *message_id, rich, screen.markup())
                .await
            {
                Ok(_) => alive.push((*chat, *message_id)),
                Err(e) if e.is_not_modified() => alive.push((*chat, *message_id)),
                Err(e) if e.is_message_gone() || e.is_rejection() => {
                    tracing::info!(chat, error = %e, "dash message is gone, forgetting it");
                }
                Err(e) => {
                    tracing::warn!(chat, error = %e, "dash refresh failed");
                    alive.push((*chat, *message_id));
                }
            }
        }
        if alive.len() != list.len() {
            let mut conn = self.pool.acquire().await?;
            storage::settings::set(&mut conn, DASH_MESSAGES, &serde_json::json!(alive), None)
                .await?;
        }
        Ok(())
    }
}

async fn dash_messages(conn: &mut sqlx::PgConnection) -> Result<Vec<(i64, i32)>, OpsError> {
    Ok(storage::settings::get(conn, DASH_MESSAGES)
        .await?
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_states_from_the_mockup() {
        let t = Texts::builtin().unwrap();
        let [(_, ok), (_, fire), (_, calm)] = sample_dash();
        assert_eq!(ok.state(), DashState::Normal);
        assert_eq!(fire.state(), DashState::Fire);
        assert_eq!(calm.state(), DashState::Calm);
        let s = ok.screen(&t);
        let rich = s.rich.unwrap();
        assert!(rich.contains("Нужно решить · 4"), "{rich}");
        assert!(rich.contains("2\u{a0}140.00"), "{rich}");
        assert!(
            rich.contains("143\u{a0}%") && rich.contains("43\u{a0}%"),
            "{rich}"
        );
        assert!(
            rich.contains("▲ 9\u{a0}%") && rich.contains("▲ 6\u{a0}%"),
            "{rich}"
        );
        let f = fire.screen(&t).rich.unwrap();
        assert!(f.contains("Нужно решить · 6"), "{f}");
        assert!(f.contains("{e:dot_act} 628.00"), "{f}");
        assert!(!f.contains("Обменов"), "в пожар цифр дня нет: {f}");
        let c = calm.screen(&t).rich.unwrap();
        assert!(c.contains("Всё спокойно") && c.contains("99\u{a0}%"), "{c}");
    }

    #[test]
    fn amounts_and_shares() {
        assert_eq!(amount(Decimal::from(2140)), "2\u{a0}140.00");
        assert_eq!(amount(Decimal::new(29_140, 2)), "291.40");
        assert_eq!(share(Decimal::from(640), Decimal::from(1500)), Some(43));
        assert_eq!(delta(Decimal::from(148), Decimal::from(136)), Some(9));
        assert_eq!(delta(Decimal::ONE, Decimal::ZERO), None);
    }
}
