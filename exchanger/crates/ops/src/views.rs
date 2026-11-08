//! Экраны пульта и переходы по кнопкам. Изменений здесь нет: кнопка открывает экран, вопрос
//! ([`crate::actions::Input`]) или предпросмотр ([`crate::actions::Action`]).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use domain::fees::{FeeMode, LeverInput};
use domain::fsm::Flow;
use domain::{Asset, Decimal, Direction, Platform};
use engine::availability::{Availability, Blocker};
use engine::fees::{PromoMode, SwitchedBy};
use storage::directions::DirectionRow;
use storage::staff::{Role, StaffMember};
use ui::{Emoji, EmojiStyle};

use crate::actions::{Action, Input};
use crate::cb::{Cb, DecisionKind, DirField, Flag, TermField};
use crate::nav::{self, List};
use crate::texts::{code, esc};
use crate::{Inner, OpsError, Screen, btn};

pub(crate) const DIRECTIONS: [Direction; 4] = [
    Direction::CbToXrCheck,
    Direction::XrToCbCheck,
    Direction::PayCbInvoice,
    Direction::PayXrInvoice,
];

pub(crate) fn usdt(v: Decimal) -> String {
    format!("{} USDT", ui::fmt::amount(v, Asset::Usdt))
}

pub(crate) fn pct(v: Decimal) -> String {
    ui::fmt::percent(v)
}

pub(crate) fn when(at: DateTime<Utc>) -> String {
    at.format("%d.%m %H:%M").to_string()
}

/// `CQab…9F`: код чека или счёта в пульте не показываем целиком (CLAUDE.md п. 16).
pub(crate) fn mask(param: &str) -> String {
    let chars: Vec<char> = param.chars().collect();
    if chars.len() <= 8 {
        return param.to_owned();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 2..].iter().collect();
    format!("{head}…{tail}")
}

/// Чеки и счета в тексте ответа кошелька — маской.
pub(crate) fn mask_links(text: &str) -> String {
    text.split_inclusive(|c: char| c.is_whitespace())
        .map(|word| {
            let trimmed = word.trim_end();
            match trimmed.split_once("start=") {
                Some((head, param)) if param.len() > 8 => {
                    format!("{head}start={}{}", mask(param), &word[trimmed.len()..])
                }
                _ => word.to_owned(),
            }
        })
        .collect()
}

/// Что показывает меню: счётчики и флаги.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MenuCounts {
    /// Заявок на ручном разборе.
    pub review: usize,
    /// Обращений клиентов без ответа.
    pub inbox: usize,
    pub kill: bool,
    pub maint: bool,
}

/// Меню пульта для роли (макет «Меню пульта»). «Персонал» — только владельцу.
pub fn menu_screen(t: &crate::Texts, role: Role, c: &MenuCounts) -> Screen {
    use botapi::ButtonStyle::{Danger, Primary};
    let role_name = t.t(&format!("roles.{}", role.as_db_str())).to_lowercase();
    let mut text = t.f("menu.title", &[("role", &role_name)]);
    if c.kill {
        text.push_str(&format!("\n\n{}", t.t("menu.flags_kill")));
    }
    if c.maint {
        text.push_str(&format!("\n{}", t.t("menu.flags_maint")));
    }
    let counted = |key: &str, n: usize, cb: Cb| {
        if n > 0 {
            btn(t.f(&format!("{key}_n"), &[("n", &n)]), &cb).style(Primary)
        } else {
            btn(t.t(key), &cb)
        }
    };
    let mut items = vec![
        counted("menu.b_queue", c.review, Cb::Queue),
        btn(t.t("menu.b_orders"), &Cb::Page(List::Orders, 0)),
        btn(t.t("menu.b_reserves"), &Cb::Reserves),
        btn(t.t("menu.b_bots"), &Cb::Bots),
        btn(t.t("menu.b_dirs"), &Cb::Dirs),
        btn(t.t("menu.b_users"), &Cb::Page(List::Users, 0)),
        counted("menu.b_inbox", c.inbox, Cb::Inbox),
        btn(t.t("menu.b_broadcast"), &Cb::Broadcast),
        btn(t.t("menu.b_stats"), &Cb::Stats(7)),
        btn(t.t("menu.b_audit"), &Cb::Audit),
        btn(t.t("menu.b_settings"), &Cb::Settings),
    ];
    if role == Role::Owner {
        items.push(btn(t.t("menu.b_staff"), &Cb::Staff));
    }
    items.push(btn(t.t("menu.b_system"), &Cb::System));
    items.push(btn(t.t("menu.b_emoji"), &Cb::Emoji));
    let mut s = Screen::new(text);
    for pair in items.chunks(2) {
        s = s.row(pair.to_vec());
    }
    let kill_label = if c.kill {
        "menu.b_kill_off"
    } else {
        "menu.b_kill"
    };
    s.row(vec![
        btn(t.t(kill_label), &Cb::Toggle(Flag::KillSwitch)).style(Danger),
    ])
}

impl Inner {
    pub(crate) fn dir_name(&self, d: Direction) -> String {
        self.texts.t(&format!("names.{}", d.code()))
    }

    pub(crate) fn platform_name(&self, p: Platform) -> String {
        self.texts.t(&format!("names.{}", p.db_name()))
    }

    pub(crate) fn state_name(&self, s: &str) -> String {
        self.texts.t(&format!("states.{s}"))
    }

    fn back(&self, to: Cb) -> Vec<botapi::InlineKeyboardButton> {
        nav::back(&self.texts, &to)
    }

    // -----------------------------------------------------------------------------------------
    // Пульт

    /// Меню пульта — макет «Меню пульта»: 14 разделов по два в ряд и красный «Стоп-кран».
    /// Счётчик — только там, где ждут человека (разбор, обращения), такие кнопки синие.
    pub(crate) async fn menu(&self, staff: &StaffMember) -> Result<Screen, OpsError> {
        let mut conn = self.pool.acquire().await?;
        let counts = MenuCounts {
            review: storage::admin::review_queue(&mut conn).await?.len(),
            inbox: crate::inbox::open_questions(&mut conn).await?.len(),
            kill: storage::settings::flag(&mut conn, Flag::KillSwitch.key(), false).await?,
            maint: storage::settings::flag(&mut conn, Flag::Maintenance.key(), false).await?,
        };
        Ok(menu_screen(&self.texts, staff.role, &counts))
    }

    // -----------------------------------------------------------------------------------------
    // Сводка

    async fn lever(
        &self,
        conn: &mut sqlx::PgConnection,
        row: &DirectionRow,
    ) -> Result<Option<LeverInput>, OpsError> {
        let Some((wallet, _)) =
            storage::wallets::active(&mut *conn, row.direction.target()).await?
        else {
            return Ok(None);
        };
        let Some(target) =
            storage::directions::reserve_target(&mut *conn, wallet, Asset::Usdt).await?
        else {
            return Ok(None);
        };
        Ok(Some(LeverInput {
            k_pct: row.skew_k_pct,
            floor_pct: row.fee_floor_pct,
            cap_pct: row.fee_cap_pct,
            available: storage::holds::available(&mut *conn, wallet, Asset::Usdt).await?,
            target,
        }))
    }

