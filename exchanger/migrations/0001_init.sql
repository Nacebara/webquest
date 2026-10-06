-- =====================================================================
-- Бот-обменник xRocket <-> CryptoBot — схема БД (PostgreSQL 16)
-- Миграция 0001_init. Деньги — только NUMERIC(38,18), никаких float.
-- Инварианты, которые держит сама БД (а не только код):
--   I1. Один чек (start_param) — не больше одной незавершённой заявки.
--   I2. На заявку — не больше одной «расчётной» операции (выплата,
--       возврат или оплата счёта) в статусах pending/dispatched/unknown/succeeded.
--   I3. Ключ идемпотентности операции уникален глобально (= spend_id/transferId).
--   I4. Каждая проводка сбалансирована по каждому активу (сумма = 0).
--   I5. Леджер и журнал переходов — только добавление (UPDATE/DELETE запрещены).
--   I6. Каждый сырой ответ бота кошелька сохраняется один раз.
--
-- Файл без BEGIN/COMMIT: sqlx оборачивает миграцию в транзакцию сам,
-- вручную применять одной транзакцией: psql -1 -d <db> -f schema.sql.
-- migrations/0001_init.sql — байт-в-байт копия (проверяет тест storage).
-- =====================================================================

CREATE EXTENSION IF NOT EXISTS pgcrypto;  -- gen_random_bytes для публичных кодов

-- ---------------------------------------------------------------------
-- Справочники
-- ---------------------------------------------------------------------

CREATE TABLE assets (
    code        TEXT PRIMARY KEY CHECK (code ~ '^[A-Z0-9]{2,10}$'),   -- USDT, TON
    title       TEXT NOT NULL,
    is_stable   BOOLEAN NOT NULL DEFAULT FALSE
);

-- Параметры актива на конкретной платформе: код, точность, минимумы.
CREATE TABLE platform_assets (
    platform        TEXT NOT NULL CHECK (platform IN ('cryptobot', 'xrocket')),
    asset           TEXT NOT NULL REFERENCES assets(code),
    platform_code   TEXT NOT NULL,                 -- 'USDT' | 'TON' (CryptoBot), 'USDT' | 'TONCOIN' (xRocket)
    decimals        SMALLINT NOT NULL CHECK (decimals BETWEEN 0 AND 18),  -- точность платформы
    payout_step     NUMERIC(38,18) NOT NULL CHECK (payout_step > 0),      -- шаг округления выплат (USDT: 0.01)
    min_transfer    NUMERIC(38,18) NOT NULL CHECK (min_transfer >= 0),
    max_transfer    NUMERIC(38,18),                -- NULL = нет известного лимита
    min_check       NUMERIC(38,18) NOT NULL DEFAULT 0,
    enabled         BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (platform, asset),
    UNIQUE (platform, platform_code)
);

-- Кошельки: личный (юзербот) и приложение (API) на каждой платформе.
CREATE TABLE wallet_accounts (
    id              SMALLSERIAL PRIMARY KEY,
    platform        TEXT NOT NULL CHECK (platform IN ('cryptobot', 'xrocket')),
    kind            TEXT NOT NULL CHECK (kind IN ('personal', 'app')),
    label           TEXT NOT NULL UNIQUE,          -- 'cb:personal', 'xr:app'
    userbot_id      SMALLINT,                      -- для personal: какой аккаунт Telegram
    is_active       BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((kind = 'personal') = (userbot_id IS NOT NULL))
);

-- Аккаунты Telegram юзербота. Сессия хранится ТОЛЬКО зашифрованной
-- (XChaCha20-Poly1305, ключ из переменной окружения, key_version для ротации).
CREATE TABLE userbot_accounts (
    id                  SMALLSERIAL PRIMARY KEY,
    label               TEXT NOT NULL UNIQUE,      -- 'ub-cryptobot-1'
    tg_user_id          BIGINT UNIQUE,
    phone_masked        TEXT NOT NULL,             -- '+31******42' — полный номер не храним
    session_ciphertext  BYTEA,
    session_nonce       BYTEA,
    key_version         SMALLINT NOT NULL DEFAULT 1,
    status              TEXT NOT NULL DEFAULT 'new'
                        CHECK (status IN ('new', 'active', 'flood_wait', 'logged_out', 'banned', 'disabled')),
    flood_wait_until    TIMESTAMPTZ,
    last_ok_at          TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((session_ciphertext IS NULL) = (session_nonce IS NULL))
);

ALTER TABLE wallet_accounts
    ADD CONSTRAINT wallet_accounts_userbot_fk FOREIGN KEY (userbot_id) REFERENCES userbot_accounts(id);

-- Курсор чтения чата с ботом кошелька: после рестарта догоняем историю.
CREATE TABLE wallet_chat_cursors (
    userbot_id          SMALLINT NOT NULL REFERENCES userbot_accounts(id),
    bot_peer_id         BIGINT NOT NULL,           -- закреплённый ID @send / @xrocket (не username!)
    last_message_id     INTEGER NOT NULL DEFAULT 0,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (userbot_id, bot_peer_id)
);

-- ---------------------------------------------------------------------
-- Пользователи, персонал, баны
-- ---------------------------------------------------------------------

