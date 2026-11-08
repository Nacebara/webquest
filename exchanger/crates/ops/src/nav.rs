//! Навигация пульта — как в гаранте, только удобнее:
//!
//! - **Нажимаемые строки.** Номер заявки, клиент, сотрудник в таблице — ссылка на этого же
//!   бота `t.me/<бот>?start=a__<кнопка>`. Нажали — бот удаляет служебное «/start» и открывает
//!   экран в том же окне пульта (последнее сообщение пульта в чате), а не новой лентой.
//! - **Страницы.** Длинный список — по [`PAGE`] строк, внизу «‹ Назад · 2 / 5 · Дальше ›»;
//!   номер страницы виден всегда.
//! - **Возврат.** Внизу каждого экрана — куда вернуться («‹ Разбор») и «Меню»; у разделов
//!   меню — «‹ Меню». Вопрос пульта отменяется, если уйти с него любой кнопкой.

use botapi::InlineKeyboardButton;

use crate::cb::Cb;
use crate::texts::{Texts, esc};
use crate::{Inner, btn};

/// Строк на странице списка.
pub const PAGE: usize = 8;

/// Списки с листанием.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum List {
    /// «Разбор».
    Queue,
    /// «Заявки» — последние заявки.
    Orders,
    /// «Клиенты» — последние клиенты.
    Users,
    /// «Обращения».
    Inbox,
    /// «Журнал».
    Audit,
    /// «Инвесторы».
    Investors,
}

impl List {
    pub(crate) const ALL: [List; 6] = [
        List::Queue,
        List::Orders,
        List::Users,
        List::Inbox,
        List::Audit,
        List::Investors,
    ];

    pub(crate) fn code(self) -> &'static str {
        match self {
            List::Queue => "q",
            List::Orders => "o",
            List::Users => "u",
            List::Inbox => "i",
            List::Audit => "a",
            List::Investors => "v",
        }
    }

    pub(crate) fn of(code: &str) -> Option<List> {
        List::ALL.into_iter().find(|l| l.code() == code)
    }
}

/// Сколько страниц у списка из `total` строк (не меньше одной).
pub(crate) fn pages(total: usize) -> u32 {
    u32::try_from(total.div_ceil(PAGE).max(1)).unwrap_or(u32::MAX)
}

/// Страница в пределах списка и диапазон строк на ней.
pub(crate) fn window(total: usize, page: u32) -> (u32, std::ops::Range<usize>) {
    let last = pages(total) - 1;
    let page = page.min(last);
    let start = usize::try_from(page).unwrap_or(0) * PAGE;
    (page, start.min(total)..(start + PAGE).min(total))
}

/// Метка ссылки-перехода: `a__` и тело кнопки, `:` → `__` (в `start` можно только
/// `A-Za-z0-9_-`, до 64 символов).
pub(crate) fn payload(cb: &Cb) -> Option<String> {
    let body = cb.encode();
    let body = body.strip_prefix("a:")?;
    let p = format!("a__{}", body.replace(':', "__"));
    (p.len() <= 64
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then_some(p)
}

/// Кнопка по метке ссылки (`a__o__15` → «заявка 15»).
pub(crate) fn parse_payload(p: &str) -> Option<Cb> {
    let body = p.strip_prefix("a__")?;
    Cb::parse(&format!("a:{}", body.replace("__", ":")))
}

/// Листалка: «‹ Назад · 2 / 5 · Дальше ›»; одна страница — ничего.
pub fn pager(t: &Texts, list: List, page: u32, total: usize) -> Vec<InlineKeyboardButton> {
    let n = pages(total);
    if n <= 1 {
        return Vec::new();
    }
    let mut row = Vec::new();
    if page > 0 {
        row.push(btn(t.t("nav.prev"), &Cb::Page(list, page - 1)));
    }
    row.push(InlineKeyboardButton::disabled(
        t.f("nav.page", &[("n", &(page + 1)), ("of", &n)]),
    ));
    if page + 1 < n {
        row.push(btn(t.t("nav.next"), &Cb::Page(list, page + 1)));
    }
    row
}

/// Листалка по своей кнопке страницы (заявки клиента и т. п.).
pub(crate) fn pager_with(
    t: &Texts,
    page: u32,
    total: usize,
    to: impl Fn(u32) -> Cb,
) -> Vec<InlineKeyboardButton> {
    let n = pages(total);
    if n <= 1 {
        return Vec::new();
    }
    let mut row = Vec::new();
    if page > 0 {
        row.push(btn(t.t("nav.prev"), &to(page - 1)));
    }
    row.push(InlineKeyboardButton::disabled(
        t.f("nav.page", &[("n", &(page + 1)), ("of", &n)]),
    ));
    if page + 1 < n {
        row.push(btn(t.t("nav.next"), &to(page + 1)));
    }
    row
}

/// Название экрана для «‹ …»: куда ведёт кнопка возврата.
fn parent_name(t: &Texts, to: &Cb) -> String {
    let key = match to {
        Cb::Menu => "nav.to_menu",
        Cb::Dash => "nav.to_dash",
        Cb::Queue | Cb::Page(List::Queue, _) => "nav.to_queue",
        Cb::Page(List::Orders, _) => "nav.to_orders",
        Cb::Page(List::Users, _) => "nav.to_users",
        Cb::Inbox | Cb::Page(List::Inbox, _) => "nav.to_inbox",
        Cb::Audit | Cb::Page(List::Audit, _) => "nav.to_audit",
        Cb::Investors | Cb::Page(List::Investors, _) => "nav.to_investors",
        Cb::Dirs => "nav.to_dirs",
        Cb::Dir(_) => "nav.to_dir",
        Cb::Promo => "nav.to_promo",
        Cb::User(_) | Cb::UserTerms(_) | Cb::UserOrders(..) => "nav.to_user",
        Cb::Settings => "nav.to_settings",
        Cb::Emoji => "nav.to_emoji",
        Cb::Bots => "nav.to_bots",
        Cb::Staff => "nav.to_staff",
        Cb::Reserves => "nav.to_reserves",
        Cb::System => "nav.to_system",
        Cb::Invest(_) => "nav.to_invest",
        Cb::Order(_) => "nav.to_order",
        _ => "nav.to_back",
    };
    t.t(key)
}

/// Строка возврата: «‹ Разбор» и «Меню»; если назад — это меню, то одна «‹ Меню».
pub fn back(t: &Texts, to: &Cb) -> Vec<InlineKeyboardButton> {
    let first = btn(t.f("nav.back", &[("name", &parent_name(t, to))]), to);
    if *to == Cb::Menu {
        vec![first]
    } else {
        vec![first, btn(t.t("nav.menu"), &Cb::Menu)]
    }
}

/// Ссылки-переходы: имя бота, на которого ведут ссылки в таблицах. Без имени — просто текст.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    bot: Option<String>,
}

