//! Чужие счета: просмотр и оплата (DESIGN-v0.2 Р4, Р12) — по живым экранам обоих кошельков
//! (`parsers::screens::classify_invoice_screen`, запись владельца 6 ноября 2026).
//!
//! CryptoBot: `/start IV…` → «Выберите монету для оплаты счета #IV… на сумму $5.» (счёт в
//! фиате, сумма в USDT — на кнопке монеты) или сразу «📥 Подтвердите оплату счёта …
//! Отправляете: X USDT» → сверка суммы → «💳 Оплатить X USDT» → дальше одно из:
//! «Выберите сеть для оплаты счёта» (денег не хватает, ничего не списано), «👉 Вы уверены,
//! что доверяете этому получателю?» → «💸 Отправить платеж получателю», или сразу оплата:
//! «💸» → «👌» и «📥 Вы оплатили счёт #IV… на сумму …».
//!
//! xRocket: `/start inv_…` → «🧾 Счёт #INV… Сумма: X … Выберите способ оплаты:» → «xRocket»
//! (не «Адрес») → «🔸 Подтвердите ваш платёж на X» или «🔺 Недостаточно средств» → сверка
//! суммы → «Оплатить» → «⏱ Загрузка» и «📋 Вы успешно оплатили счёт на сумму X…».
//!
//! Деньги двигает CryptoBot «💳 Оплатить» (знакомый получатель) или «💸 Отправить платеж
//! получателю», xRocket «Оплатить». Каждую такую кнопку нажимаем один раз и больше никогда:
//! неясный исход — `Unknown`, решает человек по статусу счёта. «Оплатить анонимно»,
//! «Добавить комментарий», «Адрес», сети пополнения не нажимаем. Код-пароль CryptoBot не
//! вводим никогда (`NeedsHuman`, деньги не двигались); движок держит траты под «лимитом без
//! код-пароля» (Р11). Мини-приложений не открываем: кнопка оплаты, которая ведёт в WebView, —
//! `NeedsHuman`.

use domain::{Asset, Money, Platform};
use parsers::screens::{
    self, InvoiceScreen, SEND_PAYMENT_BUTTON, VIA_WALLET_BUTTON, classify_invoice_screen,
};

use super::conv::{Conv, SendError};
use super::gate::Pace;
use super::{UserbotWallet, rid, typed_amount};
use crate::transport::{ButtonKind, Chat, RawButton, RawMessage, Transport};
use crate::wallet::{BotError, InvoiceInfo, OpTag, PayOutcome};

/// Сколько экранов без денег (монета, способ оплаты) можно пройти до подтверждения.
const MAX_STEPS: usize = 3;

fn screen_of(platform: Platform, m: &RawMessage) -> InvoiceScreen {
    classify_invoice_screen(platform, &m.text)
}

/// Экран, на котором можно остановиться (не «💸», не «⏱ Загрузка», не чужое).
fn settled(s: &InvoiceScreen) -> bool {
    !matches!(s, InvoiceScreen::Progress | InvoiceScreen::Unknown)
}

fn button_texts(m: &RawMessage) -> Vec<String> {
    m.buttons.iter().flatten().map(|b| b.text.clone()).collect()
}

/// Открытая карточка: наша команда, первый экран бота и признак «команду отправил прошлый вызов».
struct Opened {
    cmd: RawMessage,
    screen: RawMessage,
    earlier_call: bool,
}

/// Открыть счёт и дождаться первого экрана. Ничего не платит: любая неудача — «не выполнялось».
async fn open_card<T: Transport>(
    conv: &mut Conv<'_, T>,
    param: &str,
    random_id: i64,
) -> Result<Opened, BotError> {
    let platform = conv.platform();
    let chat = Chat::WalletBot(platform);
    conv.wait_quarantine(chat).await;
    let mark = conv.mark().await;
    let sent = match conv
        .send(chat, &format!("/start {param}"), random_id, Pace::Bot)
        .await
    {
        Ok(sent) if sent.earlier_call => {
            return Ok(Opened {
                screen: sent.msg.clone(),
                cmd: sent.msg,
                earlier_call: true,
            });
        }
        Ok(sent) => sent,
        Err(SendError::NotSent(e)) => return Err(e),
        Err(SendError::Unknown(detail)) => {
            conv.quarantine(chat);
            return Err(BotError::Unavailable(format!("invoice command: {detail}")));
        }
    };
    let deadline = conv.deadline();
    let screen = conv
        .wait(chat, sent.msg.id, mark, deadline, sent.located, |m| {
            settled(&screen_of(platform, m))
        })
        .await;
    match screen {
        Some(screen) => Ok(Opened {
            cmd: sent.msg,
            screen,
            earlier_call: false,
        }),
        None => {
            conv.quarantine(chat);
            Err(BotError::Unavailable(
                "the bot did not show the invoice".to_owned(),
            ))
        }
    }
}

