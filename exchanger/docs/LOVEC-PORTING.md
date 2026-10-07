# Перенос lovec 0.4.3 → exch

Сводный план по пяти отчётам о подсистемах lovec (workflow understand-lovec, 7 октября 2026). Цитаты и ссылки file:line — на исходники lovec 0.4.3, присланные владельцем.

# lovec → exch: план переноса юзербота

## Источники и обозначения

- Пять отчётов по подсистемам lovec: mtproto, activation, ui, lessons. Отчёт parsing не получен: классификатор безопасности удержал его содержимое.
- Чтобы закрыть этот пробел, я сам прочитал `src/replies.rs`, `src/bots.rs`, `src/parser.rs`, `src/links.rs` (частично), `src/ingest.rs` (частично), `src/password.rs:280-380`, `src/claimer.rs:355-405`, `src/fastsend.rs:160-199`, а также выдержки из `AUDIT-2.md`.
- API grammers 0.10 проверено по исходникам crates.io в `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`. Сокращения путей:
  - `gc:` — `grammers-client-0.10.0/src`;
  - `gs:` — `grammers-session-0.10.0/src`;
  - `gm:` — `grammers-mtsender-0.10.0/src`;
  - `tl:` — `grammers-tl-types-0.10.0/tl/api.tl`.
- Пути lovec даны от корня `lovec/`.
- Статусы фактов о кошельках:
  - **live** — подтверждено боевыми логами lovec;
  - **needle** — подстрока классификатора, а не захваченный текст;
  - **fixture** — строка из теста lovec, не захваченная из реального чата;
  - **doc** — документация Telegram, на аккаунте не проверено.

## 0. Блокеры: решить до M3

| # | Блокер | Факт |
|---|---|---|
| B1 | SPEC и CLAUDE.md противоречат новой модели «всё делает юзербот, API кошельков нет» | `CLAUDE.md`: «бот принимает деньги юзерботом (MTProto) и выплачивает через API кошелька». Правило 5 строит ключи из `spend_id` / `transferId` / `withdrawalId`, то есть из API. `SPEC.md:148`: мини-приложение «**отвергнуто**: нарушение правил и хрупкость». `SPEC.md:181`: «Автоматизацию PIN не делаем». `SPEC.md:147`: инлайн-чеки — «запасной путь выплат». `SPEC.md:56-57, 65-66`: `startBot`. CLAUDE.md требует: «Если код и SPEC расходятся — остановись и спроси». Владелец должен решить, что из этого отменяется, и SPEC нужно переписать **до** кода |
| B2 | Ни одного реального текста ответа кошелька нет | Все строки ниже — needle или fixture. lovec никогда не разбирал сумму из ответа о выигрыше. Нужен режим записи (§8.7) |
| B3 | В grammers 0.10.0 нет исправлений живучести из форка | `gm:sender.rs:49` `PING_DELAY = 60 s`; подключение без таймаута в `gm:sender_pool.rs:246`; `panic!` в `gm:sender.rs:217, 551, 617`; `resume_unwind` в `gm:sender_pool.rs:213`. Подробно — §5 |
| B4 | Оплата счёта через мини-приложение с PIN средствами MTProto невозможна | MTProto доводит только до URL web view (`tl:2593` `messages.requestWebView` → `tl:1546` `webViewResultUrl`). PIN вводится внутри веб-страницы. Нужен headless-браузер или обратная разработка бэкенда (§8.4) |

---

## 1. Что переносим как есть

### 1.1 Переносим: код или паттерн

| lovec (file:lines, symbols) | → exch crate/module | Адаптация |
|---|---|---|
| `src/bots.rs:65-86` `MARKERS` (`CQ`/12, `t_`/17, `mci_`/19, `mc_`/18); `:88` `MIN_PREFIX_LEN=2`; `:97-129` `is_prefix`, `markers_are_sound`, `const _: () = assert!(markers_are_sound());` | `parsers::links::markers` | Как есть, с compile-time assert. Добавить kind для счетов (`IV…`, `inv_…`) и явный «отказ» для `mc_`+10 (только веб-приложение) |
| `src/links.rs:91-139` `scan()`; `:141-159` `is_testnet_link`; `:163-189` `resolve_names_testnet`/`domain_after`; `:194-208` `link_tail`; `:218-220` `is_host_end` (`.me`, `.dog`, `:/`); `:225-229` `link_bot`; тесты `:236-327`; `tests/parsers_prop.rs` | `parsers::links` | Логику переносим, микрооптимизации не нужны: можно обернуть в `url::Url`. **Добавить** то, чего в lovec нет: (1) whitelist username из ссылки (AUDIT.md:72-80: lovec отправил `t.me/AnyBot?start=t_…` в xRocket). Алиасы: «@CryptoBot/@send, @tonRocketBot/@xrocket» (AUDIT.md:77). Username мапится на **закреплённый peer id** и никогда не резолвится (правило 10). (2) Формы `https://<bot>.t.me/?start=` и `tg:resolve` без `//` (SPEC §1.2) |
| `src/code.rs:3` `MAX_CODE_LEN: usize = 32`; `:6-8` `is_code_char` (`[A-Za-z0-9_-]`); `:14-66` `Code` | `domain::StartParam` (или `parsers`) | Display и Debug заменить на `Masked<T>` (правило 14). Лимит поднять до 64 |
| `src/replies.rs:1-121` `Reply`, `is_terminal`, таблицы needle, `classify` (порядок важен); тесты `:128-162` | `parsers::replies::{cryptobot,xrocket}` | Needle — только затравка. Порядок приоритетов закрепить тестами. Добавить: (a) сумму и актив из Won (Decimal); (b) отличать входящий перевод «… от @x» от выигрыша; (c) `Notice` = «наш чек активировали» (подтверждение выплаты); (d) `Unknown` → автопауза направления + CRITICAL (правило 12) |
| `src/parser.rs:16-42` `find_in` (text, `MessageEntity::TextUrl`, `ReplyInlineMarkup`); `:208-217` `button_url` (`Url`/`UrlAuth`/`WebView`/`SimpleWebView`) | `parsers::buttons` | Как есть, плюс разбор всех типов кнопок, включая `Callback{data}`, `SwitchInline`, `Copy{copy_text}` (`tl:703, 706, 718`). Ссылка на чек из инлайн-результата берётся именно отсюда |
| `src/parser.rs:52-117` `MsgView::{from_message, from_short, from_short_chat}`; `:178-202` `message_update` (`NewMessage`/`EditMessage`/`NewChannelMessage`/`EditChannelMessage`) | `userbot::transport::raw` → собственный `RawMessage` | Нужны только `NewMessage` и `EditMessage` (личка с ботом и «Избранное»). Важно: `from_short` ставит `markup: None` (`parser.rs:94`), потому что в `UpdateShortMessage` нет `reply_markup` |
| `src/parser.rs:219-224` `looks_like_cheque` (`STRONG`: `"Чек на", "чек на", "Создание чека", "Cheque"`; `WEAK`: `"🚀", "🦋"` + сумма) | `parsers::inline` (признак заглушки инлайн-чека) | Использовать как признак, что инлайн-сообщение ещё без ссылки и надо ждать правки (§8.2) |
| `src/amount.rs:8-13, 24-48, 92-209` (`parse_amount_usd`, `number_after/before`, `number_value`, `grouped`) | `parsers::amount` | **Переписать на `rust_decimal`**: f64 запрещён (правило 1). Привязка к тикеру актива, а не только к `$`/`USD`. Сохранить: NBSP U+00A0 и U+202F как разделители тысяч; десятичная запятая — только при ≤2 знаках после неё; «Unclear», а не догадка |
| `src/fastsend.rs:174-199` `sent_message_id(body, random_id)` | `userbot::outcome` | Логику переносим как есть. Брать из уже десериализованного `tl::enums::Updates`: `UpdateShortSentMessage.id` → `Update::MessageId` с совпавшим `random_id` → первое `out` `NewMessage`. **Обязательно расширить:** для `sendInlineBotResult` в «Избранное» вернуть и само `Message` (нужны `reply_markup` и `via_bot_id`) |
| `src/fastsend.rs:128-161` `SendError::{flood_wait, is_io, is_timeout, is_rpc}` | `userbot::outcome` | Заменить трёхзначной классификацией из `AUDIT-2.md:244-256` (`Delivered`, `Rejected`, `Unknown`), см. §3.3 |
| `src/flood.rs:10-124` (`FLOOD_NAMES` `["FLOOD_WAIT", "FLOOD_PREMIUM_WAIT"]`, `AUTH_NAMES` `AUTH_KEY_UNREGISTERED, SESSION_REVOKED, SESSION_EXPIRED, USER_DEACTIVATED, AUTH_KEY_DUPLICATED`, `clamp_wait` 1 s..900 s, по умолчанию 60 s, `FloodGate`) | `userbot::flood` | Почти дословно. Добавить `PEER_FLOOD` → статус аккаунта `restricted` + CRITICAL (AUDIT.md:90-93, 102-105). Дедлайн паузы хранить в БД: перезапуск паузу не снимает (AUDIT-2.md:1787-1791). Имена ошибок сравнивать точно, не по префиксу (AUDIT-2.md:1055-1063) |
| `src/poller.rs:26-60` `RateLimiter::{acquire, penalize}` | `userbot::governor` | Интервал 1,5 s вместо 100 ms (`account.rs:44`), плюс бюджет в минуту (§6, F1) |
| `src/poller.rs:17-20` `PRIORITY_STEPS_MS = [0,100,200,300,500,700,1000,1500,2000,3000,4000,6000,8000,11_000,15_000]`; `TEXT_STEPS_MS`; `:22` `REQUEST_TIMEOUT=2s`; `:229-241` `build_request` (`messages::GetMessages`) | `userbot::recheck` | Паттерн «перезапросить сообщение по графику, пока не появится ссылка». Нужен для инлайн-заглушки в «Избранном» и для `getHistory` после `/start` без ответа. Шаги реже (≥1,5 s, у всех один governor) |
| `src/dialogs.rs:9-97` (`CLAIM_WINDOW=8s`, `Route`, `begin_claim`, `route`, `attach_session`, `detach_session`) | `userbot::conversation` | Идея «многошаговая сессия владеет чатом бота» через `mpsc::Sender`. В exch: одно денежное действие на аккаунт (правило 11), поэтому FIFO-угадывания нет |
| `src/password.rs:284-373` `restore` → `send` → `await_verdict`; `REPLY_TIMEOUT=6s`, `GUESS_DELAY=400ms`, `MAX_PAUSE=20s`, `MAX_RETRIES_PER_GUESS=3` (`:20-24`) | `userbot::flows::password` | Пароль даёт клиент, LLM-решатель не нужен. `send` через сырой `SendMessage` с сохранённым `random_id`, а не через `client.send_message`. Пароль не логировать (правило 13) |
| `src/claimer.rs:116-138` `SeenReplies` (dedup терминального класса по `(bot, msg_id, class)`, 64 записи) | `userbot::conversation` | Заменить уникальностью в БД `wallet_messages(peer, msg_id, edit_date)` |
| `src/account.rs:111-202` `connect()`; `:54-62` `client_with()`; `:204-219` `spawn_runner()`; `CONNECT_TIMEOUT` `:50` | `userbot::conn` | Три клиента на один пул (см. §5.4). Таймауты 30 s вокруг `is_authorized`/`get_me` сохранить |
| `src/account.rs:304-332` `run_listener` (backoff от 100 ms до 5 s) | `userbot::actor::recv_loop` | `Dropped` не означает «пул умер». Повтор с отступом, после N подряд — пересоздать пул (AUDIT-2.md:300-306) |
| `src/account.rs:849-922` `spawn_watchdog` (`Ping` каждые 10 s с таймаутом 5 s, `updates::GetState` каждые 60 s, оповещение после 3 сбоев) | `userbot::watchdog` | Добавить пересоздание `SenderPool` после N сбоев: в 0.10 нет pong-таймаута и таймаута подключения (§5) |
| `src/account.rs:990-1030, 1074-1083` `resolve_bots`, `resolve_bot` | `userbot::peers` | Без `.bots.json`. `session.peer_ref(PeerId::user(PINNED)?)`; при промахе один раз `resolve_username(cfg)` и **assert id == pinned**, иначе abort (правило 10) |
| `src/account.rs:1032-1072` `log_dc_layout` (`users::GetUsers`, `UserProfilePhoto::Photo.dc_id` против `home_dc_id`) | `userbot::diag` | Диагностика при старте |
| `src/account.rs:1085-1167` `login_all`, `authorize`, `login_interactive` (`request_login_code` → `sign_in` → `SignInError::PasswordRequired` → `check_password`) | `app` CLI `exch login` | 0.10: `gc:client/auth.rs:242, 339, 416`. Сессию шифровать XChaCha20-Poly1305 в `userbot_accounts.session_ciphertext`. Внешний таймаут на создание auth key (AUDIT-2.md 4.4) |
| `src/account.rs:656-734` `supervise`, `shutdown_signal` | `app::supervisor` | Перезапускать актор внутри процесса и ставить зависимые направления на паузу. Не завершать процесс |
| `src/config.rs:14-30` `Secret`; `:321-327` проверка «один session_file у двух аккаунтов»; `:436-581` предупреждения о неизвестных и устаревших ключах | `app::config`, `exch check-config` | `secrecy::SecretString`. Проверку дублей сессии сохранить, иначе будет `AUTH_KEY_DUPLICATED` |
| `src/notifier.rs:685-724` `bot_call` (+ `e.without_url()`), `:262-294` `RichMode`, `:447-627` `LiveMessage`, `EditError`; `src/format.rs:14-34` `esc`, `:786-1096` `Doc`; `:637-756` fmt-хелперы | `tgbot::raw`, `tgbot::ui`, `ops::alerts` | По отчёту ui. Не относится к юзерботу. Логи и уведомления **никогда** не идут через юзербот (`account.rs:814-816`: «логи пойдут через клеймера (риск FLOOD_WAIT на /start)») |
| `src/metrics.rs` (Ring 4096, percentiles, reporter) | `app::metrics` | Добавить то, чего lovec не мерил: **задержку ответа бота кошелька**. Есть только `send_rtt_ms`, `ping_rtt_ms`, `age_ms`, `proc_us` (`metrics.rs:113-116`) |
| `src/logging.rs:14-55` | `app::logging` | Фильтр `exch=info,warn`, спаны `order_id/op_id/account` |
| `deploy/lovec.service:11-59` | `deploy/exch.service` | `Restart=always`, `RestartSec=2`, `StartLimitBurst=20/300s`, hardening, `EnvironmentFile` 0600 |
| `benches/support/fake_tg.rs`, `benches/e2e_loopback.rs:84-262`, `tests/send_outcomes.rs` | `userbot/tests/support/` | §7 |

