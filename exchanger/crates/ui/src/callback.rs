//! `callback_data` инлайн-кнопок клиента: один формат для рендера и разбора. Данные от
//! клиента недоверенные: разбор строгий, длина ≤ 64 байт (лимит Bot API).

use std::str::FromStr;

use domain::{Direction, Platform};

/// Действие кнопки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    /// Выбрано направление обмена → инструкция «пришлите чек».
    Direction(Direction),
    /// «Как создать чек» для направления.
    HowTo(Direction),
    /// «Ещё обмен» — снова выбор направления.
    Exchange,
    Terms,
    Privacy,
    /// История, страница с нуля.
    History {
        page: u32,
    },
    Order(String),
    /// «Ещё счёт» — то же, что кнопка меню «Оплатить счёт».
    PayInvoice,
    /// B1: развернуть «Детали» в сообщении заявки.
    Details(String),
    /// B1: свернуть «Детали».
    HideDetails(String),
    /// В поддержку, с номером заявки или без.
    Support(Option<String>),
    /// Отмена шага или заявки.
    Cancel(Option<String>),
    /// «Пришлю чек» после котировки счёта.
    PayByCheck(String),
    /// «Вернуть деньги» при недоплате.
    Refund(String),
    /// «Сообщить, когда заработает» направление.
    Notify(Direction),
    /// «Простой вид» — переключить на обычный HTML (SPEC §6.1).
    PlainView,
    /// «Красивый вид» — вернуть карточку (rich) после «Простого вида».
    RichView,
    /// Инвестиции (DESIGN-v0.2 Р18).
    Invest(InvestAction),
}

/// Кнопки раздела «💼 Инвестиции».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvestAction {
    /// Экран с цифрами (и «Обновить»).
    Home,
    /// «Пополнить» — следующий чек от инвестора идёт в инвестиции.
    Deposit,
    /// «Вывести» — выбор кошелька.
    Withdraw,
    /// Кошелёк выбран — спросить сумму.
    WithdrawTo(Platform),
    /// «Всё доступное» в этот кошелёк.
    WithdrawAll(Platform),
    /// «Как это работает?»
    HowTo,
}

/// Лимит Bot API на `callback_data`.
pub const MAX_CALLBACK_BYTES: usize = 64;

impl Callback {
    pub fn encode(&self) -> String {
        match self {
            Callback::Direction(d) => format!("dir:{}", d.code()),
            Callback::HowTo(d) => format!("howto:{}", d.code()),
            Callback::Exchange => "exchange".to_owned(),
            Callback::PayInvoice => "invoice".to_owned(),
            Callback::Terms => "terms".to_owned(),
            Callback::Privacy => "privacy".to_owned(),
            Callback::History { page } => format!("history:{page}"),
            Callback::Order(id) => format!("order:{id}"),
            Callback::Details(id) => format!("det:{id}"),
            Callback::HideDetails(id) => format!("hide:{id}"),
            Callback::Support(None) => "support".to_owned(),
            Callback::Support(Some(id)) => format!("support:{id}"),
            Callback::Cancel(None) => "cancel".to_owned(),
            Callback::Cancel(Some(id)) => format!("cancel:{id}"),
            Callback::PayByCheck(id) => format!("pay_check:{id}"),
            Callback::Refund(id) => format!("refund:{id}"),
            Callback::Notify(d) => format!("notify:{}", d.code()),
            Callback::PlainView => "plain".to_owned(),
            Callback::RichView => "rich".to_owned(),
            Callback::Invest(a) => match a {
                InvestAction::Home => "inv".to_owned(),
                InvestAction::Deposit => "inv:dep".to_owned(),
                InvestAction::Withdraw => "inv:out".to_owned(),
                InvestAction::WithdrawTo(p) => format!("inv:to:{}", p.db_name()),
                InvestAction::WithdrawAll(p) => format!("inv:all:{}", p.db_name()),
                InvestAction::HowTo => "inv:how".to_owned(),
            },
        }
    }

