# Покрытие полной проверки репозитория

Начато 19.11.2026 по просьбе владельца: проверить весь репозиторий, каждый файл. Методика —
`docs/AUDIT-METHOD.md` (тяжесть 1–4, находка = место + сценарий + доказательство, перед записью —
поиск защиты в другом месте). Находки — `docs/audit-prr/FINDINGS.md`.

Статус: **—** не прочитан · **прочитан DD.MM** — каждая строка глазами · **скрипт** — проверен
механически (чем — в примечании).

Не в таблице, проверяются скриптом: `.sqlx/` (178 файлов, `cargo sqlx prepare --check`), `assets/emoji/`
(255 файлов значков), картинки и шрифты.

## 1. Деньги: схема базы, учёт, хранилище, движок — 60 файлов, 18297 строк

| Файл | Строк | Статус | Находки |
|---|---|---|---|
| `crates/domain/src/asset.rs` | 126 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/domain/src/direction.rs` | 104 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/domain/src/fees.rs` | 597 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/domain/src/fsm.rs` | 882 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/domain/src/ledger.rs` | 659 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/domain/src/lib.rs` | 17 | прочитан 19.11 | нет |
| `crates/domain/src/money.rs` | 208 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/domain/src/pricing.rs` | 567 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/domain/src/reconcile.rs` | 266 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/engine/src/account_alerts.rs` | 192 | прочитан 19.11 (без встроенных тестов) | Д-19.6 |
| `crates/engine/src/availability.rs` | 124 | прочитан 19.11 | Д-19.4 |
| `crates/engine/src/effects.rs` | 269 | прочитан 19.11 | нет |
| `crates/engine/src/fees.rs` | 418 | прочитан 19.11 | нет |
| `crates/engine/src/invest.rs` | 1286 | прочитан 19.11 (без встроенных тестов) | Д-19.5, Д-19.6 |
| `crates/engine/src/invoice.rs` | 778 | прочитан 19.11 | нет |
| `crates/engine/src/lib.rs` | 429 | прочитан 19.11 | нет |
| `crates/engine/src/operator.rs` | 742 | прочитан 19.11 | Д-19.3 |
| `crates/engine/src/reconcile.rs` | 635 | прочитан 19.11 | нет |
| `crates/engine/src/sessions.rs` | 106 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/engine/src/settle.rs` | 123 | прочитан 19.11 | Д-19.2 |
| `crates/engine/src/submit.rs` | 330 | прочитан 19.11 | Д-19.2 |
| `crates/engine/src/worker.rs` | 1418 | прочитан 19.11 | Д-19.1 |
| `crates/storage/src/admin.rs` | 861 | частично 19.11: бан, лимиты, статистика, пауза аккаунта; карточки и очередь — бегло | нет |
| `crates/storage/src/alerts.rs` | 571 | прочитан 19.11 | нет |
| `crates/storage/src/audit.rs` | 103 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/client.rs` | 471 | прочитан 19.11 (запросы и доступ к чужим заявкам) | нет |
| `crates/storage/src/client_prefs.rs` | 61 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/directions.rs` | 312 | прочитан 19.11 | Д-19.9 |
| `crates/storage/src/error.rs` | 74 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/holds.rs` | 216 | прочитан 19.11 | Д-19.6 (двойной счёт в лимите — тяж. 1) |
| `crates/storage/src/invest.rs` | 543 | прочитан 19.11 | нет |
| `crates/storage/src/ledger.rs` | 163 | прочитан 19.11 | нет |
| `crates/storage/src/lib.rs` | 78 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/operations.rs` | 715 | прочитан 19.11 | Д-19.7 |
| `crates/storage/src/orders.rs` | 500 | прочитан 19.11 | Д-19.8 |
| `crates/storage/src/promo.rs` | 167 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/sessions.rs` | 260 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/settings.rs` | 106 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/staff.rs` | 243 | прочитан 19.11 (роли, добавление, владелец) | Д-19.6 (тяж. 1) |
| `crates/storage/src/transitions.rs` | 30 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/user_terms.rs` | 342 | прочитан 19.11 | нет |
| `crates/storage/src/users.rs` | 39 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/wallet_messages.rs` | 103 | просмотрен 19.11: все запросы и функции | нет |
| `crates/storage/src/wallets.rs` | 176 | прочитан 19.11 | к Д-19.1 |
| `migrations/0001_init.sql` | 804 | прочитан 19.11 | нет |
| `migrations/0002_promo_broadcast.sql` | 29 | прочитан 19.11 | нет |
| `migrations/0003_reconcile_unsettled.sql` | 15 | прочитан 19.11 | нет |
| `migrations/0004_reconcile_balance_read.sql` | 16 | прочитан 19.11 | нет |
| `migrations/0005_alert_copies.sql` | 27 | прочитан 19.11 | нет |
| `migrations/0006_progress_sent_at.sql` | 8 | прочитан 19.11 | нет |
| `migrations/0007_client_prefs.sql` | 12 | прочитан 19.11 | нет |
| `migrations/0008_drop_plain_view.sql` | 3 | прочитан 19.11 | нет |
| `migrations/0009_alert_mutes.sql` | 6 | прочитан 19.11 | нет |
| `migrations/0010_check_claimed.sql` | 5 | прочитан 19.11 | нет |
| `migrations/0011_alert_copy_by_message.sql` | 4 | прочитан 19.11 | нет |
| `migrations/0012_alert_copy_closed.sql` | 9 | прочитан 19.11 | нет |
| `migrations/0013_rescues.sql` | 44 | прочитан 19.11 | нет |
| `migrations/0014_drop_rescues.sql` | 19 | прочитан 19.11 | нет |
| `schema.sql` | 804 | прочитан 19.11 | нет |
| `schema_test.sql` | 82 | — | сверяется тестом storage/tests/invariants.rs |

