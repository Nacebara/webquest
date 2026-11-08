//! Справочные экраны: приветствие, курс и лимиты, выбор направления, инструкции, история,
//! карточка заявки, помощь, конфиденциальность, поддержка.

use botapi::ButtonStyle;
use chrono::{DateTime, Utc};
use domain::fsm::OrderState;
use domain::{Decimal, Direction, Flow};

use super::{Ui, inline_command, menu_command, username_ok, wallet_bot_url};
use crate::callback::{Callback, order_id_ok};
use crate::doc::{Doc, Row, Table};
use crate::emoji::Emoji;
use crate::fmt::{self, TimeFormat};
use crate::html::{Frag, clip};
use crate::model::{
    DirectionStatus, DirectionTerms, HistoryRow, HistorySummary, OrderCard, PauseReason, Terms,
    UiError,
};
use crate::screen::{Keyboard, Screen};

/// Сколько строк истории показываем за раз (SPEC S10).
pub const HISTORY_PAGE: usize = 10;

/// Пример суммы для инлайн-команды: 10 USDT, но в пределах лимитов направления.
/// Сумма в примере команды кошелька — как в макете («@send 100usdt»), в пределах направления.
fn example_amount(terms: Option<&DirectionTerms>) -> Decimal {
    let base = Decimal::ONE_HUNDRED;
    match terms {
        Some(t) if t.max >= t.min => base.max(t.min.ceil()).min(t.max.floor().max(t.min.ceil())),
        _ => base,
    }
}

/// Точка статуса строки истории: готово — зелёная, в работе — жёлтая, остальное — серая.
fn history_dot(state: OrderState) -> Emoji {
    match state {
        OrderState::Completed => Emoji::DotOk,
        s if !s.is_terminal() => Emoji::DotWait,
        _ => Emoji::DotClosed,
    }
}

/// Форма слова из текстов для [`fmt::plural`]: тексты живут всё время работы бота.
fn static_form(form: &str) -> &'static str {
    match form {
        "обмен" => "обмен",
        "обмена" => "обмена",
        _ => "обменов",
    }
}

fn direction_icon(direction: Direction) -> Emoji {
    match direction {
        Direction::CbToXrCheck => Emoji::Cb,
        Direction::XrToCbCheck => Emoji::Xr,
        Direction::PayCbInvoice | Direction::PayXrInvoice => Emoji::Invoice,
    }
}

impl Ui {
    /// Почему на паузе — фраза для «Что меняем?» и ошибок паузы.
    fn pause_why(&self, t: &DirectionTerms) -> Frag {
        let s = &self.texts.direction_status;
        match t.pause {
            Some(PauseReason::NoReserve { wallet }) => {
                self.fill(&s.why_no_reserve, &[("wallet", &self.wallet(wallet))])
            }
            Some(PauseReason::Maintenance) => self.fill(&s.why_maintenance, &[]),
            Some(PauseReason::Technical) => self.fill(&s.why_technical, &[]),
            None => self.fill(&s.why_paused, &[]),
        }
    }

    /// Комиссия: одна ставка или «от …» по самой дешёвой ступени.
    fn fee_frag(&self, t: &DirectionTerms) -> Frag {
        if t.tiers.len() > 1 {
            let cheapest = t.tiers.iter().map(|x| x.fee_pct).min().unwrap_or(t.fee_pct);
            self.fill(&self.texts.terms.fee_from, &[("fee", &self.pct(cheapest))])
        } else {
            self.pct(t.fee_pct)
        }
    }

    /// Ступени одной строкой: «до 100 USDT — 2,5 % · от 100 USDT — 1,8 %».
    fn tiers_frag(&self, t: &DirectionTerms) -> Frag {
        let x = &self.texts.terms;
        let mut out = Frag::default();
        for (i, tier) in t.tiers.iter().enumerate() {
            if i > 0 {
                out.push(&self.fill(&x.tier_sep, &[]));
            }
            let fee = self.pct(tier.fee_pct);
            let part = if i == 0 {
                let to = t.tiers.get(1).map_or(t.max, |n| n.from);
                self.fill(&x.tier_first, &[("to", &self.usdt(to)), ("fee", &fee)])
            } else {
                self.fill(
                    &x.tier_next,
                    &[("from", &self.usdt(tier.from)), ("fee", &fee)],
                )
            };
            out.push(&part);
        }
        out
    }