### 1.2 Сознательно НЕ переносим

| lovec | Почему |
|---|---|
| `fastsend.rs:15-85` `StartEncoder` (ручная сериализация TL) и `fastsend.rs:116-120` `enqueue_urgent_in_dc` | Только в форке; ради микросекунд, exch от них не зависит. В 0.10 есть только `invoke_in_dc` (`gm:sender_pool.rs:121`) |
| `tap.rs`, `seen.rs`, NetThread, busy-poll, CPU pinning (`account.rs:221-302`) | Оптимизации задержки |
| `diff.rs` (`GetChannelDifference`), канальная часть `poller.rs`, `ingest.rs:50-75` `detect` | exch не слушает каналы (правило 10) |
| `solver.rs` (OpenRouter, `PROMPT "Ты решаешь загадку-пароль от Telegram-чека…"`) | Пароль даёт клиент |
| FIFO-сопоставление `ledger.rs:128-141` `pick` | Слабейшее место (AUDIT.md:76-82). В exch одно действие в полёте |
| `claimer.rs:21` `HISTORICAL_THRESHOLD_MS = 60_000`, `ingest.rs:18` `PLACEHOLDER_MAX_AGE_MS = 30_000` | Клиент может прислать чек трёхдневной давности |
| `dedup.rs` как источник правды | Источник правды — уникальные индексы БД (`operations_check_once`, `orders_intake_param_live`) |
| MTProto-бэкенд нотифаера (`notifier.rs:355-431`) | Риск FLOOD на денежных действиях |

---

## 2. Факты о ботах кошельков (фикстуры для `parsers`)

Ни одна строка ответа ниже не захвачена из реального чата: это либо needle (`src/replies.rs`), либо тестовые fixture lovec. Все они покрывают только **активацию** чека. Для создания чека, оплаты счёта, выбора валюты и экрана баланса в lovec нет ни одной строки.

### 2.1 Идентичность ботов

| Платформа | Факт | Verbatim | Источник | Статус |
|---|---|---|---|---|
| CryptoBot | username | `default_username: "send",` | `src/bots.rs:46-50`; тест `config.rs:597` `assert_eq!(cfg.bots.crypto.username, "send");` | live |
| xRocket | username | `default_username: "xrocket",` | `src/bots.rs:52-56` | live |
| CryptoBot | пароли у чеков | `supports_passwords: true,` | `src/bots.rs:49` | live (16 из 457 /start, AUDIT-2.md:1848) |
| xRocket | пароли у чеков | `supports_passwords: false,`; при похожем ответе lovec пишет `"ответ похож на запрос пароля, но у этого бота паролей нет — уточните шаблоны"` | `src/bots.rs:55`, `src/ledger.rs:381` | **конфликт:** `SPEC.md:57` ссылается на поле `password` в `CreateChequeDto` |
| оба | user id | `PeerId::user(1_559_501_630)` (CryptoBot), `PeerId::user(5_014_831_088)` (xRocket) | `tests/send_outcomes.rs:89-98`, `src/claimer.rs:431` `peers: [peer(1_559_501_630), peer(5_014_831_088)]`, `benches/e2e_loopback.rs:133-142` | fixture. Соответствие выведено из `bots.rs:15-19` (`Crypto => 0`, `XRocket => 1`). **Проверить живым resolve** перед закреплением |
| оба | алиасы | «@CryptoBot/@send, @tonRocketBot/@xrocket» | `AUDIT.md:77` | doc/аудит |
| оба | testnet-боты | `/// Ссылка на тестовую сеть (@CryptoTestnetBot, @xrocket_testnet_bot)` | `src/links.rs:141` | код |
| оба | DC | «Все аккаунты и оба бота — в DC2 (Амстердам)» | `AUDIT-2.md:1487` | live |
| оба | display | `BotKind::Crypto => "CryptoBot"`, `BotKind::XRocket => "xRocket"` | `src/bots.rs:33-34` | код |

### 2.2 Коды чеков и формы ссылок

| Платформа | Ситуация | Verbatim / regex | Источник | Статус |
|---|---|---|---|---|
| CryptoBot | чек | `Marker { kind: BotKind::Crypto, prefix: b"CQ", len: 12 }`, regex `^CQ[A-Za-z0-9_-]{10}$`; пример `"CQAbCdEfGhIj"` | `src/bots.rs:66-70`; `src/fastsend.rs:54-61` | live: «Все 134 взятых кода нормальной длины (12, 17, 18)», AUDIT-2.md:1500 |
| xRocket | чек | `prefix: b"t_", len: 17`, regex `^t_[A-Za-z0-9_-]{15}$`; примеры `"t_ABCDEFGHIJKLMNO"`, `"t_val6vkkTbAbxd06"` | `src/bots.rs:71-75`; `src/format.rs:1131` | live |
| xRocket | мультичек | `prefix: b"mci_", len: 19`, regex `^mci_[A-Za-z0-9_-]{15}$`; `"mci_fdraQh70UOwz3nu"` | `src/bots.rs:76-80` | live |
| xRocket | мультичек | `prefix: b"mc_", len: 18`, regex `^mc_[A-Za-z0-9_-]{15}$`; `"mc_VhxQyBOeQXDmHAX"` | `src/bots.rs:81-85` | live |
| xRocket | `mc_`+10 (13 символов), **только через веб-приложение** | «это чеки xRocket, которые активируются только через веб-приложение xRocket» | `AUDIT-2.md:1776-1782` | live (18+ строк лога) → exch: отказ или MANUAL_REVIEW, `/start` не слать |
| xRocket | `t_` длиной 16 | «Ещё 4 раза — `t_` длиной 16» | `AUDIT-2.md:1776` | live, не объяснено |
| оба | параметр ссылки | `head.ends_with(b"start")` / `head.ends_with(b"startapp")` | `src/links.rs:~101-107` | код |
| оба | формы ссылок | `t.me/xrocket?start=mci_fdraQh70UOwz3nu`, `…?start=mc_VhxQyBOeQXDmHAX`, `t.me/xrocket/app?startapp=mci_…`; `tg://resolve?domain=<bot>&start=`, `tg://resolve?start=<код>&domain=<bot>`; хосты `.me`, `.dog` | `AUDIT.md:31`; `src/links.rs:141-145, 218-220` | live + тесты |
| оба | testnet по имени бота, а не по всей ссылке | ложное срабатывание на `t.me/send?ref=testnet_promo&start=…` | `AUDIT-2.md:375-388` | исправленный баг |
| xRocket | `startapp=` принимается как `/start mci_…` | «Не проверено: принимает ли бот `/start mci_…` для ссылок `startapp=`» | `AUDIT-2.md:1247` | **не проверено** |
| оба | анонс чека (текст сообщения) | `const STRONG: [&str; 4] = ["Чек на", "чек на", "Создание чека", "Cheque"]; const WEAK: [&str; 2] = ["🚀", "🦋"];` Реальные: «Rocket-чек на 400 TON», «Мультичек на 1500 USDT» | `src/parser.rs:219-224`; `AUDIT.md:66` | live/код |
| оба | синтетика корпуса | `"🦋 Чек на 5 USDT ($5.00)\n\nАктивируйте: https://t.me/send?start=CQAbCdEfGhIj"`, `"🚀 Rocket-чек на 400 TON\nhttps://t.me/xrocket?start=mci_fdraQh70UOwz3nu"` | `benches/hotpath.rs:61,65` | fixture |

### 2.3 Классификатор ответов на `/start <code>`

Порядок проверки (`src/replies.rs:92-121`, дословно): `t = text.to_lowercase()`, затем

1. `if t.contains("ваш чек") && (t.contains("активировал") || t.contains("получил"))` → `Notice`;
2. `WRONG_PASSWORD`;
3. `LOST`;
4. `NOT_FOR_US`;
5. `WON`;
6. `INVALID`;
7. `CAPTCHA`;
8. `PASSWORD`;
9. `NEEDS_JOIN`;
10. `IN_PROGRESS`;
11. иначе `Unknown`.