## 2. Юзербот и кошельки — 31 файлов, 14988 строк

| Файл | Строк | Статус | Находки |
|---|---|---|---|
| `crates/mtproto/src/config.rs` | 236 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/mtproto/src/convert.rs` | 1091 | — | |
| `crates/mtproto/src/crypto.rs` | 349 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/mtproto/src/errors.rs` | 248 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/mtproto/src/lib.rs` | 109 | — | |
| `crates/mtproto/src/login.rs` | 444 | частично 19.11: журнал входа без секретов | нет |
| `crates/mtproto/src/requests.rs` | 300 | частично 19.11: адресаты (только закреплённые боты и «Избранное»), резолв со сверкой ID | нет |
| `crates/mtproto/src/session.rs` | 1035 | частично 19.11: журнал входа без секретов | нет |
| `crates/mtproto/src/transport.rs` | 1134 | частично 19.11: адресаты (только закреплённые боты и «Избранное»), резолв со сверкой ID | нет |
| `crates/parsers/src/amount.rs` | 506 | — | |
| `crates/parsers/src/lib.rs` | 16 | — | |
| `crates/parsers/src/links.rs` | 1300 | частично 19.11: проверка формата кода, чужие боты, двойники доменов | нет |
| `crates/parsers/src/replies.rs` | 1053 | — | |
| `crates/parsers/src/screens.rs` | 414 | — | |
| `crates/userbot/src/flows/activate.rs` | 149 | прочитан 19.11 | нет |
| `crates/userbot/src/flows/balance.rs` | 48 | прочитан 19.11 | нет |
| `crates/userbot/src/flows/conv.rs` | 617 | прочитан 19.11 | Ю-19.1 |
| `crates/userbot/src/flows/gate.rs` | 227 | прочитан 19.11 (без встроенных тестов) | Ю-19.1 |
| `crates/userbot/src/flows/invoice.rs` | 440 | прочитан 19.11 | З-19.1 |
| `crates/userbot/src/flows/issue.rs` | 606 | прочитан 19.11 | нет |
| `crates/userbot/src/flows/mod.rs` | 279 | прочитан 19.11 | к Д-19.7 |
| `crates/userbot/src/flows/rid.rs` | 84 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/userbot/src/lib.rs` | 19 | — | |
| `crates/userbot/src/sim/bots.rs` | 1408 | — | |
| `crates/userbot/src/sim/mod.rs` | 426 | — | |
| `crates/userbot/src/sim/model.rs` | 327 | — | |
| `crates/userbot/src/sim/state.rs` | 886 | — | |
| `crates/userbot/src/sim/texts.rs` | 601 | — | |
| `crates/userbot/src/sim/transport.rs` | 216 | — | |
| `crates/userbot/src/transport.rs` | 192 | — | |
| `crates/userbot/src/wallet.rs` | 228 | — | |

## 3. Интерфейс: клиент, пульт, Bot API — 56 файлов, 30397 строк