    /// «🔥 Сейчас выгодно…» для открытых направлений с выгодным курсом.
    fn promo_paras(&self, terms: &Terms, doc: &mut Doc) {
        for d in terms.open() {
            if let Some(p) = d.promo {
                doc.para(&self.fill(
                    &self.texts.terms.promo,
                    &[
                        ("icon", &self.emoji.frag(Emoji::Promo)),
                        ("direction", &self.direction_name(d.direction)),
                        ("fee", &self.pct(p.fee_pct)),
                        ("up_to", &self.usdt(p.up_to)),
                    ],
                ));
            }
        }
    }

    fn bind_username(&self, name: &str) -> Option<Frag> {
        let name = name.trim_start_matches('@');
        username_ok(name).then(|| Frag::code(&format!("@{name}")))
    }

    /// S1 `/start`: что делает бот, сколько стоит, что прислать. Rich + фолбэк; к экрану
    /// прикреплено главное меню (reply-клавиатура).
    pub fn welcome(&self, terms: &Terms) -> Screen {
        let t = &self.texts.welcome;
        let mut doc = Doc::new();
        doc.title(&self.fill(&t.title, &[]));
        doc.para(&self.fill(&t.lead, &[]));

        // Строки таблицы: «Куда · Комиссия · USDT». Два обмена чеков и одна строка на оба
        // счёта (как в макете): комиссия — самая низкая из открытых, «от …», если разная.
        let mut lines: Vec<(Frag, Frag, Frag)> = Vec::new();
        for direction in [Direction::CbToXrCheck, Direction::XrToCbCheck] {
            if let Some(d) = terms.get(direction) {
                lines.push((
                    self.direction_name(direction),
                    self.welcome_fee(&[d]),
                    self.welcome_range(&[d]),
                ));
            }
        }
        let invoices: Vec<&DirectionTerms> = [Direction::PayCbInvoice, Direction::PayXrInvoice]
            .into_iter()
            .filter_map(|d| terms.get(d))
            .collect();
        if !invoices.is_empty() {
            lines.push((
                self.fill(&t.invoice_row, &[]),
                self.welcome_fee(&invoices),
                self.welcome_range(&invoices),
            ));
        }
        let rows = lines
            .iter()
            .map(|(to, fee, range)| Row {
                cells: vec![to.clone(), fee.clone(), range.clone()],
                line: self.fill(&t.row, &[("direction", to), ("fee", fee), ("range", range)]),
            })
            .collect();
        doc.table(Table {
            attrs: "striped compact",
            head: vec![
                self.fill(&t.col_to, &[]),
                self.fill(&t.col_fee, &[]),
                self.fill(&t.col_usdt, &[]),
            ],
            rows,
            right: &[1, 2],
            plain_quote: true,
        });
        if terms.open().next().is_none() {
            doc.para(&self.fill(&t.closed, &[]));
        }
        doc.footer(&self.fill(&t.footer, &[]));
        self.dual(doc, Keyboard::Reply(self.main_menu()))
    }

    /// Комиссия строки приветствия: самая низкая из открытых направлений строки («от …»,
    /// если у них разная); все на паузе — «—».
    fn welcome_fee(&self, dirs: &[&DirectionTerms]) -> Frag {
        let open: Vec<&&DirectionTerms> = dirs
            .iter()
            .filter(|d| d.status == DirectionStatus::Open)
            .collect();
        let Some(cheapest) = open.iter().map(|d| d.fee_pct).min() else {
            return Frag::text("—");
        };
        let single = open.len() == 1 && open[0].tiers.len() <= 1;
        let same = open
            .iter()
            .all(|d| d.fee_pct == cheapest && d.tiers.len() <= 1);
        if single || same {
            self.pct(cheapest)
        } else {
            self.fill(&self.texts.terms.fee_from, &[("fee", &self.pct(cheapest))])
        }
    }

    /// Пределы строки приветствия: «1–500»; все на паузе — «пауза».
    fn welcome_range(&self, dirs: &[&DirectionTerms]) -> Frag {
        let open: Vec<&&DirectionTerms> = dirs
            .iter()
            .filter(|d| d.status == DirectionStatus::Open)
            .collect();
        match (
            open.iter().map(|d| d.min).min(),
            open.iter().map(|d| d.max).max(),
        ) {
            (Some(min), Some(max)) => Frag::text(&format!(
                "{}–{}",
                fmt::bare_amount(min),
                fmt::bare_amount(max)
            )),
            _ => self.fill(&self.texts.welcome.paused, &[]),
        }
    }

