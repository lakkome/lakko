#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Restore the newest backup into a scratch directory and check that it is
# whole, without touching the running services. Run monthly as root on
# the VPS (`just backup-restore-check`). A full disaster restore is the
# runbook in infra/README.md "Backup / restore"; this script proves the
# snapshot it would start from can be read.
#
# Checks: the snapshot restores; every database dump finished
# ("-- Dump completed" at its end); the blob directory and /etc/lakko came
# back; and a full read of one tenth of the repository data verifies.

set -euo pipefail

CONFIG="${LAKKO_BACKUP_CONFIG:-/etc/lakko/backup.env}"
if [[ -f "$CONFIG" ]]; then
    # shellcheck disable=SC1090
    set -a; source "$CONFIG"; set +a
fi
: "${RESTIC_REPOSITORY:?RESTIC_REPOSITORY must be set (in $CONFIG)}"

STAGE="${LAKKO_BACKUP_STAGE:-/var/backups/lakko}"
BLOB_DIR="${LAKKO_BACKUP_BLOB_DIR:-/var/lib/lakko}"
SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/lakko-restore.XXXXXX")
trap 'rm -rf "$SCRATCH"' EXIT

log() { echo "[restore-check] $*"; }
fail() { echo "ERR $*" >&2; exit 1; }

restic restore latest --tag lakko --target "$SCRATCH"
log "OK restored the newest snapshot"

DUMPS=("$SCRATCH$STAGE"/db/*.sql)
[[ -e "${DUMPS[0]}" ]] || fail "no database dump in the snapshot"
for dump in "${DUMPS[@]}"; do
    tail -n 1 "$dump" | grep -q -- "-- Dump completed" || fail "unfinished dump $(basename "$dump")"
    log "OK $(basename "$dump") complete ($(grep -c '^INSERT INTO' "$dump" || true) insert statements)"
done

[[ -d "$SCRATCH$BLOB_DIR" ]] || fail "blob directory missing from the snapshot"
log "OK $(find "$SCRATCH$BLOB_DIR" -type f | wc -l | tr -d ' ') files under $BLOB_DIR"

if [[ -n "${LAKKO_BACKUP_EXPECT_ETC:-1}" && "${LAKKO_BACKUP_EXPECT_ETC:-1}" != 0 ]]; then
    [[ -f "$SCRATCH/etc/lakko/lakko.env" ]] || fail "lakko.env missing from the snapshot"
    log "OK the service env file is present"
fi

restic check --read-data-subset=1/10
log "OK one tenth of the repository data read back and verified"
log "OK done"
