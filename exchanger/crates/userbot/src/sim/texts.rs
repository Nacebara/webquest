//! Тексты ботов симулятора. Подобраны под словари lovec (`replies.rs`, DESIGN-v0.2 §3):
//! каждый ответ на `/start <чек>` попадает ровно в один класс, служебные экраны (меню,
//! кошелёк, счёт) не содержат подстрок из словарей активации. Менять — только вместе
//! с тестом `texts_match_lovec_needles`.

use domain::{Asset, Decimal, Money, Platform};

/// Сумма так, как её пишет бот: `10 USDT`, `0.5 TON`, у xRocket — `0.5 TONCOIN`.
pub fn money(platform: Platform, m: &Money) -> String {
    format!(
        "{} {}",
        amount(m.amount()),
        m.asset().platform_code(platform)
    )
}

pub fn amount(d: Decimal) -> String {
    d.normalize().to_string()
}

fn logo(platform: Platform) -> &'static str {
    match platform {
        Platform::CryptoBot => "🦋",
        Platform::XRocket => "🚀",
    }
}

fn asset_name(asset: Asset) -> &'static str {
    match asset {
        Asset::Usdt => "Tether",
        Asset::Ton => "Toncoin",
    }
}

// ---------- ответы на `/start <чек>` ----------

pub fn received(platform: Platform, m: &Money) -> String {
    match platform {
        Platform::CryptoBot => format!("Вы получили {}.", money(platform, m)),
        Platform::XRocket => format!("🚀 Вы получили {}", money(platform, m)),
    }
}

pub const ALREADY_ACTIVATED: &str = "Этот чек уже активирован.";
pub const CHECK_NOT_FOUND: &str = "Чек не найден.";
pub const MULTI_CHECK_NOT_FOUND: &str = "Мульти-чек не найден.";
pub const NOT_FOR_YOU: &str = "Вы не можете активировать этот чек.";
pub const CAPTCHA: &str = "Введите символы, которые вы видите на картинке.";
pub const PASSWORD_PROMPT: &str = "Введите пароль от чека.";
pub const WRONG_PASSWORD: &str = "Неверный пароль, попробуйте ещё раз.";
pub const PREMIUM_ONLY: &str = "Этот чек могут активировать только пользователи Telegram Premium.";

pub fn not_found(code: &str) -> &'static str {
    if code.starts_with("mc_") || code.starts_with("mci_") {
        MULTI_CHECK_NOT_FOUND
    } else {
        CHECK_NOT_FOUND
    }
}

pub fn needs_subscription(channel: &str) -> String {
    format!("Подпишитесь на канал {channel}, чтобы активировать этот чек.")
}

/// Создатель открыл свой чек. Нарочно без «ваш чек» и «получил»: это не уведомление.
pub fn own_check(platform: Platform, m: &Money, url: &str) -> String {
    format!(
        "{} Это ваш собственный чек на {}.\n\nСсылка: {url}",
        logo(platform),
        money(platform, m)
    )
}

// ---------- уведомления ----------

/// Создателю чека: «Ваш чек активировал @user.», ниже — сумма и ссылка этого чека
/// (по ней движок понимает, какую выплату забрали).
pub fn claim_notice(platform: Platform, m: &Money, who: &str, url: &str) -> String {
    format!(
        "Ваш чек активировал {who}.\n\nЧек на {} — {url}",
        money(platform, m)
    )
}

/// Входящий перевод: «Вы получили 5 USDT от @x» (ловушка словаря выигрыша, AUDIT.md:82).
pub fn incoming_transfer(platform: Platform, m: &Money, from: &str) -> String {
    format!("Вы получили {} от {from}.", money(platform, m))
}

pub const PROCESSING: &str = "Подождите, идёт обработка…";
pub const UNKNOWN_DEFAULT: &str = "Что-то пошло не так. Попробуйте позже.";

// ---------- общее ----------

