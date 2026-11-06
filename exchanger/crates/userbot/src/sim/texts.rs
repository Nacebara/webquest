//! Тексты ботов симулятора — живые тексты @send и @xrocket из записи владельца (6 ноября 2026,
//! `parsers/tests/fixtures/live_wallet_replies.json`), с подстановкой сумм и кодов. Тест
//! `same_as_live` сверяет их с записью символ в символ; тест `parsers_read_every_screen`
//! проверяет, что каждый экран парсеры относят ровно к своему классу.
//!
//! Чего в записи нет (капча, подписка, только Premium, пароль, свой чек, ответ на непонятный
//! текст, истёкший счёт, код-пароль) — синтетика в том же стиле; такие тексты помечены.

use domain::{Asset, Decimal, Money, Platform};

/// Неразрывный пробел: xRocket ставит его между суммой и кодом актива.
const NBSP: char = '\u{a0}';
/// Невидимый пробел в начале экранов меню чеков CryptoBot.
const ZWSP: char = '\u{200b}';

pub fn amount(d: Decimal) -> String {
    d.normalize().to_string()
}

/// Долларовая оценка, как её пишет CryptoBot: `$4.99`, `$5`, `$0.01`.
pub fn cb_usd(usd: Decimal) -> String {
    format!("${}", usd.round_dp(2).normalize())
}

/// Долларовая оценка, как её пишет xRocket: `5.0$`, `0.2$`, `0.01$`.
pub fn xr_usd(usd: Decimal) -> String {
    let tenth = Decimal::new(1, 1);
    if usd >= tenth || usd.is_zero() {
        let r = usd.round_dp(1);
        if r.fract().is_zero() {
            format!("{}.0$", r.trunc())
        } else {
            format!("{}$", r.normalize())
        }
    } else {
        format!("{}$", usd.round_dp(2).normalize())
    }
}

/// Сумма с оценкой: CryptoBot `🪙 5 USDT ($4.99)`, xRocket `5 USDT (5.0$)` с неразрывным пробелом.
pub fn money(platform: Platform, m: &Money, usd: Decimal) -> String {
    let code = m.asset().platform_code(platform);
    match platform {
        Platform::CryptoBot => format!("🪙 {} {code} ({})", amount(m.amount()), cb_usd(usd)),
        Platform::XRocket => format!("{}{NBSP}{code} ({})", amount(m.amount()), xr_usd(usd)),
    }
}

/// Сумма без значка монеты: `5.006321 USDT ($5)` / `2 USDT (2.0$)`.
fn bare(platform: Platform, m: &Money, usd: Decimal) -> String {
    let code = m.asset().platform_code(platform);
    match platform {
        Platform::CryptoBot => format!("{} {code} ({})", amount(m.amount()), cb_usd(usd)),
        Platform::XRocket => format!("{}{NBSP}{code} ({})", amount(m.amount()), xr_usd(usd)),
    }
}

fn plain(platform: Platform, m: &Money) -> String {
    format!(
        "{} {}",
        amount(m.amount()),
        m.asset().platform_code(platform)
    )
}

// ---------- ответы на `/start <чек>` ----------

/// Промежуточный экран активации; через мгновение бот правит его в [`received`].
pub fn receiving(platform: Platform, m: &Money, usd: Decimal) -> String {
    match platform {
        Platform::CryptoBot => format!("🕘 Получение {}…", money(platform, m, usd)),
        Platform::XRocket => format!("⏱ Получение {}", money(platform, m, usd)),
    }
}

pub fn received(platform: Platform, m: &Money, usd: Decimal) -> String {
    match platform {
        Platform::CryptoBot => format!("📥 Вы получили {}.", money(platform, m, usd)),
        Platform::XRocket => format!("💰 Вы успешно получили {}.", money(platform, m, usd)),
    }
}

pub fn already_activated(platform: Platform) -> &'static str {
    match platform {
        Platform::CryptoBot => "Этот чек уже активирован.",
        Platform::XRocket => "Этот перевод уже активирован.",
    }
}

/// xRocket: этот же аккаунт уже забрал чек.
pub const XR_ALREADY_ACTIVATED_BY_YOU: &str = "Этот перевод уже активирован вами.";