    /// S2 «Курс и лимиты» (`/terms`): таблица направлений, какие чеки и счета принимаю, правила.
    pub fn terms(&self, terms: &Terms) -> Screen {
        let t = &self.texts.terms;
        let mut doc = Doc::new();
        doc.title(&self.fill(&t.title, &[]));
        let rows = terms
            .directions
            .iter()
            .map(|d| {
                let dot = self.emoji.frag(match d.status {
                    DirectionStatus::Open => Emoji::DotOk,
                    DirectionStatus::Paused => Emoji::DotWait,
                    DirectionStatus::Off => Emoji::DotClosed,
                });
                let name = match d.direction.flow() {
                    Flow::CheckExchange => self.direction_name(d.direction),
                    Flow::InvoicePayment => Frag::text(&self.texts.offers.get(d.direction).send),
                };
                let (fee, range) = match d.status {
                    DirectionStatus::Open => (
                        self.fee_frag(d),
                        Frag::text(&format!(
                            "{}–{}",
                            fmt::bare_amount(d.min),
                            fmt::bare_amount(d.max)
                        )),
                    ),
                    DirectionStatus::Paused => (Frag::text("—"), self.fill(&t.paused, &[])),
                    DirectionStatus::Off => (Frag::text("—"), self.fill(&t.off, &[])),
                };
                Row {
                    line: self.fill(
                        &t.row,
                        &[
                            ("dot", &dot),
                            ("direction", &name),
                            ("fee", &fee),
                            ("range", &range),
                        ],
                    ),
                    cells: vec![dot, name, fee, range],
                }
            })
            .collect();
        doc.table(Table {
            attrs: "striped compact",
            head: vec![
                Frag::new(),
                self.fill(&t.col_to, &[]),
                self.fill(&t.col_fee, &[]),
                self.fill(&t.col_usdt, &[]),
            ],
            rows,
            right: &[2, 3],
            plain_quote: true,
        });
        // «Комиссия не меньше 0.10 USDT. Первые обмены — до 100 USDT.»
        let open: Vec<&DirectionTerms> = terms.open().collect();
        if let Some(min_fee) = open.iter().map(|d| d.min_fee).min() {
            let mut line = self.fill(&t.min_fee, &[("min_fee", &self.usdt(min_fee))]);
            if let Some(max) = open.iter().filter_map(|d| d.new_user_max).max() {
                line.push(&Frag::trusted(" "));
                line.push(&self.fill(&t.new_user, &[("max", &Frag::code(&fmt::bare_amount(max)))]));
            }
            doc.para(&line);
        }
        // Сверх макета — только когда есть: выгодный курс, личные условия, ступени.
        self.promo_paras(terms, &mut doc);
        if terms.directions.iter().any(|d| d.personal) {
            doc.para(&self.fill(&t.personal, &[]));
        }
        let tiered: Vec<&DirectionTerms> = terms
            .directions
            .iter()
            .filter(|d| d.tiers.len() > 1)
            .collect();
        if !tiered.is_empty() {
            let lines: Vec<Frag> = tiered
                .iter()
                .map(|d| {
                    self.fill(
                        &t.tier_line,
                        &[
                            ("direction", &self.direction_name(d.direction)),
                            ("tiers", &self.tiers_frag(d)),
                        ],
                    )
                })
                .collect();
            doc.details(&self.fill(&t.tiers_title, &[]), true, |d| d.list(&lines));
        }
        if let Some(at) = terms.updated_at {
            doc.footer(&self.fill(&t.updated, &[("time", &self.at(at, TimeFormat::Time))]));
        }
        let b = &self.texts.buttons;
        let keyboard = Keyboard::inline_rows(vec![vec![
            self.cb_button(
                Some(Emoji::Exchange),
                &self.texts.menu.exchange,
                &Callback::Exchange,
            )
            .style(ButtonStyle::Primary),
            self.cb_button(Some(Emoji::Details), &b.plain_view, &Callback::PlainView),
        ]]);
        self.dual(doc, keyboard)
    }

