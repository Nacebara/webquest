//! Раздел «💼 Инвестиции» в клиентском боте (DESIGN-v0.2 Р18). Доступ выдаёт владелец в
//! админ-панели; у остальных раздела нет — ни кнопки, ни ответа на неё.
//!
//! - «Пополнить» — бот ждёт чек 15 минут: следующая ссылка на чек от инвестора идёт в
//!   `Engine::invest_deposit`, а не в обмен;
//! - «Вывести» → кошелёк → сумма (или «Всё доступное») → `Engine::invest_withdraw`;
//! - итоги (зачислено, чек вывода готов, отклонено, разбор) приходят из движка
//!   (`Engine::subscribe_invest`) и уходят инвестору отдельным сообщением.
//!
//! Деньги бот не двигает — только показывает, где они и что делать.

use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use botapi::Message;
use domain::{Decimal, Platform};
use engine::invest::{InvestError, InvestRefusal, InvestUpdate, InvestView};
use parsers::{LinkError, LinkKind, WalletLink};
use storage::invest::{Move, MoveKind, MoveStatus};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use ui::{
    InvestAction, InvestHome, InvestMoveKind, InvestMoveRow, InvestMoveStatus, InvestNotice,
    InvestProblem, UiError,
};

use crate::{Inner, TgBotError};

/// Сколько бот ждёт чек пополнения или сумму вывода после нажатия кнопки.
const WAIT: Duration = Duration::from_secs(15 * 60);

/// Чего бот ждёт от инвестора следующим сообщением.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wait {
    Deposit,
    Amount(Platform),
}

fn home(view: &InvestView) -> InvestHome {
    let f = &view.figures;
    InvestHome {
        share_pct: view.investor.share_pct,
        lock_days: view.investor.lock_days,
        min_deposit: view.investor.min_deposit,
        auto_withdraw_max: view.investor.auto_withdraw_max,
        balance: f.balance,
        profit: f.profit,
        profit_24h: f.profit_24h,
        locked: f.locked.min(f.balance),
        withdrawable: f.withdrawable(),
        moves: view.recent.iter().map(row).collect(),
    }
}

fn row(m: &Move) -> InvestMoveRow {
    InvestMoveRow {
        kind: match m.kind {
            MoveKind::Deposit => InvestMoveKind::Deposit,
            MoveKind::Withdrawal => InvestMoveKind::Withdrawal,
        },
        status: status(m.status),
        platform: m.platform,
        amount: m.amount,
        at: m.created_at,
        check_url: m.check_url.clone(),
    }
}

fn status(s: MoveStatus) -> InvestMoveStatus {
    match s {
        MoveStatus::Requested => InvestMoveStatus::Requested,
        MoveStatus::Processing => InvestMoveStatus::Processing,
        MoveStatus::Done => InvestMoveStatus::Done,
        MoveStatus::Rejected => InvestMoveStatus::Rejected,
        MoveStatus::Failed => InvestMoveStatus::Failed,
        MoveStatus::Manual => InvestMoveStatus::Manual,
    }
}

/// Сумма от инвестора: `100`, `100.5`, `100,5`, `100 usdt`, `$100`.
pub(crate) fn parse_amount(text: &str) -> Option<Decimal> {
    let lower = text.trim().to_lowercase();
    let cleaned: String = lower
        .trim_end_matches("usdt")
        .trim_start_matches('$')
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    if cleaned.is_empty() || !cleaned.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    Decimal::from_str(&cleaned)
        .ok()
        .filter(|a| *a > Decimal::ZERO && a.scale() <= 6)
}

impl Inner {
    /// Экран инвестора; `None` — доступа нет.
    async fn invest_home(&self, user: i64) -> Result<Option<InvestHome>, TgBotError> {
        Ok(self.engine.invest_view(user).await?.map(|v| home(&v)))
    }

    /// Выдан ли пользователю доступ к инвестициям (кнопка в главном меню).
    pub(crate) async fn is_investor(&self, user: i64) -> bool {
        let found = async {
            let mut conn = self.pool.acquire().await?;
            storage::invest::active(&mut conn, user).await
        }
        .await;
        match found {
            Ok(i) => i.is_some(),
            Err(e) => {
                tracing::warn!(error = %e, "investor lookup failed");
                false
            }
        }
    }

    /// Забыть, что ждали чек пополнения или сумму (нажата другая кнопка, команда).
    pub(crate) fn invest_forget(&self, user: i64) {
        self.set_wait(user, None);
    }

    fn set_wait(&self, user: i64, wait: Option<Wait>) {
        self.memory(|m| match wait {
            Some(w) => {
                m.invest_wait.insert(user, (w, Instant::now()));
            }
            None => {
                m.invest_wait.remove(&user);
            }
        });
    }

    /// Чего ждём от инвестора (просроченное — забыто).
    fn wait_of(&self, user: i64) -> Option<Wait> {
        self.memory(|m| {
            m.invest_wait.retain(|_, (_, at)| at.elapsed() < WAIT);
            m.invest_wait.get(&user).map(|(w, _)| *w)
        })
    }

