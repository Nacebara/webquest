-- =====================================================================
-- Проверка инвариантов schema.sql на живом PostgreSQL 16.
-- Запуск:  createdb exch_test && psql -d exch_test -f schema.sql && psql -d exch_test -f schema_test.sql
-- Ожидание: каждый блок «must FAIL» печатает ERROR, остальные — без ошибок.
-- В CI: тот же сценарий как интеграционный тест storage (SPEC §11.2).
-- =====================================================================
\set ON_ERROR_STOP 0
-- setup
INSERT INTO userbot_accounts (label, phone_masked) VALUES ('ub-xr','+31******01'),('ub-cb','+31******02');
INSERT INTO wallet_accounts (platform, kind, label, userbot_id) VALUES
 ('xrocket','personal','xr:personal',1),('cryptobot','personal','cb:personal',2);
INSERT INTO wallet_accounts (platform, kind, label) VALUES ('xrocket','app','xr:app'),('cryptobot','app','cb:app');
INSERT INTO ledger_accounts (code,type,asset,wallet_account_id) VALUES
 ('asset:xr:personal:USDT','asset','USDT',1),('asset:cb:personal:USDT','asset','USDT',2),
 ('asset:xr:app:USDT','asset','USDT',3),('asset:cb:app:USDT','asset','USDT',4);
INSERT INTO ledger_accounts (code,type,asset) VALUES ('liability:payable:USDT','liability','USDT'),
 ('revenue:fee:USDT','revenue','USDT'),('equity:capital:USDT','equity','USDT');
-- capital 1000 into cb:app
BEGIN;
INSERT INTO ledger_transactions (kind, memo, created_by) VALUES ('capital_in','initial','owner');
INSERT INTO ledger_entries (tx_id,account_id,asset,amount) VALUES (currval('ledger_transactions_id_seq'),4,'USDT',1000),(currval('ledger_transactions_id_seq'),7,'USDT',-1000);
COMMIT;
INSERT INTO users (tg_user_id, username) VALUES (111,'client');
INSERT INTO orders (user_id,direction,asset,intake_method,intake_platform,intake_start_param,state)
 VALUES (111,'xr_to_cb_check','USDT','check','xrocket','mc_abc','INTAKE_PENDING');
\echo '--- T1 duplicate live check must FAIL'
INSERT INTO orders (user_id,direction,asset,intake_method,intake_platform,intake_start_param,state)
 VALUES (111,'xr_to_cb_check','USDT','check','xrocket','mc_abc','INTAKE_PENDING');
