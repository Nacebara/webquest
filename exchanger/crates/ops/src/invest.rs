//! Инвесторы в админ-панели (DESIGN-v0.2 Р18): выдать доступ и условия (владелец), одобрить
//! или отклонить вывод (админ — в пределах своего лимита ручных выплат), разобрать неясное
//! движение (владелец). Всё — через предпросмотр с «Подтвердить»; деньги двигает движок
//! (`engine::invest`), он же пишет журнал.

use std::sync::Arc;

use domain::{Decimal, Platform};
use engine::invest::{InvestError, InvestRefusal, InvestVerdict};
use storage::invest::{Investor, Move, MoveKind, MoveStatus};
use storage::staff::{Role, StaffMember};

use crate::actions::{Action, Input};
use crate::cb::Cb;
use crate::texts::esc;
use crate::views::{pct, usdt, when};
use crate::{Inner, OpsError, Screen, btn};

/// Чего ждём от сотрудника по инвесторам.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InvestInput {
    /// Условия одной строкой: доля, дни блокировки, автовывод, [мин. пополнение].
    Terms(i64),
    /// Причина отказа в выводе.
    Reject(i64),
    /// Сколько пришло по неясному пополнению.
    Amount(i64),
    /// Что проверили по неясному движению.
    Reason {
        move_id: i64,
        happened: bool,
        amount: Option<Decimal>,
    },
}

/// Решение по инвесторам, ждущее подтверждения.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InvestOp {
    Terms {
        before: Option<Investor>,
        after: Investor,
    },
    Access {
        user: i64,
        active: bool,
    },
    Approve {
        move_id: i64,
        user: i64,
        amount: Decimal,
        platform: Platform,
    },
    Reject {
        move_id: i64,
        user: i64,
        amount: Decimal,
        reason: String,
    },
    Resolve {
        move_id: i64,
        user: i64,
        kind: MoveKind,
        happened: bool,
        amount: Option<Decimal>,
        reason: String,
    },
}

impl InvestOp {
    pub(crate) fn min_role(&self) -> Role {
        match self {
            InvestOp::Approve { .. } | InvestOp::Reject { .. } => Role::Admin,
            _ => Role::Owner,
        }
    }
}

fn num(s: &str) -> Option<Decimal> {
    let v: Decimal = s.trim().replace(',', ".").parse().ok()?;
    (!v.is_sign_negative()).then_some(v.normalize())
}

/// «50 14 100 [10]» → доля, дни, автовывод, мин. пополнение.
fn parse_terms(text: &str, base: &Investor) -> Option<Investor> {
    let parts: Vec<&str> = text
        .split_whitespace()
        .map(|p| p.trim_end_matches('%'))
        .collect();
    let (share, days, auto, min) = match parts.as_slice() {
        [s, d, a] => (num(s)?, d.parse::<i32>().ok()?, num(a)?, base.min_deposit),
        [s, d, a, m] => (num(s)?, d.parse::<i32>().ok()?, num(a)?, num(m)?),
        _ => return None,
    };
    let ok = share <= Decimal::ONE_HUNDRED
        && (0..=365).contains(&days)
        && min > Decimal::ZERO
        && share.scale() <= 6;
    ok.then(|| Investor {
        share_pct: share,
        lock_days: days,
        auto_withdraw_max: auto,
        min_deposit: min,
        ..base.clone()
    })
}

impl Inner {
    fn move_kind(&self, k: MoveKind) -> String {
        self.texts.t(match k {
            MoveKind::Deposit => "invest.kind_deposit",
            MoveKind::Withdrawal => "invest.kind_withdrawal",
        })
    }

    fn move_status(&self, s: MoveStatus) -> String {
        self.texts.t(match s {
            MoveStatus::Requested => "invest.status_requested",
            MoveStatus::Processing => "invest.status_processing",
            MoveStatus::Done => "invest.status_done",
            MoveStatus::Rejected => "invest.status_rejected",
            MoveStatus::Failed => "invest.status_failed",
            MoveStatus::Manual => "invest.status_manual",
        })
    }

    fn move_amount(m: &Move) -> String {
        m.amount.map_or_else(|| "—".to_owned(), usdt)
    }