    /// «Обменять» — макет B1: «Что меняем?» и две кнопки, комиссия прямо на кнопке.
    /// Основное направление CryptoBot → xRocket — синее; на паузе — неактивная «· пауза».
    pub fn exchange_menu(&self, terms: &Terms) -> Screen {
        let t = &self.texts.exchange;
        let mut rows = Vec::new();
        let mut body = self.fill(&t.title, &[]);
        for direction in [Direction::CbToXrCheck, Direction::XrToCbCheck] {
            let Some(d) = terms.get(direction) else {
                continue;
            };
            let name = self.texts.directions.get(direction);
            // Значок кошелька, куда придут деньги (как в макете: ракета у «→ xRocket»).
            let role = Some(match direction.target() {
                domain::Platform::XRocket => Emoji::Xr,
                domain::Platform::CryptoBot => Emoji::Cb,
            });
            let button = if d.status == DirectionStatus::Open {
                let fee = match d.promo {
                    Some(p) => fmt::percent(p.fee_pct.min(d.fee_pct)),
                    None => self.fee_frag(d).plain().to_owned(),
                };
                let label = self.button_text(&t.button, &[("direction", name), ("fee", &fee)]);
                let button = self.cb_button(role, &label, &Callback::Direction(direction));
                if direction == Direction::CbToXrCheck {
                    button.style(ButtonStyle::Primary)
                } else {
                    button
                }
            } else {
                body.push(&Frag::trusted("\n"));
                body.push(&self.fill(
                    &t.paused,
                    &[
                        ("direction", &self.direction_name(direction)),
                        ("why", &self.pause_why(d)),
                    ],
                ));
                let label = self.button_text(&t.button_paused, &[("direction", name)]);
                botapi::InlineKeyboardButton::disabled(label)
            };
            rows.push(vec![button]);
        }
        self.plain(body, Keyboard::inline_rows(rows))
    }

    /// После выбора направления: «пришлите чек … на сумму от X до Y» (DESIGN-v0.2 §6).
    /// Направление на паузе — сразу ошибка E11 (чек ещё не прислан, деньги у клиента).
    pub fn send_check(&self, terms: &Terms, direction: Direction) -> Screen {
        let Some(d) = terms.get(direction).filter(|d| {
            d.status == DirectionStatus::Open && direction.flow() == Flow::CheckExchange
        }) else {
            return self.error(&UiError::DirectionPaused {
                direction,
                until: None,
            });
        };
        let t = &self.texts.send_check;
        let src = direction.source();
        let command = inline_command(src, example_amount(Some(d)));
        let mut body = self.fill(
            &t.title,
            &[
                ("src", &self.wallet(src)),
                ("min", &Frag::text(&fmt::bare_amount(d.min))),
                ("max", &Frag::text(&fmt::bare_amount(d.max))),
            ],
        );
        body.push(&Frag::trusted("\n"));
        body.push(&self.fill(&t.hint, &[("cmd", &Frag::code(&command))]));
        let b = &self.texts.buttons;
        let keyboard = Keyboard::inline_rows(vec![
            vec![self.copy_button(&command, &command)],
            vec![
                self.cb_button(
                    Some(Emoji::Help),
                    &b.instruction,
                    &Callback::HowTo(direction),
                ),
                self.cb_button(Some(Emoji::Hide), &b.cancel, &Callback::Cancel(None)),
            ],
        ]);
        self.plain(body, keyboard)
    }

    /// «Как создать чек»: инлайн `@send 10usdt` / `@xrocket 10` и запасной путь через меню
    /// кошелька (DESIGN-v0.2 Р5).
    pub fn check_howto(&self, terms: &Terms, direction: Direction) -> Screen {
        let t = &self.texts.howto;
        let src = direction.source();
        let amount = example_amount(terms.get(direction));
        let command = inline_command(src, amount);
        let cmd = Frag::code(&command);
        let amount_frag = Frag::code(&fmt::bare_amount(amount));
        let wallet = self.wallet(src);
        let per = t.platform(src);

        let mut body = self.fill(&t.title, &[("wallet", &wallet)]);
        body.push(&Frag::trusted("\n\n"));
        body.push(&self.fill(&t.quick_title, &[]));
        for (i, step) in per.steps.iter().enumerate() {
            body.push(&Frag::trusted(&format!("\n{}. ", i + 1)));
            body.push(&self.fill(step, &[("cmd", &cmd), ("amount", &amount_frag)]));
        }
        body.push(&Frag::trusted("\n\n"));
        body.push(&self.fill(&t.menu_title, &[("wallet", &wallet)]));
        body.push(&Frag::trusted("\n"));
        body.push(&self.fill(&per.menu, &[("cmd", &Frag::code(menu_command(src)))]));
        body.push(&Frag::trusted("\n\n"));
        body.push(&self.fill(&t.rules, &[]));

        let b = &self.texts.buttons;
        let open = self.button_text(&b.open_wallet, &[("wallet", self.wallet_name(src))]);
        let keyboard = Keyboard::inline_rows(vec![
            vec![self.copy_button(&command, &command)],
            vec![self.url_button(None, &open, wallet_bot_url(src))],
            vec![self.cb_button(None, &b.back, &Callback::Direction(direction))],
        ]);
        self.plain(body, keyboard)
    }

