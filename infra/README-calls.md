<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Calls infra — operational playbook

Maps Phase 3 call-side sub-sprints to the text files in this
tree. Use this as the index when you spin up the call infra
for the first time, or when you're paged at 3 AM and need to
remember which VPS owns what.

## Topology

```
                    ┌──────────────────────┐
                    │  Monitoring CX22     │
                    │  • Prometheus        │◄────┐ mTLS scrape
                    │  • Grafana           │     │
                    │  • Alertmanager      │     │
                    └──────────┬───────────┘     │
                               │ webhook         │
                               ▼                 │
                          Slack / email          │
                                                 │
       ┌─────────────────────────────────────────┴─┐
       │                                            │
       ▼                                            ▼
┌─────────────────┐                          ┌─────────────────┐
│  Main VPS       │                          │  coturn CX22    │
│  • API services │                          │  • coturn       │
│  • lakko-sfu    │                          │  • nginx (TLS)  │
│  • nginx (TLS)  │                          │  • node_exporter│
│  • node_exporter│                          │  turn.lakko.me  │
│  api/sfu.lakko.me│                         └─────────────────┘
└─────────────────┘
```

**SFU co-location (ADR-0011).** The Rust str0m SFU
(`lakko-svc-sfu`) is light (target <= 20 MB per active call), so
for V1 it co-locates with the API services on the main VPS
rather than the separate CCX22 the LiveKit-fork plan assumed
(ADR-0008). nginx fronts both `api.lakko.me` and `sfu.lakko.me`;
the SFU's UDP media plane is a direct public port. coturn +
monitoring stay on their own boxes (TURN is bandwidth-heavy; the
monitoring host watches the others, not shares their failure
modes). If load outgrows the shared box, the unit + vhost move
to a dedicated VPS unchanged — only DNS + the firewall move with
them.

## Sub-sprint → file map

| Sprint | Status | Files |
|---|---|---|
| **3a2** ICE infrastructure | Configs + deploy + nginx mTLS + renew hook + smoke ready (G3-turn-deploy May 2026) | [`coturn/turnserver.conf`](coturn/turnserver.conf), [`systemd/lakko-coturn.service`](systemd/lakko-coturn.service), [`scripts/deploy-coturn.sh`](scripts/deploy-coturn.sh), [`nginx/sites-available/turn.conf`](nginx/sites-available/turn.conf), [`letsencrypt/lakko-coturn-renew-hook.sh`](letsencrypt/lakko-coturn-renew-hook.sh), [`scripts/turn-smoke.sh`](scripts/turn-smoke.sh) |
| **3b2** SFU deployment (ADR-0011 Rust SFU, co-located) | Configs ready — Rust `lakko-svc-sfu`, env-var configured, in `lakko.target`, built by `just deploy`; one-time setup via `just deploy-sfu` | [`systemd/lakko-sfu.service`](systemd/lakko-sfu.service), [`nginx/sites-available/sfu.conf`](nginx/sites-available/sfu.conf), [`scripts/deploy-sfu.sh`](scripts/deploy-sfu.sh) |
| **3b3** TURN-over-TLS | Bundled with 3a2 | Same files — coturn handles both `:3478` STUN/TURN and `:443` TURN-over-TLS in one binary |
| **3b5** Monitoring | Configs ready | [`prometheus/prometheus.yml`](prometheus/prometheus.yml), [`prometheus/rules/calls.yml`](prometheus/rules/calls.yml) |
| **3b6** Cost analysis | Pending real-load measurements | See "Cost model" below; locked off real bandwidth + active-call counts |
| **3b1c** LiveKit Go fork | **Superseded by ADR-0011** (Rust str0m SFU) — the Go-fork vendor work no longer applies | [`services/sfu/UPSTREAM.md`](../services/sfu/UPSTREAM.md), [`services/sfu/PLAYBOOK.md`](../services/sfu/PLAYBOOK.md) |

## First-time deploy

Order matters — `services/auth` needs `TURN_SHARED_SECRET`
before clients can build TURN credentials.

