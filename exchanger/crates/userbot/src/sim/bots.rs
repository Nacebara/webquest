//! Поведение ботов @send (CryptoBot) и @xrocket в симуляторе — по живой записи владельца
//! (тексты — [`texts`]). Функции меняют мир и возвращают сообщения к публикации; сбои и
//! задержки накладывает транспорт.
//!
//! Как у живых ботов:
//! - меню чеков и счёт бот правит на месте; callback-данные — команды экрана («create»,
//!   «count-1», «Success», «Pay»), смысл которых зависит от того, что на экране сейчас
//!   ([`Screen`] у каждого сообщения);
//! - CryptoBot на введённую сумму отвечает НОВЫМ сообщением: экран чека, «Недостаточно монет»
//!   или, если сумма укладывается в баланс хотя бы дважды, «Пришлите количество активаций…»
//!   с «Пропустить ›» (одна активация — чек создаётся этим нажатием);
//! - xRocket на введённую сумму отвечает ПРАВКОЙ сообщения меню: «Сумма чека … подтвердите»
//!   или «🔺 Недостаточно средств»; чек создаёт «✅ Подтверждаю»; «Удалить» — молча, правкой
//!   обратно в «Персональные чеки»;
//! - счёт CryptoBot: в фиате — выбор монеты, в монете — сразу подтверждение; «💳 Оплатить» →
//!   «Выберите сеть…», если денег мало, «Вы уверены, что доверяете этому получателю?» для
//!   нового получателя или оплата: «💸» → «👌» и «📥 Вы оплатили счёт…»;
//! - счёт xRocket: способ оплаты «xRocket» → «🔸 Подтвердите ваш платёж» или «🔺 Недостаточно
//!   средств» → «Оплатить» → «⏱ Загрузка» и «📋 Вы успешно оплатили счёт…».

use std::str::FromStr;

use domain::{Asset, Decimal, Money, Platform};
use rust_decimal::prelude::ToPrimitive;

use super::model::{
    AccountId, Call, CheckOrigin, CheckSpec, CheckStatus, ClaimOutcome, InvoiceAmount, SimError,
    SimInvoiceStatus,
};
use super::state::{Dialog, Draft, Op, Outbound, Screen, WorldState};
use super::texts;
use crate::transport::{ButtonKind, CallbackAnswer, Chat, RawButton, TransportError};

const CB: Platform = Platform::CryptoBot;
const XR: Platform = Platform::XRocket;

fn callback(text: &str, data: &str) -> RawButton {
    RawButton {
        text: text.to_owned(),
        kind: ButtonKind::Callback(data.as_bytes().to_vec()),
    }
}

fn switch_inline(text: &str, query: &str) -> RawButton {
    RawButton {
        text: text.to_owned(),
        kind: ButtonKind::SwitchInline(query.to_owned()),
    }
}

fn rpc(name: &str) -> TransportError {
    TransportError::Rpc {
        code: 400,
        name: name.to_owned(),
    }
}

fn reply(account: AccountId, platform: Platform, draft: Draft) -> Outbound {
    Outbound {
        account,
        chat: Chat::WalletBot(platform),
        op: Op::New(draft),
    }
}

fn edit(account: AccountId, platform: Platform, msg_id: i32, draft: Draft) -> Outbound {
    Outbound {
        account,
        chat: Chat::WalletBot(platform),
        op: Op::Edit(msg_id, draft),
    }
}

fn staged(account: AccountId, platform: Platform, first: Draft, then: Draft) -> Outbound {
    Outbound {
        account,
        chat: Chat::WalletBot(platform),
        op: Op::Staged(first, vec![then]),
    }
}

fn alert(text: &str) -> CallbackAnswer {
    CallbackAnswer {
        message: Some(text.to_owned()),
        alert: true,
        url: None,
    }
}

/// Сумма, введённая человеком: `10`, `10.5`, `10,5`. Только положительная.
pub(crate) fn parse_user_amount(s: &str) -> Option<Decimal> {
    let s = s.trim().replace(',', ".");
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let d = Decimal::from_str(&s).ok()?;
    (d > Decimal::ZERO).then(|| d.normalize())
}

fn is_invoice_code(platform: Platform, code: &str) -> bool {
    match platform {
        Platform::CryptoBot => code.starts_with("IV"),
        Platform::XRocket => code.starts_with("inv_"),
    }
}

/// Минимальная сумма чека: CryptoBot $0.02, xRocket 0.01 в монете.
fn min_check(platform: Platform, asset: Asset) -> Money {
    let d = match (platform, asset) {
        (Platform::CryptoBot, Asset::Usdt) => Decimal::new(2, 2),
        (Platform::CryptoBot, Asset::Ton) => Decimal::new(1, 2),
        (Platform::XRocket, _) => Decimal::new(1, 2),
    };
    Money::new(d, asset).unwrap_or_else(|_| Money::zero(asset))
}

/// Код монеты xRocket в callback-данных (`Coin_30` — USDT).
fn xr_coin(asset: Asset) -> &'static str {
    match asset {
        Asset::Usdt => "Coin_30",
        Asset::Ton => "Coin_1",
    }
}

fn xr_coin_asset(data: &str) -> Option<Asset> {
    Asset::ALL.into_iter().find(|a| xr_coin(*a) == data)
}

/// `pay-<…>` / `confirm-delete-check-<n>`: числовой хвост.
fn num_after<'a>(data: &'a str, prefix: &str) -> Option<&'a str> {
    data.strip_prefix(prefix).filter(|rest| !rest.is_empty())
}

const BAD_AMOUNT: &str = "Неверная сумма. Отправьте число, например 10.5.";
const UNSUPPORTED: &str = "В симуляторе эта кнопка не работает.";

// ---------- текст в чат бота ----------