    /// Комиссия направления сейчас (с рычагом): «2,5 %» или «ступени 2,5 % … 1,2 %».
    pub(crate) async fn fee_now(
        &self,
        conn: &mut sqlx::PgConnection,
        row: &DirectionRow,
    ) -> Result<String, OpsError> {
        let lever = self.lever(conn, row).await?;
        let lev =
            |base: Decimal| domain::fees::levered_pct(row.fee_pct, base, lever).unwrap_or(base);
        Ok(match (row.fee_mode, row.fee_tiers.as_ref()) {
            (FeeMode::Tiers, Some(s)) if s.tiers().len() > 1 => {
                let first = s.tiers().first().map_or(row.fee_pct, |x| x.fee_pct);
                let last = s.tiers().last().map_or(row.fee_pct, |x| x.fee_pct);
                format!("{} … {}", pct(lev(first)), pct(lev(last)))
            }
            _ => pct(lev(row.fee_pct)),
        })
    }

    /// Состояние направления с причиной: «работает · до 300.00 USDT» или
    /// «⏸ нет резерва: в xRocket свободно 0.00, нужно от 2.00» — то же, что видит приём чека.
    pub(crate) fn availability_text(&self, a: Option<&Availability>) -> String {
        let t = &self.texts;
        let Some(a) = a else {
            return t.t("status.unknown");
        };
        if a.accepting() {
            return match a.max_now {
                Some(max) => t.f("status.open_up_to", &[("max", &usdt(max))]),
                None => t.t("status.open"),
            };
        }
        a.blockers
            .iter()
            .map(|b| match b {
                Blocker::Closed => t.t("status.closed"),
                Blocker::AutoPaused { reason } => t.f(
                    "status.auto",
                    &[("reason", &esc(reason.as_deref().unwrap_or("")))],
                ),
                Blocker::Maintenance => t.t("status.maintenance"),
                Blocker::KillSwitch => t.t("status.kill"),
                Blocker::WalletMissing { platform } => t.f(
                    "status.wallet_missing",
                    &[("name", &self.platform_name(*platform))],
                ),
                Blocker::UserbotPaused { platform } => t.f(
                    "status.userbot_paused",
                    &[("name", &self.platform_name(*platform))],
                ),
                Blocker::NoReserve {
                    platform,
                    available,
                    needed,
                } => t.f(
                    "status.no_reserve",
                    &[
                        ("name", &self.platform_name(*platform)),
                        ("available", &usdt(*available)),
                        ("needed", &usdt(*needed)),
                    ],
                ),
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub(crate) fn promo_mode_name(&self, m: PromoMode) -> String {
        self.texts.t(match m {
            PromoMode::Off => "promo.mode_off",
            PromoMode::Hint => "promo.mode_hint",
            PromoMode::Auto => "promo.mode_auto",
        })
    }

    // -----------------------------------------------------------------------------------------
    // Направления

    pub(crate) async fn dirs(&self) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let mut lines = vec![t.t("dir.list_title"), String::new()];
        let mut s = Screen::default();
        let availability = self.engine.availability().await?;
        for d in DIRECTIONS {
            let row = storage::directions::get(&mut conn, d).await?;
            let fee = self.fee_now(&mut conn, &row).await?;
            let a = availability.iter().find(|a| a.direction == d);
            lines.push(t.f(
                "dir.line",
                &[
                    ("name", &self.link(&self.dir_name(d), &Cb::Dir(d))),
                    ("fee", &fee),
                    ("status", &self.availability_text(a)),
                ],
            ));
        }
        lines.push(String::new());
        lines.push(t.t("dir.hint"));
        s.text = lines.join("\n");
        // Встречный поток — про направления и резерв: вход отсюда (в меню макета его нет).
        Ok(s.row(vec![btn(t.t("menu.promo"), &Cb::Promo)])
            .row(nav::back(t, &Cb::Menu)))
    }

    pub(crate) fn tiers_text(&self, row: &DirectionRow) -> String {
        row.fee_tiers.as_ref().map_or_else(
            || self.texts.t("common.none"),
            |s| {
                s.tiers()
                    .iter()
                    .map(|x| format!("{} {}", x.from, x.fee_pct))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        )
    }

    pub(crate) async fn dir_card(
        &self,
        staff: &StaffMember,
        d: Direction,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let row = storage::directions::get(&mut conn, d).await?;
        let mut lines = vec![t.f("dir.title", &[("name", &self.dir_name(d))])];
        lines.push(if !row.is_open {
            t.t("dir.state_closed")
        } else if row.auto_paused {
            t.f(
                "dir.state_auto",
                &[("reason", &esc(row.pause_reason.as_deref().unwrap_or("")))],
            )
        } else {
            t.t("dir.state_open")
        });
        // Открыто, но не принимает (нет резерва, кошелёк не подключён…) — почему.
        if let Some(a) = self
            .engine
            .availability()
            .await?
            .into_iter()
            .find(|a| a.direction == d)
        {
            lines.push(t.f(
                "dir.state_now",
                &[("status", &self.availability_text(Some(&a)))],
            ));
        }
        let tiered = row.fee_mode == FeeMode::Tiers && row.fee_tiers.is_some();
        if tiered {
            lines.push(t.f("dir.fee_tiers", &[("min_fee", &usdt(row.min_fee))]));
            if let Some(s) = &row.fee_tiers {
                for x in s.tiers() {
                    lines.push(t.f(
                        "dir.tier",
                        &[("from", &usdt(x.from)), ("fee", &pct(x.fee_pct))],
                    ));
                }
            }
        } else {
            lines.push(t.f(
                "dir.fee_flat",
                &[("fee", &pct(row.fee_pct)), ("min_fee", &usdt(row.min_fee))],
            ));
            if let Some(s) = &row.fee_tiers {
                let list = s
                    .tiers()
                    .iter()
                    .map(|x| {
                        format!(
                            "{}: {}",
                            ui::fmt::amount(x.from, Asset::Usdt),
                            pct(x.fee_pct)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(t.f("dir.tiers_stored", &[("tiers", &list)]));
            }
        }
        lines.push(t.f(
            "dir.lever",
            &[
                ("k", &row.skew_k_pct.normalize()),
                ("floor", &pct(row.fee_floor_pct)),
                ("cap", &pct(row.fee_cap_pct)),
            ],
        ));
        lines.push(t.f(
            "dir.lever_now",
            &[("fee", &self.fee_now(&mut conn, &row).await?)],
        ));
        lines.push(t.f(
            "dir.limits",
            &[
                ("min", &usdt(row.min_amount)),
                ("max", &usdt(row.max_amount)),
                ("newcomer", &usdt(row.new_user_max_amount)),
            ],
        ));
        if d.flow() == Flow::InvoicePayment {
            lines.push(t.f("dir.fiat", &[("buffer", &pct(row.fiat_buffer_pct))]));
        }
        lines.push(t.f("dir.examples", &[("examples", &self.examples(&row))]));

        let mut s = Screen::new(lines.join("\n"));
        if staff.role >= Role::Admin {
            s = s
                .row(vec![
                    btn(t.t("dir.b_fee"), &Cb::DirEdit(d, DirField::Fee)),
                    btn(t.t("dir.b_min_fee"), &Cb::DirEdit(d, DirField::MinFee)),
                ])
                .row(vec![
                    btn(t.t("dir.b_tiers"), &Cb::DirEdit(d, DirField::Tiers)),
                    btn(
                        t.t(if tiered {
                            "dir.b_mode_to_flat"
                        } else {
                            "dir.b_mode_to_tiers"
                        }),
                        &Cb::DirMode(d),
                    ),
                ])
                .row(vec![
                    btn(t.t("dir.b_lever"), &Cb::DirEdit(d, DirField::Lever)),
                    btn(t.t("dir.b_limits"), &Cb::DirEdit(d, DirField::Limits)),
                ]);
            let mut row4 = vec![btn(
                t.t("dir.b_newcomer"),
                &Cb::DirEdit(d, DirField::Newcomer),
            )];
            if d.flow() == Flow::InvoicePayment {
                row4.push(btn(t.t("dir.b_fiat"), &Cb::DirEdit(d, DirField::Fiat)));
            }
            s = s.row(row4);
            let mut row5 = vec![btn(
                t.t(if row.is_open {
                    "dir.b_close"
                } else {
                    "dir.b_open"
                }),
                &Cb::DirOpen(d),
            )];
            if row.auto_paused {
                row5.push(btn(t.t("dir.b_clear_auto"), &Cb::DirClearAuto(d)));
            }
            s = s.row(row5);
        }
        Ok(s.row(self.back(Cb::Dirs)))
    }

    /// Примеры по базе направления (без рычага и выгодного курса): 10, 100, 300 USDT.
    pub(crate) fn examples(&self, row: &DirectionRow) -> String {
        let t = &self.texts;
        [Decimal::from(10), Decimal::from(100), Decimal::from(300)]
            .into_iter()
            .filter_map(|a| {
                let fee = domain::fees::decide(
                    a,
                    &domain::fees::FeeInputs {
                        direction_pct: row.fee_pct,
                        min_fee: row.min_fee,
                        mode: row.fee_mode,
                        schedule: row.fee_tiers.as_ref(),
                        personal_pct: None,
                        lever: None,
                        promo: None,
                    },
                )
                .ok()?;
                let step = domain::Step::from_dp(2)?;
                let result = match row.direction.flow() {
                    Flow::CheckExchange => {
                        domain::pricing::quote_check_exchange(a, fee.policy, step)
                            .ok()
                            .map(|q| usdt(q.payout))
                    }
                    Flow::InvoicePayment => {
                        domain::pricing::quote_invoice_payment(a, fee.policy, step)
                            .ok()
                            .map(|q| usdt(q.client_pays))
                    }
                }?;
                Some(t.f(
                    "dir.example",
                    &[
                        ("amount", &ui::fmt::amount(a, Asset::Usdt)),
                        ("payout", &result),
                    ],
                ))
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    // -----------------------------------------------------------------------------------------
    // Встречный поток

    pub(crate) async fn promo(&self) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let settings = engine::fees::promo_settings(&mut conn).await?;
        let states = engine::fees::promo_states(&mut conn).await?;
        let mut lines = vec![t.t("promo.title"), t.t("promo.explain"), String::new()];
        lines.push(t.f(
            "promo.mode",
            &[("mode", &self.promo_mode_name(settings.mode))],
        ));
        lines.push(t.f(
            "promo.rate",
            &[
                ("fee", &pct(settings.fee_pct)),
                ("min_fee", &usdt(settings.min_fee)),
            ],
        ));
        lines.push(t.f(
            "promo.thresholds",
            &[
                ("trigger", &pct(settings.trigger_pct)),
                ("release", &pct(settings.release_pct)),
            ],
        ));
        lines.push(String::new());
        lines.push(t.t("promo.reserves"));
        let mut targets = true;
        for p in Platform::ALL {
            match engine::fees::reserve_of(&mut conn, p, Asset::Usdt).await? {
                Some(r) => {
                    let state = match r.deficit_pct() {
                        Some(d) if d > Decimal::ZERO => {
                            t.f("promo.deficit", &[("pct", &pct(d.round_dp(1)))])
                        }
                        Some(d) if d < Decimal::ZERO => {
                            t.f("promo.surplus", &[("pct", &pct((-d).round_dp(1)))])
                        }
                        _ => t.t("promo.balanced"),
                    };
                    lines.push(t.f(
                        "promo.reserve",
                        &[
                            ("name", &self.platform_name(p)),
                            ("available", &usdt(r.available)),
                            ("target", &usdt(r.target)),
                            ("state", &state),
                        ],
                    ));
                }
                None => {
                    targets = false;
                    let wallet = storage::wallets::active(&mut conn, p).await?;
                    let available = match wallet {
                        Some((w, _)) => {
                            storage::holds::available(&mut conn, w, Asset::Usdt).await?
                        }
                        None => Decimal::ZERO,
                    };
                    lines.push(t.f(
                        "promo.reserve_no_target",
                        &[
                            ("name", &self.platform_name(p)),
                            ("available", &usdt(available)),
                        ],
                    ));
                }
            }
        }
        if !targets {
            lines.push(t.t("promo.no_targets"));
        }
        lines.push(String::new());
        lines.push(t.t("promo.dirs"));
        let mut switches = Vec::new();
        for d in DIRECTIONS {
            let (_, _, room) = engine::fees::direction_reserves(
                &mut conn,
                d,
                Asset::Usdt,
                Decimal::ZERO,
                Decimal::ZERO,
            )
            .await?;
            let st = states.get(d.code());
            let on = st.is_some_and(|s| s.on) && settings.mode != PromoMode::Off;
            let state = if on {
                let by = match st.map(|s| s.by) {
                    Some(SwitchedBy::Manual) => t.t("promo.by_manual"),
                    _ => t.t("promo.by_auto"),
                };
                t.f("promo.dir_on", &[("by", &by)])
            } else {
                t.t("promo.dir_off")
            };
            lines.push(t.f(
                "promo.dir",
                &[
                    ("name", &self.dir_name(d)),
                    ("state", &state),
                    ("room", &usdt(room)),
                ],
            ));
            let short = self.dir_name(d);
            switches.push(if on {
                btn(
                    t.f("promo.b_off_dir", &[("name", &short)]),
                    &Cb::PromoSwitch(d, false),
                )
            } else {
                btn(
                    t.f("promo.b_on_dir", &[("name", &short)]),
                    &Cb::PromoSwitch(d, true),
                )
            });
        }
        let mark = |m: PromoMode, key: &str| {
            let label = t.t(key);
            if settings.mode == m {
                format!("✓ {label}")
            } else {
                label
            }
        };
        let mut s = Screen::new(lines.join("\n"))
            .row(vec![
                btn(
                    mark(PromoMode::Off, "promo.b_off"),
                    &Cb::PromoMode(PromoMode::Off),
                ),
                btn(
                    mark(PromoMode::Hint, "promo.b_hint"),
                    &Cb::PromoMode(PromoMode::Hint),
                ),
                btn(
                    mark(PromoMode::Auto, "promo.b_auto"),
                    &Cb::PromoMode(PromoMode::Auto),
                ),
            ])
            .row(vec![
                btn(t.t("promo.b_rate"), &Cb::PromoRate),
                btn(t.t("promo.b_thresholds"), &Cb::PromoThresholds),
            ])
            .row(
                Platform::ALL
                    .iter()
                    .map(|p| {
                        btn(
                            t.f("promo.b_target", &[("name", &self.platform_name(*p))]),
                            &Cb::PromoTarget(*p),
                        )
                    })
                    .collect(),
            );
        for pair in switches.chunks(2) {
            s = s.row(pair.to_vec());
        }
        Ok(s.row(vec![btn(t.t("nav.refresh"), &Cb::Promo)])
            .row(nav::back(t, &Cb::Dirs)))
    }

    // -----------------------------------------------------------------------------------------
    // Разбор

    pub(crate) async fn find_order(&self, query: &str) -> Result<Screen, OpsError> {
        let id = {
            let mut conn = self.pool.acquire().await?;
            storage::admin::find_order(&mut conn, query).await?
        };
        match id {
            Some(id) => self.order(None, id).await,
            None => Ok(Screen::new(self.texts.t("order.not_found"))
                .row(vec![btn(self.texts.t("common.home"), &Cb::Menu)])),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Клиенты

    pub(crate) async fn find_user(&self, query: &str) -> Result<Screen, OpsError> {
        let id = {
            let mut conn = self.pool.acquire().await?;
            storage::admin::find_user(&mut conn, query).await?
        };
        match id {
            Some(id) => self.user(None, id).await,
            None => Ok(Screen::new(self.texts.t("user.not_found"))
                .row(vec![btn(self.texts.t("common.home"), &Cb::Menu)])),
        }
    }

    pub(crate) fn terms_line(&self, terms: &storage::user_terms::UserTerms) -> String {
        let t = &self.texts;
        let mut parts = Vec::new();
        if let Some(v) = terms.fee_pct {
            parts.push(t.f("user.t_fee", &[("v", &pct(v))]));
        }
        if let Some(v) = terms.min_fee {
            parts.push(t.f("user.t_min_fee", &[("v", &usdt(v))]));
        }
        if let Some(v) = terms.min_amount {
            parts.push(t.f("user.t_min", &[("v", &usdt(v))]));
        }
        if let Some(v) = terms.max_amount {
            parts.push(t.f("user.t_max", &[("v", &usdt(v))]));
        }
        if terms.newcomer_exempt {
            parts.push(t.t("user.t_exempt"));
        }
        if parts.is_empty() {
            t.t("user.default")
        } else {
            parts.join(", ")
        }
    }

    pub(crate) async fn user(
        &self,
        staff: Option<&StaffMember>,
        id: i64,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let card = match storage::admin::user_card(&mut conn, id).await {
            Ok(c) => c,
            Err(storage::StorageError::NotFound(_)) => {
                return Ok(Screen::new(t.t("user.not_found"))
                    .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
            }
            Err(e) => return Err(e.into()),
        };
        let name = match (&card.username, &card.first_name) {
            (Some(u), _) => format!(" · @{}", esc(u)),
            (None, Some(n)) => format!(" · {}", esc(n)),
            _ => String::new(),
        };
        let mut lines = vec![
            t.f("user.title", &[("id", &card.tg_user_id), ("name", &name)]),
            t.f(
                "user.since",
                &[
                    ("date", &when(card.created_at)),
                    ("seen", &when(card.last_seen_at)),
                ],
            ),
            t.f(
                "user.stats",
                &[
                    ("completed", &card.completed),
                    ("refunded", &card.refunded),
                    ("review", &card.in_review),
                    ("live", &card.live),
                ],
            ),
            t.f(
                "user.turnover",
                &[
                    ("turnover", &usdt(card.turnover)),
                    ("fees", &usdt(card.fees)),
                ],
            ),
        ];
        if let Some(ban) = &card.ban {
            let until = ban.expires_at.map_or_else(|| t.t("user.ban_forever"), when);
            lines.push(t.f(
                "user.ban",
                &[("until", &until), ("reason", &esc(&ban.reason))],
            ));
        }
        if let Some(note) = &card.risk_note {
            lines.push(t.f("user.note", &[("note", &esc(note))]));
        }
        lines.push(
            t.f(
                "user.daily",
                &[(
                    "limit",
                    &card
                        .daily_limit_usd
                        .map_or_else(|| t.t("user.daily_default"), usdt),
                )],
            ),
        );
        lines.push(String::new());
        lines.push(t.t("user.terms"));
        let terms = storage::user_terms::for_user(&mut conn, id).await?;
        let personal: Vec<_> = terms.iter().filter(|x| x.is_personal()).collect();
        if personal.is_empty() {
            lines.push(t.t("user.terms_none"));
        }
        for x in personal {
            lines.push(t.f(
                "user.term",
                &[
                    ("direction", &self.dir_name(x.direction)),
                    ("list", &self.terms_line(x)),
                ],
            ));
        }
        let mut s = Screen::new(lines.join("\n"));
        if staff.is_none_or(|m| m.role >= Role::Admin) {
            s = s
                .row(vec![
                    btn(t.t("user.b_terms"), &Cb::UserTerms(id)),
                    btn(t.t("user.b_daily"), &Cb::UserDaily(id)),
                ])
                .row(vec![
                    if card.ban.is_some() {
                        btn(t.t("user.b_unban"), &Cb::UserUnban(id))
                    } else {
                        btn(t.t("user.b_ban"), &Cb::UserBan(id))
                    },
                    btn(t.t("user.b_note"), &Cb::UserNote(id)),
                ])
                .row(vec![btn(t.t("user.b_invest"), &Cb::Invest(id))]);
        }
        // Заявки клиента: последние пять ссылками, все — по страницам.
        let (recent, total) = self.user_recent(id).await?;
        if total > 0 {
            s.text.push_str(&format!(
                "\n\n{}\n{recent}",
                t.f("user.orders_title", &[("n", &total)])
            ));
            s = s.row(vec![btn(
                t.f("user.b_orders", &[("n", &total)]),
                &Cb::UserOrders(id, 0),
            )]);
        }
        let back = staff.map_or(Cb::Page(List::Users, 0), |m| {
            self.list_or(m.tg_user_id, Cb::Page(List::Users, 0))
        });
        // Из заявок самого клиента — назад в клиентов, а не по кругу.
        let back = if matches!(back, Cb::UserOrders(u, _) if u == id) {
            Cb::Page(List::Users, 0)
        } else {
            back
        };
        Ok(s.row(vec![btn(t.t("nav.refresh"), &Cb::User(id))])
            .row(nav::back(t, &back)))
    }

    pub(crate) async fn user_terms(&self, id: i64) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut s = Screen::new(t.f("user.dirs_title", &[("id", &id)]));
        for pair in DIRECTIONS.chunks(2) {
            s = s.row(
                pair.iter()
                    .map(|d| btn(self.dir_name(*d), &Cb::UserDir(id, *d)))
                    .collect(),
            );
        }
        Ok(s.row(self.back(Cb::User(id))))
    }

    pub(crate) async fn user_dir(&self, id: i64, d: Direction) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let row = storage::directions::get(&mut conn, d).await?;
        let terms = storage::user_terms::get(&mut conn, id, d).await?;
        let common = t.f(
            "user.common",
            &[
                ("fee", &self.fee_now(&mut conn, &row).await?),
                ("min", &usdt(row.min_amount)),
                ("max", &usdt(row.max_amount)),
            ],
        );
        let s = Screen::new(t.f(
            "user.dir_title",
            &[
                ("id", &id),
                ("direction", &self.dir_name(d)),
                ("common", &common),
                ("personal", &self.terms_line(&terms)),
            ],
        ))
        .row(vec![
            btn(t.t("user.b_fee"), &Cb::UserEdit(id, d, TermField::Fee)),
            btn(
                t.t("user.b_min_fee"),
                &Cb::UserEdit(id, d, TermField::MinFee),
            ),
        ])
        .row(vec![
            btn(t.t("user.b_min"), &Cb::UserEdit(id, d, TermField::Min)),
            btn(t.t("user.b_max"), &Cb::UserEdit(id, d, TermField::Max)),
        ])
        .row(vec![
            btn(
                t.t(if terms.newcomer_exempt {
                    "user.b_exempt_off"
                } else {
                    "user.b_exempt_on"
                }),
                &Cb::UserExempt(id, d),
            ),
            btn(t.t("user.b_reset"), &Cb::UserReset(id, d)),
        ]);
        Ok(s.row(self.back(Cb::UserTerms(id))))
    }

    // -----------------------------------------------------------------------------------------
    // Настройки

    pub(crate) fn onoff(&self, on: bool) -> String {
        self.texts.t(if on { "common.on" } else { "common.off" })
    }

    pub(crate) fn style_name(&self, s: EmojiStyle) -> String {
        self.texts.t(match s {
            EmojiStyle::Animated => "settings.style_animated",
            EmojiStyle::Static => "settings.style_static",
            EmojiStyle::Off => "settings.style_off",
        })
    }

    pub(crate) async fn emoji_settings(
        &self,
        conn: &mut sqlx::PgConnection,
    ) -> Result<(EmojiStyle, serde_json::Map<String, serde_json::Value>), OpsError> {
        let style = storage::settings::get(&mut *conn, "emoji_style")
            .await?
            .and_then(|v| v.as_str().and_then(EmojiStyle::from_key))
            .unwrap_or_default();
        let custom = storage::settings::get(&mut *conn, "custom_emoji")
            .await?
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        Ok((style, custom))
    }

    pub(crate) async fn settings(&self) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let mut flags = Vec::new();
        for f in [Flag::Newcomer, Flag::Maintenance, Flag::KillSwitch] {
            flags.push(storage::settings::flag(&mut conn, f.key(), f.default_on()).await?);
        }
        let (style, custom) = self.emoji_settings(&mut conn).await?;
        let text = [
            t.t("settings.title"),
            String::new(),
            t.f("settings.newcomer", &[("state", &self.onoff(flags[0]))]),
            t.f("settings.maint", &[("state", &self.onoff(flags[1]))]),
            t.f("settings.kill", &[("state", &self.onoff(flags[2]))]),
            t.f(
                "settings.emoji",
                &[("style", &self.style_name(style)), ("n", &custom.len())],
            ),
        ]
        .join("\n");
        Ok(Screen::new(text)
            .row(vec![
                btn(
                    t.f("settings.b_newcomer", &[("state", &self.onoff(flags[0]))]),
                    &Cb::Toggle(Flag::Newcomer),
                ),
                btn(
                    t.f("settings.b_maint", &[("state", &self.onoff(flags[1]))]),
                    &Cb::Toggle(Flag::Maintenance),
                ),
            ])
            .row(vec![
                btn(
                    t.t(if flags[2] {
                        "settings.b_kill_off"
                    } else {
                        "settings.b_kill_on"
                    }),
                    &Cb::Toggle(Flag::KillSwitch),
                ),
                btn(t.t("settings.b_emoji"), &Cb::Emoji),
            ])
            .row(nav::back(t, &Cb::Menu)))
    }

    pub(crate) fn role_title(&self, r: Emoji) -> String {
        self.texts.t(&format!("emoji_roles.{}", r.key()))
    }

    pub(crate) async fn emoji(&self) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let (style, custom) = self.emoji_settings(&mut conn).await?;
        let mut lines = vec![t.f(
            "settings.emoji_title",
            &[("style", &self.style_name(style))],
        )];
        lines.push(String::new());
        for r in Emoji::ALL {
            let source = if custom.contains_key(r.key()) {
                t.t("settings.source_custom")
            } else {
                t.t("settings.source_pack")
            };
            lines.push(t.f(
                "settings.role",
                &[
                    ("name", &format!("{} {}", r.plain(), self.role_title(r))),
                    ("source", &source),
                ],
            ));
        }
        lines.push(String::new());
        lines.push(t.t("settings.emoji_note"));
        let mut s = Screen::new(lines.join("\n")).row(
            EmojiStyle::ALL
                .iter()
                .map(|x| {
                    let name = self.style_name(*x);
                    let label = if *x == style {
                        format!("✓ {name}")
                    } else {
                        name
                    };
                    btn(label, &Cb::EmojiStyle(*x))
                })
                .collect(),
        );
        for chunk in Emoji::ALL.chunks(3) {
            s = s.row(
                chunk
                    .iter()
                    .map(|r| {
                        btn(
                            format!("{} {}", r.plain(), self.role_title(*r)),
                            &Cb::EmojiRole(*r),
                        )
                    })
                    .collect(),
            );
        }
        Ok(s.row(self.back(Cb::Settings)))
    }

    // -----------------------------------------------------------------------------------------
    // Юзерботы, персонал, журнал

    pub(crate) async fn bots(&self) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let bots = storage::admin::userbots(&mut conn).await?;
        let mut lines = vec![t.t("bots.title")];
        if bots.is_empty() {
            lines.push(t.t("bots.empty"));
        }
        let mut s = Screen::default();
        for b in &bots {
            let flood = b
                .flood_wait_until
                .filter(|u| *u > Utc::now())
                .map(|u| t.f("bots.flood", &[("until", &when(u))]))
                .unwrap_or_default();
            lines.push(t.f(
                "bots.item",
                &[
                    ("label", &esc(&b.label)),
                    ("status", &esc(&b.status)),
                    ("flood", &flood),
                    (
                        "last",
                        &b.last_ok_at.map_or_else(|| t.t("common.none"), when),
                    ),
                    ("n", &b.in_flight),
                ],
            ));
            let paused = b.status == "disabled";
            s = s.row(vec![btn(
                t.f(
                    if paused {
                        "bots.b_resume"
                    } else {
                        "bots.b_pause"
                    },
                    &[("label", &esc(&b.label))],
                ),
                &Cb::BotPause(b.id, !paused),
            )]);
        }
        s.text = lines.join("\n");
        // Самопроверка аккаунтов — здесь: с главного экрана её убрал макет.
        Ok(s.row(vec![btn(t.t("dash.b_selftest"), &Cb::Selftest)])
            .row(vec![btn(t.t("nav.refresh"), &Cb::Bots)])
            .row(nav::back(t, &Cb::Menu)))
    }

    pub(crate) async fn staff_list(&self, staff: &StaffMember) -> Result<Screen, OpsError> {
        if let Some(denied) = self.deny(staff, Role::Owner) {
            return Ok(denied);
        }
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let all = storage::staff::list(&mut conn).await?;
        let mut lines = vec![t.t("staff.title")];
        let mut s = Screen::default();
        for m in &all {
            let limit = match m.manual_payout_limit() {
                Some(l) => t.f("staff.limit", &[("limit", &usdt(l))]),
                None => String::new(),
            };
            let off = if m.active() {
                String::new()
            } else {
                t.t("staff.off")
            };
            // Имя — ссылка на карточку сотрудника (владельца не меняют).
            let name = if m.role != Role::Owner && m.active() {
                self.link(&esc(&m.display_name), &Cb::Member(m.tg_user_id))
            } else {
                esc(&m.display_name)
            };
            lines.push(t.f(
                "staff.item",
                &[
                    ("name", &name),
                    ("role", &self.role_name(m.role)),
                    ("id", &m.tg_user_id),
                    ("limit", &limit),
                    ("off", &off),
                ],
            ));
        }
        s.text = lines.join("\n");
        Ok(s.row(vec![btn(t.t("staff.b_add"), &Cb::StaffAdd)])
            .row(nav::back(t, &Cb::Menu)))
    }

    pub(crate) async fn member(&self, staff: &StaffMember, id: i64) -> Result<Screen, OpsError> {
        if let Some(denied) = self.deny(staff, Role::Owner) {
            return Ok(denied);
        }
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let Some(m) = storage::staff::get(&mut conn, id).await? else {
            return self.staff_list(staff).await;
        };
        let other = if m.role == Role::Admin {
            Role::Operator
        } else {
            Role::Admin
        };
        Ok(Screen::new(t.f(
            "staff.member",
            &[
                ("name", &esc(&m.display_name)),
                ("role", &self.role_name(m.role)),
                ("id", &m.tg_user_id),
            ],
        ))
        .row(vec![
            btn(
                t.f("staff.b_role", &[("role", &self.role_name(other))]),
                &Cb::MemberRole(id, other),
            ),
            btn(t.t("staff.b_disable"), &Cb::MemberDisable(id)),
        ])
        .row(self.back(Cb::Staff)))
    }

    /// «Журнал»: кто, что, когда — по [`nav::PAGE`] на странице; заявка и клиент — ссылки.
    pub(crate) async fn audit(&self, page: u32) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_audit")
            .fetch_one(&mut *conn)
            .await?;
        let total = usize::try_from(total).unwrap_or(0);
        let (page, range) = nav::window(total, page);
        let rows =
            storage::audit::recent(&mut conn, i64::try_from(range.end).unwrap_or(0), None, None)
                .await?;
        let rows = rows.get(range.start..).unwrap_or(&[]);
        let title = t.f("audit.title", &[("n", &total)]);
        let mut lines = Vec::new();
        for r in rows {
            let target = match (r.target_type.as_deref(), r.target_id.as_deref()) {
                (Some("order"), Some(id)) => match id.parse::<i64>() {
                    Ok(n) => format!(" · {}", self.link(&format!("№{n}"), &Cb::Order(n))),
                    Err(_) => format!(" · {}", esc(id)),
                },
                (Some("user"), Some(id)) => match id.parse::<i64>() {
                    Ok(n) => format!(" · {}", self.link(&esc(id), &Cb::User(n))),
                    Err(_) => format!(" · {}", esc(id)),
                },
                (Some(ty), Some(id)) => format!(" · {} {}", esc(ty), esc(id)),
                _ => String::new(),
            };
            let reason = r
                .reason
                .as_deref()
                .map(|x| format!(" · <i>{}</i>", esc(&ui::html::clip(x, 80))))
                .unwrap_or_default();
            lines.push(t.f(
                "audit.item",
                &[
                    ("at", &when(r.at)),
                    (
                        "who",
                        &esc(r.staff_name.as_deref().unwrap_or(&r.staff_id.to_string())),
                    ),
                    ("action", &code(&r.action)),
                    ("target", &target),
                    ("reason", &reason),
                ],
            ));
        }
        let body = if lines.is_empty() {
            t.t("audit.empty")
        } else {
            lines.join("\n")
        };
        Ok(Screen::new(format!("{title}\n\n{body}"))
            .row(nav::pager(t, List::Audit, page, total))
            .row(vec![btn(t.t("nav.refresh"), &Cb::Page(List::Audit, page))])
            .row(nav::back(t, &Cb::Menu)))
    }

    // -----------------------------------------------------------------------------------------
    // Переходы

    pub(crate) async fn navigate(
        self: &Arc<Self>,
        staff: &StaffMember,
        cb: Cb,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        match cb {
            Cb::Dash => self.dash().await,
            Cb::Dirs => self.dirs().await,
            Cb::Dir(d) => self.dir_card(staff, d).await,
            Cb::Promo => self.promo().await,
            Cb::Queue | Cb::Page(List::Queue, _) => {
                let page = if let Cb::Page(_, p) = cb { p } else { 0 };
                self.remember_list(staff.tg_user_id, Cb::Page(List::Queue, page));
                self.queue_for(Some(staff.tg_user_id), page).await
            }
            Cb::Page(List::Orders, page) => {
                self.remember_list(staff.tg_user_id, cb.clone());
                self.orders(page).await
            }
            Cb::Page(List::Users, page) => {
                self.remember_list(staff.tg_user_id, cb.clone());
                self.users(staff, page).await
            }
            Cb::Page(List::Inbox, page) => self.inbox(page).await,
            Cb::Page(List::Audit, page) => self.audit(page).await,
            Cb::Page(List::Investors, page) => {
                if let Some(denied) = self.deny(staff, Role::Admin) {
                    return Ok(denied);
                }
                self.investors(page).await
            }
            Cb::UserOrders(user, page) => {
                self.remember_list(staff.tg_user_id, cb.clone());
                self.user_orders(user, page).await
            }
            Cb::Question(client) => self.question_from_store(client).await,
            Cb::Order(id) => self.order(Some(staff), id).await,
            Cb::User(id) => self.user(Some(staff), id).await,
            Cb::UserTerms(id) => self.user_terms(id).await,
            Cb::UserDir(id, d) => self.user_dir(id, d).await,
            Cb::Settings => self.settings().await,
            Cb::Emoji => self.emoji().await,
            Cb::Bots => self.bots().await,
            Cb::Staff => self.staff_list(staff).await,
            Cb::Member(id) => self.member(staff, id).await,
            Cb::Audit => self.audit(0).await,
            Cb::Investors | Cb::Invest(_) | Cb::InvestMove(_) => {
                if let Some(denied) = self.deny(staff, Role::Admin) {
                    return Ok(denied);
                }
                match cb {
                    Cb::Invest(id) => self.invest_card(staff, id).await,
                    Cb::InvestMove(id) => self.invest_move(staff, id).await,
                    _ => self.investors(0).await,
                }
            }
            Cb::InvestEdit(_)
            | Cb::InvestAccess(..)
            | Cb::InvestApprove(_)
            | Cb::InvestReject(_)
            | Cb::InvestResolve(..) => self.invest_entry(staff, cb).await,
            Cb::Reconcile => {
                if let Some(denied) = self.deny(staff, Role::Admin) {
                    return Ok(denied);
                }
                self.reconcile_now().await
            }
            Cb::Reserves => self.reserves().await,
            Cb::AlertShow(id) => self.alert_screen(id).await,
            Cb::Investigate(id) => self.investigate(staff, id).await,
            Cb::Thresholds => self.thresholds_screen().await,
            Cb::Profit => Ok(self.profit()),
            Cb::OrderAsk => Ok(self.prompt_to(
                staff,
                Input::OrderSearch,
                "order.ask",
                &Cb::Page(List::Orders, 0),
            )),
            Cb::UserAsk => Ok(self.prompt_to(
                staff,
                Input::UserSearch,
                "user.ask",
                &Cb::Page(List::Users, 0),
            )),
            Cb::Take(id) => {
                let mut tx = self.pool.begin().await?;
                let target = id.to_string();
                storage::audit::record(
                    &mut tx,
                    &storage::audit::NewAudit {
                        staff_id: staff.tg_user_id,
                        action: "order.take",
                        target_type: Some("order"),
                        target_id: Some(&target),
                        before: None,
                        after: None,
                        reason: None,
                        double_confirmed: false,
                    },
                )
                .await?;
                tx.commit().await?;
                Ok(self.order(Some(staff), id).await?.notice(
                    &self
                        .texts
                        .f("order.taken", &[("name", &esc(&staff.display_name))]),
                ))
            }
            Cb::Decide(id, kind) => {
                let need = match kind {
                    DecisionKind::Close => Role::Owner,
                    DecisionKind::ClosePaid | DecisionKind::Intake => Role::Admin,
                    _ => Role::Operator,
                };
                if let Some(denied) = self.deny(staff, need) {
                    return Ok(denied);
                }
                let input = if kind == DecisionKind::Intake {
                    Input::DecisionAmount { order: id }
                } else {
                    Input::DecisionReason {
                        order: id,
                        kind,
                        amount: None,
                    }
                };
                if kind == DecisionKind::Intake {
                    return Ok(self.prompt(staff, input, "order.ask_amount"));
                }
                let text = t.f(
                    "order.ask_reason",
                    &[("action", &self.decision_name(kind)), ("id", &id)],
                );
                Ok(self.prompt_with(staff, input, text))
            }
            cb => self.edit_entry(staff, cb).await,
        }
    }

    pub(crate) fn decision_name(&self, k: DecisionKind) -> String {
        self.texts.t(match k {
            DecisionKind::RetryPayout => "order.b_retry",
            DecisionKind::RetryInvoice => "order.b_retry_invoice",
            DecisionKind::Refund => "order.b_refund",
            DecisionKind::ClosePaid => "order.b_paid",
            DecisionKind::Close => "order.b_close",
            DecisionKind::Intake => "order.b_intake",
        })
    }

    /// Кнопки, которые ведут к изменению: вопрос или предпросмотр.
    async fn edit_entry(self: &Arc<Self>, staff: &StaffMember, cb: Cb) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let need = match &cb {
            Cb::Toggle(Flag::KillSwitch) => Role::Operator,
            Cb::StaffAdd
            | Cb::MemberRole(..)
            | Cb::MemberDisable(_)
            | Cb::Selftest
            | Cb::Capital(_) => Role::Owner,
            _ => Role::Admin,
        };
        if let Some(denied) = self.deny(staff, need) {
            return Ok(denied);
        }
        let mut conn = self.pool.acquire().await?;
        let action = match cb {
            Cb::DirEdit(d, field) => {
                let row = storage::directions::get(&mut conn, d).await?;
                let (key, now) = match field {
                    DirField::Fee => ("dir.ask_fee", pct(row.fee_pct)),
                    DirField::MinFee => ("dir.ask_min_fee", usdt(row.min_fee)),
                    DirField::Lever => (
                        "dir.ask_lever",
                        format!(
                            "{} {} {}",
                            row.skew_k_pct.normalize(),
                            row.fee_floor_pct.normalize(),
                            row.fee_cap_pct.normalize()
                        ),
                    ),
                    DirField::Limits => (
                        "dir.ask_limits",
                        format!(
                            "{} {}",
                            row.min_amount.normalize(),
                            row.max_amount.normalize()
                        ),
                    ),
                    DirField::Newcomer => ("dir.ask_newcomer", usdt(row.new_user_max_amount)),
                    DirField::Tiers => ("dir.ask_tiers", esc(&self.tiers_text(&row))),
                    DirField::Fiat => ("dir.ask_fiat", pct(row.fiat_buffer_pct)),
                };
                let text = t.f(key, &[("now", &now)]);
                return Ok(self.prompt_with(staff, Input::Dir(d, field), text));
            }
            Cb::DirMode(d) => {
                let before = storage::directions::get(&mut conn, d).await?;
                if before.fee_tiers.is_none() {
                    return Ok(Screen::new(t.t("dir.no_tiers")).row(self.back(Cb::Dir(d))));
                }
                let mut after = before.clone();
                after.fee_mode = match before.fee_mode {
                    FeeMode::Flat => FeeMode::Tiers,
                    FeeMode::Tiers => FeeMode::Flat,
                };
                Action::Direction { before, after }
            }
            Cb::DirOpen(d) => {
                let row = storage::directions::get(&mut conn, d).await?;
                Action::DirOpen {
                    direction: d,
                    open: !row.is_open,
                }
            }
            Cb::DirClearAuto(d) => Action::DirClearAuto { direction: d },
            Cb::PromoMode(mode) => {
                let before = engine::fees::promo_settings(&mut conn).await?;
                let after = engine::fees::PromoSettings { mode, ..before };
                Action::PromoSettings { before, after }
            }
            Cb::PromoRate => {
                let s = engine::fees::promo_settings(&mut conn).await?;
                let now = format!("{} {}", s.fee_pct.normalize(), s.min_fee.normalize());
                let text = t.f("promo.ask_rate", &[("now", &now)]);
                return Ok(self.prompt_with(staff, Input::PromoRate, text));
            }
            Cb::PromoThresholds => {
                let s = engine::fees::promo_settings(&mut conn).await?;
                let now = format!(
                    "{} {}",
                    s.trigger_pct.normalize(),
                    s.release_pct.normalize()
                );
                let text = t.f("promo.ask_thresholds", &[("now", &now)]);
                return Ok(self.prompt_with(staff, Input::PromoThresholds, text));
            }
            Cb::PromoTarget(p) => {
                let now = match storage::wallets::active(&mut conn, p).await? {
                    Some((w, _)) => storage::admin::thresholds(&mut conn, w, Asset::Usdt)
                        .await?
                        .map_or_else(|| t.t("common.none"), |th| usdt(th.target)),
                    None => t.t("common.none"),
                };
                let text = t.f(
                    "promo.ask_target",
                    &[("name", &self.platform_name(p)), ("now", &now)],
                );
                return Ok(self.prompt_with(staff, Input::Target(p), text));
            }
            Cb::PromoSwitch(direction, on) => {
                let (_, _, room) = engine::fees::direction_reserves(
                    &mut conn,
                    direction,
                    Asset::Usdt,
                    Decimal::ZERO,
                    Decimal::ZERO,
                )
                .await?;
                Action::PromoSwitch {
                    direction,
                    on,
                    room,
                }
            }
            Cb::UserEdit(id, d, field) => {
                let key = match field {
                    TermField::Fee => "user.ask_fee",
                    TermField::MinFee => "user.ask_min_fee",
                    TermField::Min => "user.ask_min",
                    TermField::Max => "user.ask_max",
                };
                return Ok(self.prompt(staff, Input::Term(id, d, field), key));
            }
            Cb::UserExempt(id, d) => {
                let before = storage::user_terms::get(&mut conn, id, d).await?;
                let mut after = before.clone();
                after.newcomer_exempt = !before.newcomer_exempt;
                Action::Terms {
                    user: id,
                    before,
                    after,
                }
            }
            Cb::UserReset(id, d) => {
                let before = storage::user_terms::get(&mut conn, id, d).await?;
                let after = storage::user_terms::UserTerms {
                    note: before.note.clone(),
                    ..storage::user_terms::UserTerms::empty(d)
                };
                Action::Terms {
                    user: id,
                    before,
                    after,
                }
            }
            Cb::UserDaily(id) => return Ok(self.prompt(staff, Input::Daily(id), "user.ask_daily")),
            Cb::UserBan(id) => return Ok(self.prompt(staff, Input::Ban(id), "user.ask_ban")),
            Cb::UserUnban(id) => return Ok(self.prompt(staff, Input::Unban(id), "user.ask_unban")),
            Cb::UserNote(id) => return Ok(self.prompt(staff, Input::Note(id), "user.ask_note")),
            Cb::Toggle(flag) => {
                let on = storage::settings::flag(&mut conn, flag.key(), flag.default_on()).await?;
                if flag == Flag::KillSwitch && on {
                    // Выключить стоп-кран — только владелец.
                    if let Some(denied) = self.deny(staff, Role::Owner) {
                        return Ok(denied);
                    }
                }
                Action::Flag { flag, on: !on }
            }
            Cb::EmojiStyle(style) => {
                let (before, _) = self.emoji_settings(&mut conn).await?;
                Action::EmojiStyle {
                    before,
                    after: style,
                }
            }
            Cb::EmojiRole(role) => {
                let text = t.f("settings.ask_emoji", &[("name", &self.role_title(role))]);
                return Ok(self.prompt_with(staff, Input::EmojiFor(role), text));
            }
            Cb::BotPause(id, paused) => {
                let label = storage::admin::userbots(&mut conn)
                    .await?
                    .into_iter()
                    .find(|b| b.id == id)
                    .map_or_else(|| id.to_string(), |b| b.label);
                Action::BotPause { id, label, paused }
            }
            Cb::StaffAdd => return Ok(self.prompt(staff, Input::StaffAdd, "staff.ask_add")),
            Cb::MemberRole(id, role) => {
                let Some(m) = storage::staff::get(&mut conn, id).await? else {
                    return self.staff_list(staff).await;
                };
                Action::StaffRole {
                    id,
                    name: m.display_name,
                    before: m.role,
                    after: role,
                }
            }
            Cb::MemberDisable(id) => {
                let Some(m) = storage::staff::get(&mut conn, id).await? else {
                    return self.staff_list(staff).await;
                };
                Action::StaffDisable {
                    id,
                    name: m.display_name,
                }
            }
            Cb::Selftest => Action::Selftest {
                amount: Decimal::ONE,
            },
            Cb::Capital(alert) => {
                let Some(a) = storage::alerts::get(&mut conn, alert)
                    .await?
                    .filter(|a| a.acked_at.is_none() && a.title == "balance_mismatch")
                else {
                    return Ok(Screen::new(t.t("capital.gone"))
                        .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                };
                let platform = a.payload["platform"]
                    .as_str()
                    .and_then(Platform::from_db_name);
                let diff: Option<Decimal> = a.payload["diff"].as_str().and_then(|d| d.parse().ok());
                let (Some(platform), Some(diff)) = (platform, diff) else {
                    return Ok(Screen::new(t.t("capital.gone"))
                        .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                };
                Action::Capital {
                    platform,
                    amount: diff.abs(),
                    deposit: diff > Decimal::ZERO,
                    alert,
                }
            }
            _ => return self.menu(staff).await,
        };
        drop(conn);
        self.preview(staff, action).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_codes_are_masked() {
        assert_eq!(mask("CQabcdefgh9F"), "CQab…9F");
        assert_eq!(mask("short"), "short");
        assert_eq!(
            mask_links("Чек: https://t.me/send?start=CQabcdefgh9F готов"),
            "Чек: https://t.me/send?start=CQab…9F готов"
        );
    }
}