CREATE TABLE users (
    tg_user_id          BIGINT PRIMARY KEY,
    username            TEXT,
    first_name          TEXT,
    language_code       TEXT,
    is_premium          BOOLEAN,
    referrer_id         BIGINT REFERENCES users(tg_user_id),
    daily_limit_usd     NUMERIC(38,18),            -- персональный лимит; NULL = по умолчанию
    risk_note           TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    first_completed_at  TIMESTAMPTZ,
    CHECK (referrer_id IS NULL OR referrer_id <> tg_user_id)
);

CREATE TABLE staff (
    tg_user_id          BIGINT PRIMARY KEY,
    role                TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'operator')),
    display_name        TEXT NOT NULL,
    max_manual_payout   NUMERIC(38,18),            -- потолок ручной выплаты для роли/человека
    added_by            BIGINT REFERENCES staff(tg_user_id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    disabled_at         TIMESTAMPTZ
);
-- Владелец ровно один.
CREATE UNIQUE INDEX staff_single_owner ON staff ((role)) WHERE role = 'owner' AND disabled_at IS NULL;

CREATE TABLE bans (
    id                  BIGSERIAL PRIMARY KEY,
    user_id             BIGINT NOT NULL REFERENCES users(tg_user_id),
    reason              TEXT NOT NULL CHECK (length(reason) >= 3),
    banned_by           BIGINT NOT NULL REFERENCES staff(tg_user_id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at          TIMESTAMPTZ,               -- NULL = бессрочно
    lifted_at           TIMESTAMPTZ,
    lifted_by           BIGINT REFERENCES staff(tg_user_id),
    lift_reason         TEXT
);
CREATE UNIQUE INDEX bans_one_active ON bans (user_id) WHERE lifted_at IS NULL;

-- ---------------------------------------------------------------------
-- Направления и цены
-- ---------------------------------------------------------------------

CREATE TABLE directions (
    code                TEXT PRIMARY KEY CHECK (code IN (
                            'xr_to_cb_check', 'cb_to_xr_check',
                            'pay_cb_invoice', 'pay_xr_invoice')),
    kind                TEXT NOT NULL CHECK (kind IN ('check_exchange', 'invoice_payment')),
    source_platform     TEXT NOT NULL CHECK (source_platform IN ('cryptobot', 'xrocket')),
    target_platform     TEXT NOT NULL CHECK (target_platform IN ('cryptobot', 'xrocket')),
    asset               TEXT NOT NULL REFERENCES assets(code),
    fee_pct             NUMERIC(9,6) NOT NULL CHECK (fee_pct >= 0 AND fee_pct < 20),   -- 2.5 = 2.5 %
    min_fee             NUMERIC(38,18) NOT NULL CHECK (min_fee >= 0),
    skew_k_pct          NUMERIC(9,6) NOT NULL DEFAULT 0,      -- сила ценового рычага
    fee_floor_pct       NUMERIC(9,6) NOT NULL DEFAULT 0,
    fee_cap_pct         NUMERIC(9,6) NOT NULL DEFAULT 10,
    min_amount          NUMERIC(38,18) NOT NULL CHECK (min_amount > 0),
    max_amount          NUMERIC(38,18) NOT NULL,
    new_user_max_amount NUMERIC(38,18) NOT NULL,
    fiat_buffer_pct     NUMERIC(9,6) NOT NULL DEFAULT 1,      -- только для счетов в фиате
    is_open             BOOLEAN NOT NULL DEFAULT FALSE,
    auto_paused         BOOLEAN NOT NULL DEFAULT FALSE,       -- пауза системой (резерв, парсер, сверка)
    pause_reason        TEXT,
    updated_by          BIGINT REFERENCES staff(tg_user_id),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (source_platform <> target_platform),
    CHECK (max_amount >= min_amount),
    CHECK (new_user_max_amount <= max_amount),
    CHECK (fee_floor_pct <= fee_pct AND fee_pct <= fee_cap_pct)
);

-- ---------------------------------------------------------------------
-- Заявки
-- ---------------------------------------------------------------------

CREATE TABLE orders (
    id                  BIGSERIAL PRIMARY KEY,
    public_id           TEXT NOT NULL UNIQUE
                        DEFAULT upper(encode(gen_random_bytes(5), 'hex')),  -- 10 символов, показываем клиенту
    user_id             BIGINT NOT NULL REFERENCES users(tg_user_id),
    direction           TEXT NOT NULL REFERENCES directions(code),
    state               TEXT NOT NULL DEFAULT 'NEW' CHECK (state IN (
                            'NEW', 'AWAITING_FUNDS', 'AWAITING_PASSWORD',
                            'INTAKE_PENDING', 'INTAKE_UNKNOWN', 'RECEIVED',
                            'PAYOUT_PENDING', 'INVOICE_PAY_PENDING',
                            'REFUND_PENDING', 'MANUAL_REVIEW',
                            'COMPLETED', 'REFUNDED', 'REJECTED', 'INTAKE_FAILED',
                            'EXPIRED', 'CANCELLED', 'CLOSED_MANUAL')),
    is_terminal         BOOLEAN GENERATED ALWAYS AS (state IN (
                            'COMPLETED', 'REFUNDED', 'REJECTED', 'INTAKE_FAILED',
                            'EXPIRED', 'CANCELLED', 'CLOSED_MANUAL')) STORED,
    version             INTEGER NOT NULL DEFAULT 0,          -- оптимистическая блокировка
    asset               TEXT NOT NULL REFERENCES assets(code),

    -- Вход: чек клиента (или наш счёт для оплаты клиентом)
    intake_method       TEXT CHECK (intake_method IN ('check', 'our_invoice')),
    intake_platform     TEXT NOT NULL CHECK (intake_platform IN ('cryptobot', 'xrocket')),
    intake_start_param  TEXT,                                 -- 'CQ…' / 'mc_…' — ценность до активации, в логах маскируется
    intake_amount       NUMERIC(38,18) CHECK (intake_amount > 0),          -- сколько реально пришло

    -- Чужой счёт, который мы оплачиваем (для invoice_payment)
    invoice_platform    TEXT CHECK (invoice_platform IN ('cryptobot', 'xrocket')),
    invoice_start_param TEXT,                                 -- 'IV…' / 'inv_…'
    invoice_amount      NUMERIC(38,18),                       -- сумма в активе
    invoice_fiat        TEXT,
    invoice_fiat_amount NUMERIC(38,18),
    invoice_expires_at  TIMESTAMPTZ,

    -- Котировка и расчёт
    quote_amount_in     NUMERIC(38,18),                       -- сколько клиент должен прислать
    quote_expires_at    TIMESTAMPTZ,
    rate_used           NUMERIC(38,18),
    fee_pct_applied     NUMERIC(9,6),
    fee_amount          NUMERIC(38,18) CHECK (fee_amount >= 0),
    payout_amount       NUMERIC(38,18) CHECK (payout_amount > 0),
    payout_method       TEXT CHECK (payout_method IN ('transfer', 'check', 'invoice')),
    refund_amount       NUMERIC(38,18) CHECK (refund_amount > 0),

    -- Сообщение прогресса (редактируется по шагам)
    progress_chat_id    BIGINT,
    progress_message_id INTEGER,

    failure_code        TEXT,
    failure_detail      TEXT,
    manual_reason       TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    received_at         TIMESTAMPTZ,
    finished_at         TIMESTAMPTZ,

    CHECK (intake_method IS DISTINCT FROM 'check' OR intake_start_param IS NOT NULL OR state IN ('NEW', 'AWAITING_FUNDS', 'REJECTED', 'EXPIRED', 'CANCELLED')),
    CHECK (NOT is_terminal OR finished_at IS NOT NULL),
    CHECK (state <> 'COMPLETED' OR (intake_amount IS NOT NULL AND payout_amount IS NOT NULL)),
    -- Заявку, по которой хоть что-то пришло, нельзя «просто закрыть»: только выплата или возврат.
    CHECK (state NOT IN ('EXPIRED', 'CANCELLED', 'REJECTED', 'INTAKE_FAILED') OR intake_amount IS NULL)
);

-- I1: один чек — одна живая заявка (повторная ссылка → «уже в работе»).
CREATE UNIQUE INDEX orders_intake_param_live
    ON orders (intake_platform, intake_start_param)
    WHERE intake_start_param IS NOT NULL AND NOT is_terminal;
-- Чужой счёт: одновременно оплачиваем не больше одного раза.
CREATE UNIQUE INDEX orders_invoice_param_live
    ON orders (invoice_platform, invoice_start_param)
    WHERE invoice_start_param IS NOT NULL AND NOT is_terminal;
-- Успешно оплаченный счёт больше не принимаем никогда.
CREATE UNIQUE INDEX orders_invoice_param_completed
    ON orders (invoice_platform, invoice_start_param)
    WHERE invoice_start_param IS NOT NULL AND state = 'COMPLETED';

CREATE INDEX orders_user_created ON orders (user_id, created_at DESC);
CREATE INDEX orders_live_state ON orders (state, updated_at) WHERE NOT is_terminal;
CREATE INDEX orders_created ON orders (created_at);
CREATE INDEX orders_direction_finished ON orders (direction, finished_at) WHERE state = 'COMPLETED';

-- Разрешённые переходы FSM. Код (domain::fsm) — основная проверка,
-- триггер — страховка от ошибки в коде и ручного UPDATE.
CREATE TABLE order_state_transitions (
    from_state  TEXT NOT NULL,
    to_state    TEXT NOT NULL,
    PRIMARY KEY (from_state, to_state)
);
INSERT INTO order_state_transitions (from_state, to_state) VALUES
    ('NEW', 'REJECTED'), ('NEW', 'INTAKE_PENDING'), ('NEW', 'AWAITING_FUNDS'),
    ('AWAITING_FUNDS', 'INTAKE_PENDING'), ('AWAITING_FUNDS', 'RECEIVED'),
    ('AWAITING_FUNDS', 'EXPIRED'), ('AWAITING_FUNDS', 'CANCELLED'),
    ('AWAITING_FUNDS', 'REFUND_PENDING'),          -- TTL истёк, а часть денег уже пришла
    ('INTAKE_PENDING', 'RECEIVED'), ('INTAKE_PENDING', 'INTAKE_FAILED'),
    ('INTAKE_PENDING', 'AWAITING_PASSWORD'), ('INTAKE_PENDING', 'INTAKE_UNKNOWN'),
    ('INTAKE_PENDING', 'AWAITING_FUNDS'),           -- оплата счёта: чек не принят или недоплата — ждём ещё
    ('AWAITING_PASSWORD', 'INTAKE_PENDING'), ('AWAITING_PASSWORD', 'INTAKE_FAILED'),
    ('AWAITING_PASSWORD', 'AWAITING_FUNDS'),
    ('INTAKE_UNKNOWN', 'RECEIVED'), ('INTAKE_UNKNOWN', 'INTAKE_FAILED'),
    ('INTAKE_UNKNOWN', 'MANUAL_REVIEW'), ('INTAKE_UNKNOWN', 'AWAITING_FUNDS'),
    ('RECEIVED', 'PAYOUT_PENDING'), ('RECEIVED', 'INVOICE_PAY_PENDING'),
    ('RECEIVED', 'REFUND_PENDING'), ('RECEIVED', 'MANUAL_REVIEW'),
    ('PAYOUT_PENDING', 'COMPLETED'), ('PAYOUT_PENDING', 'MANUAL_REVIEW'),
    ('INVOICE_PAY_PENDING', 'COMPLETED'), ('INVOICE_PAY_PENDING', 'REFUND_PENDING'),
    ('INVOICE_PAY_PENDING', 'MANUAL_REVIEW'),
    ('REFUND_PENDING', 'REFUNDED'), ('REFUND_PENDING', 'MANUAL_REVIEW'),
    ('MANUAL_REVIEW', 'PAYOUT_PENDING'), ('MANUAL_REVIEW', 'REFUND_PENDING'),
    ('MANUAL_REVIEW', 'COMPLETED'), ('MANUAL_REVIEW', 'CLOSED_MANUAL'),
    ('MANUAL_REVIEW', 'INVOICE_PAY_PENDING');

CREATE FUNCTION orders_guard_transition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.state IS DISTINCT FROM OLD.state THEN
        IF NOT EXISTS (SELECT 1 FROM order_state_transitions
                        WHERE from_state = OLD.state AND to_state = NEW.state) THEN
            RAISE EXCEPTION 'illegal order transition % -> % (order %)', OLD.state, NEW.state, OLD.id;
        END IF;
        -- Выплатить/закрыть «выплачено» можно только при отсутствии активной операции возврата и наоборот —
        -- это держит индекс operations_one_settle_per_order; здесь только версия и время.
    END IF;
    NEW.version := OLD.version + 1;
    NEW.updated_at := now();
    RETURN NEW;
END $$;

CREATE TRIGGER orders_transition_guard BEFORE UPDATE ON orders
    FOR EACH ROW EXECUTE FUNCTION orders_guard_transition();

-- Журнал переходов (append-only)
CREATE TABLE order_transitions (
    id          BIGSERIAL PRIMARY KEY,
    order_id    BIGINT NOT NULL REFERENCES orders(id),
    from_state  TEXT,
    to_state    TEXT NOT NULL,
    actor       TEXT NOT NULL,                     -- 'system' | 'staff:<id>' | 'user'
    reason      TEXT,
    data        JSONB NOT NULL DEFAULT '{}'::jsonb,
    at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX order_transitions_order ON order_transitions (order_id, id);

-- Ввод от клиента, которого ждёт заявка (пароль чека).
CREATE TABLE pending_inputs (
    order_id    BIGINT PRIMARY KEY REFERENCES orders(id),
    user_id     BIGINT NOT NULL REFERENCES users(tg_user_id),
    kind        TEXT NOT NULL CHECK (kind IN ('check_password')),
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX pending_inputs_one_per_user ON pending_inputs (user_id);

-- ---------------------------------------------------------------------
-- Ребалансировки
-- ---------------------------------------------------------------------

CREATE TABLE rebalances (
    id                  BIGSERIAL PRIMARY KEY,
    from_wallet_id      SMALLINT NOT NULL REFERENCES wallet_accounts(id),
    to_wallet_id        SMALLINT NOT NULL REFERENCES wallet_accounts(id),
    asset               TEXT NOT NULL REFERENCES assets(code),
    amount              NUMERIC(38,18) NOT NULL CHECK (amount > 0),
    network             TEXT CHECK (network IN ('TON', 'TRX', 'BSC', 'ETH', 'SOL', 'internal')),
    address             TEXT,                       -- только из белого списка (settings.rebalance_whitelist)
    state               TEXT NOT NULL DEFAULT 'planned' CHECK (state IN (
                            'planned', 'confirmed', 'sent', 'arrived', 'completed', 'failed', 'cancelled')),
    withdrawal_id       TEXT UNIQUE,                -- идемпотентность xRocket /app/withdrawal
    tx_hash             TEXT,
    network_fee         NUMERIC(38,18),
    created_by          BIGINT REFERENCES staff(tg_user_id),
    confirmed_by        BIGINT REFERENCES staff(tg_user_id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (from_wallet_id <> to_wallet_id),
    CHECK (state NOT IN ('confirmed', 'sent', 'arrived', 'completed') OR confirmed_by IS NOT NULL)
);
CREATE INDEX rebalances_live ON rebalances (state) WHERE state NOT IN ('completed', 'failed', 'cancelled');

-- ---------------------------------------------------------------------
-- Операции с внешним миром (transactional outbox)
-- ---------------------------------------------------------------------

CREATE TABLE operations (
    id                  BIGSERIAL PRIMARY KEY,
    order_id            BIGINT REFERENCES orders(id),
    rebalance_id        BIGINT REFERENCES rebalances(id),
    kind                TEXT NOT NULL CHECK (kind IN (
                            'activate_check', 'submit_check_password',
                            'open_invoice', 'pay_invoice',
                            'create_intake_invoice',
                            'payout_transfer', 'payout_check',
                            'refund_transfer', 'refund_check',
                            'withdrawal', 'read_balance')),
    -- Роль в расчёте по заявке: 'settle' — то, чем заявка закрывается
    -- (выплата / возврат / оплата счёта); 'excess_refund' — возврат переплаты.
    money_role          TEXT NOT NULL CHECK (money_role IN ('intake', 'settle', 'excess_refund', 'aux')),
    via                 TEXT NOT NULL CHECK (via IN ('api', 'userbot')),
    platform            TEXT NOT NULL CHECK (platform IN ('cryptobot', 'xrocket')),
    wallet_account_id   SMALLINT REFERENCES wallet_accounts(id),
    status              TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
                            'pending', 'dispatched', 'succeeded', 'failed', 'unknown', 'cancelled')),
    idempotency_key     TEXT NOT NULL UNIQUE CHECK (length(idempotency_key) BETWEEN 8 AND 50), -- I3; = spend_id / transferId / withdrawalId
    start_param         TEXT,                       -- для activate_check / open_invoice / pay_invoice
    amount              NUMERIC(38,18),
    asset               TEXT REFERENCES assets(code),
    request             JSONB NOT NULL DEFAULT '{}'::jsonb,   -- без секретов
    response            JSONB,
    external_id         TEXT,                       -- transfer_id, check_id, invoice id
    attempts            INTEGER NOT NULL DEFAULT 0,
    max_attempts        INTEGER NOT NULL DEFAULT 8,
    next_attempt_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    dispatched_at       TIMESTAMPTZ,
    finished_at         TIMESTAMPTZ,
    CHECK ((order_id IS NULL) <> (rebalance_id IS NULL) OR kind = 'read_balance'),
    -- Ретраи вслепую разрешены только API-операциям с ключом идемпотентности
    -- и активации чека (повтор /start не может стоить нам денег).
    CHECK (via = 'api' OR kind IN ('activate_check', 'submit_check_password', 'open_invoice', 'read_balance') OR max_attempts = 1)
);
-- I2: на заявку не больше одной живой/успешной расчётной операции.
CREATE UNIQUE INDEX operations_one_settle_per_order
    ON operations (order_id)
    WHERE money_role = 'settle' AND status IN ('pending', 'dispatched', 'unknown', 'succeeded');
CREATE UNIQUE INDEX operations_one_excess_refund_per_order
    ON operations (order_id)
    WHERE money_role = 'excess_refund' AND status IN ('pending', 'dispatched', 'unknown', 'succeeded');
-- Один чек никогда не активируется двумя разными операциями.
CREATE UNIQUE INDEX operations_check_once
    ON operations (platform, start_param)
    WHERE kind = 'activate_check' AND status IN ('pending', 'dispatched', 'unknown', 'succeeded');
-- Чужой счёт оплачивается не больше одной операцией за всё время.
CREATE UNIQUE INDEX operations_invoice_pay_once
    ON operations (platform, start_param)
    WHERE kind = 'pay_invoice' AND status IN ('pending', 'dispatched', 'unknown', 'succeeded');
-- Очередь outbox-воркера.
CREATE INDEX operations_due ON operations (next_attempt_at) WHERE status = 'pending';
CREATE INDEX operations_inflight ON operations (status) WHERE status IN ('dispatched', 'unknown');
CREATE INDEX operations_order ON operations (order_id);

-- ---------------------------------------------------------------------
-- Сырые сообщения ботов кошельков (фикстуры, аудит, восстановление)
-- ---------------------------------------------------------------------

CREATE TABLE wallet_messages (
    id                  BIGSERIAL PRIMARY KEY,
    userbot_id          SMALLINT NOT NULL REFERENCES userbot_accounts(id),
    bot_peer_id         BIGINT NOT NULL,
    message_id          INTEGER NOT NULL,
    edit_date           INTEGER NOT NULL DEFAULT 0,   -- 0 = исходное сообщение, иначе unix-время правки
    is_outgoing         BOOLEAN NOT NULL,             -- наша команда или ответ бота
    text                TEXT NOT NULL,
    entities            JSONB,
    reply_markup        JSONB,                        -- кнопки (callback data, url)
    parsed_kind         TEXT,                         -- 'check_activated', 'check_expired', … | 'unknown'
    parser_version      TEXT,
    operation_id        BIGINT REFERENCES operations(id),
    received_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (userbot_id, bot_peer_id, message_id, edit_date)   -- I6
);
CREATE INDEX wallet_messages_op ON wallet_messages (operation_id);
CREATE INDEX wallet_messages_unknown ON wallet_messages (received_at) WHERE parsed_kind = 'unknown';

-- ---------------------------------------------------------------------
-- Леджер (двойная запись)
-- ---------------------------------------------------------------------

CREATE TABLE ledger_accounts (
    id                  SERIAL PRIMARY KEY,
    code                TEXT NOT NULL UNIQUE,       -- 'asset:cb:app:USDT', 'liability:payable:USDT', …
    type                TEXT NOT NULL CHECK (type IN ('asset', 'liability', 'equity', 'revenue', 'expense')),
    asset               TEXT NOT NULL REFERENCES assets(code),
    wallet_account_id   SMALLINT REFERENCES wallet_accounts(id),
    CHECK (type <> 'asset' OR wallet_account_id IS NOT NULL OR code LIKE 'asset:transit:%')
);
CREATE UNIQUE INDEX ledger_accounts_wallet_asset ON ledger_accounts (wallet_account_id, asset) WHERE wallet_account_id IS NOT NULL;

CREATE TABLE ledger_transactions (
    id                  BIGSERIAL PRIMARY KEY,
    kind                TEXT NOT NULL CHECK (kind IN (
                            'intake', 'payout', 'refund', 'excess_refund', 'excess_kept', 'invoice_paid',
                            'app_topup', 'rebalance_out', 'rebalance_in', 'network_fee', 'platform_fee',
                            'capital_in', 'capital_out', 'profit_sweep', 'manual_close', 'adjustment', 'loss')),
    order_id            BIGINT REFERENCES orders(id),
    rebalance_id        BIGINT REFERENCES rebalances(id),
    operation_id        BIGINT REFERENCES operations(id),
    memo                TEXT,
    created_by          TEXT NOT NULL DEFAULT 'system',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (kind NOT IN ('adjustment', 'loss', 'capital_in', 'capital_out', 'profit_sweep', 'manual_close') OR memo IS NOT NULL)
);
-- Одна проводка каждого вида на операцию: повторная обработка ответа не задвоит леджер.
CREATE UNIQUE INDEX ledger_tx_once_per_operation ON ledger_transactions (operation_id, kind) WHERE operation_id IS NOT NULL;
CREATE INDEX ledger_tx_order ON ledger_transactions (order_id);
CREATE INDEX ledger_tx_created ON ledger_transactions (created_at);

-- Знак: дебет > 0, кредит < 0. Баланс счёта актива = SUM(amount).
CREATE TABLE ledger_entries (
    id                  BIGSERIAL PRIMARY KEY,
    tx_id               BIGINT NOT NULL REFERENCES ledger_transactions(id),
    account_id          INTEGER NOT NULL REFERENCES ledger_accounts(id),
    asset               TEXT NOT NULL REFERENCES assets(code),
    amount              NUMERIC(38,18) NOT NULL CHECK (amount <> 0)
);
CREATE INDEX ledger_entries_account ON ledger_entries (account_id);
CREATE INDEX ledger_entries_tx ON ledger_entries (tx_id);

-- I4: проводка сбалансирована по каждому активу (проверяется в конце транзакции).
CREATE FUNCTION ledger_check_balanced() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    bad RECORD;
BEGIN
    SELECT e.asset, SUM(e.amount) AS s INTO bad
      FROM ledger_entries e
     WHERE e.tx_id = NEW.tx_id
     GROUP BY e.asset
    HAVING SUM(e.amount) <> 0
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'ledger tx % is unbalanced for %: %', NEW.tx_id, bad.asset, bad.s;
    END IF;
    IF (SELECT count(*) FROM ledger_entries WHERE tx_id = NEW.tx_id) < 2 THEN
        RAISE EXCEPTION 'ledger tx % must have at least two entries', NEW.tx_id;
    END IF;
    RETURN NULL;
END $$;

CREATE CONSTRAINT TRIGGER ledger_entries_balanced
    AFTER INSERT ON ledger_entries
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ledger_check_balanced();

-- Счёт и запись должны быть в одном активе.
CREATE FUNCTION ledger_check_asset() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT asset FROM ledger_accounts WHERE id = NEW.account_id) <> NEW.asset THEN
        RAISE EXCEPTION 'entry asset % does not match account asset', NEW.asset;
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER ledger_entries_asset
    BEFORE INSERT ON ledger_entries
    FOR EACH ROW EXECUTE FUNCTION ledger_check_asset();

-- I5: только добавление.
CREATE FUNCTION forbid_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME;
END $$;

CREATE TRIGGER ledger_entries_append_only BEFORE UPDATE OR DELETE ON ledger_entries
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER ledger_transactions_append_only BEFORE UPDATE OR DELETE ON ledger_transactions
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER order_transitions_append_only BEFORE UPDATE OR DELETE ON order_transitions
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- ---------------------------------------------------------------------
-- Резервирование (hold) под выплаты
-- ---------------------------------------------------------------------

CREATE TABLE holds (
    id                  BIGSERIAL PRIMARY KEY,
    order_id            BIGINT NOT NULL REFERENCES orders(id),
    wallet_account_id   SMALLINT NOT NULL REFERENCES wallet_accounts(id),
    asset               TEXT NOT NULL REFERENCES assets(code),
    amount              NUMERIC(38,18) NOT NULL CHECK (amount > 0),
    state               TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'consumed', 'released')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at           TIMESTAMPTZ,
    CHECK ((state = 'active') = (closed_at IS NULL))
);
CREATE UNIQUE INDEX holds_one_active_per_order ON holds (order_id) WHERE state = 'active';
CREATE INDEX holds_active_wallet ON holds (wallet_account_id, asset) WHERE state = 'active';

