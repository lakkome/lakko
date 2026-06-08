---
name: infra-deploy
description: Background knowledge for Lakko deployment + ops — single-VPS Hetzner via systemd + nginx + MariaDB, the justfile deploy targets, SFU + coturn deploy, and per-service env config. Auto-applies when working on infra/ or deployment/ops tasks. Deploy commands have side effects — confirm before running them.
---

# Lakko infra + deploy — domain knowledge

Deployment is a single Hetzner VPS running every backend service under systemd, fronted by system-nginx, backed by system-MariaDB. There is no Kubernetes, no Docker in production (Docker is dev-only for MariaDB). coturn runs on a separate VPS.

## Layout (`infra/`)

| Dir | Owns |
|---|---|
| `systemd/` | One unit per service (`lakko-svc-<name>.service`) + `lakko.target` + `lakko-sfu.service` (note: unit is `lakko-sfu`, NOT `lakko-svc-sfu`). |
| `nginx/sites-available/` | One vhost per public host (auth / ds / storage / channels / sfu...). `sfu.conf` fronts `sfu.lakko.me`. |
| `mariadb/` | DB config. Each service has its OWN database. |
| `scripts/` | `deploy.sh` (build + install + restart), `deploy-sfu.sh` (one-time SFU vhost + cert + UDP firewall). |
| `coturn/` | `turnserver.conf` (TURN-over-TLS, `turn.lakko.me`). |
| `prometheus/` | Scrape config (mTLS `/metrics`). |
| `etc-lakko/` | `/etc/lakko/lakko.env` template — the systemd `EnvironmentFile`. |
| `letsencrypt/` | Cert management. |
| `nix/` | Reproducible toolchain pin. |
| `README.md` / `README-calls.md` | Full deploy + ops runbook. Read these before non-trivial ops. |

## Production config mechanism

- Production uses systemd `EnvironmentFile=/etc/lakko/lakko.env` — NOT `.env`/dotenvy (dotenvy is `#[cfg(debug_assertions)]` only).
- Each service reads `LAKKO_<NAME>_DATABASE_URL`, falling back to generic `DATABASE_URL`.
- The SFU UDP media plane (`LAKKO_SFU_MEDIA_BIND`, default `0.0.0.0:43210`) is a DIRECT public port — not proxied through nginx (nginx only fronts the WS signaling `/v1/connect` + health endpoints).

## Deploy commands (HAVE SIDE EFFECTS — confirm before running)

```
just deploy            # ssh VPS -> deploy.sh: build + install + systemctl restart (all services incl. sfu, channels)
just restart           # systemctl restart lakko.target
just restart-svc <svc> # systemctl restart lakko-svc-<svc>
just logs <svc>        # journalctl -u lakko-svc-<svc> -f
just status            # systemctl status lakko.target

# SFU
just deploy-sfu        # ONE-TIME: cert + nginx vhost + UDP firewall for sfu.lakko.me (binary rides regular `just deploy`)
just restart-sfu       # systemctl restart lakko-sfu   (NOTE: lakko-sfu, not lakko-svc-sfu)
just logs-sfu          # journalctl -u lakko-sfu -f
just dev-sfu           # local: cargo run -p lakko-svc-sfu

# coturn (separate VPS, LAKKO_TURN_HOST default turn.lakko.me)
just deploy-coturn     # idempotent bootstrap/refresh; reloads coturn at the end
just reload-coturn     # SIGUSR2 reload (cert + config, no session drop)
just restart-coturn    # full restart — DROPS live relay sessions; only for listening-port changes
just logs-coturn       # journalctl -u coturn -f on the coturn VPS
just turn-smoke [host] # local smoke probe (DNS / STUN / TURN-over-TLS / mTLS gate)
```

## Ops safety rules

- `just deploy` rebuilds AND restarts every service — it is not a dry run. Confirm with the maintainer before running, and never run it speculatively.
- `restart-coturn` drops live calls; prefer `reload-coturn` unless a listening port changed.
- Migrations: a service refuses to start if it sees another service's migrations in its DB. Two services must NOT share a database (`sqlx` tracks `_sqlx_migrations` per DB).
- `lakko.env` holds secrets — never echo its contents into the chat or commit it.
- The stale LiveKit-fork infra (ADR-0008 — `livekit.yaml`, go build, separate CCX22) was retired; the SFU is the Rust `lakko-svc-sfu` co-located on the main VPS (ADR-0011).

## Health checks

Every service exposes `/health` (liveness), `/ready` (DB-dependent), `/version` (JSON). The SFU adds `/v1/connect` (WS) + mTLS `/metrics` → `127.0.0.1:9641`.

## When editing systemd units / nginx vhosts

- Match the existing naming: services are `lakko-svc-<name>`, the SFU exception is `lakko-sfu`.
- A new public service needs: a systemd unit (Wanted by `lakko.target`), an nginx vhost, a Let's Encrypt cert, an `/etc/lakko/lakko.env` entry, and a DB + grant.
- Logs must be plain text (no emoji, ASCII-only structured prefixes like `[auth]`, `OK`, `WARN`, `ERR`) per the CLAUDE.md logging rule.
