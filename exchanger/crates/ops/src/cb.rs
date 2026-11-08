//! Данные инлайн-кнопок пульта: короткие строки до 64 байт, разбор туда и обратно.
//! Опасные действия кнопкой не выполняются: кнопка открывает ввод или предпросмотр, а
//! подтверждение — одноразовый nonce на сервере ([`Cb::Confirm`]).

use domain::{Direction, Platform};
use engine::fees::PromoMode;
use storage::staff::Role;
use ui::{Emoji, EmojiStyle};

/// Поле направления, которое меняется вводом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirField {
    Fee,
    MinFee,
    Lever,
    Limits,
    Newcomer,
    Tiers,
    Fiat,
}

impl DirField {
    const ALL: [DirField; 7] = [
        DirField::Fee,
        DirField::MinFee,
        DirField::Lever,
        DirField::Limits,
        DirField::Newcomer,
        DirField::Tiers,
        DirField::Fiat,
    ];

    fn code(self) -> &'static str {
        match self {
            DirField::Fee => "f",
            DirField::MinFee => "m",
            DirField::Lever => "l",
            DirField::Limits => "L",
            DirField::Newcomer => "n",
            DirField::Tiers => "t",
            DirField::Fiat => "x",
        }
    }
}

/// Личное условие клиента, которое меняется вводом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TermField {
    Fee,
    MinFee,
    Min,
    Max,
}

impl TermField {
    const ALL: [TermField; 4] = [
        TermField::Fee,
        TermField::MinFee,
        TermField::Min,
        TermField::Max,
    ];

    fn code(self) -> &'static str {
        match self {
            TermField::Fee => "f",
            TermField::MinFee => "m",
            TermField::Min => "a",
            TermField::Max => "b",
        }
    }
}

/// Решение по заявке на разборе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionKind {
    RetryPayout,
    RetryInvoice,
    Refund,
    ClosePaid,
    Close,
    Intake,
}

impl DecisionKind {
    pub const ALL: [DecisionKind; 6] = [
        DecisionKind::RetryPayout,
        DecisionKind::RetryInvoice,
        DecisionKind::Refund,
        DecisionKind::ClosePaid,
        DecisionKind::Close,
        DecisionKind::Intake,
    ];

    fn code(self) -> &'static str {
        match self {
            DecisionKind::RetryPayout => "rp",
            DecisionKind::RetryInvoice => "ri",
            DecisionKind::Refund => "rf",
            DecisionKind::ClosePaid => "cp",
            DecisionKind::Close => "cl",
            DecisionKind::Intake => "in",
        }
    }
}

/// Флаг настроек.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    Newcomer,
    Maintenance,
    KillSwitch,
}

impl Flag {
    const ALL: [Flag; 3] = [Flag::Newcomer, Flag::Maintenance, Flag::KillSwitch];

