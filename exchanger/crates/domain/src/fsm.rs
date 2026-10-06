//! Машина состояний заявки (SPEC §4, §10.5.1, diagrams/08-state-order.puml).
//!
//! `transition(flow, state, event)` — единственный способ сменить состояние.
//! Множество переходов, достижимых через события, совпадает с [`ALLOWED`] и с таблицей
//! `order_state_transitions` в БД (это проверяют тесты domain и storage).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Состояние заявки. Строковое представление — как в `orders.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OrderState {
    New,
    AwaitingFunds,
    AwaitingPassword,
    IntakePending,
    IntakeUnknown,
    Received,
    PayoutPending,
    InvoicePayPending,
    RefundPending,
    ManualReview,
    Completed,
    Refunded,
    Rejected,
    IntakeFailed,
    Expired,
    Cancelled,
    ClosedManual,
}

impl OrderState {
    pub const ALL: [OrderState; 17] = [
        OrderState::New,
        OrderState::AwaitingFunds,
        OrderState::AwaitingPassword,
        OrderState::IntakePending,
        OrderState::IntakeUnknown,
        OrderState::Received,
        OrderState::PayoutPending,
        OrderState::InvoicePayPending,
        OrderState::RefundPending,
        OrderState::ManualReview,
        OrderState::Completed,
        OrderState::Refunded,
        OrderState::Rejected,
        OrderState::IntakeFailed,
        OrderState::Expired,
        OrderState::Cancelled,
        OrderState::ClosedManual,
    ];

    pub const fn as_db_str(self) -> &'static str {
        match self {
            OrderState::New => "NEW",
            OrderState::AwaitingFunds => "AWAITING_FUNDS",
            OrderState::AwaitingPassword => "AWAITING_PASSWORD",
            OrderState::IntakePending => "INTAKE_PENDING",
            OrderState::IntakeUnknown => "INTAKE_UNKNOWN",
            OrderState::Received => "RECEIVED",
            OrderState::PayoutPending => "PAYOUT_PENDING",
            OrderState::InvoicePayPending => "INVOICE_PAY_PENDING",
            OrderState::RefundPending => "REFUND_PENDING",
            OrderState::ManualReview => "MANUAL_REVIEW",
            OrderState::Completed => "COMPLETED",
            OrderState::Refunded => "REFUNDED",
            OrderState::Rejected => "REJECTED",
            OrderState::IntakeFailed => "INTAKE_FAILED",
            OrderState::Expired => "EXPIRED",
            OrderState::Cancelled => "CANCELLED",
            OrderState::ClosedManual => "CLOSED_MANUAL",
        }
    }

    /// Терминальные состояния не имеют исходящих переходов (как `orders.is_terminal`).
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderState::Completed
                | OrderState::Refunded
                | OrderState::Rejected
                | OrderState::IntakeFailed
                | OrderState::Expired
                | OrderState::Cancelled
                | OrderState::ClosedManual
        )
    }
}

impl fmt::Display for OrderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_db_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown order state: {0}")]
pub struct UnknownState(pub String);

impl FromStr for OrderState {
    type Err = UnknownState;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|st| st.as_db_str() == s)
            .ok_or_else(|| UnknownState(s.to_owned()))
    }
}

/// Вид заявки: обмен чека или оплата чужого счёта. Определяется направлением.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Flow {
    CheckExchange,
    InvoicePayment,
}

impl Flow {
    pub const ALL: [Flow; 2] = [Flow::CheckExchange, Flow::InvoicePayment];
}

/// Как закрываем полученные деньги (из `RECEIVED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Settlement {
    /// Обмен чека: выплата через API.
    Payout,
    /// Оплата счёта: юзербот платит чужой счёт.
    PayInvoice,
    /// Сверх лимита, не тот актив, счёт стал неоплачиваемым.
    Refund,
    /// Стоп-кран или паника — человеку.
    Manual,
}

/// Решение оператора по заявке в `MANUAL_REVIEW` (SPEC §4, П9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OperatorAction {
    RetryPayout,
    RetryInvoicePayment,
    Refund,
    /// «Закрыть: выплачено» — только с доказательством.
    CloseAsPaid,
    /// «Закрыть без выплаты» — только владелец, с причиной.
    CloseWithoutPayout,
}

