# CLAUDE.md — бот-обменник xRocket ⇄ CryptoBot (exch)

Корень проекта — каталог `exchanger/`. Начни с `docs/HANDOVER.md`: там состояние, порядок работ и критерии приёмки.

Источники правды, по старшинству:
1. `docs/DESIGN-v0.2.md` — решения владельца (только юзербот, основной поток CryptoBot → xRocket, без паролей).
2. Этот файл.
3. `SPEC.md` — v0.1. Где он говорит про API кошельков, `startBot`, пароли чеков или «основной поток XR → CB», он устарел (HANDOVER §3).

Если код и эти документы расходятся — остановись и спроси, не «чини» документ молча.

## Что это

Telegram-бот на Rust. Клиент присылает чек CryptoBot или xRocket (или ссылку на счёт), **юзербот** (обычный аккаунт Telegram, MTProto) активирует чек, а выплату делает **чеком другого кошелька, который сам же создаёт** (инлайн `@send 10usdt` / `@xrocket 10`, запасной путь — меню `/checks` / `/cheques`). Счета оплачивает тоже юзербот. API Crypto Pay и xRocket Pay **не используем**. Деньги клиентов — в леджере с двойной записью.

## Команды

```bash
# локальная БД (Linux/macOS/Windows — Docker)
docker run -d --name exch-pg -e POSTGRES_PASSWORD=dev -p 5432:5432 postgres:16
export DATABASE_URL=postgres://postgres:dev@localhost:5432/exch   # PowerShell: $env:DATABASE_URL="…"
cargo install sqlx-cli --version 0.9.0 --locked --no-default-features --features postgres,rustls   # один раз

cargo sqlx database create && cargo sqlx migrate run --source migrations
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features      # нужен DATABASE_URL; sqlx::test создаёт БД на тест
cargo sqlx prepare --workspace -- --all-targets   # после изменения SQL-запросов; коммитить .sqlx/
cargo run --bin exch -- --help             # CLI: run | migrate | check-config (login, selftest — впереди)
```

`--all-features` обязателен: тесты симулятора кошельков — за feature `sim`.
Сборка без БД: `SQLX_OFFLINE=true cargo build` (данные запросов — в `.sqlx/`). CI — `../.github/workflows/exchanger-ci.yml`.

Перед **каждым** коммитом все четыре: fmt, clippy без предупреждений, test, `cargo sqlx prepare --workspace --check -- --all-targets`.

## Карта крейтов

```
domain ─┬─ parsers ── userbot (Transport, WalletBot, flows, sim) ── mtproto (grammers 0.10)
        ├─ storage (PostgreSQL, миграции)
        └─ botapi (Bot API 10.3) ── ui (экраны, клавиатуры)
engine (FSM-драйвер, outbox, сверка) ← userbot + storage
tgbot (клиентский бот) ← ui + botapi + engine;  ops (ops-бот) ← botapi + engine
app (бинарник exch) ← всё
```

Есть: `domain`, `storage`, `app`, `parsers`, `userbot` (без `flows`), `mtproto`, `botapi`, `ui`. Нет: `userbot::flows`, `engine`, `tgbot`, `ops`. Пустые крейты заранее не создаём.
В `domain` нет tokio, sqlx, reqwest. `userbot` и `engine` не знают про grammers — только трейт `Transport`.

## Правила, которые нельзя нарушать

### Деньги
1. Только `rust_decimal::Decimal`. `f32`/`f64` запрещены (`clippy.toml: disallowed-types`). В БД — `NUMERIC(38,18)`, в JSON — строки.
2. Округление: выплата — вниз до `payout_step`; сумма к оплате — вверх; остаток округления — в пользу сервиса. Формулы — `domain::pricing`, не изобретать свои.
3. Любое движение денег = сбалансированная проводка в леджере (`ledger_transactions` + `ledger_entries`, сумма по активу = 0; проверяет отложенный триггер). Комиссия признаётся в момент выплаты. Леджер только дописывается.

### Двойные выплаты и потерянные чеки
4. Денежное действие = строка в `operations` (outbox), вставленная **в той же транзакции**, что и переход заявки. Действие юзербота — только после коммита `status = 'dispatched'`.
5. Ключ операции = `operations.idempotency_key`, формат `ord-<id>-payout|refund|excess|pay|intake-<n>`. Каждой операции юзербота **заранее** назначается `random_id` Telegram и сохраняется в `operations.request`. Повтор отправки внутри попытки — только с **тем же** `random_id` (Telegram не создаст второе сообщение: `RANDOM_ID_DUPLICATE`).
6. Операции, которые выводят деньги (`create_payout_check`, `create_refund_check`, `create_excess_check`, `pay_invoice`, `withdrawal`), — `max_attempts = 1` (CHECK в схеме). Неясный исход (`IssueOutcome::Unknown`, `PayOutcome::Unknown`) → сверка (`find_issued_check`, статус счёта, баланс) или `MANUAL_REVIEW`, **никогда** повторное создание чека или нажатие «Оплатить». Повторять можно только `activate_check`.
7. Переходы заявки — только через `domain::fsm::transition`. Таблица в коде (`ALLOWED`, 31 переход) = `order_state_transitions` в БД (это проверяет тест). Не добавляй переход в одном месте без другого и без обновления `diagrams/08-state-order.puml`.
8. Каждое сообщение от бота кошелька сохраняется в `wallet_messages` **до** разбора.
9. Не ослабляй уникальные индексы и CHECK в миграциях (`operations_one_settle_per_order`, `orders_intake_param_live`, `operations_check_once`, `operations_invoice_pay_once`, `ledger_tx_once_per_operation`, CHECK `max_attempts = 1` и т. д.).
10. Не принимаем заявку, если не можем её выплатить: резерв целевого кошелька проверяется до приёма (`domain::pricing::max_order_effective`, `classify_funding`).