| Файл | Строк | Статус | Находки |
|---|---|---|---|
| `crates/botapi/src/client.rs` | 706 | — | |
| `crates/botapi/src/error.rs` | 369 | — | |
| `crates/botapi/src/keyboard.rs` | 340 | — | |
| `crates/botapi/src/lib.rs` | 41 | — | |
| `crates/botapi/src/methods.rs` | 762 | — | |
| `crates/botapi/src/types.rs` | 450 | — | |
| `crates/ops/ru.toml` | 1160 | прочитан 19.11 | Т-2…Т-6 |
| `crates/ops/src/actions.rs` | 2423 | частично 19.11: права (роль заново при подтверждении), одноразовый ключ подтверждения, взнос/вывод/списание | нет |
| `crates/ops/src/alerts.rs` | 1345 | прочитан 19.11 (тесты — группа 5) | С-19.1, Б-19.3, П-19.2, П-19.3 |
| `crates/ops/src/cb.rs` | 642 | — | |
| `crates/ops/src/chart.rs` | 357 | — | |
| `crates/ops/src/dash.rs` | 1041 | — | |
| `crates/ops/src/inbox.rs` | 581 | частично 19.11: вопрос в поддержку — только своя заявка (Б-1 закрыта) | нет |
| `crates/ops/src/invest.rs` | 825 | — | |
| `crates/ops/src/journal.rs` | 358 | — | |
| `crates/ops/src/lib.rs` | 1289 | частично 19.11: кто сотрудник, ключ подтверждения, вопрос в поддержку | нет |
| `crates/ops/src/lists.rs` | 282 | — | |
| `crates/ops/src/look.rs` | 231 | — | |
| `crates/ops/src/nav.rs` | 273 | — | |
| `crates/ops/src/preview.rs` | 691 | — | |
| `crates/ops/src/reserves.rs` | 503 | — | |
| `crates/ops/src/review.rs` | 759 | прочитан 19.11 | Д-19.11 |
| `crates/ops/src/stats.rs` | 374 | — | |
| `crates/ops/src/texts.rs` | 86 | — | |
| `crates/ops/src/views.rs` | 2040 | — | |
| `crates/tgbot/src/api.rs` | 232 | прочитан 19.11 | нет |
| `crates/tgbot/src/checkwait.rs` | 259 | прочитан 19.11 | нет |
| `crates/tgbot/src/emoji.rs` | 116 | прочитан 19.11 | нет |
| `crates/tgbot/src/handler.rs` | 782 | прочитан 19.11 | З-19.2 |
| `crates/tgbot/src/intake.rs` | 342 | прочитан 19.11 | нет |
| `crates/tgbot/src/invest.rs` | 433 | прочитан 19.11 | К-19.2 |
| `crates/tgbot/src/lib.rs` | 551 | прочитан 19.11 | нет |
| `crates/tgbot/src/limits.rs` | 190 | прочитан 19.11 | нет |
| `crates/tgbot/src/locks.rs` | 90 | прочитан 19.11 | нет |
| `crates/tgbot/src/orders.rs` | 1324 | прочитан 19.11 (тесты — группа 5) | К-19.1, К-19.3 |
| `crates/tgbot/src/poll.rs` | 177 | прочитан 19.11 | нет |
| `crates/tgbot/src/promo.rs` | 91 | прочитан 19.11 | нет |
| `crates/tgbot/src/terms.rs` | 191 | прочитан 19.11 | Д-19.10 |
| `crates/ui/ru.toml` | 581 | прочитан 19.11 | Т-1 |
| `crates/ui/src/callback.rs` | 236 | — | |
| `crates/ui/src/command.rs` | 73 | — | |
| `crates/ui/src/doc.rs` | 280 | — | |
| `crates/ui/src/emoji.rs` | 785 | — | |
| `crates/ui/src/fmt.rs` | 302 | — | |
| `crates/ui/src/html.rs` | 234 | — | |
| `crates/ui/src/lib.rs` | 43 | — | |
| `crates/ui/src/model.rs` | 475 | — | |
| `crates/ui/src/preview.rs` | 360 | — | |
| `crates/ui/src/screen.rs` | 131 | — | |
| `crates/ui/src/screens/errors.rs` | 376 | — | |
| `crates/ui/src/screens/flow.rs` | 589 | — | |
| `crates/ui/src/screens/invest.rs` | 467 | — | |
| `crates/ui/src/screens/mod.rs` | 382 | — | |
| `crates/ui/src/screens/reference.rs` | 1346 | — | |
| `crates/ui/src/texts.rs` | 844 | — | |
| `crates/ui/src/widgets.rs` | 187 | — | |

## 4. Запуск, настройки, сборка, CI — 46 файлов, 12546 строк

