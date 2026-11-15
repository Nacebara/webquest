"""Записать настоящие ответы Bot API на виды сообщений exch (шаг 3.2 плана).

    python3 -I tools/telegram/record.py crates/botapi/tests/fixtures/telegram

Тестовый бот (CLIENT_BOT_TOKEN из .env) присылает владельцу (OWNER_TG_ID) каждый вид сообщения
без звука и сразу удаляет. Токен и id в вывод и в файлы не попадают; id, имена и даты в ответах
заменяются условными. Неактивная кнопка — без действия: с callback_data Telegram молча снимает
«disabled» (проверено 17.11)."""
import json, os, ssl, sys, time, urllib.request, urllib.error
env = {}
for line in open(os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', '.env'), encoding='utf-8'):
    line = line.strip()
    if line and not line.startswith('#') and '=' in line:
        k, v = line.split('=', 1); env[k] = v.strip().strip('"')
TOKEN, OWNER = env['CLIENT_BOT_TOKEN'], int(env['OWNER_TG_ID'])
ctx = ssl.create_default_context(cafile='/root/.ccr/ca-bundle.crt')
def call(method, body):
    req = urllib.request.Request(f'https://api.telegram.org/bot{TOKEN}/{method}', data=json.dumps(body).encode(), headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, context=ctx, timeout=30) as r: return json.loads(r.read())
    except urllib.error.HTTPError as e: return json.loads(e.read())
OUT = sys.argv[1]; os.makedirs(OUT, exist_ok=True)
me = call('getMe', {})
BOT_ID = me.get('result', {}).get('id')
def clean(o):
    if isinstance(o, dict):
        r = {}
        for k, v in o.items():
            if k == 'id' and v == OWNER: v = 1000001
            elif k == 'id' and v == BOT_ID: v = 999
            elif k in ('first_name',) and isinstance(v, str): v = 'Exch' if o.get('is_bot') else 'Owner'
            elif k in ('last_name',): continue
            elif k == 'username' and isinstance(v, str): v = 'exch_test_bot' if o.get('is_bot') else 'owner'
            elif k == 'date' or k == 'edit_date': v = 1794700000
            r[k] = clean(v)
        return r
    if isinstance(o, list): return [clean(x) for x in o]
    return o
def save(name, resp):
    json.dump(clean(resp), open(f'{OUT}/{name}.json', 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
    print(name, 'ok' if resp.get('ok') else f"error {resp.get('error_code')}: {resp.get('description')}")
save('get_me', me)
kb = {'inline_keyboard': [[{'text': 'Забрать 97.50 USDT', 'url': 'https://t.me/xrocket', 'style': 'success'}],
      [{'text': '@send 100usdt', 'copy_text': {'text': '@send 100usdt'}, 'style': 'primary'}, {'text': 'Отправляю…', 'disabled': {}}]]}
html = '<b>97.50 USDT</b> ждут вас в xRocket\n<code>K7M2Q9XR4T</code> · <i>за 9 с</i>\n<blockquote expandable>Прислали 100.00 · комиссия 2.5 % (2.50)\nЗаявка K7M2Q9XR4T</blockquote>'
m1 = call('sendMessage', {'chat_id': OWNER, 'text': html, 'parse_mode': 'HTML', 'disable_notification': True, 'protect_content': True, 'reply_markup': kb})
save('send_message_protected_with_buttons', m1)
m2 = call('sendMessage', {'chat_id': OWNER, 'text': 'Первый обмен 🎉', 'disable_notification': True, 'message_effect_id': '5046509860389126442'})
save('send_message_with_effect', m2)
rich = '<p><b>Курс и лимиты</b></p><table><tr><th>Куда</th><th>Комиссия</th></tr><tr><td>CryptoBot → xRocket</td><td>2.5 %</td></tr></table><details><summary>Подробнее</summary><p>Текст</p></details><footer>Обновлено</footer>'
m3 = call('sendRichMessage', {'chat_id': OWNER, 'rich_message': {'html': rich}, 'disable_notification': True})
save('send_rich_message', m3)
ids = [m.get('result', {}).get('message_id') for m in (m1, m2, m3) if m.get('ok')]
if m1.get('ok'):
    mid = m1['result']['message_id']
    save('edit_message_text', call('editMessageText', {'chat_id': OWNER, 'message_id': mid, 'text': html + '\n<i>изменено</i>', 'parse_mode': 'HTML', 'reply_markup': kb}))
    save('set_message_reaction', call('setMessageReaction', {'chat_id': OWNER, 'message_id': mid, 'reaction': [{'type': 'emoji', 'emoji': '👀'}]}))
save('send_widget_not_allowed', call('sendWidget', {'chat_id': OWNER, 'html': '<p>x</p>', 'disable_notification': True}))
time.sleep(1)
for i in ids:
    r = call('deleteMessage', {'chat_id': OWNER, 'message_id': i})
save('delete_message', r if ids else {'ok': False})
