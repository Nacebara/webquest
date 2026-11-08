//! Вопросы пульта, предпросмотр «было → станет» и применение изменений.
//!
//! Изменение применяется только из [`Inner::apply`] — после «Подтвердить» с nonce. Каждое —
//! одна транзакция с записью в `admin_audit`; решения по заявке пишет в журнал движок.

use std::sync::Arc;

use botapi::{EntityKind, Message};
use domain::fees::{FeeMode, FeeSchedule, FeeTier, MAX_FEE_PCT};
use domain::{Asset, Decimal, Direction, Platform};
use engine::fees::PromoSettings;
use engine::{OperatorDecision, OperatorError, OperatorRequest, Refusal};
use serde_json::{Value, json};
use storage::directions::DirectionRow;
use storage::staff::{Role, StaffMember};
use storage::user_terms::UserTerms;
use ui::{Emoji, EmojiStyle};

use crate::cb::{Cb, DecisionKind, DirField, Flag, TermField};
use crate::texts::esc;
use crate::views::{pct, usdt};
use crate::{Inner, OpsError, Screen, btn};

/// Чего ждём от сотрудника.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Input {
    OrderSearch,
    UserSearch,
    Dir(Direction, DirField),
    PromoRate,
    PromoThresholds,
    Target(Platform),
    DecisionAmount {
        order: i64,
    },
    DecisionReason {
        order: i64,
        kind: DecisionKind,
        amount: Option<Decimal>,
    },
    DecisionEvidence {
        order: i64,
        reason: String,
    },
    Term(i64, Direction, TermField),
    Daily(i64),
    Ban(i64),
    Unban(i64),
    Note(i64),
    EmojiFor(Emoji),
    StaffAdd,
    Invest(crate::invest::InvestInput),
    /// Ответ клиенту из «Поддержки».
    SupportReply(i64),
}

/// Изменение, ждущее подтверждения.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Action {
    Direction {
        before: DirectionRow,
        after: DirectionRow,
    },
    DirOpen {
        direction: Direction,
        open: bool,
    },
    DirClearAuto {
        direction: Direction,
    },
    PromoSettings {
        before: PromoSettings,
        after: PromoSettings,
    },
    PromoSwitch {
        direction: Direction,
        on: bool,
        room: Decimal,
    },
    Target {
        platform: Platform,
        target: Decimal,
    },
    Decide {
        order: i64,
        kind: DecisionKind,
        reason: String,
        evidence: Option<String>,
        amount: Option<Decimal>,
    },
    Terms {
        user: i64,
        before: UserTerms,
        after: UserTerms,
    },
    Daily {
        user: i64,
        limit: Option<Decimal>,
    },
    Note {
        user: i64,
        note: Option<String>,
    },
    Ban {
        user: i64,
        reason: String,
        days: u32,
    },
    Unban {
        user: i64,
        reason: String,
    },
    Flag {
        flag: Flag,
        on: bool,
    },
    EmojiStyle {
        before: EmojiStyle,
        after: EmojiStyle,
    },
    CustomEmoji {
        role: Emoji,
        id: Option<String>,
    },
    BotPause {
        id: i16,
        label: String,
        paused: bool,
    },
    StaffAdd {
        id: i64,
        role: Role,
        name: String,
    },
    StaffRole {
        id: i64,
        name: String,
        before: Role,
        after: Role,
    },
    StaffDisable {
        id: i64,
        name: String,
    },
    /// Расхождение сверки — взнос (`deposit`) или вывод владельца.
    Capital {
        platform: Platform,
        amount: Decimal,
        deposit: bool,
        alert: i64,
    },
    Selftest {
        amount: Decimal,
    },
    Invest(crate::invest::InvestOp),
}

/// Базовая ставка на примерных суммах: рост или падение больше 1 п.п. — решение владельца
/// (SPEC П11).
fn big_change(before: &DirectionRow, after: &DirectionRow) -> bool {
    let base = |row: &DirectionRow, a: Decimal| {
        domain::fees::decide(
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
        .map_or(row.fee_pct, |d| d.base_pct)
    };
    let mut amounts = vec![Decimal::from(10), Decimal::from(100), Decimal::from(300)];
    for row in [before, after] {
        if let Some(s) = &row.fee_tiers {
            amounts.extend(s.tiers().iter().map(|t| t.from));
        }
    }
    amounts
        .into_iter()
        .any(|a| (base(after, a) - base(before, a)).abs() > Decimal::ONE)
}

impl Action {
    pub(crate) fn min_role(&self) -> Role {
        match self {
            Action::Direction { before, after } if big_change(before, after) => Role::Owner,
            Action::Decide { kind, .. } => match kind {
                DecisionKind::Close => Role::Owner,
                DecisionKind::ClosePaid | DecisionKind::Intake => Role::Admin,
                _ => Role::Operator,
            },
            Action::Flag {
                flag: Flag::KillSwitch,
                on,
            } => {
                if *on {
                    Role::Operator
                } else {
                    Role::Owner
                }
            }
            Action::StaffAdd { .. }
            | Action::StaffRole { .. }
            | Action::StaffDisable { .. }
            | Action::Capital { .. }
            | Action::Selftest { .. } => Role::Owner,
            Action::Invest(op) => op.min_role(),
            _ => Role::Admin,
        }
    }
}

/// Число из ответа: «2,5», «2.5 %», «10 USDT».
fn num(s: &str) -> Option<Decimal> {
    let cleaned: String = s
        .trim()
        .trim_end_matches('%')
        .trim_end_matches("USDT")
        .trim()
        .replace(',', ".");
    let v: Decimal = cleaned.parse().ok()?;
    (!v.is_sign_negative() && v.scale() <= 18).then_some(v.normalize())
}

fn nums(s: &str) -> Option<Vec<Decimal>> {
    s.split_whitespace().map(num).collect()
}

/// «-» — убрать личное значение.
fn optional_num(s: &str) -> Option<Option<Decimal>> {
    if s.trim() == "-" {
        Some(None)
    } else {
        num(s).map(Some)
    }
}

fn parse_tiers(s: &str) -> Option<FeeSchedule> {
    let mut tiers = Vec::new();
    for line in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let v = nums(line)?;
        let [from, fee_pct] = v.as_slice() else {
            return None;
        };
        tiers.push(FeeTier {
            from: *from,
            fee_pct: *fee_pct,
        });
    }
    FeeSchedule::new(tiers).ok()
}

