//! Справочные экраны: приветствие, курс и лимиты, выбор направления, инструкции, история,
//! карточка заявки, помощь, конфиденциальность, поддержка.

use botapi::ButtonStyle;
use domain::{Decimal, Direction, Flow};

use super::{Ui, inline_command, menu_command, username_ok, wallet_bot_url};
use crate::callback::{Callback, order_id_ok};
use crate::doc::{Doc, Row, Table};
use crate::emoji::Emoji;
use crate::fmt::{self, TimeFormat};
use crate::html::{Frag, clip};
use crate::model::{DirectionStatus, DirectionTerms, HistoryRow, OrderCard, Terms, UiError};
use crate::screen::{Keyboard, Screen};

/// Сколько строк истории показываем за раз (SPEC S10).
pub const HISTORY_PAGE: usize = 10;

/// Пример суммы для инлайн-команды: 10 USDT, но в пределах лимитов направления.
fn example_amount(terms: Option<&DirectionTerms>) -> Decimal {
    let ten = Decimal::TEN;
    match terms {
        Some(t) if t.max >= t.min => ten.max(t.min.ceil()).min(t.max.floor().max(t.min.ceil())),
        _ => ten,
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
    fn status_text(&self, status: DirectionStatus) -> &str {
        let s = &self.texts.direction_status;
        match status {
            DirectionStatus::Open => &s.open,
            DirectionStatus::Paused => &s.paused,
            DirectionStatus::Off => &s.off,
        }
    }

    /// Комиссия открытого направления или его статус.
    fn fee_or_status(&self, t: &DirectionTerms) -> Frag {
        match t.status {
            DirectionStatus::Open => self.pct(t.fee_pct),
            other => Frag::text(self.status_text(other)),
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
        doc.gap();

        let rows = terms
            .directions
            .iter()
            .map(|d| {
                let offer = self.texts.offers.get(d.direction);
                let icon = self.emoji.frag(direction_icon(d.direction));
                let send = Frag::text(&offer.send);
                let get = Frag::text(&offer.get);
                let fee = self.fee_or_status(d);
                let mut first = icon.clone();
                first.push_text(" ").push(&send);
                Row {
                    cells: vec![first, get.clone(), fee.clone()],
                    line: self.fill(
                        &t.row,
                        &[
                            ("icon", &icon),
                            ("send", &send),
                            ("get", &get),
                            ("fee", &fee),
                        ],
                    ),
                }
            })
            .collect();
        doc.table(Table {
            attrs: "bordered compact",
            head: vec![
                self.fill(&t.col_send, &[]),
                self.fill(&t.col_get, &[]),
                self.fill(&t.col_fee, &[]),
            ],
            rows,
            right: &[2],
            plain_quote: true,
        });

        let open: Vec<&DirectionTerms> = terms.open().collect();
        let min = open.iter().map(|d| d.min).min();
        let max = open.iter().map(|d| d.max).max();
        let min_fee = open.iter().map(|d| d.min_fee).min();
        match (min, max, min_fee) {
            (Some(min), Some(max), Some(min_fee)) => doc.para(&self.fill(
                &t.limits,
                &[
                    ("min", &self.usdt(min)),
                    ("max", &self.usdt(max)),
                    ("min_fee", &self.usdt(min_fee)),
                ],
            )),
            _ => doc.para(&self.fill(&t.closed, &[])),
        }
        doc.gap();

        let cb_cmd = Frag::code(&inline_command(
            domain::Platform::CryptoBot,
            example_amount(terms.get(Direction::CbToXrCheck)),
        ));
        let xr_cmd = Frag::code(&inline_command(
            domain::Platform::XRocket,
            example_amount(terms.get(Direction::XrToCbCheck)),
        ));
        doc.details(&self.fill(&t.howto_title, &[]), false, |d| {
            d.list(&[
                self.fill(&t.howto_cb, &[("cmd", &cb_cmd)]),
                self.fill(&t.howto_xr, &[("cmd", &xr_cmd)]),
            ]);
        });
        doc.footer(&self.fill(&t.footer, &[]));
        self.dual(doc, Keyboard::Reply(self.main_menu()))
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
                let direction = self.direction_name(d.direction);
                let fee = self.pct(d.fee_pct);
                let status = Frag::text(self.status_text(d.status));
                let min = Frag::text(&fmt::amount(d.min, domain::Asset::Usdt));
                let max = Frag::text(&fmt::amount(d.max, domain::Asset::Usdt));
                Row {
                    line: self.fill(
                        &t.row,
                        &[
                            ("direction", &direction),
                            ("fee", &fee),
                            ("min", &self.usdt(d.min)),
                            ("max", &self.usdt(d.max)),
                            ("status", &status),
                        ],
                    ),
                    cells: vec![direction, fee, min, max, status],
                }
            })
            .collect();
        doc.table(Table {
            attrs: "bordered striped compact",
            head: vec![
                self.fill(&t.col_direction, &[]),
                self.fill(&t.col_fee, &[]),
                self.fill(&t.col_min, &[]),
                self.fill(&t.col_max, &[]),
                self.fill(&t.col_status, &[]),
            ],
            rows,
            right: &[1, 2, 3],
            plain_quote: true,
        });
        doc.gap();

        let mut checks: Vec<Frag> = t.checks.iter().map(|s| self.fill(s, &[])).collect();
        if let Some(name) = terms
            .bind_username
            .as_deref()
            .and_then(|n| self.bind_username(n))
        {
            checks.push(self.fill(&t.checks_bind, &[("username", &name)]));
        }
        doc.details(&self.fill(&t.checks_title, &[]), false, |d| d.list(&checks));
        doc.details(&self.fill(&t.invoices_title, &[]), false, |d| {
            let items: Vec<Frag> = t.invoices.iter().map(|s| self.fill(s, &[])).collect();
            d.list(&items);
        });
        doc.details(&self.fill(&t.rules_title, &[]), false, |d| {
            d.para(&self.fill(&t.rules, &[]));
        });
        doc.footer(&self.fill(&t.footer, &[]));

        let keyboard = Keyboard::inline_rows(vec![vec![self.cb_button(
            Some(Emoji::Privacy),
            &self.texts.buttons.privacy,
            &Callback::Privacy,
        )]]);
        self.dual(doc, keyboard)
    }

    /// «Обменять чек»: два направления с текущей комиссией и кнопками (DESIGN-v0.2 §6).
    /// Основное направление CryptoBot → xRocket — `primary`; на паузе — `disabled`.
    pub fn exchange_menu(&self, terms: &Terms) -> Screen {
        let t = &self.texts.exchange;
        let directions = [Direction::CbToXrCheck, Direction::XrToCbCheck];
        let mut body = self.fill(&t.title, &[]);
        body.push(&Frag::trusted("\n"));
        body.push(&self.fill(&t.lead, &[]));
        body.push(&Frag::trusted("\n"));
        let mut rows = Vec::new();
        for direction in directions {
            let Some(d) = terms.get(direction) else {
                continue;
            };
            let icon = self.emoji.frag(direction_icon(direction));
            let name = self.direction_name(direction);
            body.push(&Frag::trusted("\n"));
            if d.status == DirectionStatus::Open {
                body.push(&self.fill(
                    &t.line,
                    &[
                        ("icon", &icon),
                        ("direction", &name),
                        ("fee", &self.pct(d.fee_pct)),
                        ("min", &self.usdt(d.min)),
                        ("max", &self.usdt(d.max)),
                    ],
                ));
            } else {
                body.push(&self.fill(&t.line_paused, &[("icon", &icon), ("direction", &name)]));
            }
            let b = &self.texts.buttons;
            let label = match direction {
                Direction::CbToXrCheck => &b.cb_to_xr,
                _ => &b.xr_to_cb,
            };
            let button = if d.status == DirectionStatus::Open {
                let button = self.cb_button(None, label, &Callback::Direction(direction));
                if direction == Direction::CbToXrCheck {
                    button.style(ButtonStyle::Primary)
                } else {
                    button
                }
            } else {
                botapi::InlineKeyboardButton::disabled(label.as_str())
            };
            rows.push(vec![button]);
        }
        body.push(&Frag::trusted("\n\n"));
        body.push(&self.fill(&t.footer, &[]));
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
        let cmd = Frag::code(&inline_command(src, example_amount(Some(d))));
        let mut body = self.fill(&t.title, &[("direction", &self.direction_name(direction))]);
        body.push(&Frag::trusted("\n"));
        body.push(&self.fill(
            &t.body,
            &[
                ("src", &self.wallet(src)),
                ("dst", &self.wallet(direction.target())),
                ("min", &self.usdt(d.min)),
                ("max", &self.usdt(d.max)),
                ("fee", &self.pct(d.fee_pct)),
                ("min_fee", &self.usdt(d.min_fee)),
            ],
        ));
        body.push(&Frag::trusted("\n\n<blockquote>"));
        body.push(&self.fill(&t.quick, &[("cmd", &cmd)]));
        body.push(&Frag::trusted("</blockquote>\n"));
        body.push(&self.fill(&t.rules, &[]));
        let b = &self.texts.buttons;
        let keyboard = Keyboard::inline_rows(vec![
            vec![self.cb_button(None, &b.howto, &Callback::HowTo(direction))],
            vec![self.cb_button(None, &b.cancel, &Callback::Cancel(None))],
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
    pub fn history(&self, rows: &[HistoryRow], next_page: Option<u32>) -> Screen {
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
        doc.subtitle(&self.fill(&t.title, &[]));
        let table_rows = rows
            .iter()
            .map(|r| {
                let order = self.order_id(&r.order);
                let when = self.at(r.created_at, TimeFormat::DayTime);
                let what = self.direction_name(r.direction);
                let amount = r
                    .amount
                    .as_ref()
                    .map_or_else(|| Frag::text("—"), |m| self.money(m));
                let result = Frag::text(self.texts.states.get(r.state));
                Row {
                    line: self.fill(
                        &t.row,
                        &[
                            ("order", &order),
                            ("when", &when),
                            ("what", &what),
                            ("amount", &amount),
                            ("result", &result),
                        ],
                    ),
                    cells: vec![order, when, what, amount, result],
                }
            })
            .collect();
        doc.table(Table {
            attrs: "compact striped",
            head: vec![
                self.fill(&t.col_order, &[]),
                self.fill(&t.col_when, &[]),
                self.fill(&t.col_what, &[]),
                self.fill(&t.col_amount, &[]),
                self.fill(&t.col_result, &[]),
            ],
            rows: table_rows,
            right: &[3],
            plain_quote: false,
        });
        doc.footer(&self.fill(&t.footer, &[]));

        // Кнопка — только для корректного номера: длинные данные кнопки Telegram отвергнет
        // вместе со всем сообщением.
        let buttons: Vec<_> = rows
            .iter()
            .filter(|r| order_id_ok(&r.order))
            .map(|r| self.cb_button(None, &r.order, &Callback::Order(r.order.clone())))
            .collect();
        let mut keyboard: Vec<Vec<_>> = buttons.chunks(2).map(<[_]>::to_vec).collect();
        keyboard.extend(more);
        self.dual(doc, Keyboard::inline_rows(keyboard))
    }

    /// S11 карточка заявки.
    pub fn order_card(&self, card: &OrderCard) -> Screen {
        let t = &self.texts.card;
        let mut doc = Doc::new();
        doc.title(&self.fill(
            &t.title,
            &[
                ("order", &self.order_id(&card.order.id)),
                ("state", &Frag::text(self.texts.states.get(card.state))),
            ],
        ));
        let mut kv: Vec<(&str, Frag)> = vec![
            (&t.created, self.at(card.created_at, TimeFormat::Full)),
            (&t.direction, self.direction_name(card.order.direction)),
        ];
        let money_rows = [
            (&t.received, &card.amount_in),
            (&t.invoice, &card.invoice_amount),
            (&t.fee, &card.fee),
            (&t.paid_out, &card.payout),
            (&t.refunded, &card.refunded),
        ];
        for (label, value) in money_rows {
            if let Some(m) = value {
                kv.push((label, self.money(m)));
            }
        }
        let rows = kv
            .into_iter()
            .map(|(label, value)| {
                let label = self.fill(label, &[]);
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
            right: &[],
            plain_quote: true,
        });

        let b = &self.texts.buttons;
        let id = &card.order.id;
        let mut keyboard = Vec::new();
        if let Some(url) = &card.check_url {
            keyboard.push(self.claim_row(url));
        }
        let support = Callback::Support(order_id_ok(id).then(|| id.clone()));
        keyboard.push(vec![
            self.copy_order_button(id),
            self.cb_button(Some(Emoji::Support), &b.order_question, &support),
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
        let contact = contact
            .map(|c| c.trim_start_matches('@'))
            .filter(|c| username_ok(c));
        match contact {
            Some(name) => {
                let body = self.fill(
                    &t.with_contact,
                    &[("contact", &Frag::text(&format!("@{name}")))],
                );
                let keyboard = Keyboard::inline_rows(vec![vec![
                    self.url_button(
                        Some(Emoji::Support),
                        &self.texts.buttons.write_operator,
                        &format!("https://t.me/{name}"),
                    )
                    .style(ButtonStyle::Primary),
                ]]);
                self.plain(body, keyboard)
            }
            None => self.plain(self.fill(&t.without_contact, &[]), Keyboard::None),
        }
    }

    /// S13: сообщение без ссылки.
    pub fn no_link(&self) -> Screen {
        let body = self.fill(
            &self.texts.misc.no_link,
            &[
                ("cb_check", &Frag::code("https://t.me/send?start=CQ…")),
                ("xr_check", &Frag::code("https://t.me/xrocket?start=t_…")),
                (
                    "cb_cmd",
                    &Frag::code(&inline_command(domain::Platform::CryptoBot, Decimal::TEN)),
                ),
            ],
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
        }
    }

    #[test]
    fn example_amount_stays_within_limits() {
        assert_eq!(example_amount(None), dec!(10));
        assert_eq!(example_amount(Some(&terms(dec!(2), dec!(500)))), dec!(10));
        assert_eq!(
            example_amount(Some(&terms(dec!(15.5), dec!(500)))),
            dec!(16)
        );
        assert_eq!(example_amount(Some(&terms(dec!(2), dec!(5.5)))), dec!(5));
    }
}