pub fn not_found(platform: Platform, code: &str) -> &'static str {
    match platform {
        Platform::CryptoBot => {
            "Чек не найден, удалён или не был создан из-за недостаточного баланса."
        }
        Platform::XRocket if code.starts_with("mc_") || code.starts_with("mci_") => {
            "Мульти-чек не найден."
        }
        Platform::XRocket => "Этот перевод не найден.",
    }
}

/// CryptoBot (в записи); у xRocket — синтетика.
pub fn not_for_you(platform: Platform) -> &'static str {
    match platform {
        Platform::CryptoBot => {
            "К сожалению, вы не можете получить этот подарок. Он предназначен для другого получателя."
        }
        Platform::XRocket => "Вы не можете активировать этот чек.",
    }
}

/// Синтетика: в записи таких чеков нет.
pub const CAPTCHA: &str = "Введите символы, которые вы видите на картинке.";
/// Синтетика.
pub const PASSWORD_PROMPT: &str = "Введите пароль от чека.";
/// Синтетика.
pub const WRONG_PASSWORD: &str = "Неверный пароль, попробуйте ещё раз.";
/// Синтетика.
pub const PREMIUM_ONLY: &str = "Этот чек могут активировать только пользователи Telegram Premium.";

/// Синтетика.
pub fn needs_subscription(channel: &str) -> String {
    format!("Подпишитесь на канал {channel}, чтобы активировать этот чек.")
}

/// Синтетика: создатель открыл свой чек. Нарочно без «ваш чек» и «получил».
pub fn own_check(platform: Platform, m: &Money, url: &str) -> String {
    format!(
        "Это ваш собственный чек на {}.\n\n{url}",
        plain(platform, m)
    )
}

// ---------- уведомления ----------

/// Создателю чека: кто забрал. Имя — между невидимыми метками направления текста, как у ботов.
pub fn claim_notice(platform: Platform, m: &Money, usd: Decimal, who: &str) -> String {
    match platform {
        Platform::CryptoBot => format!(
            "🦋 \u{2068}{who}\u{2069} активировал(а) ваш чек и получил(а) {}.",
            money(platform, m, usd)
        ),
        Platform::XRocket => format!(
            "\u{2068}{who}\u{2069} активировал(а) ваш чек на {}",
            money(platform, m, usd)
        ),
    }
}

/// Входящий перевод (CryptoBot — в записи, xRocket — синтетика). Похож на выигрыш, отличается «от …».
pub fn incoming_transfer(platform: Platform, m: &Money, usd: Decimal, from: &str) -> String {
    match platform {
        Platform::CryptoBot => format!("📥 Вы получили {} от {from}.", money(platform, m, usd)),
        Platform::XRocket => format!("💰 Вы получили {} от {from}.", money(platform, m, usd)),
    }
}

/// Сбой `ProcessingFirst`: промежуточный экран вместо ответа (как «🕘 Получение…» без суммы).
pub fn processing(platform: Platform) -> &'static str {
    match platform {
        Platform::CryptoBot => "🕘 Получение…",
        Platform::XRocket => "⏱ Получение…",
    }
}

// ---------- общее ----------

/// Синтетика: `/start` без параметра.
pub fn welcome(platform: Platform) -> String {
    match platform {
        Platform::CryptoBot => "🦋 Crypto Bot — кошелёк в Telegram.".to_owned(),
        Platform::XRocket => "🚀 xRocket — это бот-кошелёк для получения, отправки, покупки и хранения криптовалюты в Telegram.".to_owned(),
    }
}

/// Синтетика: непонятный текст.
pub const UNKNOWN_COMMAND: &str = "Не понимаю. Отправьте /start, чтобы открыть меню.";

fn asset_name(platform: Platform, asset: Asset) -> &'static str {
    match (platform, asset) {
        (Platform::CryptoBot, Asset::Usdt) => "Tether",
        (Platform::CryptoBot, Asset::Ton) => "Toncoin",
        (Platform::XRocket, a) => a.platform_code(Platform::XRocket),
    }
}