```bash
# 1. Provision the VPSes. The SFU co-locates on the main API
#    VPS (ADR-0011), so for V1 you need only the main VPS +
#    coturn (+ optional monitoring). Set DNS A + AAAA records:
#      api.lakko.me   → main VPS
#      sfu.lakko.me   → main VPS  (same IP — SFU co-located)
#      turn.lakko.me  → coturn VPS
#      monitor.lakko.me (optional) → monitoring VPS

# 2. SFU config. The binary (`lakko-svc-sfu`) is a regular
#    workspace crate — NO go build, NO livekit.yaml; all config
#    is env-vars. On the main VPS, add the LAKKO_SFU_* block from
#    infra/etc-lakko/lakko.env.example to /etc/lakko/lakko.env:
#      LAKKO_SFU_JWKS_URL=https://auth.lakko.me/.well-known/jwks.json
#      LAKKO_SFU_MEDIA_BIND=0.0.0.0:43210
#    (LAKKO_SFU_BIND / _METRICS_BIND default to loopback 7880 / 9641).

# 3. coturn VPS bootstrap (idempotent). `just deploy-coturn`
#    wraps it — same posture as `just deploy` for the main fleet.
just deploy-coturn   # rsync infra/ → root@turn.lakko.me + run deploy-coturn.sh
# Note the TURN_SHARED_SECRET it prints — paste it into
# /etc/lakko/lakko.env on the main VPS too.
# Day-to-day: `just restart-coturn`, `just reload-coturn`,
# `just logs-coturn`, `just turn-smoke` (local smoke probe).

# 4. SFU one-time setup on the main VPS: issue the sfu.lakko.me
#    cert, install the nginx vhost, open the UDP media port. The
#    BINARY + systemd unit ride the regular `just deploy`
#    (deploy.sh builds + installs lakko-svc-sfu; lakko.target
#    Wants lakko-sfu.service).
just deploy-sfu      # ssh main VPS + sudo bash infra/scripts/deploy-sfu.sh
just deploy          # build + install all service binaries (incl. sfu) + restart lakko.target
# Day-to-day: `just restart-sfu`, `just logs-sfu`.

# 5. Monitoring VPS. Issue the mTLS client CA + scrape cert
#    on your laptop (one-off operation; private CA lives only
#    on your machine + the monitoring VPS):
mkdir -p /tmp/lakko-mtls && cd /tmp/lakko-mtls
openssl req -x509 -newkey rsa:4096 -nodes -days 3650 \
  -keyout monitoring-ca.key -out monitoring-ca.crt \
  -subj "/CN=Lakko Monitoring CA"
openssl req -newkey rsa:4096 -nodes \
  -keyout monitoring.key -out monitoring.csr \
  -subj "/CN=monitor.lakko.me"
openssl x509 -req -in monitoring.csr -CA monitoring-ca.crt \
  -CAkey monitoring-ca.key -CAcreateserial \
  -out monitoring.crt -days 365
# Distribute:
scp monitoring-ca.crt root@sfu.lakko.me:/etc/lakko/mtls/
scp monitoring-ca.crt root@turn.lakko.me:/etc/lakko/mtls/
scp monitoring.{crt,key} root@monitor.lakko.me:/etc/prometheus/mtls/

# 6. Monitoring VPS: install Prometheus + Grafana + Alertmanager.
ssh root@monitor.lakko.me '
  apt-get update && apt-get install -y prometheus grafana alertmanager
'
scp infra/prometheus/prometheus.yml root@monitor.lakko.me:/etc/prometheus/
scp infra/prometheus/rules/calls.yml root@monitor.lakko.me:/etc/prometheus/rules/
ssh root@monitor.lakko.me 'systemctl reload prometheus'

# 7. Smoke test. From your laptop:
curl -sI https://sfu.lakko.me/health         # expect HTTP/2 200
openssl s_client -connect turn.lakko.me:443 \
  -servername turn.lakko.me < /dev/null 2>&1 | head -3
# From any Lakko client, place a 1×1 call — the BandwidthController
# (#39) + QualityIndicator (3c6) will surface "Direct" or "Relay"
# based on which path won the ICE pair.
```