/// Аккаунт написал боту текст (уже сохранён как исходящее сообщение).
pub(crate) fn on_text(
    st: &mut WorldState,
    acc: AccountId,
    platform: Platform,
    text: &str,
    random_id: i64,
) -> Vec<Outbound> {
    let trimmed = text.trim();
    if trimmed.starts_with('/') {
        st.set_dialog(acc, platform, Dialog::Idle);
        let mut parts = trimmed.split_whitespace();
        let cmd = parts
            .next()
            .unwrap_or_default()
            .split('@')
            .next()
            .unwrap_or_default();
        let arg = parts.next();
        return match (cmd, arg, platform) {
            ("/start", None, _) => {
                vec![reply(acc, platform, Draft::text(texts::welcome(platform)))]
            }
            ("/start", Some(p), _) if is_invoice_code(platform, p) => {
                vec![open_invoice(st, acc, platform, p)]
            }
            ("/start", Some(p), _) => claim(st, acc, platform, p, None).1,
            ("/wallet", _, _) => vec![wallet(st, acc, platform)],
            ("/checks", _, Platform::CryptoBot) => vec![reply(acc, platform, cb_menu(st, acc))],
            ("/cheques", _, Platform::XRocket) => vec![reply(acc, platform, xr_menu())],
            _ => vec![reply(acc, platform, Draft::text(texts::UNKNOWN_COMMAND))],
        };
    }
    match st.dialog(acc, platform) {
        Dialog::AwaitCheckAmount { asset, menu } => {
            let Some(m) = parse_user_amount(trimmed).and_then(|d| Money::new(d, asset).ok()) else {
                return vec![reply(acc, platform, Draft::text(BAD_AMOUNT))];
            };
            match platform {
                Platform::CryptoBot => {
                    st.set_dialog(acc, platform, Dialog::Idle);
                    vec![reply(acc, platform, cb_after_amount(st, acc, m, random_id))]
                }
                Platform::XRocket => {
                    vec![edit(
                        acc,
                        platform,
                        menu,
                        xr_after_amount(st, acc, m, random_id),
                    )]
                }
            }
        }
        Dialog::AwaitCheckPassword(code) => claim(st, acc, platform, &code, Some(trimmed)).1,
        Dialog::Idle => vec![reply(acc, platform, Draft::text(texts::UNKNOWN_COMMAND))],
    }
}

fn wallet(st: &WorldState, acc: AccountId, platform: Platform) -> Outbound {
    let balances: Vec<(Money, Decimal)> = Asset::ALL
        .iter()
        .map(|a| {
            let m = st.balance_money(acc, platform, *a);
            (m, st.usd_value(&m))
        })
        .collect();
    reply(
        acc,
        platform,
        Draft::text(texts::wallet(platform, &balances)),
    )
}

// ---------- активация чека ----------

/// `/start <код>` от аккаунта `acc` (или ввод пароля, если `password` задан).
/// Возвращает исход и сообщения: ответ активирующему и уведомление создателю.
pub(crate) fn claim(
    st: &mut WorldState,
    acc: AccountId,
    platform: Platform,
    code: &str,
    password: Option<&str>,
) -> (ClaimOutcome, Vec<Outbound>) {
    let by_us_before = st
        .checks
        .get(code)
        .is_some_and(|c| c.claimed_by.contains(&acc));
    let (outcome, notice) = evaluate_claim(st, acc, platform, code, password);
    let back = match platform {
        Platform::CryptoBot => callback("Открыть кошелёк", "wallet-as-new"),
        Platform::XRocket => callback("‹ Назад", "Back"),
    };
    // Свой чек (третья запись владельца) зачисляется, а уведомление создателю приходит в тот же
    // чат. CryptoBot — между «🕘 Получение…» и правкой «📥 Вы получили…» (74248 → 74249 →
    // правка 74248); xRocket — после правки (72623 → 72624, 73357 → 73358): это обычный
    // `Staged` и уведомление следом.
    let own = platform == CB
        && notice
            .as_ref()
            .is_some_and(|n| n.account == acc && n.chat == Chat::WalletBot(platform));
    let mut notice = notice;
    let first = match outcome {
        ClaimOutcome::Received(m) if own => {
            let usd = st.usd_value(&m);
            let between = match notice.take().map(|n| n.op) {
                Some(Op::New(d)) => d,
                _ => Draft::text(String::new()),
            };
            Op::StagedAround(Box::new((
                Draft::text(texts::receiving(platform, &m, usd)),
                between,
                Draft::text(texts::received(platform, &m, usd)).with_buttons(vec![vec![back]]),
            )))
        }
        ClaimOutcome::Received(m) => {
            let usd = st.usd_value(&m);
            Op::Staged(
                Draft::text(texts::receiving(platform, &m, usd)),
                vec![
                    Draft::text(texts::received(platform, &m, usd)).with_buttons(vec![vec![back]]),
                ],
            )
        }
        other => {
            let text = match other {
                ClaimOutcome::AlreadyActivated if platform == XR && by_us_before => {
                    texts::XR_ALREADY_ACTIVATED_BY_YOU.to_owned()
                }
                ClaimOutcome::AlreadyActivated => texts::already_activated(platform).to_owned(),
                ClaimOutcome::NotFound => texts::not_found(platform, code).to_owned(),
                ClaimOutcome::NotForYou => texts::not_for_you(platform).to_owned(),
                ClaimOutcome::NeedsSubscription => {
                    let channel = st
                        .checks
                        .get(code)
                        .and_then(|c| c.subscription.clone())
                        .unwrap_or_default();
                    texts::needs_subscription(&channel)
                }
                ClaimOutcome::Captcha => texts::CAPTCHA.to_owned(),
                ClaimOutcome::PasswordRequired => texts::PASSWORD_PROMPT.to_owned(),
                ClaimOutcome::WrongPassword => texts::WRONG_PASSWORD.to_owned(),
                ClaimOutcome::PremiumOnly => texts::PREMIUM_ONLY.to_owned(),
                ClaimOutcome::Received(_) => String::new(),
            };
            Op::New(Draft::text(text).with_buttons(vec![vec![back]]))
        }
    };
    let dialog = match outcome {
        ClaimOutcome::PasswordRequired | ClaimOutcome::WrongPassword => {
            Dialog::AwaitCheckPassword(code.to_owned())
        }
        _ => Dialog::Idle,
    };
    st.set_dialog(acc, platform, dialog);
    let mut outs = vec![Outbound {
        account: acc,
        chat: Chat::WalletBot(platform),
        op: first,
    }];
    outs.extend(notice);
    (outcome, outs)
}

