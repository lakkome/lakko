<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Local full-stack run (Docker Compose)

Bring up every Lakko backend service a client needs for the core flow —
**register → recover → username discovery → identity bundle + KeyPackage →
MLS 1×1 chat over the Delivery Service**, plus channels and push — with one
command. This is the reproducible local/dev/CI run of the whole stack.

> Production deploys via systemd on a single VPS — see
> [`infra/README.md`](../README.md) and [`infra/systemd/`](../systemd/). This
> Compose stack is the local mirror; it does **not** replace the systemd
> deployment.

## Prerequisites

- Docker Engine + the Compose v2 plugin (`docker compose version`).
- ~3 GB free disk for the build cache + images.

## Bring it up

```bash
docker compose -f infra/docker/docker-compose.yml up --build
```

First run compiles the whole Rust workspace once inside the builder image
(several minutes); subsequent runs reuse the build cache. Each service
applies its own SQL migrations against its own database on startup. Add
`-d` to detach. Bring it down with:

```bash
docker compose -f infra/docker/docker-compose.yml down          # keep data
docker compose -f infra/docker/docker-compose.yml down -v       # wipe DB + blobs
```

Convenience recipes are wired into the justfile:

```bash
just stack-up        # docker compose up --build -d
just stack-down      # docker compose down
just stack-logs      # follow all service logs
```

## Services + ports

All ports are published on `127.0.0.1` only.

| Service | Port | Purpose |
|---|---|---|
| `db` (MariaDB 11) | 3306 | one database per service (`lakko_auth`, `lakko_ds`, `lakko_discovery`, `lakko_channels`, `lakko_push_gw`) |
| `auth` | 8081 | OPAQUE register/login, session-token + call-JWT mint, WebAuthn, TURN creds, JWKS |
| `ds` | 8082 | MLS group journal + stranger-inbox mailbox |
| `storage` | 8083 | ciphertext blob proxy (local blob dir in this profile) |
| `discovery` | 8084 | OPRF username register/lookup, identity bundle, KeyPackage |
| `push-gw` | 8085 | encrypted push fan-out (LogOnlyProvider until real creds — below) |
| `channels` | 8086 | channel broadcast backend |

Health: `curl -sf http://127.0.0.1:8081/ready` (and `/health`). `channels`
exposes `/health` only.

## Pointing a client at the stack

The web client proxies `/api/{auth,discovery,ds,storage,push}` to these
ports in `apps/web/vite.config.ts`; run `just dev-web` alongside this stack.
Native clients take the base URLs from their build config (`LAKKO_*_URL` /
Info.plist / `BuildConfig`).

## Configuration

All secrets in the compose file are **fixed local-dev fixtures**, never
production values. Override per run via the environment (the compose reads
`${VAR:-default}`):

```bash
LAKKO_SIGNUP_POW_EFFORT_BITS=1 LAKKO_DISCOVERY_POW_EFFORT_BITS=1 \
  docker compose -f infra/docker/docker-compose.yml up --build
```

