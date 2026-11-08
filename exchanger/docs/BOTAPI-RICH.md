# Rich-сообщения Bot API 10.3 — выжимка для exch

Источник: core.telegram.org/bots/api (Bot API 10.3), сверено 8 ноября 2026. Здесь — только то, на чём
строится интерфейс exch (фаза 1 PLAN-v1).

## Формат

- `sendRichMessage(chat_id, rich_message: InputRichMessage, reply_markup?, reply_parameters?, …)`;
  правка — `editMessageText(…, rich_message)` (тот же метод, что для текста).
- `InputRichMessage` — **ровно одно** из `html`, `markdown`, `blocks`; плюс `media` (файлы для ссылок
  `tg://photo?id=…` в html/markdown), `is_rtl`, `skip_entity_detection`.
- exch пишет `html` (`ui::html::Frag`, `ui::doc::Doc`): это официальный формат, не обходной путь.
  Гарант пишет `blocks` — тоже верно; разница только в записи.

## Теги rich-HTML, нужные макетам

`<h1>…<h6>`, `<p>`, `<b> <i> <u> <s> <code> <mark> <sub> <sup> <tg-spoiler>`, `<a href>`,
`<tg-emoji emoji-id="…">`, `<tg-time unix="…" format="wDT">` (время в поясе читателя),
`<footer>`, `<hr/>`, `<ul>/<ol>`, чек-лист `<li><input type="checkbox" checked>…`,
`<blockquote>…<cite>`, `<blockquote expandable>`, `<aside>`,
`<table bordered striped compact>` с `<caption>`, `<th>`, `<td align valign colspan rowspan>`,
`<details open><summary>`, `<img src="tg://photo?id=chart"/>` (картинка из `media` — графики прямо
в карточке), `<tg-button type="callback_data" style="success|danger|primary|link" data="…">`,
`<tg-button-row align="…">` — кнопки внутри текста (10.3).

Только эти теги; именованные сущности — `&lt; &gt; &amp; &quot; &apos; &nbsp; &hellip; &mdash;
&ndash; &lsquo; &rsquo; &ldquo; &rdquo;` и любые числовые.

## Лимиты

32 768 символов текста (вместе с alt-текстом эмодзи), 500 блоков (с вложенными, строками таблиц,
пунктами списков), вложенность 16, 50 медиа, 20 колонок таблицы.

## Кнопки (`InlineKeyboardButton`)

- `style`: `danger` (красная), `success` (зелёная), `primary` (синяя); нет — по умолчанию клиента.
- `icon_custom_emoji_id` — премиум-эмодзи перед текстом кнопки.
- `disabled: {}` — неактивная кнопка (например, «Отправляю…»).
- Кастомные эмодзи в тексте и на кнопках — только если **у владельца бота Telegram Premium** (или
  у бота купленные имена на Fragment). Нет Premium — Telegram отказывает: бот сам переходит на
  обычные эмодзи и запоминает это (PLAN-v1, 1.2).
