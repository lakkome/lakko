#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Keep logs that can name a client address for 7 days on the server
# (`just log-retention`).
# Idempotent; run as root. The privacy policy states the same period.
#
# - journald: 7 days, at most 1G, no copy forwarded to rsyslog
#   (infra/journald/lakko.conf, the drop-in setup-metrics.sh also installs);
# - nginx error log (critical errors only, they can carry the client
#   address): rotated daily, 7 kept;
# - what is older than 7 days is removed now, not at the next rotation.

set -euo pipefail

# The infra directory this script sits in.
INFRA_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DAYS=7
log() { echo "[log-retention] $*"; }
[[ $EUID -eq 0 ]] || { echo "ERR run as root" >&2; exit 1; }

install -d -m 755 /etc/systemd/journald.conf.d
install -m 644 "$INFRA_DIR/journald/lakko.conf" \
    /etc/systemd/journald.conf.d/lakko.conf
# The earlier name of the same drop-in.
rm -f /etc/systemd/journald.conf.d/lakko-retention.conf
if [[ "${LAKKO_LOG_RETENTION_NO_RESTART:-0}" != 1 ]]; then
    systemctl restart systemd-journald
    journalctl --vacuum-time="${DAYS}d" >/dev/null
fi
log "OK journald keeps ${DAYS} days"

NGINX_ROTATE=/etc/logrotate.d/nginx
if [[ -f "$NGINX_ROTATE" ]]; then
    sed -i -E "s/^([[:space:]]*)(weekly|monthly)[[:space:]]*$/\1daily/; s/^([[:space:]]*)rotate[[:space:]]+[0-9]+/\1rotate ${DAYS}/" "$NGINX_ROTATE"
    grep -qE "^[[:space:]]*maxage[[:space:]]" "$NGINX_ROTATE" \
        || sed -i -E "s/^([[:space:]]*)rotate[[:space:]]+${DAYS}/&\n\1maxage ${DAYS}/" "$NGINX_ROTATE"
    logrotate -d "$NGINX_ROTATE" >/dev/null 2>&1 || { echo "ERR $NGINX_ROTATE does not parse" >&2; exit 1; }
    find /var/log/nginx -type f -name '*.log.*' -mtime +"$DAYS" -delete
    log "OK nginx logs rotate daily, ${DAYS} kept"
else
    log "SKIP no nginx on this server"
fi
