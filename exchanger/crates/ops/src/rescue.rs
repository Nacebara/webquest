//! «🆘 Спасти резерв» в пульте (решение владельца 17.11.2026, вариант Б).
//!
//! Только владелец: кнопка в «Резервах» и в тревогах «Новый вход» и «Аккаунт ограничен» →
//! выбор кошелька → подтверждение красной кнопкой → движок (`engine::rescue`) создаёт чек на
//! весь баланс кошелька. Ссылка приходит тревогой `rescue_done` только владельцу, с зелёной
//! «Забрать». Кнопки, которые открывают спасение (`rsq…`), у остальных ролей не рисуются.

use std::sync::Arc;

use botapi::{ButtonStyle, InlineKeyboardButton, InlineKeyboardMarkup};
use domain::{Decimal, Platform};
use engine::rescue::{RescueError, RescueRefusal};
use serde_json::Value;
use storage::rescues::RescueStatus;
use storage::staff::{Role, StaffMember};

use crate::actions::Action;
use crate::cb::Cb;
use crate::texts::esc;
use crate::views::usdt;
use crate::{Inner, OpsError, Screen, btn};

/// Префикс кнопок, которые открывают спасение: их видит только владелец.
const OWNER_ONLY_PREFIXES: [&str; 2] = ["rsq", "rsr"];

/// Убрать из клавиатуры кнопки спасения для не-владельца (копии тревог рисуются одной
/// клавиатурой на всех).
pub(crate) fn for_role(
    markup: Option<InlineKeyboardMarkup>,
    role: Role,
) -> Option<InlineKeyboardMarkup> {
    if role >= Role::Owner {
        return markup;
    }
    let markup = markup?;
    let rows: Vec<Vec<InlineKeyboardButton>> = markup
        .inline_keyboard
        .into_iter()
        .map(|row| {
            row.into_iter()
                .filter(|b| match &b.action {
                    botapi::InlineButtonAction::CallbackData(d) => !OWNER_ONLY_PREFIXES
                        .iter()
                        .any(|p| d.split(':').next() == Some(*p)),
                    _ => true,
                })
                .collect::<Vec<_>>()
        })
        .filter(|row| !row.is_empty())
        .collect();
    (!rows.is_empty()).then(|| InlineKeyboardMarkup::new(rows))
}

fn dec(p: &Value, key: &str) -> Decimal {
    p[key]
        .as_str()
        .and_then(|a| a.parse().ok())
        .unwrap_or_default()
}

/// Карточки тревог спасения: `rescue_done` (ссылка), `rescue_failed`, `rescue_unknown`.
pub fn alert_screen(
    t: &crate::Texts,
    title: &str,
    p: &Value,
    id: i64,
    name: &dyn Fn(Platform) -> String,
) -> Screen {
    let wallet = p["platform"]
        .as_str()
        .and_then(Platform::from_db_name)
        .map_or_else(|| esc(p["platform"].as_str().unwrap_or("")), name);
    let amount = usdt(dec(p, "amount"));
    let rescue = p["rescue"].as_i64().unwrap_or_default();
    let reason = esc(p["reason"].as_str().unwrap_or(""));
    let vars: [(&str, &dyn std::fmt::Display); 3] =
        [("name", &wallet), ("amount", &amount), ("reason", &reason)];
    match title {
        "rescue_done" => {
            let mut sc = Screen::new(t.f("rescue.alert_done", &vars));
            if let Some(url) = p["url"].as_str() {
                sc = sc.row(vec![
                    InlineKeyboardButton::url(t.f("rescue.b_claim", &vars), url)
                        .style(ButtonStyle::Success),
                ]);
            }
            sc.row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))])
        }
        "rescue_unknown" => Screen::new(t.f("rescue.alert_unknown", &vars))
            .row(vec![
                btn(t.t("rescue.b_happened"), &Cb::RescueResolve(rescue, true)),
                btn(t.t("rescue.b_not"), &Cb::RescueResolve(rescue, false)),
            ])
            .row(vec![btn(t.t("alert.b_ack"), &Cb::Ack(id))]),
        _ => Screen::new(t.f("rescue.alert_failed", &vars)).row(vec![
            btn(t.t("rescue.b_again"), &Cb::Rescue),
            btn(t.t("alert.b_ack"), &Cb::Ack(id)),
        ]),
    }
}

impl Inner {
    /// Баланс кошелька по леджеру (USDT).
    async fn ledger_of(&self, platform: Platform) -> Result<Option<Decimal>, OpsError> {
        let mut conn = self.pool.acquire().await?;
        let Some((wallet, _)) = storage::wallets::active(&mut conn, platform).await? else {
            return Ok(None);
        };
        Ok(Some(
            storage::ledger::wallet_balances(&mut conn)
                .await?
                .into_iter()
                .find(|b| b.wallet_account_id == wallet && b.asset == "USDT")
                .map_or(Decimal::ZERO, |b| b.ledger_balance),
        ))
    }