Нетерминальные классы: `Reply::Unknown | Reply::WrongPassword | Reply::InProgress | Reply::Notice` (`:17-22`).

| Класс | Платформа | Needle (verbatim, lowercase-substring) | Источник | Пример в тестах lovec | Боевая частота |
|---|---|---|---|---|---|
| Notice (чужой активировал НАШ чек) | оба | `"ваш чек"` + (`"активировал"` или `"получил"`) | `replies.rs:96-99` | `"Ваш чек активировал @someone"` (`:147`) | — |
| WrongPassword | CryptoBot | `"неверный пароль", "неправильный пароль", "wrong password", "incorrect password", "invalid password"` | `replies.rs:41-47` | `"Неверный пароль, попробуйте ещё"` (`:132`) | — |
| LostRace (уже активирован) | оба | `"уже активирован", "уже получен", "already activated", "already been activated", "already claimed"` | `replies.rs:48-54` | `"Этот чек уже активирован"` (`:130`); fixture CryptoBot `"Этот чек уже активирован."` (`format.rs:1440`) | CryptoBot 13/457; xRocket 55 первых попыток (AUDIT-2.md:1848-1849) |
| NotForUs (персональный, не нам) | оба | `"не можете активировать", "предназначен для другого", "not intended for you", "cannot activate"` | `replies.rs:76-81` | `"Вы не можете активировать этот чек"` (`:143-144`) | CryptoBot 239/457; xRocket 81 первых + 21 повтор |
| Won | оба | `"успешно получили", "вы получили", "you received", "you have received", "successfully received"` | `replies.rs:55-61` | `"Вы получили 1 USDT"`, `"YOU RECEIVED 5 TON"` (`:129, 150`); fixture xRocket `"Вы получили\n1 USDT"` (`format.rs:1395`) | CryptoBot 17; xRocket 58+6 |
| Invalid (не найден) | оба | `"не найден", "не существует", "недоступен", "not found", "does not exist"` | `replies.rs:62-68` | `"Чек не найден"` (`:136`); fixture xRocket `"Мульти-чек не найден."` (`format.rs:1463`) | CryptoBot 8; xRocket 49 на повторе |
| Captcha | оба (по SPEC — xRocket) | `"капч", "captcha", "символы, которые вы видите", "на картинке"` | `replies.rs:69-74` | `"Введите символы, которые вы видите на картинке"` (`:138`) | не попадает ни в одну строку разбивки за неделю (AUDIT-2.md:1848-1849) |
| PasswordPrompt | CryptoBot | `"пароль", "password"` (после WON, INVALID, CAPTCHA) | `replies.rs:82` | `"Введите пароль от чека"` (`:135`); fixture `"Введите пароль"` (`format.rs:1477`) | 16/457 |
| NeedsJoin | CryptoBot | `"подпиш", "подписк", "subscribe", "join the"` | `replies.rs:83` | `"Подпишитесь на канал"` (`:141`); fixture `"Подпишитесь"` (`format.rs:1494`) | 164/457 |
| InProgress (промежуточное) | оба | `"получение", "обработ", "processing", "подождите", "please wait"` | `replies.rs:84-90` | `"Подождите, идёт обработка"` (`:146`) | — |
| Unknown | оба | — | `replies.rs:119` | `"что-то новое"` (`:148`) | 60 «нераспознанных» — это ручные действия владельца в ботах (AUDIT-2.md:1498) |

**Подводные камни классификатора.** Они станут тестами exch.

- `"неверный пароль"` содержит `"пароль"`, поэтому WrongPassword проверяется раньше PasswordPrompt.
- Входящий перевод «Вы получили 5 USDT от @x» совпадает с WON (AUDIT.md:82, это пересказ, а не verbatim). В exch Won обязан содержать сумму и актив **и** не содержать `от @`.
- Английские needle показывают, что язык ответа зависит от языка аккаунта. Фиксировать `lang_code` (§5.6).

**Регулярки — предложение для exch, не факт из lovec.** Won:

```
(?i)вы получили\s+(?P<amount>\d[\d\u00A0\u202F ]*(?:[.,]\d+)?)\s*(?P<asset>[A-Z]{2,10})
```

Notice:

```
(?i)ваш чек .*(?:активировал|получил)
```

Утверждать их только после записанных фикстур.

### 2.4 Поведенческие факты

| Платформа | Факт | Verbatim / число | Источник | Статус |
|---|---|---|---|---|
| оба | Боты **не ставят reply_to** | «reply_to у ботов нет, сопоставление не трогать» | `AUDIT-2-notes.md:61` | live |
| оба | Ответы приходят как `UpdateNewMessage`, `UpdateEditMessage` или `UpdateShortMessage` (у последнего нет `reply_markup`) | `U::NewMessage(u) …, U::EditMessage(u) …` | `src/parser.rs:181-184`, `:86-100`, `tap.rs:68-69` | код |
| оба | Второй `/start` с **новым** random_id после нашей победы → «уже активирован» | «Следующее появление чека … отправит второй `/start` с новым `random_id` → бот ответит «уже активирован» → в статистике проигрыш вместо выигрыша» | `AUDIT.md:46` | вывод аудита |
| Telegram | Повтор с тем же random_id безопасен | «from any session of the current account at any time in the past (used random_ids stored by the server do not expire), the method call will simply return the messages generated by the previous method call»; «`RANDOM_ID_DUPLICATE` … приходит тоже с кодом 500»; «на реальном аккаунте не проверял» | `AUDIT-2.md:241, 237, 306` | **doc** |
| xRocket | «Невалид» на свежем чеке иногда проходит со второго раза | `const MAX_ATTEMPTS: u8 = 2; const RETRY_MAX_AGE_MS: u64 = 3_000;`; `fn retries_on_invalid(bot: BotKind) -> bool { bot == BotKind::XRocket }`; лог `"♻️ невалид на свежем чеке — повторяю /start сразу"` | `src/ledger.rs:19-20, 116-126, 341` | live: 6 из 77 повторов (8 %), `AUDIT-2.md:1850` |
| CryptoBot | Пароль: после `/start CODE` бот ждёт пароль текстом в том же чате; новая сессия ввода начинается с повторного `/start` | `self.send(format!("/start {code}"))`; `Reply::PasswordPrompt \| Reply::WrongPassword => return Ok(())`; `Reply::Won => return Err("чек уже активирован без пароля")` | `src/password.rs:294-304` | live |
| оба | Ответ на угаданный пароль | `Reply::Won => Verdict::Solved`, `WrongPassword \| PasswordPrompt => Wrong`, `LostRace \| Invalid => Gone`, тишина 6 s → `Silent` | `src/password.rs:360-373` | код |
| оба | Ручные действия на том же аккаунте засоряют чат | «Нераспознанные ответы ботов (60) — это ваши собственные действия в ботах: кошелёк, создание и удаление чеков, счета, выводы, обмен, реклама» | `AUDIT-2.md:1498` | live → аккаунты exch выделенные, без ручного использования |
| оба | FLOOD на sendMessage к ботам | 29 s после 10 /start за 60 s; CryptoBot 61 s после 11 чеков за секунду; xRocket 703 s (тест владельца), после перезапуска первый /start снова получал 307 и 235 s; xRocket 30 s на серии повторов | `AUDIT-2.md:1491, 1787-1791` | live |
| оба | RTT `sendMessage` | CryptoBot p50 63 мс / p90 113; xRocket p50 64 / p90 114 (неделя: 68/116 и 69/109) | `AUDIT-2.md:1482-1486, 1851` | live. Задержка **ответа бота** не измерялась |
| оба | Признак чужого персонального чека в тексте анонса неизвестен | «Если в тексте таких чеков есть признак (например, «для @…»)… Нужны примеры текстов» | `AUDIT-2.md:1224` | пробел |
| оба | Заглушка инлайн-чека правится позже: lovec запускает приоритетный опрос сообщений `via_bot_id ∈ {wallet bots}` | `let via_bot = v.via_bot_id.is_some_and(\|id\| self.claimer.bot_by_id(id).is_some()); if (via_bot \|\| parser::looks_like_cheque(v.text)) … self.poller.start(PollJob{… priority: via_bot})` | `src/ingest.rs:112-128` | live-поведение, вывод: инлайн-сообщение сначала приходит без ссылки |

---

## 3. Как юзербот отправляет `/start` и ловит ответ

### 3.1 Peer

Peer бота берётся только по закреплённому id.

- В 0.10 `PeerId::user(id)` возвращает `Option<Self>` (`gs:peer.rs:168`), а в форке возвращал `PeerId`.
- `Session::peer_ref` возвращает `BoxFuture<Result<Option<PeerRef>, Self::Error>>` (`gs:session.rs:69`).

Последовательность:

1. `session.peer_ref(PeerId::user(PINNED)?)`.
2. Если `None`: `cold.resolve_username(cfg_username)` (`gc:client/chats.rs:378`), затем `peer.to_ref()`.
3. `assert_eq!(id, PINNED)`, иначе abort и CRITICAL.
4. Cold-клиент с `auto_cache_peers: true` (`gc:client/client.rs:81-95`) сам сохранит peer в сессию. Без этого после рестарта `peer_ref` вернёт `None` (урок `account.rs:59`).

### 3.2 Отправка

Сырой `SendMessage` с заранее сохранённым `random_id`. Это доказанный путь: 167 доставленных `/start` (`AUDIT-2.md:218`).

```rust
// random_id генерируется ДО dispatch и пишется в operations (правило 4), повтор — только им же.
let req = tl::functions::messages::SendMessage {          // tl:2441, слой 227
    no_webpage: true, silent: false, background: false, clear_draft: false,
    noforwards: false, update_stickersets_order: false, invert_media: false,
    allow_paid_floodskip: false,
    peer: bot_ref.into(), reply_to: None,
    message: format!("/start {param}"),
    random_id,
    reply_markup: None, entities: None, schedule_date: None, schedule_repeat_period: None,
    send_as: None, quick_reply_shortcut: None, effect: None, allow_paid_stars: None,
    suggested_post: None, rich_message: None,
};
let res = tokio::time::timeout(SEND_TIMEOUT /*10 s, claimer.rs:22*/, money.invoke(&req)).await;
```

- **Не использовать** `client.send_message(...)` (`gc:client/messages.rs:593`): он берёт свой `generate_random_id()` (`gc:utils.rs:22`), и идемпотентный повтор становится невозможен. lovec использовал его только для паролей (`password.rs:316`).
- Альтернатива — `tl::functions::messages::StartBot { bot, peer, random_id, start_param }` (`tl:2475`). lovec её ни разу не вызывал: grep по `src/`, `tests/`, `benches/` пуст. Оставить как вариант для проверки на тестовом аккаунте. Основной путь — `/start` текстом.
- `money` — клиент с `retry_policy: Box::new(NoRetries)` (`gc:client/retry_policy.rs:42`). Default `AutoSleep` (`gc:client/client.rs:81-95`: `threshold: 60s`, `io_errors_as_flood_of: Some(1s)`) молча повторяет тело после `Io` (`gc:client/retry_policy.rs:63-77`). Для `sendMessage` с тем же random_id это безопасно, но такой повтор невидим для outbox. Для `getBotCallbackAnswer` это двойное нажатие.

### 3.3 Классификация исхода

Источник — `AUDIT-2.md:244-256`, доказано тестами `tests/send_outcomes.rs`.