    /// «Оплатить счёт»: что прислать и сколько стоит.
    pub fn invoice_prompt(&self, terms: &Terms) -> Screen {
        let t = &self.texts.invoice_prompt;
        let mut body = self.fill(&t.title, &[]);
        body.push(&Frag::trusted("\n"));
        body.push(&self.fill(&t.body, &[]));
        let lines: Vec<Frag> = [Direction::PayCbInvoice, Direction::PayXrInvoice]
            .into_iter()
            .filter_map(|direction| terms.get(direction))
            .map(|d| {
                let offer = self.texts.offers.get(d.direction);
                let icon = self.emoji.frag(direction_icon(d.direction));
                let send = Frag::text(&offer.send);
                let get = Frag::text(&offer.get);
                if d.status == DirectionStatus::Open {
                    self.fill(
                        &t.line,
                        &[
                            ("icon", &icon),
                            ("send", &send),
                            ("get", &get),
                            ("fee", &self.pct(d.fee_pct)),
                        ],
                    )
                } else {
                    self.fill(&t.line_paused, &[("icon", &icon), ("send", &send)])
                }
            })
            .collect();
        if !lines.is_empty() {
            body.push(&Frag::trusted("\n\n<blockquote>"));
            body.push(&Frag::join(&lines, "\n"));
            body.push(&Frag::trusted("</blockquote>"));
        }
        body.push(&Frag::trusted("\n"));
        body.push(&self.fill(
            &t.examples,
            &[
                ("cb", &Frag::code("https://t.me/send?start=IV…")),
                ("xr", &Frag::code("https://t.me/xrocket?start=inv_…")),
            ],
        ));
        body.push(&Frag::trusted("\n"));
        body.push(&self.fill(&t.rules, &[]));
        self.plain(body, Keyboard::None)
    }

    /// S10 `/history`: до 10 заявок, кнопки с номерами по две в ряд, «Ещё».
    /// `next_page` — номер следующей страницы, если она есть.
    pub fn history(
        &self,
        rows: &[HistoryRow],
        next_page: Option<u32>,
        summary: Option<&HistorySummary>,
        now: DateTime<Utc>,
    ) -> Screen {
        let t = &self.texts.history;
        let b = &self.texts.buttons;
        let more =
            next_page.map(|page| vec![self.cb_button(None, &b.more, &Callback::History { page })]);
        if rows.is_empty() {
            let keyboard = Keyboard::inline_rows(more.into_iter().collect());
            return self.plain(self.fill(&t.empty, &[]), keyboard);
        }
        let rows = &rows[..rows.len().min(HISTORY_PAGE)];
        let mut doc = Doc::new();
        doc.title(&self.fill(&t.title, &[]));
        let labels: Vec<String> = rows
            .iter()
            .map(|r| {
                self.config
                    .clock
                    .day_label(r.created_at, now, &t.yesterday, &t.months)
            })
            .collect();
        let table_rows = rows
            .iter()
            .zip(&labels)
            .map(|(r, when)| {
                let dot = self.emoji.frag(history_dot(r.state));
                let when = Frag::text(when);
                let what = Frag::text(match r.direction {
                    Direction::CbToXrCheck => &t.dir_cb_to_xr,
                    Direction::XrToCbCheck => &t.dir_xr_to_cb,
                    Direction::PayCbInvoice => &t.dir_pay_cb,
                    Direction::PayXrInvoice => &t.dir_pay_xr,
                });
                let result = Frag::text(&self.history_result(r));
                Row {
                    line: self.fill(
                        &t.row,
                        &[
                            ("dot", &dot),
                            ("when", &when),
                            ("what", &what),
                            ("result", &result),
                        ],
                    ),
                    cells: vec![dot, when, what, result],
                }
            })
            .collect();
        doc.table(Table {
            attrs: "striped compact",
            head: vec![
                Frag::new(),
                self.fill(&t.col_when, &[]),
                self.fill(&t.col_what, &[]),
                self.fill(&t.col_result, &[]),
            ],
            rows: table_rows,
            right: &[3],
            plain_quote: true,
        });
        if let Some(s) = summary.filter(|s| s.completed > 0) {
            let count = format!(
                "{} {}",
                s.completed,
                fmt::plural(
                    s.completed,
                    [
                        static_form(&t.exchange_forms[0]),
                        static_form(&t.exchange_forms[1]),
                        static_form(&t.exchange_forms[2]),
                    ]
                )
            );
            doc.footer(&self.fill(
                &t.total,
                &[
                    ("count", &Frag::text(&count)),
                    ("volume", &Frag::text(&fmt::grouped(s.volume))),
                ],
            ));
        }

        // Тап — карточка заявки (макет: кнопки с «Когда»). Одинаковые подписи («вчера» дважды)
        // различаем итогом. Номер — только корректный: длинные данные Telegram отвергнет.
        let buttons: Vec<_> = rows
            .iter()
            .zip(&labels)
            .filter(|(r, _)| order_id_ok(&r.order))
            .map(|(r, when)| {
                let repeated = labels.iter().filter(|l| *l == when).count() > 1;
                let label = if repeated {
                    format!("{when} · {}", self.history_result(r))
                } else {
                    when.clone()
                };
                self.cb_button(None, &label, &Callback::Order(r.order.clone()))
            })
            .collect();
        let mut keyboard: Vec<Vec<_>> = buttons.chunks(2).map(<[_]>::to_vec).collect();
        keyboard.extend(more);
        self.dual(doc, Keyboard::inline_rows(keyboard))
    }

