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
│   ├── lakko-svc-gif-proxy.service     # GIPHY search + media proxy (ADR-0047)
│   ├── lakko-svc-discovery.service     # username OPRF lookup
│   ├── lakko-svc-push-gw.service       # APNs / FCM / VAPID
│   └── lakko-svc-channels.service      # Phase 4 broadcast backend
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
| push-gw | 127.0.0.1:8085 (public API) + 127.0.0.1:8092 (internal, DS wake forward) | `journalctl -u lakko-svc-push-gw` |
| channels | 127.0.0.1:8086 | `journalctl -u lakko-svc-channels` |
| gif-proxy | 127.0.0.1:8091 | `journalctl -u lakko-svc-gif-proxy` |

system-nginx listens on 80/443 → reverse proxies to 127.0.0.1:808X. Services are not exposed externally.

push-gw's second listener (127.0.0.1:8092, `LAKKO_PUSH_GW_INTERNAL_BIND`) is **never** proxied: it serves only `/push/notify-group`, which the DS calls after a signed group publish that asked for a wake (ADR-0054 decision 1), authenticated by a shared secret (`LAKKO_PUSH_GW_INTERNAL_SECRET` on push-gw = `LAKKO_DS_PUSH_GW_SECRET` on the DS, plus `LAKKO_DS_PUSH_GW_URL=http://127.0.0.1:8092`). With the secret unset on push-gw the listener does not start and no group wake is delivered; with both DS variables unset the DS drops wakes; with only one DS variable set the DS refuses to start.

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
    CREATE DATABASE IF NOT EXISTS lakko_push_gw
        CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
    CREATE DATABASE IF NOT EXISTS lakko_channels
        CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
SQL

# Create the service user — password is NOT in git.
LAKKO_DB_PASS=$(openssl rand -hex 32)
sudo mariadb -e "
    CREATE USER 'lakko'@'localhost' IDENTIFIED BY '$LAKKO_DB_PASS';
    GRANT ALL PRIVILEGES ON lakko_auth.*      TO 'lakko'@'localhost';
    GRANT ALL PRIVILEGES ON lakko_ds.*        TO 'lakko'@'localhost';
    GRANT ALL PRIVILEGES ON lakko_discovery.* TO 'lakko'@'localhost';
    GRANT ALL PRIVILEGES ON lakko_push_gw.*   TO 'lakko'@'localhost';
    GRANT ALL PRIVILEGES ON lakko_channels.*  TO 'lakko'@'localhost';
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
#   LAKKO_PUSH_GW_DATABASE_URL=mysql://lakko:$LAKKO_DB_PASS@127.0.0.1:3306/lakko_push_gw
#   LAKKO_CHANNELS_DATABASE_URL=mysql://lakko:$LAKKO_DB_PASS@127.0.0.1:3306/lakko_channels

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

### Deploy — Rust services

From the Mac:

```bash
just deploy
```

(needs an ssh key for `lakkome@lakko.me`.) What it does: ssh-es to the VPS and runs `~/lakko/infra/scripts/deploy.sh` as `lakkome`. The script does `git pull` → `cargo build --release` → `sudo install /usr/local/bin/...` → `sudo systemctl restart lakko.target`.

### Deploy — apps/web (Phase 1.17c)

**Primary path:** GitHub Actions. Open the repo → Actions → "Deploy apps/web" → Run workflow. The CI runner builds in a controlled environment, ssh-rsyncs `dist/` into `/var/www/lakko-web/`, and reloads nginx. Artefact is byte-identical to a local build (verified weekly by `web-repro` in `.github/workflows/repro.yml`).

**Fallback** (CI down, unmerged branch, etc.): from the Mac:

```bash
just deploy-web
```

Same end-state — local `npm run build` → ssh-rsync → reload — using your own ssh agent.

#### One-time bootstrap on the VPS

The deploy paths assume the following are already in place on `lakko.me`:

```bash
# 1. Web root, owned by lakkome (writable for the rsync), readable by nginx.
sudo install -d -o lakkome -g www-data -m 755 /var/www/lakko-web

# 2. Sudoers entry — passwordless atomic install + nginx reload, scoped to
#    the exact commands the deploy paths invoke (rsync's args matter; sudo
#    matches them literally).
sudo tee /etc/sudoers.d/lakko-deploy-web <<'EOF'
lakkome ALL=(root) NOPASSWD: /usr/bin/rsync -a --delete /tmp/lakko-web-stage/ /var/www/lakko-web/, /usr/bin/systemctl reload nginx
EOF
sudo chmod 440 /etc/sudoers.d/lakko-deploy-web

# 3. nginx vhost — drop-in from the repo.
sudo cp /home/lakkome/lakko/infra/nginx/sites-available/lakko.conf /etc/nginx/sites-available/
sudo cp /home/lakkome/lakko/infra/nginx/sites-available/lakko-proxy-headers.inc /etc/nginx/sites-available/
sudo ln -sf /etc/nginx/sites-available/lakko.conf /etc/nginx/sites-enabled/lakko.conf
sudo nginx -t && sudo systemctl reload nginx
```

#### One-time bootstrap on GitHub (CI deploy only)

Generate a dedicated Ed25519 deploy key — DO NOT reuse your personal one:

```bash
ssh-keygen -t ed25519 -f /tmp/lakko_ci_deploy -N "" \
    -C "lakko-ci-deploy-$(date +%Y%m%d)"
ssh-keyscan -H lakko.me > /tmp/lakko_known_hosts
```

Add the public half on the VPS:

```bash
cat /tmp/lakko_ci_deploy.pub | ssh lakkome@lakko.me 'cat >> ~/.ssh/authorized_keys'
```

Add **two repository secrets** (Settings → Secrets and variables → Actions → New repository secret):

- `LAKKO_DEPLOY_SSH_KEY` — paste the contents of `/tmp/lakko_ci_deploy` (the **private** half).
- `LAKKO_DEPLOY_KNOWN_HOSTS` — paste the contents of `/tmp/lakko_known_hosts`.

Wipe the local copies once the secrets are saved:

```bash
shred -u /tmp/lakko_ci_deploy /tmp/lakko_ci_deploy.pub /tmp/lakko_known_hosts
```

Smoke the workflow once via "Run workflow" → check the run logs.

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

### Post-deploy smoke (Phase 1.22)

```bash
just smoke
```

Hits the public HTTPS surface from the dev machine and asserts every nginx route + every service `/health` + every same-origin `/api/*` proxy + every `/version` JSON is what we expect after the deploy. No ssh needed — pure outside-in. Override the bases when probing a tunnel / staging mirror:

```bash
just smoke https://staging.lakko.me https://api-staging.lakko.me
```

The script lives at `infra/scripts/staging-smoke.sh` and is the canonical "did this deploy actually land?" oracle. Run it after every `just deploy` (or after `just deploy-web` if you only touched apps/web). Exit code 0 = green; 1 = at least one route mismatched; 2 = `curl` missing locally.

### What the nginx vhosts expose

Two `server_name`s share one nginx config (`infra/nginx/sites-available/lakko.conf`); the web client uses both:

| Vhost | Routes | Purpose |
|---|---|---|
| `lakko.me` | `/`, `/assets/*`, `/sw.js`, `/manifest.webmanifest` | The web SPA root + hashed assets + the service worker + the PWA manifest |
| `lakko.me` | `/.well-known/lakko-release` (ADR-0038) | The signed "update available" manifest — a raw 89-byte Ed25519 blob served from a **separate** root `/var/www/lakko-release/` (NOT the SPA root) with `Cache-Control: no-store` |
| `lakko.me` | `/api/auth/*`, `/api/ds/*`, `/api/discovery/*`, `/api/storage/*` (1.19c1), `/api/push/*` (1.20b), `/api/gifs/*` (ADR-0047) | Same-origin proxy to the backend services so the web client's `connect-src 'self'` CSP stays valid |
| `api.lakko.me` | `/auth/*`, `/ds/*`, `/storage/*`, `/discovery/*`, `/push-gw/*`, `/gifs/*` | Canonical external API entry point for the iOS / Android / Tauri clients (different CSP boundary; CORS not needed because Lakko clients never cross the `lakko.me ↔ api.lakko.me` origin pair) |

Every `/api/*` path on `lakko.me` must be mirrored against a real upstream — the **vite dev proxy in `apps/web/vite.config.ts` is the source of truth for what the client expects**. If you add a new dev-proxy entry, the matching production `location /api/...` block here is mandatory. The 1.22 smoke probe will catch a missing route by failing the corresponding `/api/.../health` assertion.

### Release manifest (ADR-0038)

The "update available" notice is a raw **89-byte** Ed25519-signed blob, minted **offline** by `tools/release-manifest` and client-verified against a pinned publisher key. It is served as a static file at **`https://lakko.me/.well-known/lakko-release`** (an nginx `alias` to `/var/www/lakko-release/lakko-release.bin`, `Cache-Control: no-store`). The signing key is a **dedicated publisher key** — `services/auth` is never in the loop; neither the host serving the blob nor the zero-trust backend can forge one.

The manifest lives **outside** `/var/www/lakko-web/` on purpose: `just deploy-web` rsyncs the SPA with `--delete`, which would wipe the manifest on the next web deploy if it shared the SPA root.

**One-time bootstrap on the VPS** (a `lakkome`-owned dir so publishing needs no sudo; nginx reads it as `www-data`):

```bash
sudo install -d -o lakkome -g www-data -m 755 /var/www/lakko-release
```

**Generate the keypair — once, ideally on an air-gapped machine:**

```bash
just gen-release-key
# -> writes release-signing-key.seed (mode 0600) — keep OFFLINE, never commit
# -> prints the base64url public key; paste it into each client build config
#    (S4/S5): VITE_LAKKO_RELEASE_KEY / Info.plist LakkoReleaseKey /
#    BuildConfig.LAKKO_RELEASE_KEY
```

**Mint + publish a manifest — on each release:**

```bash
just mint-release-manifest <latest> <min_supported>   # e.g. 42 30
# -> writes release-manifest.bin (raw 89 bytes)
scp release-manifest.bin lakkome@lakko.me:/var/www/lakko-release/lakko-release.bin
```

Served **immediately** — no service restart, no nginx reload. Clients fetch the raw blob, verify the Ed25519 signature against the pinned key, and render the notice; an **absent** manifest (before the first publish, or a 404) means **no notice** (fail-open). `latest` is the newest published release number; `min_supported` is the oldest release the backend still accepts (a client below it is told it must update). `no-store` means a freshly-published manifest is picked up on the next fetch.

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