```text
Ok(updates)                         → Delivered(sent_message_id(updates, random_id))
Err(Rpc "RANDOM_ID_DUPLICATE")      → Delivered(None)       // повтор того же random_id
Err(Rpc code 400..=499, вкл. 420)   → Rejected               // явный отказ; FLOOD_WAIT → FloodGate
Err(InvalidDc)                      → Rejected
Err(Transport|Deserialize|Dropped|Io|Rpc 5xx|timeout) → Unknown
```

Обработка `Unknown`:

1. `handle.disconnect_from_dc(home_dc)` (`gm:sender_pool.rs:140`) — принудительное переподключение.
2. До 2 повторов **тем же телом и тем же random_id** (`MAX_RESENDS = 2`, `AUDIT-2.md:256`).
3. Если всё ещё `Unknown` — сверка через `messages.getHistory` (§3.5), иначе `INTAKE_UNKNOWN` / `MANUAL_REVIEW`.

Код (start_param) освобождается только при `Rejected`. Решение принимает одна сущность — строка `operations` (урок `AUDIT-2.md:1140-1155`).

### 3.4 id нашего сообщения

Портировать `fastsend.rs:174-199` `sent_message_id` для уже разобранного `tl::enums::Updates`. Сохранить `our_msg_id` в `operations`. Это якорь корреляции и `min_id` для сверки.

### 3.5 Как ловить ответ

1. Поток апдейтов: `stream.next_raw()` (`gc:client/updates.rs:105`) → `(tl::enums::Update, State, PeerMap)`. Берём `Update::NewMessage` и `Update::EditMessage` (`parser.rs:181-182`). Короткие `UpdateShortMessage` grammers превращает в `NewMessage` в `next_raw`. Короткая форма приходит без `reply_markup`. Если класс ответа требует кнопок, перезапросить `client.get_messages_by_id(bot_ref, &[id])` (`gc:client/messages.rs:1111`).
2. **До разбора** каждое сообщение чата бота пишется в `wallet_messages` с уникальностью `(peer, msg_id, edit_date)` (правило 8).
3. Фильтр «это ответ на нашу команду»:
   - `peer_id == PeerUser(PINNED_BOT)`;
   - `!out` (`ingest.rs:86`);
   - `id > our_msg_id`;
   - или `EditMessage` с `id` ответа, уже привязанного к команде;
   - на аккаунт в полёте ровно одна команда (правило 11). FIFO и окно `CLAIM_WINDOW` не нужны.
4. Классификация: `InProgress` или `Unknown` до дедлайна — ждём новое сообщение или правку. Терминальные классы завершают шаг. `Notice` и входящие переводы — это **несвязанные** сообщения, они не закрывают команду и идут в отдельный роутер.
5. Если ответ не пришёл, сверка через историю:

   ```rust
   tl::functions::messages::GetHistory { peer: bot_ref.into(), offset_id: 0, offset_date: 0, add_offset: 0, limit: 20, max_id: 0, min_id: our_msg_id, hash: 0 }
   ```

   (`tl:2434`). Сначала через 2–3 s, затем 5, 10 и 20 s, через governor. Это замена `poller.rs`.

### 3.6 Тайминги

| Параметр | lovec | exch (рекомендация) |
|---|---|---|
| Таймаут RPC `sendMessage` | 10 s (`claimer.rs:22`) | 10 s, затем Unknown → reconnect → повтор тем же random_id |
| Окно ответа бота | `CLAIM_WINDOW = 8s` (`dialogs.rs:9`), `PENDING_TTL = 30s` (`ledger.rs:17`, лог `"бот не ответил за 30 с — запись закрыта"` `ledger.rs:421`) | 20 s, капча 30 s (`SPEC.md:1349`), затем getHistory-сверка |
| Ответ на пароль | `REPLY_TIMEOUT = 6s`, `GUESS_DELAY = 400ms` (`password.rs:20-24`) | 6–10 s, не чаще governor |
| Повтор xRocket на «невалид» | сразу, если чеку ≤ 3000 ms (`ledger.rs:19-20`) | 1 повтор через 1–3 s, **новым** random_id. Это безопасно: на первый пришёл явный ответ «не найден» |
| Темп | 4 FLOOD_WAIT за неделю, первый — после 10 /start за 60 s | ≥ 1,5 s между действиями **и** ≤ 6–8 сообщений боту в минуту на аккаунт (§6, F1) |
| Задержка ответа бота | не измерялась | мерить с первого дня (метрика) |

### 3.7 Пароль (только CryptoBot)

Порядок взят из `password.rs:284-373`:

1. `/start CODE` → ждать `PasswordPrompt` или `WrongPassword`.
2. Пароль текстом через сырой `SendMessage` (новый random_id на каждую попытку, сохранённый до отправки).
3. Вердикт.
4. Перед второй попыткой снова `/start CODE` (`AUDIT-2.md:1077-1079`).

Пароль — `SecretString`, в лог и БД не пишется (правило 13).

---

## 4. Капча xRocket: как решает lovec

**lovec капчу не решает.** Что в нём есть:

- Классификация по needle `CAPTCHA: &[&str] = &["капч", "captcha", "символы, которые вы видите", "на картинке"]` (`src/replies.rs:69-74`).
- Карточка `Reply::Captcha => ("🧩 Капча", Topic::Activations, true)` с признаком `l.text("не поддерживается")` (`src/format.rs:119, 155`).
- Счётчик `captcha_total` / «чеков с капчей» (`src/metrics.rs:91, 156, 266`).

Чего в нём нет:

- Ни одного нажатия кнопки: grep по `GetBotCallbackAnswer` и `KeyboardButton::Callback` в `src/`, `tests/`, `benches/` пуст.
- Разбора медиа: `parser.rs` читает только text, entities и URL-кнопки.
- Попыток решения. `src/solver.rs` (`AI_URL "https://openrouter.ai/api/v1/chat/completions"`, `PROMPT "Ты решаешь загадку-пароль от Telegram-чека…"`) решает **загадки-пароли**, а не капчу.

Боевых данных о капче нет: в разбивке за неделю (AUDIT-2.md:1848-1849) капчи нет ни в одной строке.

**Что делать exch.** В lovec переносить нечего.

1. Записать капчу (В8, `SPEC.md:1755`): тип (кнопки-варианты, эмодзи, картинка), `reply_markup` с `KeyboardButtonCallback{text, data}` (`tl:703`, флаг `requires_password`), количество попыток, срок жизни, что происходит при ошибке.
2. Нажатие — `messages.GetBotCallbackAnswer { game: false, peer, msg_id, data: Some(bytes), password: None }` (`tl:2490`) через клиент `NoRetries`, `max_attempts = 1` (правило 6). Результат — правка того же сообщения (`UpdateEditMessage`) или новое сообщение.
3. Решатель по фикстурам (`SPEC.md:120`). Если не распознан с первой попытки — `INTAKE_UNKNOWN` и оператор нажимает кнопку из админки.
4. Профилактика (`SPEC.md:645, 662, 680`): просить клиента создавать чеки «без капчи». **Свои** выплатные чеки xRocket создавать с отключённой капчей: по `SPEC.md:57` `enableCaptcha` по умолчанию `true` (см. риск X3 в §8.3).

---

## 5. grammers: версия, патчи, сессия

### 5.1 Что использует lovec

- `Cargo.toml:36-37`: `grammers-client = { path = "C:/grammers/grammers-client", features = ["html"] }`, `grammers-session = { path = "C:/grammers/grammers-session", features = ["sqlite-storage"] }`.
- `Cargo.lock`: client/mtsender 0.8.1, session/tl-types 0.8.0. Слой TL 222. Основа — upstream `fa7692e` (10.02.2026) плюс ветка `lovec-audit-2` на `ac9d4bd`.
- Dev-deps: `grammers-crypto`, `grammers-mtproto`, `sha1 0.10`, `sha2 0.10`.
- `[profile.release] panic = "unwind"`, `[lints.rust] unsafe_code = "forbid"`.
- Исходников форка (`C:/grammers`) в архиве нет. Его API известен только по коду lovec и аудитам.

Коммиты форка: `6735e4d` (TCP_NODELAY, `POSSIBLE_GAP_TIMEOUT` 500→100 ms), `511feb5` (biased select, по пакету), `7b2f389` (таймаут подключения 10 s), `4761c49` (panic → ошибки), `710d89e` (паника message_box), `d13c451`, `f4cf441` + `1220d3a` / `566d2c7` / `1bb6d17` / `525787c` (`PING_DELAY` 15 s, `PONG_TIMEOUT` 10 s, Io вместо Dropped), `8bbd39f` (`enqueue_urgent_in_dc`), `b937b00`, `bcacc00`, `80360ef`, `f9d495b`. Патчи в `patches/`: `grammers-biased-io.patch` и `grammers-tcp-nodelay.patch`.

### 5.2 Форк 0.8.1 против 0.10.0 (проверено по исходникам 0.10.0)