    fn invest_refusal(&self, e: InvestError) -> String {
        let t = &self.texts;
        let why = match e {
            InvestError::Engine(e) => {
                tracing::error!(error = %e, "investor decision failed");
                return t.f("common.error", &[("error", &esc(&e.to_string()))]);
            }
            InvestError::Refused(r) => match r {
                InvestRefusal::NotInvestor => t.t("invest.r_not_investor"),
                InvestRefusal::WrongState => t.t("invest.r_wrong_state"),
                InvestRefusal::OverWithdrawable { max } => {
                    t.f("invest.r_over", &[("max", &usdt(max))])
                }
                InvestRefusal::NoReserve { available, needed } => t.f(
                    "invest.r_reserve",
                    &[("available", &usdt(available)), ("needed", &usdt(needed))],
                ),
                InvestRefusal::OverManualLimit { amount, limit } => t.f(
                    "invest.r_limit",
                    &[("amount", &usdt(amount)), ("limit", &usdt(limit))],
                ),
                InvestRefusal::Forbidden(role) => {
                    t.f("invest.r_forbidden", &[("role", &self.role_name(role))])
                }
                InvestRefusal::NoWallet => t.t("invest.r_wallet"),
                InvestRefusal::BadTerms(_) => t.t("invest.r_terms"),
                InvestRefusal::UnknownUser => t.t("invest.r_user"),
                InvestRefusal::ReasonTooShort => t.t("invest.r_reason"),
                InvestRefusal::BadAmount => t.t("invest.r_amount"),
                other => esc(&other.to_string()),
            },
        };
        t.f("invest.refusal", &[("why", &why)])
    }