fn evaluate_claim(
    st: &mut WorldState,
    acc: AccountId,
    platform: Platform,
    code: &str,
    password: Option<&str>,
) -> (ClaimOutcome, Option<Outbound>) {
    let now = st.now_ms;
    let window = st.xrocket_fresh_window_ms;
    let Some((premium, channels)) = st
        .accounts
        .get(&acc)
        .map(|a| (a.premium, a.channels.clone()))
    else {
        return (ClaimOutcome::NotFound, None);
    };
    let Some(check) = st.checks.get_mut(code) else {
        return (ClaimOutcome::NotFound, None);
    };
    if check.platform != platform || check.status == CheckStatus::Cancelled {
        return (ClaimOutcome::NotFound, None);
    }
    // xRocket: «не найден» на совсем свежий чек, со второй попытки — как обычно
    // (lovec ledger.rs:19-20, AUDIT-2.md:1850).
    if platform == Platform::XRocket
        && !check.fresh_not_found_served
        && window.is_some_and(|w| now - check.created_at_ms < w)
    {
        check.fresh_not_found_served = true;
        return (ClaimOutcome::NotFound, None);
    }
    if check.status == CheckStatus::Claimed || check.claimed_by.contains(&acc) {
        return (ClaimOutcome::AlreadyActivated, None);
    }
    if check.for_user.is_some_and(|u| u != acc) {
        return (ClaimOutcome::NotForYou, None);
    }
    if check.premium_only && !premium {
        return (ClaimOutcome::PremiumOnly, None);
    }
    if check
        .subscription
        .as_ref()
        .is_some_and(|ch| !channels.contains(ch))
    {
        return (ClaimOutcome::NeedsSubscription, None);
    }
    if check.captcha {
        return (ClaimOutcome::Captcha, None);
    }
    if let Some(expected) = &check.password {
        match password {
            None => return (ClaimOutcome::PasswordRequired, None),
            Some(p) if p != expected => return (ClaimOutcome::WrongPassword, None),
            Some(_) => {}
        }
    }
    check.claimed_by.push(acc);
    if check.claimed_by.len() >= usize::try_from(check.activations).unwrap_or(usize::MAX) {
        check.status = CheckStatus::Claimed;
    }
    let (amount, creator) = (check.amount, check.creator);
    // Аккаунт проверен в начале, зачисление не падает.
    let _ = st.credit(acc, platform, amount);
    let usd = st.usd_value(&amount);
    let notice = creator.map(|c| {
        let who = st.display(acc);
        reply(
            c,
            platform,
            Draft::text(texts::claim_notice(platform, &amount, usd, &who)),
        )
    });
    (ClaimOutcome::Received(amount), notice)
}

// ---------- меню чеков CryptoBot ----------

fn has_active_checks(st: &WorldState, acc: AccountId, platform: Platform) -> bool {
    st.checks.values().any(|c| {
        c.creator == Some(acc) && c.platform == platform && c.status == CheckStatus::Active
    })
}

fn cb_menu(st: &WorldState, acc: AccountId) -> Draft {
    let mut rows = vec![
        vec![callback("Создать чек", "create")],
        vec![switch_inline("Создать из чата", "")],
    ];
    if has_active_checks(st, acc, CB) {
        rows.push(vec![callback("Активные чеки", "manage")]);
    }
    rows.push(vec![callback("‹ Назад", "back-to-main-menu")]);
    Draft::text(texts::cb_checks_menu())
        .with_urls(vec!["https://t.me/CryptoBotRU/23".to_owned()])
        .with_buttons(rows)
        .on(Screen::CbChecksMenu)
}

fn cb_choose_coin() -> Draft {
    let mut rows: Vec<Vec<RawButton>> = Asset::ALL
        .iter()
        .map(|a| {
            let code = a.platform_code(CB);
            vec![callback(code, &format!("choose-currency-{code}"))]
        })
        .collect();
    rows.push(vec![callback("‹ Назад к чекам", "checks")]);
    Draft::text(texts::CB_CHOOSE_COIN)
        .with_buttons(rows)
        .on(Screen::CbChooseCoin)
}

fn cb_ask_amount(st: &WorldState, acc: AccountId, asset: Asset) -> Draft {
    let balance = st.balance_money(acc, CB, asset);
    let min = min_check(CB, asset);
    let code = asset.platform_code(CB);
    Draft::text(texts::cb_ask_amount(
        asset,
        &balance,
        st.usd_value(&balance),
    ))
    .with_buttons(vec![
        vec![callback(
            &format!("Мин. · {} {code}", texts::amount(min.amount())),
            "use-min",
        )],
        vec![callback(
            &format!("Макс. · {} {code}", texts::amount(balance.amount())),
            "use-max",
        )],
        vec![callback("‹ Изменить монету", "back")],
    ])
    .on(Screen::CbAskAmount(asset))
}

fn cb_insufficient(st: &WorldState, acc: AccountId, asset: Asset) -> Draft {
    let balance = st.balance_money(acc, CB, asset);
    Draft::text(texts::cb_insufficient(&balance, st.usd_value(&balance)))
        .with_buttons(vec![vec![callback("‹ Изменить монету", "back")]])
        .on(Screen::CbAskAmount(asset))
}

/// Сумма введена (текстом или кнопкой «Мин.» / «Макс.»).
fn cb_after_amount(st: &mut WorldState, acc: AccountId, m: Money, random_id: i64) -> Draft {
    let balance = st.balance_money(acc, CB, m.asset());
    if m.amount() < min_check(CB, m.asset()).amount() {
        return Draft::text(BAD_AMOUNT);
    }
    if balance.amount() < m.amount() {
        return cb_insufficient(st, acc, m.asset());
    }
    let max = (balance.amount() / m.amount())
        .floor()
        .to_u32()
        .unwrap_or(u32::MAX)
        .min(1000);
    if max >= 2 {
        return Draft::text(texts::cb_ask_activations(max))
            .with_buttons(vec![
                vec![callback("🎁 Создать подарок", "create-gift")],
                vec![callback("Пропустить ›", "count-1")],
                vec![callback(
                    &format!("Макс. кол-во · {max}"),
                    &format!("count-{max}"),
                )],
                vec![callback("‹ Изменить сумму", "back")],
            ])
            .on(Screen::CbAskActivations {
                amount: m,
                max,
                random_id,
            });
    }
    cb_create_check(st, acc, m, 1, random_id)
}

fn cb_create_check(
    st: &mut WorldState,
    acc: AccountId,
    m: Money,
    activations: u32,
    random_id: i64,
) -> Draft {
    let spec = CheckSpec::new(CB, m).by(acc).multi(activations);
    match st.insert_check(spec, CheckOrigin::Menu { random_id }) {
        Ok(check) => cb_check_screen(st, &check.code),
        Err(SimError::InsufficientFunds { .. }) => cb_insufficient(st, acc, m.asset()),
        Err(_) => Draft::text(BAD_AMOUNT),
    }
}

fn cb_check_screen(st: &WorldState, code: &str) -> Draft {
    let Some(check) = st.checks.get(code) else {
        return Draft::text(texts::not_found(CB, code));
    };
    let num = st.check_num(code).unwrap_or_default();
    let usd = st.usd_value(&check.amount);
    let link = format!("t.me/send?start={code}");
    let asset_code = check.amount.asset().platform_code(CB);
    let image = format!(
        "https://imggen.cr.bot/checks/image?asset={asset_code}&asset_amount={}&main=asset&v5",
        texts::amount(check.amount.amount())
    );
    Draft::text(texts::cb_check(&check.amount, usd, code))
        .with_urls(vec![image, link])
        .with_buttons(vec![
            vec![callback(
                "🎁 Конвертировать в подарок",
                &format!("convert-to-gift-{num}"),
            )],
            vec![switch_inline("Поделиться чеком", &format!("CS{num}"))],
            vec![callback(
                "Показать QR-код",
                &format!("checks-show-qr-code-{num}"),
            )],
            vec![callback(
                "Добавить описание",
                &format!("set-description-{num}"),
            )],
            vec![callback(
                "Удалить чек",
                &format!("confirm-delete-check-{num}"),
            )],
            vec![callback("‹ Назад к списку чеков", "checks-change-page-0")],
        ])
        .on(Screen::CbCheck(code.to_owned()))
}