| Файл | Строк | Статус | Находки |
|---|---|---|---|
| `.cargo/audit.toml` | 14 | — | |
| `.cargo/config.toml` | 6 | — | |
| `.env.example` | 26 | — | |
| `../.github/workflows/exchanger-ci.yml` | 178 | — | |
| `.gitignore` | 9 | прочитан 19.11 | нет |
| `Cargo.lock` | 4017 | — | |
| `Cargo.toml` | 50 | — | |
| `clippy.toml` | 7 | — | |
| `config.example.toml` | 40 | — | |
| `crates/app/Cargo.toml` | 66 | — | |
| `crates/app/src/backup.rs` | 695 | прочитан 19.11 (копия, проверка, восстановление, хранение) | Б-19.1 |
| `crates/app/src/check.rs` | 419 | — | |
| `crates/app/src/config.rs` | 266 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/app/src/db.rs` | 151 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/app/src/demo.rs` | 540 | — | |
| `crates/app/src/emoji.rs` | 533 | — | |
| `crates/app/src/health.rs` | 121 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/app/src/lib.rs` | 38 | — | |
| `crates/app/src/listen.rs` | 148 | — | |
| `crates/app/src/logfile.rs` | 242 | частично 19.11: хранение по дням | нет |
| `crates/app/src/login.rs` | 332 | частично 19.11: код и пароль не печатаются | нет |
| `crates/app/src/main.rs` | 1061 | частично 19.11: run(), задачи, пинг, сторож, копии | Э-19.1 |
| `crates/app/src/project.rs` | 266 | — | |
| `crates/app/src/prompt.rs` | 151 | — | |
| `crates/app/src/record.rs` | 225 | — | |
| `crates/app/src/runtime.rs` | 184 | прочитан 19.11 (подключение аккаунтов) | Э-19.1 |
| `crates/app/src/service.rs` | 276 | прочитан 19.11 (без встроенных тестов) | Б-19.2 |
| `crates/app/src/session_store.rs` | 98 | — | |
| `crates/app/src/settings_file.rs` | 107 | — | |
| `crates/app/src/setup.rs` | 735 | частично 19.11: секреты только в .env, .env.bak вне git | нет |
| `crates/app/src/startup.rs` | 251 | — | |
| `crates/app/src/stop.rs` | 165 | прочитан 19.11 (без встроенных тестов) | нет |
| `crates/app/src/supervisor.rs` | 357 | прочитан 19.11 (без встроенных тестов) | нет (запускает Ю-19.1) |
| `crates/app/src/telegram.rs` | 244 | — | |
| `crates/app/src/wallet_log.rs` | 223 | прочитан 19.11 | Р: правило 8 «по возможности» |
| `crates/app/src/watchdog.rs` | 159 | прочитан 19.11 (без встроенных тестов) | нет (запускает Ю-19.1) |
| `crates/botapi/Cargo.toml` | 33 | — | |
| `crates/domain/Cargo.toml` | 20 | — | |
| `crates/engine/Cargo.toml` | 32 | — | |
| `crates/mtproto/Cargo.toml` | 45 | — | |
| `crates/ops/Cargo.toml` | 44 | — | |
| `crates/parsers/Cargo.toml` | 26 | — | |
| `crates/storage/Cargo.toml` | 23 | — | |
| `crates/tgbot/Cargo.toml` | 36 | — | |
| `crates/ui/Cargo.toml` | 30 | — | |
| `crates/userbot/Cargo.toml` | 32 | — | |
| `rust-toolchain.toml` | 3 | — | |

## 5. Тесты и фикстуры — 187 файлов, 31302 строк

| Файл | Строк | Статус | Находки |
|---|---|---|---|
| `crates/app/tests/backup.rs` | 172 | — | |
| `crates/app/tests/health.rs` | 102 | — | |
| `crates/app/tests/session_store.rs` | 233 | — | |
| `crates/app/tests/startup.rs` | 78 | — | |
| `crates/app/tests/wallet_log.rs` | 289 | — | |
| `crates/app/tests/wallet_move.rs` | 112 | — | |
| `crates/botapi/tests/fixtures/telegram/delete_message.json` | 3 | — | |
| `crates/botapi/tests/fixtures/telegram/edit_message_text.json` | 72 | — | |
| `crates/botapi/tests/fixtures/telegram/get_me.json` | 18 | — | |
| `crates/botapi/tests/fixtures/telegram/send_message_protected_with_buttons.json` | 66 | — | |
| `crates/botapi/tests/fixtures/telegram/send_message_with_effect.json` | 20 | — | |
| `crates/botapi/tests/fixtures/telegram/send_rich_message.json` | 74 | — | |
| `crates/botapi/tests/fixtures/telegram/send_widget_not_allowed.json` | 4 | — | |
| `crates/botapi/tests/fixtures/telegram/set_message_reaction.json` | 3 | — | |
| `crates/botapi/tests/http.rs` | 565 | — | |
| `crates/botapi/tests/serialize.rs` | 484 | — | |
| `crates/engine/tests/admin.rs` | 1170 | — | |
| `crates/engine/tests/availability.rs` | 101 | — | |
| `crates/engine/tests/common/mod.rs` | 229 | — | |
| `crates/engine/tests/crash.rs` | 314 | — | |
| `crates/engine/tests/engine.rs` | 1141 | — | |
| `crates/engine/tests/invest.rs` | 477 | — | |
| `crates/engine/tests/invoice.rs` | 767 | — | |
| `crates/engine/tests/multi_check.rs` | 341 | — | |
| `crates/engine/tests/reconcile.rs` | 387 | — | |
| `crates/mtproto/tests/fake_login.rs` | 250 | — | |
| `crates/mtproto/tests/fake_transport.rs` | 592 | — | |
| `crates/mtproto/tests/support/fake_tg.rs` | 493 | — | |
| `crates/mtproto/tests/support/mod.rs` | 322 | — | |
| `crates/ops/tests/mockup_dash.rs` | 461 | — | |
| `crates/ops/tests/ops_bot.rs` | 2370 | — | |
| `crates/ops/tests/snapshots.rs` | 279 | — | |
| `crates/ops/tests/snapshots/snapshots__critical_account_restricted.snap` | 13 | — | |
| `crates/ops/tests/snapshots/snapshots__critical_engine_down.snap` | 11 | — | |
| `crates/ops/tests/snapshots/snapshots__critical_flood_long.snap` | 12 | — | |
| `crates/ops/tests/snapshots/snapshots__critical_kill_switch.snap` | 13 | — | |
| `crates/ops/tests/snapshots/snapshots__critical_login_lost.snap` | 13 | — | |
| `crates/ops/tests/snapshots/snapshots__critical_new_login.snap` | 12 | — | |
| `crates/ops/tests/snapshots/snapshots__critical_second_instance.snap` | 13 | — | |
| `crates/ops/tests/snapshots/snapshots__critical_unknown_reply.snap` | 13 | — | |
| `crates/ops/tests/snapshots/snapshots__dash_calm.snap` | 45 | — | |
| `crates/ops/tests/snapshots/snapshots__dash_fire.snap` | 62 | — | |
| `crates/ops/tests/snapshots/snapshots__dash_ok.snap` | 68 | — | |
| `crates/ops/tests/snapshots/snapshots__dash_pauses.snap` | 70 | — | |
| `crates/ops/tests/snapshots/snapshots__maintenance_since_idle.snap` | 12 | — | |
| `crates/ops/tests/snapshots/snapshots__maintenance_until.snap` | 12 | — | |
| `crates/ops/tests/snapshots/snapshots__menu_operator_kill_switch.snap` | 18 | — | |
| `crates/ops/tests/snapshots/snapshots__menu_owner.snap` | 17 | — | |
| `crates/ops/tests/snapshots/snapshots__order_card_hint_done.snap` | 78 | — | |
| `crates/ops/tests/snapshots/snapshots__order_card_hint_not.snap` | 79 | — | |
| `crates/ops/tests/snapshots/snapshots__order_card_hint_unclear.snap` | 79 | — | |
| `crates/ops/tests/snapshots/snapshots__order_card_operator.snap` | 77 | — | |
| `crates/ops/tests/snapshots/snapshots__order_card_operator_unknown.snap` | 77 | — | |
| `crates/ops/tests/snapshots/snapshots__order_card_owner.snap` | 77 | — | |
| `crates/ops/tests/snapshots/snapshots__queue.snap` | 48 | — | |
| `crates/ops/tests/snapshots/snapshots__queue_empty.snap` | 13 | — | |
| `crates/ops/tests/snapshots/snapshots__reconcile_stalled.snap` | 14 | — | |
| `crates/ops/tests/snapshots/snapshots__reconcile_stalled_selftest.snap` | 12 | — | |
| `crates/ops/tests/snapshots/snapshots__reserves.snap` | 62 | — | |
| `crates/ops/tests/snapshots/snapshots__review_alert_no_reply.snap` | 14 | — | |
| `crates/ops/tests/snapshots/snapshots__review_alert_reply.snap` | 14 | — | |
| `crates/ops/tests/snapshots/snapshots__shortfall.snap` | 14 | — | |
| `crates/ops/tests/snapshots/snapshots__shortfall_unsettled.snap` | 15 | — | |
| `crates/ops/tests/snapshots/snapshots__shortfall_unsettled_between.snap` | 15 | — | |
| `crates/ops/tests/snapshots/snapshots__surplus.snap` | 13 | — | |
| `crates/parsers/tests/fixtures.rs` | 450 | — | |
| `crates/parsers/tests/fixtures/activation.json` | 63 | — | |
| `crates/parsers/tests/fixtures/amounts.json` | 24 | — | |
| `crates/parsers/tests/fixtures/balances.json` | 35 | — | |
| `crates/parsers/tests/fixtures/bare_codes.json` | 16 | — | |
| `crates/parsers/tests/fixtures/created_checks.json` | 59 | — | |
| `crates/parsers/tests/fixtures/fiat.json` | 11 | — | |
| `crates/parsers/tests/fixtures/invoice_cards.json` | 70 | — | |
| `crates/parsers/tests/fixtures/invoice_payments.json` | 25 | — | |
| `crates/parsers/tests/fixtures/links.json` | 63 | — | |
| `crates/parsers/tests/fixtures/live_wallet_replies.json` | 3412 | — | |
| `crates/parsers/tests/fixtures/messages.json` | 61 | — | |
| `crates/parsers/tests/live.rs` | 238 | — | |
| `crates/parsers/tests/prop.proptest-regressions` | 8 | — | |
| `crates/parsers/tests/prop.rs` | 406 | — | |
| `crates/storage/tests/alerts.rs` | 101 | — | |
| `crates/storage/tests/client.rs` | 291 | — | |
| `crates/storage/tests/common/mod.rs` | 82 | — | |
| `crates/storage/tests/invariants.rs` | 426 | — | |
| `crates/storage/tests/orders.rs` | 298 | — | |
| `crates/storage/tests/outbox.rs` | 444 | — | |
| `crates/storage/tests/promo.rs` | 104 | — | |
| `crates/storage/tests/schema.rs` | 79 | — | |
| `crates/storage/tests/userbots.rs` | 404 | — | |
| `crates/tgbot/tests/client_bot.rs` | 2171 | — | |
| `crates/ui/tests/common/mod.rs` | 919 | — | |
| `crates/ui/tests/errors.rs` | 39 | — | |
| `crates/ui/tests/html_valid.proptest-regressions` | 7 | — | |
| `crates/ui/tests/html_valid.rs` | 470 | — | |
| `crates/ui/tests/menu.rs` | 120 | — | |
| `crates/ui/tests/mockup.rs` | 548 | — | |
| `crates/ui/tests/snapshots.rs` | 139 | — | |
| `crates/ui/tests/snapshots/snapshots__cancelled_flow.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__cancelled_order.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__check_link_missing.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__check_link_pending.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__check_link_waiting.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__closed_by_operator_kept.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__closed_by_operator_paid.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__errors.snap` | 747 | — | |
| `crates/ui/tests/snapshots/snapshots__exchange_menu.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__exchange_menu_paused.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__exchange_menu_with_offers.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__help.snap` | 83 | — | |
| `crates/ui/tests/snapshots/snapshots__history.snap` | 55 | — | |
| `crates/ui/tests/snapshots/snapshots__history_empty.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__history_links_page_2.snap` | 49 | — | |
| `crates/ui/tests/snapshots/snapshots__howto_cryptobot.snap` | 39 | — | |
| `crates/ui/tests/snapshots/snapshots__howto_xrocket.snap` | 39 | — | |
| `crates/ui/tests/snapshots/snapshots__inline_cards.snap` | 55 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_deposit.snap` | 21 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_home.snap` | 41 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_home_losses.snap` | 43 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_home_new.snap` | 31 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_howto.snap` | 139 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_howto_new.snap` | 135 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_deposit_accepted.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_deposit_done.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_deposit_manual.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_deposit_rejected.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_withdraw_done.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_withdraw_failed.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_withdraw_manual.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_withdraw_processing.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_withdraw_rejected.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_notice_withdraw_requested.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_problem_bad_amount.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_problem_internal.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_problem_not_a_check.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_problem_open.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_problem_over.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_problem_wallet.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_withdraw.snap` | 21 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_withdraw_amount.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__invest_withdraw_nothing.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_awaiting.snap` | 43 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_check_rejected.snap` | 35 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_paid.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_paid_excess_check.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_paid_excess_kept.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_paid_excess_refund.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_paying.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_prompt.snap` | 41 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_quote.snap` | 47 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_quote_fiat.snap` | 47 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_soon.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__invoice_underpaid.snap` | 39 | — | |
| `crates/ui/tests/snapshots/snapshots__manual_review.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__manual_review_unknown.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__menu_and_commands.snap` | 45 | — | |
| `crates/ui/tests/snapshots/snapshots__no_link.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__operator_answer.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__order_card.snap` | 35 | — | |
| `crates/ui/tests/snapshots/snapshots__order_card_with_check.snap` | 45 | — | |
| `crates/ui/tests/snapshots/snapshots__order_checking.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__order_checking_amount.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__order_done.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__order_done_details.snap` | 21 | — | |
| `crates/ui/tests/snapshots/snapshots__order_sending.snap` | 13 | — | |
| `crates/ui/tests/snapshots/snapshots__order_sending_slow.snap` | 15 | — | |
| `crates/ui/tests/snapshots/snapshots__other_coin_review.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__privacy.snap` | 49 | — | |
| `crates/ui/tests/snapshots/snapshots__promo_broadcast.snap` | 23 | — | |
| `crates/ui/tests/snapshots/snapshots__refund_done.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__refund_done_below_minimum.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__refund_started.snap` | 120 | — | |
| `crates/ui/tests/snapshots/snapshots__result_below.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__send_check_cb_to_xr.snap` | 39 | — | |
| `crates/ui/tests/snapshots/snapshots__send_check_paused.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__send_check_xr_to_cb.snap` | 39 | — | |
| `crates/ui/tests/snapshots/snapshots__support_with_contact.snap` | 19 | — | |
| `crates/ui/tests/snapshots/snapshots__support_without_contact.snap` | 17 | — | |
| `crates/ui/tests/snapshots/snapshots__terms.snap` | 49 | — | |
| `crates/ui/tests/snapshots/snapshots__terms_partly_paused.snap` | 49 | — | |
| `crates/ui/tests/snapshots/snapshots__terms_with_offers.snap` | 69 | — | |
| `crates/ui/tests/snapshots/snapshots__welcome.snap` | 47 | — | |
| `crates/ui/tests/snapshots/snapshots__welcome_all_paused.snap` | 51 | — | |
| `crates/ui/tests/snapshots/snapshots__welcome_with_offers.snap` | 51 | — | |
| `crates/ui/tests/style_guard.rs` | 96 | — | |
| `crates/ui/tests/widgets.rs` | 91 | — | |
| `crates/userbot/tests/flows.rs` | 1672 | — | |
| `crates/userbot/tests/sim.rs` | 1473 | — | |

