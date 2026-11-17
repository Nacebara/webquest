-- «Спасти резерв» (решение владельца 17.11.2026, вариант Б).
--
-- Владелец одной кнопкой выводит весь баланс кошелька юзербота в чек: юзербот создаёт чек
-- на всю сумму через меню бота кошелька и присылает ссылку только владельцу, кошелёк встаёт
-- на паузу. На случай, когда аккаунту юзербота грозит блокировка или чужой вход: деньги
-- уходят в чек, который владелец забирает своим аккаунтом.
--
-- Чек создаётся так же, как выплата: операция outbox `create_payout_check` с одной
-- попыткой, неясный исход — поиск созданного чека, затем решение владельца. Проводка —
-- `capital_out` (деньги ушли владельцу) в момент, когда чек создан.
CREATE TABLE rescues (
    id                  BIGSERIAL PRIMARY KEY,
    platform            TEXT NOT NULL CHECK (platform IN ('cryptobot', 'xrocket')),
    wallet_account_id   SMALLINT NOT NULL REFERENCES wallet_accounts(id),
    asset               TEXT NOT NULL REFERENCES assets(code),
    amount              NUMERIC(38,18) NOT NULL CHECK (amount > 0),
    -- Сколько было в кошельке по замеру перед созданием чека (у CryptoBot сумма чека может
    -- быть меньше: не больше лимита без код-пароля).
    measured            NUMERIC(38,18) NOT NULL CHECK (measured >= amount),
    status              TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
                            'pending',   -- операция в очереди или в работе
                            'issued',    -- чек создан, ссылка у владельца
                            'unknown',   -- исход неясен, чек не найден: решает владелец
                            'failed')),  -- чек не создан
    check_url           TEXT,
    requested_by        BIGINT NOT NULL REFERENCES staff(tg_user_id),
    reason              TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at         TIMESTAMPTZ,
    CHECK (status <> 'issued' OR check_url IS NOT NULL)
);
-- На кошелёк не больше одного незавершённого спасения.
CREATE UNIQUE INDEX rescues_one_open_per_wallet ON rescues (wallet_account_id)
    WHERE status IN ('pending', 'unknown');

ALTER TABLE operations ADD COLUMN rescue_id BIGINT REFERENCES rescues(id);
-- У операции ровно один хозяин: заявка, ребалансировка, движение инвестора или спасение.
ALTER TABLE operations DROP CONSTRAINT operations_check;
ALTER TABLE operations ADD CONSTRAINT operations_check
    CHECK (num_nonnulls(order_id, rebalance_id, invest_move_id, rescue_id) = 1
           OR kind = 'read_balance');
-- У спасения одна живая или успешная операция.
CREATE UNIQUE INDEX operations_one_per_rescue ON operations (rescue_id)
    WHERE rescue_id IS NOT NULL AND status IN ('pending', 'dispatched', 'unknown', 'succeeded');