/// Текст отказа движка по-русски.
fn refusal_text(inner: &Inner, r: &Refusal) -> String {
    let t = &inner.texts;
    match r {
        Refusal::NotInReview(s) => t.f(
            "refusal.not_in_review",
            &[("state", &inner.state_name(s.as_db_str()))],
        ),
        Refusal::WrongFlow => t.t("refusal.wrong_flow"),
        Refusal::Forbidden(role) => t.f("refusal.forbidden", &[("role", &inner.role_name(*role))]),
        Refusal::ReasonTooShort => t.t("refusal.reason_short"),
        Refusal::EvidenceRequired => t.t("refusal.evidence"),
        Refusal::InFlight(k) => t.f("refusal.in_flight", &[("key", &esc(k))]),
        Refusal::FundsNotReceived => t.t("refusal.not_received"),
        Refusal::Underfunded => t.t("refusal.underfunded"),
        Refusal::AlreadySettled(k) => t.f("refusal.settled", &[("key", &esc(k))]),
        Refusal::PreviousUnknown(k) => t.f("refusal.unknown", &[("key", &esc(k))]),
        Refusal::OverManualLimit { amount, limit } => t.f(
            "refusal.limit",
            &[("amount", &usdt(*amount)), ("limit", &usdt(*limit))],
        ),
        Refusal::Rules(why) => t.f("refusal.rules", &[("why", &esc(why))]),
        Refusal::NoReserve { available, needed } => t.f(
            "refusal.reserve",
            &[("available", &usdt(*available)), ("needed", &usdt(*needed))],
        ),
        Refusal::IntakeAlreadyPosted => t.t("refusal.intake_posted"),
        Refusal::NoIntakeOperation => t.t("refusal.no_intake"),
        Refusal::BadAmount => t.t("refusal.bad_amount"),
        Refusal::NoSettleAttempt => t.t("refusal.no_attempt"),
        Refusal::CheckAlreadyUsed => t.t("refusal.check_used"),
        Refusal::Changed => t.t("refusal.changed"),
    }
}

fn row_json(r: &DirectionRow) -> Value {
    json!({
        "fee_pct": r.fee_pct.to_string(),
        "min_fee": r.min_fee.to_string(),
        "skew_k_pct": r.skew_k_pct.to_string(),
        "fee_floor_pct": r.fee_floor_pct.to_string(),
        "fee_cap_pct": r.fee_cap_pct.to_string(),
        "min_amount": r.min_amount.to_string(),
        "max_amount": r.max_amount.to_string(),
        "new_user_max_amount": r.new_user_max_amount.to_string(),
        "fiat_buffer_pct": r.fiat_buffer_pct.to_string(),
        "fee_mode": r.fee_mode.as_db_str(),
        "fee_tiers": r.fee_tiers.as_ref().map(|s| s.tiers().iter().map(|t| format!("{} {}", t.from, t.fee_pct)).collect::<Vec<_>>()),
    })
}

fn terms_json(t: &UserTerms) -> Value {
    json!({
        "fee_pct": t.fee_pct.map(|v| v.to_string()),
        "min_fee": t.min_fee.map(|v| v.to_string()),
        "min_amount": t.min_amount.map(|v| v.to_string()),
        "max_amount": t.max_amount.map(|v| v.to_string()),
        "newcomer_exempt": t.newcomer_exempt,
    })
}

impl Inner {
    fn retry(&self, staff: &StaffMember, input: Input, hint_key: &str) -> Screen {
        let text = self
            .texts
            .f("common.bad_input", &[("hint", &self.texts.t(hint_key))]);
        self.prompt_with(staff, input, text)
    }

    fn retry_number(&self, staff: &StaffMember, input: Input) -> Screen {
        self.prompt_with(staff, input, self.texts.t("common.bad_number"))
    }