fn cb_manage(st: &WorldState, acc: AccountId) -> Draft {
    let mut rows: Vec<Vec<RawButton>> = st
        .check_order
        .iter()
        .rev()
        .filter_map(|code| st.checks.get(code))
        .filter(|c| c.creator == Some(acc) && c.platform == CB && c.status == CheckStatus::Active)
        .take(20)
        .map(|c| {
            let num = st.check_num(&c.code).unwrap_or_default();
            vec![callback(
                &format!(
                    "{} {} ",
                    texts::amount(c.amount.amount()),
                    c.amount.asset().platform_code(CB)
                ),
                &format!("open-check-{num}"),
            )]
        })
        .collect();
    rows.push(vec![callback("‹ Назад к чекам", "checks")]);
    Draft::text(texts::CB_MANAGE)
        .with_buttons(rows)
        .on(Screen::CbManage)
}

fn cb_check_by_num(st: &WorldState, num: &str) -> Option<String> {
    let n: u64 = num.parse().ok()?;
    st.check_by_num(n).map(|c| c.code.clone())
}

// ---------- меню чеков xRocket ----------

fn xr_menu() -> Draft {
    Draft::text(texts::XR_CHEQUES_MENU)
        .with_buttons(vec![
            vec![
                callback("Персональный", "Personal"),
                callback("Мульти-чек", "Multi"),
            ],
            vec![callback(" Rocket-чек", "Rocket")],
            vec![callback(" Доска чеков", "ChequeBoard")],
            vec![callback("‹ Назад", "Back")],
        ])
        .on(Screen::XrChequesMenu)
}

fn xr_personal() -> Draft {
    Draft::text(texts::XR_PERSONAL)
        .with_buttons(vec![
            vec![callback(" Создать чек", "Create")],
            vec![callback("‹ Назад", "Back")],
        ])
        .on(Screen::XrPersonal)
}

fn xr_choose_coin() -> Draft {
    let mut rows: Vec<Vec<RawButton>> = Asset::ALL
        .iter()
        .map(|a| vec![callback(&format!(" {}", a.platform_code(XR)), xr_coin(*a))])
        .collect();
    rows.push(vec![callback("‹ Назад", "Back")]);
    Draft::text(texts::XR_CHOOSE_COIN)
        .with_buttons(rows)
        .on(Screen::XrChooseCoin)
}

fn xr_ask_amount(st: &WorldState, acc: AccountId, asset: Asset, insufficient: bool) -> Draft {
    let max = st.balance_money(acc, XR, asset);
    let min = min_check(XR, asset);
    let (max_usd, min_usd) = (st.usd_value(&max), st.usd_value(&min));
    Draft::text(texts::xr_ask_amount(
        asset,
        &max,
        max_usd,
        &min,
        min_usd,
        insufficient,
    ))
    .with_buttons(vec![
        vec![callback(
            &format!(
                "Максимум · {} {} ({})",
                texts::amount(max.amount()),
                asset.platform_code(XR),
                texts::xr_usd(max_usd)
            ),
            "Max",
        )],
        vec![callback("‹ Выбор валюты", "Coin")],
        vec![callback("‹ Назад", "Back")],
    ])
    .on(Screen::XrAskAmount(asset))
}

/// Сумма введена: подтверждение или «Недостаточно средств» на том же экране.
fn xr_after_amount(st: &mut WorldState, acc: AccountId, m: Money, random_id: i64) -> Draft {
    let balance = st.balance_money(acc, XR, m.asset());
    if m.amount() < min_check(XR, m.asset()).amount() {
        return xr_ask_amount(st, acc, m.asset(), false);
    }
    if balance.amount() < m.amount() {
        // Бот ждёт сумму дальше.
        return xr_ask_amount(st, acc, m.asset(), true);
    }
    st.set_dialog(acc, XR, Dialog::Idle);
    Draft::text(texts::xr_confirm(&m, st.usd_value(&m)))
        .with_buttons(vec![
            vec![
                callback("✅ Подтверждаю", "Success"),
                callback("❌ Отклоняю", "Back"),
            ],
            vec![callback("‹ Изменить сумму", "Amount")],
            vec![callback("‹ Назад", "Back")],
        ])
        .on(Screen::XrConfirm {
            amount: m,
            random_id,
        })
}

fn xr_check_screen(st: &WorldState, code: &str) -> Draft {
    let Some(check) = st.checks.get(code) else {
        return xr_personal();
    };
    let usd = st.usd_value(&check.amount);
    Draft::text(texts::xr_check(&check.amount, usd, &check.url))
        .with_urls(vec![check.url.clone()])
        .with_buttons(vec![
            vec![switch_inline(" Отправить", code)],
            vec![callback("📷 Показать QR-код", "Qr_1")],
            vec![callback("Добавить описание", "Comment")],
            vec![callback("Привязать к пользователю", "Lock")],
            vec![callback("Удалить", "Delete")],
            vec![callback("‹ Назад", "Back")],
        ])
        .on(Screen::XrCheck(code.to_owned()))
}

// ---------- нажатия ----------

type Pressed = Result<(CallbackAnswer, Vec<Outbound>), TransportError>;

/// Вид нажатия для сбоев: создаёт чек ([`Call::CheckPress`]), платит ([`Call::PayPress`])
/// или обычное.
pub(crate) fn press_kind(
    st: &WorldState,
    acc: AccountId,
    chat: Chat,
    msg_id: i32,
    data: &[u8],
) -> Call {
    let Ok(data) = std::str::from_utf8(data) else {
        return Call::Press;
    };
    match st.screen(acc, chat, msg_id) {
        Some(Screen::CbAskActivations { .. }) if data.starts_with("count-") => Call::CheckPress,
        Some(Screen::XrConfirm { .. }) if data == "Success" => Call::CheckPress,
        Some(Screen::CbInvoiceConfirm { .. }) if data.starts_with("confirm-payment-") => {
            Call::PayPress
        }
        Some(Screen::CbInvoiceTrust { .. }) if data.starts_with("adtl-confirm-p-") => {
            Call::PayPress
        }
        Some(Screen::XrInvoiceConfirm(_)) if data == "Pay" => Call::PayPress,
        _ => Call::Press,
    }
}