    fn code(self) -> &'static str {
        match self {
            Flag::Newcomer => "n",
            Flag::Maintenance => "m",
            Flag::KillSwitch => "k",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Flag::Newcomer => storage::user_terms::NEWCOMER_LIMIT,
            Flag::Maintenance => "maintenance",
            Flag::KillSwitch => "kill_switch_payouts",
        }
    }

    /// Значение, если настройки нет.
    pub fn default_on(self) -> bool {
        matches!(self, Flag::Newcomer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cb {
    Menu,
    Dash,
    Dirs,
    Dir(Direction),
    DirEdit(Direction, DirField),
    DirMode(Direction),
    DirOpen(Direction),
    DirClearAuto(Direction),
    Promo,
    PromoMode(PromoMode),
    PromoRate,
    PromoThresholds,
    PromoTarget(Platform),
    PromoSwitch(Direction, bool),
    Queue,
    Order(i64),
    OrderAsk,
    Take(i64),
    Decide(i64, DecisionKind),
    UserAsk,
    User(i64),
    UserTerms(i64),
    UserDir(i64, Direction),
    UserEdit(i64, Direction, TermField),
    UserExempt(i64, Direction),
    UserReset(i64, Direction),
    UserDaily(i64),
    UserBan(i64),
    UserUnban(i64),
    UserNote(i64),
    Settings,
    Toggle(Flag),
    Emoji,
    EmojiStyle(EmojiStyle),
    EmojiRole(Emoji),
    Bots,
    BotPause(i16, bool),
    Staff,
    StaffAdd,
    Member(i64),
    MemberRole(i64, Role),
    MemberDisable(i64),
    Audit,
    Confirm(String),
    /// Подтверждение «проверил: прежней попытки не было» по заявке.
    ConfirmUnknown(String),
    Cancel,
    Ack(i64),
    Reconcile,
    Selftest,
    /// Провести расхождение из события сверки как взнос или вывод владельца.
    Capital(i64),
    /// Ответить клиенту на вопрос из «Поддержки» (макет: «Обращение · Ответить»).
    SupportReply(i64),
    /// «Статистика» за 1, 7 или 30 дней (макет: карточка с графиком).
    Stats(u8),
    /// «Обращения»: вопросы клиентов, ждущие ответа.
    Inbox,
    /// Закрыть обращение клиента без ответа.
    SupportClose(i64),
    /// «Система»: версия, база, очередь, связь.
    System,
    /// «Предпросмотр экранов» клиента и пульта настоящими сообщениями.
    Preview,
    /// «Рассылка».
    Broadcast,
    /// «Резервы и сверка».
    Reserves,
    /// Событие пульта по id («Недостача»).
    AlertShow(i64),
    /// «Нет, разбираться» по событию «не сходится».
    Investigate(i64),
    /// «Пороги» резерва.
    Thresholds,
    /// «Прибыль».
    Profit,
    /// Страница списка («Разбор», «Заявки», «Клиенты», «Обращения», «Журнал», «Инвесторы»).
    Page(crate::nav::List, u32),
    /// Заявки клиента, страница.
    UserOrders(i64, u32),
    /// Обращение клиента — карточка с «Ответить» и «Закрыть».
    Question(i64),
    /// Все инвесторы и движения, ждущие решения.
    Investors,
    /// «💼 Инвестиции» клиента.
    Invest(i64),
    /// Выдать доступ или поменять условия.
    InvestEdit(i64),
    /// Включить или отключить доступ.
    InvestAccess(i64, bool),
    /// Карточка движения.
    InvestMove(i64),
    InvestApprove(i64),
    InvestReject(i64),
    /// Неясное движение: было (`true`) или не было.
    InvestResolve(i64, bool),
}

fn dir_code(d: Direction) -> &'static str {
    match d {
        Direction::CbToXrCheck => "cx",
        Direction::XrToCbCheck => "xc",
        Direction::PayCbInvoice => "pc",
        Direction::PayXrInvoice => "px",
    }
}

fn dir_of(s: &str) -> Option<Direction> {
    Direction::ALL.into_iter().find(|d| dir_code(*d) == s)
}

fn platform_of(s: &str) -> Option<Platform> {
    match s {
        "cb" => Some(Platform::CryptoBot),
        "xr" => Some(Platform::XRocket),
        _ => None,
    }
}

fn platform_code(p: Platform) -> &'static str {
    match p {
        Platform::CryptoBot => "cb",
        Platform::XRocket => "xr",
    }
}

fn mode_code(m: PromoMode) -> &'static str {
    match m {
        PromoMode::Off => "0",
        PromoMode::Hint => "h",
        PromoMode::Auto => "a",
    }
}

fn bit(b: bool) -> &'static str {
    if b { "1" } else { "0" }
}

impl Cb {
    /// Данные кнопки: префикс `a:` отделяет кнопки пульта от клиентских в общем боте.
    pub fn encode(&self) -> String {
        format!("a:{}", self.encode_body())
    }

    pub fn parse(s: &str) -> Option<Cb> {
        Self::parse_body(s.strip_prefix("a:")?)
    }