    pub(crate) async fn on_input(
        self: &Arc<Self>,
        staff: &StaffMember,
        input: Input,
        message: &Message,
    ) -> Result<Screen, OpsError> {
        let text = message.text.as_deref().unwrap_or("").trim().to_owned();
        let t = &self.texts;
        let action = match input.clone() {
            Input::Invest(i) => return self.invest_input(staff, i, &text).await,
            Input::SupportReply(client) => return self.support_reply(staff, client, &text).await,
            Input::OrderSearch => return self.find_order(&text).await,
            Input::UserSearch => return self.find_user(&text).await,
            Input::Dir(d, field) => {
                let before = {
                    let mut conn = self.pool.acquire().await?;
                    storage::directions::get(&mut conn, d).await?
                };
                let mut after = before.clone();
                match field {
                    DirField::Fee => {
                        let Some(v) = num(&text).filter(|v| *v < MAX_FEE_PCT) else {
                            return Ok(self.retry_number(staff, input));
                        };
                        // Полоса рычага — вместе с комиссией.
                        let shift = v - before.fee_pct;
                        after.fee_pct = v;
                        after.fee_floor_pct =
                            (before.fee_floor_pct + shift).max(Decimal::ZERO).min(v);
                        after.fee_cap_pct = (before.fee_cap_pct + shift).max(v).min(MAX_FEE_PCT);
                    }
                    DirField::MinFee => {
                        let Some(v) = num(&text) else {
                            return Ok(self.retry_number(staff, input));
                        };
                        after.min_fee = v;
                    }
                    DirField::Lever => {
                        let ok = nums(&text).and_then(|v| match v.as_slice() {
                            [k, floor, cap]
                                if floor <= &before.fee_pct
                                    && &before.fee_pct <= cap
                                    && *cap < MAX_FEE_PCT =>
                            {
                                Some((*k, *floor, *cap))
                            }
                            _ => None,
                        });
                        let Some((k, floor, cap)) = ok else {
                            return Ok(self.retry(staff, input, "dir.hint_lever"));
                        };
                        after.skew_k_pct = k;
                        after.fee_floor_pct = floor;
                        after.fee_cap_pct = cap;
                    }
                    DirField::Limits => {
                        let ok = nums(&text).and_then(|v| match v.as_slice() {
                            [min, max] if *min > Decimal::ZERO && max >= min => Some((*min, *max)),
                            _ => None,
                        });
                        let Some((min, max)) = ok else {
                            return Ok(self.retry(staff, input, "dir.hint_limits"));
                        };
                        after.min_amount = min;
                        after.max_amount = max;
                        after.new_user_max_amount = before.new_user_max_amount.min(max);
                    }
                    DirField::Newcomer => {
                        let Some(v) =
                            num(&text).filter(|v| *v <= before.max_amount && *v > Decimal::ZERO)
                        else {
                            return Ok(self.retry(staff, input, "dir.hint_newcomer"));
                        };
                        after.new_user_max_amount = v;
                    }
                    DirField::Tiers => {
                        let Some(s) = parse_tiers(&text) else {
                            return Ok(self.retry(staff, input, "dir.hint_tiers"));
                        };
                        after.fee_tiers = Some(s);
                    }
                    DirField::Fiat => {
                        let Some(v) = num(&text).filter(|v| *v < MAX_FEE_PCT) else {
                            return Ok(self.retry_number(staff, input));
                        };
                        after.fiat_buffer_pct = v;
                    }
                }
                Action::Direction { before, after }
            }
            Input::PromoRate => {
                let mut conn = self.pool.acquire().await?;
                let before = engine::fees::promo_settings(&mut conn).await?;
                let after = match nums(&text).as_deref() {
                    Some([fee, min]) => PromoSettings {
                        fee_pct: *fee,
                        min_fee: *min,
                        ..before
                    },
                    _ => return Ok(self.retry(staff, input, "promo.hint_rate")),
                };
                if after.validate().is_err() {
                    return Ok(self.retry(staff, input, "promo.hint_rate"));
                }
                Action::PromoSettings { before, after }
            }
            Input::PromoThresholds => {
                let mut conn = self.pool.acquire().await?;
                let before = engine::fees::promo_settings(&mut conn).await?;
                let after = match nums(&text).as_deref() {
                    Some([trigger, release]) => PromoSettings {
                        trigger_pct: *trigger,
                        release_pct: *release,
                        ..before
                    },
                    _ => return Ok(self.retry(staff, input, "promo.hint_thresholds")),
                };
                if after.validate().is_err() {
                    return Ok(self.retry(staff, input, "promo.hint_thresholds"));
                }
                Action::PromoSettings { before, after }
            }
            Input::Target(platform) => {
                let Some(target) = num(&text).filter(|v| *v > Decimal::ZERO) else {
                    return Ok(self.retry(staff, input, "promo.hint_target"));
                };
                Action::Target { platform, target }
            }
            Input::DecisionAmount { order } => {
                let Some(amount) = num(&text).filter(|v| *v > Decimal::ZERO) else {
                    return Ok(self.retry_number(staff, input));
                };
                let text = t.f(
                    "order.ask_reason",
                    &[
                        ("action", &self.decision_name(DecisionKind::Intake)),
                        ("id", &order),
                    ],
                );
                return Ok(self.prompt_with(
                    staff,
                    Input::DecisionReason {
                        order,
                        kind: DecisionKind::Intake,
                        amount: Some(amount),
                    },
                    text,
                ));
            }
            Input::DecisionReason {
                order,
                kind,
                amount,
            } => {
                if text.chars().count() < 3 {
                    return Ok(self.prompt_with(staff, input, t.t("refusal.reason_short")));
                }
                if kind == DecisionKind::ClosePaid {
                    return Ok(self.prompt(
                        staff,
                        Input::DecisionEvidence {
                            order,
                            reason: text,
                        },
                        "order.ask_evidence",
                    ));
                }
                Action::Decide {
                    order,
                    kind,
                    reason: text,
                    evidence: None,
                    amount,
                }
            }
            Input::DecisionEvidence { order, reason } => {
                if text.chars().count() < 5 {
                    return Ok(self.prompt_with(staff, input, t.t("refusal.evidence")));
                }
                Action::Decide {
                    order,
                    kind: DecisionKind::ClosePaid,
                    reason,
                    evidence: Some(text),
                    amount: None,
                }
            }
            Input::Term(user, d, field) => {
                let Some(value) = optional_num(&text) else {
                    return Ok(self.retry(staff, input, "user.hint_number_or_dash"));
                };
                let (before, dir) = {
                    let mut conn = self.pool.acquire().await?;
                    (
                        storage::user_terms::get(&mut conn, user, d).await?,
                        storage::directions::get(&mut conn, d).await?,
                    )
                };
                let mut after = before.clone();
                match field {
                    TermField::Fee => {
                        if value.is_some_and(|v| v >= MAX_FEE_PCT) {
                            return Ok(self.retry_number(staff, input));
                        }
                        after.fee_pct = value;
                    }
                    TermField::MinFee => after.min_fee = value,
                    TermField::Min => after.min_amount = value.filter(|v| *v > Decimal::ZERO),
                    TermField::Max => after.max_amount = value.filter(|v| *v > Decimal::ZERO),
                }
                // Личные минимум и максимум не должны перекрыть друг друга.
                let eff = after.apply(&dir);
                if eff.min_amount > eff.max_amount {
                    return Ok(self.retry(staff, input, "dir.hint_limits"));
                }
                Action::Terms {
                    user,
                    before,
                    after,
                }
            }
            Input::Daily(user) => {
                let Some(limit) = optional_num(&text) else {
                    return Ok(self.retry(staff, input, "user.hint_number_or_dash"));
                };
                Action::Daily { user, limit }
            }
            Input::Ban(user) => {
                let parsed = text.split_once(';').and_then(|(reason, days)| {
                    let reason = reason.trim();
                    let days: u32 = days.trim().parse().ok()?;
                    (reason.chars().count() >= 3).then(|| (reason.to_owned(), days))
                });
                let Some((reason, days)) = parsed else {
                    return Ok(self.retry(staff, input, "user.hint_ban"));
                };
                Action::Ban { user, reason, days }
            }
            Input::Unban(user) => {
                if text.chars().count() < 3 {
                    return Ok(self.prompt_with(staff, input, t.t("refusal.reason_short")));
                }
                Action::Unban { user, reason: text }
            }
            Input::Note(user) => Action::Note {
                user,
                note: (text != "-" && !text.is_empty()).then(|| text.chars().take(500).collect()),
            },
            Input::EmojiFor(role) => {
                if text == "-" {
                    Action::CustomEmoji { role, id: None }
                } else {
                    let id = message
                        .entities
                        .iter()
                        .find(|e| e.kind == EntityKind::CustomEmoji)
                        .and_then(|e| e.custom_emoji_id.clone())
                        .filter(|id| ui::is_custom_emoji_id(id));
                    let Some(id) = id else {
                        return Ok(self.prompt_with(staff, input, t.t("settings.no_emoji")));
                    };
                    Action::CustomEmoji { role, id: Some(id) }
                }
            }
            Input::StaffAdd => {
                let mut parts = text.splitn(3, ' ');
                let parsed = (|| {
                    let id: i64 = parts.next()?.trim().parse().ok()?;
                    let role = match parts.next()?.trim() {
                        "admin" => Role::Admin,
                        "operator" => Role::Operator,
                        _ => return None,
                    };
                    let name = parts.next()?.trim();
                    (!name.is_empty() && id > 0).then(|| (id, role, name.to_owned()))
                })();
                let Some((id, role, name)) = parsed else {
                    return Ok(self.retry(staff, input, "staff.hint_add"));
                };
                Action::StaffAdd { id, role, name }
            }
        };
        self.preview(staff, action).await
    }

