# Картинки «на утверждение»

Новый или изменённый экран сначала показывается владельцу картинкой в стиле утверждённых макетов,
код — только после его «да» (правило 20–21 CLAUDE.md).

1. Группа — `docs/mockups/widgets/<имя>.js`. Каждый экран — копия утверждённого из
   `docs/mockups/template.html` (в подписи — имя образца), меняется только то, что нужно; затем
   сверка с «Правилами интерфейса» (вкладка «Правила» в макетах) строка за строкой.
2. Эмодзи и lottie (один раз за сессию):
   `EXCH_JSON=1 node tools/emoji/tgs.js /tmp/emoji`;
   `npm pack lottie-web@5.12.2 && tar -xzf lottie-web-5.12.2.tgz`.
3. `python3 -I tools/mockups/proposal.py docs/mockups/widgets/<имя>.js /tmp/emoji package/build/player/lottie_light.min.js /tmp/page.html`
4. `node tools/mockups/shot.js /tmp/page.html docs/mockups/widgets/<имя>.png` — отказ, если хоть
   один значок не стал премиум-эмодзи, нет lottie или на странице ошибка. Такая картинка не
   отправляется.
5. Посмотреть картинку глазами: обрезанные кнопки, переносы, лишние строки.

20.11: две картинки ушли владельцу без премиум-эмодзи — lottie грузилась с cdnjs, а из среды
снимка он закрыт; предупреждение скрипта было пропущено. Отсюда встраивание и жёсткий отказ.

## Реестр принятых экранов и сторож (решение владельца 21.11)

- `python3 -I tools/mockups/accepted.py` → `docs/mockups/accepted.json`: все принятые экраны из
  `template.html` и принятых разделов `widgets/samples.json`. Новый принятый раздел — дописать в
  `SECTIONS` с датой; новая принятая картинка — экраны копируются в `template.html`.
- `docs/mockups/accepted-ledger.json` — книга: каждый id реестра со статусом `match` (и `screens` —
  экраны бота), `todo` (`kind`, `why`), `superseded` (`by`, `why`) или `later` (виджет).
- Тесты `crates/ui/tests/accepted.rs` и `crates/ops/tests/accepted.rs` печатают «по макету N из M».
  Экран исправлен — статус `match` и его экраны; тест сам скажет, если `todo` уже совпадает.
