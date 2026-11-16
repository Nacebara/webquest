-- П-10 (картинка принята владельцем 18.11): решённая тревога — правкой всех копий («⚪ Решено ·
-- Игнат — вернул(а) деньги»), без кнопок. Отметка, что копия уже поправлена, — чтобы править
-- один раз и не трогать после перезапуска.
ALTER TABLE alert_copies ADD COLUMN closed_at TIMESTAMPTZ;

-- Тревоги, решённые до этой миграции, не правим задним числом: их копии старые.
UPDATE alert_copies c SET closed_at = now()
  FROM alerts a
 WHERE a.id = c.alert_id AND a.resolved_at IS NOT NULL;
