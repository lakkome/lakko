#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# One-time SFU setup on the MAIN VPS (ADR-0011 — Rust SFU,
# supersedes the LiveKit-fork CCX22 bootstrap of ADR-0008).
#
# The Rust SFU (`lakko-svc-sfu`) co-locates with the API services
# on the main VPS. Its BINARY + systemd unit + lakko.env are
# handled by the regular flow:
#   - `just deploy` builds + installs /usr/local/bin/lakko-svc-sfu
#     (deploy.sh now includes `sfu` in SERVICES) and restarts
#     lakko.target (which now Wants lakko-sfu.service).
#   - lakko.env carries the LAKKO_SFU_* vars (see
#     infra/etc-lakko/lakko.env.example).
#
# This script does ONLY the SFU-specific one-time bits that
# `just deploy` does not:
#   1. Issue / ensure the Let's Encrypt cert for sfu.lakko.me
#      (the SFU runs behind its own nginx vhost / subdomain).
#   2. Install + enable the nginx vhost
#      (infra/nginx/sites-available/sfu.conf).
#   3. Open the firewall for the public UDP media port
#      (LAKKO_SFU_MEDIA_BIND port; default 43210/udp). 443/tcp is
#      already open for the API.
#
# Idempotent — safe to re-run. Run as root on the main VPS:
#   sudo SFU_DOMAIN=sfu.lakko.me MEDIA_UDP_PORT=43210 \
#        bash infra/scripts/deploy-sfu.sh

set -euo pipefail

SFU_DOMAIN="${SFU_DOMAIN:-sfu.lakko.me}"
# Must match the port in LAKKO_SFU_MEDIA_BIND (lakko.env). str0m
# gathers host ICE candidates on this UDP port; clients send RTP
# straight to it (it does NOT go through nginx).
MEDIA_UDP_PORT="${MEDIA_UDP_PORT:-43210}"
CERTBOT_EMAIL="${CERTBOT_EMAIL:-ops@lakko.me}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

note() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\n\033[1;33mWARN:\033[0m %s\n' "$*"; }

# ----------------------------------------------------------
# 0. Sanity — must be root.
# ----------------------------------------------------------
if [[ $EUID -ne 0 ]]; then
  echo "must run as root (sudo $0)" >&2
  exit 1
fi

# ----------------------------------------------------------
# 1. Pre-flight reminders (binary + env are NOT this script's job).
# ----------------------------------------------------------
if [[ ! -x /usr/local/bin/lakko-svc-sfu ]]; then
  warn "/usr/local/bin/lakko-svc-sfu missing — run 'just deploy' first (it builds + installs it)"
fi
if [[ ! -f /etc/lakko/lakko.env ]]; then
  warn "/etc/lakko/lakko.env missing — author it from infra/etc-lakko/lakko.env.example"
  warn "(LAKKO_SFU_JWKS_URL + LAKKO_SFU_MEDIA_BIND=0.0.0.0:${MEDIA_UDP_PORT} + LAKKO_SFU_MEDIA_ADVERTISE_IP=<public IP> at minimum)"
fi
# The mTLS CA for the /metrics scrape (shared with the other
# services' monitoring setup).
install -d -m 700 /etc/lakko/mtls
if [[ ! -f /etc/lakko/mtls/monitoring-ca.crt ]]; then
  warn "/etc/lakko/mtls/monitoring-ca.crt missing — the /metrics location will 400 until the monitoring CA cert is installed"
fi

# ----------------------------------------------------------
# 2. TLS cert for the SFU subdomain.
# ----------------------------------------------------------
if [[ ! -d /etc/letsencrypt/live/$SFU_DOMAIN ]]; then
  note "issuing Let's Encrypt cert for $SFU_DOMAIN"
  # --nginx plugin: the main VPS already runs nginx for the API
  # domains, so reuse it rather than --standalone (which would
  # need port 80 free).
  certbot certonly --nginx -d "$SFU_DOMAIN" \
    --non-interactive --agree-tos -m "$CERTBOT_EMAIL"
else
  note "cert for $SFU_DOMAIN already present — skipping"
fi

# ----------------------------------------------------------
# 3. nginx vhost.
# ----------------------------------------------------------
note "installing nginx vhost for $SFU_DOMAIN"
install -m 644 "$REPO_ROOT/nginx/sites-available/sfu.conf" /etc/nginx/sites-available/sfu.conf
ln -sf /etc/nginx/sites-available/sfu.conf /etc/nginx/sites-enabled/sfu.conf
nginx -t
systemctl reload nginx

# ----------------------------------------------------------
# 4. Firewall — open the public UDP media port. 443/tcp is
#    already open for the API; the SFU signaling rides it.
# ----------------------------------------------------------
if command -v ufw >/dev/null 2>&1; then
  note "opening ${MEDIA_UDP_PORT}/udp for SFU media (RTP)"
  ufw allow "${MEDIA_UDP_PORT}/udp" comment 'lakko sfu media (str0m RTP)'
else
  warn "ufw not found — open ${MEDIA_UDP_PORT}/udp for SFU media manually"
fi

note "done."
note "  - ensure lakko.env has LAKKO_SFU_JWKS_URL + LAKKO_SFU_MEDIA_BIND=0.0.0.0:${MEDIA_UDP_PORT} + LAKKO_SFU_MEDIA_ADVERTISE_IP=<public IP>"
note "    (without the advertise IP the SFU refuses to start: 0.0.0.0 is not a usable ICE candidate)"
note "  - run 'just deploy' to build + install lakko-svc-sfu + start it via lakko.target"
note "  - verify:  curl -sI https://${SFU_DOMAIN}/health"
note "  - logs:    just logs-sfu"