/// Экран «Кошелёк»: `(сумма, оценка в $)` по активам. CryptoBot пишет удержание, если оно есть.
pub fn wallet(platform: Platform, balances: &[(Money, Decimal)]) -> String {
    let total: Decimal = balances.iter().map(|(_, usd)| *usd).sum();
    match platform {
        Platform::CryptoBot => {
            let mut s = String::from("👛 Кошелёк");
            for (m, usd) in balances {
                let tail = if m.amount().is_zero() {
                    String::new()
                } else {
                    format!(" ({})", cb_usd(*usd))
                };
                s.push_str(&format!(
                    "\n\n🪙 {}: {}{tail}",
                    asset_name(platform, m.asset()),
                    plain(platform, m)
                ));
            }
            s.push_str(&format!("\n\n≈ {}", cb_usd(total)));
            s
        }
        Platform::XRocket => {
            let mut s = String::from("👛 Мой кошелёк");
            for (m, usd) in balances {
                let code = m.asset().platform_code(platform);
                let tail = if m.amount().is_zero() {
                    String::new()
                } else {
                    format!(" ({})", xr_usd(*usd))
                };
                s.push_str(&format!(
                    "\n\n🪙{NBSP}{}: {}{NBSP}{code}{tail}",
                    asset_name(platform, m.asset()),
                    amount(m.amount())
                ));
            }
            s.push_str(&format!(
                "\n\n≈ {} $\n\nЧтобы настроить отображение, нажми на кнопку \"Отображение балансов\".",
                total.round_dp(2).normalize()
            ));
            s
        }
    }
}

// ---------- меню чеков CryptoBot ----------

pub fn cb_checks_menu() -> String {
    format!(
        "{ZWSP}Здесь вы можете создать чек для мгновенной отправки криптовалюты любому пользователю. Смотреть видеоинструкцию ›"
    )
}

pub const CB_CHOOSE_COIN: &str = "Выберите криптовалюту для создания чека.";

pub fn cb_ask_amount(asset: Asset, balance: &Money, usd: Decimal) -> String {
    let code = asset.platform_code(Platform::CryptoBot);
    format!(
        "Пришлите сумму чека в {code}. Если вы хотите создать мультичек, введите сумму одной активации, которой кратен ваш баланс.\n\nВаш баланс: {}",
        bare(Platform::CryptoBot, balance, usd)
    )
}

pub fn cb_ask_activations(max: u32) -> String {
    format!("Пришлите количество активаций, чтобы создать мультичек (до {max} активаций).")
}

/// Экран созданного чека; ссылка — текстом без схемы, как у бота.
pub fn cb_check(m: &Money, usd: Decimal, code: &str) -> String {
    format!(
        "{ZWSP}Чек\n\nСумма: {}\n\nЛюбой может активировать этот чек.\n\nСкопируйте ссылку, чтобы поделиться чеком:\nt.me/send?start={code}\n\n⚠️ Никогда не делайте скриншот вашего чека и не отправляйте его никому! Ссылку на чек могут использовать мошенники, чтобы получить доступ к вашим средствам.",
        money(Platform::CryptoBot, m, usd)
    )
}

pub fn cb_insufficient(balance: &Money, usd: Decimal) -> String {
    format!(
        "Недостаточно монет. Пополните баланс.\n\nВаш баланс: {}.",
        bare(Platform::CryptoBot, balance, usd)
    )
}

pub const CB_CONFIRM_DELETE: &str = "❌ Вы уверены, что хотите удалить этот чек?";
pub const CB_DELETING: &str = "🕘 Удаление чека…";
pub const CB_MANAGE: &str = "Здесь вы можете управлять своими созданными чеками.";

// ---------- меню чеков xRocket ----------

pub const XR_CHEQUES_MENU: &str = "🏷 Чеки\n    \nЧеки позволяют отправлять криптовалюту прямо в сообщениях.\n\n· Персональный чек - для отправки монет одному пользователю\n· Мульти-чек - для отправки монет нескольким пользователям с возможностью добавить условие подписки\n· Rocket-чек - улучшенный мульти-чек с наградой за распространение\n\nВыберите тип чека:";
pub const XR_PERSONAL: &str = "🏷 Персональные чеки\n  \nСоздайте персональный чек, чтобы отправить сообщение с монетами выбранному вами пользователю.";
pub const XR_CHOOSE_COIN: &str = "🏷 Персональный чек\n      \nВыберите валюту чека:";
/// Синтетика: мульти-чеки в симуляторе не создаются.
pub const XR_MULTI_UNSUPPORTED: &str = "Мульти-чеки в симуляторе не поддерживаются.";

