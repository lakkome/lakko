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
| channels | 127.0.0.1:8086 (public API) + 127.0.0.1:8093 (internal, directory moderation) | `journalctl -u lakko-svc-channels` |
| gif-proxy | 127.0.0.1:8091 | `journalctl -u lakko-svc-gif-proxy` |

channels' second listener (`LAKKO_CHANNELS_INTERNAL_BIND`, default 127.0.0.1:8093) is **never** proxied: it serves only `POST /internal/v1/channels/{channel_id}/moderation` (`{"action": "remove"|"block"|"unblock", "reason": "<a-z0-9->"}`, ADR-0059 section 7), authenticated by `x-lakko-internal-secret` = `LAKKO_CHANNELS_INTERNAL_SECRET` (base64, at least 16 bytes). With the secret unset the listener does not start and listings cannot be removed by the operator. The service also needs `LAKKO_ANON_TOKEN_MASTER_SECRET` (the value auth and the other verifiers share) for subscribe and directory search; unset, both answer `503 anon-tokens-disabled`. Channel poll votes (ADR-0092 section 4) need `LAKKO_ANON_CRED_MASTER_SECRET`, the value auth issues the poll credential under (a master of its own, not the token one); unset, `PUT`/`DELETE .../votes/{tag}` and auth's `/auth/anon-credential*` answer `503 anon-credential-disabled`.

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
    CREATE DATABASE IF NOT EXISTS lakko_storage
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
    GRANT ALL PRIVILEGES ON lakko_storage.*   TO 'lakko'@'localhost';
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
#   LAKKO_STORAGE_DATABASE_URL=mysql://lakko:$LAKKO_DB_PASS@127.0.0.1:3306/lakko_storage
# and set LAKKO_STORAGE_PASS_SECRET=$(openssl rand -hex 32) (ADR-0065 blob passes).

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

Discovery loads the signed donations file as a systemd credential, and systemd
fails a unit whose `LoadCredential=` source is missing, so create the empty
placeholder first ("Donation methods" below replaces it):

```bash
sudo install -d -m 700 -o root -g root /etc/lakko/credstore
sudo sh -c 'umask 077; f=/etc/lakko/credstore/lakko-donations.json; \
  [ -f "$f" ] || printf "{\"methods\":[]}\n" > "$f"'
```

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

### Donation methods (`docs/ux/donations.md` section 3)

