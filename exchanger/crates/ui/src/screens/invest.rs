//! Раздел «💼 Инвестиции» (DESIGN-v0.2 Р18): экран с цифрами, пополнение, вывод, события и
//! справка «Как это работает?» с примером в цифрах. Справка — rich с HTML-фолбэком, всё
//! остальное — обычный HTML (это деньги).

use botapi::{ButtonStyle, KeyboardButton};
use domain::{Decimal, Platform};

use super::{Ui, inline_command};
use crate::callback::{Callback, InvestAction};
use crate::doc::{Doc, Row, Table};
use crate::emoji::Emoji;
use crate::fmt::TimeFormat;
use crate::html::Frag;
use crate::model::{InvestHome, InvestMoveKind, InvestMoveStatus, InvestNotice, InvestProblem};
use crate::screen::{Keyboard, Screen};

/// Входные цифры примера в справке. Доля инвестора — его настоящая.
const EX_DEPOSIT: i64 = 1_000;
const EX_CAPITAL: i64 = 10_000;
const EX_TURNOVER: i64 = 5_000;
/// Средняя комиссия примера, %.
const EX_FEE_PCT: i64 = 1;

/// Пример суммы вывода: 100 или меньше, если столько нет.
fn example_amount(withdrawable: Decimal) -> Decimal {
    let hundred = Decimal::ONE_HUNDRED;
    if withdrawable >= hundred || withdrawable <= Decimal::ZERO {
        hundred
    } else {
        withdrawable.trunc_with_scale(2)
    }
}

fn platform_icon(platform: Platform) -> Emoji {
    match platform {
        Platform::CryptoBot => Emoji::Cb,
        Platform::XRocket => Emoji::Xr,
    }
}

impl Ui {
    /// Сумма с активом: `1250.75 USDT`.
    fn usd(&self, value: Decimal) -> Frag {
        Frag::code(&format!(
            "{} USDT",
            crate::fmt::amount(value, domain::Asset::Usdt)
        ))
    }

    /// Кнопка «💼 Инвестиции» для главного меню инвестора.
    pub fn invest_menu_button(&self) -> KeyboardButton {
        let (text, icon) = self
            .emoji
            .button(Some(Emoji::Invest), &self.texts.menu.invest);
        KeyboardButton::new(text).maybe_icon(icon)
    }

    fn invest_button(
        &self,
        role: Option<Emoji>,
        label: &str,
        action: InvestAction,
    ) -> botapi::InlineKeyboardButton {
        self.cb_button(role, label, &Callback::Invest(action))
    }

    fn invest_home_button(&self) -> botapi::InlineKeyboardButton {
        self.invest_button(
            Some(Emoji::Invest),
            &self.texts.invest.btn_home,
            InvestAction::Home,
        )
    }

    fn move_status(&self, status: InvestMoveStatus) -> &str {
        let t = &self.texts.invest;
        match status {
            InvestMoveStatus::Requested => &t.status_requested,
            InvestMoveStatus::Processing => &t.status_processing,
            InvestMoveStatus::Done => &t.status_done,
            InvestMoveStatus::Rejected => &t.status_rejected,
            InvestMoveStatus::Failed => &t.status_failed,
            InvestMoveStatus::Manual => &t.status_manual,
        }
    }