/// Экран ввода суммы; `insufficient` — бот дописал «Недостаточно средств» после неудачной суммы.
pub fn xr_ask_amount(
    asset: Asset,
    max: &Money,
    max_usd: Decimal,
    min: &Money,
    min_usd: Decimal,
    insufficient: bool,
) -> String {
    let code = asset.platform_code(Platform::XRocket);
    let mut s = format!(
        "🏷 Персональный чек\n      \nСколько {code} вы хотите отправить пользователю с помощью чека?\n      \nМаксимум: {}\nМинимум: {}\n\nВведите сумму чека в {code}:",
        bare(Platform::XRocket, max, max_usd),
        bare(Platform::XRocket, min, min_usd)
    );
    if insufficient {
        s.push_str("\n\n🔺 Недостаточно средств на балансе.");
    }
    s
}

pub fn xr_confirm(m: &Money, usd: Decimal) -> String {
    format!(
        "🏷 Персональный чек\n\nСумма чека: {}\n\nПожалуйста, подтвердите корректность данных:",
        bare(Platform::XRocket, m, usd)
    )
}

pub fn xr_check(m: &Money, usd: Decimal, url: &str) -> String {
    format!(
        "🏷 Персональный чек\n         \nСумма чека: {}\n\n‼️ Никогда не делайте скриншот и не передавайте ссылку на чек людям, которым вы не доверяете. Делая это, вы передаете средства в этом чеке безвозмездно, без гарантий получить что-то взамен. Мошенники могут использовать это, чтобы получить ваши монеты\n\nСсылка на чек:\n{url}",
        bare(Platform::XRocket, m, usd)
    )
}

// ---------- счета CryptoBot ----------

/// Сумма счёта в фиате: `$5` для долларов, иначе `5 EUR`.
pub fn cb_fiat(amount_: Decimal, currency: &str) -> String {
    if currency == "USD" {
        cb_usd(amount_)
    } else {
        format!("{} {currency}", amount(amount_))
    }
}

/// Счёт в фиате: выбор монеты, сумма в монете — на кнопке.
pub fn cb_invoice_choose_coin(number: &str, fiat: &str) -> String {
    format!("Выберите монету для оплаты счета #{number} на сумму {fiat}.")
}

pub fn cb_invoice_confirm(number: &str, due: &Money, usd: Decimal) -> String {
    format!(
        "📥 Подтвердите оплату счёта #{number}\n\nОтправляете: {}\n   \nВы уверены, что хотите оплатить этот счёт?",
        bare(Platform::CryptoBot, due, usd)
    )
}

pub const CB_TRUST_RECIPIENT: &str = "👉 Вы уверены, что доверяете этому получателю? Вы не переводили ему монеты ранее.\n\nЕсли вы переводите монеты по просьбе незнакомых вам лиц, это может быть мошенничеством. Мы не отвечаем за качество услуг или товаров, предоставленных третьими лицами. Подробнее ›\n\nОбо всех мошеннических действиях сообщайте в @CryptoSupportBot.";

/// Денег не хватает: бот предлагает пополнить из сети.
pub fn cb_choose_network(asset: Asset) -> String {
    format!(
        "Выберите сеть для оплаты счёта в {}.",
        asset.platform_code(Platform::CryptoBot)
    )
}

pub const CB_PAYING: &str = "💸";
pub const CB_PAID_MARK: &str = "👌";

/// Счёт оплачен. Для счёта в долларах: `на сумму 4.27 USD (4.274303 USDT)`.
pub fn cb_invoice_paid(
    number: &str,
    due: &Money,
    usd: Decimal,
    fiat: Option<Decimal>,
    recipient: Option<&str>,
) -> String {
    let sum = match fiat {
        Some(f) => format!(
            "{} USD ({})",
            f.round_dp(4).normalize(),
            plain(Platform::CryptoBot, due)
        ),
        None => money(Platform::CryptoBot, due, usd),
    };
    let mut s = format!("📥 Вы оплатили счёт #{number} на сумму {sum}.");
    if let Some(r) = recipient {
        s.push_str(&format!("\n\nПолучатель: {r}"));
    }
    s
}