pub fn welcome(platform: Platform) -> String {
    match platform {
        Platform::CryptoBot => {
            "🦋 CryptoBot — кошелёк в Telegram.\n\n/wallet — кошелёк\n/checks — чеки".to_owned()
        }
        Platform::XRocket => {
            "🚀 xRocket — кошелёк в Telegram.\n\n/wallet — кошелёк\n/cheques — чеки".to_owned()
        }
    }
}

pub const UNKNOWN_COMMAND: &str = "Не понимаю. Отправьте /start, чтобы открыть меню.";

/// Экран «Кошелёк»: строка на актив, сумма с кодом актива платформы.
pub fn wallet(platform: Platform, balances: &[Money]) -> String {
    let title = match platform {
        Platform::CryptoBot => "👛 Кошелёк",
        Platform::XRocket => "👛 Мой кошелёк",
    };
    let mut s = format!("{title}\n");
    for m in balances {
        s.push_str(&format!(
            "\n{}: {}",
            asset_name(m.asset()),
            money(platform, m)
        ));
    }
    s
}

// ---------- меню чеков ----------

pub const CB_CHECKS_MENU: &str =
    "🦋 Чеки\n\nЧеки позволяют отправить криптовалюту любому пользователю Telegram.";
pub const XR_CHEQUES_MENU: &str = "🚀 Чеки\n\nВыберите тип чека.";
pub const XR_PERSONAL: &str = "Персональный чек\n\nЧек сможет активировать один пользователь.";
pub const CHOOSE_CHECK_ASSET: &str = "Выберите валюту чека.";
pub const BAD_AMOUNT: &str = "Неверная сумма. Отправьте число, например 10.5.";
pub const MULTI_UNSUPPORTED: &str = "Мульти-чеки в симуляторе не поддерживаются.";
pub const NO_ACTIVE_CHECKS: &str = "У вас нет активных чеков.";

pub fn ask_check_amount(platform: Platform, asset: Asset, available: &Money) -> String {
    let label = match platform {
        Platform::CryptoBot => "Доступно",
        Platform::XRocket => "Баланс",
    };
    format!(
        "Отправьте сумму чека в {}.\n\n{label}: {}.",
        asset.platform_code(platform),
        money(platform, available)
    )
}

pub fn check_created(platform: Platform, m: &Money, url: &str) -> String {
    format!(
        "{} Чек на {} создан.\n\nСсылка на чек: {url}",
        logo(platform),
        money(platform, m)
    )
}

pub fn insufficient_for_check(platform: Platform, available: &Money) -> String {
    format!(
        "Недостаточно средств. Доступно: {}.",
        money(platform, available)
    )
}

pub fn active_checks(platform: Platform, lines: &[(Money, String)]) -> String {
    let mut s = String::from("Активные чеки:\n");
    for (m, url) in lines {
        s.push_str(&format!("\n• Чек на {} — {url}", money(platform, m)));
    }
    s
}

// ---------- инлайн-режим ----------

pub fn inline_check_title(platform: Platform, m: &Money) -> String {
    match platform {
        Platform::CryptoBot => format!("Отправить {}", money(platform, m)),
        Platform::XRocket => format!("Чек на {}", money(platform, m)),
    }
}

pub fn inline_check_description(platform: Platform, m: &Money, available: &Money) -> String {
    format!(
        "Чек на {}. Доступно: {}",
        money(platform, m),
        money(platform, available)
    )
}

pub const INLINE_INSUFFICIENT_TITLE: &str = "Недостаточно средств";

pub fn inline_insufficient_description(platform: Platform, m: &Money, available: &Money) -> String {
    format!(
        "Нужно {}, доступно {}",
        money(platform, m),
        money(platform, available)
    )
}

pub fn inline_invoice_title(platform: Platform, m: &Money) -> String {
    format!("Запросить {}", money(platform, m))
}

pub fn inline_invoice_description(platform: Platform, m: &Money) -> String {
    format!("Счёт на {}", money(platform, m))
}

/// Сообщение с чеком, отправленное через инлайн-режим (ссылка — только в кнопке).
pub fn inline_check_message(platform: Platform, m: &Money) -> String {
    format!("{} Чек на {}", logo(platform), money(platform, m))
}