/// Нажатие callback-кнопки сообщения `msg_id`.
pub(crate) fn on_press(
    st: &mut WorldState,
    acc: AccountId,
    chat: Chat,
    msg_id: i32,
    data: &[u8],
) -> Pressed {
    let msg = st
        .find_message(acc, chat, msg_id)
        .ok_or_else(|| rpc("MESSAGE_ID_INVALID"))?;
    let has_button = msg
        .buttons
        .iter()
        .flatten()
        .any(|b| matches!(&b.kind, ButtonKind::Callback(d) if d.as_slice() == data));
    let Chat::WalletBot(platform) = chat else {
        return Err(rpc("DATA_INVALID"));
    };
    if !has_button || msg.out {
        return Err(rpc("DATA_INVALID"));
    }
    let data = std::str::from_utf8(data).map_err(|_| rpc("DATA_INVALID"))?;
    let screen = st
        .screen(acc, chat, msg_id)
        .cloned()
        .unwrap_or(Screen::Static);
    match platform {
        Platform::CryptoBot => cb_press(st, acc, msg_id, screen, data),
        Platform::XRocket => xr_press(st, acc, msg_id, screen, data),
    }
}

fn cb_press(
    st: &mut WorldState,
    acc: AccountId,
    msg_id: i32,
    screen: Screen,
    data: &str,
) -> Pressed {
    let ok = |outs: Vec<Outbound>| Ok((CallbackAnswer::default(), outs));
    let show = |d: Draft| edit(acc, CB, msg_id, d);
    match (screen, data) {
        (Screen::CbChecksMenu, "create") => ok(vec![show(cb_choose_coin())]),
        (Screen::CbChecksMenu, "manage") => ok(vec![show(cb_manage(st, acc))]),
        (Screen::CbChooseCoin, d) if d.starts_with("choose-currency-") => {
            let code = &d["choose-currency-".len()..];
            let asset = Asset::from_str(code).map_err(|_| rpc("DATA_INVALID"))?;
            st.set_dialog(
                acc,
                CB,
                Dialog::AwaitCheckAmount {
                    asset,
                    menu: msg_id,
                },
            );
            ok(vec![show(cb_ask_amount(st, acc, asset))])
        }
        (Screen::CbChooseCoin | Screen::CbManage, "checks") => {
            st.set_dialog(acc, CB, Dialog::Idle);
            ok(vec![show(cb_menu(st, acc))])
        }
        (Screen::CbAskAmount(asset), "use-min" | "use-max") => {
            st.set_dialog(acc, CB, Dialog::Idle);
            let m = if data == "use-min" {
                min_check(CB, asset)
            } else {
                st.balance_money(acc, CB, asset)
            };
            ok(vec![show(cb_after_amount(st, acc, m, 0))])
        }
        (Screen::CbAskAmount(_), "back") => {
            st.set_dialog(acc, CB, Dialog::Idle);
            ok(vec![show(cb_choose_coin())])
        }
        (
            Screen::CbAskActivations {
                amount,
                max,
                random_id,
            },
            d,
        ) if d.starts_with("count-") => {
            let n: u32 = d["count-".len()..]
                .parse()
                .map_err(|_| rpc("DATA_INVALID"))?;
            if n == 0 || n > max {
                return Err(rpc("DATA_INVALID"));
            }
            ok(vec![show(cb_create_check(st, acc, amount, n, random_id))])
        }
        (Screen::CbAskActivations { amount, .. }, "back") => {
            st.set_dialog(
                acc,
                CB,
                Dialog::AwaitCheckAmount {
                    asset: amount.asset(),
                    menu: msg_id,
                },
            );
            ok(vec![show(cb_ask_amount(st, acc, amount.asset()))])
        }
        (Screen::CbCheck(code), d) if num_after(d, "confirm-delete-check-").is_some() => {
            ok(vec![show(
                Draft::text(texts::CB_CONFIRM_DELETE)
                    .with_buttons(vec![vec![
                        callback(
                            "Да",
                            &format!("delete-check-{}", &d["confirm-delete-check-".len()..]),
                        ),
                        callback(
                            "Нет",
                            &format!("open-check-{}", &d["confirm-delete-check-".len()..]),
                        ),
                    ]])
                    .on(Screen::CbConfirmDelete(code)),
            )])
        }
        (Screen::CbCheck(_), "checks-change-page-0") => ok(vec![show(cb_manage(st, acc))]),
        (Screen::CbConfirmDelete(code), d) if num_after(d, "delete-check-").is_some() => {
            st.cancel_check(&code).map_err(|_| rpc("DATA_INVALID"))?;
            ok(vec![
                show(Draft::text(texts::CB_DELETING)),
                show(cb_menu(st, acc)),
            ])
        }
        (Screen::CbConfirmDelete(code), d) if num_after(d, "open-check-").is_some() => {
            ok(vec![show(cb_check_screen(st, &code))])
        }
        (Screen::CbManage, d) if num_after(d, "open-check-").is_some() => {
            let code = num_after(d, "open-check-")
                .and_then(|n| cb_check_by_num(st, n))
                .ok_or_else(|| rpc("DATA_INVALID"))?;
            ok(vec![show(cb_check_screen(st, &code))])
        }
        (Screen::CbInvoiceCoins(code), d) if d.starts_with("choose-currency-") => {
            let asset =
                Asset::from_str(&d["choose-currency-".len()..]).map_err(|_| rpc("DATA_INVALID"))?;
            ok(vec![show(cb_invoice_confirm(st, &code, asset))])
        }
        (Screen::CbInvoiceConfirm { code, asset }, d) if d.starts_with("confirm-payment-") => Ok((
            CallbackAnswer::default(),
            cb_pay(st, acc, msg_id, &code, asset, false),
        )),
        (Screen::CbInvoiceTrust { code, asset }, d) if d.starts_with("adtl-confirm-p-") => Ok((
            CallbackAnswer::default(),
            cb_pay(st, acc, msg_id, &code, asset, true),
        )),
        (
            Screen::CbInvoiceTrust { code, asset } | Screen::CbInvoiceConfirm { code, asset },
            "pay-back-to-invoice" | "cancel-payment",
        ) => match st.invoices.get(&code).map(|i| i.amount.clone()) {
            Some(InvoiceAmount::Fiat { .. }) => ok(vec![show(cb_invoice_coins(st, &code))]),
            _ => ok(vec![show(cb_invoice_confirm(st, &code, asset))]),
        },
        (Screen::CbInvoiceNetwork(code), "pay-back-to-payment") => {
            let asset = st
                .invoices
                .get(&code)
                .and_then(|i| i.accepted_assets.first().copied())
                .unwrap_or(Asset::Usdt);
            ok(vec![show(cb_invoice_confirm(st, &code, asset))])
        }
        // Кнопки, которые exch не нажимает: подарок, QR, описание, анонимность, комментарий,
        // пополнение из сети.
        _ => Ok((alert(UNSUPPORTED), Vec::new())),
    }
}