    /// «Итог» строки истории: выплата или сумма счёта, «возврат», «в работе», «—».
    fn history_result(&self, r: &HistoryRow) -> String {
        let t = &self.texts.history;
        match r.state {
            OrderState::Completed => r
                .payout
                .as_ref()
                .map_or_else(|| t.failed.clone(), |m| fmt::amount(m.amount(), m.asset())),
            OrderState::Refunded => t.refund.clone(),
            s if !s.is_terminal() => t.working.clone(),
            _ => t.failed.clone(),
        }
    }

    /// S11 карточка заявки.
    pub fn order_card(&self, card: &OrderCard, now: DateTime<Utc>) -> Screen {
        let t = &self.texts.card;
        let mut doc = Doc::new();
        doc.title(&self.fill(&t.title, &[("order", &self.order_id(&card.order.id))]));
        let state = self.texts.states.get(card.state);
        let state = {
            let mut c = state.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        };
        let when =
            self.config
                .clock
                .date_time_label(card.created_at, now, &self.texts.history.months);
        let took = match card.finished_at {
            Some(f) => self.fill(
                &t.took,
                &[(
                    "took",
                    &Frag::text(&fmt::took((f - card.created_at).num_seconds())),
                )],
            ),
            None => Frag::new(),
        };
        doc.para(&self.fill(
            &t.status,
            &[
                ("dot", &self.emoji.frag(history_dot(card.state))),
                ("state", &Frag::text(&state)),
                ("when", &Frag::text(&when)),
                ("took", &took),
            ],
        ));

        let src = card.order.direction.source();
        let dst = card.order.direction.target();
        let with_wallet = |m: &domain::Money, platform: domain::Platform| {
            self.fill(
                &t.amount_wallet,
                &[
                    ("amount", &Frag::text(&fmt::amount(m.amount(), m.asset()))),
                    ("wallet", &self.wallet(platform)),
                ],
            )
        };
        let mut kv: Vec<(Frag, Frag)> = Vec::new();
        if let Some(m) = &card.amount_in {
            kv.push((self.fill(&t.sent, &[]), with_wallet(m, src)));
        }
        if let (Some(fee), Some(pct)) = (&card.fee, card.fee_pct) {
            kv.push((
                self.fill(&t.fee, &[]),
                self.fill(
                    &t.fee_value,
                    &[
                        ("fee", &Frag::text(&fmt::amount(fee.amount(), fee.asset()))),
                        ("pct", &self.pct(pct)),
                    ],
                ),
            ));
        }
        if let Some(m) = &card.invoice_amount {
            kv.push((self.fill(&t.invoice, &[]), with_wallet(m, dst)));
        } else if let Some(m) = &card.payout {
            kv.push((self.fill(&t.got, &[]), with_wallet(m, dst)));
        }
        if let Some(m) = &card.refunded {
            kv.push((self.fill(&t.refunded, &[]), with_wallet(m, src)));
        }
        if card.check_url.is_some() {
            kv.push((self.fill(&t.check, &[]), self.fill(&t.check_ready, &[])));
        }
        let rows = kv
            .into_iter()
            .map(|(label, value)| {
                let mut line = label.clone();
                line.push_text(": ").push(&value);
                Row {
                    cells: vec![label, value],
                    line,
                }
            })
            .collect();
        doc.table(Table {
            attrs: "compact",
            head: Vec::new(),
            rows,
            right: &[1],
            plain_quote: true,
        });

        let id = &card.order.id;
        let mut keyboard = Vec::new();
        if let Some(url) = &card.check_url {
            let amount = card.refunded.as_ref().or(card.payout.as_ref());
            match amount {
                Some(m) => keyboard.push(vec![self.claim_button(m, url)]),
                None => keyboard.push(self.claim_row(url)),
            }
        }
        let support = Callback::Support(order_id_ok(id).then(|| id.clone()));
        keyboard.push(vec![
            self.cb_button(Some(Emoji::Support), &self.texts.buttons.support, &support),
            self.copy_button(id, id),
        ]);
        self.dual(doc, Keyboard::inline_rows(keyboard))
    }