pub(crate) async fn inspect<T: Transport>(
    w: &UserbotWallet<T>,
    param: &str,
) -> Result<InvoiceInfo, BotError> {
    let mut conv = Conv::open(w, None).await;
    let opened = open_card(&mut conv, param, rid::new_random_id()).await?;
    Ok(InvoiceInfo {
        param: param.to_owned(),
        card: screens::read_invoice_card(
            w.platform,
            &opened.screen.text,
            &button_texts(&opened.screen),
        ),
    })
}

fn needs_human(reason: impl Into<String>) -> PayOutcome {
    PayOutcome::NeedsHuman {
        reason: reason.into(),
    }
}

fn unknown(detail: impl Into<String>) -> PayOutcome {
    PayOutcome::Unknown {
        detail: detail.into(),
    }
}

/// Экран до нажатия, которое платит: что он значит для оплаты. `None` — идём дальше.
fn before_pay(screen: &InvoiceScreen) -> Option<PayOutcome> {
    match screen {
        InvoiceScreen::ChooseCoin { .. }
        | InvoiceScreen::ChooseMethod { .. }
        | InvoiceScreen::Confirm { .. } => None,
        InvoiceScreen::InsufficientFunds => Some(PayOutcome::InsufficientFunds),
        // Мы ещё ничего не нажимали: оплатил кто-то другой.
        InvoiceScreen::Paid { .. } | InvoiceScreen::AlreadyPaid => Some(PayOutcome::AlreadyPaid),
        InvoiceScreen::Expired => Some(PayOutcome::Expired),
        InvoiceScreen::NotFound => Some(needs_human("invoice not found")),
        InvoiceScreen::OpenAmount => Some(needs_human(
            "open-amount invoice: the payer chooses the amount",
        )),
        InvoiceScreen::PasscodeRequired => Some(needs_human(
            "CryptoBot asks for the passcode: spending is over the passcode-free limit",
        )),
        InvoiceScreen::TrustRecipient => Some(needs_human("unexpected screen before payment")),
        InvoiceScreen::Progress | InvoiceScreen::Unknown => {
            Some(needs_human("unrecognised invoice screen"))
        }
    }
}

fn callback_of(b: &RawButton) -> Option<&[u8]> {
    match &b.kind {
        ButtonKind::Callback(data) => Some(data),
        _ => None,
    }
}

/// Кнопка, которая ведёт к подтверждению: монета (CryptoBot) или способ «xRocket».
fn next_button<'a>(m: &'a RawMessage, screen: &InvoiceScreen, asset: Asset) -> Option<&'a [u8]> {
    m.buttons.iter().flatten().find_map(|b| {
        let fits = match screen {
            InvoiceScreen::ChooseCoin { .. } => screens::is_asset_button(&b.text, asset),
            InvoiceScreen::ChooseMethod { .. } => screens::text_matches(&b.text, VIA_WALLET_BUTTON),
            _ => false,
        };
        if fits { callback_of(b) } else { None }
    })
}

/// Сумма к оплате, если экран до подтверждения её показывает.
fn shown_amount(
    m: &RawMessage,
    screen: &InvoiceScreen,
    asset: Asset,
) -> Option<(domain::Decimal, Asset)> {
    match screen {
        InvoiceScreen::ChooseMethod { amount, .. } => *amount,
        InvoiceScreen::ChooseCoin { .. } => m
            .buttons
            .iter()
            .flatten()
            .find_map(|b| screens::coin_option(m_platform(m), &b.text, asset))
            .map(|v| (v, asset)),
        _ => None,
    }
}

