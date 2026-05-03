<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko Infrastructure

Single-VPS deployment skeleton for Phase 1+. **Target machine: an existing Hetzner CPX22** (Ubuntu 22.04) currently running the previous Node.js `lakko_me` codebase. There are no real users on it → the plan = **wipe the old setup and install the new stack on the same VPS, via systemd + system-nginx + system-MariaDB**.

No containers. Containerisation can be added later when actually needed — horizontal scaling, multiple environments, container registry. For solo + 1 VPS it is overkill.

```
infra/
├── systemd/
│   ├── lakko.target                    # group: start/stop all services together
│   ├── lakko-svc-auth.service          # OPAQUE-PAKE + WebAuthn
│   ├── lakko-svc-ds.service            # MLS Delivery Service
│   ├── lakko-svc-storage.service       # ciphertext blob proxy
│   ├── lakko-svc-discovery.service     # username OPRF lookup
│   └── lakko-svc-push-gw.service       # APNs / FCM / VAPID
├── nginx/
│   └── sites-available/
│       ├── lakko.conf                  # reverse proxy for system-nginx
│       └── lakko-proxy-headers.inc
├── mariadb/
│   └── migrations/
│       └── 0001-init.sql               # placeholder; per-service schemas land in their sprint
├── etc-lakko/
│   └── lakko.env.example               # template for /etc/lakko/lakko.env
├── scripts/
│   └── deploy.sh                       # git pull + cargo build + systemctl restart
├── secrets/                            # gitignored — APNs / FCM / VAPID keys
└── nix/
    └── flake.nix                       # dev-shell toolchain pin
```

## Service layout

| Service | Listens on | Logs |
|---|---|---|
| auth | 127.0.0.1:8081 | `journalctl -u lakko-svc-auth` |
| ds | 127.0.0.1:8082 | `journalctl -u lakko-svc-ds` |
| storage | 127.0.0.1:8083 | `journalctl -u lakko-svc-storage` |
| discovery | 127.0.0.1:8084 | `journalctl -u lakko-svc-discovery` |
| push-gw | 127.0.0.1:8085 | `journalctl -u lakko-svc-push-gw` |

system-nginx listens on 80/443 → reverse proxies to 127.0.0.1:808X. Services are not exposed externally.

## What's already on the VPS (from lakko_me)

| Component | What we do |
|---|---|
| Hetzner CPX22 + Ubuntu 22.04 | **reuse** |
| User `lakkome` (sudo) | **reuse** — services run as `lakkome`, sudoers entry for systemctl |
| Domains `lakko.me`, `api.lakko.me`, `edge.lakko.me` (Cloudflare) | **reuse** |
| system nginx | **reuse**, rewrite the config as `lakko.conf` |
| Let's Encrypt certs `/etc/letsencrypt/live/{lakko.me,api.lakko.me,edge.lakko.me}/` | **reuse** |
| system MariaDB | **drop** the database, reuse the server — fresh `lakko` DB |
| system Redis | **stop / disable** — the new stack does not use it |
| system coturn (`edge.lakko.me`) | **reuse** (for calls in Phase 3) |
| Old Node.js `~/lakkome/lakko_me/` | **archive** to `~/lakkome/lakko_me.archived.YYYYMMDD/` |

## Migration (one-time, manual steps)

> ⚠️ Destructive. There are no real users (confirmed), but we still take a backup.

### 0. Backup

```bash
ssh lakkome@lakko.me
cd ~

sudo mariadb-dump --all-databases | zstd > ~/pre-migration-mariadb-$(date +%F).sql.zst
sudo tar c /etc/nginx | zstd > ~/pre-migration-nginx-$(date +%F).tar.zst
tar c ~/lakko_me 2>/dev/null | zstd > ~/lakko_me-archive-$(date +%F).tar.zst || true
```

### 1. Stop lakko_me

```bash
# pm2 or systemd — depending on how it was launched
pm2 stop all && pm2 delete all && pm2 save 2>/dev/null || true
sudo systemctl stop lakko-me 2>/dev/null || true
sudo systemctl disable lakko-me 2>/dev/null || true

# Redis (the new stack does not use it)
sudo systemctl stop redis-server
sudo systemctl disable redis-server

# Verify only nginx + MariaDB + sshd are listening
sudo ss -tlnp | grep -E ':(22|80|443|3306)\s'
```