The donations page shows only methods signed **offline** by a dedicated
donations key: each payment link, QR image hash, crypto address and phone
number is an Ed25519 statement (`lakko-attest`'s `donation` module) that the web
client verifies against the key pinned into its build. The server can hide a
method; it cannot alter one. Discovery reads the signed file from the systemd
credential `lakko-donations` (source `/etc/lakko/credstore/lakko-donations.json`,
root:root 0600; systemd hands the service a private copy in
`$CREDENTIALS_DIRECTORY`). The country rule (`countries`) is the only unsigned
part.

**Generate the key - once, offline:**

```bash
just gen-donations-key
# -> donations-signing-key.seed (0600, keep OFFLINE, never commit) and the
#    base64url public key: pin it in the web build (VITE_LAKKO_DONATIONS_KEY)
#    and, optionally, in /etc/lakko/lakko.env as LAKKO_DISCOVERY_DONATIONS_KEY
#    (discovery then refuses a file whose signatures do not verify)
```

**Sign and install - whenever a method changes:**

```bash
# donations.json: {"methods":[{"id","kind": sbp|revolut|bmc|crypto,"link",
#   "qr_file" (path relative to the file),"phone","countries","coin","network","address",
#   "icon": btc|eth|ton (crypto only: the coin mark in the centre of its QR)}]}
just sign-donations donations.json        # -> donations.signed.json (0600)
scp donations.signed.json lakkome@lakko.me:/tmp/
ssh lakkome@lakko.me 'sudo install -m 600 -o root -g root /tmp/donations.signed.json \
  /etc/lakko/credstore/lakko-donations.json && rm /tmp/donations.signed.json && \
  sudo systemctl restart lakko-svc-discovery'
```

The two `sudo` commands are not in the deploy sudoers entry; run them in an
interactive session. A file discovery refuses (bad JSON, a statement that does
not match its entry, a QR that does not hash to the signed value, a bad
signature when the key is set) leaves the page empty and logs one warning
naming only the method's position. QR images are PNG, at most 512 KiB; the
signed file is at most 1 MiB (the tool refuses a larger one).

**At rest the file is plain.** Ubuntu 22.04 has systemd 249;
`LoadCredentialEncrypted=` needs 250. After an OS upgrade, encrypt it with
`systemd-creds encrypt --name=lakko-donations donations.signed.json
/etc/lakko/credstore.encrypted/lakko-donations.cred` and switch the unit line
as its comment says.

### Translation bundle (ADR-0061)

Translation fixes reach the iOS and Android apps between releases as one Ed25519-signed blob holding **every** interface language, served at **`https://lakko.me/.well-known/lakko-translations`** (an nginx `alias` to `/var/www/lakko-release/lakko-translations.bin`, `no-store`, the release-manifest directory above). Every device downloads the same bytes, so the request does not tell the server which language a device uses. The web client takes no bundle: a translation fix ships with the next web deploy.

The signer is a **dedicated translation-publisher key**, not the release key. A client accepts a bundle only if the signature verifies under its pinned key, the version is greater than both its built-in catalog version and the last bundle it accepted, and `min_release` is not above its own release number; a string for a key the built-in catalog marks security-critical is never taken from a bundle. Anything else and the client silently keeps its built-in translations.

**Generate the keypair — once, offline:**

```bash
just gen-translation-key
# -> translation-signing-key.seed (0600, never commit) + the base64url public
#    key for Info.plist LakkoTranslationKey / BuildConfig.LAKKO_TRANSLATION_KEY
```

**Sign + publish:**

```bash
just sign-translations <version> <min_release>   # e.g. 2 1
scp translation-bundle.bin lakkome@lakko.me:/var/www/lakko-release/lakko-translations.bin
```

`version` must be greater than the `version` in `l10n/catalog/en.json` that the builds in the field were compiled with. Served immediately, no reload.

### City database for device cards (ADR-0080)

`GET /discovery/locate` (anonymous, behind the existing `/discovery/` and `/api/discovery/` locations; no nginx change) answers the city and country of the request's own address so a device can put them in its end-to-end encrypted device card. Discovery reads **DB-IP City Lite** in MaxMind format from the path in `LAKKO_DISCOVERY_CITY_DB`. Unset or unreadable: the route answers `{}` and discovery logs one warning at startup; nothing else depends on it.

**Install and refresh monthly** (DB-IP publishes a new Lite file each month; the file name carries the month. The download URL below was written from DB-IP's naming and not fetched when this was written; if it fails, take the "MMDB" link from https://db-ip.com/db/download/ip-to-city-lite):

```bash
# on the VPS, as lakkome
mkdir -p /var/lib/lakko/geo
curl -fsSL -o /tmp/dbip-city-lite.mmdb.gz "https://download.db-ip.com/free/dbip-city-lite-$(date +%Y-%m).mmdb.gz"
gunzip -c /tmp/dbip-city-lite.mmdb.gz > /var/lib/lakko/geo/dbip-city-lite.mmdb.new
mv /var/lib/lakko/geo/dbip-city-lite.mmdb.new /var/lib/lakko/geo/dbip-city-lite.mmdb
# /etc/lakko/lakko.env: LAKKO_DISCOVERY_CITY_DB=/var/lib/lakko/geo/dbip-city-lite.mmdb
sudo systemctl restart lakko-svc-discovery   # the file is read once at startup
```

**Licence:** DB-IP City Lite is CC BY 4.0. The attribution "City data: DB-IP.com, CC BY 4.0" is in each client's Settings > About (`settings.about.city_data`). The route keeps no record of who asked; nginx access logging is already off on the discovery locations.

## Backup / restore

Two systemd timers run `infra/scripts/backup.sh` (ADR-0096 P2):

- `lakko-backup-db.timer`, daily at 03:15 UTC: every `lakko_*` database is
  dumped with `mariadb-dump --single-transaction`, compressed with zstd and
  encrypted to an age public key; the last `LAKKO_BACKUP_KEEP_DAYS` days
  (default 7) are kept in `/var/backups/lakko/db/` and mirrored to the
  Storage Box, so the box holds the same days.
- `lakko-backup-blobs.timer`, Sundays at 04:15 UTC: `/var/lib/lakko/blobs`
  is mirrored to the Storage Box. The blobs are client-encrypted
  attachments; nothing is added to them.

Each successful run writes `lakko_job_last_success_timestamp_seconds{task}`
to node_exporter's textfile directory; the monitoring host alerts when it
is older than 26 hours (dumps) or 8 days (blobs), and when no dump has ever
reported. A failed run writes nothing.

`just setup-metrics` (section "Monitoring" below) installs the script and the
timers and enables the timers once `LAKKO_BACKUP_TARGET` is set. The rest of
the one-time setup on the VPS:

```bash
sudo apt-get install -y age zstd rsync
sudo install -m 0755 infra/scripts/backup.sh /usr/local/bin/lakko-backup
sudo cp infra/systemd/lakko-backup-*.service infra/systemd/lakko-backup-*.timer /etc/systemd/system/
sudo mkdir -p /var/backups/lakko /var/lib/node_exporter/textfile_collector
# ssh key the Storage Box accepts, and its host key (port 23):
sudo ssh-keygen -t ed25519 -N '' -f /etc/lakko/backup_ssh_key
ssh-keyscan -p 23 u123456.your-storagebox.de | sudo tee /etc/lakko/backup_known_hosts
# In /etc/lakko/lakko.env: LAKKO_BACKUP_TARGET and LAKKO_BACKUP_AGE_RECIPIENT
# (an age public key whose private half is NOT on the VPS).
sudo systemctl daemon-reload
sudo systemctl enable --now lakko-backup-db.timer lakko-backup-blobs.timer
sudo systemctl start lakko-backup-db.service   # one run now, then check the journal
```

**A restore is a security incident, not a routine step.** A dump puts back
state that must only ever move forward: key generations and device session
floors (ADR-0073, ADR-0074: revoked devices and reset accounts come back),
the anonymous-token spent set (spent tokens work again), recall anchors,
the backup-head version counter (ADR-0037: version numbers reused), DS epoch
registrations and journal rows (ADR-0053, ADR-0057), and, once they run
here, the KT log and witness high-water marks (ADR-0020, ADR-0039). The
procedure:

1. Stop `lakko.target`. Restore **every** `lakko_*` database from the same
   night, never one alone: auth older than discovery makes discovery refuse
   every client's identity write as `key-generation-superseded`, and a
   client that receives that answer wipes itself (ADR-0073).
   `age -d -i <private key> lakko_ds.sql.zst.age | zstd -d | sudo mariadb`,
   once per database.
2. Before starting the services, in `/etc/lakko/lakko.env` replace
   `LAKKO_SESSION_TOKEN_SECRET` and `LAKKO_ANON_TOKEN_MASTER_SECRET`
   (`openssl rand -hex 32` each; the services that share a value must get
   the same new one). Every session and every outstanding anonymous token
   then fails: clients see `refresh-invalid` and ask the person to sign in
   again, which does not wipe them, and refill their tokens.
3. Move the backup-head counters past anything a client may have seen:
   `UPDATE lakko_auth.backup_head_versions SET version = version + 1000000;`.
4. A KT log or witness database restored shorter than what clients already
   verified is refused by every client that pinned a later root (no-rollback,
   fail closed); do not serve one without checking its size against the
   last published root.
5. Start `lakko.target`. What happened after the dump is gone: device
   revocations, key resets and account deletions made since then must be
   made again by the people who made them (the server cannot tell who),
   and conversations whose epoch moved past the dump need their devices to
   rejoin.

The age private key belongs offline, never on the main VPS or the Storage
Box: on that host it would undo the encryption for anyone who seizes it.
Dumps keep what was deleted: an account deleted today stays in up to
`LAKKO_BACKUP_KEEP_DAYS` days of dumps on the VPS and on the Storage Box, and
a blob deleted or swept stays on the Storage Box until the next weekly
mirror (`rsync --delete`).

Not backed up by the timers: `/etc/lakko/` (copy it whenever a secret
changes) and `/etc/letsencrypt/` (reissued by certbot if lost).

## Monitoring (ADR-0096 P3)

A separate small VPS runs Prometheus, Alertmanager, Grafana and
blackbox_exporter. It scrapes the Lakko hosts over client-certificate TLS
and sends alerts to a Telegram bot and e-mail, never through Lakko. Nothing
on it listens publicly; Grafana is reached over an SSH port forward.

| Host | What runs | Reached at |
|---|---|---|
| monitoring VPS (Ubuntu 24.04, x86_64) | Prometheus 127.0.0.1:9090 (30 days), Alertmanager :9093, blackbox :9115, node_exporter :9100, Grafana :3000, the canary :9661 (prod) and :9662 (test) | ssh only |
| main VPS | every service's `/metrics` on 127.0.0.1:9641 (SFU) and 9651-9660, node_exporter :9100, mysqld_exporter :9104, nginx exporter :9113 | `metrics.lakko.me:9643/<target>/metrics`, client certificate required, firewall admits the monitoring VPS only |
| coturn VPS | coturn's exporter :9641, node_exporter :9100 | `turn.lakko.me:9642/metrics` and `/node/metrics`, client certificate required, firewall admits the monitoring VPS only (when `deploy-coturn` ran with `LAKKO_MONITORING_IP`) |
| test stack (`stack="test"`, ADR-0096 P4) | as the main VPS, without transparency and witness; coturn there exports nothing | `test.lakko.me:9643/<target>/metrics`; see `infra/test-stack/README.md` |

Configuration in the repository: `infra/prometheus/` (scrape config, alert
rules, rule tests), `infra/monitoring/` (setup script, Alertmanager template,
blackbox modules, secrets example), `infra/grafana/` (datasource, six
dashboards as code), `infra/nginx/sites-available/metrics.conf`,
`infra/scripts/setup-metrics.sh`, `infra/journald/lakko.conf`.

**First setup, in this order:**

1. Create the monitoring VPS (Ubuntu 24.04, its own encrypted disk) and a DNS
   name for it; add an A record `metrics.lakko.me` pointing at the MAIN VPS.
2. On the monitoring VPS, as root: copy `infra/monitoring/secrets.env.example`
   to `/etc/lakko-monitoring/secrets.env` (mode 0600) and fill in the Telegram
   bot token, chat id and the SMTP relay.
3. From a checkout: `LAKKO_MONITORING_HOST=<name> just deploy-monitoring`.
   The first run makes the monitoring CA and Prometheus's client certificate
   in `/etc/lakko-monitoring/mtls` (the CA key stays there).
4. `just push-monitoring-ca` copies the CA certificate (not the key) to
   `/etc/lakko/mtls/monitoring-ca.crt` on the main and coturn VPSes.
5. `LAKKO_MONITORING_IP=<its address> just setup-metrics` on the main VPS
   (runs the script from the VPS checkout of `main`): exporters, the metrics
   server block and its certificate, journald retention, the schema-size and
   backup timers, the `LAKKO_<SVC>_METRICS_BIND` lines in `lakko.env`. Then
   `just restart` so the services open their listeners.
6. `LAKKO_MONITORING_IP=<its address> just deploy-coturn` once more:
   node_exporter moves to loopback behind the coturn vhost, its old public
   port 9100 is closed, and 9642 admits the monitoring VPS only.
7. `just grafana`, then http://127.0.0.1:3000 (user `admin`, password in
   `/etc/lakko-monitoring/grafana-admin-password` on the monitoring VPS). The
   Prometheus targets page is at `ssh -L 9090:127.0.0.1:9090`.

**After editing** rules, dashboards or the Alertmanager template: `just
deploy-monitoring` again. Dashboards edited in the Grafana UI are not kept;
change the JSON in `infra/grafana/dashboards/`.

**Checks that need no server** (Docker): `just monitoring-check` runs
`promtool check config`, the rule unit tests, `amtool check-config` on the
rendered template with the example secrets, and the blackbox config check.

**The canary (ADR-0096 P5).** `lakko-canary@prod` and `lakko-canary@test`
run on the monitoring VPS (`tools/canary`, metrics on 127.0.0.1:9661 and
:9662). Each holds two dedicated accounts that only talk to each other and
every 10 minutes signs both in, sends a message from one and decrypts it on
the other, moves an encrypted attachment between them and deletes it, and
every third run places a call whose SFrame-sealed audio must reach the
other side. The accounts' secrets and MLS state are in
`/var/lib/lakko-canary/<stack>/state.json` (mode 0600, user `lakko-canary`);
losing the file means running init again with new accounts. Setup, after
step 3 above: `just deploy-canary` (builds on the main VPS from `main`,
copies the binary), then `just canary-init prod` and `just canary-init test`.
The call step starts a day after init (a young account gets 8 call-join
tokens a day). Settings per stack in `/etc/lakko-monitoring/canary-<stack>.env`.
A step that fails twice in a row alerts (`LakkoCanaryStepFailing`).

**Not covered yet:** a STUN answer from coturn (blackbox has no STUN
prober; only the TLS handshake on 443 is probed), the obfs listeners, and an
alert when the monitoring VPS itself is down (nothing outside it watches
it).

## Out of scope for Phase 1

- Multi-region / failover (Phase 5)
- Container registry / podman (whenever horizontal scaling becomes a real need)
- TLS certificate pinning on the client (Phase 1 sprint 1.8)
- DDoS protection beyond Cloudflare (Phase 4-5 multi-CDN)
- Moving coturn into a managed service (Phase 3 — calls)
- Hardware security module integration (Phase 5)
- Rolling update without downtime (currently systemd restart with Restart=on-failure)
- Dedicated `lakko` system user (instead of `lakkome`) — Phase 2 hardening