/// Синтетика по объявлению «Траты без код-пароля»: живого текста ещё нет.
pub const CB_PASSCODE_REQUIRED: &str = "Траты за последние 24 часа превышают лимит без код-пароля. Введите код-пароль, чтобы подтвердить операцию.";

// ---------- счета xRocket ----------

/// Карточка счёта. `screen` — хвост экрана: выбор способа, подтверждение или нехватка.
fn xr_invoice_head(
    number: &str,
    due: &Money,
    usd: Decimal,
    balance: &Money,
    bal_usd: Decimal,
) -> String {
    format!(
        "🧾 Счёт #{number}\n\nСумма: {}\n      \n⚠️ Внимание, вы оплачиваете счёт, выставленный вам другим пользователем. @xRocket не гарантирует, что вы получите что-либо взамен после оплаты.\n\nВаш баланс: {}",
        bare(Platform::XRocket, due, usd),
        bare(Platform::XRocket, balance, bal_usd)
    )
}

pub fn xr_invoice(
    number: &str,
    due: &Money,
    usd: Decimal,
    balance: &Money,
    bal_usd: Decimal,
) -> String {
    format!(
        "{}\n\nВыберите способ оплаты:",
        xr_invoice_head(number, due, usd, balance, bal_usd)
    )
}

pub fn xr_invoice_confirm(
    number: &str,
    due: &Money,
    usd: Decimal,
    balance: &Money,
    bal_usd: Decimal,
) -> String {
    format!(
        "{}\n\n🔸 Подтвердите ваш платёж на {}",
        xr_invoice_head(number, due, usd, balance, bal_usd),
        bare(Platform::XRocket, due, usd)
    )
}

pub fn xr_invoice_insufficient(
    number: &str,
    due: &Money,
    usd: Decimal,
    balance: &Money,
    bal_usd: Decimal,
) -> String {
    format!(
        "{}\n     \n🔺 Недостаточно средств на балансе.",
        xr_invoice_head(number, due, usd, balance, bal_usd)
    )
}

pub const XR_LOADING: &str = "⏱ Загрузка";

pub fn xr_invoice_paid(due: &Money, usd: Decimal, payment_id: &str) -> String {
    format!(
        "📋 Вы успешно оплатили счёт на сумму {}. ID платежа: #{payment_id}",
        bare(Platform::XRocket, due, usd)
    )
}

// ---------- счета, общее ----------

pub fn invoice_already_paid(platform: Platform) -> &'static str {
    match platform {
        // Синтетика: живого текста CryptoBot нет.
        Platform::CryptoBot => "Этот счёт уже оплачен.",
        Platform::XRocket => "Данный счёт уже оплачен.",
    }
}

/// Синтетика.
pub fn invoice_not_found(platform: Platform) -> &'static str {
    match platform {
        Platform::CryptoBot => "Счёт не найден.",
        Platform::XRocket => "Данный счёт не найден.",
    }
}

/// Синтетика.
pub const INVOICE_EXPIRED: &str = "Срок действия счёта истёк.";
/// Синтетика: пометка многоразового счёта на первом экране (живого текста ещё нет).
pub const MULTI_USE_MARK: &str = "\n\nМногоразовый счёт.";
/// Синтетика.
pub const INVOICE_OWN: &str = "Нельзя оплатить собственный счёт.";