    async fn invest_problem(&self, chat: i64, problem: &InvestProblem) -> Result<(), TgBotError> {
        self.reply(chat, &self.ui().invest_problem(problem)).await
    }

    /// Отказ движка → экран. `true` — пользователь не инвестор (раздел молчит).
    async fn invest_refusal(
        &self,
        chat: i64,
        platform: Option<Platform>,
        e: InvestError,
    ) -> Result<(), TgBotError> {
        let refusal = match e {
            InvestError::Refused(r) => r,
            InvestError::Engine(e) => {
                tracing::error!(error = %e, "investor action failed");
                return self.invest_problem(chat, &InvestProblem::Internal).await;
            }
        };
        let problem = match refusal {
            InvestRefusal::NotInvestor => {
                return self.reply(chat, &self.ui().no_link()).await;
            }
            InvestRefusal::NotACheck => InvestProblem::NotACheck,
            InvestRefusal::CheckUsed => {
                let screen = self
                    .ui()
                    .invest_notice(&InvestNotice::DepositRejected(UiError::CheckUsed));
                return self.reply(chat, &screen).await;
            }
            InvestRefusal::NoWallet => InvestProblem::WalletUnavailable {
                platform: platform.unwrap_or(Platform::CryptoBot),
            },
            InvestRefusal::BadAmount => InvestProblem::BadAmount,
            InvestRefusal::OverWithdrawable { max } => InvestProblem::OverWithdrawable { max },
            InvestRefusal::OpenWithdrawal => InvestProblem::OpenWithdrawal,
            other => {
                tracing::warn!(refusal = %other, "unexpected investor refusal");
                InvestProblem::Internal
            }
        };
        self.invest_problem(chat, &problem).await
    }

    /// Кнопка «💼 Инвестиции» или «Обновить».
    pub(crate) async fn invest_open(
        &self,
        chat: i64,
        user: i64,
        message: Option<&Message>,
    ) -> Result<(), TgBotError> {
        self.set_wait(user, None);
        match self.invest_home(user).await? {
            Some(h) => {
                let screen = self.ui().invest_home(&h);
                match message {
                    Some(_) => self.replace(chat, message, &screen).await,
                    None => self.reply(chat, &screen).await,
                }
            }
            // Не инвестор: кнопки у него нет — ответ как на непонятный текст.
            None => self.reply(chat, &self.ui().no_link()).await,
        }
    }

    pub(crate) async fn invest_callback(
        &self,
        chat: i64,
        user: i64,
        message: Option<&Message>,
        action: InvestAction,
    ) -> Result<(), TgBotError> {
        if action == InvestAction::Home {
            return self.invest_open(chat, user, message).await;
        }
        let Some(h) = self.invest_home(user).await? else {
            // Доступ отключили, а кнопка осталась — молча.
            return Ok(());
        };
        match action {
            InvestAction::Home => Ok(()),
            InvestAction::HowTo => {
                self.reply_doc(chat, user, &self.ui().invest_howto(&h))
                    .await
            }
            InvestAction::Deposit => {
                self.set_wait(user, Some(Wait::Deposit));
                self.replace(chat, message, &self.ui().invest_deposit_prompt(&h))
                    .await
            }
            InvestAction::Withdraw => {
                self.set_wait(user, None);
                self.replace(chat, message, &self.ui().invest_withdraw_menu(&h))
                    .await
            }
            InvestAction::WithdrawTo(platform) => {
                if h.withdrawable <= Decimal::ZERO {
                    return self
                        .replace(chat, message, &self.ui().invest_withdraw_menu(&h))
                        .await;
                }
                self.set_wait(user, Some(Wait::Amount(platform)));
                self.replace(
                    chat,
                    message,
                    &self.ui().invest_withdraw_amount(&h, platform),
                )
                .await
            }
            InvestAction::WithdrawAll(platform) => {
                self.set_wait(user, None);
                if h.withdrawable <= Decimal::ZERO {
                    return self
                        .replace(chat, message, &self.ui().invest_withdraw_menu(&h))
                        .await;
                }
                self.invest_withdraw(chat, user, platform, h.withdrawable)
                    .await
            }
        }
    }

    async fn invest_withdraw(
        &self,
        chat: i64,
        user: i64,
        platform: Platform,
        amount: Decimal,
    ) -> Result<(), TgBotError> {
        match self.engine.invest_withdraw(user, platform, amount).await {
            Ok(m) => {
                let notice = match m.status {
                    MoveStatus::Requested => InvestNotice::WithdrawRequested { amount, platform },
                    _ => InvestNotice::WithdrawProcessing { amount, platform },
                };
                self.reply(chat, &self.ui().invest_notice(&notice)).await
            }
            Err(e) => self.invest_refusal(chat, Some(platform), e).await,
        }
    }