    /// Экран инвестора: баланс, прибыль, условия, что можно вывести, последние движения.
    pub fn invest_home(&self, h: &InvestHome) -> Screen {
        let t = &self.texts.invest;
        let mut lines = vec![
            self.fill(&t.title, &[]),
            self.fill(&t.balance, &[("balance", &self.usd(h.balance))]),
            self.fill(
                &t.profit,
                &[
                    ("profit", &self.usd(h.profit)),
                    ("profit_24h", &self.usd(h.profit_24h)),
                ],
            ),
        ];
        let days = Frag::text(&h.lock_days.to_string());
        let share = self.pct(h.share_pct);
        lines.push(if h.lock_days > 0 {
            self.fill(&t.terms, &[("share", &share), ("days", &days)])
        } else {
            self.fill(&t.terms_no_lock, &[("share", &share)])
        });
        lines.push(self.fill(
            &t.withdrawable,
            &[("withdrawable", &self.usd(h.withdrawable))],
        ));
        if h.locked > Decimal::ZERO {
            lines.push(self.fill(&t.locked, &[("locked", &self.usd(h.locked))]));
        }
        let mut body = Frag::join(&lines, "\n");
        if h.moves.is_empty() && h.balance <= Decimal::ZERO {
            body.push_text("\n\n");
            body.push(&self.fill(&t.empty, &[]));
        } else if !h.moves.is_empty() {
            body.push_text("\n\n");
            body.push(&self.fill(&t.moves_title, &[]));
            let rows: Vec<Frag> = h
                .moves
                .iter()
                .map(|m| {
                    let mut what = Frag::text(match m.kind {
                        InvestMoveKind::Deposit => &t.kind_deposit,
                        InvestMoveKind::Withdrawal => &t.kind_withdrawal,
                    });
                    if let Some(a) = m.amount {
                        what.push_text(" ");
                        what.push(&self.usd(a));
                    }
                    let mut status = Frag::text(self.move_status(m.status));
                    if let Some(url) = m
                        .check_url
                        .as_deref()
                        .filter(|u| u.starts_with("https://t.me/"))
                    {
                        status.push_text(" · ");
                        status.push(&Frag::link(url, &t.check_link));
                    }
                    self.fill(
                        &t.move_line,
                        &[
                            ("at", &self.at(m.at, TimeFormat::DayTime)),
                            ("what", &what),
                            ("status", &status),
                        ],
                    )
                })
                .collect();
            body.push_text("\n");
            body.push(&Frag::trusted("<blockquote>"));
            body.push(&Frag::join(&rows, "\n"));
            body.push(&Frag::trusted("</blockquote>"));
        }
        let keyboard = Keyboard::inline_rows(vec![
            vec![
                self.invest_button(None, &t.btn_deposit, InvestAction::Deposit)
                    .style(ButtonStyle::Primary),
                self.invest_button(None, &t.btn_withdraw, InvestAction::Withdraw),
            ],
            vec![self.invest_button(None, &t.btn_howto, InvestAction::HowTo)],
            vec![self.invest_button(None, &t.btn_refresh, InvestAction::Home)],
        ]);
        self.plain(body, keyboard)
    }

    /// «Пополнить»: какой чек прислать.
    pub fn invest_deposit_prompt(&self, h: &InvestHome) -> Screen {
        let t = &self.texts.invest;
        let amount = Decimal::ONE_HUNDRED.max(h.min_deposit.ceil());
        let body = self.fill(
            &t.deposit,
            &[
                ("cb", &self.wallet(Platform::CryptoBot)),
                ("xr", &self.wallet(Platform::XRocket)),
                ("min", &self.usd(h.min_deposit)),
                (
                    "cb_cmd",
                    &Frag::code(&inline_command(Platform::CryptoBot, amount)),
                ),
                (
                    "xr_cmd",
                    &Frag::code(&inline_command(Platform::XRocket, amount)),
                ),
            ],
        );
        let keyboard = Keyboard::inline_rows(vec![vec![self.invest_button(
            None,
            &self.texts.buttons.cancel,
            InvestAction::Home,
        )]]);
        self.plain(body, keyboard)
    }

    /// «Вывести»: выбор кошелька (или «выводить нечего»).
    pub fn invest_withdraw_menu(&self, h: &InvestHome) -> Screen {
        let t = &self.texts.invest;
        let cancel = self.invest_button(None, &self.texts.buttons.cancel, InvestAction::Home);
        if h.withdrawable <= Decimal::ZERO {
            let body = self.fill(
                &t.withdraw_nothing,
                &[("days", &Frag::text(&h.lock_days.to_string()))],
            );
            return self.plain(body, Keyboard::inline_rows(vec![vec![cancel]]));
        }
        let mut body = self.fill(&t.withdraw, &[("withdrawable", &self.usd(h.withdrawable))]);
        body.push_text("\n");
        body.push(&self.withdraw_note(h));
        let wallets = Platform::ALL
            .into_iter()
            .map(|p| {
                self.invest_button(
                    Some(platform_icon(p)),
                    self.wallet_name(p),
                    InvestAction::WithdrawTo(p),
                )
            })
            .collect();
        self.plain(body, Keyboard::inline_rows(vec![wallets, vec![cancel]]))
    }

    fn withdraw_note(&self, h: &InvestHome) -> Frag {
        let t = &self.texts.invest;
        if h.auto_withdraw_max > Decimal::ZERO {
            self.fill(
                &t.withdraw_auto_note,
                &[("auto", &self.usd(h.auto_withdraw_max))],
            )
        } else {
            self.fill(&t.withdraw_owner_note, &[])
        }
    }