### 2. Wipe lakko_me MariaDB

```bash
sudo mariadb -e "DROP DATABASE IF EXISTS lakko_me;"
sudo mariadb -e "DROP USER IF EXISTS 'lakko_user'@'localhost';"
sudo mariadb -e "FLUSH PRIVILEGES;"
```

### 3. Archive the old code

```bash
mv ~/lakko_me ~/lakko_me.archived.$(date +%Y%m%d)
```

### 4. Install the Rust toolchain (if not installed yet)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
source ~/.cargo/env
rustc --version  # cargo will pick up the right toolchain from rust-toolchain.toml on first build
```

### 5. Clone the new Lakko repo

```bash
cd ~
git clone https://github.com/lakkome/lakko.git
cd lakko
cargo build --release --bins  # first build is slow — ~10 minutes on a CPX22
```

### 6. Databases and user in MariaDB

Each service has its own database. sqlx tracks applied migrations in a
hardcoded `_sqlx_migrations` table per database, so two services on the
same DB would see each other's migrations as "missing" and refuse to
start. Per-service DBs are also nicer for blast-radius — a compromised
service can only see / modify its own tables.

```bash
# One-time: create the per-service databases.
sudo mariadb <<'SQL'
    CREATE DATABASE IF NOT EXISTS lakko_auth
        CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
    CREATE DATABASE IF NOT EXISTS lakko_ds
        CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
    CREATE DATABASE IF NOT EXISTS lakko_discovery
        CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
    -- (lakko_push_gw — added in its sprint)
SQL

# Create the service user — password is NOT in git.
LAKKO_DB_PASS=$(openssl rand -hex 32)
sudo mariadb -e "
    CREATE USER 'lakko'@'localhost' IDENTIFIED BY '$LAKKO_DB_PASS';
    GRANT ALL PRIVILEGES ON lakko_auth.*      TO 'lakko'@'localhost';
    GRANT ALL PRIVILEGES ON lakko_ds.*        TO 'lakko'@'localhost';
    GRANT ALL PRIVILEGES ON lakko_discovery.* TO 'lakko'@'localhost';
    FLUSH PRIVILEGES;
"
echo "DB password: $LAKKO_DB_PASS"  # save it — needed in step 7
```

Schema migrations themselves are applied automatically by each service
on first start (`sqlx::migrate!()` embeds the SQL into the binary).

### 7. /etc/lakko/lakko.env

```bash
sudo mkdir -p /etc/lakko
sudo cp ~/lakko/infra/etc-lakko/lakko.env.example /etc/lakko/lakko.env
sudo nano /etc/lakko/lakko.env
# Replace CHANGE-ME with $LAKKO_DB_PASS in:
#   LAKKO_AUTH_DATABASE_URL=mysql://lakko:$LAKKO_DB_PASS@127.0.0.1:3306/lakko_auth
#   LAKKO_DS_DATABASE_URL=mysql://lakko:$LAKKO_DB_PASS@127.0.0.1:3306/lakko_ds
#   LAKKO_DISCOVERY_DATABASE_URL=mysql://lakko:$LAKKO_DB_PASS@127.0.0.1:3306/lakko_discovery

sudo chown root:lakkome /etc/lakko/lakko.env
sudo chmod 640 /etc/lakko/lakko.env
```

### 8. Storage directory

```bash
sudo mkdir -p /var/lib/lakko/blobs
sudo chown lakkome:lakkome /var/lib/lakko /var/lib/lakko/blobs
```

### 9. Push secrets (placeholder in Phase 1.0)

Real keys come in Phase 1.6 (push-gw). For now we drop in placeholders:

```bash
sudo mkdir -p /etc/lakko/secrets
sudo chmod 700 /etc/lakko/secrets

# VAPID — real key for Web Push
sudo openssl ecparam -name prime256v1 -genkey -noout \
    -out /etc/lakko/secrets/vapid_priv.pem

# APNs / FCM — empty placeholders for Phase 1.0
sudo touch /etc/lakko/secrets/apns_key.p8
sudo touch /etc/lakko/secrets/fcm_sa.json