    /// «🆘 Спасти резерв»: какой кошелёк.
    pub(crate) async fn rescue_menu(&self, staff: &StaffMember) -> Result<Screen, OpsError> {
        if let Some(denied) = self.deny(staff, Role::Owner) {
            return Ok(denied);
        }
        let t = &self.texts;
        let mut sc = Screen::new(t.t("rescue.menu"));
        let mut any = false;
        for p in [Platform::XRocket, Platform::CryptoBot] {
            if let Some(amount) = self.ledger_of(p).await? {
                any = true;
                sc = sc.row(vec![
                    btn(
                        t.f(
                            "rescue.b_wallet",
                            &[("name", &self.platform_name(p)), ("amount", &usdt(amount))],
                        ),
                        &Cb::RescueGo(p),
                    )
                    .style(ButtonStyle::Danger),
                ]);
            }
        }
        if !any {
            sc = Screen::new(t.t("rescue.no_wallets"));
        }
        Ok(sc.row(crate::nav::back(t, &Cb::Reserves)))
    }

    /// Кошелёк выбран — подтверждение красной кнопкой.
    pub(crate) async fn rescue_go(
        self: &Arc<Self>,
        staff: &StaffMember,
        platform: Platform,
    ) -> Result<Screen, OpsError> {
        if let Some(denied) = self.deny(staff, Role::Owner) {
            return Ok(denied);
        }
        let ledger = self.ledger_of(platform).await?.unwrap_or_default();
        self.preview(staff, Action::Rescue { platform, ledger })
            .await
    }

    /// Подтверждено: движок замеряет баланс и ставит чек в очередь.
    pub(crate) async fn rescue_apply(
        &self,
        staff: &StaffMember,
        platform: Platform,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let name = self.platform_name(platform);
        let back = || crate::nav::back(t, &Cb::Reserves);
        match self.engine.rescue_request(platform, staff, None).await {
            Ok(r) => {
                let mut text = t.f(
                    "rescue.started",
                    &[("name", &name), ("amount", &usdt(r.rescue.amount))],
                );
                if r.capped {
                    text.push('\n');
                    text.push_str(&t.f(
                        "rescue.started_capped",
                        &[("measured", &usdt(r.rescue.measured))],
                    ));
                }
                if r.in_flight > 0 {
                    text.push('\n');
                    text.push_str(&t.f("rescue.started_inflight", &[("n", &r.in_flight)]));
                }
                Ok(Screen::new(text).row(back()))
            }
            Err(RescueError::Refused(why)) => {
                let text = match why {
                    RescueRefusal::Forbidden => t.f(
                        "common.not_allowed",
                        &[("role", &self.role_name(Role::Owner))],
                    ),
                    RescueRefusal::NoWallet => t.f("rescue.no_wallet", &[("name", &name)]),
                    RescueRefusal::AlreadyOpen => t.f("rescue.already", &[("name", &name)]),
                    RescueRefusal::Unreadable(e) => {
                        t.f("rescue.unreadable", &[("name", &name), ("error", &esc(&e))])
                    }
                    RescueRefusal::Nothing { available, min } => t.f(
                        "rescue.nothing",
                        &[
                            ("name", &name),
                            ("available", &usdt(available)),
                            ("min", &usdt(min)),
                        ],
                    ),
                    RescueRefusal::WrongState => t.t("rescue.gone"),
                };
                Ok(Screen::new(text).row(back()))
            }
            Err(RescueError::Engine(e)) => Err(e.into()),
        }
    }

    /// «Чек был» / «Чека не было» на неясном спасении — подтверждение.
    pub(crate) async fn rescue_resolve_entry(
        self: &Arc<Self>,
        staff: &StaffMember,
        id: i64,
        happened: bool,
    ) -> Result<Screen, OpsError> {
        if let Some(denied) = self.deny(staff, Role::Owner) {
            return Ok(denied);
        }
        let r = {
            let mut conn = self.pool.acquire().await?;
            storage::rescues::get(&mut conn, id, false).await.ok()
        };
        let Some(r) = r.filter(|r| r.status == RescueStatus::Unknown) else {
            return Ok(Screen::new(self.texts.t("rescue.gone"))
                .row(crate::nav::back(&self.texts, &Cb::Reserves)));
        };
        self.preview(
            staff,
            Action::RescueResolve {
                id,
                happened,
                amount: r.amount,
                platform: r.platform,
            },
        )
        .await
    }

    pub(crate) async fn rescue_resolve_apply(
        &self,
        staff: &StaffMember,
        id: i64,
        happened: bool,
    ) -> Result<Screen, OpsError> {
        let t = &self.texts;
        let text = match self.engine.rescue_resolve(id, staff, happened).await {
            Ok(r) => t.f(
                if happened {
                    "rescue.resolved_happened"
                } else {
                    "rescue.resolved_not"
                },
                &[
                    ("name", &self.platform_name(r.platform)),
                    ("amount", &usdt(r.amount)),
                ],
            ),
            Err(RescueError::Refused(RescueRefusal::Forbidden)) => t.f(
                "common.not_allowed",
                &[("role", &self.role_name(Role::Owner))],
            ),
            Err(RescueError::Refused(_)) => t.t("rescue.gone"),
            Err(RescueError::Engine(e)) => return Err(e.into()),
        };
        Ok(Screen::new(text).row(crate::nav::back(t, &Cb::Reserves)))
    }
}
