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
│  SFU CCX22      │                          │  coturn CX22    │
│  • lakko-sfu    │                          │  • coturn       │
│  • nginx (TLS)  │                          │  • nginx (TLS)  │
│  • node_exporter│                          │  • node_exporter│
│  sfu.lakko.me   │                          │  turn.lakko.me  │
└─────────────────┘                          └─────────────────┘
```

Three independent VPSes. **Don't co-locate** — an SFU crash
shouldn't drag TURN down with it, and the monitoring host's
job is to watch the others, not to share their failure
modes.

## Sub-sprint → file map

| Sprint | Status | Files |
|---|---|---|
| **3a2** ICE infrastructure | Configs ready | [`coturn/turnserver.conf`](coturn/turnserver.conf), [`systemd/lakko-coturn.service`](systemd/lakko-coturn.service), [`scripts/deploy-coturn.sh`](scripts/deploy-coturn.sh) |
| **3b2** SFU deployment | Configs ready | [`systemd/lakko-sfu.service`](systemd/lakko-sfu.service), [`nginx/sites-available/sfu.conf`](nginx/sites-available/sfu.conf), [`scripts/deploy-sfu.sh`](scripts/deploy-sfu.sh) |
| **3b3** TURN-over-TLS | Bundled with 3a2 | Same files — coturn handles both `:3478` STUN/TURN and `:443` TURN-over-TLS in one binary |
| **3b5** Monitoring | Configs ready | [`prometheus/prometheus.yml`](prometheus/prometheus.yml), [`prometheus/rules/calls.yml`](prometheus/rules/calls.yml) |
| **3b6** Cost analysis | Pending real-load measurements | See "Cost model" below; locked off real bandwidth + active-call counts |
| **3b1c** LiveKit Go fork | Vendor work | [`services/sfu/UPSTREAM.md`](../services/sfu/UPSTREAM.md), [`services/sfu/PLAYBOOK.md`](../services/sfu/PLAYBOOK.md) |

## First-time deploy

Order matters — `services/auth` needs `TURN_SHARED_SECRET`
before clients can build TURN credentials.

```bash
# 1. Provision three Hetzner VPSes (CCX22 for SFU; CX22 for
#    coturn + monitoring). Set DNS A + AAAA records:
#      sfu.lakko.me   → SFU VPS
#      turn.lakko.me  → coturn VPS
#      monitor.lakko.me (optional) → monitoring VPS

# 2. Build the SFU binary from the fork. On a CI runner or
#    your laptop:
cd services/sfu
go build -o /tmp/lakko-sfu ./...
scp /tmp/lakko-sfu root@sfu.lakko.me:/usr/local/bin/lakko-sfu

# 3. Author livekit.yaml on the SFU VPS. Critical fields:
#      port: 7880
#      tcp_port: 7881
#      rtc.port_range_start: 50000
#      rtc.port_range_end:   60000
#      prometheus_port: 6789
#      keys:
#        jwks_url: https://auth.lakko.me/.well-known/jwks.json
#    (see services/sfu/README.md for the canonical shape)
ssh root@sfu.lakko.me 'install -m 640 -g lakkome /dev/stdin /etc/lakko/livekit.yaml' < livekit.yaml

# 4. Run the bootstrap on each VPS. Each script is idempotent.
ssh root@turn.lakko.me 'bash -s' < infra/scripts/deploy-coturn.sh
# Note the TURN_SHARED_SECRET it prints — paste it into
# /etc/lakko/lakko.env on the auth VPS too.

ssh root@sfu.lakko.me 'bash -s' < infra/scripts/deploy-sfu.sh

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