    /// Предпросмотр с «Подтвердить» (nonce) и «Отмена».
    pub(crate) async fn preview(
        self: &Arc<Self>,
        staff: &StaffMember,
        action: Action,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let need = action.min_role();
        let mut unknown_key: Option<String> = None;
        let text = match &action {
            Action::Direction { before, after } => {
                let mut changes = Vec::new();
                let mut change = |field: &str, b: String, a: String| {
                    if a != b {
                        changes.push(t.f(
                            "dir.change",
                            &[
                                ("field", &t.t(field)),
                                ("before", &esc(&b)),
                                ("after", &esc(&a)),
                            ],
                        ));
                    }
                };
                change("dir.f_fee", pct(before.fee_pct), pct(after.fee_pct));
                change("dir.f_min_fee", usdt(before.min_fee), usdt(after.min_fee));
                change(
                    "dir.f_lever",
                    format!(
                        "{} / {} / {}",
                        before.skew_k_pct.normalize(),
                        pct(before.fee_floor_pct),
                        pct(before.fee_cap_pct)
                    ),
                    format!(
                        "{} / {} / {}",
                        after.skew_k_pct.normalize(),
                        pct(after.fee_floor_pct),
                        pct(after.fee_cap_pct)
                    ),
                );
                change(
                    "dir.f_limits",
                    format!("{} – {}", usdt(before.min_amount), usdt(before.max_amount)),
                    format!("{} – {}", usdt(after.min_amount), usdt(after.max_amount)),
                );
                change(
                    "dir.f_newcomer",
                    usdt(before.new_user_max_amount),
                    usdt(after.new_user_max_amount),
                );
                change(
                    "dir.f_tiers",
                    self.tiers_text(before).replace('\n', "; "),
                    self.tiers_text(after).replace('\n', "; "),
                );
                let mode = |m: FeeMode| {
                    t.t(if m == FeeMode::Flat {
                        "dir.mode_flat"
                    } else {
                        "dir.mode_tiers"
                    })
                };
                change("dir.f_mode", mode(before.fee_mode), mode(after.fee_mode));
                change(
                    "dir.f_fiat",
                    pct(before.fiat_buffer_pct),
                    pct(after.fiat_buffer_pct),
                );
                let mut notes = Vec::new();
                notes.push(t.f(
                    "dir.examples_line",
                    &[
                        ("before", &self.examples(before)),
                        ("after", &self.examples(after)),
                    ],
                ));
                if before.fee_floor_pct != after.fee_floor_pct && before.fee_pct != after.fee_pct {
                    notes.push(t.t("dir.band_moved"));
                }
                if let Some(s) = &after.fee_tiers {
                    for i in s.dearer_than_previous() {
                        notes.push(t.f("dir.dearer", &[("from", &usdt(s.tiers()[i].from))]));
                    }
                }
                if need == Role::Owner {
                    notes.push(t.t("dir.big_change"));
                }
                t.f(
                    "dir.preview",
                    &[
                        ("name", &self.dir_name(after.direction)),
                        ("changes", &changes.join("\n")),
                        ("notes", &format!("\n{}", notes.join("\n"))),
                    ],
                )
            }
            Action::DirOpen { direction, open } => t.f(
                "dir.preview",
                &[
                    ("name", &self.dir_name(*direction)),
                    (
                        "changes",
                        &t.f(
                            "dir.change",
                            &[
                                ("field", &t.t("dir.f_open")),
                                ("before", &self.onoff(!open)),
                                ("after", &self.onoff(*open)),
                            ],
                        ),
                    ),
                    ("notes", &""),
                ],
            ),
            Action::DirClearAuto { direction } => t.f(
                "dir.preview",
                &[
                    ("name", &self.dir_name(*direction)),
                    ("changes", &t.t("dir.b_clear_auto")),
                    ("notes", &""),
                ],
            ),
            Action::PromoSettings { before, after } => {
                let mut changes = Vec::new();
                if before.mode != after.mode {
                    changes.push(t.f(
                        "dir.change",
                        &[
                            ("field", &t.t("promo.f_mode")),
                            ("before", &self.promo_mode_name(before.mode)),
                            ("after", &self.promo_mode_name(after.mode)),
                        ],
                    ));
                }
                if (before.fee_pct, before.min_fee) != (after.fee_pct, after.min_fee) {
                    changes.push(t.f(
                        "dir.change",
                        &[
                            ("field", &t.t("promo.f_rate")),
                            (
                                "before",
                                &format!("{} / {}", pct(before.fee_pct), usdt(before.min_fee)),
                            ),
                            (
                                "after",
                                &format!("{} / {}", pct(after.fee_pct), usdt(after.min_fee)),
                            ),
                        ],
                    ));
                }
                if (before.trigger_pct, before.release_pct)
                    != (after.trigger_pct, after.release_pct)
                {
                    changes.push(t.f(
                        "dir.change",
                        &[
                            ("field", &t.t("promo.f_thresholds")),
                            (
                                "before",
                                &format!(
                                    "{} / {}",
                                    pct(before.trigger_pct),
                                    pct(before.release_pct)
                                ),
                            ),
                            (
                                "after",
                                &format!("{} / {}", pct(after.trigger_pct), pct(after.release_pct)),
                            ),
                        ],
                    ));
                }
                t.f("promo.preview", &[("changes", &changes.join("\n"))])
            }
            Action::PromoSwitch {
                direction,
                on,
                room,
            } => {
                let fee = {
                    let mut conn = self.pool.acquire().await?;
                    engine::fees::promo_settings(&mut conn).await?.fee_pct
                };
                if *on {
                    t.f(
                        "promo.switch_on",
                        &[
                            ("name", &self.dir_name(*direction)),
                            ("fee", &pct(fee)),
                            ("room", &usdt(*room)),
                        ],
                    )
                } else {
                    t.f("promo.switch_off", &[("name", &self.dir_name(*direction))])
                }
            }
            Action::Target { platform, target } => t.f(
                "promo.preview",
                &[(
                    "changes",
                    &t.f(
                        "dir.change",
                        &[
                            (
                                "field",
                                &t.f(
                                    "promo.f_target",
                                    &[("name", &self.platform_name(*platform))],
                                ),
                            ),
                            ("before", &"…"),
                            ("after", &usdt(*target)),
                        ],
                    ),
                )],
            ),
            Action::Decide {
                order,
                kind,
                reason,
                evidence,
                amount,
            } => {
                let mut extra = String::new();
                if let Some(e) = evidence {
                    extra.push_str(&t.f("order.preview_evidence", &[("evidence", &esc(e))]));
                }
                if let Some(a) = amount {
                    extra.push_str(&t.f("order.preview_amount", &[("amount", &usdt(*a))]));
                }
                let card = {
                    let mut conn = self.pool.acquire().await?;
                    storage::admin::order_card(&mut conn, *order).await.ok()
                };
                if matches!(
                    kind,
                    DecisionKind::RetryPayout | DecisionKind::RetryInvoice | DecisionKind::Refund
                ) {
                    unknown_key = card.as_ref().and_then(|c| {
                        c.operations
                            .iter()
                            .find(|o| {
                                o.status == "unknown"
                                    && matches!(
                                        o.kind.as_str(),
                                        "create_payout_check"
                                            | "create_refund_check"
                                            | "pay_invoice"
                                    )
                            })
                            .map(|o| o.key.clone())
                    });
                }
                let warning = match (&unknown_key, kind) {
                    (Some(k), _) => t.f("order.warn_unknown", &[("key", &esc(k))]),
                    (None, DecisionKind::Close) => t.t("order.warn_close"),
                    (
                        None,
                        DecisionKind::RetryPayout
                        | DecisionKind::RetryInvoice
                        | DecisionKind::Refund,
                    ) => t.t("order.warn_money"),
                    _ => String::new(),
                };
                t.f(
                    "order.preview",
                    &[
                        ("action", &self.decision_name(*kind)),
                        ("id", order),
                        ("reason", &esc(reason)),
                        ("extra", &extra),
                        ("warning", &warning),
                    ],
                )
            }
            Action::Terms {
                user,
                before,
                after,
            } => {
                let d = after.direction;
                let name = self.dir_name(d);
                let mut changes = Vec::new();
                let fmt_opt = |v: Option<Decimal>, f: fn(Decimal) -> String| {
                    v.map_or_else(|| t.t("user.default"), f)
                };
                let mut change = |key: &str, b: String, a: String| {
                    if a != b {
                        changes.push(t.f(
                            "dir.change",
                            &[
                                ("field", &t.f(key, &[("direction", &name)])),
                                ("before", &esc(&b)),
                                ("after", &esc(&a)),
                            ],
                        ));
                    }
                };
                change(
                    "user.f_fee",
                    fmt_opt(before.fee_pct, pct),
                    fmt_opt(after.fee_pct, pct),
                );
                change(
                    "user.f_min_fee",
                    fmt_opt(before.min_fee, usdt),
                    fmt_opt(after.min_fee, usdt),
                );
                change(
                    "user.f_min",
                    fmt_opt(before.min_amount, usdt),
                    fmt_opt(after.min_amount, usdt),
                );
                change(
                    "user.f_max",
                    fmt_opt(before.max_amount, usdt),
                    fmt_opt(after.max_amount, usdt),
                );
                change(
                    "user.f_exempt",
                    self.onoff(before.newcomer_exempt),
                    self.onoff(after.newcomer_exempt),
                );
                if changes.is_empty() {
                    changes.push(t.t("user.default"));
                }
                t.f(
                    "user.preview",
                    &[("id", user), ("changes", &changes.join("\n"))],
                )
            }
            Action::Daily { user, limit } => t.f(
                "user.preview",
                &[
                    ("id", user),
                    (
                        "changes",
                        &t.f(
                            "dir.change",
                            &[
                                ("field", &t.t("user.f_daily")),
                                ("before", &"…"),
                                (
                                    "after",
                                    &limit.map_or_else(|| t.t("user.daily_default"), usdt),
                                ),
                            ],
                        ),
                    ),
                ],
            ),
            Action::Note { user, note } => t.f(
                "user.preview",
                &[
                    ("id", user),
                    (
                        "changes",
                        &t.f(
                            "dir.change",
                            &[
                                ("field", &t.t("user.f_note")),
                                ("before", &"…"),
                                ("after", &esc(note.as_deref().unwrap_or("—"))),
                            ],
                        ),
                    ),
                ],
            ),
            Action::Ban { user, reason, days } => {
                let until = if *days == 0 {
                    t.t("user.ban_forever")
                } else {
                    t.f(
                        "user.ban_until",
                        &[(
                            "date",
                            &crate::views::when(
                                chrono::Utc::now() + chrono::Duration::days(i64::from(*days)),
                            ),
                        )],
                    )
                };
                t.f(
                    "user.ban_preview",
                    &[("id", user), ("until", &until), ("reason", &esc(reason))],
                )
            }
            Action::Unban { user, reason } => t.f(
                "user.unban_preview",
                &[("id", user), ("reason", &esc(reason))],
            ),
            Action::Flag { flag, on } => {
                let name = t.t(match flag {
                    Flag::Newcomer => "settings.n_newcomer",
                    Flag::Maintenance => "settings.n_maint",
                    Flag::KillSwitch => "settings.n_kill",
                });
                let mut text = t.f(
                    "settings.preview_toggle",
                    &[
                        ("name", &name),
                        ("before", &self.onoff(!on)),
                        ("after", &self.onoff(*on)),
                    ],
                );
                if *flag == Flag::KillSwitch && *on {
                    text.push_str(&t.t("settings.kill_warn"));
                }
                text
            }
            Action::EmojiStyle { before, after } => t.f(
                "settings.preview_toggle",
                &[
                    ("name", &t.t("settings.n_emoji_style")),
                    ("before", &self.style_name(*before)),
                    ("after", &self.style_name(*after)),
                ],
            ),
            Action::CustomEmoji { role, id } => {
                let after = match id {
                    Some(id) => format!(
                        "<tg-emoji emoji-id=\"{}\">{}</tg-emoji>",
                        esc(id),
                        role.plain()
                    ),
                    None => t.t("settings.pack"),
                };
                t.f(
                    "settings.preview_toggle",
                    &[
                        (
                            "name",
                            &t.f("settings.n_custom", &[("name", &self.role_title(*role))]),
                        ),
                        ("before", &"…"),
                        ("after", &after),
                    ],
                )
            }
            Action::BotPause { label, paused, .. } => t.f(
                if *paused {
                    "bots.preview_pause"
                } else {
                    "bots.preview_resume"
                },
                &[("label", &esc(label))],
            ),
            Action::StaffAdd { id, role, name } => t.f(
                "staff.preview_add",
                &[
                    ("name", &esc(name)),
                    ("id", id),
                    ("role", &self.role_name(*role)),
                ],
            ),
            Action::StaffRole {
                name,
                before,
                after,
                ..
            } => t.f(
                "staff.preview_role",
                &[
                    ("name", &esc(name)),
                    ("before", &self.role_name(*before)),
                    ("after", &self.role_name(*after)),
                ],
            ),
            Action::StaffDisable { name, .. } => {
                t.f("staff.preview_disable", &[("name", &esc(name))])
            }
            Action::Capital {
                platform,
                amount,
                deposit,
                ..
            } => t.f(
                if *deposit {
                    "capital.preview_in"
                } else {
                    "capital.preview_out"
                },
                &[
                    ("amount", &usdt(*amount)),
                    ("name", &self.platform_name(*platform)),
                ],
            ),
            Action::Selftest { amount } => t.f("selftest.preview", &[("amount", &usdt(*amount))]),
            Action::Invest(op) => self.invest_preview(op),
        };
        let mut s = Screen::new(text);
        if staff.role >= need {
            let nonce = self.hold(staff.tg_user_id, action);
            let confirm = match unknown_key {
                Some(_) => btn(t.t("order.b_confirm_unknown"), &Cb::ConfirmUnknown(nonce)),
                None => btn(t.t("common.confirm"), &Cb::Confirm(nonce)),
            };
            s = s.row(vec![confirm, btn(t.t("common.cancel"), &Cb::Cancel)]);
        } else {
            s.text.push_str(&format!(
                "\n\n{}",
                t.f("common.not_allowed", &[("role", &self.role_name(need))])
            ));
            s = s.row(vec![btn(t.t("common.cancel"), &Cb::Cancel)]);
        }
        Ok(s)
    }

