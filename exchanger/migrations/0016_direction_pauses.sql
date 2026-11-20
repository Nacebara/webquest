-- Проверка 19.11, Д-19.9: причин автопаузы у направления может быть несколько — у каждой свой
-- кошелёк, из-за которого она стоит (без ответа, незнакомый ответ, недостача по сверке, вход
-- сброшен…). Снимается только своя причина; направление на паузе, пока есть хоть одна. Раньше
-- вторая причина не записывалась («уже на паузе»), а снятие первой снимало паузу целиком.
--
-- `directions.auto_paused` и `directions.pause_reason` остаются сводкой для чтения (пауза — есть
-- причина; показывается самая ранняя). Их ведут те же функции `storage::directions`.
CREATE TABLE direction_pauses (
    direction   TEXT NOT NULL REFERENCES directions(code) ON DELETE CASCADE,
    -- Кошелёк причины; '' — без кошелька (пауза одного направления и паузы, стоявшие до 0016).
    platform    TEXT NOT NULL CHECK (platform IN ('cryptobot', 'xrocket', '')),
    -- Вид причины — текст до двоеточия: повтор того же вида (сверка с новой суммой) заменяет текст.
    kind        TEXT NOT NULL CHECK (kind <> ''),
    reason      TEXT NOT NULL,
    since       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (direction, platform, kind)
);

-- Паузы, что стоят сейчас: причина известна, кошелёк — нет.
INSERT INTO direction_pauses (direction, platform, kind, reason)
SELECT code, '',
       COALESCE(NULLIF(btrim(split_part(COALESCE(pause_reason, ''), ':', 1)), ''), 'автопауза'),
       COALESCE(pause_reason, 'автопауза')
  FROM directions
 WHERE auto_paused;
