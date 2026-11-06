"""Живые фикстуры из очищенной записи: смысл каждого ответа — по его значению (разметка вручную
правилами ниже), а не по тому, что выдаёт парсер."""
import json, re, sys

msgs = json.load(open(sys.argv[1], encoding='utf-8'))
out_path = sys.argv[2]

def num(s):
    return s.replace(',', '.')

def money(text, label=None):
    """Первая «N USDT» (после подписи, если задана)."""
    src = text
    if label:
        for line in text.split('\n'):
            if label.lower() in line.lower():
                src = line
                break
        else:
            return None
    m = re.search(r'(\d[\d\s.,]*?)\s*USDT', src)
    return [num(re.sub(r'\s', '', m.group(1))), 'USDT'] if m else None

cases = []
seen = set()
def add(kind, m, expect):
    key = (kind, m['platform'], m['text'], json.dumps(m['buttons'], ensure_ascii=False))
    if key in seen:
        return
    seen.add(key)
    cases.append({
        'kind': kind, 'platform': m['platform'], 'text': m['text'],
        'buttons': [[b['text'] for b in row] for row in m['buttons']],
        'urls': [b.get('data') for row in m['buttons'] for b in row if b['kind'] == 'Url'] + m['entity_urls'],
        'expect': expect,
    })

for m in msgs:
    if m['out']:
        continue
    t, p = m['text'], m['platform']
    low = t.lower()
    first = t.split('\n')[0]
    # ---- активация и всё, что может прийти, пока ждём ответа на /start <чек> ----
    if re.search(r'получение .*usdt', low):
        add('activation', m, 'in_progress')
    elif 'активировал(а) ваш чек' in low:
        add('activation', m, 'claim_notice')
    elif re.search(r'вы (успешно )?получили', low) and ' от ' in low:
        add('activation', m, ['incoming_transfer'] + money(t))
    elif re.search(r'вы (успешно )?получили', low):
        add('activation', m, ['received'] + money(t))
    elif 'уже активирован вами' in low:
        add('activation', m, 'already_activated_by_us')
    elif 'уже активирован' in low:
        add('activation', m, 'already_activated')
    elif 'предназначен для другого' in low:
        add('activation', m, 'not_for_you')
    elif re.match(r'^(чек|этот перевод|мульти-чек) не найден', low):
        add('activation', m, 'not_found')
    elif first.startswith(('🍁', '🚀 Litecoin')) or 'успешно завершён' in low or 'заявка на обмен' in low:
        add('activation', m, 'unknown')
    # ---- экраны оплаты счёта ----
    if t.strip() in ('👌', '💸') or low.strip() == '⏱ загрузка':
        add('invoice_screen', m, 'progress')
    elif 'выберите монету для оплаты' in low:
        add('invoice_screen', m, ['choose_coin', re.search(r'#(IV\d+)', t).group(1)])
        add('invoice_card', m, {'status': 'active', 'number': re.search(r'#(IV\d+)', t).group(1),
            'fiat': [re.search(r'\$\s*([\d.]+)', first).group(1), 'USD'],
            'amount': [re.search(r'USDT\s*·\s*([\d.]+)\s+USDT', ' '.join(b['text'] for r in m['buttons'] for b in r)).group(1), 'USDT'],
            'insufficient_funds': False})
    elif 'подтвердите оплату счёта' in low:
        add('invoice_screen', m, ['confirm', re.search(r'#(IV\d+)', t).group(1)] + money(t, 'Отправляете'))
    elif 'подтвердите ваш платёж' in low:
        add('invoice_screen', m, ['confirm', re.search(r'#(INV\d+)', t).group(1)] + money(t, 'Подтвердите ваш платёж'))
    elif 'доверяете этому получателю' in low:
        add('invoice_screen', m, 'trust_recipient')
    elif 'выберите сеть для оплаты' in low or (first.startswith('🧾 Счёт #INV') and 'недостаточно средств' in low):
        add('invoice_screen', m, 'insufficient_funds')
        if 'выберите сеть' in low:
            add('invoice_card', m, {'status': 'active', 'number': None, 'fiat': None, 'amount': None, 'insufficient_funds': True})
    elif first.startswith('🧾 Счёт #INV'):
        number = re.search(r'#(INV\d+)', t).group(1)
        add('invoice_screen', m, ['choose_method', number] + money(t, 'Сумма:'))
        add('invoice_card', m, {'status': 'active', 'number': number, 'fiat': None,
            'amount': money(t, 'Сумма:'), 'insufficient_funds': False})
    elif 'пришлите сумму usdt для оплаты счёта' in low:
        add('invoice_screen', m, 'open_amount')
    elif re.search(r'вы (успешно )?оплатили сч', low):
        nm = re.search(r'#(IV\d+)', t)
        amt = money(t)
        add('invoice_screen', m, ['paid', nm.group(1) if nm else None] + amt)
    elif 'уже оплачен' in low:
        add('invoice_screen', m, 'already_paid')
        add('invoice_card', m, {'status': 'paid', 'number': None, 'fiat': None, 'amount': None, 'insufficient_funds': False})
    # ---- меню чеков ----
    if 'скопируйте ссылку' in low or 'ссылка на чек' in low:
        link = re.search(r'(?:t\.me/send\?start=(CQ\w{10})|t\.me/xrocket\?start=(t_[\w-]{15}))', t)
        param = link.group(1) or link.group(2)
        add('check_creation', m, ['created', param] + money(t, 'Сумма'))
    elif 'количество активаций' in low:
        add('check_creation', m, 'ask_activations')
    elif 'подтвердите корректность данных' in low and first.startswith('🏷'):
        add('check_creation', m, ['confirm'] + money(t, 'Сумма чека'))
    elif first.startswith('🏷') and 'недостаточно средств' in low:
        add('check_creation', m, 'insufficient_funds')
    elif first.startswith('Недостаточно монет'):
        add('check_creation', m, 'insufficient_funds')
    elif first.startswith(('🏷', '​Здесь вы можете', 'Выберите криптовалюту', 'Пришлите сумму чека', 'Здесь вы можете управлять')):
        add('check_creation', m, 'unknown')
    # ---- кошелёк ----
    if first.startswith('👛'):
        if p == 'cryptobot':
            tm = re.search(r'Tether:\s*([\d.]+)\s+USDT', t)
            av = re.search(r'Доступно:\s*([\d.]+)\s+USDT', t)
            hd = re.search(r'На удержании:\s*([\d.]+)\s+USDT', t)
            exp = [[av.group(1) if av else tm.group(1), 'USDT', hd.group(1) if hd else '0']]
        else:
            tm = re.search(r'USDT:\s*([\d.]+)\s+USDT', t)
            exp = [[tm.group(1), 'USDT', '0']]
        add('balance', m, exp)

about = ('Живые ответы @send и @xrocket из записи владельца 6 ноября 2026 (exch listen). Коды чеков и '
         'счетов перемешаны с сохранением формата, имена людей и магазинов заменены. Смысл каждого '
         'ответа размечен по значению. kind: activation | invoice_screen | invoice_card | '
         'check_creation | balance.')
json.dump({'about': about, 'cases': cases}, open(out_path, 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
from collections import Counter
print(Counter(c['kind'] for c in cases))