impl Links {
    pub fn new(bot: Option<String>) -> Self {
        Self { bot }
    }

    /// Нажимаемый текст: ссылка-переход на экран `to`.
    pub fn link(&self, html: &str, to: &Cb) -> String {
        match (self.bot.as_deref(), payload(to)) {
            (Some(bot), Some(p)) => {
                format!("<a href=\"https://t.me/{}?start={p}\">{html}</a>", esc(bot))
            }
            _ => html.to_owned(),
        }
    }
}

impl Inner {
    /// Имя бота для ссылок (`getMe` при запуске пульта).
    pub(crate) fn bot_username(&self) -> Option<&str> {
        self.bot_username.get().map(String::as_str)
    }

    pub(crate) fn links(&self) -> Links {
        Links::new(self.bot_username().map(str::to_owned))
    }

    /// Нажимаемый текст: ссылка-переход на экран `to`.
    pub(crate) fn link(&self, html: &str, to: &Cb) -> String {
        self.links().link(html, to)
    }

    /// Запомнить, из какого списка сотрудник открыл карточку («‹ Назад» в карточке).
    pub(crate) fn remember_list(&self, staff: i64, list: Cb) {
        self.state(|s| {
            s.last_list.insert(staff, list);
        });
    }

    /// Куда вернуться из карточки: в последний открытый список или в `default`.
    pub(crate) fn list_or(&self, staff: i64, default: Cb) -> Cb {
        self.state(|s| s.last_list.get(&staff).cloned())
            .unwrap_or(default)
    }

    /// Окно пульта в чате — последнее сообщение экрана.
    pub(crate) fn set_window(&self, chat: i64, message_id: i32) {
        self.state(|s| {
            s.windows.insert(chat, message_id);
        });
    }

    pub(crate) fn window_of(&self, chat: i64) -> Option<i32> {
        self.state(|s| s.windows.get(&chat).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_and_fits_telegram() {
        for cb in [
            Cb::Order(i64::MAX),
            Cb::User(51_234_567),
            Cb::Page(List::Audit, 12),
            Cb::UserOrders(51_234_567, 3),
            Cb::Question(4242),
            Cb::Member(501),
            Cb::Invest(7),
            Cb::Dir(domain::Direction::CbToXrCheck),
        ] {
            let p = payload(&cb).unwrap();
            assert!(p.len() <= 64, "{p}");
            assert_eq!(parse_payload(&p), Some(cb), "{p}");
        }
        assert_eq!(parse_payload("a__zz"), None);
        assert_eq!(parse_payload("CQabcdef"), None);
    }

    #[test]
    fn pages_and_windows() {
        assert_eq!(pages(0), 1);
        assert_eq!(pages(8), 1);
        assert_eq!(pages(9), 2);
        assert_eq!(window(20, 1), (1, 8..16));
        assert_eq!(window(20, 9), (2, 16..20), "за последней — последняя");
        assert_eq!(window(0, 0), (0, 0..0));
    }
}