    /// Применить подтверждённое изменение.
    pub(crate) async fn apply(
        &self,
        staff: &StaffMember,
        action: Action,
        previous_failed: bool,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let staff_id = staff.tg_user_id;
        if let Action::Decide {
            order,
            kind,
            reason,
            evidence,
            amount,
        } = &action
        {
            let decision = match kind {
                DecisionKind::RetryPayout => OperatorDecision::RetryPayout { previous_failed },
                DecisionKind::RetryInvoice => {
                    OperatorDecision::RetryInvoicePayment { previous_failed }
                }
                DecisionKind::Refund => OperatorDecision::Refund { previous_failed },
                DecisionKind::ClosePaid => OperatorDecision::CloseAsPaid {
                    evidence: evidence.clone().unwrap_or_default(),
                },
                DecisionKind::Close => OperatorDecision::CloseWithoutPayout,
                DecisionKind::Intake => OperatorDecision::ConfirmIntake {
                    amount: amount.unwrap_or_default(),
                },
            };
            let result = self
                .engine
                .operator_decide(OperatorRequest {
                    order_id: *order,
                    decision,
                    staff,
                    reason,
                    double_confirmed: true,
                })
                .await;
            let head = match result {
                Ok(o) => t.f(
                    "order.done",
                    &[
                        ("id", order),
                        ("state", &self.state_name(o.state.as_db_str())),
                    ],
                ),
                Err(OperatorError::Refused(r)) => {
                    t.f("order.refusal", &[("why", &refusal_text(self, &r))])
                }
                Err(OperatorError::Engine(e)) => {
                    tracing::error!(order, error = %e, "operator decision failed");
                    t.f("common.error", &[("error", &esc(&e.to_string()))])
                }
            };
            let card = self.order(Some(staff), *order).await?;
            return Ok(card.notice(&head));
        }

        if let Action::Selftest { amount } = &action {
            return self.run_selftest(*amount).await;
        }
        if let Action::Invest(op) = action {
            return self.invest_apply(staff, op).await;
        }

        let mut tx = self.pool.begin().await?;
        let (audit_action, target_type, target_id, before, after, back): (
            &str,
            &str,
            String,
            Option<Value>,
            Option<Value>,
            Cb,
        ) = match &action {
            Action::Direction { before, after } => {
                storage::directions::save_terms(&mut tx, after, staff_id).await?;
                (
                    "direction.set",
                    "direction",
                    after.direction.code().to_owned(),
                    Some(row_json(before)),
                    Some(row_json(after)),
                    Cb::Dir(after.direction),
                )
            }
            Action::DirOpen { direction, open } => {
                storage::directions::set_open(&mut tx, *direction, *open).await?;
                (
                    if *open {
                        "direction.open"
                    } else {
                        "direction.close"
                    },
                    "direction",
                    direction.code().to_owned(),
                    Some(json!({ "is_open": !open })),
                    Some(json!({ "is_open": open })),
                    Cb::Dir(*direction),
                )
            }
            Action::DirClearAuto { direction } => {
                storage::directions::clear_auto_pause(&mut tx, *direction, staff_id).await?;
                (
                    "direction.clear_auto_pause",
                    "direction",
                    direction.code().to_owned(),
                    None,
                    None,
                    Cb::Dir(*direction),
                )
            }
            Action::PromoSettings { before, after } => {
                engine::fees::save_promo_settings(&mut tx, after, staff_id).await?;
                (
                    "promo.settings",
                    "settings",
                    engine::fees::PROMO_SETTINGS.to_owned(),
                    serde_json::to_value(before).ok(),
                    serde_json::to_value(after).ok(),
                    Cb::Promo,
                )
            }
            Action::PromoSwitch { direction, on, .. } => {
                engine::fees::switch_promo(&mut tx, *direction, *on, staff_id).await?;
                if *on {
                    storage::alerts::resolve(&mut tx, &format!("promo_hint:{}", direction.code()))
                        .await?;
                }
                (
                    "promo.switch",
                    "direction",
                    direction.code().to_owned(),
                    None,
                    Some(json!({ "on": on })),
                    Cb::Promo,
                )
            }
            Action::Target { platform, target } => {
                let Some((wallet, _)) = storage::wallets::active(&mut tx, *platform).await? else {
                    return Ok(Screen::new(t.t("dash.no_wallets"))
                        .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                };
                let before = storage::admin::thresholds(&mut tx, wallet, Asset::Usdt)
                    .await?
                    .map(|th| th.target.to_string());
                storage::admin::set_thresholds(
                    &mut tx,
                    wallet,
                    Asset::Usdt,
                    storage::admin::Thresholds::from_target(*target),
                )
                .await?;
                (
                    "reserve.target",
                    "wallet",
                    platform.db_name().to_owned(),
                    Some(json!({ "target": before })),
                    Some(json!({ "target": target.to_string() })),
                    Cb::Promo,
                )
            }
            Action::Terms {
                user,
                before,
                after,
            } => {
                storage::user_terms::save(&mut tx, *user, after, staff_id).await?;
                (
                    "user.terms",
                    "user",
                    format!("{user}:{}", after.direction.code()),
                    Some(terms_json(before)),
                    Some(terms_json(after)),
                    Cb::UserDir(*user, after.direction),
                )
            }
            Action::Daily { user, limit } => {
                let before = storage::admin::daily_limit(&mut tx, *user).await?;
                storage::admin::set_daily_limit(&mut tx, *user, *limit).await?;
                (
                    "user.daily_limit",
                    "user",
                    user.to_string(),
                    Some(json!({ "limit": before.map(|v| v.to_string()) })),
                    Some(json!({ "limit": limit.map(|v| v.to_string()) })),
                    Cb::User(*user),
                )
            }
            Action::Note { user, note } => {
                storage::admin::set_note(&mut tx, *user, note.as_deref()).await?;
                (
                    "user.note",
                    "user",
                    user.to_string(),
                    None,
                    Some(json!({ "note": note })),
                    Cb::User(*user),
                )
            }
            Action::Ban { user, reason, days } => {
                let expires = (*days > 0)
                    .then(|| chrono::Utc::now() + chrono::Duration::days(i64::from(*days)));
                if let Err(e) = storage::admin::ban(&mut tx, *user, reason, expires, staff_id).await
                {
                    tracing::warn!(user, error = %e, "ban failed");
                    return Ok(Screen::new(
                        t.f("common.error", &[("error", &esc(&e.to_string()))]),
                    )
                    .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                }
                (
                    "user.ban",
                    "user",
                    user.to_string(),
                    None,
                    Some(json!({ "days": days, "reason": reason })),
                    Cb::User(*user),
                )
            }
            Action::Unban { user, reason } => {
                storage::admin::unban(&mut tx, *user, reason, staff_id).await?;
                (
                    "user.unban",
                    "user",
                    user.to_string(),
                    None,
                    Some(json!({ "reason": reason })),
                    Cb::User(*user),
                )
            }
            Action::Flag { flag, on } => {
                let key = flag.key();
                let mut value = storage::settings::get(&mut tx, key)
                    .await?
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
                value["on"] = json!(on);
                storage::settings::set(&mut tx, key, &value, Some(staff_id)).await?;
                (
                    "settings.flag",
                    "settings",
                    key.to_owned(),
                    Some(json!({ "on": !on })),
                    Some(json!({ "on": on })),
                    Cb::Settings,
                )
            }
            Action::EmojiStyle { before, after } => {
                storage::settings::set(&mut tx, "emoji_style", &json!(after.key()), Some(staff_id))
                    .await?;
                (
                    "settings.emoji_style",
                    "settings",
                    "emoji_style".to_owned(),
                    Some(json!(before.key())),
                    Some(json!(after.key())),
                    Cb::Emoji,
                )
            }
            Action::CustomEmoji { role, id } => {
                let mut map = storage::settings::get(&mut tx, "custom_emoji")
                    .await?
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                let before = map.get(role.key()).cloned();
                match id {
                    Some(id) => {
                        map.insert(role.key().to_owned(), json!(id));
                    }
                    None => {
                        map.remove(role.key());
                    }
                }
                storage::settings::set(
                    &mut tx,
                    "custom_emoji",
                    &Value::Object(map),
                    Some(staff_id),
                )
                .await?;
                (
                    "settings.custom_emoji",
                    "emoji",
                    role.key().to_owned(),
                    before,
                    id.as_ref().map(|i| json!(i)),
                    Cb::Emoji,
                )
            }
            Action::BotPause { id, paused, .. } => {
                storage::admin::set_userbot_paused(&mut tx, *id, *paused).await?;
                (
                    if *paused {
                        "userbot.pause"
                    } else {
                        "userbot.resume"
                    },
                    "userbot",
                    id.to_string(),
                    None,
                    None,
                    Cb::Bots,
                )
            }
            Action::StaffAdd { id, role, name } => {
                storage::staff::add(&mut tx, *id, *role, name, staff_id).await?;
                (
                    "staff.add",
                    "staff",
                    id.to_string(),
                    None,
                    Some(json!({ "role": role.as_db_str(), "name": name })),
                    Cb::Staff,
                )
            }
            Action::StaffRole {
                id, before, after, ..
            } => {
                storage::staff::set_role(&mut tx, *id, *after).await?;
                (
                    "staff.role",
                    "staff",
                    id.to_string(),
                    Some(json!(before.as_db_str())),
                    Some(json!(after.as_db_str())),
                    Cb::Staff,
                )
            }
            Action::StaffDisable { id, .. } => {
                storage::staff::disable(&mut tx, *id).await?;
                (
                    "staff.disable",
                    "staff",
                    id.to_string(),
                    None,
                    None,
                    Cb::Staff,
                )
            }
            Action::Capital {
                platform,
                amount,
                deposit,
                alert,
            } => {
                // Событие должно быть ещё открыто: иначе разницу уже провели или она ушла.
                let open = storage::alerts::get(&mut tx, *alert)
                    .await?
                    .is_some_and(|a| a.acked_at.is_none());
                if !open {
                    return Ok(Screen::new(t.t("capital.gone"))
                        .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                }
                let posting = if *deposit {
                    domain::ledger::capital_in(*platform, Asset::Usdt, *amount)
                } else {
                    domain::ledger::capital_out(*platform, Asset::Usdt, *amount)
                };
                let Ok(posting) = posting else {
                    return Ok(Screen::new(t.t("refusal.bad_amount"))
                        .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                };
                let created_by = format!("staff:{staff_id}");
                let memo = if *deposit {
                    "взнос владельца по сверке"
                } else {
                    "вывод владельца по сверке"
                };
                storage::ledger::post(
                    &mut tx,
                    &posting,
                    storage::ledger::PostingRefs {
                        memo: Some(memo),
                        created_by: Some(&created_by),
                        ..Default::default()
                    },
                )
                .await?;
                storage::alerts::ack(&mut tx, *alert, staff_id).await?;
                (
                    if *deposit {
                        "capital.in"
                    } else {
                        "capital.out"
                    },
                    "wallet",
                    platform.db_name().to_owned(),
                    None,
                    Some(json!({ "amount": amount.to_string() })),
                    Cb::Dash,
                )
            }
            // Решения по заявке и самопроверка применены выше.
            Action::Decide { .. } | Action::Selftest { .. } | Action::Invest(_) => {
                return self.menu(staff).await;
            }
        };
        storage::audit::record(
            &mut tx,
            &storage::audit::NewAudit {
                staff_id,
                action: audit_action,
                target_type: Some(target_type),
                target_id: Some(&target_id),
                before,
                after,
                reason: None,
                double_confirmed: true,
            },
        )
        .await?;
        tx.commit().await?;
        tracing::info!(staff = staff_id, action = audit_action, target = %target_id, "ops change applied");
        let screen = match back {
            Cb::Dir(d) => self.dir_card(staff, d).await?,
            Cb::Promo => self.promo().await?,
            Cb::UserDir(id, d) => self.user_dir(id, d).await?,
            Cb::User(id) => self.user(Some(staff), id).await?,
            Cb::Settings => self.settings().await?,
            Cb::Emoji => self.emoji().await?,
            Cb::Bots => self.bots().await?,
            Cb::Staff => self.staff_list(staff).await?,
            Cb::Dash => self.dash().await?,
            _ => self.menu(staff).await?,
        };
        Ok(screen.notice(&t.t("common.saved")))
    }
}

impl Inner {
    async fn run_selftest(&self, amount: Decimal) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let steps = match self.engine.selftest(amount).await {
            Ok(steps) => steps,
            Err(engine::EngineError::Invariant(_)) => {
                return Ok(
                    Screen::new(t.t("selftest.busy")).row(vec![btn(t.t("common.home"), &Cb::Menu)])
                );
            }
            Err(e) => return Err(e.into()),
        };
        let mut lines = vec![t.t("selftest.title")];
        for s in &steps {
            lines.push(t.f(
                "selftest.step",
                &[
                    ("mark", &if s.ok { "✅" } else { "❌" }),
                    ("name", &self.platform_name(s.platform)),
                    ("step", &t.t(&format!("selftest.s_{}", s.step))),
                    ("detail", &esc(&s.detail)),
                ],
            ));
        }
        lines.push(String::new());
        let all_ok = !steps.is_empty() && steps.iter().all(|s| s.ok);
        lines.push(t.t(if all_ok {
            "selftest.all_ok"
        } else {
            "selftest.failed"
        }));
        Ok(Screen::new(lines.join("\n")).row(vec![
            btn(t.t("menu.dash"), &Cb::Dash),
            btn(t.t("common.home"), &Cb::Menu),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn numbers_and_tiers_parse_like_people_type_them() {
        assert_eq!(num("2,5"), Some(dec!(2.5)));
        assert_eq!(num(" 2.5 % "), Some(dec!(2.5)));
        assert_eq!(num("10 USDT"), Some(dec!(10)));
        assert_eq!(num("-1"), None);
        assert_eq!(num("abc"), None);
        assert_eq!(optional_num("-"), Some(None));
        let s = parse_tiers("0 2.5\n100 1,8\n\n250 1.2").unwrap();
        assert_eq!(s.tiers().len(), 3);
        assert!(parse_tiers("10 2.5").is_none(), "первая ступень — с нуля");
        assert!(parse_tiers("0 2.5\n0 1").is_none());
    }
}