## 6. Виджеты, макеты, инструменты, диаграммы — 65 файлов, 17728 строк

| Файл | Строк | Статус | Находки |
|---|---|---|---|
| `assets/fonts/LICENSE-DejaVu.txt` | 78 | — | |
| `assets/widgets/accounts.html` | 32 | — | |
| `assets/widgets/calc_a.html` | 63 | — | |
| `assets/widgets/calc_b.html` | 57 | — | |
| `assets/widgets/code.html` | 34 | — | |
| `assets/widgets/dash.html` | 62 | — | |
| `assets/widgets/inv_calc.html` | 39 | — | |
| `assets/widgets/inv_home.html` | 39 | — | |
| `assets/widgets/inv_withdraw.html` | 44 | — | |
| `assets/widgets/invoice.html` | 44 | — | |
| `assets/widgets/maint.html` | 38 | — | |
| `assets/widgets/order_a.html` | 46 | — | |
| `assets/widgets/process.html` | 55 | — | |
| `assets/widgets/promo.html` | 30 | — | |
| `assets/widgets/queue.html` | 34 | — | |
| `assets/widgets/rebalance.html` | 49 | — | |
| `assets/widgets/reserves.html` | 47 | — | |
| `assets/widgets/system.html` | 33 | — | |
| `assets/widgets/value.html` | 53 | — | |
| `assets/widgets/withdraw.html` | 48 | — | |
| `diagrams/01-use-cases.puml` | 89 | — | |
| `diagrams/02-activity-check-exchange.puml` | 141 | — | |
| `diagrams/03-activity-invoice-payment.puml` | 108 | — | |
| `diagrams/04-activity-rebalance.puml` | 63 | — | |
| `diagrams/05-activity-recovery-reconcile.puml` | 84 | — | |
| `diagrams/06-sequence-check-exchange.puml` | 58 | — | |
| `diagrams/07-sequence-invoice-payment.puml` | 67 | — | |
| `diagrams/08-state-order.puml` | 76 | — | |
| `diagrams/09-components.puml` | 50 | — | |
| `diagrams/10-deployment.puml` | 36 | — | |
| `diagrams/11-er.puml` | 193 | — | |
| `docs/mockups/build.py` | 37 | — | |
| `docs/mockups/logo/README.md` | 37 | — | |
| `docs/mockups/logo/avatar-render.js` | 13 | — | |
| `docs/mockups/logo/avatar.html` | 23 | — | |
| `docs/mockups/logo/logo.html` | 70 | — | |
| `docs/mockups/logo/render.js` | 9 | — | |
| `docs/mockups/template.html` | 790 | — | |
| `docs/mockups/widgets/GAPS.md` | 78 | — | |
| `docs/mockups/widgets/RECHECK-14-11.md` | 121 | — | |
| `docs/mockups/widgets/analysis.json` | 515 | — | |
| `docs/mockups/widgets/build.py` | 106 | — | |
| `docs/mockups/widgets/console-login.js` | 7 | — | |
| `docs/mockups/widgets/console-p234.html` | 21 | — | |
| `docs/mockups/widgets/console-p8-p10.html` | 27 | — | |
| `docs/mockups/widgets/gaps_samples.py` | 577 | — | |
| `docs/mockups/widgets/gen_samples.py` | 244 | — | |
| `docs/mockups/widgets/preview.html` | 755 | — | |
| `docs/mockups/widgets/samples.json` | 8955 | — | |
| `tools/emoji/.gitignore` | 2 | — | |
| `tools/emoji/README.md` | 90 | — | |
| `tools/emoji/check.js` | 47 | — | |
| `tools/emoji/demo-packs.json` | 241 | — | |
| `tools/emoji/frames.js` | 43 | — | |
| `tools/emoji/icons.js` | 701 | — | |
| `tools/emoji/package-lock.json` | 536 | — | |
| `tools/emoji/package.json` | 9 | — | |
| `tools/emoji/preview.py` | 99 | — | |
| `tools/emoji/static.js` | 84 | — | |
| `tools/emoji/tgs.js` | 1107 | — | |
| `tools/emoji/verify_tgs.py` | 153 | — | |
| `tools/recording/README.md` | 20 | — | |
| `tools/recording/live_fixtures.py` | 146 | — | |
| `tools/recording/sanitize.py` | 113 | — | |
| `tools/telegram/record.py` | 62 | — | |

