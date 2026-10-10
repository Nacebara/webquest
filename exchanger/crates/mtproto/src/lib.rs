//! Настоящий транспорт юзербота exch: MTProto через grammers 0.10.0 (crates.io, слой TL 227).
//!
//! Реализует `userbot::Transport` ([`GrammersTransport`]) для одного выделенного аккаунта и
//! даёт помощник входа ([`login()`]) для будущей команды `exch login`. Тяжёлые зависимости
//! MTProto живут только здесь: `userbot` и `engine` знают лишь трейт (DESIGN v0.2 §4).
//!
//! # Модули
//!
//! - [`config`] — api_id/api_hash, закреплённые боты (CryptoBot `1559501630` `@send`,
//!   xRocket `5014831088` `@xrocket`), параметры устройства (`lang_code = ru`), таймауты;
//! - [`crypto`] — XChaCha20-Poly1305: [`crypto::seal`] / [`crypto::open`] с `key_version`, AAD
//!   с меткой аккаунта, nonce 24 байта на каждое сохранение, открытый текст в `Zeroizing`;
//! - [`session`] — своя реализация `grammers_session::Session`: в памяти только auth key,
//!   DC, свой аккаунт и боты кошельков, сохранение — только зашифрованным ([`SessionStore`]);
//! - [`errors`] — ошибки grammers → `TransportError` (что считается «исход неизвестен»);
//! - [`convert`] — TL → `RawMessage` (кнопки, сущности в UTF-16, `via_bot`), поиск нашего
//!   сообщения в ответе по `random_id`, сверка ботов;
//! - [`requests`] — сырые запросы: `sendMessage` (`no_webpage`, заданный `random_id`),
//!   `getBotCallbackAnswer`, `getInlineBotResults` (peer = self), `sendInlineBotResult`
//!   в «Избранное», `requestWebView` / `requestSimpleWebView` / `requestAppWebView`, `getHistory`;
//! - [`transport`] — [`GrammersTransport`]: подключение, сверка ботов при старте, поток
//!   апдейтов в `broadcast`, сторож и переподключение;
//! - [`login`] — телефон → код → пароль 2FA, [`TerminalPrompt`] для терминала.
//!
//! # Деньги и идемпотентность
//!
//! - `send_text` и `send_inline` отправляют ровно тот `random_id`, который дал вызывающий;
//!   транспорт сам ничего не повторяет. Повтор с тем же `random_id` после неясного исхода
//!   безопасен: сервер вернёт прежний результат или `RANDOM_ID_DUPLICATE`
//!   (→ `TransportError::RandomIdDuplicate`).
//! - Таймаут, обрыв, `Dropped`, RPC 5xx и `BOT_RESPONSE_TIMEOUT` → `Timeout` / `Disconnected`
//!   (`outcome_unknown() == true`). Нажатие кнопки и web view идут через клиент без
//!   автоповторов (`NoRetries`): grammers по умолчанию молча повторяет запрос после `Io`.
//! - `FLOOD_WAIT_X` → `FloodWait(X)` без верхнего предела; паузу держит актор аккаунта.
//!
//! # Что проверено, а что нет
//!
//! Сети к Telegram в среде разработки нет, поэтому **живьём не проверено ничего, что ходит
//! в сеть**. Офлайн проверено двумя уровнями тестов.
//!
//! Интеграционные (`tests/fake_transport.rs`): настоящий grammers 0.10, настоящее шифрование
//! MTProto 2.0 и сериализация TL против поддельного сервера на 127.0.0.1 (`tests/support`,
//! порт lovec `fake_tg`; логику ответов задаёт тест):
//!
//! - старт: `get_me`, сверка ботов по сохранённому access hash и через `resolveUsername` по
//!   username из конфига (с сохранением в сессию), отказ при чужом id или username, 401;
//! - `send_text` уходит ровно с данным `random_id` и `no_webpage`; повтор → `RandomIdDuplicate`;
//! - «сервер выполнил и не ответил» → `Timeout`, сам транспорт не повторяет, повтор вызывающего
//!   тем же `random_id` идёт по новому соединению и даёт `RandomIdDuplicate`; ответ бота,
//!   пришедший, пока запрос «висел», догоняется после пересоздания пула (`getDifference`
//!   от сохранённого pts);
//! - нажатие после обрыва соединения не повторяется (ровно один `getBotCallbackAnswer`),
//!   `BOT_RESPONSE_TIMEOUT` → исход неизвестен, `FLOOD_WAIT` возвращается сразу;
//! - инлайн: запрос от `inputPeerSelf`, отправка в «Избранное» с данным `random_id`, сообщение
//!   с URL-кнопкой из ответа; история и выбор `requestSimpleWebView` / `requestWebView`;
//! - поток апдейтов через message box grammers: только чаты ботов и «Избранное», короткая
//!   форма, правки; переподключение после обрыва со стороны сервера и догон `getDifference`;
//! - потеря авторизации после старта; сессия с pts сохраняется только зашифрованной;
//! - вход (`tests/fake_login.rs`): телефон → код с повтором после неверного, сохранение и
//!   последующее подключение транспорта, рабочая сессия без входа, замена отозванной сессии,
//!   «нужна регистрация», отказ телефона. Пароль 2FA (SRP) поддельный сервер не умеет.
//!
//! Модульные:
//!
//! - преобразование сообщений и разметки TL → `RawMessage` (все типы кнопок, reply-клавиатура,
//!   UTF-16-смещения, «Избранное» и `via_bot`, чужие чаты, правки, короткие апдейты);
//! - поиск отправленного сообщения по `random_id` в `Updates` (через настоящую сериализацию
//!   TL), результаты инлайн-запроса, ответ на нажатие, URL web view, страницы истории;
//! - построение запросов: флаги, peer «Избранного», `random_id` в теле, окно истории;
//! - отображение ошибок grammers (FLOOD_WAIT, RANDOM_ID_DUPLICATE, 401, 5xx, Io, CRC…);
//! - шифрование сессии: round-trip, свежий nonce, обнаружение любой порчи, чужой ключ,
//!   чужой аккаунт, подмена версии ключа; формат снимка сессии и отказ на мусоре;
//! - сессия grammers: кэшируются только свой аккаунт и закреплённые боты, сохранение
//!   «срочных» и «ленивых» изменений, повтор после сбоя хранилища;
//! - сверка ботов по id/username/флагу `bot`; маска телефона; отказ входа, если сохранённая
//!   сессия не расшифровывается (она не перезаписывается).
//!
//! **Не проверено живьём** (нужен прогон на тестовом аккаунте, `exch selftest`):
//!
//! - подключение к настоящим DC, создание auth key (в тестах ключ задан заранее), вход на
//!   настоящем сервере, пароль 2FA (`auth.checkPassword`, SRP) и миграция DC при входе;
//! - настоящие @send и @xrocket: id и username взяты из фикстур lovec, `getUsers` /
//!   `resolveUsername` на них не вызывались;
//! - что настоящий сервер на повтор с тем же `random_id` отвечает `RANDOM_ID_DUPLICATE` или
//!   прежним результатом (в тестах так отвечает поддельный сервер; AUDIT-2.md:306 — «на
//!   реальном аккаунте не проверял»), и как боты отвечают на `sendMessage` в слое 227;
//! - инлайн-режим ботов: создаётся ли чек при запросе или при отправке (LOVEC-PORTING §8.1, I1),
//!   настоящий вид ответа `Updates` на `sendInlineBotResult`;
//! - ответы ботов на `getBotCallbackAnswer`, `requestWebView` / `requestSimpleWebView` /
//!   `requestAppWebView` и значение `platform` для мини-приложения CryptoBot;
//! - семантика окна `getHistory` (`offset_id = after_id + 1`, `add_offset = -limit`) на
//!   настоящем сервере — поддельный сервер лишь проверяет, какие параметры уходят;
//! - догон `catch_up` после долгого простоя, «тихое» соединение в бою (TCP жив, сервер молчит),
//!   паника runner grammers на странных данных настоящего сервера.

pub mod config;
pub mod convert;
pub mod crypto;
pub mod errors;
pub mod login;
pub mod requests;
pub mod session;
pub mod transport;

pub use config::{DeviceInfo, MtprotoConfig, PinnedBot, PinnedBots, Timeouts};
pub use crypto::{CryptoError, SealedSession, SessionKey, open, seal};
pub use login::{LoginError, LoginOutcome, LoginPrompt, TerminalPrompt, login, mask_phone};
pub use session::{EncryptedSession, MemoryStore, SessionStore, StoreError};
pub use transport::{ConnectError, GrammersTransport};