## Day-to-day ops

| Symptom | Where to look |
|---|---|
| Users see "Reconnecting…" banners | Grafana → SFU panel → packet loss + dropped streams; `journalctl -u lakko-sfu -f` on SFU VPS |
| New connections refused on `sfu.lakko.me` | `systemctl status lakko-sfu` + `systemctl status nginx` |
| TURN fallback failing | `journalctl -u coturn -f`, check TLS cert expiry: `openssl x509 -enddate -noout -in /etc/letsencrypt/live/turn.lakko.me/cert.pem` |
| Bandwidth budget alarm fired | Grafana → "Bandwidth budget" panel; consider second SFU VPS (see 3b6 below) |
| Alertmanager went quiet | `systemctl status alertmanager` + `curl localhost:9093/-/healthy` from monitoring VPS |

## Cost model (3b6)

Numbers are planning estimates — replace with real
measurements after the first month of production load.

**Bandwidth per user-minute** (one direction):

| Codec / resolution | Bitrate | Per minute | Per hour |
|---|---|---|---|
| Opus voice 32 kbps | ~240 KB | ~14 MB | ~860 MB |
| VP9 540p @ 30 fps | ~750 kbps | ~5.6 MB | ~340 MB |
| VP9 1080p @ 30 fps | ~2.5 Mbps | ~18.7 MB | ~1.1 GB |

**SFU egress** = upstream from publisher × (N − 1) receivers
per stream (selective forwarding doesn't duplicate to the
publisher itself).

For a 5-person 1080p call:
- Each publisher uploads ~2.5 Mbps to the SFU
- SFU forwards to 4 others = 10 Mbps egress per publisher
- Total egress for the call = 5 × 10 Mbps = 50 Mbps
- 1 hour = 50 Mbps × 3600 s / 8 = ~22.5 GB

Hetzner CCX22 budget = 20 TB/month. Math:
- 20 TB / 22.5 GB/hour ≈ 911 hours/month of busy 5-person
  1080p call activity. With 70 % efficiency factor (TCP
  overhead, retransmits, control plane) ≈ 640 hours/month.
- That's 21 hours of busy call per day, or ~7 simultaneous
  5-person 1080p rooms running 8 h/day.

**Scale-out triggers:**
- > 80 % of monthly transfer budget consumed by day 24 →
  provision second SFU VPS.
- p95 CPU on the SFU > 85 % sustained 5+ min → second VPS
  (capacity, not bandwidth).
- p95 RTT through TURN > 200 ms → second coturn VPS in
  a different region (Hetzner FSN1 + HEL1 give the EU
  user base reasonable spread).

Track in Grafana: `lakko/calls-overview` dashboard once
3b5-grafana-dashboard sprint lands.

## What's NOT in this tree (yet)

These are deliberate — call out so an auditor sees the
honest scope:

- **Grafana dashboard JSON.** Builds against the metrics
  Prometheus exposes here, but `calls-overview.json` waits
  for the first deploy + a real query baseline. A canned
  empty dashboard would be worse than a blank one.
- **Alertmanager routing config.** `/etc/alertmanager/alertmanager.yml`
  needs the maintainer's Slack webhook URL + email gateway —
  secrets that can't be committed. Template lives in the
  ops bible the maintainer keeps locally.
- **First-deploy mTLS CA.** The private CA above is a one-off
  ceremony per fleet — committing the recipe is fine, the
  artefacts themselves never land in git.
- **LiveKit `livekit.yaml`.** Bundled with the fork in
  `services/sfu/` (template) — landed there because it
  evolves with the LiveKit fork patches, not with infra.
- **Second SFU / coturn VPS configs.** When 3b6 triggers a
  scale-out, copy this tree, change the DNS names, run the
  same scripts. No additional infra patterns needed for V1.