    /// Кошелёк выбран: сколько вывести.
    pub fn invest_withdraw_amount(&self, h: &InvestHome, platform: Platform) -> Screen {
        let t = &self.texts.invest;
        let example = Frag::code(&crate::fmt::bare_amount(example_amount(h.withdrawable)));
        let body = self.fill(
            &t.withdraw_amount,
            &[
                ("wallet", &self.wallet(platform)),
                ("example", &example),
                ("withdrawable", &self.usd(h.withdrawable)),
            ],
        );
        let all = self.button_text(
            &t.btn_all,
            &[(
                "amount",
                &crate::fmt::amount(h.withdrawable, domain::Asset::Usdt),
            )],
        );
        let keyboard = Keyboard::inline_rows(vec![
            vec![
                self.invest_button(
                    Some(platform_icon(platform)),
                    &all,
                    InvestAction::WithdrawAll(platform),
                )
                .style(ButtonStyle::Primary),
            ],
            vec![self.invest_button(None, &self.texts.buttons.cancel, InvestAction::Home)],
        ]);
        self.plain(body, keyboard)
    }

    /// Событие по пополнению или выводу.
    pub fn invest_notice(&self, n: &InvestNotice) -> Screen {
        let t = &self.texts.invest;
        let home = || Keyboard::inline_rows(vec![vec![self.invest_home_button()]]);
        match n {
            InvestNotice::DepositAccepted => {
                self.plain(self.fill(&t.deposit_accepted, &[]), Keyboard::None)
            }
            InvestNotice::DepositDone { amount, balance } => self.plain(
                self.fill(
                    &t.deposit_done,
                    &[
                        ("amount", &self.usd(*amount)),
                        ("balance", &self.usd(*balance)),
                    ],
                ),
                home(),
            ),
            InvestNotice::DepositRejected(error) => {
                let mut screen = self.error(error);
                screen.keyboard = home();
                screen
            }
            InvestNotice::DepositManual => self.plain(self.fill(&t.deposit_manual, &[]), home()),
            InvestNotice::WithdrawRequested { amount, platform } => self.plain(
                self.fill(
                    &t.withdraw_requested,
                    &[
                        ("amount", &self.usd(*amount)),
                        ("wallet", &self.wallet(*platform)),
                    ],
                ),
                home(),
            ),
            InvestNotice::WithdrawProcessing { amount, platform } => self.plain(
                self.fill(
                    &t.withdraw_processing,
                    &[
                        ("amount", &self.usd(*amount)),
                        ("wallet", &self.wallet(*platform)),
                    ],
                ),
                Keyboard::None,
            ),
            InvestNotice::WithdrawDone {
                amount,
                platform,
                check_url,
            } => self.plain(
                self.fill(
                    &t.withdraw_done,
                    &[
                        ("amount", &self.usd(*amount)),
                        ("wallet", &self.wallet(*platform)),
                    ],
                ),
                Keyboard::inline_rows(vec![
                    self.claim_row(check_url),
                    vec![self.invest_home_button()],
                ]),
            ),
            InvestNotice::WithdrawRejected { amount, reason } => self.plain(
                self.fill(
                    &t.withdraw_rejected,
                    &[
                        ("amount", &self.usd(*amount)),
                        ("reason", &Frag::text(&crate::html::clip(reason, 300))),
                    ],
                ),
                home(),
            ),
            InvestNotice::WithdrawFailed { amount } => self.plain(
                self.fill(&t.withdraw_failed, &[("amount", &self.usd(*amount))]),
                home(),
            ),
            InvestNotice::WithdrawManual { amount } => self.plain(
                self.fill(&t.withdraw_manual, &[("amount", &self.usd(*amount))]),
                home(),
            ),
        }
    }

    /// Действие не принято — ничего не списано.
    pub fn invest_problem(&self, p: &InvestProblem) -> Screen {
        let t = &self.texts.invest;
        let body = match p {
            InvestProblem::NotACheck => self.fill(&t.not_a_check, &[]),
            InvestProblem::BadAmount => {
                self.fill(&t.bad_amount, &[("example", &Frag::code("100"))])
            }
            InvestProblem::OverWithdrawable { max } => {
                self.fill(&t.over_withdrawable, &[("max", &self.usd(*max))])
            }
            InvestProblem::OpenWithdrawal => self.fill(&t.open_withdrawal, &[]),
            InvestProblem::WalletUnavailable { platform } => self.fill(
                &t.wallet_unavailable,
                &[("wallet", &self.wallet(*platform))],
            ),
            InvestProblem::Internal => self.fill(&t.internal, &[]),
        };
        self.plain(
            body,
            Keyboard::inline_rows(vec![vec![self.invest_home_button()]]),
        )
    }