fn m_platform(m: &RawMessage) -> Platform {
    match m.chat {
        Chat::WalletBot(p) => p,
        Chat::SavedMessages => Platform::CryptoBot,
    }
}

pub(crate) async fn pay<T: Transport>(
    w: &UserbotWallet<T>,
    param: &str,
    asset: Asset,
    expected: &Money,
    tag: &OpTag,
) -> Result<PayOutcome, BotError> {
    let platform = w.platform;
    let chat = Chat::WalletBot(platform);
    let mut conv = Conv::open(w, Some(&tag.key)).await;
    let opened = open_card(
        &mut conv,
        param,
        rid::derive(tag.random_id, rid::INVOICE_OPEN),
    )
    .await?;
    // Эту оплату уже начинал прошлый вызов: «Оплатить» могли нажать. Второго нажатия нет.
    if opened.earlier_call {
        tracing::warn!(op = %tag.key, "invoice payment was started earlier, not pressing again");
        return Ok(unknown(
            "payment was started earlier; check the invoice status",
        ));
    }
    let cmd_id = opened.cmd.id;
    let mut msg = opened.screen;
    let mut screen = screen_of(platform, &msg);
    let number = |s: &InvoiceScreen| match s {
        InvoiceScreen::ChooseCoin { number, .. }
        | InvoiceScreen::ChooseMethod { number, .. }
        | InvoiceScreen::Confirm { number, .. } => number.clone(),
        _ => None,
    };
    let mut invoice_number = number(&screen);

    // Экраны без денег: монета, способ оплаты — до подтверждения с суммой.
    for _ in 0..MAX_STEPS {
        if let Some(outcome) = before_pay(&screen) {
            return Ok(outcome);
        }
        if matches!(screen, InvoiceScreen::Confirm { .. }) {
            break;
        }
        // Сумма видна уже здесь (карточка xRocket, кнопка монеты CryptoBot): не та — дальше не идём.
        if let Some(shown) = shown_amount(&msg, &screen, asset)
            && shown != (expected.amount(), expected.asset())
        {
            return Ok(needs_human(format!(
                "invoice shows {shown:?}, expected {} {}",
                typed_amount(expected),
                expected.asset().code()
            )));
        }
        let Some(data) = next_button(&msg, &screen, asset).map(<[u8]>::to_vec) else {
            return Ok(needs_human(format!(
                "no button to pay in {} on the invoice",
                asset.code()
            )));
        };
        let mark = conv.mark().await;
        match conv.press(chat, msg.id, &data).await {
            Ok(answer) if answer.alert => {
                return Ok(needs_human(format!(
                    "the bot answered {:?}",
                    answer.message
                )));
            }
            Ok(_) => {}
            Err(SendError::NotSent(e)) => return Err(e),
            Err(SendError::Unknown(detail)) => {
                tracing::warn!(%detail, "invoice step outcome unknown, waiting for the screen");
            }
        }
        let before = screen.clone();
        let deadline = conv.deadline();
        msg = conv
            .wait(chat, cmd_id, mark, deadline, false, |m| {
                let s = screen_of(platform, m);
                settled(&s) && s != before
            })
            .await
            .ok_or_else(|| BotError::Unavailable("no invoice screen after the step".to_owned()))?;
        screen = screen_of(platform, &msg);
        invoice_number = number(&screen).or(invoice_number);
    }
    let InvoiceScreen::Confirm { amount: due, .. } = screen else {
        return Ok(needs_human("no payment confirmation screen"));
    };

    // Сумма к оплате должна совпасть с той, под которую клиент прислал деньги.
    if due != Some((expected.amount(), expected.asset())) {
        return Ok(needs_human(format!(
            "amount due {due:?} differs from expected {} {}",
            typed_amount(expected),
            expected.asset().code()
        )));
    }
    let Some(button) = msg
        .buttons
        .iter()
        .flatten()
        .find(|b| screens::is_pay_button(&b.text))
    else {
        return Ok(needs_human("no pay button"));
    };
    let data = match &button.kind {
        ButtonKind::Callback(data) => data.clone(),
        ButtonKind::WebApp(_) => {
            return Ok(needs_human("the pay button opens a mini app: pay manually"));
        }
        other => return Ok(needs_human(format!("unsupported pay button {other:?}"))),
    };
    let paying = Paying {
        cmd_id,
        number: invoice_number,
        expected: *expected,
    };
    paying.press(&mut conv, msg.id, &data, "pay").await
}