pub fn inline_check_button(platform: Platform, m: &Money) -> String {
    format!("Получить {}", money(platform, m))
}

pub fn inline_invoice_message(platform: Platform, m: &Money) -> String {
    format!("🧾 Счёт на {}", money(platform, m))
}

pub const INLINE_INSUFFICIENT_MESSAGE: &str = "Недостаточно средств для создания чека.";

// ---------- счета ----------

pub const INVOICE_NOT_FOUND: &str = "Счёт не найден.";
pub const INVOICE_ALREADY_PAID: &str = "Этот счёт уже оплачен.";
pub const INVOICE_EXPIRED: &str = "Срок действия счёта истёк.";
pub const INVOICE_OWN: &str = "Нельзя оплатить собственный счёт.";
pub const CHOOSE_PAY_ASSET: &str = "Выберите валюту для оплаты.";

pub struct CardView<'a> {
    pub platform: Platform,
    pub code: &'a str,
    pub amount_line: String,
    pub description: Option<&'a str>,
    pub single_use: bool,
    pub status_line: &'static str,
}

pub fn status_line(status: super::SimInvoiceStatus) -> &'static str {
    match status {
        super::SimInvoiceStatus::Active => "Статус: ожидает оплаты",
        super::SimInvoiceStatus::Paid => "Статус: оплачен",
        super::SimInvoiceStatus::Expired => "Статус: истёк",
    }
}

fn card_head(v: &CardView<'_>) -> String {
    let icon = match v.platform {
        Platform::CryptoBot => "🧾",
        Platform::XRocket => "🚀",
    };
    let mut s = format!("{icon} Счёт {}\n\nСумма: {}", v.code, v.amount_line);
    if let Some(d) = v.description {
        s.push_str(&format!("\nОписание: {d}"));
    }
    s.push_str(if v.single_use {
        "\nОдноразовый счёт"
    } else {
        "\nМногоразовый счёт"
    });
    s
}

pub fn invoice_card(v: &CardView<'_>, active: bool) -> String {
    let mut s = card_head(v);
    s.push_str(&format!("\n{}", v.status_line));
    if active {
        s.push_str(&format!("\n\n{CHOOSE_PAY_ASSET}"));
    }
    s
}

pub fn invoice_pay_screen(v: &CardView<'_>, due: &Money, available: &Money) -> String {
    format!(
        "{}\n\nК оплате: {}\nДоступно: {}",
        card_head(v),
        money(v.platform, due),
        money(v.platform, available)
    )
}

pub fn invoice_paid(platform: Platform, code: &str, m: &Money) -> String {
    format!(
        "✅ Счёт {code} оплачен.\n\nСписано: {}.",
        money(platform, m)
    )
}

pub fn invoice_insufficient(platform: Platform, available: &Money, due: &Money) -> String {
    format!(
        "Недостаточно средств для оплаты счёта. Доступно: {}, нужно {}.",
        money(platform, available),
        money(platform, due)
    )
}