    /// Все инвесторы и движения, которые ждут решения.
    pub(crate) async fn investors(&self) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let totals = storage::invest::totals(&mut conn).await?;
        let list = storage::invest::list(&mut conn).await?;
        let awaiting = storage::invest::awaiting_staff(&mut conn, 10).await?;
        let mut lines = vec![t.t("invest.list_title")];
        if list.is_empty() {
            lines.push(t.t("invest.list_empty"));
        } else {
            lines.push(t.f(
                "invest.list_totals",
                &[
                    ("n", &totals.investors),
                    ("balance", &usdt(totals.balance)),
                    ("accrued", &usdt(totals.accrued_24h)),
                ],
            ));
        }
        if !awaiting.is_empty() {
            lines.push(t.f("invest.list_awaiting", &[("n", &awaiting.len())]));
        }
        let mut s = Screen::new(lines.join("\n"));
        for m in &awaiting {
            s = s.row(vec![btn(
                t.f(
                    "invest.b_move",
                    &[
                        ("id", &m.id),
                        ("kind", &self.move_kind(m.kind)),
                        ("amount", &Self::move_amount(m)),
                    ],
                ),
                &Cb::InvestMove(m.id),
            )]);
        }
        for i in list.iter().take(20) {
            let balance = storage::invest::figures(&mut conn, i.user_id, i.lock_days)
                .await?
                .balance;
            let label = t.f(
                if i.is_active {
                    "invest.b_investor"
                } else {
                    "invest.b_investor_off"
                },
                &[
                    ("id", &i.user_id),
                    ("share", &pct(i.share_pct)),
                    ("balance", &usdt(balance)),
                ],
            );
            s = s.row(vec![btn(label, &Cb::Invest(i.user_id))]);
        }
        Ok(s.row(vec![
            btn(t.t("common.refresh"), &Cb::Investors),
            btn(t.t("common.home"), &Cb::Menu),
        ]))
    }

    /// Карточка «💼 Инвестиции» клиента.
    pub(crate) async fn invest_card(
        &self,
        staff: &StaffMember,
        user: i64,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let investor = storage::invest::investor(&mut conn, user).await?;
        let mut lines = vec![t.f("invest.card_title", &[("id", &user)])];
        let owner = staff.role >= Role::Owner;
        let Some(i) = investor else {
            lines.push(t.t("invest.card_none"));
            let mut s = Screen::new(lines.join("\n"));
            if owner {
                s = s.row(vec![btn(t.t("invest.b_grant"), &Cb::InvestEdit(user))]);
            }
            return Ok(s.row(vec![
                btn(t.t("common.back"), &Cb::User(user)),
                btn(t.t("common.home"), &Cb::Menu),
            ]));
        };
        if !i.is_active {
            lines.push(t.t("invest.card_off"));
        }
        lines.push(t.f(
            "invest.card_terms",
            &[
                ("share", &pct(i.share_pct)),
                ("days", &i.lock_days),
                ("auto", &usdt(i.auto_withdraw_max)),
                ("min", &usdt(i.min_deposit)),
            ],
        ));
        let f = storage::invest::figures(&mut conn, user, i.lock_days).await?;
        lines.push(t.f(
            "invest.card_figures",
            &[
                ("balance", &usdt(f.balance)),
                ("profit", &usdt(f.profit)),
                ("profit_24h", &usdt(f.profit_24h)),
                ("withdrawable", &usdt(f.withdrawable())),
            ],
        ));
        let moves = storage::invest::recent(&mut conn, user, 8).await?;
        if !moves.is_empty() {
            lines.push(String::new());
            lines.push(t.t("invest.card_moves"));
            for m in &moves {
                lines.push(t.f(
                    "invest.move_line",
                    &[
                        ("id", &m.id),
                        ("at", &when(m.created_at)),
                        ("kind", &self.move_kind(m.kind)),
                        ("amount", &Self::move_amount(m)),
                        ("status", &self.move_status(m.status)),
                    ],
                ));
            }
        }
        let mut s = Screen::new(lines.join("\n"));
        for m in moves
            .iter()
            .filter(|m| matches!(m.status, MoveStatus::Requested | MoveStatus::Manual))
        {
            s = s.row(vec![btn(
                t.f(
                    "invest.b_move",
                    &[
                        ("id", &m.id),
                        ("kind", &self.move_kind(m.kind)),
                        ("amount", &Self::move_amount(m)),
                    ],
                ),
                &Cb::InvestMove(m.id),
            )]);
        }
        if owner {
            s = s.row(vec![
                btn(t.t("invest.b_edit"), &Cb::InvestEdit(user)),
                if i.is_active {
                    btn(t.t("invest.b_off"), &Cb::InvestAccess(user, false))
                } else {
                    btn(t.t("invest.b_on"), &Cb::InvestAccess(user, true))
                },
            ]);
        }
        Ok(s.row(vec![
            btn(t.t("common.refresh"), &Cb::Invest(user)),
            btn(t.t("invest.b_list"), &Cb::Investors),
        ])
        .row(vec![
            btn(t.t("common.back"), &Cb::User(user)),
            btn(t.t("common.home"), &Cb::Menu),
        ]))
    }

    /// Карточка движения с решениями.
    pub(crate) async fn invest_move(
        &self,
        staff: &StaffMember,
        id: i64,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let mut conn = self.pool.acquire().await?;
        let m = match storage::invest::get(&mut conn, id, false).await {
            Ok(m) => m,
            Err(storage::StorageError::NotFound(_)) => {
                return Ok(Screen::new(t.t("invest.move_gone"))
                    .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
            }
            Err(e) => return Err(e.into()),
        };
        let lock = storage::invest::investor(&mut conn, m.user_id)
            .await?
            .map_or(0, |i| i.lock_days);
        let f = storage::invest::figures(&mut conn, m.user_id, lock).await?;
        let note = m
            .note
            .as_deref()
            .map(|n| t.f("invest.move_note", &[("note", &esc(n))]))
            .unwrap_or_default();
        let text = t.f(
            "invest.move_card",
            &[
                ("id", &m.id),
                ("kind", &self.move_kind(m.kind)),
                ("user", &m.user_id),
                ("wallet", &self.platform_name(m.platform)),
                ("amount", &Self::move_amount(&m)),
                ("status", &self.move_status(m.status)),
                ("at", &when(m.created_at)),
                ("note", &note),
                ("balance", &usdt(f.balance)),
                ("withdrawable", &usdt(f.withdrawable())),
            ],
        );
        let mut s = Screen::new(text);
        match m.status {
            MoveStatus::Requested if staff.role >= Role::Admin => {
                s = s.row(vec![
                    btn(t.t("invest.b_approve"), &Cb::InvestApprove(m.id)),
                    btn(t.t("invest.b_reject"), &Cb::InvestReject(m.id)),
                ]);
            }
            MoveStatus::Manual if staff.role >= Role::Owner => {
                let yes = t.t(match m.kind {
                    MoveKind::Deposit => "invest.b_happened_deposit",
                    MoveKind::Withdrawal => "invest.b_happened_withdrawal",
                });
                s = s.row(vec![
                    btn(yes, &Cb::InvestResolve(m.id, true)),
                    btn(
                        t.t("invest.b_not_happened"),
                        &Cb::InvestResolve(m.id, false),
                    ),
                ]);
            }
            _ => {}
        }
        Ok(s.row(vec![
            btn(t.t("invest.b_investor_card"), &Cb::Invest(m.user_id)),
            btn(t.t("common.refresh"), &Cb::InvestMove(m.id)),
        ])
        .row(vec![btn(t.t("common.home"), &Cb::Menu)]))
    }

    /// Кнопки, которые ведут к решению: вопрос или предпросмотр.
    pub(crate) async fn invest_entry(
        self: &Arc<Self>,
        staff: &StaffMember,
        cb: Cb,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let need = match cb {
            Cb::InvestApprove(_) | Cb::InvestReject(_) => Role::Admin,
            _ => Role::Owner,
        };
        if let Some(denied) = self.deny(staff, need) {
            return Ok(denied);
        }
        let mut conn = self.pool.acquire().await?;
        let op = match cb {
            Cb::InvestEdit(user) => {
                let now = storage::invest::investor(&mut conn, user)
                    .await?
                    .map_or_else(
                        || t.t("common.none"),
                        |i| {
                            format!(
                                "<code>{} {} {} {}</code>",
                                i.share_pct.normalize(),
                                i.lock_days,
                                i.auto_withdraw_max.normalize(),
                                i.min_deposit.normalize()
                            )
                        },
                    );
                let text = t.f("invest.ask_terms", &[("now", &now)]);
                return Ok(self.prompt_with(staff, Input::Invest(InvestInput::Terms(user)), text));
            }
            Cb::InvestAccess(user, active) => InvestOp::Access { user, active },
            Cb::InvestApprove(id) | Cb::InvestReject(id) | Cb::InvestResolve(id, _) => {
                let m = match storage::invest::get(&mut conn, id, false).await {
                    Ok(m) => m,
                    Err(storage::StorageError::NotFound(_)) => {
                        return Ok(Screen::new(t.t("invest.move_gone"))
                            .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                    }
                    Err(e) => return Err(e.into()),
                };
                let amount = m.amount.unwrap_or_default();
                match cb {
                    Cb::InvestApprove(_) => InvestOp::Approve {
                        move_id: id,
                        user: m.user_id,
                        amount,
                        platform: m.platform,
                    },
                    Cb::InvestReject(_) => {
                        let text = t.f("invest.ask_reject", &[("id", &id)]);
                        return Ok(self.prompt_with(
                            staff,
                            Input::Invest(InvestInput::Reject(id)),
                            text,
                        ));
                    }
                    Cb::InvestResolve(_, true) if m.kind == MoveKind::Deposit => {
                        let text = t.f("invest.ask_amount", &[("id", &id)]);
                        return Ok(self.prompt_with(
                            staff,
                            Input::Invest(InvestInput::Amount(id)),
                            text,
                        ));
                    }
                    Cb::InvestResolve(_, happened) => {
                        let text = t.f("invest.ask_reason", &[("id", &id)]);
                        return Ok(self.prompt_with(
                            staff,
                            Input::Invest(InvestInput::Reason {
                                move_id: id,
                                happened,
                                amount: None,
                            }),
                            text,
                        ));
                    }
                    _ => return self.menu(staff).await,
                }
            }
            _ => return self.menu(staff).await,
        };
        drop(conn);
        self.preview(staff, Action::Invest(op)).await
    }

    /// Ответ сотрудника на вопрос по инвесторам.
    pub(crate) async fn invest_input(
        self: &Arc<Self>,
        staff: &StaffMember,
        input: InvestInput,
        text: &str,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let retry = |me: &Arc<Self>, hint: &str| {
            let text = t.f("common.bad_input", &[("hint", &t.t(hint))]);
            me.prompt_with(staff, Input::Invest(input.clone()), text)
        };
        let mut conn = self.pool.acquire().await?;
        let op = match input.clone() {
            InvestInput::Terms(user) => {
                let before = storage::invest::investor(&mut conn, user).await?;
                let base = before.clone().unwrap_or(Investor {
                    user_id: user,
                    share_pct: Decimal::ZERO,
                    lock_days: 14,
                    min_deposit: Decimal::TEN,
                    auto_withdraw_max: Decimal::ZERO,
                    is_active: true,
                    granted_by: Some(staff.tg_user_id),
                    created_at: chrono::Utc::now(),
                });
                let Some(mut after) = parse_terms(text, &base) else {
                    return Ok(retry(self, "invest.hint_terms"));
                };
                after.is_active = true;
                InvestOp::Terms { before, after }
            }
            InvestInput::Reject(move_id) => {
                if text.chars().count() < 3 {
                    return Ok(self.prompt_with(
                        staff,
                        Input::Invest(input),
                        t.t("refusal.reason_short"),
                    ));
                }
                let m = storage::invest::get(&mut conn, move_id, false).await?;
                InvestOp::Reject {
                    move_id,
                    user: m.user_id,
                    amount: m.amount.unwrap_or_default(),
                    reason: text.chars().take(300).collect(),
                }
            }
            InvestInput::Amount(move_id) => {
                let Some(amount) = num(text).filter(|a| *a > Decimal::ZERO) else {
                    return Ok(self.prompt_with(
                        staff,
                        Input::Invest(input),
                        t.t("common.bad_number"),
                    ));
                };
                let text = t.f("invest.ask_reason", &[("id", &move_id)]);
                return Ok(self.prompt_with(
                    staff,
                    Input::Invest(InvestInput::Reason {
                        move_id,
                        happened: true,
                        amount: Some(amount),
                    }),
                    text,
                ));
            }
            InvestInput::Reason {
                move_id,
                happened,
                amount,
            } => {
                if text.chars().count() < 3 {
                    return Ok(self.prompt_with(
                        staff,
                        Input::Invest(input),
                        t.t("refusal.reason_short"),
                    ));
                }
                let m = storage::invest::get(&mut conn, move_id, false).await?;
                InvestOp::Resolve {
                    move_id,
                    user: m.user_id,
                    kind: m.kind,
                    happened,
                    amount: amount.or(m.amount),
                    reason: text.chars().take(300).collect(),
                }
            }
        };
        drop(conn);
        self.preview(staff, Action::Invest(op)).await
    }

    /// Текст предпросмотра.
    pub(crate) fn invest_preview(&self, op: &InvestOp) -> String {
        let t = &self.texts;
        match op {
            InvestOp::Terms { before, after } => {
                let field = |key: &str, b: Option<String>, a: String| {
                    t.f(
                        "dir.change",
                        &[
                            ("field", &t.t(key)),
                            ("before", &b.unwrap_or_else(|| t.t("common.none"))),
                            ("after", &a),
                        ],
                    )
                };
                let b = before.as_ref();
                let changes = [
                    field(
                        "invest.f_share",
                        b.map(|i| pct(i.share_pct)),
                        pct(after.share_pct),
                    ),
                    field(
                        "invest.f_lock",
                        b.map(|i| i.lock_days.to_string()),
                        after.lock_days.to_string(),
                    ),
                    field(
                        "invest.f_auto",
                        b.map(|i| usdt(i.auto_withdraw_max)),
                        usdt(after.auto_withdraw_max),
                    ),
                    field(
                        "invest.f_min",
                        b.map(|i| usdt(i.min_deposit)),
                        usdt(after.min_deposit),
                    ),
                ]
                .join("\n");
                t.f(
                    if before.is_some() {
                        "invest.preview_terms"
                    } else {
                        "invest.preview_grant"
                    },
                    &[("id", &after.user_id), ("changes", &changes)],
                )
            }
            InvestOp::Access { user, active } => t.f(
                if *active {
                    "invest.preview_on"
                } else {
                    "invest.preview_off"
                },
                &[("id", user)],
            ),
            InvestOp::Approve {
                user,
                amount,
                platform,
                ..
            } => t.f(
                "invest.preview_approve",
                &[
                    ("user", user),
                    ("amount", &usdt(*amount)),
                    ("wallet", &self.platform_name(*platform)),
                ],
            ),
            InvestOp::Reject {
                user,
                amount,
                reason,
                ..
            } => t.f(
                "invest.preview_reject",
                &[
                    ("user", user),
                    ("amount", &usdt(*amount)),
                    ("reason", &esc(reason)),
                ],
            ),
            InvestOp::Resolve {
                move_id,
                user,
                kind,
                happened,
                amount,
                reason,
            } => {
                let key = match (kind, happened) {
                    (MoveKind::Deposit, true) => "invest.preview_dep_yes",
                    (MoveKind::Deposit, false) => "invest.preview_dep_no",
                    (MoveKind::Withdrawal, true) => "invest.preview_out_yes",
                    (MoveKind::Withdrawal, false) => "invest.preview_out_no",
                };
                t.f(
                    key,
                    &[
                        ("id", move_id),
                        ("user", user),
                        ("amount", &amount.map_or_else(|| "—".to_owned(), usdt)),
                        ("reason", &esc(reason)),
                    ],
                )
            }
        }
    }

    /// Применить подтверждённое решение.
    pub(crate) async fn invest_apply(
        &self,
        staff: &StaffMember,
        op: InvestOp,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let result = match &op {
            InvestOp::Terms { after, .. } => {
                let user = after.user_id;
                return Ok(match self.engine.invest_save_terms(staff, after).await {
                    Ok(()) => {
                        let mut card = self.invest_card(staff, user).await?;
                        card.text = format!("{}\n\n{}", t.t("common.saved"), card.text);
                        card
                    }
                    Err(e) => Screen::new(self.invest_refusal(e))
                        .row(vec![btn(t.t("common.back"), &Cb::Invest(user))]),
                });
            }
            InvestOp::Access { user, active } => {
                let current = {
                    let mut conn = self.pool.acquire().await?;
                    storage::invest::investor(&mut conn, *user).await?
                };
                let Some(mut terms) = current else {
                    return self.invest_card(staff, *user).await;
                };
                terms.is_active = *active;
                return Ok(match self.engine.invest_save_terms(staff, &terms).await {
                    Ok(()) => {
                        let mut card = self.invest_card(staff, *user).await?;
                        card.text = format!("{}\n\n{}", t.t("common.saved"), card.text);
                        card
                    }
                    Err(e) => Screen::new(self.invest_refusal(e))
                        .row(vec![btn(t.t("common.back"), &Cb::Invest(*user))]),
                });
            }
            InvestOp::Approve { move_id, .. } => {
                (*move_id, self.engine.invest_approve(*move_id, staff).await)
            }
            InvestOp::Reject {
                move_id, reason, ..
            } => (
                *move_id,
                self.engine.invest_reject(*move_id, staff, reason).await,
            ),
            InvestOp::Resolve {
                move_id,
                happened,
                amount,
                reason,
                ..
            } => {
                let verdict = if *happened {
                    InvestVerdict::Happened { amount: *amount }
                } else {
                    InvestVerdict::DidNotHappen
                };
                (
                    *move_id,
                    self.engine
                        .invest_resolve(*move_id, staff, &verdict, reason)
                        .await,
                )
            }
        };
        let (move_id, result) = result;
        let head = match result {
            Ok(m) => t.f("invest.done", &[("status", &self.move_status(m.status))]),
            Err(e) => self.invest_refusal(e),
        };
        let mut card = self.invest_move(staff, move_id).await?;
        card.text = format!("{head}\n\n{}", card.text);
        Ok(card)
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn base() -> Investor {
        Investor {
            user_id: 1,
            share_pct: dec!(0),
            lock_days: 14,
            min_deposit: dec!(10),
            auto_withdraw_max: dec!(0),
            is_active: true,
            granted_by: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn terms_are_read_from_one_line() {
        let i = parse_terms("50 14 100", &base()).unwrap();
        assert_eq!(
            (i.share_pct, i.lock_days, i.auto_withdraw_max, i.min_deposit),
            (dec!(50), 14, dec!(100), dec!(10))
        );
        let i = parse_terms("33,5% 0 0 25", &base()).unwrap();
        assert_eq!(
            (i.share_pct, i.lock_days, i.auto_withdraw_max, i.min_deposit),
            (dec!(33.5), 0, dec!(0), dec!(25))
        );
        for bad in [
            "",
            "50",
            "50 14",
            "101 14 100",
            "50 400 100",
            "50 14 -1",
            "50 14 100 0",
            "a b c",
        ] {
            assert!(parse_terms(bad, &base()).is_none(), "{bad}");
        }
    }
}