/// Событие, меняющее заявку.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Event {
    /// Проверки пройдены: обмен — чек уходит на активацию; оплата — котировка выдана.
    LinkAccepted,
    /// Бан, лимит, пауза, нет резерва, неподходящий счёт — деньги не тронуты.
    Rejected,
    /// Оплата счёта: клиент прислал чек.
    ClientCheckSubmitted,
    /// Оплата счёта: клиент оплатил наш счёт.
    OurInvoicePaid,
    QuoteExpired {
        funds_received: bool,
    },
    ClientCancelled {
        funds_received: bool,
    },
    /// Кошелёк зачислил чек. `fully_funded = false` только для оплаты счёта (недоплата).
    CheckActivated {
        fully_funded: bool,
    },
    /// Чек истёк, забран другим, Premium, подписка и т. п. — денег нет.
    CheckRejected,
    PasswordRequested,
    PasswordSubmitted,
    PasswordTimedOut,
    /// Таймаут или незнакомый ответ кошелька при активации.
    IntakeOutcomeUnknown,
    ReconciledReceived {
        fully_funded: bool,
    },
    ReconciledNotReceived,
    ReconciliationAmbiguous,
    Settle(Settlement),
    PayoutConfirmed,
    PayoutFailed,
    InvoicePaid,
    /// Счёт истёк или его оплатили до нашего нажатия.
    InvoiceUnpayable,
    /// Нужен код-пароль, мини-приложение или исход нажатия неясен.
    InvoicePayNeedsHuman,
    RefundConfirmed,
    RefundFailed,
    Operator(OperatorAction),
}

impl Event {
    /// Все события со всеми вариантами параметров — для исчерпывающих тестов.
    pub fn all() -> Vec<Event> {
        let mut v = vec![
            Event::LinkAccepted,
            Event::Rejected,
            Event::ClientCheckSubmitted,
            Event::OurInvoicePaid,
            Event::CheckRejected,
            Event::PasswordRequested,
            Event::PasswordSubmitted,
            Event::PasswordTimedOut,
            Event::IntakeOutcomeUnknown,
            Event::ReconciledNotReceived,
            Event::ReconciliationAmbiguous,
            Event::PayoutConfirmed,
            Event::PayoutFailed,
            Event::InvoicePaid,
            Event::InvoiceUnpayable,
            Event::InvoicePayNeedsHuman,
            Event::RefundConfirmed,
            Event::RefundFailed,
        ];
        for b in [false, true] {
            v.push(Event::QuoteExpired { funds_received: b });
            v.push(Event::ClientCancelled { funds_received: b });
            v.push(Event::CheckActivated { fully_funded: b });
            v.push(Event::ReconciledReceived { fully_funded: b });
        }
        for s in [
            Settlement::Payout,
            Settlement::PayInvoice,
            Settlement::Refund,
            Settlement::Manual,
        ] {
            v.push(Event::Settle(s));
        }
        for a in [
            OperatorAction::RetryPayout,
            OperatorAction::RetryInvoicePayment,
            OperatorAction::Refund,
            OperatorAction::CloseAsPaid,
            OperatorAction::CloseWithoutPayout,
        ] {
            v.push(Event::Operator(a));
        }
        v
    }
}

/// Вид операции outbox (`operations.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpKind {
    ActivateCheck,
    SubmitCheckPassword,
    PayInvoice,
    PayoutTransfer,
    RefundTransfer,
}

impl OpKind {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            OpKind::ActivateCheck => "activate_check",
            OpKind::SubmitCheckPassword => "submit_check_password",
            OpKind::PayInvoice => "pay_invoice",
            OpKind::PayoutTransfer => "payout_transfer",
            OpKind::RefundTransfer => "refund_transfer",
        }
    }
}

/// Роль операции в расчёте (`operations.money_role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MoneyRole {
    Intake,
    Settle,
}

impl MoneyRole {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            MoneyRole::Intake => "intake",
            MoneyRole::Settle => "settle",
        }
    }
}

/// Действие с удержанием резерва (`holds`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HoldAction {
    /// Удержать под заявку (обмен — `max_order_eff`, оплата счёта — сумму счёта).
    Place,
    /// Уменьшить до точной суммы выплаты.
    ShrinkToPayout,
    Release,
    Consume,
}

/// Какую проводку сделать; суммы считает движок по данным заявки (SPEC §10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PostingKind {
    Intake,
    Payout,
    InvoicePaid,
    Refund,
    ManualClose,
}

