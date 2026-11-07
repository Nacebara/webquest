# Запуск exch на сервере и первый живой прогон

Подробности и обоснования — SPEC §12. Здесь — короткий путь от пустого сервера до первого
обмена на 1 USDT. Все секреты — только в `/opt/exch/.env` на сервере (`chmod 600`): не в чатах,
не в промтах, не в коммитах.

## 0. Что понадобится

| Что | Где взять |
|---|---|
| Сервер: Ubuntu 24.04, 1 vCPU, 2 ГБ RAM, 25 ГБ диска | любой VPS |
| Выделенный аккаунт Telegram для юзербота (один на оба кошелька или два) | номер телефона; руками этим аккаунтом не пользоваться |
| Облачный пароль (2FA) на этом аккаунте | Telegram → Настройки → Конфиденциальность → Облачный пароль |
| `TG_API_ID`, `TG_API_HASH` | my.telegram.org → API development tools (одно приложение на все аккаунты) |
| Клиентский бот и бот-пульт | @BotFather → `/newbot` дважды; токены — `CLIENT_BOT_TOKEN`, `OPS_BOT_TOKEN` |
| Ваш Telegram ID | @userinfobot → `OWNER_TG_ID` |
| В CryptoBot у аккаунта юзербота — «Лимит без код-пароля» | CryptoBot → Кошелёк → Настройки → Безопасность; то же число — `cb_passcode_free_limit_usd` (по умолчанию 500) |

## 1. Сервер

```bash
adduser deploy && usermod -aG sudo deploy          # затем входить как deploy
sudo ufw allow OpenSSH && sudo ufw enable           # входящих портов боту не нужно
curl -fsSL https://get.docker.com | sudo sh && sudo usermod -aG docker deploy   # перелогиньтесь
sudo mkdir -p /opt/exch && sudo chown deploy: /opt/exch
cd /opt/exch
git clone https://github.com/Nacebara/webquest.git src
git -C src checkout claude/xrocket-cryptobot-exchanger-5dt6yg
cp src/exchanger/deploy/docker-compose.yml .
```

## 2. Переменные окружения

```bash
cd /opt/exch && touch .env && chmod 600 .env && nano .env
```

```
EXCH_SRC=./src/exchanger
DB_PASSWORD=<openssl rand -hex 16>
DATABASE_URL=postgres://exch:<тот же пароль>@db:5432/exch
OWNER_TG_ID=<ваш Telegram ID>
TG_API_ID=<…>
TG_API_HASH=<…>
SESSION_KEY=<openssl rand -base64 32>
CLIENT_BOT_TOKEN=<…>
OPS_BOT_TOKEN=<…>
SUPPORT_CONTACT=<@username для «Поддержки»>
TZ_OWNER=Europe/Moscow
RUST_LOG=info
```

Копию `.env` — в менеджер паролей. `SESSION_KEY` шифрует сессии юзерботов в базе: без него
после восстановления из бэкапа придётся входить заново.

## 3. Сборка, база, проверка

```bash
docker compose build exch                 # первый раз ~10 минут
docker compose up -d db
docker compose run --rm exch migrate
docker compose run --rm exch check-config  # каких переменных не хватает; значения не печатает
```

## 4. Вход юзербота

Один аккаунт может обслуживать оба кошелька: второй вход той же метки код не спрашивает.

```bash
docker compose run --rm exch login --label ub-1 --platform cryptobot   # телефон, код, облачный пароль
docker compose run --rm exch login --label ub-1 --platform xrocket
```

Телефон, код и пароль никуда не сохраняются; сессия — только зашифрованной в базе.

## 5. Запуск

```bash
docker compose up -d
docker compose logs -f exch    # ждём: migrations applied, engine, client-bot, ops-bot
```

Напишите `/start` обоим ботам со своего аккаунта. Премиум-эмодзи (один раз, нужен `/start`
клиентскому боту от владельца):

```bash
docker compose run --rm exch emoji install --owner <ваш Telegram ID>
```

