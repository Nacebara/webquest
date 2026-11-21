"""Реестр принятых экранов: docs/mockups/accepted.json.

Источники (решение владельца 21.11 — сверять всё принятое, а не по памяти):
  * docs/mockups/template.html — утверждённые экраны клиента (FLOW) и пульта (ADMIN); сюда же
    скопированы картинки 18–21.11;
  * docs/mockups/widgets/samples.json — разделы preview.html, принятые 13–16.11 (список
    SECTIONS ниже, даты — из заголовков разделов и NEXT.md / PROGRESS.md).

Не берутся: состояния «Сейчас» (как было до правки), «Утверждено · …» (копии template.html),
вариант «А» концепции (выбран «Б», 15.11), раздел «Клиент: находки на решение» 15.11 — его
«Предлагаю» вошли в концепцию Б (cx_errors, cx_flows), «Простой вид» убран (15.11).

Запуск: python3 -I tools/mockups/accepted.py  (нужен node). Тест-сторож читает результат.
"""

import json
import os
import re
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TEMPLATE = os.path.join(REPO, 'docs/mockups/template.html')
SAMPLES = os.path.join(REPO, 'docs/mockups/widgets/samples.json')
OUT = os.path.join(REPO, 'docs/mockups/accepted.json')

# Ключ samples.json → (дата принятия, раздел preview.html, бот, отложено ли до виджетов).
SECTIONS = {}
for keys, date, section, bot, later in [
    ('order_a calc_b calc_a invoice promo', '8.11', 'Клиент — утверждено (виджеты)', 'client', True),
    ('dash accounts reserves process code', '8.11', 'Пульт — утверждено (виджеты)', 'ops', True),
    ('card queue value rebalance withdraw maint system', '13.11', 'Позже', 'ops', True),
    ('inv_home inv_withdraw inv_calc', '13.11', 'Инвест-панель, вариант Б (виджеты)', 'client', True),
    ('inv_home_rich inv_deposit inv_status inv_howto', '13.11', 'Инвест-панель, вариант Б', 'client', False),
    ('ops_inv_list ops_inv_card ops_inv_approve ops_inv_admin', '13.11', 'Пульт: инвесторы', 'ops', False),
    ('emo_style emo_roles emo_role emo_ask', '13.11', 'Панель эмодзи · пульт', 'ops', False),
    ('ops_find ops_refusal ops_owner', '13.11', 'Пульт: денежные действия', 'ops', False),
    ('cl_privacy cl_link_bad', '13.11', 'Клиент: недостающие листы', 'client', False),
    ('ops_crit ops_crit_repeat', '14.11', 'Критичные тревоги', 'ops', False),
    ('dash_p3', '14.11', 'Главный экран при паузах (П3)', 'ops', True),
    ('cl_late cl_errors', '14.11', 'Клиент: итог после ожидания и кнопки под ошибками', 'client', False),
    ('ops_dir ops_tiers ops_client ops_promo ops_capital ops_kill ops_thresholds ops_alerts '
     'ops_support ops_intake ops_paid emo_home emo_sets', '14.11', 'Перерисовано по утверждённым', 'ops', False),
    ('cl_invoice_bad', '14.11', 'Перерисовано по утверждённым', 'client', False),
    ('cx_errors cx_flows', '15.11', 'Концепция Б: значок говорит, где деньги', 'client', False),
    ('cx_pult', '15.11', 'Концепция Б: пульт без обычных эмодзи', 'ops', False),
    ('pol_client', '16.11', 'Полировка: до → после', 'client', False),
    ('pol_ops', '16.11', 'Полировка: до → после', 'ops', False),
    ('pol_w_calc pol_w_invoice pol_w_dash', '16.11', 'Полировка: виджеты', 'client', True),
]:
    for k in keys.split():
        SECTIONS[k] = (date, section, bot, later)


def template_data():
    lines = open(TEMPLATE, encoding='utf-8').read().split('\n')
    fb = next(l for l in lines if l.startswith('const FB = '))
    start = next(i for i, l in enumerate(lines) if l.startswith('// ── разметка сообщений'))
    end = next(i for i, l in enumerate(lines) if l.startswith('const VIEWS = '))
    code = "'use strict';\n" + fb + '\n' + '\n'.join(lines[start:end]) + \
        '\nprocess.stdout.write(JSON.stringify({FLOW, ADMIN, DASH: [DASH_OK, DASH_FIRE, DASH_CALM]}));\n'
    with tempfile.NamedTemporaryFile('w', suffix='.js', delete=False, encoding='utf-8') as f:
        f.write(code)
        path = f.name
    try:
        out = subprocess.run(['node', path], check=True, capture_output=True, text=True).stdout
    finally:
        os.unlink(path)
    return json.loads(out)