-- Доступный резерв = баланс по леджеру − активные hold'ы.
CREATE VIEW wallet_available AS
SELECT wa.id AS wallet_account_id, wa.label, la.asset,
       COALESCE(b.balance, 0) AS ledger_balance,
       COALESCE(h.held, 0) AS held,
       COALESCE(b.balance, 0) - COALESCE(h.held, 0) AS available
  FROM wallet_accounts wa
  JOIN ledger_accounts la ON la.wallet_account_id = wa.id
  LEFT JOIN (SELECT account_id, SUM(amount) AS balance FROM ledger_entries GROUP BY account_id) b
         ON b.account_id = la.id
  LEFT JOIN (SELECT wallet_account_id, asset, SUM(amount) AS held FROM holds WHERE state = 'active'
              GROUP BY wallet_account_id, asset) h
         ON h.wallet_account_id = wa.id AND h.asset = la.asset;

CREATE TABLE reserve_thresholds (
    wallet_account_id   SMALLINT NOT NULL REFERENCES wallet_accounts(id),
    asset               TEXT NOT NULL REFERENCES assets(code),
    target              NUMERIC(38,18) NOT NULL,
    low                 NUMERIC(38,18) NOT NULL,
    critical            NUMERIC(38,18) NOT NULL,
    high                NUMERIC(38,18) NOT NULL,      -- выше — излишек, кандидат на вывод прибыли/ребаланс
    PRIMARY KEY (wallet_account_id, asset),
    CHECK (critical <= low AND low <= target AND target <= high)
);

