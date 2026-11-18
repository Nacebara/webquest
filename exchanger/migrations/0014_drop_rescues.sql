-- «Спасти резерв» убрано из проекта (решение владельца 17.11.2026). Миграция 0013 остаётся:
-- в базах, где она уже применена, без неё бот не запустится. Здесь схема возвращается к
-- виду до 0013.
--
-- Если спасения уже были, молча удалять их нельзя: это записи о деньгах владельца. Тогда
-- миграция останавливается, и решает человек.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM rescues) OR EXISTS (SELECT 1 FROM operations WHERE rescue_id IS NOT NULL) THEN
        RAISE EXCEPTION 'В базе есть записи «Спасти резерв» — миграция 0014 их не удаляет. Сначала разберите их вручную.';
    END IF;
END $$;

DROP INDEX operations_one_per_rescue;
ALTER TABLE operations DROP CONSTRAINT operations_check;
ALTER TABLE operations DROP COLUMN rescue_id;
ALTER TABLE operations ADD CONSTRAINT operations_check
    CHECK (num_nonnulls(order_id, rebalance_id, invest_move_id) = 1 OR kind = 'read_balance');
DROP TABLE rescues;
