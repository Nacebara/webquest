-- Рассылка «🔥 Выгодно» клиентам (решение владельца 9 ноября 2026). Уходит сама, когда на
-- направлении включился выгодный курс (автоматикой или вручную), если рассылка включена в
-- «Ребалансировке» (`settings.promo_broadcast`, включает владелец). Одному клиенту — не чаще
-- раза в 3 часа.
--
-- Журнал переживает перезапуск: отправка продолжается с курсора, а клиент помечается
-- (`users.promo_notified_at`) до отправки — второй раз то же сообщение он не получит.

CREATE TABLE promo_broadcasts (
    id          BIGSERIAL PRIMARY KEY,
    direction   TEXT NOT NULL,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    -- Последний клиент, до которого дошла очередь (по tg_user_id).
    cursor_user BIGINT NOT NULL DEFAULT 0,
    -- Сколько клиентов было на старте (без забаненных).
    total       INTEGER NOT NULL DEFAULT 0 CHECK (total >= 0),
    delivered   INTEGER NOT NULL DEFAULT 0 CHECK (delivered >= 0),
    -- Писали меньше 3 часов назад.
    skipped     INTEGER NOT NULL DEFAULT 0 CHECK (skipped >= 0),
    -- Заблокировали бота или Telegram отказал.
    failed      INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0)
);

-- Одна рассылка за раз: второе включение, пока первая идёт, новой не заводит.
CREATE UNIQUE INDEX promo_broadcasts_one_running ON promo_broadcasts ((finished_at IS NULL))
    WHERE finished_at IS NULL;

ALTER TABLE users ADD COLUMN promo_notified_at TIMESTAMPTZ;