## 6. Пульт: первая настройка

1. **Капитал.** Пополните кошельки юзербота (CryptoBot и xRocket) — чеком со своего аккаунта
   или переводом. Через ≤ 10 минут сверка пришлёт «Баланс … не сходится: +N» — нажмите
   «Это мой взнос: провести». Сразу — «Сводка» → «🔁 Сверить балансы».
2. **Цели резерва.** «Встречный поток» → «Цель: CryptoBot», «Цель: xRocket» — рабочий объём
   каждого кошелька. От них считаются рычаг и встречный поток.
3. **Направления.** Все направления после установки закрыты. «Направления» → карточка →
   «Лимиты» (для теста по 1 USDT: `1 300`) → «▶️ Открыть приём».
4. **Самопроверка.** «Сводка» → «🧪 Самопроверка»: на каждом кошельке бот создаст чек на
   1 USDT и активирует его сам. Всё ✅ — можно принимать.
5. **Персонал** (по желанию): «Персонал» → «Добавить» → `id admin Имя`.

## 7. Первый живой прогон (суммы по 1 USDT)

Со **своего** личного аккаунта (не юзербота):

| Что | Как | Ожидаем |
|---|---|---|
| CryptoBot → xRocket | `@send 1usdt` в любом чате → чек → ссылку клиентскому боту | одно сообщение заявки, затем «Ваш чек xRocket на 0.97…» и кнопка «Забрать» |
| xRocket → CryptoBot | `@xrocket 1` → чек USDT → ссылку боту | чек CryptoBot |
| Оплата счёта xRocket | счёт xRocket на 1 USDT → ссылку боту | сумма к оплате → прислать чек CryptoBot → «Счёт оплачен» |
| Оплата счёта CryptoBot | счёт CryptoBot на 1 USDT → ссылку боту | то же, чек xRocket |
| Отказ | уже активированный чек | «чек уже активирован», деньги не тронуты |

После прогона: «Сводка» — очередь разбора пуста, «Сверить балансы» — всё сходится.
Что-то пошло не так — заявка появится в «Разборе» с причиной и ответами кошелька; решения —
кнопками в карточке заявки.

## 8. Бэкапы

```bash
# на ноутбуке: age-keygen -o key.txt  (приватный ключ — в менеджер паролей)
sudo apt install -y age rclone && rclone config        # remote для копий (необязательно)
cp src/exchanger/deploy/backup.sh /opt/exch/ && chmod +x /opt/exch/backup.sh
cat > /opt/exch/backup.env <<'X'
AGE_RECIPIENT=age1…
RCLONE_REMOTE=remote:exch-backups
X
sudo cp src/exchanger/deploy/exch-backup.{service,timer} /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now exch-backup.timer
sudo systemctl start exch-backup.service && journalctl -u exch-backup -n 5   # проверка
```

Восстановление: `age -d -i key.txt exch-….dump.age | docker compose exec -T db pg_restore -U exch -d exch --clean`.

## 9. Обновление

1. Пульт → «Настройки» → «Обслуживание: вкл» (новые ссылки не принимаются, заявки доводятся).
2. `git -C src pull && docker compose build exch && docker compose up -d` — мягкая остановка
   до 90 с, после старта движок сам доводит прерванное.
3. «Самопроверка» → «Обслуживание: выкл».

## 10. Если что-то случилось

| Ситуация | Что делать |
|---|---|
| В Telegram чужая сессия на аккаунте юзербота | пульт → «Юзерботы» → пауза; Telegram → Устройства → завершить чужие; сменить облачный пароль |
| Сверка: «не хватает» | выплаты с кошелька уже на паузе; сверить историю кошелька с журналом заявки; «Снять автопаузу» — после разбора |
| Нужно сделать что-то руками в аккаунте юзербота | «Юзерботы» → пауза аккаунта (операции подождут), после — «▶️» |
| Стоп всего | «Настройки» → «🛑 Включить стоп-кран»: выплаты встанут, ничего не потеряется |