fn xr_press(
    st: &mut WorldState,
    acc: AccountId,
    msg_id: i32,
    screen: Screen,
    data: &str,
) -> Pressed {
    let ok = |outs: Vec<Outbound>| Ok((CallbackAnswer::default(), outs));
    let show = |d: Draft| edit(acc, XR, msg_id, d);
    match (screen, data) {
        (Screen::XrChequesMenu, "Personal") => ok(vec![show(xr_personal())]),
        (Screen::XrChequesMenu, "Multi" | "Rocket" | "ChequeBoard") => {
            Ok((alert(texts::XR_MULTI_UNSUPPORTED), Vec::new()))
        }
        (Screen::XrPersonal, "Create") => ok(vec![show(xr_choose_coin())]),
        (Screen::XrPersonal, "Back") => ok(vec![show(xr_menu())]),
        (Screen::XrChooseCoin, d) if xr_coin_asset(d).is_some() => {
            let asset = xr_coin_asset(d).ok_or_else(|| rpc("DATA_INVALID"))?;
            st.set_dialog(
                acc,
                XR,
                Dialog::AwaitCheckAmount {
                    asset,
                    menu: msg_id,
                },
            );
            ok(vec![show(xr_ask_amount(st, acc, asset, false))])
        }
        (Screen::XrChooseCoin, "Back") => ok(vec![show(xr_personal())]),
        (Screen::XrAskAmount(asset), "Max") => {
            let m = st.balance_money(acc, XR, asset);
            ok(vec![show(xr_after_amount(st, acc, m, 0))])
        }
        (Screen::XrAskAmount(_), "Coin") => {
            st.set_dialog(acc, XR, Dialog::Idle);
            ok(vec![show(xr_choose_coin())])
        }
        (Screen::XrAskAmount(_) | Screen::XrConfirm { .. }, "Back") => {
            st.set_dialog(acc, XR, Dialog::Idle);
            ok(vec![show(xr_personal())])
        }
        (Screen::XrConfirm { amount, .. }, "Amount") => {
            st.set_dialog(
                acc,
                XR,
                Dialog::AwaitCheckAmount {
                    asset: amount.asset(),
                    menu: msg_id,
                },
            );
            ok(vec![show(xr_ask_amount(st, acc, amount.asset(), false))])
        }
        (Screen::XrConfirm { amount, random_id }, "Success") => {
            let spec = CheckSpec::new(XR, amount).by(acc);
            let screen = match st.insert_check(spec, CheckOrigin::Menu { random_id }) {
                Ok(check) => xr_check_screen(st, &check.code),
                Err(SimError::InsufficientFunds { .. }) => {
                    st.set_dialog(
                        acc,
                        XR,
                        Dialog::AwaitCheckAmount {
                            asset: amount.asset(),
                            menu: msg_id,
                        },
                    );
                    xr_ask_amount(st, acc, amount.asset(), true)
                }
                Err(_) => xr_personal(),
            };
            ok(vec![show(screen)])
        }
        // «Удалить» — молча: чек пропадает, экран — снова «Персональные чеки».
        (Screen::XrCheck(code), "Delete") => {
            st.cancel_check(&code).map_err(|_| rpc("DATA_INVALID"))?;
            ok(vec![show(xr_personal())])
        }
        (Screen::XrCheck(_), "Back") => ok(vec![show(xr_personal())]),
        (Screen::XrInvoice(code), "ViaRocket") => ok(vec![show(xr_invoice_method(st, acc, &code))]),
        (Screen::XrInvoiceConfirm(code), "Pay") => {
            Ok((CallbackAnswer::default(), xr_pay(st, acc, msg_id, &code)))
        }
        (Screen::XrInvoiceConfirm(code) | Screen::XrInvoiceInsufficient(code), "Invoice") => {
            ok(vec![show(xr_invoice_screen(st, acc, &code))])
        }
        // «Адрес», «Пополнение», QR, описание, привязка — exch их не нажимает.
        _ => Ok((alert(UNSUPPORTED), Vec::new())),
    }
}

// ---------- счета ----------

fn invoice_number(st: &WorldState, code: &str) -> String {
    st.invoice_number(code).unwrap_or_else(|| code.to_owned())
}

/// `/start IV…` / `/start inv_…`.
fn open_invoice(st: &WorldState, acc: AccountId, platform: Platform, code: &str) -> Outbound {
    let Some(inv) = st.invoices.get(code).filter(|i| i.platform == platform) else {
        return reply(
            acc,
            platform,
            Draft::text(texts::invoice_not_found(platform)),
        );
    };
    match inv.status {
        SimInvoiceStatus::Paid => {
            return reply(
                acc,
                platform,
                Draft::text(texts::invoice_already_paid(platform))
                    .with_buttons(vec![vec![callback("‹ Назад", "Back")]]),
            );
        }
        SimInvoiceStatus::Expired => {
            return reply(acc, platform, Draft::text(texts::INVOICE_EXPIRED));
        }
        SimInvoiceStatus::Active => {}
    }
    if inv.creator == Some(acc) {
        return reply(acc, platform, Draft::text(texts::INVOICE_OWN));
    }
    let mut draft = match platform {
        Platform::CryptoBot => match &inv.amount {
            InvoiceAmount::Fiat { .. } => cb_invoice_coins(st, code),
            InvoiceAmount::Crypto(m) => cb_invoice_confirm(st, code, m.asset()),
        },
        Platform::XRocket => xr_invoice_screen(st, acc, code),
    };
    if !inv.single_use {
        match platform {
            // Живая запись: многоразовый счёт CryptoBot — без суммы, «Пришлите сумму USDT…».
            Platform::CryptoBot => {
                let asset = inv.accepted_assets.first().copied().unwrap_or(Asset::Usdt);
                let min = min_check(Platform::CryptoBot, asset);
                let balance = st.balance_money(acc, platform, asset);
                draft = Draft::text(texts::cb_invoice_open_amount(
                    &min,
                    st.usd_value(&min),
                    &balance,
                    st.usd_value(&balance),
                ))
                .with_buttons(vec![vec![callback(
                    &format!(
                        "Мин. · {} {}",
                        texts::amount(min.amount()),
                        asset.platform_code(platform)
                    ),
                    "pay-use-min",
                )]]);
            }
            // Синтетика: экран многоразового счёта xRocket в записи ещё не попадался.
            Platform::XRocket => draft.text.push_str(texts::MULTI_USE_MARK),
        }
    }
    reply(acc, platform, draft)
}

