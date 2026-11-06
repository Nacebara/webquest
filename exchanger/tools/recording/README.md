# Запись → фикстуры

Как из файла `exch listen` / `exch record` (docs/RECORDING.md) сделать живые фикстуры парсеров.

```bash
python3 -I tools/recording/sanitize.py exch-record.jsonl /tmp/clean.json
python3 -I tools/recording/live_fixtures.py /tmp/clean.json crates/parsers/tests/fixtures/live_wallet_replies.json
cargo test -p parsers --test live
```

`sanitize.py` убирает «Избранное» целиком (там личное) и сообщения о выводах в сеть и обменах
(адреса и хэши транзакций). Коды чеков и счетов (`CQ…`, `IV…`, `t_…`, `mc_…`, `inv_…`), номера
счетов и платежей он перемешивает с сохранением формата, хэши оплаты заменяет. Имена людей и
магазинов заменяет на «Ivan» / «Shop» по месту в тексте, без списка настоящих имён. Чужие ссылки
заменяет на `example.invalid`. Исходную запись в репозиторий не кладём; после очистки проверь
результат глазами.

`live_fixtures.py` размечает смысл каждого ответа по значению текста, а не по ответу
парсера. Тест `parsers/tests/live.rs` проверяет, что парсеры понимают ответ так же.
Новый вид ответа — новое правило разметки здесь и фикстура.