### Юзербот
11. Писать только ботам кошельков по **закреплённым peer ID** (`mtproto::config`: CryptoBot `1559501630` `@send`, xRocket `5014831088` `@xrocket`) и в «Избранное». Никогда не резолвить username из ссылки клиента. Не подписываться на каналы, не писать людям. Аккаунты юзерботов — выделенные, руками ими не пользуются.
12. Одно денежное действие в полёте на аккаунт; не чаще 1 действия в 1,5 с; FLOOD_WAIT — пауза всего аккаунта.
13. Незнакомый ответ кошелька → `Unknown` → автопауза направления + CRITICAL. Не подгоняй словарь под один текст: добавь фикстуру в `crates/parsers/tests/fixtures/*.json` и правило, которое не пересекается с другими классами.
14. Чеки с паролем не принимаем (отказ, деньги остались в чеке). Логики паролей в коде нет и не будет.

### Секреты и логи
15. Токены ботов, `TG_API_HASH`, сессии, `SESSION_KEY`, `CRYPTOBOT_PIN` — только из переменных окружения. Тип `secrecy::SecretString`. Не попадают в логи, ошибки, БД (кроме зашифрованной сессии), коммиты, тестовые снимки и промты.
16. `start_param` чеков, ссылки на чеки и телефоны в логах — маскировать.
17. `.env` никогда не коммитится; новые переменные — в `.env.example` без значений и в `app::config`.

### Интерфейс
18. Тексты — в `crates/ui/ru.toml`, не в коде. Telegram HTML: только разрешённые теги, все подстановки экранированы (тест `ui/tests/html_valid.rs`). Денежный поток — обычные HTML-сообщения; rich — только справочные экраны, у каждого есть HTML-фолбэк.
19. Каждая ошибка говорит, где деньги клиента и что делать.

## Ошибки и стиль

- В библиотечных крейтах — `thiserror`, в `app` — `anyhow`. Не `unwrap()`/`expect()` вне тестов и инициализации (там — с понятным сообщением).
- Async: не блокировать рантайм (тяжёлое — `spawn_blocking`). Не держать транзакцию БД во время действия юзербота.
- Логи — `tracing` со спанами `order_id`, `op_id`, `account`. Уровни: `error` = нужна реакция человека.
- Время — UTC (`chrono::DateTime<Utc>`), в UI — часовой пояс владельца или клиента.
- Новые зависимости — только с обоснованием в коммите. grammers закреплён `=0.10.0`.

## Тесты

- Не удаляй и не ослабляй тесты, чтобы они прошли. Если тест или требование неверны — остановись и напиши, почему.
- Денежная математика — `proptest`; FSM — табличные тесты; парсеры — JSON-фикстуры; экраны — `insta`-снимки; сценарии юзербота и движка — `userbot::sim` + реальный PostgreSQL (`sqlx::test`).
- Любой новый сценарий с деньгами — тест на сбой «выполнил и вернул таймаут» и проверка: ≤ 1 созданный чек / оплата на операцию, леджер сбалансирован, балансы симулятора = леджер.

## Как работать

1. Прочитай `docs/HANDOVER.md`, `docs/DESIGN-v0.2.md`, этот файл, `docs/PROGRESS.md`, `git log --oneline -30`.
2. Прогони проверки — должны быть зелёными до начала работы.
3. Делай маленькими шагами; каждый законченный шаг — коммит с понятным сообщением (`userbot: issue check via inline mode with menu fallback`).
4. Не трогай то, что не относится к задаче.
5. В конце: обнови `docs/PROGRESS.md` (сделано / дальше / известные проблемы).

## Где что искать

| Вопрос | Где |
|---|---|
| Состояние проекта, что осталось, порядок работ | `docs/HANDOVER.md` |
| Решения владельца v0.2 | `docs/DESIGN-v0.2.md` |
| Как работает ловец lovec: активация, ответы ботов, FLOOD, grammers | `docs/LOVEC-PORTING.md` |
| Форматы ссылок, словари ответов | `crates/parsers/src/links.rs`, `replies.rs`, фикстуры |
| Состояния заявки | `crates/domain/src/fsm.rs`, `schema.sql` (`order_state_transitions`) |
| Проводки | `crates/domain/src/ledger.rs` |
| Цены, рычаг резерва | `crates/domain/src/pricing.rs`, `schema.sql` (`directions`) |
| Экраны клиента | `crates/ui/src/screens/`, снимки `crates/ui/tests/snapshots/` |