-- ---------------------------------------------------------------------
-- Сверка
-- ---------------------------------------------------------------------

CREATE TABLE reconciliations (
    id                  BIGSERIAL PRIMARY KEY,
    wallet_account_id   SMALLINT NOT NULL REFERENCES wallet_accounts(id),
    asset               TEXT NOT NULL REFERENCES assets(code),
    source              TEXT NOT NULL CHECK (source IN ('api', 'userbot', 'manual')),
    expected            NUMERIC(38,18) NOT NULL,
    actual              NUMERIC(38,18) NOT NULL,
    diff                NUMERIC(38,18) GENERATED ALWAYS AS (actual - expected) STORED,
    inflight_ops        INTEGER NOT NULL DEFAULT 0,   -- операции в полёте на момент замера
    verdict             TEXT NOT NULL CHECK (verdict IN ('ok', 'warn', 'pause', 'skipped_inflight')),
    at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX reconciliations_wallet_at ON reconciliations (wallet_account_id, at DESC);

-- ---------------------------------------------------------------------
-- Настройки, аудит, алерты, рассылки, курсы
-- ---------------------------------------------------------------------

CREATE TABLE settings (
    key                 TEXT PRIMARY KEY,
    value               JSONB NOT NULL,
    updated_by          BIGINT REFERENCES staff(tg_user_id),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE admin_audit (
    id                  BIGSERIAL PRIMARY KEY,
    staff_id            BIGINT NOT NULL REFERENCES staff(tg_user_id),
    action              TEXT NOT NULL,              -- 'direction.set_fee', 'order.retry_payout', 'user.ban', …
    target_type         TEXT,
    target_id           TEXT,
    before              JSONB,
    after               JSONB,
    reason              TEXT,
    double_confirmed    BOOLEAN NOT NULL DEFAULT FALSE,
    at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX admin_audit_at ON admin_audit (at DESC);
CREATE TRIGGER admin_audit_append_only BEFORE UPDATE OR DELETE ON admin_audit
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

CREATE TABLE alerts (
    id                  BIGSERIAL PRIMARY KEY,
    level               TEXT NOT NULL CHECK (level IN ('info', 'notice', 'warn', 'critical')),
    topic               TEXT NOT NULL CHECK (topic IN ('exchanges', 'errors', 'reserves', 'security', 'admin', 'digest')),
    dedup_key           TEXT NOT NULL,
    title               TEXT NOT NULL,
    payload             JSONB NOT NULL DEFAULT '{}'::jsonb,   -- уже замаскированное
    count               INTEGER NOT NULL DEFAULT 1,
    first_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_message_id     INTEGER,
    acked_by            BIGINT REFERENCES staff(tg_user_id),
    acked_at            TIMESTAMPTZ
);
CREATE UNIQUE INDEX alerts_open_dedup ON alerts (dedup_key) WHERE acked_at IS NULL;

CREATE TABLE broadcasts (
    id                  BIGSERIAL PRIMARY KEY,
    created_by          BIGINT NOT NULL REFERENCES staff(tg_user_id),
    text_html           TEXT NOT NULL,
    audience            TEXT NOT NULL CHECK (audience IN ('all', 'active_30d', 'completed_any', 'test')),
    state               TEXT NOT NULL DEFAULT 'draft' CHECK (state IN ('draft', 'confirmed', 'running', 'done', 'cancelled')),
    total               INTEGER NOT NULL DEFAULT 0,
    sent                INTEGER NOT NULL DEFAULT 0,
    failed              INTEGER NOT NULL DEFAULT 0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at          TIMESTAMPTZ,
    finished_at         TIMESTAMPTZ
);

CREATE TABLE broadcast_deliveries (
    broadcast_id        BIGINT NOT NULL REFERENCES broadcasts(id),
    user_id             BIGINT NOT NULL REFERENCES users(tg_user_id),
    status              TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed', 'blocked')),
    PRIMARY KEY (broadcast_id, user_id)
);

CREATE TABLE rates (
    base                TEXT NOT NULL,              -- 'USDT'
    quote               TEXT NOT NULL,              -- 'USD', 'RUB'
    source              TEXT NOT NULL CHECK (source IN ('cryptobot', 'xrocket')),
    rate                NUMERIC(38,18) NOT NULL CHECK (rate > 0),
    fetched_at          TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (base, quote, source)
);

-- Реферальная программа (опция v1.0)
CREATE TABLE referral_rewards (
    id                  BIGSERIAL PRIMARY KEY,
    referrer_id         BIGINT NOT NULL REFERENCES users(tg_user_id),
    order_id            BIGINT NOT NULL UNIQUE REFERENCES orders(id),
    asset               TEXT NOT NULL REFERENCES assets(code),
    amount              NUMERIC(38,18) NOT NULL CHECK (amount > 0),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    paid_operation_id   BIGINT REFERENCES operations(id)
);

-- ---------------------------------------------------------------------
-- Статистика: дневные агрегаты (материализованное представление,
-- REFRESH CONCURRENTLY раз в 5 минут; «сегодня» считаем онлайн).
-- ---------------------------------------------------------------------

CREATE MATERIALIZED VIEW stats_daily AS
SELECT date_trunc('day', o.created_at AT TIME ZONE 'UTC')::date AS day,
       o.direction,
       count(*)                                               AS orders_total,
       count(*) FILTER (WHERE o.state = 'COMPLETED')          AS orders_completed,
       count(*) FILTER (WHERE o.state = 'REFUNDED')           AS orders_refunded,
       count(*) FILTER (WHERE o.state IN ('INTAKE_FAILED', 'REJECTED')) AS orders_failed,
       count(*) FILTER (WHERE o.state = 'MANUAL_REVIEW')      AS orders_manual_open,
       COALESCE(SUM(o.intake_amount) FILTER (WHERE o.state = 'COMPLETED'), 0) AS turnover_in,
       COALESCE(SUM(o.payout_amount) FILTER (WHERE o.state = 'COMPLETED'), 0) AS turnover_out,
       COALESCE(SUM(o.fee_amount)    FILTER (WHERE o.state = 'COMPLETED'), 0) AS gross_fee,
       count(DISTINCT o.user_id)                              AS users_active,
       percentile_cont(0.5)  WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (o.finished_at - o.created_at)))
           FILTER (WHERE o.state = 'COMPLETED')               AS p50_seconds,
       percentile_cont(0.95) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (o.finished_at - o.created_at)))
           FILTER (WHERE o.state = 'COMPLETED')               AS p95_seconds
  FROM orders o
 GROUP BY 1, 2;
