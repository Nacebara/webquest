# Бот-обменник xRocket ⇄ CryptoBot — проектный пакет

**Начните с [`docs/HANDOVER.md`](docs/HANDOVER.md)** — состояние проекта, порядок работ и критерии приёмки.

| Файл | Что внутри |
|---|---|
| [`docs/HANDOVER.md`](docs/HANDOVER.md) | Передача проекта: что готово, что осталось, как запустить |
| [`docs/DESIGN-v0.2.md`](docs/DESIGN-v0.2.md) | Решения владельца v0.2 (только юзербот, основной поток CryptoBot → xRocket) — главнее SPEC |
| [`docs/LOVEC-PORTING.md`](docs/LOVEC-PORTING.md) | Что берём из ловца lovec 0.4.3: активация, ответы ботов, FLOOD, grammers |
| [`SPEC.md`](SPEC.md) | Спецификация: сводка, принятые допущения, разделы 1–15 (платформы, экономика, ликвидность, процессы, UML, интерфейс, админка, логи, статистика, архитектура на Rust, безопасность и тесты, деплой, риски, план, открытые вопросы). Версия v0.2 (10 октября 2026) |
| [`schema.sql`](schema.sql) | Схема PostgreSQL 16 (миграция `0001_init`) с инвариантами против двойных выплат |
| [`schema_test.sql`](schema_test.sql) | Проверка инвариантов схемы на живой БД |
| [`diagrams/`](diagrams/) | PlantUML-исходники и PNG в `diagrams/png/` |
| [`CLAUDE.md`](CLAUDE.md) | Правила разработки в Claude Code |
| [`docs/PROGRESS.md`](docs/PROGRESS.md) | Что сделано по вехам, что дальше, известные проблемы |
| `crates/`, `migrations/`, `Cargo.toml` | Код: `domain` (деньги, цены, FSM, проводки), `storage` (PostgreSQL), `parsers` (ссылки и ответы ботов кошельков), `userbot` (контракты и симулятор кошельков), `mtproto` (grammers), `botapi` (Bot API 10.3), `ui` (экраны клиента), `engine` (движок заявок), `app` (бинарник `exch`) |

Готово: фундамент M1, ядро v0.2, парсеры, симулятор, MTProto-транспорт, сценарии юзербота, Bot API клиент, интерфейс, движок обмена чеков, SPEC v0.2. Дальше — вход юзербота и запуск, клиентский бот, оплата счетов, ops-бот (`docs/HANDOVER.md` §5).