sudo chown -R root:lakkome /etc/lakko/secrets
sudo chmod 640 /etc/lakko/secrets/*
```

### 10. Install systemd units

```bash
cd ~/lakko
sudo cp infra/systemd/*.service infra/systemd/lakko.target /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable lakko.target
```

### 11. Install binaries

```bash
sudo install -m 755 ~/lakko/target/release/lakko-svc-{auth,ds,storage,discovery,push-gw} /usr/local/bin/
```

### 12. Sudoers for passwordless deploy

`sudo visudo -f /etc/sudoers.d/lakko-deploy`:

```
lakkome ALL=(root) NOPASSWD: /usr/bin/install -m 755 /tmp/lakko-svc-* /usr/local/bin/, /usr/bin/systemctl restart lakko.target, /usr/bin/systemctl status lakko.target
```

### 13. Start the services

```bash
sudo systemctl start lakko.target
sudo systemctl status lakko.target
sudo journalctl -u lakko-svc-auth -f  # confirm the service starts
```

> Phase 1.0: services print "starting (Phase 0 stub)" and exit. systemd restarts them (Restart=on-failure). Real listening sockets land in sprint 1.1+.

### 14. Replace the nginx config

```bash
# Back up the existing config (if any)
sudo mkdir -p /etc/nginx/sites-enabled.bak
sudo mv /etc/nginx/sites-enabled/* /etc/nginx/sites-enabled.bak/ 2>/dev/null || true

# Install the new one
sudo cp ~/lakko/infra/nginx/sites-available/lakko.conf /etc/nginx/sites-available/
sudo cp ~/lakko/infra/nginx/sites-available/lakko-proxy-headers.inc /etc/nginx/sites-available/
sudo ln -sf /etc/nginx/sites-available/lakko.conf /etc/nginx/sites-enabled/lakko.conf

# Validate + reload
sudo nginx -t
sudo systemctl reload nginx
```

### 15. End-to-end check

```bash
curl https://lakko.me/health
curl https://api.lakko.me/health
```

Expected: `lakko.me ok` and `api.lakko.me ok` (these are at the nginx layer). `/api/auth/...` etc. will return 502 until the services have real logic (Phase 1.1+).

### 16. coturn — leave it alone

```bash
sudo systemctl status coturn
```

If it is running, leave it. Phase 3 (calls) will integrate with it.

## Daily ops

### Deploy

From the Mac:

```bash
just deploy
```

(needs an ssh key for `lakkome@lakko.me`.) What it does: ssh-es to the VPS and runs `~/lakko/infra/scripts/deploy.sh` as `lakkome`. The script does `git pull` → `cargo build --release` → `sudo install /usr/local/bin/...` → `sudo systemctl restart lakko.target`.

### Restart a single service

```bash
ssh lakkome@lakko.me sudo systemctl restart lakko-svc-auth
```

### Logs

```bash
ssh lakkome@lakko.me journalctl -u lakko-svc-auth -f
ssh lakkome@lakko.me journalctl -u lakko.target --since "1 hour ago"
```

### Whole-stack status

```bash
ssh lakkome@lakko.me systemctl status lakko.target
```

## Backup / restore

Phase 2 sprint task. Bare minimum:

- MariaDB dump → Hetzner Storage Box daily: `mariadb-dump --single-transaction lakko | zstd > /backup/lakko-$(date +%F).sql.zst`
- `/var/lib/lakko/blobs` — weekly rsync snapshot to Storage Box
- `/etc/lakko/` — back up whenever secrets change
- `/etc/letsencrypt/` — back up before renewal

## Out of scope for Phase 1

- Multi-region / failover (Phase 5)
- Container registry / podman (whenever horizontal scaling becomes a real need)
- TLS certificate pinning on the client (Phase 1 sprint 1.8)
- DDoS protection beyond Cloudflare (Phase 4-5 multi-CDN)
- Moving coturn into a managed service (Phase 3 — calls)
- Hardware security module integration (Phase 5)
- Rolling update without downtime (currently systemd restart with Restart=on-failure)
- Dedicated `lakko` system user (instead of `lakkome`) — Phase 2 hardening