def key(k):
    """Кнопка макета: 'текст' | [текст, значок, цвет, вид] → {label, icon, style}."""
    if isinstance(k, str):
        k = [k]
    k = list(k) + [None] * 4
    return {'label': k[0], 'icon': k[1], 'style': k[2] if k[2] in ('primary', 'success', 'danger') else None}


def sample_key(k):
    """Кнопка samples.json: [цвет, значок, текст]."""
    style, icon, label = (list(k) + ['', '', ''])[:3]
    return {'label': label, 'icon': icon or None, 'style': style or None}


def kind_of(html):
    return 'rich' if re.search(r'<(h3|h4|p|table|div|ul|details|footer)\b', html or '') else 'plain'


def from_template(data):
    out = []
    for item in data['FLOW']:
        out.append(entry('tpl', 'client', '8.11', 'Клиент (B1)', item))
    # Главный экран «вариант А» (принят 8.11): три состояния — в renderAdmin(), не в ADMIN.
    for title, m in zip(('Обычный день', 'Пожар', 'Тишина'), data['DASH']):
        out.append(entry('tpl', 'ops', '8.11', 'Главный экран · вариант А', {'t': title, 'm': [m]}))
    for group in data['ADMIN']:
        for item in group['items']:
            bot = 'client' if item.get('bot') != 'ops' else 'ops'
            out.append(entry('tpl', bot, '8.11', group['group'], item))
    return out


def entry(src, bot, date, section, item):
    msgs = []
    for m in item.get('m', []):
        if m.get('who') != 'bot' or 'html' not in m:
            continue
        msgs.append({
            'kind': 'rich' if m.get('rich') else 'plain',
            'html': m['html'],
            'keys': [[key(k) for k in row] for row in (m.get('kb') or [])],
        })
    return {
        'id': f"{src}:{bot}:{item['t']}",
        'source': 'template.html',
        'date': date,
        'section': section,
        'bot': bot,
        'title': item['t'],
        'note': item.get('n', ''),
        'later': False,
        'messages': msgs,
    }


def from_samples():
    samples = json.load(open(SAMPLES, encoding='utf-8'))
    out = []
    for k, (date, section, bot, later) in SECTIONS.items():
        v = samples[k]
        for st in v.get('states', []):
            name = st.get('name', '')
            if name.startswith('Сейчас') or name.startswith('Утверждено') or name.startswith('А ·'):
                continue
            msgs = []
            for p in st.get('pre') or []:
                msgs.append({'kind': kind_of(p.get('rich')), 'html': p.get('rich', ''),
                             'keys': [[sample_key(x) for x in r] for r in p.get('keys') or []]})
            body = st.get('rich') or st.get('caption') or ''
            msgs.append({
                'kind': 'widget' if st.get('caption') else kind_of(body),
                'html': body.replace('<br>', '\n'),
                'keys': [[sample_key(x) for x in r] for r in st.get('keys') or []],
            })
            out.append({
                'id': f"samples:{k}:{st.get('id') or name}",
                'source': f'samples.json · {k}',
                'date': date,
                'section': section,
                'bot': bot,
                'title': f"{v.get('user') or k} · {name}",
                'note': '',
                'later': later or bool(st.get('caption')),
                'messages': msgs,
            })
    return out


def main():
    entries = from_template(template_data()) + from_samples()
    ids = [e['id'] for e in entries]
    dup = sorted({i for i in ids if ids.count(i) > 1})
    if dup:
        sys.exit(f'повторяются id: {dup}')
    with open(OUT, 'w', encoding='utf-8') as f:
        json.dump(entries, f, ensure_ascii=False, indent=1)
        f.write('\n')
    by = {}
    for e in entries:
        by[(e['bot'], e['later'])] = by.get((e['bot'], e['later']), 0) + 1
    print(f'{OUT}: {len(entries)} экранов', by)


if __name__ == '__main__':
    main()