| Аспект | Форк lovec | grammers 0.10.0 | Что делать exch |
|---|---|---|---|
| Слой TL | 222 | 227 (`tl:1` `// LAYER 227`). По SPEC слой 229 есть только в git (`SPEC.md:1250`) | Решить: crates.io 0.10.0 или Codeberg rev. Фикстуры fake_tg перегенерировать под слой |
| `Session` | `peer_ref → Option`, `home_dc_id() → i32` | `type Error`; `home_dc_id() → Result<i32, Error>` (`gs:session.rs:32`); `peer_ref → BoxFuture<Result<Option<PeerRef>>>` (`:69`); также `set_home_dc_id`, `dc_option`, `set_dc_option`, `peer`, `cache_peer`, `updates_state`, `set_update_state` | Свой `impl Session` (§5.5) |
| `SessionData` | в тестах `{home_dc, dc_options}` | `{home_dc, dc_options, peer_infos, updates_state}` + `import_to` (`gs:session_data.rs:18-32, 53`) | Основа для зашифрованного blob |
| `MemorySession` | `from(SessionData)` | `pub struct MemorySession(Mutex<SessionData>)` + `From<SessionData>` (`gs:storages/memory.rs:23-31`). **Экспорта нет**, поле приватное | Допущение `SPEC.md:1410` «есть хранилище в памяти с экспортом» **ложно**. Нужна своя реализация |
| SQLite | `sqlite-storage` → libsql | `default = ["sqlite-storage"]`, `sqlite-storage = ["dep:libsql"]` | `default-features = false` |
| serde | ? | feature `serde`: `DcOption` (с hex `auth_key`), `UpdatesState`, `ChannelState`, `PeerInfo` и др. (`gs:types.rs:20-21, 49, 65, 74`; `gs:peer.rs:30-115`) | Включить `serde` для сериализации blob |
| Pool | `SenderPool::new(Arc<S>, api_id) → {runner, updates, handle}` | то же (`gm:sender_pool.rs:160`) + `with_configuration(.., ConnectionParams)` (`:169`); `SenderPoolFatHandle` (`:58`); `invoke_in_dc` (`:121`), `disconnect_from_dc` (`:140`), `quit` (`:146`) | Без изменений; `enqueue_urgent_in_dc` нет и не нужен |
| `ClientConfiguration` | `{retry_policy, auto_cache_peers}` | то же (`gc:client/client.rs:39-50`); default `AutoSleep{60s, io 1s}` + `auto_cache_peers: true` (`:81-95`) | Деньги — `NoRetries` |
| `UpdatesConfiguration` | `catch_up:false`, `Some(10_000)` | default `update_queue_limit: Some(100)` (`gc:client/client.rs:97-102`) | Ставить `Some(10_000)`; `catch_up: true` при сохранённом `UpdatesState` |
| UpdateStream | `next_raw` | `next` (`:97`), `next_raw` (`:105`), `sync_update_state` (`:288`), `stream_updates` (`:308`) | `sync_update_state` вызывать при остановке |
| Ping / dead connection | `PING_DELAY` 15 s + `PONG_TIMEOUT` 10 s | `PING_DELAY = 60s` (`gm:sender.rs:49`), `NO_PING_DISCONNECT = 75` (`:58`), pong-таймаута нет | Свой watchdog: `Ping` с таймаутом 5 s каждые 10 s; после 3 сбоев `disconnect_from_dc` или пересоздание пула |
| Таймаут подключения | 10 s в runner | `create_connection(dc_id).await` прямо в цикле runner (`gm:sender_pool.rs:246, 272`), без таймаута. Тихий DC блокирует runner, включая `quit` | Каждый `invoke` в `tokio::time::timeout`; при зависании `quit` — drop runner-задачи и новый пул |
| Паники на данных сервера | превращены в ошибки | `panic!` в `gm:sender.rs:217, 551, 617`; runner делает `panic::resume_unwind(reason)` (`gm:sender_pool.rs:213`) | Runner в отдельной задаче; `JoinError::is_panic()` → пересоздать актор, CRITICAL |
| Dropped для запросов в очереди | Io (`566d2c7`) | `Dropped` (`gm:sender.rs:585`, `gm:sender_pool.rs:129-130`) | Для денег Dropped = Unknown, для чтения — повтор с отступом |
| TCP_NODELAY | есть | нет (`gm:net/tcp.rs:34`) | Не обязательно |
| Паника message_box (4.5) | исправлена в `710d89e` | по `AUDIT-2.md:1941` исправлена так же | Аккаунт не держать в каналах |
| Высокоуровневый inline | — | `client.inline_query(bot, query)` (`gc:client/bots.rs:155`). `InlineResult::send` берёт свой `generate_random_id()` и **выбрасывает результат**: `// TODO return the produced message`, `.map(drop)` (`gc:client/bots.rs:31-52`) | Сырой `SendInlineBotResult` со своим random_id |
| Нажатие кнопки | — | у `Message` нет `click()`. Есть `via_bot_id()` (`gc:message/message.rs:317`), `reply_markup()` (`:417`), `edit_date()` (`:534`), `refetch()` (`:769`) | Сырой `GetBotCallbackAnswer` |
| Помощники peer | `PeerId::user → PeerId` | `PeerId::user(id) → Option<Self>` (`gs:peer.rs:168`), `PeerId::self_user()` (`:163`), `to_ambient_ref()` (`:300`) | Для «Избранного» — `tl::enums::InputPeer::PeerSelf` |
| `deserializable-functions` | (для fake_tg) | feature есть в `grammers-tl-types-0.10.0/Cargo.toml` | Включить в dev-deps тестового набора |

### 5.3 Рекомендация по версии

1. Закрепить grammers по `git = "https://codeberg.org/Lonami/grammers", rev = "<sha>"` (`SPEC.md:1251`) для всех `grammers-*`. Если git-rev недоступен в CI — crates.io `=0.10.0` (слой 227). Код форка 0.8.1 **не переносить**: «две несовместимые версии» (`AUDIT-2.md` 16.4).
2. Исправления форка закрывать **на уровне приложения**: таймауты, watchdog, supervisor, классификация Dropped. Патч к grammers — только если двухнедельный прогон на тестовом аккаунте покажет зависания. Тогда это свой форк на Codeberg с рядом маленьких коммитов (`7b2f389`, `4761c49`, `f4cf441…525787c`), закреплённый по rev.
3. `patches/grammers-*.patch` не нужны (latency).

### 5.4 Клиенты на один пул

```rust
let pool = SenderPool::with_configuration(Arc::new(pg_session), api_id, conn_params); // gm:sender_pool.rs:169
let runner = tokio::spawn(pool.runner.run());                      // JoinHandle — супервизору
let money  = Client::with_configuration(pool.handle.clone(), ClientConfiguration { retry_policy: Box::new(NoRetries), auto_cache_peers: false });
let reads  = Client::with_configuration(pool.handle.clone(), ClientConfiguration { retry_policy: Box::new(AutoSleep { threshold: Duration::from_secs(30), io_errors_as_flood_of: Some(Duration::from_secs(1)) }), auto_cache_peers: true });
let stream = reads.stream_updates(pool.updates, UpdatesConfiguration { catch_up: true, update_queue_limit: Some(10_000) }).await?;
```

Через `money` идут: `sendMessage`, `startBot`, `getBotCallbackAnswer`, `sendInlineBotResult`, `requestWebView`. Через `reads` идут: `getHistory`, `getMessages`, `getInlineBotResults`, `resolve_username`, `get_me`, `Ping`, `GetState`. FLOOD_WAIT на `money` не досыпается молча: он идёт в `FloodGate` и в БД.

### 5.5 Хранение сессии

Реализовать `PgEncryptedSession: grammers_session::Session`:

- В памяти держит собственную структуру: `home_dc`, `HashMap<i32, DcOption>`, `HashMap<PeerId, PeerInfo>` (только боты кошельков и self), `UpdatesState`.
- Сериализация через serde (feature `serde`), шифрование XChaCha20-Poly1305 с ключом из `SESSION_KEY` (`SPEC.md:1410`), запись в `userbot_accounts.session_ciphertext` с `key_version`.
- `set_home_dc_id`, `set_dc_option`, `cache_peer` записываются сразу. `set_update_state` — с дебаунсом 1–5 s и обязательно при `sync_update_state` на остановке.
- Plaintext держать в `Zeroizing<Vec<u8>>`, на диск не писать.
- Логин (`exch login`) создаёт ту же структуру и шифрует её.
- Миграция существующих сессий lovec (SQLite/libsql 0.8) не нужна: для exch нужны отдельные выделенные аккаунты, а сессию одного auth key на два процесса запускать нельзя (`AUTH_KEY_DUPLICATED`).

### 5.6 Параметры подключения

`ConnectionParams` (`gm:configuration.rs`) по умолчанию:

- `device_model: format!("{} {}", info.os_type(), info.bitness())`;
- `app_version: env!("CARGO_PKG_VERSION")`, то есть «0.10.0»;
- `lang_code` и `system_lang_code` берутся из локали системы, иначе `"en"`.

Явно задать `lang_code: "ru"` и `system_lang_code: "ru"`. Боты кошельков, вероятно, выбирают язык по `language_code` пользователя. Это вывод: lovec держал английские needle (`replies.rs:44-46, 51-53`). Без фиксации язык ответов зависит от локали сервера, и фикстуры разъедутся. `device_model` и `app_version` задать правдоподобными и неизменными: это бан-гигиена (допущение, проверить).

---

## 6. Уроки аудитов, критичные для денег и банов

### Деньги

| # | Урок | Источник | Действие в exch |
|---|---|---|---|
| M1 | Transport (CRC, seq, длина), Deserialize, Dropped, Io (даже дважды подряд), RPC 500 (и коды вне списка, например −503) и таймаут **не означают «не выполнено»**. grammers отдаёт Transport-ошибку всем запросам соединения, включая уже выполненные (`sender.rs:373-383` upstream) | `AUDIT-2.md:230-245` | Трёхзначная классификация §3.3. Unknown → сверка / `MANUAL_REVIEW` |
| M2 | Повтор только **тем же** random_id. Telegram вернёт прежний результат или `RANDOM_ID_DUPLICATE` (500), пока исходный вызов ещё выполняется | `AUDIT-2.md:237, 241, 256`; `claimer.rs:374-391` | random_id в `operations` до dispatch. Применяется к `sendMessage`, `startBot`, `sendInlineBotResult` (у всех есть `random_id`: `tl:2441, 2475, 2486`). **Проверить на тестовом аккаунте**: «на реальном аккаунте не проверял» (`AUDIT-2.md:306`) |
| M3 | Новый `/start` с новым random_id после нашей победы → «уже активирован» → ложный проигрыш | `AUDIT.md:46` | «Уже активирован» после любого неясного исхода = неоднозначно → сверка баланса или `MANUAL_REVIEW`, никогда `INTAKE_FAILED`. `SPEC.md` (строка 123 по отчёту activation: «Нет ответа 20 с → повтор /start … безопасно») уточнить: повтор только с тем же random_id |
| M4 | grammers по умолчанию (`AutoSleep`) сам повторяет запрос после Io | `gc:client/retry_policy.rs:63-77`; `gc:client/client.rs:81-95`; `AUDIT-2.md:464` | `NoRetries` для всех денежных действий. У `getBotCallbackAnswer` и web view нет ключа, поэтому `max_attempts = 1` (правило 6) |
| M5 | Таймаут 10 s держал код и ничего не делал. Поздний ответ бота сопоставлялся со следующим чеком | `AUDIT-2.md:277-301` | Таймаут → reconnect → повтор тем же random_id. Reconnect даёт `Dropped` другим запросам — для них тоже Unknown |
| M6 | Решение «освободить ключ» принималось в двух местах (`finish_send` и `ledger keep_claimed`), между ними было окно для дубля | `AUDIT-2.md:1140-1155` | Только FSM `operations` |
| M7 | Нет reply_to, FIFO с TTL 30 s приписывал чужие сообщения (переводы, ручные действия). Маркер «Failed» проглатывал ответ следующей активации (исправлено в `3945b04`) | `AUDIT.md:76-83`; `AUDIT-2.md:309-332, 1498` | Одно действие в полёте, `id > our_msg_id`, явный роутер для unsolicited, выделенные аккаунты |
| M8 | Сумма выигрыша не разбиралась, f64 и только `$`/`USD` | `amount.rs:24-48` | Decimal, тикер, сверка «ожидали / получили» с балансом |
| M9 | Фильтр возраста 60 s терял чеки после getDifference | `AUDIT.md:95-97`; `claimer.rs:21` | В exch возраст сообщения клиента не фильтровать |
| M10 | Ссылку на чужого бота с тем же префиксом отправили в xRocket | `AUDIT.md:72-80` | Whitelist username + закреплённые id |
| M11 | `mc_`+10 активируется только через веб-приложение | `AUDIT-2.md:1782` | Отказ или `MANUAL_REVIEW` до `/start` |
| M12 | Поток 5xx → HTML-фолбэк в нотифаере дублирует сообщения | `AUDIT-2.md:1569` | Никогда для сообщений, подтверждающих деньги |

### FLOOD и баны

