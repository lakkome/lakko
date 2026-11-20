#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Operations on the lakko.me server (the production layout: /etc/lakko,
# /var/lib/lakko, databases lakko_*). Copies infra/ to /root/lakko-infra on
# the host, then runs one operation there as root:
#
#   log-retention         keep logs 7 days (infra/scripts/log-retention.sh)
#   backup-setup          one-time backup set-up (needs the Storage Box ssh alias)
#   backup-now            run a backup now and show its log
#   backup-status         last success, timer, snapshots, sizes, disk
#   backup-restore-check  restore the newest snapshot to a scratch dir, verify
#
# lakko.me is behind Cloudflare, which does not carry ssh, so the host is
# reached by an address that resolves to it directly. Environment:
#   LAKKO_HOST_SSH      ssh target (default root@test.lakko.me: the test stack
#                       runs on the same machine)
#   LAKKO_HOST_SSH_KEY  identity file (default: LAKKO_TEST_SSH_KEY, if set)
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SSH_TARGET="${LAKKO_HOST_SSH:-root@test.lakko.me}"
SSH_KEY="${LAKKO_HOST_SSH_KEY:-${LAKKO_TEST_SSH_KEY:-}}"
SSH_OPTS=(-o BatchMode=yes -o StrictHostKeyChecking=accept-new)
if [[ -n $SSH_KEY ]]; then SSH_OPTS+=(-i "$SSH_KEY"); fi
REMOTE_INFRA=/root/lakko-infra

op="${1:-}"
case "$op" in
    log-retention|backup-setup|backup-now|backup-status|backup-restore-check) ;;
    *) echo "usage: $0 {log-retention|backup-setup|backup-now|backup-status|backup-restore-check}" >&2; exit 2 ;;
esac

rsync -a --delete --exclude='target/' -e "ssh ${SSH_OPTS[*]}" "$REPO/infra/" "$SSH_TARGET:$REMOTE_INFRA/"

case "$op" in
    log-retention)
        ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "bash $REMOTE_INFRA/scripts/log-retention.sh" ;;
    backup-setup)
        ssh -t "${SSH_OPTS[@]}" "$SSH_TARGET" "bash $REMOTE_INFRA/scripts/backup-setup.sh" ;;
    backup-now)
        ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "systemctl start lakko-backup && journalctl -u lakko-backup -n 40 --no-pager" ;;
    backup-status)
        ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "cat /var/backups/lakko/last-success 2>/dev/null || echo 'no successful backup yet'; \
            systemctl list-timers lakko-backup.timer --no-pager; \
            bash -c 'set -a; . /etc/lakko/backup.env; restic snapshots --compact && restic stats --mode raw-data'; \
            df -h / /var/lib/lakko; du -sh /var/lib/lakko /var/lib/mysql" ;;
    backup-restore-check)
        ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "bash $REMOTE_INFRA/scripts/restore-check.sh" ;;
esac