/// Счёт CryptoBot в фиате: выбор монеты, сумма в монете — на кнопке.
fn cb_invoice_coins(st: &WorldState, code: &str) -> Draft {
    let Some(inv) = st.invoices.get(code) else {
        return Draft::text(texts::invoice_not_found(CB));
    };
    let fiat = match &inv.amount {
        InvoiceAmount::Fiat { amount, currency } => texts::cb_fiat(*amount, currency),
        InvoiceAmount::Crypto(m) => format!(
            "{} {}",
            texts::amount(m.amount()),
            m.asset().platform_code(CB)
        ),
    };
    let mut rows: Vec<Vec<RawButton>> = inv
        .accepted_assets
        .iter()
        .filter_map(|a| {
            let due = st.invoice_due(inv, *a)?;
            let c = a.platform_code(CB);
            Some(vec![callback(
                &format!("{c} · {} {c}", texts::amount(due.amount())),
                &format!("choose-currency-{c}"),
            )])
        })
        .collect();
    rows.push(vec![callback("‹ Назад", "cancel-payment")]);
    Draft::text(texts::cb_invoice_choose_coin(
        &invoice_number(st, code),
        &fiat,
    ))
    .with_buttons(rows)
    .on(Screen::CbInvoiceCoins(code.to_owned()))
}

fn pay_hash(code: &str, asset: Asset) -> String {
    format!("{}{}", code.to_lowercase(), asset.code().to_lowercase())
}

fn cb_invoice_confirm(st: &WorldState, code: &str, asset: Asset) -> Draft {
    let Some(due) = st
        .invoices
        .get(code)
        .and_then(|inv| st.invoice_due(inv, asset))
    else {
        return Draft::text(texts::invoice_not_found(CB));
    };
    let c = asset.platform_code(CB);
    Draft::text(texts::cb_invoice_confirm(
        &invoice_number(st, code),
        &due,
        st.usd_value(&due),
    ))
    .with_buttons(vec![
        vec![callback(
            &format!("💳 Оплатить {} {c}", texts::amount(due.amount())),
            &format!("confirm-payment-{}", pay_hash(code, asset)),
        )],
        vec![callback(
            "Оплатить анонимно: Нет",
            "payment-change-anonymous",
        )],
        vec![callback("Добавить комментарий", "payment-add-comment")],
        vec![callback("‹ Назад", "cancel-payment")],
    ])
    .on(Screen::CbInvoiceConfirm {
        code: code.to_owned(),
        asset,
    })
}

/// «💳 Оплатить» (или «💸 Отправить платеж получателю», `trust_confirmed`).
fn cb_pay(
    st: &mut WorldState,
    acc: AccountId,
    msg_id: i32,
    code: &str,
    asset: Asset,
    trust_confirmed: bool,
) -> Vec<Outbound> {
    let show = |d: Draft| edit(acc, CB, msg_id, d);
    let Some(inv) = st.invoices.get(code) else {
        return vec![show(Draft::text(texts::invoice_not_found(CB)))];
    };
    let recipient = inv.creator;
    let Some(due) = st.invoice_due(inv, asset) else {
        return vec![show(Draft::text(texts::invoice_not_found(CB)))];
    };
    if inv.status == SimInvoiceStatus::Active
        && st.balance_money(acc, CB, asset).amount() < due.amount()
    {
        let networks = [
            ("Tron (TRC20)", "TRX"),
            ("BNB Smart Chain (BEP20)", "BSC"),
            ("TON (TON)", "TON"),
        ];
        let mut rows: Vec<Vec<RawButton>> = networks
            .iter()
            .map(|(name, net)| vec![callback(name, &format!("pay-get-address-{net}"))])
            .collect();
        rows.push(vec![callback("‹ Назад к оплате", "pay-back-to-payment")]);
        return vec![show(
            Draft::text(texts::cb_choose_network(asset))
                .with_buttons(rows)
                .on(Screen::CbInvoiceNetwork(code.to_owned())),
        )];
    }
    if !trust_confirmed
        && inv.status == SimInvoiceStatus::Active
        && !st.trusted.contains(&(acc, recipient))
    {
        return vec![show(
            Draft::text(texts::CB_TRUST_RECIPIENT)
                .with_urls(vec!["https://help.cr.bot/".to_owned()])
                .with_buttons(vec![
                    vec![callback(
                        "💸 Отправить платеж получателю",
                        &format!("adtl-confirm-p-{}", pay_hash(code, asset)),
                    )],
                    vec![callback("‹ Назад", "pay-back-to-invoice")],
                ])
                .on(Screen::CbInvoiceTrust {
                    code: code.to_owned(),
                    asset,
                }),
        )];
    }
    match pay_invoice(st, acc, code, asset) {
        Ok((paid, notice)) => {
            st.trusted.insert((acc, recipient));
            let fiat = match st.invoices.get(code).map(|i| &i.amount) {
                Some(InvoiceAmount::Fiat { amount, .. }) => Some(*amount),
                _ => None,
            };
            let who = recipient.map(|r| st.display(r));
            let text = texts::cb_invoice_paid(
                &invoice_number(st, code),
                &paid,
                st.usd_value(&paid),
                fiat,
                who.as_deref(),
            );
            let mut outs = vec![
                staged(
                    acc,
                    CB,
                    Draft::text(texts::CB_PAYING),
                    Draft::text(texts::CB_PAID_MARK),
                ),
                reply(acc, CB, Draft::text(text)),
            ];
            outs.extend(notice);
            outs
        }
        Err(e) => vec![show(Draft::text(pay_error_text(CB, &e)))],
    }
}

fn xr_due(st: &WorldState, code: &str) -> Option<Money> {
    let inv = st.invoices.get(code)?;
    inv.accepted_assets
        .iter()
        .find_map(|a| st.invoice_due(inv, *a))
}

fn xr_invoice_parts(
    st: &WorldState,
    acc: AccountId,
    code: &str,
) -> Option<(String, Money, Decimal, Money, Decimal)> {
    let due = xr_due(st, code)?;
    let balance = st.balance_money(acc, XR, due.asset());
    Some((
        invoice_number(st, code),
        due,
        st.usd_value(&due),
        balance,
        st.usd_value(&balance),
    ))
}

fn xr_invoice_screen(st: &WorldState, acc: AccountId, code: &str) -> Draft {
    let Some((number, due, usd, balance, bal_usd)) = xr_invoice_parts(st, acc, code) else {
        return Draft::text(texts::invoice_not_found(XR));
    };
    Draft::text(texts::xr_invoice(&number, &due, usd, &balance, bal_usd))
        .with_buttons(vec![
            vec![
                callback(" xRocket", "ViaRocket"),
                callback(" Адрес", "ViaAddress"),
            ],
            vec![callback("‹ Назад", "Back")],
        ])
        .on(Screen::XrInvoice(code.to_owned()))
}