\echo '--- T2 unbalanced ledger tx must FAIL'
BEGIN;
INSERT INTO ledger_transactions (kind, order_id) VALUES ('intake',1);
INSERT INTO ledger_entries (tx_id,account_id,asset,amount) VALUES (currval('ledger_transactions_id_seq'),1,'USDT',100),(currval('ledger_transactions_id_seq'),5,'USDT',-97.5);
COMMIT;
\echo '--- T3 balanced intake OK (fee is recognised at payout, not at intake)'
BEGIN;
INSERT INTO operations (order_id,kind,money_role,via,platform,wallet_account_id,idempotency_key,status) VALUES (1,'activate_check','intake','userbot','xrocket',1,'ord-1-intake','succeeded');
INSERT INTO ledger_transactions (kind, order_id, operation_id) VALUES ('intake',1,1);
INSERT INTO ledger_entries (tx_id,account_id,asset,amount) VALUES (currval('ledger_transactions_id_seq'),1,'USDT',100),(currval('ledger_transactions_id_seq'),5,'USDT',-100);
COMMIT;
\echo '--- T4 same operation posted twice must FAIL'
BEGIN;
INSERT INTO ledger_transactions (kind, order_id, operation_id) VALUES ('intake',1,1);
ROLLBACK;
\echo '--- T5 settle op OK, second settle (refund) must FAIL'
INSERT INTO operations (order_id,kind,money_role,via,platform,wallet_account_id,idempotency_key,amount,asset) VALUES (1,'payout_transfer','settle','api','cryptobot',4,'ord-1-payout',97.5,'USDT');
INSERT INTO operations (order_id,kind,money_role,via,platform,wallet_account_id,idempotency_key,amount,asset) VALUES (1,'refund_transfer','settle','api','xrocket',3,'ord-1-refund',100,'USDT');
\echo '--- T6 userbot pay_invoice with retries must FAIL'
INSERT INTO operations (order_id,kind,money_role,via,platform,idempotency_key,max_attempts) VALUES (1,'pay_invoice','aux','userbot','cryptobot','ord-1-pay-x',5);
\echo '--- T7 ledger append-only must FAIL'
UPDATE ledger_entries SET amount = 1 WHERE id = 1;
DELETE FROM ledger_entries WHERE id = 1;
\echo '--- T8 hold + available view'
INSERT INTO holds (order_id,wallet_account_id,asset,amount) VALUES (1,4,'USDT',97.5);
SELECT label, ledger_balance, held, available FROM wallet_available ORDER BY wallet_account_id;
\echo '--- T9 second active hold for same order must FAIL'
INSERT INTO holds (order_id,wallet_account_id,asset,amount) VALUES (1,4,'USDT',1);
\echo '--- T10 complete order, then same check param may be reused only by a NEW order after terminal'
BEGIN;
INSERT INTO ledger_transactions (kind, order_id, operation_id) VALUES ('payout',1,(SELECT id FROM operations WHERE idempotency_key='ord-1-payout'));
INSERT INTO ledger_entries (tx_id,account_id,asset,amount) VALUES (currval('ledger_transactions_id_seq'),5,'USDT',100),(currval('ledger_transactions_id_seq'),4,'USDT',-97.5),(currval('ledger_transactions_id_seq'),6,'USDT',-2.5);
UPDATE operations SET status='succeeded', finished_at=now() WHERE idempotency_key='ord-1-payout';
UPDATE holds SET state='consumed', closed_at=now() WHERE order_id=1 AND state='active';
UPDATE orders SET state='RECEIVED', intake_amount=100, fee_amount=2.5, payout_amount=97.5, received_at=now() WHERE id=1;
UPDATE orders SET state='PAYOUT_PENDING' WHERE id=1;
UPDATE orders SET state='COMPLETED', finished_at=now() WHERE id=1;
COMMIT;
SELECT label, ledger_balance, held, available FROM wallet_available ORDER BY wallet_account_id;
SELECT a.code, SUM(e.amount) FROM ledger_entries e JOIN ledger_accounts a ON a.id=e.account_id GROUP BY a.code ORDER BY a.code;
\echo '--- T11 illegal transition COMPLETED -> REFUNDED must FAIL'
UPDATE orders SET state='REFUNDED' WHERE id=1;
\echo '--- T12 expiring an order that already received money must FAIL'
INSERT INTO orders (user_id,direction,asset,intake_method,intake_platform,state,invoice_platform,invoice_start_param,quote_amount_in)
 VALUES (111,'pay_cb_invoice','USDT','check','xrocket','AWAITING_FUNDS','cryptobot','IVtest',10.30);
UPDATE orders SET intake_amount=5 WHERE invoice_start_param='IVtest';
UPDATE orders SET state='EXPIRED', finished_at=now() WHERE invoice_start_param='IVtest';
\echo '--- T13 the same check activated by a second operation must FAIL'
INSERT INTO operations (order_id,kind,money_role,via,platform,idempotency_key,start_param) VALUES ((SELECT id FROM orders WHERE invoice_start_param='IVtest'),'activate_check','intake','userbot','xrocket','ord-x-intake-1','mc_zzz');
INSERT INTO operations (order_id,kind,money_role,via,platform,idempotency_key,start_param) VALUES ((SELECT id FROM orders WHERE invoice_start_param='IVtest'),'activate_check','intake','userbot','xrocket','ord-x-intake-2','mc_zzz');
\echo '--- T14 version bumps on every update'
SELECT id, state, version FROM orders ORDER BY id;
REFRESH MATERIALIZED VIEW stats_daily;
SELECT day, direction, orders_completed, turnover_in, gross_fee FROM stats_daily;
