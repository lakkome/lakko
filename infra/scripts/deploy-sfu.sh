#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Bootstrap script for the LiveKit SFU VPS (Phase 3b2).
#
# Run once on a fresh Hetzner CCX22 (Ubuntu 24.04 LTS). Idempotent
# — safe to re-run; existing artefacts are left untouched.
#
# Prereqs:
#   - DNS A + AAAA for sfu.lakko.me → this VPS's IP.
#   - The Lakko-flavoured `lakko-sfu` binary already built on a
#     CI runner (or your laptop with `go build`) and uploaded to
#     /usr/local/bin/lakko-sfu on this host.
#   - /etc/lakko/livekit.yaml authored separately (see
#     services/sfu/README.md for the canonical shape).
#   - /etc/lakko/lakko.env carrying TURN_SHARED_SECRET so the
#     SFU can mint matching TURN creds for clients.

set -euo pipefail

SFU_DOMAIN="${SFU_DOMAIN:-sfu.lakko.me}"
LAKKO_USER="${LAKKO_USER:-lakkome}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

note() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\n\033[1;33mWARN:\033[0m %s\n' "$*"; }

# ----------------------------------------------------------
# 0. Sanity — must be root + Ubuntu 24+.
# ----------------------------------------------------------
if [[ $EUID -ne 0 ]]; then
  echo "must run as root (sudo $0)" >&2
  exit 1
fi
if ! grep -q "ID=ubuntu" /etc/os-release; then
  warn "non-Ubuntu host — proceed at your own risk"
fi

# ----------------------------------------------------------
# 1. System packages.
# ----------------------------------------------------------
note "installing system packages"
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y \
  nginx certbot python3-certbot-nginx \
  ufw prometheus-node-exporter

# ----------------------------------------------------------
# 2. Service user.
# ----------------------------------------------------------
if ! id -u "$LAKKO_USER" >/dev/null 2>&1; then
  note "creating service user '$LAKKO_USER'"
  useradd --system --shell /usr/sbin/nologin --home-dir /var/lib/lakko-sfu "$LAKKO_USER"
fi

# ----------------------------------------------------------
# 3. State + config dirs.
# ----------------------------------------------------------
note "creating /var/lib/lakko-sfu + /var/log/lakko-sfu + /etc/lakko"
install -d -o "$LAKKO_USER" -g "$LAKKO_USER" -m 750 /var/lib/lakko-sfu /var/log/lakko-sfu
install -d -m 750 /etc/lakko
install -d -m 700 /etc/lakko/mtls

if [[ ! -f /etc/lakko/livekit.yaml ]]; then
  warn "/etc/lakko/livekit.yaml is missing — author it before starting the service"
  warn "see services/sfu/README.md for the canonical shape"
fi
if [[ ! -f /etc/lakko/lakko.env ]]; then
  warn "/etc/lakko/lakko.env is missing — author it (TURN_SHARED_SECRET at minimum)"
fi

# ----------------------------------------------------------
# 4. Binary.
# ----------------------------------------------------------
if [[ ! -x /usr/local/bin/lakko-sfu ]]; then
  echo "/usr/local/bin/lakko-sfu missing — build from services/sfu and scp it here first" >&2
  exit 2
fi

# ----------------------------------------------------------
# 5. systemd unit.
# ----------------------------------------------------------
note "installing lakko-sfu.service"
install -m 644 "$REPO_ROOT/systemd/lakko-sfu.service" /etc/systemd/system/
systemctl daemon-reload

# ----------------------------------------------------------
# 6. TLS cert.
# ----------------------------------------------------------
if [[ ! -d /etc/letsencrypt/live/$SFU_DOMAIN ]]; then
  note "issuing Let's Encrypt cert for $SFU_DOMAIN"
  certbot certonly --standalone -d "$SFU_DOMAIN" \
    --non-interactive --agree-tos -m "ops@$SFU_DOMAIN"
fi

# ----------------------------------------------------------
# 7. nginx vhost.
# ----------------------------------------------------------
note "installing nginx vhost"
install -m 644 "$REPO_ROOT/nginx/sites-available/sfu.conf" /etc/nginx/sites-available/sfu.conf
ln -sf /etc/nginx/sites-available/sfu.conf /etc/nginx/sites-enabled/sfu.conf
nginx -t
systemctl reload nginx

# ----------------------------------------------------------
# 8. Firewall.
# ----------------------------------------------------------
note "configuring ufw"
ufw allow 22/tcp comment 'ssh'
ufw allow 443/tcp comment 'sfu signaling + metrics (nginx)'
ufw allow 7881/tcp comment 'livekit tcp fallback'
ufw allow 50000:60000/udp comment 'livekit media (RTP)'
ufw allow 5349/udp comment 'livekit ICE-lite'
ufw allow 9100/tcp comment 'node_exporter'  # mTLS-protected
ufw --force enable

# ----------------------------------------------------------
# 9. Bring it up.
# ----------------------------------------------------------
note "enabling + starting lakko-sfu"
systemctl enable --now lakko-sfu

note "done. tail logs with: journalctl -u lakko-sfu -f"
note "verify TLS:           curl -sI https://$SFU_DOMAIN/health"
