<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko Infrastructure

Single-VPS deployment скелет для Phase 1+. **Целевая машина: уже существующий Hetzner CPX22** (Ubuntu 22.04), на котором сейчас работает прошлая Node.js-версия `lakko_me`. Real users на ней нет → план = **wipe старого + install нового на той же VPS, через systemd + system-nginx + system-MariaDB**.

Никаких контейнеров. Контейнеризация добавится когда понадобится — горизонтальный scaling, несколько окружений, container registry. Пока оверkill для solo + 1 VPS.

```
infra/
├── systemd/
│   ├── lakko.target                    # group: start/stop всех services разом
│   ├── lakko-svc-auth.service          # OPAQUE-PAKE + WebAuthn
│   ├── lakko-svc-ds.service            # MLS Delivery Service
│   ├── lakko-svc-storage.service       # ciphertext blob proxy
│   ├── lakko-svc-discovery.service     # username OPRF lookup
│   └── lakko-svc-push-gw.service       # APNs / FCM / VAPID
├── nginx/
│   └── sites-available/
│       ├── lakko.conf                  # reverse proxy для system-nginx
│       └── lakko-proxy-headers.inc
├── mariadb/
│   └── migrations/
│       └── 0001-init.sql               # placeholder; per-service schemas в их sprint-ах
├── etc-lakko/
│   └── lakko.env.example               # шаблон для /etc/lakko/lakko.env
├── scripts/
│   └── deploy.sh                       # git pull + cargo build + systemctl restart
├── secrets/                            # gitignored — APNs / FCM / VAPID keys
└── nix/
    └── flake.nix                       # dev-shell toolchain pin
```

## Layout сервисов

| Service | Listen на | Логи |
|---|---|---|
| auth | 127.0.0.1:8081 | `journalctl -u lakko-svc-auth` |
| ds | 127.0.0.1:8082 | `journalctl -u lakko-svc-ds` |
| storage | 127.0.0.1:8083 | `journalctl -u lakko-svc-storage` |
| discovery | 127.0.0.1:8084 | `journalctl -u lakko-svc-discovery` |
| push-gw | 127.0.0.1:8085 | `journalctl -u lakko-svc-push-gw` |

system-nginx слушает 80/443 → reverse proxy на 127.0.0.1:808X. Service-ы наружу не торчат.

## Что уже на VPS (от lakko_me)

| Компонент | Что делаем |
|---|---|
| Hetzner CPX22 + Ubuntu 22.04 | **reuse** |
| User `lakkome` (sudo) | **reuse** — services запускаются как `lakkome`, sudoers для systemctl |
| Domains `lakko.me`, `api.lakko.me`, `edge.lakko.me` (Cloudflare) | **reuse** |
| system nginx | **reuse**, конфиг переписать на `lakko.conf` |
| Let's Encrypt certs `/etc/letsencrypt/live/{lakko.me,api.lakko.me,edge.lakko.me}/` | **reuse** |
| system MariaDB | DB **drop**, server reuse — новая БД `lakko` |
| system Redis | **stop / disable** — новый стек не использует |
| system coturn (`edge.lakko.me`) | **reuse** (для звонков в Phase 3) |
| Старый Node.js `~/lakkome/lakko_me/` | **archive** в `~/lakkome/lakko_me.archived.YYYYMMDD/` |

## Migration (one-time, ручные шаги)

> ⚠️ Деструктивно. Real users нет (подтверждено), но backup всё равно делаем.

### 0. Backup

```bash
ssh lakkome@lakko.me
cd ~

sudo mariadb-dump --all-databases | zstd > ~/pre-migration-mariadb-$(date +%F).sql.zst
sudo tar c /etc/nginx | zstd > ~/pre-migration-nginx-$(date +%F).tar.zst
tar c ~/lakko_me 2>/dev/null | zstd > ~/lakko_me-archive-$(date +%F).tar.zst || true
```

### 1. Остановить lakko_me

```bash
# pm2 или systemd — в зависимости от того как было поднято
pm2 stop all && pm2 delete all && pm2 save 2>/dev/null || true
sudo systemctl stop lakko-me 2>/dev/null || true
sudo systemctl disable lakko-me 2>/dev/null || true

# Redis (новый стек не использует)
sudo systemctl stop redis-server
sudo systemctl disable redis-server

# Проверить что только nginx + MariaDB + sshd слушают
sudo ss -tlnp | grep -E ':(22|80|443|3306)\s'
```

### 2. Wipe lakko_me MariaDB

```bash
sudo mariadb -e "DROP DATABASE IF EXISTS lakko_me;"
sudo mariadb -e "DROP USER IF EXISTS 'lakko_user'@'localhost';"
sudo mariadb -e "FLUSH PRIVILEGES;"
```

### 3. Archive старый код

```bash
mv ~/lakko_me ~/lakko_me.archived.$(date +%Y%m%d)
```

### 4. Установить Rust toolchain (если ещё нет)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
source ~/.cargo/env
rustc --version  # cargo подтянет нужный по rust-toolchain.toml на первом build
```

### 5. Clone репо нового Lakko

```bash
cd ~
git clone https://github.com/lakkome/lakko.git
cd lakko
cargo build --release --bins  # первый build долгий — ~10 минут на CPX22
```

### 6. БД и пользователь в MariaDB

```bash
# Применить init migration (создаёт database `lakko`)
sudo mariadb < ~/lakko/infra/mariadb/migrations/0001-init.sql

# Создать service user — пароль НЕ из git
LAKKO_DB_PASS=$(openssl rand -hex 32)
sudo mariadb -e "
    CREATE USER 'lakko'@'localhost' IDENTIFIED BY '$LAKKO_DB_PASS';
    GRANT ALL PRIVILEGES ON lakko.* TO 'lakko'@'localhost';
    FLUSH PRIVILEGES;