## 7. Документы — 26 файлов, 7689 строк

| Файл | Строк | Статус | Находки |
|---|---|---|---|
| `CLAUDE.md` | 130 | — | |
| `README.md` | 72 | — | |
| `SPEC.md` | 1878 | — | |
| `docs/AUDIT-2026-11-11.md` | 278 | — | |
| `docs/AUDIT-METHOD.md` | 284 | — | |
| `docs/AUDIT-UI.md` | 130 | — | |
| `docs/BOTAPI-RICH.md` | 41 | — | |
| `docs/BOTAPI-WIDGETS.md` | 145 | — | |
| `docs/DEPLOY.md` | 104 | — | |
| `docs/DESIGN-v0.2.md` | 112 | — | |
| `docs/HANDOVER.md` | 327 | — | |
| `docs/INVEST-LOSSES.md` | 46 | — | |
| `docs/LAPTOP.md` | 83 | — | |
| `docs/LOVEC-PORTING.md` | 726 | — | |
| `docs/NEXT.md` | 258 | — | |
| `docs/OWNER-BRIEF.md` | 105 | — | |
| `docs/PLAN-v1.md` | 263 | — | |
| `docs/PLAN-v2.md` | 218 | — | |
| `docs/PROGRESS.md` | 663 | — | |
| `docs/PROPOSALS-16-11.md` | 131 | — | |
| `docs/RECORDING.md` | 163 | — | |
| `docs/RUNBOOK.md` | 138 | — | |
| `docs/STATUS-18-11.md` | 202 | — | |
| `docs/audit-16-11/client.md` | 243 | — | |
| `docs/audit-16-11/ops.md` | 599 | — | |
| `docs/audit-16-11/platform.md` | 350 | — | |

## 8. Прочее — 2 файлов, 413 строк

| Файл | Строк | Статус | Находки |
|---|---|---|---|
| `crates/domain/proptest-regressions/reconcile.txt` | 9 | — | |
| `docs/launch-checklist.json` | 404 | — | |