    /// Сообщение, пока бот ждёт чек пополнения или сумму вывода. `true` — обработано тут.
    pub(crate) async fn invest_message(
        &self,
        chat: i64,
        user: i64,
        text: &str,
        found: &[Result<WalletLink, LinkError>],
    ) -> Result<bool, TgBotError> {
        let Some(wait) = self.wait_of(user) else {
            return Ok(false);
        };
        match wait {
            Wait::Deposit => {
                let Some(first) = found.first() else {
                    return Ok(false);
                };
                self.set_wait(user, None);
                let link = match first {
                    Ok(link) if link.kind == LinkKind::Check => link,
                    _ => {
                        self.invest_problem(chat, &InvestProblem::NotACheck).await?;
                        return Ok(true);
                    }
                };
                match self.engine.invest_deposit(user, link).await {
                    Ok(move_id) => {
                        tracing::info!(move_id, user, "investor deposit check accepted");
                        self.reply(
                            chat,
                            &self.ui().invest_notice(&InvestNotice::DepositAccepted),
                        )
                        .await?;
                    }
                    Err(e) => self.invest_refusal(chat, Some(link.platform), e).await?,
                }
                Ok(true)
            }
            Wait::Amount(platform) => {
                if !found.is_empty() {
                    // Прислали ссылку — это уже не сумма: обычная обработка.
                    self.set_wait(user, None);
                    return Ok(false);
                }
                let Some(amount) = parse_amount(text) else {
                    self.invest_problem(chat, &InvestProblem::BadAmount).await?;
                    return Ok(true);
                };
                self.set_wait(user, None);
                self.invest_withdraw(chat, user, platform, amount).await?;
                Ok(true)
            }
        }
    }
}

/// Итог движения → сообщение инвестору. Промежуточные шаги, о которых он уже знает из ответа
/// на свою кнопку (заявка у владельца, автовывод начат), не повторяем.
async fn notify(inner: &Inner, update: InvestUpdate) -> Result<(), TgBotError> {
    let (m, balance) = {
        let mut conn = inner.pool.acquire().await?;
        let m = storage::invest::get(&mut conn, update.move_id, false).await?;
        let lock = storage::invest::investor(&mut conn, m.user_id)
            .await?
            .map_or(0, |i| i.lock_days);
        let balance = storage::invest::figures(&mut conn, m.user_id, lock)
            .await?
            .balance;
        (m, balance)
    };
    let amount = m.amount.unwrap_or_default();
    let notice = match (m.kind, m.status) {
        (MoveKind::Deposit, MoveStatus::Done) => InvestNotice::DepositDone { amount, balance },
        (MoveKind::Deposit, MoveStatus::Failed) => InvestNotice::DepositRejected(
            crate::orders::intake_failure(m.note.as_deref(), inner.cfg.bind_username.as_deref()),
        ),
        (MoveKind::Deposit, MoveStatus::Manual) => InvestNotice::DepositManual,
        // Одобрил владелец — инвестор ещё не знает, что чек создаётся.
        (MoveKind::Withdrawal, MoveStatus::Processing) if m.decided_by.is_some() => {
            InvestNotice::WithdrawProcessing {
                amount,
                platform: m.platform,
            }
        }
        (MoveKind::Withdrawal, MoveStatus::Done) => match m.check_url.clone() {
            Some(check_url) => InvestNotice::WithdrawDone {
                amount,
                platform: m.platform,
                check_url,
            },
            // Закрыто владельцем вручную: чек он передаст сам.
            None => return Ok(()),
        },
        (MoveKind::Withdrawal, MoveStatus::Rejected) => InvestNotice::WithdrawRejected {
            amount,
            reason: m.note.clone().unwrap_or_default(),
        },
        (MoveKind::Withdrawal, MoveStatus::Failed) => InvestNotice::WithdrawFailed { amount },
        (MoveKind::Withdrawal, MoveStatus::Manual) => InvestNotice::WithdrawManual { amount },
        _ => return Ok(()),
    };
    inner
        .reply(m.user_id, &inner.ui().invest_notice(&notice))
        .await
}

/// События движений инвесторов → сообщения им.
pub(crate) async fn listen(
    inner: Arc<Inner>,
    mut updates: broadcast::Receiver<InvestUpdate>,
    stop: CancellationToken,
) {
    loop {
        let update = tokio::select! {
            () = stop.cancelled() => return,
            u = updates.recv() => u,
        };
        match update {
            Ok(u) => {
                if let Err(e) = notify(&inner, u).await {
                    tracing::warn!(move_id = u.move_id, error = %e, "investor notice not sent");
                }
            }
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "investor notices lagged");
            }
            Err(RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn amounts_are_read_leniently_but_strictly() {
        assert_eq!(parse_amount("100"), Some(dec!(100)));
        assert_eq!(parse_amount(" 100,5 "), Some(dec!(100.5)));
        assert_eq!(parse_amount("100 USDT"), Some(dec!(100)));
        assert_eq!(parse_amount("$12.25"), Some(dec!(12.25)));
        assert_eq!(parse_amount("1 000"), Some(dec!(1000)));
        for bad in ["", "0", "-5", "abc", "1e3", "10.1234567", "5 TON", "1.2.3"] {
            assert_eq!(parse_amount(bad), None, "{bad}");
        }
    }
}