"
echo "DB password: $LAKKO_DB_PASS"  # запомни — нужно в шаге 7
```

### 7. /etc/lakko/lakko.env

```bash
sudo mkdir -p /etc/lakko
sudo cp ~/lakko/infra/etc-lakko/lakko.env.example /etc/lakko/lakko.env
sudo nano /etc/lakko/lakko.env
# заполни DATABASE_URL=mysql://lakko:$LAKKO_DB_PASS@127.0.0.1:3306/lakko

sudo chown root:lakkome /etc/lakko/lakko.env
sudo chmod 640 /etc/lakko/lakko.env
```

### 8. Storage директория

```bash
sudo mkdir -p /var/lib/lakko/blobs
sudo chown lakkome:lakkome /var/lib/lakko /var/lib/lakko/blobs
```

### 9. Push secrets (placeholder в Phase 1.0)

Реальные ключи — Phase 1.6 (push-gw). Пока ставим placeholder:

```bash
sudo mkdir -p /etc/lakko/secrets
sudo chmod 700 /etc/lakko/secrets

# VAPID — реальный ключ для Web Push
sudo openssl ecparam -name prime256v1 -genkey -noout \
    -out /etc/lakko/secrets/vapid_priv.pem

# APNs / FCM — пустышки на Phase 1.0
sudo touch /etc/lakko/secrets/apns_key.p8
sudo touch /etc/lakko/secrets/fcm_sa.json

sudo chown -R root:lakkome /etc/lakko/secrets
sudo chmod 640 /etc/lakko/secrets/*
```

### 10. Установить systemd unit-ы

```bash
cd ~/lakko
sudo cp infra/systemd/*.service infra/systemd/lakko.target /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable lakko.target
```

### 11. Установить бинари

```bash
sudo install -m 755 ~/lakko/target/release/lakko-svc-{auth,ds,storage,discovery,push-gw} /usr/local/bin/
```

### 12. Sudoers для passwordless deploy

`sudo visudo -f /etc/sudoers.d/lakko-deploy`:

```
lakkome ALL=(root) NOPASSWD: /usr/bin/install -m 755 /tmp/lakko-svc-* /usr/local/bin/, /usr/bin/systemctl restart lakko.target, /usr/bin/systemctl status lakko.target
```

### 13. Запустить services

```bash
sudo systemctl start lakko.target
sudo systemctl status lakko.target
sudo journalctl -u lakko-svc-auth -f  # пощупать что service стартует
```

> Phase 1.0: services печатают "starting (Phase 0 stub)" и выходят. systemd рестартанёт (Restart=on-failure). Реальный listen появится в sprint 1.1+.

### 14. Заменить nginx config

```bash
# Backup существующий config (если был)
sudo mkdir -p /etc/nginx/sites-enabled.bak
sudo mv /etc/nginx/sites-enabled/* /etc/nginx/sites-enabled.bak/ 2>/dev/null || true

# Установить новый
sudo cp ~/lakko/infra/nginx/sites-available/lakko.conf /etc/nginx/sites-available/
sudo cp ~/lakko/infra/nginx/sites-available/lakko-proxy-headers.inc /etc/nginx/sites-available/
sudo ln -sf /etc/nginx/sites-available/lakko.conf /etc/nginx/sites-enabled/lakko.conf

# Проверить + reload
sudo nginx -t
sudo systemctl reload nginx
```

### 15. End-to-end проверка

```bash
curl https://lakko.me/health
curl https://api.lakko.me/health
```

Ожидаем `lakko.me ok` и `api.lakko.me ok` (это nginx-уровень). `/api/auth/...` etc. вернёт 502 пока services не имеют реальной логики (Phase 1.1+).

### 16. coturn — оставить

```bash
sudo systemctl status coturn
```

Если работает — не трогаем. Phase 3 (calls) будет интегрировать.

## Daily ops

### Deploy

С Mac:

```bash
just deploy
```

(требует ssh-ключ к `lakkome@lakko.me`.) Что делает: ssh-ит на VPS и запускает `~/lakko/infra/scripts/deploy.sh` от имени `lakkome`. Скрипт делает `git pull` → `cargo build --release` → `sudo install /usr/local/bin/...` → `sudo systemctl restart lakko.target`.

### Restart одного service

```bash
ssh lakkome@lakko.me sudo systemctl restart lakko-svc-auth
```

### Логи

```bash
ssh lakkome@lakko.me journalctl -u lakko-svc-auth -f
ssh lakkome@lakko.me journalctl -u lakko.target --since "1 hour ago"
```

### Status всего

```bash
ssh lakkome@lakko.me systemctl status lakko.target
```

## Backup / restore

Phase 2 sprint task. Минимум:

- MariaDB dump → Hetzner Storage Box ежесуточно: `mariadb-dump --single-transaction lakko | zstd > /backup/lakko-$(date +%F).sql.zst`
- `/var/lib/lakko/blobs` — еженедельный snapshot rsync на Storage Box
- `/etc/lakko/` — backup при изменении секретов
- `/etc/letsencrypt/` — backup перед renewal

## Что НЕ в скоупе Phase 1

- Multi-region / failover (Phase 5)
- Container registry / podman (когда понадобится horizontal scaling)
- TLS certificate pinning на клиенте (Phase 1 sprint 1.8)
- DDoS protection beyond Cloudflare (Phase 4-5 multi-CDN)
- Перенос coturn в managed (Phase 3 — звонки)
- Hardware security module integration (Phase 5)
- Rolling update без downtime (пока systemd restart с Restart=on-failure)
- Dedicated `lakko` system user (вместо `lakkome`) — Phase 2 hardening