/// Нажатие, которое может заплатить, и всё после него.
struct Paying {
    cmd_id: i32,
    number: Option<String>,
    expected: Money,
}

impl Paying {
    /// Нажать один раз и дождаться итога. CryptoBot для нового получателя сначала спрашивает
    /// «доверяете?» — тогда одно нажатие «💸 Отправить платеж получателю».
    async fn press<T: Transport>(
        &self,
        conv: &mut Conv<'_, T>,
        msg_id: i32,
        data: &[u8],
        what: &str,
    ) -> Result<PayOutcome, BotError> {
        let platform = conv.platform();
        let chat = Chat::WalletBot(platform);
        let mark = conv.mark().await;
        match conv.press(chat, msg_id, data).await {
            Ok(answer) => {
                if let Some(text) = answer.message.as_deref() {
                    let s = classify_invoice_screen(platform, text);
                    if settled(&s) && !matches!(s, InvoiceScreen::Confirm { .. }) {
                        return Ok(self.result(&s));
                    }
                }
            }
            // Нажатие не ушло: ничего не списано. Повтор операции только сверяет (метка — команда).
            Err(SendError::NotSent(e)) => return Err(e),
            Err(SendError::Unknown(detail)) => {
                tracing::warn!(%detail, button = what, "pay press outcome unknown, waiting for the bot");
            }
        }
        let deadline = conv.deadline();
        let reply = conv
            .wait(chat, self.cmd_id, mark, deadline, false, |m| {
                matches!(
                    screen_of(platform, m),
                    InvoiceScreen::Paid { .. }
                        | InvoiceScreen::TrustRecipient
                        | InvoiceScreen::InsufficientFunds
                        | InvoiceScreen::AlreadyPaid
                        | InvoiceScreen::Expired
                        | InvoiceScreen::NotFound
                        | InvoiceScreen::PasscodeRequired
                )
            })
            .await;
        let Some(reply) = reply else {
            conv.quarantine(chat);
            return Ok(unknown(format!("no reply after '{what}'")));
        };
        let screen = screen_of(platform, &reply);
        if screen != InvoiceScreen::TrustRecipient {
            return Ok(self.result(&screen));
        }
        if what != "pay" {
            return Ok(unknown("the bot asks to trust the recipient again"));
        }
        let Some(data) = reply.buttons.iter().flatten().find_map(|b| {
            if screens::text_matches(&b.text, SEND_PAYMENT_BUTTON) {
                callback_of(b).map(<[u8]>::to_vec)
            } else {
                None
            }
        }) else {
            // «Оплатить» показало вопрос, но кнопки отправки нет: ничего не списано.
            return Ok(needs_human(
                "the bot asks to trust the recipient, no send button",
            ));
        };
        Box::pin(self.press(conv, reply.id, &data, "send payment")).await
    }

    /// Итог по экрану после нажатия.
    fn result(&self, screen: &InvoiceScreen) -> PayOutcome {
        match screen {
            InvoiceScreen::Paid { number, amount } => {
                if let (Some(want), Some(got)) = (&self.number, number)
                    && want != got
                {
                    return unknown(format!("receipt is for #{got}, expected #{want}"));
                }
                match amount {
                    Some((a, asset))
                        if *a == self.expected.amount() && *asset == self.expected.asset() =>
                    {
                        PayOutcome::Paid
                    }
                    other => unknown(format!(
                        "paid {other:?}, expected {} {}",
                        typed_amount(&self.expected),
                        self.expected.asset().code()
                    )),
                }
            }
            InvoiceScreen::InsufficientFunds => PayOutcome::InsufficientFunds,
            // Нажали мы, а бот говорит «уже оплачен»: мог оплатить кто-то другой — или мы.
            InvoiceScreen::AlreadyPaid => unknown("the bot says the invoice is already paid"),
            InvoiceScreen::Expired => PayOutcome::Expired,
            InvoiceScreen::PasscodeRequired => needs_human(
                "CryptoBot asks for the passcode: spending is over the passcode-free limit",
            ),
            other => unknown(format!("unexpected screen after paying: {other:?}")),
        }
    }
}