    /// Разобрать `callback_data`. Всё незнакомое или с плохим номером заявки — `None`.
    pub fn parse(data: &str) -> Option<Self> {
        if data.is_empty() || data.len() > MAX_CALLBACK_BYTES {
            return None;
        }
        let (name, arg) = match data.split_once(':') {
            Some((name, arg)) => (name, Some(arg)),
            None => (data, None),
        };
        let order = |arg: Option<&str>| arg.filter(|a| order_id_ok(a)).map(str::to_owned);
        let direction = |arg: Option<&str>| arg.and_then(|a| Direction::from_str(a).ok());
        match (name, arg) {
            ("dir", _) => direction(arg).map(Callback::Direction),
            ("howto", _) => direction(arg).map(Callback::HowTo),
            ("notify", _) => direction(arg).map(Callback::Notify),
            ("exchange", None) => Some(Callback::Exchange),
            ("invoice", None) => Some(Callback::PayInvoice),
            ("terms", None) => Some(Callback::Terms),
            ("privacy", None) => Some(Callback::Privacy),
            ("plain", None) => Some(Callback::PlainView),
            ("rich", None) => Some(Callback::RichView),
            ("history", Some(page)) => page
                .parse()
                .ok()
                .filter(|_| page.bytes().all(|b| b.is_ascii_digit()))
                .map(|page| Callback::History { page }),
            ("order", _) => order(arg).map(Callback::Order),
            ("det", _) => order(arg).map(Callback::Details),
            ("hide", _) => order(arg).map(Callback::HideDetails),
            ("pay_check", _) => order(arg).map(Callback::PayByCheck),
            ("refund", _) => order(arg).map(Callback::Refund),
            ("support", None) => Some(Callback::Support(None)),
            ("support", Some(_)) => order(arg).map(|id| Callback::Support(Some(id))),
            ("cancel", None) => Some(Callback::Cancel(None)),
            ("cancel", Some(_)) => order(arg).map(|id| Callback::Cancel(Some(id))),
            ("inv", None) => Some(Callback::Invest(InvestAction::Home)),
            ("inv", Some(rest)) => {
                let action = match rest.split_once(':') {
                    None => match rest {
                        "dep" => InvestAction::Deposit,
                        "out" => InvestAction::Withdraw,
                        "how" => InvestAction::HowTo,
                        _ => return None,
                    },
                    Some(("to", p)) => InvestAction::WithdrawTo(Platform::from_db_name(p)?),
                    Some(("all", p)) => InvestAction::WithdrawAll(Platform::from_db_name(p)?),
                    Some(_) => return None,
                };
                Some(Callback::Invest(action))
            }
            _ => None,
        }
    }
}

/// Публичный номер заявки: 1–32 символа `[A-Za-z0-9_-]`.
pub fn order_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<Callback> {
        let mut v = vec![
            Callback::Exchange,
            Callback::PayInvoice,
            Callback::Terms,
            Callback::Privacy,
            Callback::PlainView,
            Callback::RichView,
            Callback::History { page: 0 },
            Callback::History {
                page: 4_000_000_000,
            },
            Callback::Order("A1B2C3D4E5".into()),
            Callback::Details("A1B2C3D4E5".into()),
            Callback::HideDetails("x".repeat(32)),
            Callback::Support(None),
            Callback::Support(Some("A1B2C3D4E5".into())),
            Callback::Cancel(None),
            Callback::Cancel(Some("A1B2C3D4E5".into())),
            Callback::PayByCheck("x".repeat(32)),
            Callback::Refund("A1B2C3D4E5".into()),
            Callback::Invest(InvestAction::Home),
            Callback::Invest(InvestAction::Deposit),
            Callback::Invest(InvestAction::Withdraw),
            Callback::Invest(InvestAction::HowTo),
        ];
        for p in Platform::ALL {
            v.push(Callback::Invest(InvestAction::WithdrawTo(p)));
            v.push(Callback::Invest(InvestAction::WithdrawAll(p)));
        }
        for d in Direction::ALL {
            v.push(Callback::Direction(d));
            v.push(Callback::HowTo(d));
            v.push(Callback::Notify(d));
        }
        v
    }

    #[test]
    fn round_trip_and_fits_64_bytes() {
        for cb in all() {
            let data = cb.encode();
            assert!(data.len() <= MAX_CALLBACK_BYTES, "{data}");
            assert_eq!(Callback::parse(&data), Some(cb), "{data}");
        }
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "dir:",
            "dir:btc_to_eth",
            "order:",
            "order:a b",
            "order:<script>",
            "history:-1",
            "history:+1",
            "history:1x",
            "terms:1",
            "unknown",
            "inv:",
            "inv:x",
            "inv:to:",
            "inv:to:btc",
            "inv:all:cryptobot:1",
            "inv:dep:1",
            &"order:".repeat(20),
        ] {
            assert_eq!(Callback::parse(bad), None, "{bad}");
        }
    }
}
