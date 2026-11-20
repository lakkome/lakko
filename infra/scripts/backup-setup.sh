#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# One-time set-up of the nightly backup on the server, as root
# (`just backup-setup`, through infra/scripts/host-ops.sh). Idempotent: a step
# already done is skipped. Refuses until Lakko is deployed on the host
# (/etc/lakko/lakko.env present).
#
# Before running it, the Storage Box must be reachable as root over sftp
# under the alias in /etc/lakko/backup.env (default `storagebox`), see
# infra/README.md "Backup / restore". This script never prints the
# repository password.

set -euo pipefail

# The infra directory this script sits in (copied to the host).
INFRA_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TEMPLATE="${LAKKO_BACKUP_TEMPLATE:-$INFRA_DIR/etc-lakko/backup.env.example}"
CONFIG=/etc/lakko/backup.env
log() { echo "[backup-setup] $*"; }

[[ $EUID -eq 0 ]] || { echo "ERR run as root" >&2; exit 1; }
[[ -f /etc/lakko/lakko.env ]] || { echo "ERR /etc/lakko/lakko.env missing: deploy Lakko first" >&2; exit 1; }

if ! command -v restic >/dev/null; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq restic
    log "OK restic installed"
fi
command -v mariadb-dump >/dev/null || command -v mysqldump >/dev/null \
    || { echo "ERR mariadb-dump not found" >&2; exit 1; }

if [[ ! -f "$CONFIG" ]]; then
    install -d -m 755 /etc/lakko
    install -m 600 -o root -g root "$TEMPLATE" "$CONFIG"
    log "OK $CONFIG created from the template; check RESTIC_REPOSITORY"
fi
# shellcheck disable=SC1090
set -a; source "$CONFIG"; set +a

if [[ ! -s "$RESTIC_PASSWORD_FILE" ]]; then
    umask 077
    head -c 32 /dev/urandom | base64 | tr -d '\n' > "$RESTIC_PASSWORD_FILE"
    echo >> "$RESTIC_PASSWORD_FILE"
    log "OK repository password written to $RESTIC_PASSWORD_FILE"
    log "ACTION copy that password off the server now (password manager):"
    log "ACTION   sudo cat $RESTIC_PASSWORD_FILE"
    log "ACTION without it no backup can be read, by anyone"
fi

if restic cat config >/dev/null 2>&1; then
    log "OK repository already initialised"
else
    restic init
    log "OK repository initialised at $RESTIC_REPOSITORY"
fi

# The unit runs backup.sh from this infra directory, wherever it was copied.
sed "s#^ExecStart=.*#ExecStart=$INFRA_DIR/scripts/backup.sh#" \
    "$INFRA_DIR/systemd/lakko-backup.service" > /etc/systemd/system/lakko-backup.service
chmod 644 /etc/systemd/system/lakko-backup.service
install -m 644 "$INFRA_DIR/systemd/lakko-backup.timer" /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now lakko-backup.timer
log "OK lakko-backup.timer enabled: $(systemctl show -p NextElapseUSecRealtime --value lakko-backup.timer)"
log "NEXT run the first backup now: sudo systemctl start lakko-backup && journalctl -u lakko-backup -n 50"
