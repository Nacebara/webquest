"""Очистка записи `exch listen` / `exch record` для фикстур парсеров.

- «Избранное» убирается целиком: там личное.
- Сообщения о выводах в сеть и обменах убираются: в них адреса кошельков и хэши транзакций.
- Коды чеков и счетов, номера счетов и платежей перемешиваются с сохранением формата,
  хэши оплаты заменяются.
- Имена людей и магазинов заменяются по месту в тексте («<имя> активировал(а) ваш чек»,
  «Получатель: …», «… от <имя>.»), а не по списку: список настоящих имён сам был бы утечкой.
- Ссылки на чужих ботов и любые не-Telegram ссылки, кроме картинок чеков, убираются.

    python3 -I tools/recording/sanitize.py exch-record.jsonl clean.json
"""

import hashlib
import json
import re
import sys

ALNUM = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789'


def scramble(code, keep):
    head, tail = code[:keep], code[keep:]
    h = hashlib.sha256(('exch-fixture:' + code).encode()).digest()
    return head + ''.join(ALNUM[h[i % len(h)] % len(ALNUM)] if ch.isalnum() else ch for i, ch in enumerate(tail))


CODE_RE = [
    (re.compile(r'\bCQ[A-Za-z0-9_-]{10}\b'), 2),
    (re.compile(r'\bIV[A-Za-z0-9_-]{10}\b'), 2),
    (re.compile(r'\bt_[A-Za-z0-9_-]{15}\b'), 2),
    (re.compile(r'\bmci_[A-Za-z0-9_-]{15}\b'), 4),
    (re.compile(r'\bmc_[A-Za-z0-9_-]{15}\b'), 3),
    (re.compile(r'\binv_[A-Za-z0-9_-]{15}\b'), 4),
    (re.compile(r'\bCS[A-Za-z0-9_-]{10}\b'), 2),
]
HEX_RE = re.compile(r'\b[0-9a-f]{32,64}\b')
# Номера счетов и платежей: «#IV69544493», «#INV7276063», «#i7276056_4775817».
NUMBER_RE = re.compile(r'(#(?:IV|INV|i))([0-9_]+)')


def scramble_digits(m):
    h = hashlib.sha256(('exch-fixture:' + m.group(0)).encode()).digest()
    return m.group(1) + ''.join(str(h[i] % 10) if ch.isdigit() else ch for i, ch in enumerate(m.group(2)))

# Имена по месту: уведомление о забранном чеке, получатель платежа, отправитель перевода,
# магазин в подтверждении xRocket, профиль в настройках.
NAME_RULES = [
    (re.compile(r'^(🦋 )?⁨?.*?⁩?(?= активировал\(а\) ваш чек)', re.M), lambda m: (m.group(1) or '') + '⁨Ivan⁩'),
    (re.compile(r'^⁨?.*?⁩?(?= оплатил\(а\) ваш сч)', re.M), '⁨Ivan⁩'),
    (re.compile(r'(Получатель: ).*'), r'\1Shop'),
    (re.compile(r'(\) от ).+?(\.?)$', re.M), r'\1Shop\2'),
    (re.compile(r'(оплатили счёт )(?!#)(?!на сумму).+?( на сумму)'), r'\1Shop\2'),
    (re.compile(r'^👤 .*', re.M), '👤 Ivan'),
    (re.compile(r'^💬 .*', re.M), '💬 комментарий'),
    (re.compile(r'^🧾 (?!Сч).*', re.M), '🧾 описание'),
]
DROP = ('Вывод #WD', 'Заявка на обмен')
URL_KEEP = re.compile(r'^(https?://)?t\.me/(send|CryptoBot|xrocket)\b', re.I)


def clean(s):
    if not isinstance(s, str):
        return s
    for rx, keep in CODE_RE:
        s = rx.sub(lambda m: scramble(m.group(0), keep), s)
    s = HEX_RE.sub(lambda m: hashlib.sha1(m.group(0).encode()).hexdigest()[: len(m.group(0))], s)
    s = NUMBER_RE.sub(scramble_digits, s)
    for rx, repl in NAME_RULES:
        s = rx.sub(repl, s)
    return s


def clean_url(u):
    return clean(u) if URL_KEEP.match(u) else 'https://example.invalid/'


def btn(b):
    k = b['kind']
    if isinstance(k, dict):
        (t, v), = k.items()
        if t == 'Callback' and isinstance(v, list):
            v = bytes(v).decode('utf-8', 'replace')
        if t in ('Url', 'WebApp'):
            v = clean_url(v)
        return {'text': clean(b['text']), 'kind': t, 'data': clean(v)}
    return {'text': clean(b['text']), 'kind': k}


def main():
    src, out = sys.argv[1], sys.argv[2]
    rows = [json.loads(l) for l in open(src, encoding='utf-8') if l.strip()]
    keep = []
    for r in rows:
        chat = r['chat']
        if not isinstance(chat, dict) or any(d in r['text'] for d in DROP):
            continue
        keep.append({
            'platform': 'cryptobot' if chat['WalletBot'] == 'CryptoBot' else 'xrocket',
            'id': r['id'],
            'out': r['out'],
            'date': r['date'],
            'edit_date': r.get('edit_date'),
            'text': clean(r['text']),
            'buttons': [[btn(b) for b in row] for row in r['buttons']],
            'entity_urls': [clean_url(u) for u in r['entity_urls']],
        })
    json.dump(keep, open(out, 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
    print(len(keep), 'messages kept')


if __name__ == '__main__':
    main()
