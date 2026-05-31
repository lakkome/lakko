#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Bootstrap script for the coturn VPS (Phase 3a2 / 3b3).
#
# Run once on a fresh Hetzner CX22 (Ubuntu 24.04 LTS). Idempotent
# — safe to re-run.
#
# Prereqs:
#   - DNS A + AAAA for turn.lakko.me → this VPS's IP.
#   - The TURN_SHARED_SECRET value (services/auth needs the same
#     bytes to mint matching credentials).

set -euo pipefail

TURN_DOMAIN="${TURN_DOMAIN:-turn.lakko.me}"
TURN_SECRET="${TURN_SHARED_SECRET:-}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

note() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\n\033[1;33mWARN:\033[0m %s\n' "$*"; }

if [[ $EUID -ne 0 ]]; then
  echo "must run as root (sudo $0)" >&2
  exit 1
fi

# ----------------------------------------------------------
# 1. System packages.
# ----------------------------------------------------------
note "installing system packages"
apt-get update
# G3-turn-deploy — `nginx` added so the mTLS-fronted Prometheus
# scrape on :9642 can be installed below. Bundle gets coturn +
# certbot + UFW + node_exporter + nginx in one pass.
DEBIAN_FRONTEND=noninteractive apt-get install -y \
  coturn certbot ufw prometheus-node-exporter nginx

# Ubuntu's coturn package ships disabled by default — we'll
# manage start/stop through our own override unit.
sed -i 's/^TURNSERVER_ENABLED=.*/TURNSERVER_ENABLED=1/' /etc/default/coturn || true

# ----------------------------------------------------------
# 2. TLS cert.
# ----------------------------------------------------------
if [[ ! -d /etc/letsencrypt/live/$TURN_DOMAIN ]]; then
  note "issuing Let's Encrypt cert for $TURN_DOMAIN"
  # certbot --standalone needs port 80 free. Stop nginx if it's
  # there from a previous co-located deploy.
  systemctl stop coturn 2>/dev/null || true
  certbot certonly --standalone -d "$TURN_DOMAIN" \
    --non-interactive --agree-tos -m "ops@$TURN_DOMAIN"
fi

# turnserver runs as `turnserver` user; it needs to read the
# Let's Encrypt files. Add it to the ssl-cert group + tighten
# permissions.
note "granting turnserver access to TLS certs"
usermod -a -G ssl-cert turnserver || true
chgrp -R ssl-cert /etc/letsencrypt/live /etc/letsencrypt/archive
chmod -R g+rX /etc/letsencrypt/live /etc/letsencrypt/archive

# ----------------------------------------------------------
# 3. turnserver.conf.
# ----------------------------------------------------------
note "installing turnserver.conf"
if [[ -z "$TURN_SECRET" ]]; then
  note "generating fresh TURN_SHARED_SECRET"
  TURN_SECRET=$(head -c 32 /dev/urandom | base64)
  echo
  echo "TURN_SHARED_SECRET=$TURN_SECRET"
  echo
  warn "save the above value to /etc/lakko/lakko.env on the auth VPS"
  warn "(services/auth uses it to mint TURN credentials clients"
  warn " present to this coturn instance)"
fi
install -m 640 -g turnserver "$REPO_ROOT/coturn/turnserver.conf" /etc/turnserver.conf
sed -i "s|REPLACE-WITH-RANDOM-32-BYTES-BASE64|$TURN_SECRET|" /etc/turnserver.conf

# ----------------------------------------------------------
# 4. systemd override.
# ----------------------------------------------------------
note "installing systemd override"
install -d /etc/systemd/system/coturn.service.d
install -m 644 "$REPO_ROOT/systemd/lakko-coturn.service" \
  /etc/systemd/system/coturn.service.d/lakko-override.conf
systemctl daemon-reload

# ----------------------------------------------------------
# 5. nginx mTLS-fronted Prometheus scrape.
# ----------------------------------------------------------
# coturn's internal Prometheus exporter speaks plain HTTP on
# 127.0.0.1:9641. We do NOT expose that port to the open
# internet — the monitoring VPS scrapes through nginx on
# `turn.lakko.me:9642` with mTLS client-cert verification.
# See `infra/nginx/sites-available/turn.conf` for the full
# vhost shape.
note "installing nginx mTLS frontend for coturn metrics"
install -m 644 "$REPO_ROOT/nginx/sites-available/turn.conf" \
  /etc/nginx/sites-available/turn.conf
ln -sf /etc/nginx/sites-available/turn.conf \
  /etc/nginx/sites-enabled/turn.conf

# The mTLS client-CA cert must already be present at
# `/etc/lakko/mtls/monitoring-ca.crt` (deployed via the
# `scp monitoring-ca.crt root@turn.lakko.me:/etc/lakko/mtls/`
# step in the first-time deploy runbook —
# `infra/README-calls.md`). Refuse to start nginx if it is
# missing rather than booting an unauthenticated metrics
# endpoint.
if [[ ! -f /etc/lakko/mtls/monitoring-ca.crt ]]; then
  warn "/etc/lakko/mtls/monitoring-ca.crt is missing — the"
  warn "mTLS-fronted scrape will refuse client connects until"
  warn "you scp the monitoring CA cert here. nginx will still"
  warn "start (the vhost is disabled-by-default until the cert"
  warn "lands) but the Prometheus scrape will return 400."
fi

# Validate the config + reload. `nginx -t` exits non-zero on
# syntax errors, set -e turns that into a failed deploy.
nginx -t
systemctl enable --now nginx
systemctl reload nginx || systemctl restart nginx

# ----------------------------------------------------------
# 6. Let's Encrypt renewal hook.
# ----------------------------------------------------------
# Drop a deploy-hook that reloads BOTH coturn and nginx after
# every successful certbot renewal. Without this hook, coturn
# would keep serving the expired cert until the next manual
# restart — and the cert is the same one nginx uses on :9642,
# so the scrape endpoint would also break.
note "installing letsencrypt renew hook"
install -d /etc/letsencrypt/renewal-hooks/deploy
install -m 755 "$REPO_ROOT/letsencrypt/lakko-coturn-renew-hook.sh" \
  /etc/letsencrypt/renewal-hooks/deploy/lakko-coturn

# ----------------------------------------------------------
# 7. Firewall.
# ----------------------------------------------------------
note "configuring ufw"
ufw allow 22/tcp comment 'ssh'
ufw allow 443/tcp comment 'turn-over-tls'
ufw allow 3478 comment 'turn (udp+tcp)'
ufw allow 49152:65535/udp comment 'turn relay range'
ufw allow 9100/tcp comment 'node_exporter'  # mTLS-protected
# G3-turn-deploy — mTLS-fronted Prometheus scrape lives on
# 9642 (see nginx vhost). 9641 stays internal-only because
# coturn's exporter speaks plain HTTP.
ufw allow 9642/tcp comment 'coturn prometheus (mTLS via nginx)'
ufw --force enable

# ----------------------------------------------------------
# 8. Bring it up.
# ----------------------------------------------------------
note "enabling + starting coturn"
systemctl enable --now coturn

note "done. tail logs with: journalctl -u coturn -f"
note "verify TURN-over-TLS: openssl s_client -connect $TURN_DOMAIN:443 -servername $TURN_DOMAIN < /dev/null 2>&1 | head -5"
note "verify Prometheus:    curl --cert /path/to/monitoring.crt --key /path/to/monitoring.key https://$TURN_DOMAIN:9642/metrics | head -20"
note "test smoke harness:   bash infra/scripts/turn-smoke.sh $TURN_DOMAIN"