/// Побочный эффект перехода. Все эффекты выполняются в той же транзакции БД, что и переход;
/// сетевые вызовы делает outbox-воркер после коммита.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Effect {
    /// Создать операцию outbox. Для `Settle` движок переиспользует незавершённую операцию
    /// того же вида с тем же ключом идемпотентности (SPEC §10.5.2).
    Operation {
        kind: OpKind,
        role: MoneyRole,
    },
    Hold(HoldAction),
    Post(PostingKind),
}

/// Результат перехода.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub to: OrderState,
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsmError {
    #[error("order is terminal ({0}); no further events are accepted")]
    Terminal(OrderState),
    #[error("event {event:?} is not allowed in state {state} for {flow:?}")]
    NotAllowed {
        flow: Flow,
        state: OrderState,
        event: Event,
    },
}

/// Все разрешённые переходы — 1:1 с `order_state_transitions` в `schema.sql`.
pub const ALLOWED: [(OrderState, OrderState); 36] = {
    use OrderState::*;
    [
        (New, Rejected),
        (New, IntakePending),
        (New, AwaitingFunds),
        (AwaitingFunds, IntakePending),
        (AwaitingFunds, Received),
        (AwaitingFunds, Expired),
        (AwaitingFunds, Cancelled),
        (AwaitingFunds, RefundPending),
        (IntakePending, Received),
        (IntakePending, IntakeFailed),
        (IntakePending, AwaitingPassword),
        (IntakePending, IntakeUnknown),
        (IntakePending, AwaitingFunds),
        (AwaitingPassword, IntakePending),
        (AwaitingPassword, IntakeFailed),
        (AwaitingPassword, AwaitingFunds),
        (IntakeUnknown, Received),
        (IntakeUnknown, IntakeFailed),
        (IntakeUnknown, ManualReview),
        (IntakeUnknown, AwaitingFunds),
        (Received, PayoutPending),
        (Received, InvoicePayPending),
        (Received, RefundPending),
        (Received, ManualReview),
        (PayoutPending, Completed),
        (PayoutPending, ManualReview),
        (InvoicePayPending, Completed),
        (InvoicePayPending, RefundPending),
        (InvoicePayPending, ManualReview),
        (RefundPending, Refunded),
        (RefundPending, ManualReview),
        (ManualReview, PayoutPending),
        (ManualReview, RefundPending),
        (ManualReview, Completed),
        (ManualReview, ClosedManual),
        (ManualReview, InvoicePayPending),
    ]
};

pub fn is_allowed(from: OrderState, to: OrderState) -> bool {
    ALLOWED.contains(&(from, to))
}