    fn encode_body(&self) -> String {
        match self {
            Cb::Menu => "m".into(),
            Cb::Dash => "dash".into(),
            Cb::Dirs => "dirs".into(),
            Cb::Dir(d) => format!("d:{}", dir_code(*d)),
            Cb::DirEdit(d, f) => format!("de:{}:{}", dir_code(*d), f.code()),
            Cb::DirMode(d) => format!("dm:{}", dir_code(*d)),
            Cb::DirOpen(d) => format!("do:{}", dir_code(*d)),
            Cb::DirClearAuto(d) => format!("da:{}", dir_code(*d)),
            Cb::Promo => "pr".into(),
            Cb::PromoMode(m) => format!("pm:{}", mode_code(*m)),
            Cb::PromoRate => "prr".into(),
            Cb::PromoThresholds => "prt".into(),
            Cb::PromoTarget(p) => format!("pt:{}", platform_code(*p)),
            Cb::PromoSwitch(d, on) => format!("ps:{}:{}", dir_code(*d), bit(*on)),
            Cb::Queue => "q".into(),
            Cb::Order(id) => format!("o:{id}"),
            Cb::OrderAsk => "oa".into(),
            Cb::Take(id) => format!("ot:{id}"),
            Cb::Decide(id, k) => format!("od:{id}:{}", k.code()),
            Cb::UserAsk => "ua".into(),
            Cb::User(id) => format!("u:{id}"),
            Cb::UserTerms(id) => format!("ut:{id}"),
            Cb::UserDir(id, d) => format!("uD:{id}:{}", dir_code(*d)),
            Cb::UserEdit(id, d, f) => format!("ue:{id}:{}:{}", dir_code(*d), f.code()),
            Cb::UserExempt(id, d) => format!("ux:{id}:{}", dir_code(*d)),
            Cb::UserReset(id, d) => format!("ur:{id}:{}", dir_code(*d)),
            Cb::UserDaily(id) => format!("ul:{id}"),
            Cb::UserBan(id) => format!("ub:{id}"),
            Cb::UserUnban(id) => format!("uu:{id}"),
            Cb::UserNote(id) => format!("un:{id}"),
            Cb::Settings => "s".into(),
            Cb::Toggle(f) => format!("st:{}", f.code()),
            Cb::Emoji => "se".into(),
            Cb::EmojiStyle(s) => format!("ss:{}", s.key()),
            Cb::EmojiRole(r) => format!("sr:{}", r.key()),
            Cb::Bots => "b".into(),
            Cb::BotPause(id, p) => format!("bp:{id}:{}", bit(*p)),
            Cb::Staff => "sf".into(),
            Cb::StaffAdd => "sfa".into(),
            Cb::Member(id) => format!("sm:{id}"),
            Cb::MemberRole(id, r) => format!("smr:{id}:{}", r.as_db_str()),
            Cb::MemberDisable(id) => format!("smd:{id}"),
            Cb::Audit => "au".into(),
            Cb::Confirm(n) => format!("ok:{n}"),
            Cb::ConfirmUnknown(n) => format!("ou:{n}"),
            Cb::Cancel => "no".into(),
            Cb::Ack(id) => format!("ack:{id}"),
            Cb::Reconcile => "rc".into(),
            Cb::Selftest => "stt".into(),
            Cb::Capital(id) => format!("cap:{id}"),
            Cb::Investors => "iv".into(),
            Cb::SupportReply(id) => format!("sq:{id}"),
            Cb::Stats(days) => format!("sx:{days}"),
            Cb::Inbox => "ib".into(),
            Cb::SupportClose(id) => format!("sc:{id}"),
            Cb::System => "sy".into(),
            Cb::Preview => "pv".into(),
            Cb::Broadcast => "bc".into(),
            Cb::Reserves => "rs".into(),
            Cb::AlertShow(id) => format!("al:{id}"),
            Cb::Investigate(id) => format!("iq:{id}"),
            Cb::Thresholds => "th".into(),
            Cb::Profit => "pf".into(),
            Cb::Page(list, page) => format!("pg:{}:{page}", list.code()),
            Cb::UserOrders(id, page) => format!("uo:{id}:{page}"),
            Cb::Question(id) => format!("qv:{id}"),
            Cb::Invest(id) => format!("iv:{id}"),
            Cb::InvestEdit(id) => format!("ivt:{id}"),
            Cb::InvestAccess(id, on) => format!("iva:{id}:{}", bit(*on)),
            Cb::InvestMove(id) => format!("im:{id}"),
            Cb::InvestApprove(id) => format!("ima:{id}"),
            Cb::InvestReject(id) => format!("imr:{id}"),
            Cb::InvestResolve(id, yes) => format!("imx:{id}:{}", bit(*yes)),
        }
    }

