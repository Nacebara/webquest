//! Виджеты Bot API 10.4 (шаг 6.7 плана, решение владельца 13 ноября 2026): HTML каждого
//! утверждённого виджета. Код один на бота и на страницу предпросмотра макетов —
//! `assets/widgets/*.html` (`docs/mockups/widgets/build.py` берёт его оттуда же).
//!
//! Код виджета задаётся при отправке и больше не меняется; меняется только состояние
//! (`chat_data`), и пишет его бот. Деньги считает бот, виджет только показывает готовые строки
//! (`docs/BOTAPI-WIDGETS.md`, правила exch). Лимиты Telegram проверяет тест `tests/widgets.rs`.

/// Код виджета: не больше 32768 байт.
pub const HTML_MAX: usize = 32_768;
/// И не больше 4096 байт после zlib уровня 9 — так считает сервер.
pub const ZIP_MAX: usize = 4_096;
/// Состояние (`chat_data`, `public_data`) и ответ на запрос — до 1024 байт UTF-8.
pub const DATA_MAX: usize = 1_024;
/// Подпись под виджетом — до 1024 знаков после разбора разметки.
pub const CAPTION_MAX: usize = 1_024;

/// Утверждённый виджет.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WidgetKind {
    /// Живая заявка, карточка заявки (клиент).
    OrderA,
    /// «Пришлите чек» — живая квитанция (клиент).
    CalcB,
    /// Калькулятор курса с ⇅ (клиент).
    CalcA,
    /// Живой счёт (клиент).
    Invoice,
    /// Рассылка «Выгодно» (клиент).
    Promo,
    /// «Инвестиции», вариант Б (клиент).
    InvHome,
    /// Вывод инвестора (клиент).
    InvWithdraw,
    /// Калькулятор инвестора (клиент).
    InvCalc,
    /// «Живой пульт».
    Dash,
    /// «Аккаунты» с точкой статуса.
    Accounts,
    /// «Резервы и сверка».
    Reserves,
    /// «Ход процесса»: самопроверка, рассылка, замена аккаунта, эмодзи.
    Process,
    /// Ввод кода входа.
    Code,
    /// Очередь разбора.
    Queue,
    /// «Изменить значение».
    Value,
    /// «Ребалансировка».
    Rebalance,
    /// Вывод прибыли.
    Withdraw,
    /// «Обслуживание».
    Maint,
    /// «Система».
    System,
}

impl WidgetKind {
    /// Все виджеты — для «Предпросмотра» и проверки лимитов.
    pub const ALL: [Self; 19] = [
        Self::OrderA,
        Self::CalcB,
        Self::CalcA,
        Self::Invoice,
        Self::Promo,
        Self::InvHome,
        Self::InvWithdraw,
        Self::InvCalc,
        Self::Dash,
        Self::Accounts,
        Self::Reserves,
        Self::Process,
        Self::Code,
        Self::Queue,
        Self::Value,
        Self::Rebalance,
        Self::Withdraw,
        Self::Maint,
        Self::System,
    ];

    /// Имя файла в `assets/widgets` и виджета в макетах (`samples.json`, поле `w`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::OrderA => "order_a",
            Self::CalcB => "calc_b",
            Self::CalcA => "calc_a",
            Self::Invoice => "invoice",
            Self::Promo => "promo",
            Self::InvHome => "inv_home",
            Self::InvWithdraw => "inv_withdraw",
            Self::InvCalc => "inv_calc",
            Self::Dash => "dash",
            Self::Accounts => "accounts",
            Self::Reserves => "reserves",
            Self::Process => "process",
            Self::Code => "code",
            Self::Queue => "queue",
            Self::Value => "value",
            Self::Rebalance => "rebalance",
            Self::Withdraw => "withdraw",
            Self::Maint => "maint",
            Self::System => "system",
        }
    }

    /// HTML виджета, каким его утвердил владелец.
    pub const fn html(self) -> &'static str {
        match self {
            Self::OrderA => include_str!("../../../assets/widgets/order_a.html"),
            Self::CalcB => include_str!("../../../assets/widgets/calc_b.html"),
            Self::CalcA => include_str!("../../../assets/widgets/calc_a.html"),
            Self::Invoice => include_str!("../../../assets/widgets/invoice.html"),
            Self::Promo => include_str!("../../../assets/widgets/promo.html"),
            Self::InvHome => include_str!("../../../assets/widgets/inv_home.html"),
            Self::InvWithdraw => include_str!("../../../assets/widgets/inv_withdraw.html"),
            Self::InvCalc => include_str!("../../../assets/widgets/inv_calc.html"),
            Self::Dash => include_str!("../../../assets/widgets/dash.html"),
            Self::Accounts => include_str!("../../../assets/widgets/accounts.html"),
            Self::Reserves => include_str!("../../../assets/widgets/reserves.html"),
            Self::Process => include_str!("../../../assets/widgets/process.html"),
            Self::Code => include_str!("../../../assets/widgets/code.html"),
            Self::Queue => include_str!("../../../assets/widgets/queue.html"),
            Self::Value => include_str!("../../../assets/widgets/value.html"),
            Self::Rebalance => include_str!("../../../assets/widgets/rebalance.html"),
            Self::Withdraw => include_str!("../../../assets/widgets/withdraw.html"),
            Self::Maint => include_str!("../../../assets/widgets/maint.html"),
            Self::System => include_str!("../../../assets/widgets/system.html"),
        }
    }

    /// Виджет пульта (служебный), а не клиента.
    pub const fn for_staff(self) -> bool {
        matches!(
            self,
            Self::Dash
                | Self::Accounts
                | Self::Reserves
                | Self::Process
                | Self::Code
                | Self::Queue
                | Self::Value
                | Self::Rebalance
                | Self::Withdraw
                | Self::Maint
                | Self::System
        )
    }

    /// Виджет по имени из макетов; незнакомое имя — `None`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

/// Влезает ли состояние или ответ виджету в лимит Telegram (1024 байта UTF-8).
pub fn data_fits(data: &str) -> bool {
    data.len() <= DATA_MAX
}