    /// «Как это работает?» — rich с примером в цифрах по настоящей доле инвестора.
    pub fn invest_howto(&self, h: &InvestHome) -> Screen {
        let t = &self.texts.invest;
        let mut doc = Doc::new();
        doc.title(&self.fill(&t.howto_title, &[]));
        doc.para(&self.fill(&t.howto_lead, &[]));

        doc.subtitle(&self.fill(&t.howto_example_title, &[]));
        let deposit = Decimal::from(EX_DEPOSIT);
        let capital = Decimal::from(EX_CAPITAL);
        let turnover = Decimal::from(EX_TURNOVER);
        let fee_pct = Decimal::from(EX_FEE_PCT);
        let revenue = turnover * fee_pct / Decimal::ONE_HUNDRED;
        let part_pct = deposit * Decimal::ONE_HUNDRED / capital;
        let on_you = revenue * deposit / capital;
        let day = (on_you * h.share_pct / Decimal::ONE_HUNDRED).trunc_with_scale(2);
        let month = day * Decimal::from(30);
        let fee = self.pct(fee_pct);
        let rows: Vec<(Frag, Frag)> = vec![
            (self.fill(&t.ex_deposit, &[]), self.usd(deposit)),
            (self.fill(&t.ex_capital, &[]), self.usd(capital)),
            (self.fill(&t.ex_part, &[]), self.pct(part_pct)),
            (self.fill(&t.ex_turnover, &[]), self.usd(turnover)),
            (
                self.fill(&t.ex_revenue, &[("fee", &fee)]),
                self.usd(revenue),
            ),
            (self.fill(&t.ex_on_you, &[]), self.usd(on_you)),
            (self.fill(&t.ex_share, &[]), self.pct(h.share_pct)),
            (self.fill(&t.ex_day, &[]), Frag::bold(self.usd(day))),
            (self.fill(&t.ex_month, &[]), Frag::bold(self.usd(month))),
        ];
        doc.table(Table {
            attrs: "bordered striped compact",
            head: vec![
                self.fill(&t.howto_col_what, &[]),
                self.fill(&t.howto_col_amount, &[]),
            ],
            rows: rows
                .into_iter()
                .map(|(what, amount)| Row {
                    line: self.fill(&t.howto_row, &[("what", &what), ("amount", &amount)]),
                    cells: vec![what, amount],
                })
                .collect(),
            right: &[1],
            plain_quote: true,
        });
        doc.para(&Frag::italic(self.fill(&t.howto_example_note, &[])));

        doc.subtitle(&self.fill(&t.howto_accrual_title, &[]));
        let accrual: Vec<Frag> = t.howto_accrual.iter().map(|s| self.fill(s, &[])).collect();
        doc.list(&accrual);

        doc.subtitle(&self.fill(&t.howto_withdraw_title, &[]));
        let days = Frag::text(&h.lock_days.to_string());
        let auto = self.usd(h.auto_withdraw_max);
        let mut withdraw = vec![if h.lock_days > 0 {
            self.fill(&t.howto_withdraw_lock, &[("days", &days)])
        } else {
            self.fill(&t.howto_withdraw_no_lock, &[])
        }];
        if h.auto_withdraw_max > Decimal::ZERO {
            withdraw.push(self.fill(&t.howto_withdraw_auto, &[("auto", &auto)]));
            withdraw.push(self.fill(&t.howto_withdraw_owner, &[]));
        } else {
            withdraw.push(self.fill(&t.howto_withdraw_owner_all, &[]));
        }
        withdraw.push(self.fill(&t.howto_withdraw_one, &[]));
        doc.list(&withdraw);

        let risks_title = self.fill(&t.howto_risks_title, &[]);
        doc.details(&risks_title, true, |doc| {
            let risks: Vec<Frag> = t.howto_risks.iter().map(|s| self.fill(s, &[])).collect();
            doc.list(&risks);
        });
        doc.footer(&self.fill(&t.howto_footer, &[]));

        let keyboard = Keyboard::inline_rows(vec![
            vec![
                self.invest_button(None, &t.btn_deposit, InvestAction::Deposit)
                    .style(ButtonStyle::Primary),
                self.invest_button(None, &t.btn_withdraw, InvestAction::Withdraw),
            ],
            vec![self.invest_home_button()],
        ]);
        self.dual(doc, keyboard)
    }
}