/// Создателю счёта: оплата пришла.
pub fn invoice_paid_notice(platform: Platform, code: &str, m: &Money, who: &str) -> String {
    format!(
        "💰 Счёт {code} оплачен пользователем {who}: +{}.",
        money(platform, m)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// Классификатор lovec (`replies.rs`) один в один: проверяем, что тексты симулятора
    /// попадают в нужные классы и служебные экраны не задевают словари.
    fn lovec_class(text: &str) -> &'static str {
        let t = text.to_lowercase();
        let has = |n: &[&str]| n.iter().any(|x| t.contains(x));
        if t.contains("ваш чек") && (t.contains("активировал") || t.contains("получил"))
        {
            return "notice";
        }
        if has(&["неверный пароль", "неправильный пароль", "wrong password"])
        {
            "wrong_password"
        } else if has(&["уже активирован", "уже получен", "already activated"])
        {
            "lost"
        } else if has(&[
            "не можете активировать",
            "предназначен для другого",
            "cannot activate",
        ]) {
            "not_for_us"
        } else if has(&["успешно получили", "вы получили", "you received"])
        {
            "won"
        } else if has(&["не найден", "не существует", "недоступен", "not found"])
        {
            "invalid"
        } else if has(&[
            "капч",
            "captcha",
            "символы, которые вы видите",
            "на картинке",
        ]) {
            "captcha"
        } else if has(&["пароль", "password"]) {
            "password"
        } else if has(&["подпиш", "подписк", "subscribe", "join the"]) {
            "needs_join"
        } else if has(&[
            "получение",
            "обработ",
            "processing",
            "подождите",
            "please wait",
        ]) {
            "in_progress"
        } else {
            "unknown"
        }
    }

    #[test]
    fn texts_match_lovec_needles() {
        let m = Money::new(dec!(100), Asset::Usdt).unwrap();
        let cb = Platform::CryptoBot;
        let xr = Platform::XRocket;
        assert_eq!(received(cb, &m), "Вы получили 100 USDT.");
        assert_eq!(lovec_class(&received(cb, &m)), "won");
        assert_eq!(lovec_class(&received(xr, &m)), "won");
        assert_eq!(lovec_class(ALREADY_ACTIVATED), "lost");
        assert_eq!(lovec_class(CHECK_NOT_FOUND), "invalid");
        assert_eq!(lovec_class(MULTI_CHECK_NOT_FOUND), "invalid");
        assert_eq!(lovec_class(NOT_FOR_YOU), "not_for_us");
        assert_eq!(lovec_class(&needs_subscription("@chan")), "needs_join");
        assert_eq!(lovec_class(CAPTCHA), "captcha");
        assert_eq!(lovec_class(PASSWORD_PROMPT), "password");
        assert_eq!(lovec_class(WRONG_PASSWORD), "wrong_password");
        assert_eq!(lovec_class(PROCESSING), "in_progress");
        let notice = claim_notice(cb, &m, "@client", "https://t.me/send?start=CQAbCdEfGhIj");
        assert!(notice.starts_with("Ваш чек активировал @client."));
        assert_eq!(lovec_class(&notice), "notice");
        // Ловушка: входящий перевод похож на выигрыш, отличается «от @».
        let transfer = incoming_transfer(cb, &m, "@x");
        assert_eq!(lovec_class(&transfer), "won");
        assert!(transfer.contains("от @"));
        // Остальное — вне словарей активации.
        let url = "https://t.me/send?start=CQAbCdEfGhIj";
        for t in [
            PREMIUM_ONLY.to_owned(),
            UNKNOWN_DEFAULT.to_owned(),
            UNKNOWN_COMMAND.to_owned(),
            own_check(cb, &m, url),
            welcome(cb),
            welcome(xr),
            wallet(cb, &[m]),
            wallet(xr, &[m]),
            CB_CHECKS_MENU.to_owned(),
            XR_CHEQUES_MENU.to_owned(),
            XR_PERSONAL.to_owned(),
            CHOOSE_CHECK_ASSET.to_owned(),
            ask_check_amount(cb, Asset::Usdt, &m),
            ask_check_amount(xr, Asset::Usdt, &m),
            check_created(cb, &m, url),
            insufficient_for_check(cb, &m),
            active_checks(cb, &[(m, url.to_owned())]),
            NO_ACTIVE_CHECKS.to_owned(),
            BAD_AMOUNT.to_owned(),
            MULTI_UNSUPPORTED.to_owned(),
            inline_check_message(cb, &m),
            INLINE_INSUFFICIENT_MESSAGE.to_owned(),
            invoice_paid(cb, "IVAbCdEfGhIj", &m),
            invoice_insufficient(cb, &m, &m),
            INVOICE_ALREADY_PAID.to_owned(),
            INVOICE_EXPIRED.to_owned(),
        ] {
            assert_eq!(lovec_class(&t), "unknown", "{t}");
        }
    }

    #[test]
    fn xrocket_writes_toncoin() {
        let m = Money::new(dec!(0.5), Asset::Ton).unwrap();
        assert_eq!(money(Platform::XRocket, &m), "0.5 TONCOIN");
        assert_eq!(money(Platform::CryptoBot, &m), "0.5 TON");
    }
}