/// Способ «xRocket»: подтверждение или «Недостаточно средств».
fn xr_invoice_method(st: &WorldState, acc: AccountId, code: &str) -> Draft {
    let Some((number, due, usd, balance, bal_usd)) = xr_invoice_parts(st, acc, code) else {
        return Draft::text(texts::invoice_not_found(XR));
    };
    if balance.amount() < due.amount() {
        return Draft::text(texts::xr_invoice_insufficient(
            &number, &due, usd, &balance, bal_usd,
        ))
        .with_buttons(vec![
            vec![callback(" Пополнение", "Deposit")],
            vec![callback("‹ Счёт", "Invoice")],
            vec![callback("‹ Назад", "Back")],
        ])
        .on(Screen::XrInvoiceInsufficient(code.to_owned()));
    }
    Draft::text(texts::xr_invoice_confirm(
        &number, &due, usd, &balance, bal_usd,
    ))
    .with_buttons(vec![
        vec![callback("Оплатить", "Pay")],
        vec![callback("‹ Счёт", "Invoice")],
    ])
    .on(Screen::XrInvoiceConfirm(code.to_owned()))
}

fn xr_pay(st: &mut WorldState, acc: AccountId, msg_id: i32, code: &str) -> Vec<Outbound> {
    let Some(due) = xr_due(st, code) else {
        return vec![edit(
            acc,
            XR,
            msg_id,
            Draft::text(texts::invoice_not_found(XR)),
        )];
    };
    match pay_invoice(st, acc, code, due.asset()) {
        Ok((paid, notice)) => {
            let number = invoice_number(st, code);
            let payment = format!(
                "i{}_{}",
                number.trim_start_matches("INV"),
                st.next_payment()
            );
            let mut outs = vec![
                reply(acc, XR, Draft::text(texts::XR_LOADING)),
                reply(
                    acc,
                    XR,
                    Draft::text(texts::xr_invoice_paid(&paid, st.usd_value(&paid), &payment))
                        .with_buttons(vec![vec![callback("‹ Кошелёк", "BackToWallet")]]),
                ),
            ];
            outs.extend(notice);
            outs
        }
        Err(PayError::Insufficient { .. }) => {
            vec![edit(acc, XR, msg_id, xr_invoice_method(st, acc, code))]
        }
        Err(e) => vec![edit(acc, XR, msg_id, Draft::text(pay_error_text(XR, &e)))],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PayError {
    NotFound,
    AlreadyPaid,
    Expired,
    Own,
    Insufficient {
        available: Money,
        due: Money,
    },
    /// CryptoBot: траты за 24 часа вместе с этой — выше лимита без код-пароля.
    PasscodeRequired,
}

fn pay_error_text(platform: Platform, e: &PayError) -> String {
    match e {
        PayError::NotFound => texts::invoice_not_found(platform).to_owned(),
        PayError::AlreadyPaid => texts::invoice_already_paid(platform).to_owned(),
        PayError::Expired => texts::INVOICE_EXPIRED.to_owned(),
        PayError::Own => texts::INVOICE_OWN.to_owned(),
        PayError::Insufficient { due, .. } => texts::cb_choose_network(due.asset()),
        PayError::PasscodeRequired => texts::CB_PASSCODE_REQUIRED.to_owned(),
    }
}

/// Оплатить счёт с баланса `payer`: списание, зачисление создателю, статус.
pub(crate) fn pay_invoice(
    st: &mut WorldState,
    payer: AccountId,
    code: &str,
    asset: Asset,
) -> Result<(Money, Option<Outbound>), PayError> {
    let inv = st.invoices.get(code).ok_or(PayError::NotFound)?;
    match inv.status {
        SimInvoiceStatus::Paid => return Err(PayError::AlreadyPaid),
        SimInvoiceStatus::Expired => return Err(PayError::Expired),
        SimInvoiceStatus::Active => {}
    }
    if inv.creator == Some(payer) {
        return Err(PayError::Own);
    }
    let platform = inv.platform;
    let creator = inv.creator;
    let due = st.invoice_due(inv, asset).ok_or(PayError::NotFound)?;
    if platform == Platform::CryptoBot && st.passcode_needed(payer, &due) {
        return Err(PayError::PasscodeRequired);
    }
    match st.debit(payer, platform, due) {
        Ok(()) => {}
        Err(SimError::InsufficientFunds { available, .. }) => {
            return Err(PayError::Insufficient { available, due });
        }
        Err(_) => return Err(PayError::NotFound),
    }
    if let Some(c) = creator {
        let _ = st.credit(c, platform, due);
    }
    let now = st.now_ms;
    if let Some(inv) = st.invoices.get_mut(code) {
        inv.payments.push(super::InvoicePayment {
            payer,
            paid: due,
            at_ms: now,
        });
        if inv.single_use {
            inv.status = SimInvoiceStatus::Paid;
        }
    }
    let usd = st.usd_value(&due);
    let number = invoice_number(st, code);
    let notice = creator.map(|c| {
        let who = st.display(payer);
        reply(
            c,
            platform,
            Draft::text(texts::invoice_paid_notice(
                platform, &number, &due, usd, &who,
            )),
        )
    });
    Ok((due, notice))
}

/// Параметр из ссылки: `https://t.me/send?start=IVx` → `IVx`.
pub(crate) fn query_param(query: &str, name: &str) -> Option<String> {
    let query = query.split('#').next()?;
    let query = query.rsplit('?').next()?;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name && !v.is_empty()).then(|| v.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn user_amounts() {
        assert_eq!(parse_user_amount("10"), Some(dec!(10)));
        assert_eq!(parse_user_amount(" 10,50 "), Some(dec!(10.5)));
        assert_eq!(parse_user_amount("0"), None);
        assert_eq!(parse_user_amount("-1"), None);
        assert_eq!(parse_user_amount("1e3"), None);
        assert_eq!(parse_user_amount("abc"), None);
        assert_eq!(parse_user_amount(""), None);
    }

    #[test]
    fn query_params() {
        let url = "https://t.me/send?start=IVx&ref=promo#frag";
        assert_eq!(query_param(url, "start").as_deref(), Some("IVx"));
        assert_eq!(query_param(url, "ref").as_deref(), Some("promo"));
        assert_eq!(query_param(url, "frag"), None);
    }

    #[test]
    fn xrocket_coins_round_trip() {
        for a in Asset::ALL {
            assert_eq!(xr_coin_asset(xr_coin(a)), Some(a));
        }
    }
}
