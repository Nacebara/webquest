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

/// Сколько букв надписи помещается на кнопке рядом с «Отмена» в телефоне.
const LONG_BUTTON: usize = 16;

/// Чего ждём от сотрудника.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Input {
    /// Контрольная строка (шаг 3, макет «Контрольная строка»): самое опасное действие ждёт,
    /// пока сотрудник введёт номер заявки или «ПОДТВЕРЖДАЮ».
    Control {
        action: Box<Action>,
        expected: String,
        previous_failed: bool,
    },
    OrderSearch,
    /// «Текст для клиентов» на время обслуживания (решение владельца 18.11, вариант «а»).
    MaintText,
    UserSearch,
    Dir(Direction, DirField),
    PromoRate,
    PromoThresholds,
    Target(Platform),
    /// Лимит без код-пароля CryptoBot, $ (Д-19.10).
    PasscodeLimit,
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
        kind: DecisionKind,
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
    /// Причина «Списать как потерю» по событию недостачи (id события).
    WriteOffReason(i64),
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
        /// Только пауза «незнакомый ответ» кошелька приёма (кнопка тревоги, Д-19.9); `false` —
        /// все причины (кнопка в карточке направления).
        only_unknown: bool,
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
    /// Рассылка «🔥 Выгодно» клиентам: вкл / выкл; `recipients` — сколько получат сейчас.
    PromoBroadcast {
        on: bool,
        recipients: i64,
    },
    Target {
        platform: Platform,
        target: Decimal,
    },
    /// Лимит без код-пароля CryptoBot: было → стало (Д-19.10).
    PasscodeLimit {
        before: Option<Decimal>,
        after: Decimal,
    },
    Decide {
        order: i64,
        kind: DecisionKind,
        reason: String,
        evidence: Option<String>,
        amount: Option<Decimal>,
        /// Сумма по чеку (для «Деньги пришли»): расхождение больше 1 % — решает владелец.
        check: Option<Decimal>,
    },
    Terms {
        user: i64,
        before: UserTerms,
        after: UserTerms,
        /// Обычная ставка направления — для правила «больше 1 п.п. — владелец».
        base_pct: Decimal,
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
    /// «Текст для клиентов» на время обслуживания: строка под «Обновляемся до 15:30».
    MaintText {
        before: Option<String>,
        after: Option<String>,
    },
    /// Обслуживание со сроком (П-13): «Обслуживание 30 мин», «+15 мин». Было — для предпросмотра.
    Maintain {
        before_on: bool,
        before_until: Option<chrono::DateTime<chrono::Utc>>,
        until: chrono::DateTime<chrono::Utc>,
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
    /// Недостачу признать потерей (решение владельца 17.11, `docs/INVEST-LOSSES.md`, строки 6–7):
    /// проводка `loss`, событие закрывается.
    WriteOff {
        platform: Platform,
        amount: Decimal,
        alert: i64,
        reason: String,
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
    let one = Decimal::ONE;
    // Рычаг тоже двигает итог для клиента: пол, потолок и сила (шаг 3 — «итоговая ставка»).
    (after.fee_floor_pct - before.fee_floor_pct).abs() > one
        || (after.fee_cap_pct - before.fee_cap_pct).abs() > one
        || (after.skew_k_pct - before.skew_k_pct).abs() > one
        || amounts
            .into_iter()
            .any(|a| (base(after, a) - base(before, a)).abs() > one)
}

/// Приход отличается от суммы чека больше чем на 1 %.
pub(crate) fn off_check(amount: Decimal, check: Decimal) -> bool {
    check > Decimal::ZERO && (amount - check).abs() * Decimal::ONE_HUNDRED > check
}

/// Личная ставка: сдвиг больше 1 п.п. или ниже обычной больше чем на 1 п.п. — решает владелец.
fn personal_big(before: &UserTerms, after: &UserTerms, base: Decimal) -> bool {
    if before.fee_pct == after.fee_pct {
        return false;
    }
    let old = before.fee_pct.unwrap_or(base);
    let new = after.fee_pct.unwrap_or(base);
    (new - old).abs() > Decimal::ONE || base - new > Decimal::ONE
}

impl Action {
    pub(crate) fn min_role(&self) -> Role {
        match self {
            Action::Direction { before, after } if big_change(before, after) => Role::Owner,
            Action::Terms {
                before,
                after,
                base_pct,
                ..
            } if personal_big(before, after, *base_pct) => Role::Owner,
            Action::PromoSettings { before, after }
                if (after.fee_pct - before.fee_pct).abs() > Decimal::ONE =>
            {
                Role::Owner
            }
            Action::Decide {
                kind: DecisionKind::Intake,
                amount: Some(a),
                check: Some(c),
                ..
            } if off_check(*a, *c) => Role::Owner,
            Action::Decide { kind, .. } => kind.min_role(),
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
            | Action::WriteOff { .. }
            | Action::PromoBroadcast { .. }
            | Action::PasscodeLimit { .. }
            | Action::Selftest { .. } => Role::Owner,
            Action::Invest(op) => op.min_role(),
            _ => Role::Admin,
        }
    }
}

/// Что нужно ввести после «Подтвердить» (макет «Контрольная строка», шаг 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    /// Номер заявки (публичный).
    Order(i64),
    /// Слово «ПОДТВЕРЖДАЮ».
    Word,
}

/// Слово контрольной строки.
pub(crate) const CONTROL_WORD: &str = "ПОДТВЕРЖДАЮ";

/// Сообщение — слово контрольной строки (в любом регистре, с пробелами по краям).
pub(crate) fn is_control_word(text: &str) -> bool {
    text.trim().to_uppercase() == CONTROL_WORD
}

impl Action {
    /// Самые опасные действия — с контрольной строкой: закрыть без выплаты, «прежней попытки
    /// не было» (неясный исход), снять стоп-кран.
    pub(crate) fn control(&self, previous_failed: bool) -> Option<Control> {
        match self {
            Action::Decide {
                order,
                kind: DecisionKind::Close,
                ..
            } => Some(Control::Order(*order)),
            Action::Decide { order, .. } if previous_failed => Some(Control::Order(*order)),
            Action::Flag {
                flag: Flag::KillSwitch,
                on: false,
            } => Some(Control::Word),
            _ => None,
        }
    }

    /// Необратимое — красная кнопка подтверждения.
    pub(crate) fn irreversible(&self) -> bool {
        matches!(
            self,
            Action::Decide { .. }
                | Action::Capital { .. }
                | Action::WriteOff { .. }
                | Action::StaffDisable { .. }
                | Action::Flag {
                    flag: Flag::KillSwitch,
                    ..
                }
        )
    }
}

/// Строка предпросмотра стоп-крана (П-13): «В очереди 3 выплаты на 412.50 USDT — встанут…».
fn kill_warn(t: &crate::Texts, queued: &[(domain::Asset, i64, Decimal)]) -> String {
    if queued.is_empty() {
        return t.t("settings.kill_warn_none");
    }
    let what = queued
        .iter()
        .map(|(asset, n, sum)| {
            t.f(
                "settings.kill_queued",
                &[
                    (
                        "n",
                        &format!(
                            "{n} {}",
                            ui::fmt::plural(*n, ["выплата", "выплаты", "выплат"])
                        ),
                    ),
                    ("sum", &crate::dash::amount(*sum)),
                    ("asset", &asset.code()),
                ],
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    t.f("settings.kill_warn", &[("what", &what)])
}

/// Недостача по событию сверки, ещё не закрытому: кошелёк и сколько не хватает.
pub(crate) async fn shortage_of(
    conn: &mut sqlx::PgConnection,
    alert: i64,
) -> Result<Option<(Platform, Decimal)>, OpsError> {
    let Some(a) = storage::alerts::get(conn, alert)
        .await?
        .filter(|a| a.acked_at.is_none() && a.title == "balance_mismatch")
    else {
        return Ok(None);
    };
    let platform = a.payload["platform"]
        .as_str()
        .and_then(Platform::from_db_name);
    let diff: Option<Decimal> = a.payload["diff"].as_str().and_then(|d| d.parse().ok());
    Ok(match (platform, diff) {
        (Some(p), Some(d)) if d < Decimal::ZERO => Some((p, -d)),
        _ => None,
    })
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
        Refusal::PreviousDone(k) => t.f("refusal.previous_done", &[("key", &esc(k))]),
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
        Refusal::NotMeasured => t.t("refusal.not_measured"),
        Refusal::OverWalletExcess { amount, room } => t.f(
            "refusal.over_excess",
            &[
                ("amount", &usdt(*amount)),
                ("room", &usdt((*room).max(Decimal::ZERO))),
            ],
        ),
        Refusal::NoSettleAttempt => t.t("refusal.no_attempt"),
        Refusal::CheckAlreadyUsed => t.t("refusal.check_used"),
        Refusal::Changed => t.t("refusal.changed"),
        Refusal::CheckLinkRequired(p) => t.f(
            "refusal.check_link",
            &[("wallet", &inner.platform_name(*p))],
        ),
        Refusal::NoStuckExcess => t.t("refusal.no_stuck_excess"),
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

    /// Контрольная строка: совпало (без учёта регистра и пробелов по краям) — применить, права
    /// проверяются заново; нет — ничего не делать.
    async fn control_input(
        self: &Arc<Self>,
        staff: &StaffMember,
        action: Action,
        expected: &str,
        previous_failed: bool,
        text: &str,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        if text.trim().to_uppercase() != expected.to_uppercase() {
            return Ok(Screen::new(t.t("common.control_mismatch"))
                .row(vec![btn(t.t("nav.menu"), &Cb::Menu)]));
        }
        let Some(staff) = self.staff(staff.tg_user_id).await? else {
            return Ok(Screen::new(t.t("common.expired")));
        };
        let need = if previous_failed {
            action.min_role().max(Role::Admin)
        } else {
            action.min_role()
        };
        if let Some(denied) = self.deny(&staff, need) {
            return Ok(denied);
        }
        self.apply(&staff, action, previous_failed).await
    }

    /// П-13: тревога «Стоп-кран» говорит «Выключит владелец: введите ПОДТВЕРЖДАЮ» — слово без
    /// вопроса пульта снимает стоп-кран (только владелец; аудит — с двойным подтверждением).
    /// Стоп-кран не включён — подтверждать нечего: меню с пометкой.
    pub(crate) async fn bare_control_word(
        self: &Arc<Self>,
        staff: &StaffMember,
    ) -> Result<Screen, OpsError> {
        let on = {
            let mut conn = self.pool.acquire().await?;
            storage::settings::flag(&mut conn, Flag::KillSwitch.key(), false).await?
        };
        if !on {
            let notice = self.texts.t("common.nothing_to_confirm");
            return Ok(self.menu(staff).await?.notice(&notice));
        }
        let action = Action::Flag {
            flag: Flag::KillSwitch,
            on: false,
        };
        if let Some(denied) = self.deny(staff, action.min_role()) {
            return Ok(denied);
        }
        self.apply(staff, action, false).await
    }

    /// Шаг «введите …» перед самым опасным действием; не нужен — `None`.
    pub(crate) async fn control_prompt(
        &self,
        staff: &StaffMember,
        action: &Action,
        previous_failed: bool,
    ) -> Result<Option<Screen>, OpsError> {
        let t = &self.texts;
        let (expected, text) = match action.control(previous_failed) {
            None => return Ok(None),
            Some(Control::Word) => (CONTROL_WORD.to_owned(), t.t("common.type_word")),
            Some(Control::Order(order)) => {
                let public = {
                    let mut conn = self.pool.acquire().await?;
                    storage::admin::order_card(&mut conn, order)
                        .await?
                        .public_id
                };
                let text = t.f("common.type_order", &[("public", &esc(&public))]);
                (public, text)
            }
        };
        Ok(Some(self.prompt_with(
            staff,
            Input::Control {
                action: Box::new(action.clone()),
                expected,
                previous_failed,
            },
            text,
        )))
    }

    /// Кошелёк, которым возвращаем клиенту (возврат и переплата — кошелёк входа).
    pub(crate) async fn refund_wallet(&self, order: i64) -> Result<Platform, OpsError> {
        let mut conn = self.pool.acquire().await?;
        Ok(storage::admin::order_card(&mut conn, order)
            .await?
            .direction
            .source())
    }

    /// Макет «Ссылка на чек» (принят 20.11): что сделать и что прислать.
    pub(crate) async fn ask_check_text(
        &self,
        order: i64,
        kind: DecisionKind,
    ) -> Result<String, OpsError> {
        let card = {
            let mut conn = self.pool.acquire().await?;
            storage::admin::order_card(&mut conn, order).await?
        };
        let t = &self.texts;
        Ok(t.f(
            "order.ask_check",
            &[
                (
                    "action",
                    &t.t(if kind == DecisionKind::RefundIssued {
                        "order.n_refund_issued"
                    } else {
                        "order.n_excess_issued"
                    }),
                ),
                ("public", &esc(&card.public_id)),
                ("wallet", &self.platform_name(card.direction.source())),
                (
                    "amount",
                    &card
                        .refund_amount
                        .map_or_else(|| "—".to_owned(), crate::dash::amount),
                ),
            ],
        ))
    }

    /// Сумма по чеку заявки (приход или котировка); нет — `None`.
    pub(crate) async fn check_amount(&self, order: i64) -> Result<Option<Decimal>, OpsError> {
        let mut conn = self.pool.acquire().await?;
        let card = storage::admin::order_card(&mut conn, order).await?;
        Ok(card.intake_amount.or(card.quote_amount_in))
    }

    /// Вопрос причины «Деньги пришли» с уже известной суммой.
    pub(crate) async fn intake_reason(
        &self,
        staff: &StaffMember,
        order: i64,
        amount: Decimal,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        // П-7: везде публичный номер — его называет клиент; внутренний id в тексте не нужен.
        let public = esc(&self.public_id(order).await?);
        let text = t.f(
            "order.ask_reason",
            &[
                ("action", &self.decision_name(DecisionKind::Intake)),
                ("public", &public),
            ],
        );
        Ok(self.prompt_with(
            staff,
            Input::DecisionReason {
                order,
                kind: DecisionKind::Intake,
                amount: Some(amount),
            },
            text,
        ))
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
            Input::Control {
                action,
                expected,
                previous_failed,
            } => {
                return self
                    .control_input(staff, *action, &expected, previous_failed, &text)
                    .await;
            }
            Input::Invest(i) => return self.invest_input(staff, i, &text).await,
            Input::SupportReply(client) => return self.support_reply(staff, client, &text).await,
            Input::OrderSearch => return self.find_order(staff, &text).await,
            Input::UserSearch => return self.find_user(staff, &text).await,
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
            Input::PasscodeLimit => {
                let cleaned: String = text
                    .chars()
                    .filter(|c| !c.is_whitespace() && *c != '$')
                    .collect();
                let Some(after) = num(&cleaned).filter(|v| *v > Decimal::ZERO) else {
                    return Ok(self.retry(staff, input, "reserves.hint_passcode"));
                };
                let before = {
                    let mut conn = self.pool.acquire().await?;
                    engine::passcode_free_limit(&mut conn, self.engine.config()).await?
                };
                Action::PasscodeLimit { before, after }
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
                return self.intake_reason(staff, order, amount).await;
            }
            Input::MaintText => {
                let before = {
                    let mut conn = self.pool.acquire().await?;
                    storage::settings::on_note(&mut conn, Flag::Maintenance.key()).await?
                };
                let after: String = text.chars().take(200).collect();
                let after = (after != "-" && !after.is_empty()).then_some(after);
                Action::MaintText { before, after }
            }
            Input::WriteOffReason(alert) => {
                if text.chars().count() < 3 {
                    return Ok(self.prompt_with(staff, input, t.t("refusal.reason_short")));
                }
                let mut conn = self.pool.acquire().await?;
                let Some((platform, amount)) = shortage_of(&mut conn, alert).await? else {
                    return Ok(Screen::new(t.t("capital.gone"))
                        .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                };
                Action::WriteOff {
                    platform,
                    amount,
                    alert,
                    reason: text,
                }
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
                            kind,
                        },
                        "order.ask_evidence",
                    ));
                }
                if kind.needs_evidence() {
                    // «Возврат выдан» / «Чек выдан» (Д-19.11, Д-19.1; макет «Ссылка на чек»).
                    let ask = self.ask_check_text(order, kind).await?;
                    return Ok(self.prompt_with(
                        staff,
                        Input::DecisionEvidence {
                            order,
                            reason: text,
                            kind,
                        },
                        ask,
                    ));
                }
                let check = if kind == DecisionKind::Intake {
                    self.check_amount(order).await?
                } else {
                    None
                };
                Action::Decide {
                    order,
                    kind,
                    reason: text,
                    evidence: None,
                    amount,
                    check,
                }
            }
            Input::DecisionEvidence {
                order,
                reason,
                kind,
            } => {
                if text.chars().count() < 5 {
                    return Ok(self.prompt_with(staff, input, t.t("refusal.evidence")));
                }
                if kind != DecisionKind::ClosePaid {
                    // Клиент получит ссылку кнопкой: только чек кошелька, которым возвращаем.
                    let wallet = self.refund_wallet(order).await?;
                    let fits = text.split_whitespace().any(|w| {
                        parsers::links::parse_url(w).is_ok_and(|l| {
                            l.kind == parsers::links::LinkKind::Check && l.platform == wallet
                        })
                    });
                    if !fits {
                        let why = t.f(
                            "refusal.check_link",
                            &[("wallet", &self.platform_name(wallet))],
                        );
                        return Ok(self.prompt_with(staff, input, why));
                    }
                }
                Action::Decide {
                    order,
                    kind,
                    reason,
                    evidence: Some(text),
                    amount: None,
                    check: None,
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
                    base_pct: dir.fee_pct,
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
                // Д-19.6: ID владельца с ролью admin понижал владельца — отказ, ничего не меняется.
                let existing = {
                    let mut conn = self.pool.acquire().await?;
                    storage::staff::get(&mut conn, id).await?
                };
                if existing.is_some_and(|m| m.role == Role::Owner) {
                    let text = crate::owner_add_refusal(&self.texts, id);
                    return Ok(self.prompt_with(staff, input, text));
                }
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
        self.preview_for(staff, action, self.cfg.confirm_ttl).await
    }

    /// «Отправить владельцу» (шаг 3): тот же предпросмотр уходит владельцам с пометкой, кто
    /// предлагает; решают они обычным «Подтвердить» (кнопка живёт сутки) или «Отмена».
    pub(crate) async fn ask_owner(
        self: &Arc<Self>,
        staff: &StaffMember,
        nonce: &str,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let Some(action) = self.take_pending(staff.tg_user_id, nonce) else {
            return Ok(
                Screen::new(t.t("common.expired")).row(vec![btn(t.t("nav.menu"), &Cb::Menu)])
            );
        };
        let owners: Vec<StaffMember> = {
            let mut conn = self.pool.acquire().await?;
            storage::staff::list(&mut conn)
                .await?
                .into_iter()
                .filter(|m| m.role == Role::Owner && m.active())
                .collect()
        };
        if owners.is_empty() {
            return Ok(Screen::new(t.t("common.request_no_owner"))
                .row(vec![btn(t.t("nav.menu"), &Cb::Menu)]));
        }
        for owner in &owners {
            let mut screen = self
                .preview_for(
                    owner,
                    action.clone(),
                    std::time::Duration::from_secs(24 * 3600),
                )
                .await?;
            let head = t.f(
                "common.request_from",
                &[("name", &esc(&staff.display_name))],
            );
            screen.text = format!("{head}\n\n{}", screen.text);
            if let Err(e) = self.send(owner.tg_user_id, &screen).await {
                tracing::warn!(owner = owner.tg_user_id, error = %e, "owner request not delivered");
            }
        }
        Ok(Screen::new(t.t("common.request_sent")).row(vec![btn(t.t("nav.menu"), &Cb::Menu)]))
    }

    pub(crate) async fn preview_for(
        self: &Arc<Self>,
        staff: &StaffMember,
        action: Action,
        ttl: std::time::Duration,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let need = action.min_role();
        let mut unknown_key: Option<String> = None;
        // Действие с суммой для кнопки «Проверил — …» при неясном исходе.
        let mut unknown_label: Option<String> = None;
        // Подпись кнопки «Подтвердить» по действию («Повторить выплату 97.50»).
        let mut confirm_label: Option<String> = None;
        // Синяя кнопка — главное действие экрана (макет «Изменить значение»: «Сохранить»).
        let mut confirm_primary = false;
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
            Action::DirClearAuto { direction, .. } => t.f(
                "dir.preview",
                &[
                    ("name", &self.dir_name(*direction)),
                    ("changes", &t.t("dir.b_clear_auto")),
                    ("notes", &""),
                ],
            ),
            Action::PromoBroadcast { on, recipients } => {
                let (before, after) = if *on {
                    (t.t("promo.bc_word_off"), t.t("promo.bc_word_on"))
                } else {
                    (t.t("promo.bc_word_on"), t.t("promo.bc_word_off"))
                };
                t.f(
                    if *on {
                        "promo.bc_preview"
                    } else {
                        "promo.bc_preview_off"
                    },
                    &[
                        ("before", &before),
                        ("after", &after),
                        ("n", &ui::fmt::grouped(Decimal::from(*recipients))),
                    ],
                )
            }
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
            Action::PasscodeLimit { before, after } => {
                confirm_label = Some(t.t("reserves.b_save"));
                confirm_primary = true;
                crate::reserves::passcode_preview(t, *before, *after)
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
                check,
            } => {
                let mut extra = String::new();
                if let (Some(a), Some(c)) = (amount, check)
                    && off_check(*a, *c)
                {
                    extra.push_str(&t.f(
                        "order.intake_mismatch",
                        &[
                            ("amount", &crate::dash::amount(*a)),
                            ("check", &crate::dash::amount(*c)),
                        ],
                    ));
                }
                if let Some(e) = evidence {
                    // Ссылка на чек на предъявителя — под спойлером (правило «Ссылки на чеки в
                    // пульте — под спойлером»).
                    let key = if kind.needs_evidence() && *kind != DecisionKind::ClosePaid {
                        "order.preview_check"
                    } else {
                        "order.preview_evidence"
                    };
                    extra.push_str(&t.f(key, &[("evidence", &esc(e))]));
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
                if *kind == DecisionKind::RetryExcess {
                    unknown_key = card.as_ref().and_then(|c| {
                        c.operations
                            .iter()
                            .find(|o| o.status == "unknown" && o.kind == "create_excess_check")
                            .map(|o| o.key.clone())
                    });
                }
                // Макет «Подтверждение»: что произойдёт, сумма, кошелёк, клиент, публичный
                // номер, прежняя попытка, «Необратимо», 60 с.
                let Some(c) = card else {
                    return Ok(Screen::new(t.t("order.not_found"))
                        .row(vec![btn(t.t("nav.menu"), &Cb::Menu)]));
                };
                let client = c.user_id.to_string();
                let money =
                    |v: Option<Decimal>| v.map_or_else(|| "—".to_owned(), crate::dash::amount);
                let (q, what, wallet, sum) = match kind {
                    DecisionKind::RetryPayout => (
                        "order.q_retry",
                        "order.what_retry",
                        c.direction.target(),
                        c.payout_amount,
                    ),
                    DecisionKind::RetryInvoice => (
                        "order.q_retry_invoice",
                        "order.what_retry_invoice",
                        c.direction.target(),
                        c.invoice_amount,
                    ),
                    DecisionKind::Refund => (
                        "order.q_refund",
                        "order.what_refund",
                        c.direction.source(),
                        c.refund_amount.or(c.intake_amount),
                    ),
                    DecisionKind::ClosePaid => (
                        "order.q_paid",
                        "order.what_paid",
                        c.direction.target(),
                        None,
                    ),
                    DecisionKind::Close => (
                        "order.q_close",
                        "order.what_close",
                        c.direction.source(),
                        c.intake_amount.or(c.quote_amount_in),
                    ),
                    DecisionKind::Intake => (
                        "order.q_intake",
                        "order.what_intake",
                        c.direction.source(),
                        *amount,
                    ),
                    DecisionKind::RefundIssued => (
                        "order.q_refund_issued",
                        "order.what_refund_issued",
                        c.direction.source(),
                        c.refund_amount,
                    ),
                    DecisionKind::RetryExcess => (
                        "order.q_retry_excess",
                        "order.what_retry",
                        c.direction.source(),
                        c.refund_amount,
                    ),
                    DecisionKind::ExcessIssued => (
                        "order.q_excess_issued",
                        "order.what_excess_issued",
                        c.direction.source(),
                        c.refund_amount,
                    ),
                };
                let mut what = t.f(
                    what,
                    &[
                        ("wallet", &self.platform_name(wallet)),
                        ("amount", &money(sum)),
                        ("client", &client),
                    ],
                );
                // Д-19.3 (картинка last-fixes.png принята 20.11): что будет с переплатой по счёту —
                // тот же расчёт, что при автоматической оплате.
                if *kind == DecisionKind::ClosePaid
                    && let Some(x) = self.engine.paid_excess(*order).await?
                {
                    let line = paid_excess_line(t, &x, &self.platform_name(x.platform));
                    what = format!("{what}\n{line}");
                }
                let money_kinds = matches!(
                    kind,
                    DecisionKind::RetryPayout
                        | DecisionKind::RetryInvoice
                        | DecisionKind::Refund
                        | DecisionKind::RetryExcess
                );
                let previous = if let Some(op) = c
                    .operations
                    .iter()
                    .find(|o| money_kinds && unknown_key.as_deref() == Some(o.key.as_str()))
                {
                    t.f(
                        "order.prev_unknown",
                        &[
                            ("at", &crate::dash::hhmm(op.created_at)),
                            ("key", &esc(&op.key)),
                        ],
                    )
                } else if let Some(op) = c.operations.iter().rev().find(|o| {
                    money_kinds
                        && o.status == "failed"
                        && if *kind == DecisionKind::RetryExcess {
                            o.kind == "create_excess_check"
                        } else {
                            matches!(
                                o.kind.as_str(),
                                "create_payout_check" | "create_refund_check" | "pay_invoice"
                            )
                        }
                }) {
                    t.f(
                        "order.prev_failed",
                        &[("at", &crate::dash::hhmm(op.created_at))],
                    )
                } else {
                    String::new()
                };
                let mut warning = t.t("order.irreversible");
                if *kind == DecisionKind::Close {
                    warning = format!("{}\n{warning}", t.t("order.warn_close"));
                }
                unknown_label = Some(t.f(
                    match kind {
                        DecisionKind::RetryInvoice => "order.unknown_retry_invoice",
                        DecisionKind::Refund => "order.unknown_refund",
                        _ => "order.unknown_retry",
                    },
                    &[("amount", &money(sum))],
                ));
                confirm_label = Some(t.f(
                    match kind {
                        DecisionKind::RetryPayout => "order.b_do_retry",
                        DecisionKind::RetryInvoice => "order.b_do_retry_invoice",
                        DecisionKind::Refund => "order.b_do_refund",
                        DecisionKind::ClosePaid => "order.b_do_paid",
                        DecisionKind::Close => "order.b_do_close",
                        DecisionKind::Intake => "order.b_do_intake",
                        DecisionKind::RefundIssued => "order.b_do_refund_issued",
                        DecisionKind::RetryExcess => "order.b_do_retry_excess",
                        DecisionKind::ExcessIssued => "order.b_do_excess_issued",
                    },
                    &[("amount", &money(sum))],
                ));
                t.f(
                    "order.confirm",
                    &[
                        ("question", &t.t(q)),
                        ("public", &esc(&c.public_id)),
                        ("what", &what),
                        ("previous", &previous),
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
                ..
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
                    let queued = {
                        let mut conn = self.pool.acquire().await?;
                        storage::operations::queued_money_out(&mut conn).await?
                    };
                    text.push_str(&kill_warn(t, &queued));
                }
                text
            }
            Action::MaintText { before, after } => t.f(
                "maint.preview_text",
                &[
                    (
                        "before",
                        &before.as_deref().map_or_else(|| t.t("common.none"), esc),
                    ),
                    (
                        "after",
                        &after.as_deref().map_or_else(|| t.t("common.none"), esc),
                    ),
                ],
            ),
            Action::Maintain {
                before_on,
                before_until,
                until,
            } => t.f(
                "settings.preview_toggle",
                &[
                    ("name", &t.t("settings.n_maint")),
                    ("before", &self.maint_state(*before_on, *before_until)),
                    ("after", &self.maint_state(true, Some(*until))),
                ],
            ),
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
            Action::WriteOff {
                platform,
                amount,
                reason,
                ..
            } => {
                confirm_label = Some(t.f("capital.b_write_off", &[("amount", &usdt(*amount))]));
                t.f(
                    "capital.preview_write_off",
                    &[
                        ("amount", &usdt(*amount)),
                        ("name", &self.platform_name(*platform)),
                        ("reason", &esc(reason)),
                    ],
                )
            }
            Action::Selftest { amount } => t.f("selftest.preview", &[("amount", &usdt(*amount))]),
            Action::Invest(op) => self.invest_preview(op),
        };
        // «Прежней попытки не было» при неясном исходе — админ и выше (шаг 3, таблица прав).
        let need = if unknown_key.is_some() {
            need.max(Role::Admin)
        } else {
            need
        };
        let mut s = Screen::new(text);
        if staff.role >= need {
            let danger = action.irreversible();
            let nonce = self.hold_for(staff.tg_user_id, action, ttl);
            // Неясный исход: на кнопке — что именно произойдёт и сколько (аудит 16.11, П-1),
            // а не голое «подтверждаю».
            let (label, cb) = match unknown_key {
                Some(_) => (
                    t.f(
                        "order.b_do_unknown",
                        &[("action", &unknown_label.clone().unwrap_or_default())],
                    ),
                    Cb::ConfirmUnknown(nonce),
                ),
                None => (
                    confirm_label.unwrap_or_else(|| t.t("common.confirm")),
                    Cb::Confirm(nonce),
                ),
            };
            let style = if danger {
                Some(botapi::ButtonStyle::Danger)
            } else if confirm_primary {
                Some(botapi::ButtonStyle::Primary)
            } else {
                None
            };
            s = confirm_rows(t, s, label, &cb, style, &Cb::Cancel);
        } else if need == Role::Owner {
            // Решает владелец: предложить ему одной кнопкой (шаг 3, макет «решает владелец»).
            s.text
                .push_str(&format!("\n\n{}", t.t("common.owner_decides")));
            let nonce = self.hold(staff.tg_user_id, action);
            s = s.row(vec![
                btn(t.t("common.b_ask_owner"), &Cb::AskOwner(nonce))
                    .style(botapi::ButtonStyle::Primary),
                btn(t.t("common.cancel"), &Cb::Cancel),
            ]);
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
            ..
        } = &action
        {
            // П-2: пока ждали подтверждения, заявку мог забрать другой — решает он.
            if let crate::views::Claim::Taken(name) = self.claim(staff, *order, false).await? {
                return self.taken_by_other(staff, *order, &name).await;
            }
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
                DecisionKind::RefundIssued => OperatorDecision::CloseAsRefunded {
                    evidence: evidence.clone().unwrap_or_default(),
                },
                DecisionKind::RetryExcess => OperatorDecision::RetryExcess { previous_failed },
                DecisionKind::ExcessIssued => OperatorDecision::CloseExcessIssued {
                    evidence: evidence.clone().unwrap_or_default(),
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
            let public = esc(&self.public_id(*order).await?);
            let head = match result {
                Ok(o) => t.f(
                    "order.done",
                    &[
                        ("public", &public),
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
            Action::DirClearAuto {
                direction,
                only_unknown,
            } => {
                if *only_unknown {
                    storage::directions::clear_auto_pause_reason(
                        &mut tx,
                        direction.source(),
                        engine::UNKNOWN_REPLY_PAUSE,
                    )
                    .await?;
                } else {
                    storage::directions::clear_auto_pause(&mut tx, *direction, staff_id).await?;
                }
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
            Action::PromoBroadcast { on, .. } => {
                let before =
                    storage::settings::flag(&mut tx, engine::fees::PROMO_BROADCAST, false).await?;
                storage::settings::set(
                    &mut tx,
                    engine::fees::PROMO_BROADCAST,
                    &json!({ "on": on }),
                    Some(staff_id),
                )
                .await?;
                (
                    "promo.broadcast",
                    "settings",
                    engine::fees::PROMO_BROADCAST.to_owned(),
                    Some(json!({ "on": before })),
                    Some(json!({ "on": on })),
                    Cb::Promo,
                )
            }
            Action::PromoSwitch { direction, on, .. } => {
                engine::fees::switch_promo(&mut tx, *direction, *on, staff_id).await?;
                if *on {
                    storage::alerts::resolve(&mut tx, &format!("promo_hint:{}", direction.code()))
                        .await?;
                    engine::fees::announce_promo(&mut tx, *direction).await?;
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
            Action::PasscodeLimit { before, after } => {
                storage::settings::set(
                    &mut tx,
                    engine::PASSCODE_FREE_LIMIT_KEY,
                    &json!(after.to_string()),
                    Some(staff_id),
                )
                .await?;
                (
                    "settings.passcode_free_limit",
                    "setting",
                    engine::PASSCODE_FREE_LIMIT_KEY.to_owned(),
                    Some(json!({ "limit": before.map(|b| b.to_string()) })),
                    Some(json!({ "limit": after.to_string() })),
                    Cb::Reserves,
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
                ..
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
                // Переключатель — без срока: и «вкл», и «выкл» снимают «Обслуживание 30 мин»; конец
                // обслуживания снимает и текст для клиентов — в следующий раз не всплывёт старый.
                if let Some(o) = value.as_object_mut() {
                    o.remove("until");
                    o.remove("since");
                    if !on {
                        o.remove("text");
                    }
                }
                storage::settings::set(&mut tx, key, &value, Some(staff_id)).await?;
                // Стоп-кран — критичная тревога всему персоналу, пока его не снимут (шаг 2.8).
                if *flag == Flag::KillSwitch {
                    if *on {
                        storage::alerts::raise_condition(
                            &mut tx,
                            &storage::alerts::NewAlert {
                                level: storage::alerts::Level::Critical,
                                topic: storage::alerts::Topic::Errors,
                                dedup_key: "kill_switch",
                                title: "kill_switch",
                                payload: json!({
                                    "by": staff.display_name,
                                    "at": chrono::Utc::now().to_rfc3339(),
                                }),
                            },
                        )
                        .await?;
                    } else {
                        storage::alerts::resolve(&mut tx, "kill_switch").await?;
                    }
                }
                (
                    "settings.flag",
                    "settings",
                    key.to_owned(),
                    Some(json!({ "on": !on })),
                    Some(json!({ "on": on })),
                    Cb::Settings,
                )
            }
            Action::MaintText { before, after } => {
                let key = Flag::Maintenance.key();
                let mut value = storage::settings::get(&mut tx, key)
                    .await?
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
                value["text"] = json!(after);
                storage::settings::set(&mut tx, key, &value, Some(staff_id)).await?;
                (
                    "settings.maint_text",
                    "settings",
                    key.to_owned(),
                    Some(json!({ "text": before })),
                    Some(json!({ "text": after })),
                    Cb::Maintenance,
                )
            }
            Action::Maintain {
                before_on,
                before_until,
                until,
            } => {
                let key = Flag::Maintenance.key();
                // «С 15:00» на главном экране продление не сдвигает.
                let since = match storage::settings::on_since(&mut tx, key).await? {
                    Some(since) if *before_on => since,
                    _ => chrono::Utc::now(),
                };
                // Текст для клиентов остаётся при продлении; новое обслуживание — без старого.
                let text = if *before_on {
                    storage::settings::on_note(&mut tx, key).await?
                } else {
                    None
                };
                let value = json!({
                    "on": true,
                    "until": until.to_rfc3339(),
                    "since": since.to_rfc3339(),
                    "text": text,
                });
                storage::settings::set(&mut tx, key, &value, Some(staff_id)).await?;
                (
                    "settings.flag",
                    "settings",
                    key.to_owned(),
                    Some(json!({ "on": before_on, "until": before_until.map(|u| u.to_rfc3339()) })),
                    Some(json!({ "on": true, "until": until.to_rfc3339() })),
                    Cb::Maintenance,
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
            Action::WriteOff {
                platform,
                amount,
                alert,
                reason,
            } => {
                let open = storage::alerts::get(&mut tx, *alert)
                    .await?
                    .is_some_and(|a| a.acked_at.is_none());
                if !open {
                    return Ok(Screen::new(t.t("capital.gone"))
                        .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                }
                let Ok(posting) = domain::ledger::loss(*platform, Asset::Usdt, *amount) else {
                    return Ok(Screen::new(t.t("refusal.bad_amount"))
                        .row(vec![btn(t.t("common.home"), &Cb::Menu)]));
                };
                let created_by = format!("staff:{staff_id}");
                storage::ledger::post(
                    &mut tx,
                    &posting,
                    storage::ledger::PostingRefs {
                        memo: Some(reason),
                        created_by: Some(&created_by),
                        ..Default::default()
                    },
                )
                .await?;
                storage::alerts::ack(&mut tx, *alert, staff_id).await?;
                (
                    "capital.loss",
                    "wallet",
                    platform.db_name().to_owned(),
                    None,
                    Some(json!({ "amount": amount.to_string(), "reason": reason })),
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
            Cb::Promo => self.promo(staff).await?,
            Cb::UserDir(id, d) => self.user_dir(id, d).await?,
            Cb::User(id) => self.user(Some(staff), id).await?,
            Cb::Settings => self.settings().await?,
            Cb::Maintenance => self.maintenance().await?,
            Cb::Emoji => self.emoji().await?,
            Cb::Bots => self.bots().await?,
            Cb::Staff => self.staff_list(staff).await?,
            Cb::Dash => self.dash().await?,
            Cb::Reserves => self.reserves(staff).await?,
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

/// Строка о переплате на «Отметить выплаченной?» (Д-19.3, картинка last-fixes.png принята 20.11).
pub(crate) fn paid_excess_line(t: &crate::Texts, x: &engine::PaidExcess, wallet: &str) -> String {
    match x.refund {
        Some(r) => t.f(
            "order.paid_excess_refund",
            &[("amount", &crate::dash::amount(r)), ("wallet", &wallet)],
        ),
        None => t.f(
            "order.paid_excess_kept",
            &[
                ("amount", &crate::dash::amount(x.excess)),
                ("min", &crate::dash::amount(x.threshold)),
            ],
        ),
    }
}

/// Кнопка подтверждения и «Отмена». Длинная надпись рядом с «Отмена» обрезается («Отметить
/// вып…»): такая кнопка — отдельной строкой (макеты «Подтверждение · переплата», last-fixes.png).
pub(crate) fn confirm_rows(
    t: &crate::Texts,
    s: Screen,
    label: String,
    cb: &Cb,
    style: Option<botapi::ButtonStyle>,
    cancel: &Cb,
) -> Screen {
    let long = label
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .trim()
        .chars()
        .count()
        > LONG_BUTTON;
    let mut confirm = btn(label, cb);
    if let Some(style) = style {
        confirm = confirm.style(style);
    }
    let cancel = btn(t.t("common.cancel"), cancel);
    if long {
        s.row(vec![confirm]).row(vec![cancel])
    } else {
        s.row(vec![confirm, cancel])
    }
}

/// «Отметить выплаченной?» по счёту с переплатой на данных макета: `refund` — вернётся чеком,
/// иначе останется в доходе (предпросмотр, снимок).
pub fn sample_paid_confirm(t: &crate::Texts, refund: bool) -> Screen {
    let x = engine::PaidExcess {
        excess: if refund {
            Decimal::new(875, 2)
        } else {
            Decimal::new(30, 2)
        },
        refund: refund.then(|| Decimal::new(875, 2)),
        threshold: Decimal::new(50, 2),
        platform: Platform::XRocket,
    };
    let what = format!(
        "{}\n{}",
        t.f("order.what_paid", &[("client", &"51234567")]),
        paid_excess_line(t, &x, &t.t("names.xrocket"))
    );
    let extra = t.f(
        "order.preview_evidence",
        &[("evidence", &"оплатил кнопкой в @send, 14:10")],
    );
    let text = t.f(
        "order.confirm",
        &[
            ("question", &t.t("order.q_paid")),
            ("public", &"7F3A1C9D2B"),
            ("what", &what),
            ("previous", &""),
            ("reason", &"счёт оплачен вручную"),
            ("extra", &extra),
            ("warning", &t.t("order.irreversible")),
        ],
    );
    confirm_rows(
        t,
        Screen::new(text),
        t.t("order.b_do_paid"),
        &Cb::Sample,
        Some(botapi::ButtonStyle::Danger),
        &Cb::Sample,
    )
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    /// П-13: предпросмотр стоп-крана — сколько выплат встанет, на какую сумму, без «random_id».
    #[test]
    fn kill_warn_counts_the_queued_payouts() {
        let t = crate::Texts::builtin().unwrap();
        let line = kill_warn(&t, &[(domain::Asset::Usdt, 3, dec!(412.5))]);
        assert_eq!(
            line,
            "\nВ очереди 3 выплаты на <code>412.50</code> USDT — встанут, после снятия уйдут сами, \
             без дублей. Новые заявки до снятия — в разбор."
        );
        let one = kill_warn(&t, &[(domain::Asset::Usdt, 1, dec!(10))]);
        assert!(one.contains("1 выплата на"), "{one}");
        assert!(kill_warn(&t, &[]).contains("Выплат в очереди нет"));
    }

    /// Шаг 3: «Деньги пришли» дальше 1 % от чека, личная ставка ниже обычной больше чем на
    /// 1 п.п. — решает владелец.
    #[test]
    fn owner_decides_off_check_intake_and_big_personal_discounts() {
        use rust_decimal_macros::dec;
        assert!(!off_check(dec!(100.5), dec!(100)));
        assert!(!off_check(dec!(101), dec!(100)), "ровно 1 % — ещё можно");
        assert!(off_check(dec!(101.01), dec!(100)));
        assert!(off_check(dec!(1000), dec!(100)));
        let intake = |amount| Action::Decide {
            order: 1,
            kind: DecisionKind::Intake,
            reason: "ok".into(),
            evidence: None,
            amount: Some(amount),
            check: Some(dec!(100)),
        };
        assert_eq!(intake(dec!(100)).min_role(), Role::Admin);
        assert_eq!(intake(dec!(1000)).min_role(), Role::Owner);

        let d = Direction::CbToXrCheck;
        let terms = |fee| storage::user_terms::UserTerms {
            fee_pct: fee,
            ..storage::user_terms::UserTerms::empty(d)
        };
        let action = |before, after| Action::Terms {
            user: 7,
            before: terms(before),
            after: terms(after),
            base_pct: dec!(2.5),
        };
        assert_eq!(action(None, Some(dec!(2))).min_role(), Role::Admin);
        assert_eq!(action(None, Some(dec!(1.4))).min_role(), Role::Owner);
        assert_eq!(
            action(Some(dec!(2)), Some(dec!(0.5))).min_role(),
            Role::Owner
        );
        assert_eq!(
            action(Some(dec!(1)), None).min_role(),
            Role::Owner,
            "сдвиг 1.5 п.п."
        );
    }

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