/// Единственная функция смены состояния заявки.
pub fn transition(flow: Flow, state: OrderState, event: Event) -> Result<Transition, FsmError> {
    use Effect::{Hold, Operation, Post};
    use Event as E;
    use Flow::{CheckExchange as Check, InvoicePayment as Invoice};
    use OrderState as S;

    if state.is_terminal() {
        return Err(FsmError::Terminal(state));
    }

    let op = |kind, role| Operation { kind, role };
    // Чек не принят: обмен заканчивается, оплата счёта ждёт другой чек.
    let intake_lost = |flow: Flow| match flow {
        Check => (S::IntakeFailed, vec![Hold(HoldAction::Release)]),
        Invoice => (S::AwaitingFunds, vec![]),
    };
    let refund = || {
        vec![
            Hold(HoldAction::Release),
            op(OpKind::RefundTransfer, MoneyRole::Settle),
        ]
    };

    let next: Option<(OrderState, Vec<Effect>)> = match (state, flow, event) {
        (S::New, Check, E::LinkAccepted) => Some((
            S::IntakePending,
            vec![
                Hold(HoldAction::Place),
                op(OpKind::ActivateCheck, MoneyRole::Intake),
            ],
        )),
        (S::New, Invoice, E::LinkAccepted) => {
            Some((S::AwaitingFunds, vec![Hold(HoldAction::Place)]))
        }
        (S::New, _, E::Rejected) => Some((S::Rejected, vec![])),

        (S::AwaitingFunds, Invoice, E::ClientCheckSubmitted) => Some((
            S::IntakePending,
            vec![op(OpKind::ActivateCheck, MoneyRole::Intake)],
        )),
        (S::AwaitingFunds, Invoice, E::OurInvoicePaid) => {
            Some((S::Received, vec![Post(PostingKind::Intake)]))
        }
        (
            S::AwaitingFunds,
            Invoice,
            E::QuoteExpired { funds_received } | E::ClientCancelled { funds_received },
        ) => Some(match (funds_received, event) {
            (true, _) => (S::RefundPending, refund()),
            (false, E::QuoteExpired { .. }) => (S::Expired, vec![Hold(HoldAction::Release)]),
            (false, _) => (S::Cancelled, vec![Hold(HoldAction::Release)]),
        }),

        (S::IntakePending, _, E::CheckActivated { fully_funded: true }) => {
            Some((S::Received, vec![Post(PostingKind::Intake)]))
        }
        (
            S::IntakePending,
            Invoice,
            E::CheckActivated {
                fully_funded: false,
            },
        ) => Some((S::AwaitingFunds, vec![Post(PostingKind::Intake)])),
        (S::IntakePending, _, E::CheckRejected) => Some(intake_lost(flow)),
        (S::IntakePending, _, E::PasswordRequested) => Some((S::AwaitingPassword, vec![])),
        (S::IntakePending, _, E::IntakeOutcomeUnknown) => Some((S::IntakeUnknown, vec![])),

        (S::AwaitingPassword, _, E::PasswordSubmitted) => Some((
            S::IntakePending,
            vec![op(OpKind::SubmitCheckPassword, MoneyRole::Intake)],
        )),
        // Отмена ввода пароля бросает только этот чек; деньги, пришедшие раньше
        // (доплата по счёту), остаются в заявке, и она возвращается в AWAITING_FUNDS.
        (S::AwaitingPassword, _, E::PasswordTimedOut | E::ClientCancelled { .. }) => {
            Some(intake_lost(flow))
        }

        (S::IntakeUnknown, _, E::ReconciledReceived { fully_funded: true }) => {
            Some((S::Received, vec![Post(PostingKind::Intake)]))
        }
        (
            S::IntakeUnknown,
            Invoice,
            E::ReconciledReceived {
                fully_funded: false,
            },
        ) => Some((S::AwaitingFunds, vec![Post(PostingKind::Intake)])),
        (S::IntakeUnknown, _, E::ReconciledNotReceived) => Some(intake_lost(flow)),
        (S::IntakeUnknown, _, E::ReconciliationAmbiguous) => Some((S::ManualReview, vec![])),

        (S::Received, Check, E::Settle(Settlement::Payout)) => Some((
            S::PayoutPending,
            vec![
                Hold(HoldAction::ShrinkToPayout),
                op(OpKind::PayoutTransfer, MoneyRole::Settle),
            ],
        )),
        (S::Received, Invoice, E::Settle(Settlement::PayInvoice)) => Some((
            S::InvoicePayPending,
            vec![op(OpKind::PayInvoice, MoneyRole::Settle)],
        )),
        (S::Received, _, E::Settle(Settlement::Refund)) => Some((S::RefundPending, refund())),
        (S::Received, _, E::Settle(Settlement::Manual)) => Some((S::ManualReview, vec![])),

        (S::PayoutPending, Check, E::PayoutConfirmed) => Some((
            S::Completed,
            vec![Post(PostingKind::Payout), Hold(HoldAction::Consume)],
        )),
        (S::PayoutPending, Check, E::PayoutFailed) => Some((S::ManualReview, vec![])),

        (S::InvoicePayPending, Invoice, E::InvoicePaid) => Some((
            S::Completed,
            vec![Post(PostingKind::InvoicePaid), Hold(HoldAction::Consume)],
        )),
        (S::InvoicePayPending, Invoice, E::InvoiceUnpayable) => Some((S::RefundPending, refund())),
        (S::InvoicePayPending, Invoice, E::InvoicePayNeedsHuman) => Some((S::ManualReview, vec![])),

        (S::RefundPending, _, E::RefundConfirmed) => {
            Some((S::Refunded, vec![Post(PostingKind::Refund)]))
        }
        (S::RefundPending, _, E::RefundFailed) => Some((S::ManualReview, vec![])),

        (S::ManualReview, Check, E::Operator(OperatorAction::RetryPayout)) => Some((
            S::PayoutPending,
            vec![op(OpKind::PayoutTransfer, MoneyRole::Settle)],
        )),
        (S::ManualReview, Invoice, E::Operator(OperatorAction::RetryInvoicePayment)) => Some((
            S::InvoicePayPending,
            vec![op(OpKind::PayInvoice, MoneyRole::Settle)],
        )),
        (S::ManualReview, _, E::Operator(OperatorAction::Refund)) => {
            Some((S::RefundPending, refund()))
        }
        (S::ManualReview, _, E::Operator(OperatorAction::CloseAsPaid)) => Some((
            S::Completed,
            vec![
                Post(match flow {
                    Check => PostingKind::Payout,
                    Invoice => PostingKind::InvoicePaid,
                }),
                Hold(HoldAction::Consume),
            ],
        )),
        (S::ManualReview, _, E::Operator(OperatorAction::CloseWithoutPayout)) => Some((
            S::ClosedManual,
            vec![Post(PostingKind::ManualClose), Hold(HoldAction::Release)],
        )),

        _ => None,
    };

    match next {
        Some((to, effects)) => {
            debug_assert!(
                is_allowed(state, to),
                "{state} -> {to} missing from ALLOWED"
            );
            Ok(Transition { to, effects })
        }
        None => Err(FsmError::NotAllowed { flow, state, event }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use Effect::{Hold, Operation, Post};
    use OrderState as S;

    fn ok(flow: Flow, state: OrderState, event: Event) -> Transition {
        transition(flow, state, event).unwrap()
    }

    #[test]
    fn state_names_round_trip() {
        for s in OrderState::ALL {
            assert_eq!(s.as_db_str().parse::<OrderState>(), Ok(s));
        }
        assert!("PAYOUT_UNKNOWN".parse::<OrderState>().is_err());
    }

    #[test]
    fn allowed_has_no_duplicates_and_no_exits_from_terminals() {
        let set: BTreeSet<_> = ALLOWED.iter().collect();
        assert_eq!(set.len(), ALLOWED.len());
        assert!(ALLOWED.iter().all(|(from, _)| !from.is_terminal()));
    }

    /// Исчерпывающий перебор (поток × состояние × событие): переходы, которые дают события,
    /// в точности совпадают с ALLOWED, и каждый из них разрешён.
    #[test]
    fn reachable_transitions_equal_allowed() {
        let mut reached = BTreeSet::new();
        for flow in Flow::ALL {
            for state in OrderState::ALL {
                for event in Event::all() {
                    match transition(flow, state, event) {
                        Ok(t) => {
                            assert!(
                                is_allowed(state, t.to),
                                "{flow:?}: {state} --{event:?}--> {} is not in ALLOWED",
                                t.to
                            );
                            reached.insert((state, t.to));
                        }
                        Err(FsmError::Terminal(s)) => assert!(s.is_terminal()),
                        Err(FsmError::NotAllowed { .. }) => {}
                    }
                }
            }
        }
        let allowed: BTreeSet<_> = ALLOWED.into_iter().collect();
        assert_eq!(reached, allowed);
    }

    #[test]
    fn terminal_states_accept_nothing() {
        for state in OrderState::ALL.into_iter().filter(|s| s.is_terminal()) {
            for event in Event::all() {
                assert_eq!(
                    transition(Flow::CheckExchange, state, event),
                    Err(FsmError::Terminal(state))
                );
            }
        }
    }

    /// Табличный тест ключевых путей (SPEC §4): состояние, событие → новое состояние и эффекты.
    #[test]
    fn spec_paths() {
        use Flow::{CheckExchange as C, InvoicePayment as I};
        use HoldAction as H;
        let cases: Vec<(Flow, OrderState, Event, OrderState, Vec<Effect>)> = vec![
            (
                C,
                S::New,
                Event::LinkAccepted,
                S::IntakePending,
                vec![
                    Hold(H::Place),
                    Operation {
                        kind: OpKind::ActivateCheck,
                        role: MoneyRole::Intake,
                    },
                ],
            ),
            (
                C,
                S::IntakePending,
                Event::CheckActivated { fully_funded: true },
                S::Received,
                vec![Post(PostingKind::Intake)],
            ),
            (
                C,
                S::Received,
                Event::Settle(Settlement::Payout),
                S::PayoutPending,
                vec![
                    Hold(H::ShrinkToPayout),
                    Operation {
                        kind: OpKind::PayoutTransfer,
                        role: MoneyRole::Settle,
                    },
                ],
            ),
            (
                C,
                S::PayoutPending,
                Event::PayoutConfirmed,
                S::Completed,
                vec![Post(PostingKind::Payout), Hold(H::Consume)],
            ),
            (
                C,
                S::IntakePending,
                Event::CheckRejected,
                S::IntakeFailed,
                vec![Hold(H::Release)],
            ),
            (
                C,
                S::Received,
                Event::Settle(Settlement::Refund),
                S::RefundPending,
                vec![
                    Hold(H::Release),
                    Operation {
                        kind: OpKind::RefundTransfer,
                        role: MoneyRole::Settle,
                    },
                ],
            ),
            (
                C,
                S::RefundPending,
                Event::RefundConfirmed,
                S::Refunded,
                vec![Post(PostingKind::Refund)],
            ),
            (
                I,
                S::New,
                Event::LinkAccepted,
                S::AwaitingFunds,
                vec![Hold(H::Place)],
            ),
            (
                I,
                S::IntakePending,
                Event::CheckActivated {
                    fully_funded: false,
                },
                S::AwaitingFunds,
                vec![Post(PostingKind::Intake)],
            ),
            (
                I,
                S::IntakePending,
                Event::CheckRejected,
                S::AwaitingFunds,
                vec![],
            ),
            (
                I,
                S::AwaitingFunds,
                Event::QuoteExpired {
                    funds_received: true,
                },
                S::RefundPending,
                vec![
                    Hold(H::Release),
                    Operation {
                        kind: OpKind::RefundTransfer,
                        role: MoneyRole::Settle,
                    },
                ],
            ),
            (
                I,
                S::AwaitingFunds,
                Event::QuoteExpired {
                    funds_received: false,
                },
                S::Expired,
                vec![Hold(H::Release)],
            ),
            (
                I,
                S::Received,
                Event::Settle(Settlement::PayInvoice),
                S::InvoicePayPending,
                vec![Operation {
                    kind: OpKind::PayInvoice,
                    role: MoneyRole::Settle,
                }],
            ),
            (
                I,
                S::InvoicePayPending,
                Event::InvoicePaid,
                S::Completed,
                vec![Post(PostingKind::InvoicePaid), Hold(H::Consume)],
            ),
            (
                I,
                S::InvoicePayPending,
                Event::InvoicePayNeedsHuman,
                S::ManualReview,
                vec![],
            ),
            (
                I,
                S::ManualReview,
                Event::Operator(OperatorAction::CloseAsPaid),
                S::Completed,
                vec![Post(PostingKind::InvoicePaid), Hold(H::Consume)],
            ),
        ];
        for (flow, from, event, to, effects) in cases {
            assert_eq!(
                ok(flow, from, event),
                Transition { to, effects },
                "{flow:?} {from} {event:?}"
            );
        }
    }

    /// Защита от двойной выплаты: из PAYOUT_PENDING нельзя напрямую в возврат или повторную выплату.
    #[test]
    fn payout_pending_cannot_jump_to_refund() {
        for event in Event::all() {
            if let Ok(t) = transition(Flow::CheckExchange, S::PayoutPending, event) {
                assert!(
                    matches!(t.to, S::Completed | S::ManualReview),
                    "{event:?} -> {}",
                    t.to
                );
            }
        }
    }

    /// Поток определяет, какие расчёты возможны: обмен не платит счёт и наоборот.
    #[test]
    fn settlement_must_match_flow() {
        assert!(
            transition(
                Flow::CheckExchange,
                S::Received,
                Event::Settle(Settlement::PayInvoice)
            )
            .is_err()
        );
        assert!(
            transition(
                Flow::InvoicePayment,
                S::Received,
                Event::Settle(Settlement::Payout)
            )
            .is_err()
        );
        assert!(
            transition(
                Flow::CheckExchange,
                S::IntakePending,
                Event::CheckActivated {
                    fully_funded: false
                }
            )
            .is_err()
        );
        assert!(
            transition(
                Flow::CheckExchange,
                S::ManualReview,
                Event::Operator(OperatorAction::RetryInvoicePayment)
            )
            .is_err()
        );
    }

    /// После прихода денег заявку нельзя «просто закрыть»: только выплата, возврат или человек.
    #[test]
    fn received_money_never_expires_silently() {
        for flow in Flow::ALL {
            for state in [
                S::Received,
                S::PayoutPending,
                S::InvoicePayPending,
                S::RefundPending,
                S::ManualReview,
            ] {
                for event in Event::all() {
                    if let Ok(t) = transition(flow, state, event) {
                        assert!(
                            !matches!(
                                t.to,
                                S::Expired | S::Cancelled | S::Rejected | S::IntakeFailed
                            ),
                            "{flow:?}: {state} --{event:?}--> {}",
                            t.to
                        );
                    }
                }
            }
        }
    }
}