/// Синтетика: создателю счёта пришла оплата.
pub fn invoice_paid_notice(
    platform: Platform,
    number: &str,
    m: &Money,
    usd: Decimal,
    who: &str,
) -> String {
    format!(
        "💰 Счёт #{number} оплачен пользователем {who}: +{}.",
        money(platform, m, usd)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Platform::{CryptoBot as CB, XRocket as XR};
    use rust_decimal_macros::dec;

    fn usdt(d: Decimal) -> Money {
        Money::new(d, Asset::Usdt).unwrap()
    }

    #[test]
    fn usd_formats() {
        assert_eq!(cb_usd(dec!(4.99)), "$4.99");
        assert_eq!(cb_usd(dec!(5)), "$5");
        assert_eq!(cb_usd(dec!(5.0000)), "$5");
        assert_eq!(cb_usd(dec!(0.014)), "$0.01");
        assert_eq!(xr_usd(dec!(5)), "5.0$");
        assert_eq!(xr_usd(dec!(5.02)), "5.0$");
        assert_eq!(xr_usd(dec!(0.15)), "0.2$");
        assert_eq!(xr_usd(dec!(3.3)), "3.3$");
        assert_eq!(xr_usd(dec!(0.01)), "0.01$");
    }

    /// Тексты симулятора с теми же суммами и кодами — те же, что в живой записи.
    #[test]
    fn same_as_live() {
        let live: serde_json::Value = serde_json::from_str(include_str!(
            "../../../parsers/tests/fixtures/live_wallet_replies.json"
        ))
        .unwrap();
        let texts: Vec<&str> = live["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["text"].as_str().unwrap())
            .collect();
        let ours = [
            received(CB, &usdt(dec!(5)), dec!(4.99)),
            receiving(CB, &usdt(dec!(5)), dec!(4.99)),
            received(XR, &usdt(dec!(3.3)), dec!(3.3)),
            receiving(XR, &usdt(dec!(1)), dec!(1)),
            already_activated(CB).to_owned(),
            already_activated(XR).to_owned(),
            not_found(CB, "CQx").to_owned(),
            not_found(XR, "t_x").to_owned(),
            not_found(XR, "mc_x").to_owned(),
            not_for_you(CB).to_owned(),
            claim_notice(CB, &usdt(dec!(5)), dec!(4.99), "Ivan"),
            claim_notice(XR, &usdt(dec!(6.5)), dec!(6.5), "Ivan"),
            incoming_transfer(CB, &usdt(dec!(4.77)), dec!(4.76), "Shop"),
            cb_checks_menu(),
            CB_CHOOSE_COIN.to_owned(),
            cb_ask_amount(Asset::Usdt, &usdt(dec!(5.05353)), dec!(5.05)),
            cb_ask_activations(2),
            cb_check(&usdt(dec!(4)), dec!(3.99), "CQs9CiUoXUDt"),
            cb_insufficient(&usdt(dec!(5.05353)), dec!(5.05)),
            CB_MANAGE.to_owned(),
            XR_CHEQUES_MENU.to_owned(),
            XR_PERSONAL.to_owned(),
            xr_confirm(&usdt(dec!(1)), dec!(1)),
            xr_check(
                &usdt(dec!(0.15)),
                dec!(0.15),
                "https://t.me/xrocket?start=t_bqBl2ZFBlwE6CwL",
            ),
            cb_invoice_choose_coin("IV95554731", &cb_fiat(dec!(5), "USD")),
            cb_invoice_confirm("IV95554731", &usdt(dec!(5.006321)), dec!(5)),
            CB_TRUST_RECIPIENT.to_owned(),
            cb_choose_network(Asset::Usdt),
            CB_PAYING.to_owned(),
            CB_PAID_MARK.to_owned(),
            cb_invoice_paid("IV95554731", &usdt(dec!(5.006321)), dec!(5), None, None),
            cb_invoice_paid(
                "IV93498164",
                &usdt(dec!(4.274303)),
                dec!(4.27),
                Some(dec!(4.27)),
                Some("Shop"),
            ),
            xr_invoice(
                "INV3524202",
                &usdt(dec!(10)),
                dec!(10),
                &usdt(dec!(5.02)),
                dec!(5.02),
            ),
            XR_LOADING.to_owned(),
            invoice_already_paid(XR).to_owned(),
            wallet(XR, &[(usdt(dec!(10)), dec!(10))])
                .split("\n\n🪙")
                .next()
                .unwrap()
                .to_owned(),
        ];
        for t in &ours {
            assert!(
                texts.iter().any(|l| l == t || l.starts_with(t.as_str())),
                "not in the live recording: {t:?}"
            );
        }
    }

    #[test]
    fn xrocket_writes_toncoin() {
        let m = Money::new(dec!(0.5), Asset::Ton).unwrap();
        assert_eq!(plain(XR, &m), "0.5 TONCOIN");
        assert_eq!(plain(CB, &m), "0.5 TON");
    }
}
