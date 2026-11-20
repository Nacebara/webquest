-- Проверка 19.11, Д-19.6 (картинка last-fixes.png принята владельцем 20.11, вариант Б): чек
-- пополнения меньше минимума инвестора в инвестиции не идёт — его сумма возвращается чеком того
-- же кошелька, как «Меньше минимума» у обмена. Раньше такой чек молча зачислялся.
--
-- Два новых статуса пополнения: 'refunding' — чек возврата создаётся (удержание держит деньги),
-- 'refunded' — чек возврата создан (`check_url`), в капитал инвестора деньги не попадали.
ALTER TABLE invest_moves DROP CONSTRAINT invest_moves_status_check;
ALTER TABLE invest_moves ADD CONSTRAINT invest_moves_status_check CHECK (status IN (
    'requested', 'processing', 'done', 'rejected', 'failed', 'manual', 'refunding', 'refunded'));

-- У движения инвестора — по одной живой или успешной операции каждой роли: приход (активация
-- чека пополнения) и расчёт (чек вывода или чек возврата пополнения). Раньше индекс стоял на
-- движение целиком, и у пополнения после активации не могло быть чека возврата. Двух чеков
-- (двух расчётных операций) на одно движение по-прежнему быть не может.
DROP INDEX operations_one_per_invest_move;
CREATE UNIQUE INDEX operations_one_per_invest_move_role
    ON operations (invest_move_id, money_role)
    WHERE invest_move_id IS NOT NULL AND status IN ('pending', 'dispatched', 'unknown', 'succeeded');