    /// S12 `/help`.
    pub fn help(&self, terms: &Terms) -> Screen {
        let t = &self.texts.help;
        let cb = Frag::code(&inline_command(
            domain::Platform::CryptoBot,
            example_amount(terms.get(Direction::CbToXrCheck)),
        ));
        let xr = Frag::code(&inline_command(
            domain::Platform::XRocket,
            example_amount(terms.get(Direction::XrToCbCheck)),
        ));
        let mut body = self.fill(&t.title, &[]);
        for line in &t.lines {
            body.push(&Frag::trusted("\n• "));
            body.push(&self.fill(line, &[("cb", &cb), ("xr", &xr)]));
        }
        let mut checks: Vec<Frag> = t.checks.iter().map(|s| self.fill(s, &[])).collect();
        if let Some(name) = terms
            .bind_username
            .as_deref()
            .and_then(|n| self.bind_username(n))
        {
            checks.push(self.fill(&t.checks_bind, &[("username", &name)]));
        }
        let invoices: Vec<Frag> = t.invoices.iter().map(|s| self.fill(s, &[])).collect();
        for (title, items) in [(&t.checks_title, checks), (&t.invoices_title, invoices)] {
            body.push(&Frag::trusted("\n\n"));
            body.push(&self.fill(title, &[]));
            for item in items {
                body.push(&Frag::trusted("\n• "));
                body.push(&item);
            }
        }
        body.push(&Frag::trusted("\n\n"));
        body.push(&self.fill(&t.rules_title, &[]));
        body.push(&Frag::trusted("\n"));
        body.push(&self.fill(&t.rules, &[]));
        let keyboard = Keyboard::inline_rows(vec![vec![
            self.cb_button(
                Some(Emoji::Support),
                &self.texts.buttons.write_operator,
                &Callback::Support(None),
            )
            .style(ButtonStyle::Primary),
        ]]);
        self.plain(body, keyboard)
    }

    /// `/privacy` — короткая политика (SPEC §1.8, Bot Developer Terms).
    pub fn privacy(&self) -> Screen {
        let t = &self.texts.privacy;
        let mut doc = Doc::new();
        doc.title(&self.fill(&t.title, &[]));
        doc.subtitle(&self.fill(&t.stored_title, &[]));
        let items: Vec<Frag> = t.stored.iter().map(|s| self.fill(s, &[])).collect();
        doc.list(&items);
        doc.gap();
        doc.para(&self.fill(&t.why, &[]));
        doc.para(&self.fill(&t.never, &[]));
        doc.footer(&self.fill(&t.footer, &[]));
        self.dual(doc, Keyboard::None)
    }

    /// «Поддержка». `contact` — `@username` оператора; если задан и корректен — кнопка-ссылка.
    pub fn support(&self, contact: Option<&str>) -> Screen {
        let t = &self.texts.support;
        let mut body = self.fill(&t.title, &[]);
        if let Some(name) = contact
            .map(|c| c.trim_start_matches('@'))
            .filter(|c| username_ok(c))
        {
            body.push(&Frag::trusted("\n"));
            body.push(&self.fill(&t.contact, &[("contact", &Frag::text(&format!("@{name}")))]));
        }
        self.plain(body, Keyboard::None)
    }

