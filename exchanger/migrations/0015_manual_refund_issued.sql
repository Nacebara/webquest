-- Проверка 19.11, Д-19.11 (картинка принята владельцем 20.11): неясный возврат на ручном разборе
-- нашёлся в кошельке — «Возврат выдан» со ссылкой на чек закрывает заявку возвратом.
-- Переход MANUAL_REVIEW → REFUNDED; в коде — domain::fsm::ALLOWED (32), диаграмма 08.
INSERT INTO order_state_transitions (from_state, to_state) VALUES ('MANUAL_REVIEW', 'REFUNDED');