| # | Урок | Источник | Действие |
|---|---|---|---|
| F1 | FLOOD_WAIT 29 s после всего 10 `sendMessage` к ботам за 60 s; 61 s после 11 чеков за секунду; 703 s при тесте; перезапуск отсчёт не сбрасывает | `AUDIT-2.md:1491, 1787-1791` | Governor из `SPEC.md` (1 действие / 1,5 s = 40/мин) в ~4 раза выше уровня, на котором уже был FLOOD. Ограничить: 1,5 s минимум **и** токен-бакет ≈ 6–8 сообщений боту в минуту на аккаунт. Callback-нажатия считать в том же бюджете. Пауза хранится в БД |
| F2 | `PEER_FLOOD` (аккаунт под спам-ограничением) приходит без значения ожидания, lovec долбил дальше | `AUDIT.md:90-93, 102-105` | Статус `restricted`, CRITICAL, автопауза направления |
| F3 | Одна FLOOD-пауза останавливает **все** действия аккаунта, включая пароли (`password.rs:336-355`). Поток фальшивых ссылок с валидным префиксом может сжечь лимит | `flood.rs:67-124`; `AUDIT.md:295`; `AUDIT-2.md:1036-1046` | Лимит заявок на клиента (`SPEC`: 5 за 10 мин), проверка формата и whitelist до любого действия |
| F4 | Логи и уведомления через юзербот повышают риск FLOOD | `account.rs:814-816` | Только ops-бот (Bot API) |
| F5 | `AUTH_KEY_DUPLICATED` при двух подключениях на одном ключе | `flood.rs:14-20`; `config.rs:321-327` | Одна сессия — один процесс. Проверка в `check-config` |
| F6 | Аккаунт в каналах без access_hash: около 2600 предупреждений «cannot getChannelDifference … missing its hash» в неделю; паника message_box дала 3 ч 15 мин и около 8 ч простоя | `AUDIT-2.md` 4.1, 4.5, 15.1, 1752-1760 | Выделенный аккаунт без каналов; systemd `Restart=always` |
| F7 | Тихое соединение «TCP жив, сервер молчит» живёт около 15 мин на Linux, getDifference без таймаута, поток апдейтов глохнет | `AUDIT-2.md:404-415, 440, 446-473` | Watchdog §5.2. Ложные срабатывания на больших записях (Р1, Н1) для exch неактуальны: трафик мелкий |
| F8 | Ручные действия владельца на рабочем аккаунте — шум и путаница | `AUDIT-2.md:1498` | Аккаунты exch только для exch. Ручной вход только в режиме записи (§8.7) |
| F9 | Не подписываться на каналы ради NeedsJoin (164 из 457) | `AUDIT-2.md:1848`; правило 10 | NeedsJoin → понятная ошибка клиенту |
| F10 | Секреты: derived `Debug` утекает `api_hash`; токен в URL reqwest — нужен `without_url()`; Windows `set X="…"` кладёт кавычки в значение (неделя 401 от OpenRouter); session-файлы и config в архивах — это полный доступ к аккаунту | `AUDIT-2.md:1094-1098, 1692, 1800-1806`; `notifier.rs:699, 704` | `SecretString`; при старте предупреждать о кавычках и пробелах в ключах (печатать только длину); сессии только в БД, зашифрованные |
| F11 | Неустойчивость хоста: Windows, без супервизора, сбросы 10054/10051 | `AUDIT-2.md:1213-1221` | Linux + systemd; для Windows — скрипт `:loop … timeout /t 10 … goto loop` |
| F12 | Хостинг рядом с DC: боты в DC2 | `AUDIT.md:192-195` | Сервер в Европе; `log_dc_layout` при старте |

---

## 7. fake_tg: можно ли использовать для тестов exch

**Да, но только как нижний уровень** — тесты адаптера `GrammersTransport` при сетевых сбоях. Сценарные тесты денег должны идти на симуляторе ботов (§9) через тот же трейт.

Что такое `benches/support/fake_tg.rs` (около 550 строк, по отчёту lessons):

- Сервер MTProto 2.0, transport Full, на 127.0.0.1. `Listener::bind` (`:46-65`) с детерминированным `auth_key`, поэтому DH не нужен.
- Сессия клиента засевается так: `SessionData{home_dc, dc_options: DcOption{id, ipv4: listener.addr, ipv6, auth_key: Some(listener.auth_key)}}` → `MemorySession::from` → `SenderPool::new` (`benches/e2e_loopback.rs:84-129`).
- Шифрование: `ServerCrypto::{seal, open}` (`:118-228`), контейнеры `0x73f1f8dc`, паника на `gzip_packed 0x3072cfa1` (`:338`).
- Ответы: `config_bytes` на `initConnection` (`:360-415`), `pong` (`:518-520`), `rpc_result` (`:310-316`), `rpc_error(code,msg)` (`:522-528`), `short_sent(id)` (`:504-516`), `channel_update` (`:485-501`).
- Разбор запросов: `parse_send_message(body) → (text, random_id)` (`:531-539`, нужен `Deserializable` у функций).
- Инъекции сбоев: `send_corrupted` (последний байт `^0xff` → `BadCrc`, `:262-270`), `send_raw` (transport error −404), `send_packets` (пачка), молчание.
- Готовые тесты-образцы (`tests/send_outcomes.rs`): `io_error_resends_the_same_random_id` (228-251), `transport_error_must_not_lead_to_second_start` (294-306), `rpc_500_must_not_lead_to_second_start` (339-348), `stalled_connection_gets_the_same_start_on_a_new_one` (369-389). Каждый проверяет «один код — один random_id».

Как перенести:

1. Скопировать в `crates/userbot/tests/support/fake_tg.rs`. Это код владельца, лицензионных проблем нет.
2. Dev-deps: `grammers-crypto`, `grammers-mtproto`, `grammers-tl-types = { features = ["deserializable-functions"] }` (feature есть в 0.10.0), `sha1 0.10`, `sha2 0.10`. Все закреплены тем же rev, что и основной grammers.
3. Под слой 227/229 пересобрать литералы `tl::types::Message` (в кнопках появилось поле `style: flags.10`, `tl:702-718`) и `Config`.
4. Сверить с 0.10 сигнатуры `Transport::unpack` (`data_range`, `next_offset`), `DequeBuffer` (`grammers_crypto::DequeBuffer` есть), функции ige, `SenderPool::new`, `DcOption`. `SessionData` в 0.10 требует ещё `peer_infos` и `updates_state`: брать `SessionData::default()` и переопределять поля.
5. Заменить busy-spin (`recv`, `write_all_spin`) на блокирующее чтение с таймаутом, иначе CI занимает ядро.
6. Добавить построители:
   - `UpdateNewMessage` / `UpdateEditMessage` в личке с ботом (`PeerUser`, `users: [User{bot: true, access_hash}]`);
   - сообщения в «Избранном» (`out: true`, `via_bot_id`);
   - `ReplyInlineMarkup` с `KeyboardButtonCallback{data}`, `KeyboardButtonUrl`, `KeyboardButtonWebView`;
   - `messages.BotResults` (`tl:821`), `messages.BotCallbackAnswer` (`tl:845`), `WebViewResultUrl` (`tl:1546`), `updates.State`, пустой `updates.Difference`, `messages.Messages` для `getHistory`/`getMessages`.
7. Тесты адаптера для каждого денежного метода:
   - «выполнил и вернул таймаут»;
   - bad CRC после выполнения;
   - RPC 500 `RPC_CALL_FAIL`;
   - `rpc_error(420, "FLOOD_WAIT_30")`;
   - `RANDOM_ID_DUPLICATE` на повторе;
   - Dropped при переподключении.

   Проверка: ≤1 эффекта на `random_id`; для `getBotCallbackAnswer` — ровно 1 вызов.

Ограничения: fake_tg не знает логики ботов, это даёт симулятор (§9). Создание auth key (DH) он не покрывает, и для `exch login` нужен ручной прогон.

---

## 8. Пробелы: чего в lovec нет, но нужно exch

### 8.0 Чем lovec может помочь (только чтение)

- Кнопки: `parser.rs:32-40`. `if let Some(tl::enums::ReplyMarkup::ReplyInlineMarkup(markup)) = markup { for tl::enums::KeyboardButtonRow::Row(row) in &markup.rows { for button in &row.buttons { … button_url(button) …` берёт только URL из `Url`, `UrlAuth`, `WebView`, `SimpleWebView` (`:208-217`). Callback, SwitchInline и Copy игнорируются.
- Правки: `parser.rs:178-202` (`EditMessage`, `edit_date`, `timestamp = edit_date.unwrap_or(date)`), `claimer.rs:116-138` (dedup класса по правке).
- Инлайн-сообщения: `MsgView.via_bot_id` (`parser.rs:61`) и приоритетный опрос `PRIORITY_STEPS_MS` до 15 s (`ingest.rs:112-128`, `poller.rs:17-19`). Это единственный опыт lovec с инлайн-чеками, причём с **чужими**.
- Многошаговый диалог: `dialogs.rs:84-92` (`attach_session`) и `password.rs:284-373`.
- Веб-приложение: только упоминание в `AUDIT-2.md:1226` — «нужен запуск веб-приложения xRocket от имени аккаунта (`messages.requestWebView` / `requestAppWebView` и дальнейшие запросы самого приложения). Это отдельная функция со своим протоколом, о котором в коде и логах ничего нет».
- lovec **ни разу** не вызывает `GetBotCallbackAnswer`, `GetInlineBotResults`, `SendInlineBotResult`, `RequestWebView`, `RequestAppWebView`, `RequestSimpleWebView`, `StartBot`, `GetHistory`, `InputPeer::PeerSelf` (grep по `src/`, `tests/`, `benches/` пуст). Эти TL-функции существуют задолго до слоя 222, то есть в форке они есть как `tl::functions::messages::*`, но ни одна не проверена в бою. Сигнатуры ниже — по слою 227.

### 8.1 Создание чека: инлайн-режим (основной путь)

Шаги владельца: набрать `@send 10usdt` (CryptoBot) или `@xrocket 10` (xRocket), над клавиатурой появляется результат «send», нажатие отправляет красивое сообщение с кнопкой.

| Шаг | MTProto (слой 227) | grammers 0.10 | Примечание |
|---|---|---|---|
| 1. Запрос результатов | `messages.GetInlineBotResults { bot: InputUser(pinned), peer: InputPeer::PeerSelf, geo_point: None, query: "10usdt" / "10", offset: "" }` (`tl:2484`) → `messages.BotResults{query_id, results, switch_pm, cache_time, users}` (`tl:821`) | Есть помощник `client.inline_query(bot, query)` (`gc:client/bots.rs:155`), но лучше сырой `invoke` через `reads`: нужны `switch_pm` и `cache_time` | Ничего не меняет, если чек создаётся только при отправке (**проверить**, X1) |
| 2. Выбор результата | Разобрать `BotInlineResult{id, type, title, description, send_message: BotInlineMessageText{message, entities, reply_markup} / MediaAuto}` (`tl:818, 809-810`) | `InlineResult.raw` | Выбирать по точному совпадению суммы и актива в title/description/message. Неоднозначность → abort. Пустые results или `switch_pm` (например, «недостаточно средств») → явная ошибка |
| 3. Отправка в «Избранное» | `messages.SendInlineBotResult { silent:false, background:false, clear_draft:false, hide_via:false, peer: InputPeer::PeerSelf, reply_to:None, random_id /*из operations*/, query_id, id, schedule_date:None, send_as:None, quick_reply_shortcut:None, allow_paid_stars:None }` (`tl:2486`) → `Updates` | **Не** `InlineResult::send`: свой random_id и `.map(drop)` (`gc:client/bots.rs:31-52`) | Клиент `money` (NoRetries). Unknown → повтор тем же random_id, это ожидаемо идемпотентно (M2). Пока не проверено — `max_attempts=1` (правило 6, `SPEC.md:147` «без идемпотентности») |
| 4. Наше сообщение | Из `Updates`: `UpdateMessageID{id, random_id}` (`tl:317`) + `UpdateNewMessage{message: Message{out: true, peer_id: PeerUser(self), via_bot_id: Some(bot), reply_markup}}` | `sent_message_id` (порт `fastsend.rs:174-199`) | **lovec отбрасывает `out`** (`ingest.rs:51, 86`). В exch для `peer == self && via_bot_id ∈ pinned` исходящие принимать |
| 5. Ссылка | `reply_markup: ReplyInlineMarkup` → `KeyboardButtonUrl{url}` (`tl:702`) → `links::scan` → маркер `CQ`/12 или `t_`/17 + whitelist | `parser.rs:208-217` + `links.rs` | Если ссылки ещё нет (заглушка вроде «Создание чека», `parser.rs:220`) — ждать `UpdateEditMessage` того же id и параллельно `get_messages_by_id(self, &[id])` (`gc:client/messages.rs:1111`) по шагам `PRIORITY_STEPS_MS` |
| 6. Фиксация | `wallet_messages` (правило 8), ссылка → `operations.result` → клиенту через tgbot (Bot API), без пересылки сообщения | — | — |