    /// «Передал оператору · заявка K7M2Q9XR4T» (макет B1).
    pub fn support_sent(&self, order: Option<&str>) -> Screen {
        let t = &self.texts.support;
        let body = match order.filter(|o| order_id_ok(o)) {
            Some(id) => self.fill(&t.sent, &[("order", &self.order_id(id))]),
            None => self.fill(&t.sent_no_order, &[]),
        };
        self.plain(body, Keyboard::None)
    }

    /// Вопрос передать не вышло: что делать клиенту.
    pub fn support_unavailable(&self, contact: Option<&str>) -> Screen {
        let t = &self.texts.support;
        let body = match contact
            .map(|c| c.trim_start_matches('@'))
            .filter(|c| username_ok(c))
        {
            Some(name) => self.fill(
                &t.unavailable_contact,
                &[("contact", &Frag::text(&format!("@{name}")))],
            ),
            None => self.fill(&t.unavailable, &[]),
        };
        self.plain(body, Keyboard::None)
    }

    /// Чек создаётся через инлайн-режим кошелька прямо в чате: ссылка придёт позже, бот
    /// её подхватит сам — клиенту ничего делать не нужно.
    pub fn check_link_pending(&self, platform: domain::Platform) -> Screen {
        let body = self.fill(
            &self.texts.misc.check_pending,
            &[("wallet", &self.wallet(platform))],
        );
        self.plain(body, Keyboard::None)
    }

    /// То же во время ожидания: сколько секунд ждём (бот обновляет сообщение сам).
    pub fn check_link_waiting(&self, platform: domain::Platform, secs: u64) -> Screen {
        let body = self.fill(
            &self.texts.misc.check_waiting,
            &[
                ("wallet", &self.wallet(platform)),
                ("secs", &Frag::text(&secs.to_string())),
            ],
        );
        self.plain(body, Keyboard::None)
    }

    /// Кошелёк так и не дописал ссылку: чек не открыт, деньги в нём.
    pub fn check_link_missing(&self, platform: domain::Platform) -> Screen {
        let body = self.fill(
            &self.texts.misc.check_missing,
            &[("wallet", &self.wallet(platform))],
        );
        self.plain(body, Keyboard::None)
    }

    /// S13: сообщение без ссылки.
    pub fn no_link(&self) -> Screen {
        let body = self.fill(&self.texts.misc.no_link, &[]);
        let keyboard = Keyboard::inline_rows(vec![vec![self.cb_button(
            Some(Emoji::Help),
            &self.texts.buttons.howto,
            &Callback::HowTo(Direction::CbToXrCheck),
        )]]);
        self.plain(body, keyboard)
    }

    /// Прислали ссылку на счёт, а оплата счетов ещё не подключена: счёт не тронут, что
    /// можно сделать сейчас.
    pub fn invoice_soon(&self) -> Screen {
        let body = self.fill(
            &self.texts.misc.invoice_soon,
            &[("exchange", &Frag::text(&self.texts.menu.exchange))],
        );
        self.plain(body, Keyboard::None)
    }

    /// Отмена: заявки (до прихода денег) или шага диалога.
    pub fn cancelled(&self, order: Option<&str>) -> Screen {
        let m = &self.texts.misc;
        let body = match order {
            Some(id) => self.fill(&m.cancelled, &[("order", &self.order_id(&clip(id, 32)))]),
            None => self.fill(&m.cancelled_flow, &[]),
        };
        self.plain(body, Keyboard::None)
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn terms(min: Decimal, max: Decimal) -> DirectionTerms {
        DirectionTerms {
            direction: Direction::CbToXrCheck,
            fee_pct: dec!(2.5),
            min_fee: dec!(0.1),
            min,
            max,
            status: DirectionStatus::Open,
            tiers: Vec::new(),
            promo: None,
            personal: false,
            pause: None,
            new_user_max: None,
        }
    }

    #[test]
    fn example_amount_stays_within_limits() {
        assert_eq!(example_amount(None), dec!(100));
        assert_eq!(example_amount(Some(&terms(dec!(2), dec!(500)))), dec!(100));
        assert_eq!(
            example_amount(Some(&terms(dec!(150.5), dec!(500)))),
            dec!(151)
        );
        assert_eq!(example_amount(Some(&terms(dec!(2), dec!(55.5)))), dec!(55));
    }
}