| Variable | Default | Notes |
|---|---|---|
| `LAKKO_SESSION_TOKEN_SECRET` | dev fixture (base64) | MUST be identical across auth + discovery + storage + push-gw (auth mints, the rest verify) |
| `LAKKO_SIGNUP_POW_EFFORT_BITS` / `LAKKO_DISCOVERY_POW_EFFORT_BITS` / `LAKKO_PUSH_GW_POW_EFFORT_BITS` | 8 | proof-of-work cost; set to `1` for fast scripted tests |
| `LAKKO_TURN_SHARED_SECRET` / `LAKKO_TURN_URIS` | dev fixtures | base64 secret; only used by the call-credential endpoint |
| `LAKKO_TOTP_ENC_KEY` | dev fixture (base64, 32 B) | encrypts TOTP secrets at rest (ADR-0018 factor (c)); unset disables the `/totp/*` routes (503). A DB-only leak defence, not a host-compromise one |
| `LAKKO_VAULT_ENC_KEY` | dev fixture (base64, 32 B) | wraps each account's word-vault row at rest (ADR-0049); unset disables the `/vault/*` routes (503) and clients fall back to the 24 words. Losing it destroys every vault |
| `LAKKO_RECOVERY_CONTACT_PEPPER` | dev fixture (base64, 32 B) | keys the phone/email contact + OTP-code hashes (ADR-0018 factor (d)); unset disables the `/recovery-contact/*` routes (503). The contact plaintext is NEVER stored/logged (invariant #1) — only a keyed hash. A DB-only leak defence, not a host-compromise one |
| `LAKKO_AUTH_HANDLE_PEPPER` | dev fixture (hex, 32 B) | keys the stored login handles (ADR-0054 S5); unset makes every route that names an account by login handle (login, recovery, reset, TOTP, word vault, phone/email recovery) answer 503. Losing or rotating it unbinds every handle |
| `LAKKO_SMTP_*` / `LAKKO_SMS_*` | unset (LogOnly) | OTP delivery adapters (ADR-0018 factor (d)). Email + SMS both default to a log-only sender in the dev stack (the code is written to the auth log, never sent). Set `LAKKO_SMTP_{HOST,PORT,USERNAME,PASSWORD,FROM}` to arm real SMTP email (lettre, rustls); set `LAKKO_SMS_{API_URL,ACCOUNT_SID,AUTH_TOKEN,FROM}` to arm a Twilio-shaped SMS adapter (reqwest, rustls) |
| `LAKKO_PUSH_GW_INTERNAL_SECRET` | dev fixture (base64, 32 B) | ADR-0054: the shared secret the DS presents when it forwards a group wake to push-gw's internal listener (`push-gw:8092`, on the compose network only, not published). The compose file feeds the same value to the DS as `LAKKO_DS_PUSH_GW_SECRET` |
| `LAKKO_DS_BINDING_PEPPER` | dev fixture (hex, 32 B) | ADR-0057: keys the DS's stored push bindings (one `push_gid` per journal, bound by its root or an admin); a wake whose `push_gid` is not the publishing journal's binding is dropped. Unset: `PUT .../push-binding` answers 503 and every wake is dropped. DS only |
| `LAKKO_DISCOVERY_FRESH_IDENTITY_COOLDOWN_SECS` | 3600 | per-account cool-down (seconds) on UNPROVEN AccountIdentity changes to a NEW key — the lost-everything publish (ADR-0017 (ii)). Blunts a compromised-ACCESS attacker churning fresh identities; `0` disables. A heal-back to the previous key and a proven rotation are never gated, so the cool-down can never lock a legitimate owner out |

### Enabling real push delivery

Without APNs / FCM / WebPush credentials, `push-gw` uses the
`LogOnlyProvider` (records envelopes instead of delivering them). To deliver
for real, add to the `push-gw` service's `environment:` block: the
`LAKKO_APNS_*` set (`KEY_PATH`, `KEY_ID`, `TEAM_ID`, `TOPIC`, `ENDPOINT`),
`LAKKO_FCM_SA_PATH`, and/or the `LAKKO_WEBPUSH_VAPID_*` set, and mount the
key files as read-only volumes.

### Calls (SFU)

The SFU carries a UDP media plane and ships its own image
([`services/sfu/Dockerfile`](../../services/sfu/Dockerfile)). To add it to
this stack, append an `sfu` service that builds from that Dockerfile, point
its JWKS fetch at `http://auth:8081/.well-known/jwks.json`, and publish its
signaling port (and the UDP media port if you exercise media). The
standalone SFU e2e stack lives at [`ci/sfu-e2e/`](../../ci/sfu-e2e/).

> `edge` (broadcast cascade / audio rooms) and `transport-rendezvous`
> (anti-censorship bootstrap) are later-phase services and are not wired
> into this Compose file yet.

## Troubleshooting

- **A service exits immediately** — check `docker compose logs <svc>`. A
  missing/garbled required env var (e.g. a non-base64
  `LAKKO_TURN_SHARED_SECRET`) makes auth refuse to start, by design.
- **`depends_on` waits forever** — the `db` healthcheck only passes once
  `initdb.sql` finished creating every database; give it the
  `start_period`. A corrupt volume: `down -v` and retry.
- **Migrations "missing"** — each service owns its own database; never point
  two services at the same one (sqlx tracks applied migrations per-DB).