CREATE UNIQUE INDEX stats_daily_pk ON stats_daily (day, direction);

-- ---------------------------------------------------------------------
-- Начальные данные (значения — по умолчанию из SPEC §2; меняются в админке)
-- ---------------------------------------------------------------------

INSERT INTO assets (code, title, is_stable) VALUES
    ('USDT', 'Tether USD', TRUE),
    ('TON', 'Toncoin', FALSE);

-- Точность и минимумы — допущения до сверки с getCurrencies / /currencies/available (SPEC §10.3).
INSERT INTO platform_assets (platform, asset, platform_code, decimals, payout_step, min_transfer, max_transfer, min_check, enabled) VALUES
    ('cryptobot', 'USDT', 'USDT',    6, 0.01,  1.00, 25000, 0.10, TRUE),
    ('xrocket',   'USDT', 'USDT',    6, 0.01,  0.01, NULL,  0.01, TRUE),
    ('cryptobot', 'TON',  'TON',     9, 0.001, 0.30, NULL,  0.01, FALSE),
    ('xrocket',   'TON',  'TONCOIN', 9, 0.001, 0.001, NULL, 0.001, FALSE);

INSERT INTO directions (code, kind, source_platform, target_platform, asset, fee_pct, min_fee, skew_k_pct,
                        fee_floor_pct, fee_cap_pct, min_amount, max_amount, new_user_max_amount, is_open) VALUES
    ('xr_to_cb_check', 'check_exchange',  'xrocket',   'cryptobot', 'USDT', 2.5, 0.10, 0.75, 1.5, 3.5, 2, 300, 100, FALSE),
    ('cb_to_xr_check', 'check_exchange',  'cryptobot', 'xrocket',   'USDT', 1.5, 0.10, 0.75, 0.5, 3.0, 2, 300, 100, FALSE),
    ('pay_cb_invoice', 'invoice_payment', 'xrocket',   'cryptobot', 'USDT', 3.0, 0.20, 0.75, 2.0, 4.0, 2, 300, 100, FALSE),
    ('pay_xr_invoice', 'invoice_payment', 'cryptobot', 'xrocket',   'USDT', 2.0, 0.20, 0.75, 1.0, 3.5, 2, 300, 100, FALSE);

INSERT INTO settings (key, value) VALUES
    ('maintenance',            '{"on": false, "message": null}'),
    ('kill_switch_payouts',    '{"on": false}'),
    ('quote_ttl_seconds',      '900'),
    ('check_password_ttl_seconds', '600'),
    ('user_rate_limit',        '{"orders_per_10min": 5, "links_per_min": 10}'),
    ('daily_limit_usd',        '{"default": 1000, "new_user": 200}'),
    ('min_refund_amount',      '0.50'),
    ('reconcile_tolerance',    '{"warn": "0.01", "pause": "1.00"}'),
    ('rebalance_whitelist',    '[]'),
    ('custom_emoji',           '{}');