    fn parse_body(s: &str) -> Option<Cb> {
        let parts: Vec<&str> = s.split(':').collect();
        let id = |i: usize| parts.get(i).and_then(|p| p.parse::<i64>().ok());
        let dir = |i: usize| parts.get(i).and_then(|p| dir_of(p));
        let on = |i: usize| match parts.get(i) {
            Some(&"1") => Some(true),
            Some(&"0") => Some(false),
            _ => None,
        };
        Some(match parts.as_slice() {
            ["m"] => Cb::Menu,
            ["dash"] => Cb::Dash,
            ["dirs"] => Cb::Dirs,
            ["d", _] => Cb::Dir(dir(1)?),
            ["de", _, f] => {
                Cb::DirEdit(dir(1)?, DirField::ALL.into_iter().find(|x| x.code() == *f)?)
            }
            ["dm", _] => Cb::DirMode(dir(1)?),
            ["do", _] => Cb::DirOpen(dir(1)?),
            ["da", _] => Cb::DirClearAuto(dir(1)?),
            ["pr"] => Cb::Promo,
            ["pm", m] => Cb::PromoMode(
                [PromoMode::Off, PromoMode::Hint, PromoMode::Auto]
                    .into_iter()
                    .find(|x| mode_code(*x) == *m)?,
            ),
            ["prr"] => Cb::PromoRate,
            ["prt"] => Cb::PromoThresholds,
            ["pt", p] => Cb::PromoTarget(platform_of(p)?),
            ["ps", _, _] => Cb::PromoSwitch(dir(1)?, on(2)?),
            ["q"] => Cb::Queue,
            ["o", _] => Cb::Order(id(1)?),
            ["oa"] => Cb::OrderAsk,
            ["ot", _] => Cb::Take(id(1)?),
            ["od", _, k] => Cb::Decide(
                id(1)?,
                DecisionKind::ALL.into_iter().find(|x| x.code() == *k)?,
            ),
            ["ua"] => Cb::UserAsk,
            ["u", _] => Cb::User(id(1)?),
            ["ut", _] => Cb::UserTerms(id(1)?),
            ["uD", _, _] => Cb::UserDir(id(1)?, dir(2)?),
            ["ue", _, _, f] => Cb::UserEdit(
                id(1)?,
                dir(2)?,
                TermField::ALL.into_iter().find(|x| x.code() == *f)?,
            ),
            ["ux", _, _] => Cb::UserExempt(id(1)?, dir(2)?),
            ["ur", _, _] => Cb::UserReset(id(1)?, dir(2)?),
            ["ul", _] => Cb::UserDaily(id(1)?),
            ["ub", _] => Cb::UserBan(id(1)?),
            ["uu", _] => Cb::UserUnban(id(1)?),
            ["un", _] => Cb::UserNote(id(1)?),
            ["s"] => Cb::Settings,
            ["st", f] => Cb::Toggle(Flag::ALL.into_iter().find(|x| x.code() == *f)?),
            ["se"] => Cb::Emoji,
            ["ss", k] => Cb::EmojiStyle(EmojiStyle::from_key(k)?),
            ["sr", k] => Cb::EmojiRole(Emoji::from_key(k)?),
            ["b"] => Cb::Bots,
            ["bp", _, _] => Cb::BotPause(i16::try_from(id(1)?).ok()?, on(2)?),
            ["sf"] => Cb::Staff,
            ["sfa"] => Cb::StaffAdd,
            ["sm", _] => Cb::Member(id(1)?),
            ["smr", _, r] => Cb::MemberRole(id(1)?, Role::from_db_str(r)?),
            ["smd", _] => Cb::MemberDisable(id(1)?),
            ["au"] => Cb::Audit,
            ["ok", n] => Cb::Confirm((*n).to_owned()),
            ["ou", n] => Cb::ConfirmUnknown((*n).to_owned()),
            ["no"] => Cb::Cancel,
            ["ack", _] => Cb::Ack(id(1)?),
            ["rc"] => Cb::Reconcile,
            ["stt"] => Cb::Selftest,
            ["cap", _] => Cb::Capital(id(1)?),
            ["iv"] => Cb::Investors,
            ["sq", _] => Cb::SupportReply(id(1)?),
            ["sx", d] => Cb::Stats(d.parse().ok().filter(|d| matches!(d, 1 | 7 | 30))?),
            ["ib"] => Cb::Inbox,
            ["sc", _] => Cb::SupportClose(id(1)?),
            ["sy"] => Cb::System,
            ["pv"] => Cb::Preview,
            ["bc"] => Cb::Broadcast,
            ["rs"] => Cb::Reserves,
            ["al", _] => Cb::AlertShow(id(1)?),
            ["iq", _] => Cb::Investigate(id(1)?),
            ["th"] => Cb::Thresholds,
            ["pf"] => Cb::Profit,
            ["pg", l, p] => Cb::Page(crate::nav::List::of(l)?, p.parse().ok()?),
            ["uo", _, p] => Cb::UserOrders(id(1)?, p.parse().ok()?),
            ["qv", _] => Cb::Question(id(1)?),
            ["iv", _] => Cb::Invest(id(1)?),
            ["ivt", _] => Cb::InvestEdit(id(1)?),
            ["iva", _, _] => Cb::InvestAccess(id(1)?, on(2)?),
            ["im", _] => Cb::InvestMove(id(1)?),
            ["ima", _] => Cb::InvestApprove(id(1)?),
            ["imr", _] => Cb::InvestReject(id(1)?),
            ["imx", _, _] => Cb::InvestResolve(id(1)?, on(2)?),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_callback_round_trips_and_fits_telegram() {
        let big = i64::MAX;
        let mut all = vec![
            Cb::Menu,
            Cb::Dash,
            Cb::Dirs,
            Cb::Promo,
            Cb::PromoRate,
            Cb::PromoThresholds,
            Cb::Queue,
            Cb::OrderAsk,
            Cb::UserAsk,
            Cb::Settings,
            Cb::Emoji,
            Cb::Bots,
            Cb::Staff,
            Cb::StaffAdd,
            Cb::Audit,
            Cb::Cancel,
            Cb::Reconcile,
            Cb::Selftest,
            Cb::Capital(big),
            Cb::Confirm("abcdefgh12".into()),
            Cb::ConfirmUnknown("abcdefgh12".into()),
            Cb::Order(big),
            Cb::Take(big),
            Cb::User(big),
            Cb::UserTerms(big),
            Cb::UserDaily(big),
            Cb::UserBan(big),
            Cb::UserUnban(big),
            Cb::UserNote(big),
            Cb::Member(big),
            Cb::MemberDisable(big),
            Cb::Ack(big),
            Cb::BotPause(i16::MAX, true),
            Cb::PromoTarget(Platform::CryptoBot),
            Cb::PromoTarget(Platform::XRocket),
            Cb::Investors,
            Cb::SupportReply(big),
            Cb::Stats(7),
            Cb::Inbox,
            Cb::SupportClose(big),
            Cb::System,
            Cb::Preview,
            Cb::Broadcast,
            Cb::Invest(big),
            Cb::InvestEdit(big),
            Cb::InvestAccess(big, true),
            Cb::InvestAccess(big, false),
            Cb::InvestMove(big),
            Cb::InvestApprove(big),
            Cb::InvestReject(big),
            Cb::InvestResolve(big, true),
            Cb::InvestResolve(big, false),
        ];
        for d in Direction::ALL {
            all.extend([
                Cb::Dir(d),
                Cb::DirMode(d),
                Cb::DirOpen(d),
                Cb::DirClearAuto(d),
                Cb::PromoSwitch(d, true),
                Cb::PromoSwitch(d, false),
                Cb::UserDir(big, d),
                Cb::UserExempt(big, d),
                Cb::UserReset(big, d),
            ]);
            all.extend(DirField::ALL.map(|f| Cb::DirEdit(d, f)));
            all.extend(TermField::ALL.map(|f| Cb::UserEdit(big, d, f)));
        }
        all.extend(DecisionKind::ALL.map(|k| Cb::Decide(big, k)));
        all.extend(Flag::ALL.map(Cb::Toggle));
        all.extend(EmojiStyle::ALL.map(Cb::EmojiStyle));
        all.extend(Emoji::ALL.map(Cb::EmojiRole));
        all.extend(Role::ALL.map(|r| Cb::MemberRole(big, r)));
        all.extend([PromoMode::Off, PromoMode::Hint, PromoMode::Auto].map(Cb::PromoMode));
        for cb in all {
            let s = cb.encode();
            assert!(s.len() <= 64, "{s}");
            assert_eq!(Cb::parse(&s), Some(cb), "{s}");
        }
        assert_eq!(Cb::parse("a:d:zz"), None);
        assert_eq!(Cb::parse("a:o:x"), None);
        // Клиентские кнопки — не пульту.
        assert_eq!(Cb::parse("dir:cb_to_xr_check"), None);
        assert_eq!(Cb::parse("m"), None);
    }
}