**Риски инлайн-создания:**

- **I1.** Неизвестно, когда бот создаёт чек: на запросе (шаг 1) или на выборе (шаг 3, через chosen-inline-result бота). Если на запросе — каждая попытка создаёт чек.
- **I2.** **Чек, созданный в «Избранном», может оказаться привязанным к собеседнику**, то есть к самому себе, и клиент его не активирует. Проверить для обоих ботов.
- **I3.** Бот может так и не отредактировать заглушку: сбой у бота или нет feedback. Это Unknown → сверка через список чеков в боте (`/checks`, `/cheques`), API нет.
- **I4.** `query_id` живёт ограниченно (`cache_time`). Ошибки `QUERY_ID_INVALID` и `RESULT_ID_INVALID` — Rejected, нужен новый запрос (безопасно только при I1 = «на выборе»).
- **I5.** Набор и названия результатов меняются (особенно у xRocket: `@xrocket 10` даёт выбор актива). Выбор только по разобранному тексту, фикстуры обязательны.
- **I6.** Отдельные лимиты на `getInlineBotResults` неизвестны. Считать в бюджете F1.
- **I7.** Комиссия или минимум за создание чека неизвестны. Нужно для резервов.
- **I8.** Удаление сообщения из «Избранного» чек не отменяет. Отмена или возврат невостребованного чека — отдельный поток (удаление в боте), его тоже нет в lovec.
- **I9.** Параметры xRocket-чека по умолчанию (`enableCaptcha` default `true`, `SPEC.md:57`) — проверить, что инлайн-чек без капчи, подписок и Premium.

### 8.2 Создание чека: меню бота (запасной путь)

CryptoBot: `/checks` → кнопка «Создать чек» → выбор валюты → ввод суммы → ссылка.
xRocket: `/cheques` → «Персональный» → «Создать чек» → USDT → ввод суммы → ссылка.

| Шаг | MTProto | Идемпотентность |
|---|---|---|
| Команда `/checks` или `/cheques` | `SendMessage{message:"/checks", random_id}` (§3.2) | да (random_id) |
| Кнопка «Создать чек», «Персональный», валюта | найти в `reply_markup` ответа `KeyboardButtonCallback{text, data}` (`tl:703`) **по тексту** → `GetBotCallbackAnswer{game:false, peer: bot, msg_id, data: Some(data), password: None}` (`tl:2490`) → `messages.BotCallbackAnswer{alert, has_url, native_ui, message, url, cache_time}` (`tl:845`); экран меняется правкой (`UpdateEditMessage`, тот же msg_id) или новым сообщением | **нет**: `max_attempts=1`, клиент NoRetries |
| Ввод суммы | бот ждёт текст → `SendMessage{message: "10", random_id}` | да |
| (возможно) подтверждение | ещё callback | нет |
| Ссылка | текст, `TextUrl` или URL-кнопка → `find_in` (`parser.rs:16-42`). Возможны `KeyboardButtonCopy{copy_text}` (`tl:718`) и `KeyboardButtonSwitchInline` (`tl:706`) | — |

**Риски меню:**

- **X1.** Callback без ключа: `BOT_RESPONSE_TIMEOUT` или Io после нажатия — Unknown. Состояние определять **чтением** экрана (`getMessages` по msg_id или `getHistory`), не повторным нажатием.
- **X2.** Состояние бота «ожидаю сумму» после прерванного прошлого потока: случайный текст станет суммой. Каждый поток начинать с команды-сброса и проверять экран перед вводом.
- **X3.** «Персональный» у xRocket — нужно подтвердить, что это одноразовый чек, а не привязанный к пользователю. Параметры по умолчанию (капча) отключить в настройках.
- **X4.** Устаревшая `data` (бот перезапущен) — Rejected или alert, поток начинается заново.
- **X5.** Позиции кнопок не стабильны, искать по тексту. Тексты кнопок записать в фикстуры (`ru.toml` не трогать: это тексты ботов, а не наши).
- **X6.** Ввод суммы с запятой или точкой, формат по боту — фикстура.

### 8.3 Оплата счёта CryptoBot

Шаги владельца: открыть ссылку → кнопка выбора валюты → «Оплатить» → открывается мини-приложение → ввод PIN.

| Шаг | MTProto | Что известно |
|---|---|---|
| Открыть счёт | `SendMessage{"/start IV…", random_id}` (§3.2) | Префикс `IV…` есть только в SPEC (`SPEC.md:65`). В lovec маркера нет. «Открытие не оплачивает счёт [допущение]» (`SPEC.md:138`) |
| Карточка счёта | разбор текста: сумма, актив, статус, срок; кнопки | фикстур нет |
| Выбор валюты | `GetBotCallbackAnswer` (callback, §8.2) → правка карточки | тип кнопки не записан |
| «Оплатить» | по типу кнопки: `KeyboardButtonWebView{url}` (`tl:714`) → `messages.RequestWebView{from_bot_menu:false, silent:false, compact:false, fullscreen:false, peer: bot, bot: InputUser, url: Some(url), start_param: None, theme_params: None, platform: "android", reply_to: None, send_as: None}` (`tl:2593`); `KeyboardButtonSimpleWebView` (`tl:715`) → `messages.RequestSimpleWebView{bot, url, platform, …}` (`tl:2595`); URL `t.me/<bot>/<app>?startapp=` → `messages.RequestAppWebView{peer, app: InputBotApp::ShortName{bot_id, short_name} (tl:1684), start_param, platform, write_allowed}` (`tl:2617`); главное приложение бота → `messages.RequestMainWebView` (`tl:2645`); `KeyboardButtonCallback` → `GetBotCallbackAnswer` и далее по ответу | Результат во всех случаях — `webViewResultUrl{query_id?, url}` (`tl:1546`), где `url` содержит `#tgWebAppData=…` (initData, подписанные для этого бота). **На этом MTProto заканчивается** |
| Ввод PIN | внутри веб-страницы (JS) | Вариант A — headless Chromium (например, `chromiumoxide`/CDP): загрузить URL, ввести PIN, прочитать результат. Тяжёлая зависимость, отпечаток устройства, хрупкость. Вариант B — повторить HTTP-вызовы бэкенда мини-приложения с `initData`: недокументированный API, ломается при обновлении фронтенда. Вариант C (`SPEC.md:148, 181`) — полуручной режим, владелец платит руками. **SPEC сейчас выбирает C**, а справка CryptoBot «никогда не вводить PIN в сторонних ботах и на сайтах» (`SPEC.md:181`) делает A и B нарушением правил с риском блокировки кошелька |
| Подтверждение | новое сообщение бота «счёт оплачен» и/или правка карточки | фикстур нет |

**Риски оплаты:**

- **P1.** Отсутствие PIN или выбор A/B/C — решение владельца (B1, В1 `SPEC.md:1748`).
- **P2.** Оплата неидемпотентна: `max_attempts=1`. Неясный исход сверяется повторным открытием счёта (карточка показывает статус — проверить) и балансом.
- **P3.** Мини-приложение может требовать `messages.ProlongWebView{peer, bot, query_id}` (`tl:2594`) при длинной сессии.
- **P4.** Параметр `platform` (`"android"`, `"ios"`, `"tdesktop"`, `"web"`) может менять поведение приложения.
- **P5.** Если PIN можно отключить и оплата пойдёт callback-кнопкой в чате — это путь без веб-приложения (`SPEC.md:146`: «основной путь… **если** подтверждение не уходит в мини-приложение»).
- **P6.** Для xRocket-счетов (`inv_…`, `SPEC.md:66`) поток не описан совсем.

### 8.4 Выбор валюты кнопкой (общий механизм)

- Найти кнопку по нормализованному тексту (тикер), получить `data: Vec<u8>` и `msg_id` экрана, вызвать `GetBotCallbackAnswer` через `money`.
- Ждать правку того же msg_id (`EditMessage` с новым `edit_date`) или новое сообщение `id > msg_id`.
- Успех — экран, распознанный парсером как «следующий шаг». Иначе `Unknown` → пауза.
- Риски:
  - `requires_password` у кнопки (`tl:703`) требует SRP 2FA (`InputCheckPasswordSRP`). Для денег — сразу `MANUAL_REVIEW`.
  - `alert: true` с текстом ошибки: парсить `BotCallbackAnswer.message`.
  - Правка может прийти раньше ответа RPC — оба пути надо слушать.

### 8.5 Чего ещё нет в lovec, но нужно exch

1. **Чтение баланса** (экран кошелька в боте) для сверки «каждые 5–15 минут» (`SPEC.md:14`) без API. Ни фикстур, ни парсера нет.
2. **Список своих чеков** (`/checks`, `/cheques`) — для сверки после Unknown при создании чека и для поиска невостребованных.
3. **Отмена или удаление невостребованного чека** (возврат денег).
4. **Notice «ваш чек активировал …»** — подтверждение выплаты и кто её забрал. Есть только needle `replies.rs:96-99`.
5. **Входящие переводы** («Вы получили … от @…») как отдельный класс.
6. **Карточка счёта** (сумма, актив, статус, срок, мульти-счёт).
7. **Догон истории после простоя**: `catch_up:true` + `getHistory` по каждой операции в полёте.
8. **Задержка ответа бота** — метрика.
9. **Капча** (§4) и **web-app-only чеки** `mc_`+10.

### 8.6 Конфликты, которые снимает только владелец

| Документ | Что сказано | Что требуется теперь |
|---|---|---|
| `CLAUDE.md` «Что это», правило 5 | выплаты через API, ключи `spend_id/transferId/withdrawalId` | ключи на `random_id` и шаги операций юзербота; формат `ord-<id>-payout` сохранить, но идемпотентность — через random_id |
| `SPEC.md:147` | инлайн-чеки — запасной путь, «без идемпотентности» | инлайн — **основной** путь; идемпотентность через random_id `sendInlineBotResult` (проверить) |
| `SPEC.md:148, 181` | мини-приложение и PIN «отвергнуто» | владелец описал оплату через мини-приложение с PIN — решить A/B/C |
| `SPEC.md:56-57, 65-66` | `startBot` | `/start` текстом (доказано lovec) или `startBot` (проверить) |
| `SPEC.md:1410` | «в grammers 0.10 есть хранилище в памяти с экспортом» | экспорта нет (`gs:storages/memory.rs:23-31`) → свой `Session` |
| `SPEC.md` §10 governor | 1 действие / 1,5 s | + бюджет в минуту (F1) |

### 8.7 Что снять у владельца: режим записи

Сделать `exch capture --account <name>`: только чтение, **ничего не отправляет**. Он подписан на апдейты и пишет в JSON каждое `Message` из личек `@send`, `@xrocket` и из «Избранного»:

- `id`, `date`, `edit_date`, `out`, `via_bot_id`, `message`, `entities`;
- `reply_markup` с **типом** каждой кнопки, `text`, `url` и `data` (base64), флагом `requires_password`;
- медиа-тип.

Владелец в это время выполняет потоки руками в Telegram Desktop **на тестовом аккаунте**. Нажатия кнопок не видны, видны их последствия — правки. Трафик web view не виден, его владелец описывает словами или скринами.

Список захвата:

1. Активация чеков обеих платформ: успех (сумма и актив), уже активирован (нами и не нами), не найден, не для нас, пароль (приглашение, неверный, верный), подписка, Premium, капча (кнопки, медиа), мультичек, `mc_`+10. Это `SPEC.md:1750` В3 и `SPEC.md:1755` В8.
2. Инлайн-создание: запрос `@send 10usdt` и `@xrocket 10`. Сохранить сырой `messages.BotResults`: на один запрос можно выполнить `getInlineBotResults` через reads-клиент, это не создаёт чек, если I1 = «на выборе» — сначала проверить по балансу. Затем отправка в «Избранное», вид заглушки, правка, тип кнопки со ссылкой, формат кода. Проверить I1, I2 и I9.
3. Меню-создание: каждый экран, тексты кнопок, приглашение ввести сумму, итоговое сообщение.
4. Счёт CryptoBot: карточка до и после выбора валюты, тип кнопки «Оплатить», URL web view (замаскированный), есть ли PIN, можно ли отключить PIN, сообщение «оплачено», повторное открытие оплаченного счёта. То же для xRocket.
5. Экран баланса, список своих чеков, удаление чека.
6. Notice «ваш чек активировал …» и входящий перевод.
7. Язык ответов при `lang_code` en и ru.
8. Решения B1–B4 и подтверждение user id ботов живым resolve.

---

## 9. Рекомендуемая архитектура юзербота exch

### 9.1 Слои (крейт `userbot`)

```
engine ──(WalletUserbot: activate_check / create_check / pay_invoice / read_balance)──▶ userbot::flows
userbot::flows      шаговые сценарии; каждый шаг = запись в operations (step, random_id, our_msg_id)
userbot::actor      один на аккаунт: очередь команд, ОДНО денежное действие в полёте, governor+FloodGate,
                    Conversation (владеет чатом бота до конца сценария), роутер unsolicited, watchdog
userbot::transport  трейт WalletTransport ─┬─ GrammersTransport (grammers 0.10, PgEncryptedSession)
                                           └─ SimTransport (feature "fakes": in-memory симулятор ботов)
parsers             links, markers, replies::{cryptobot,xrocket}, buttons, inline, amount (Decimal)
```

### 9.2 Трейт транспорта

```rust
pub struct BotPeer(pub i64);                       // закреплённый user id
pub enum Chat { Bot(BotPeer), SavedMessages }

pub struct RawMessage {                            // всё, что нужно парсерам и wallet_messages
    pub chat: Chat, pub id: i32, pub out: bool, pub date: DateTime<Utc>,
    pub edit_date: Option<DateTime<Utc>>, pub via_bot_id: Option<i64>,
    pub text: String, pub entities: Vec<Entity>, pub buttons: Vec<Vec<Button>>,
    pub media_kind: Option<MediaKind>, pub raw_tl: Vec<u8>,   // сериализованный tl::types::Message
}
pub enum Button {
    Callback { text: String, data: Vec<u8>, requires_password: bool },
    Url { text: String, url: String }, WebView { text: String, url: String },
    SimpleWebView { text: String, url: String }, SwitchInline { text: String, query: String },
    Copy { text: String, copy_text: String }, Other { text: String },
}
pub enum SendOutcome { Delivered { msg: Option<RawMessage>, msg_id: Option<i32> }, Rejected(RpcErr), Unknown(TransportErr) }
pub enum CallbackOutcome { Answered { alert: bool, message: Option<String>, url: Option<String> }, Rejected(RpcErr), Unknown(TransportErr) }
pub struct InlineResults { pub query_id: i64, pub items: Vec<InlineItem>, pub switch_pm: Option<String>, pub cache_time: i32 }
pub struct InlineItem { pub id: String, pub title: Option<String>, pub description: Option<String>, pub text: String, pub buttons: Vec<Vec<Button>> }
pub enum WebViewTarget { Button { url: String }, Simple { url: String }, App { short_name: String, start_param: Option<String> }, Main }
pub enum WalletEvent { New(RawMessage), Edited(RawMessage), Deleted { chat: Chat, ids: Vec<i32> } }

#[trait_variant::make(Send)]   // или явные Box<dyn Future>; edition 2024
pub trait WalletTransport {
    async fn send_text(&self, chat: Chat, text: &str, random_id: i64) -> SendOutcome;          // tl:2441
    async fn start_bot(&self, bot: BotPeer, param: &str, random_id: i64) -> SendOutcome;       // = send_text("/start …"); опц. tl:2475
    async fn press_callback(&self, bot: BotPeer, msg_id: i32, data: &[u8]) -> CallbackOutcome; // tl:2490, никогда не повторяется
    async fn inline_query(&self, bot: BotPeer, query: &str) -> Result<InlineResults, TErr>;   // tl:2484, peer = PeerSelf
    async fn send_inline_result(&self, query_id: i64, result_id: &str, random_id: i64) -> SendOutcome; // tl:2486 → «Избранное»
    async fn request_webview(&self, bot: BotPeer, target: WebViewTarget) -> Result<SecretString /*url c tgWebAppData*/, TErr>; // tl:2593/2595/2617/2645
    async fn read_history(&self, chat: Chat, min_id: i32, limit: u32) -> Result<Vec<RawMessage>, TErr>; // tl:2434
    async fn get_messages(&self, chat: Chat, ids: &[i32]) -> Result<Vec<Option<RawMessage>>, TErr>;    // gc:client/messages.rs:1111
    fn events(&self) -> tokio::sync::mpsc::Receiver<WalletEvent>;                             // только чаты Bot(pinned) и SavedMessages
    async fn ping(&self) -> Result<Duration, TErr>;                                            // tl::functions::Ping
}
```

Правила реализации:

- **`GrammersTransport`.**
  - Денежные методы — через `money` (NoRetries), чтение — через `reads`.
  - Каждый вызов в `tokio::time::timeout`, классификация по §3.3.
  - `events()` фильтрует апдейты по закреплённым id и self. Исходящие принимаются только в «Избранном» при `via_bot_id ∈ pinned`.
  - Для `UpdateShortMessage` с нужными кнопками — `get_messages`.
  - FLOOD → `FloodGate` → `Rejected` с `flood_wait()`.
  - Watchdog и пересоздание пула внутри (§5.2).
- **`SimTransport`** (`feature = "fakes"`, как фейки в `wallets`). Это модель CryptoBot и xRocket в памяти:
  - балансы по активам;
  - чеки: код, сумма, актив, состояние, привязка, пароль, капча, подписка;
  - счета, инлайн-результаты, экраны меню с callback-`data`;
  - **реестр random_id на аккаунт** (повтор → прежний результат, во время выполнения → `RANDOM_ID_DUPLICATE`);
  - id сообщений по чатам.

  Тексты ответов берутся **из тех же фикстур**, что `parsers/tests/fixtures/<platform>/<case>/`. Фикстуры — единый источник для парсеров и симулятора.

  Инъекции сбоев:
  - «выполнил и вернул таймаут» (эффект применён, исход `Unknown`);
  - RPC 500;
  - `FLOOD_WAIT_X`, `PEER_FLOOD`;
  - ответ только правкой;
  - сначала `InProgress`, потом правка;
  - ответ не приходит;
  - ответ после дедлайна;
  - unsolicited-сообщения (перевод, Notice, ручные действия);
  - инлайн-заглушка без ссылки и правка через N s;
  - `BOT_RESPONSE_TIMEOUT` на callback после применения эффекта;
  - бот в состоянии «ожидаю сумму» от прошлого потока.

  Инварианты проверяются в каждом тесте:
  - ≤1 эффекта на `random_id`;
  - ≤1 эффекта на callback-нажатие;
  - балансы симулятора = леджер;
  - ни одного сообщения вне закреплённых чатов.

### 9.3 Актор и сценарии

- **Actor.** Команды — `mpsc`, ответы — `oneshot`. В полёте одна денежная команда (правило 11). `Conversation` получает все `WalletEvent` чата бота, пока сценарий не закончится (паттерн `dialogs.rs:84-92`). Остальные события идут в роутер unsolicited: Notice, переводы, Unknown → CRITICAL. Перед роутингом каждое событие пишется в `wallet_messages` (правило 8).
- **Governor.** Порт `RateLimiter` (`poller.rs:26-60`): 1,5 s, бюджет 6–8 в минуту, `penalize` при FLOOD, дедлайн паузы в БД.
- **Flows.** Каждый — явная машина шагов. Шаг сохраняется **до** вызова (`step`, `random_id`), результат — после.
  - `activate`: `/start` → ответ / пароль / капча / повтор xRocket-невалида / сверка.
  - `create_check_inline`: query → выбор → `send_inline_result` → ожидание ссылки (событие или опрос) → проверка ссылки.
  - `create_check_menu`: команда → callback × N → сумма текстом → ссылка.
  - `pay_invoice`: `/start IV…` → карточка → валюта (callback) → «Оплатить» → **решение B4**: веб-приложение или полуручной режим.
  - После рестарта (`kill -9`, критерий M3 `SPEC.md:1692`) сценарий продолжается с последнего сохранённого шага. Денежный шаг без random_id не повторяется, сразу идёт сверка (`read_history` / `get_messages` / экран баланса).
- **Watchdog и супервизор.** `ping` каждые 10 s, 3 сбоя → пересоздать транспорт. Паника runner (`JoinError::is_panic`) → пересоздать актор, CRITICAL, пауза направлений аккаунта. Auth-ошибки (`AUTH_NAMES`) → статус `revoked`.
- **CLI.** `exch login`, `exch capture` (§8.7). `exch check-config` проверяет закреплённые id живым resolve (только в check-config) и уникальность сессий.

### 9.4 Тесты

| Уровень | Что | Инструмент |
|---|---|---|
| `parsers` | фикстуры каждого экрана и ответа + `insta`; порядок приоритетов классификатора; proptest для ссылок (5 форм, testnet, whitelist) и сумм (Decimal) | `parsers/tests/fixtures/<platform>/<case>/` |
| Адаптер grammers | §3.3 при сетевых сбоях; один random_id на операцию; callback ровно 1 раз | `fake_tg` (§7) |
| Актор и сценарии | все сценарии §9.3 × все инъекции сбоев | `SimTransport` |
| Engine | заявки end-to-end, ≤1 внешний эффект на ключ, леджер сбалансирован, балансы симулятора = леджер | `SimTransport` + `sqlx::test` |
| Ручной | тестовый аккаунт: activate, inline-создание, меню-создание, (оплата по решению B4) | чек-лист M3/M6 |