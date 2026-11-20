#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Nightly backup of the Lakko VPS into an encrypted restic repository
# (a Hetzner Storage Box over sftp in production). Run by
# lakko-backup.service as root; see infra/README.md "Backup / restore".
#
# What it saves, in this order:
#   1. every MariaDB database named lakko_* (one consistent dump per
#      database, --single-transaction), written to $STAGE;
#   2. the blob directory, AFTER the dumps: the storage index in the dump
#      can then only name blobs that are also in the snapshot, never the
#      other way round;
#   3. /etc/lakko (secrets, signing keys on disk), /etc/letsencrypt, and
#      the nginx site files.
# Then it keeps 7 daily, 4 weekly and 3 monthly snapshots and prunes the
# rest, so a deleted account leaves the backups within about three months.
#
# Configuration: /etc/lakko/backup.env (see infra/etc-lakko/backup.env.example).
# Every path below can be overridden there, which is also how the local
# test (infra/scripts/backup-test.sh) runs it.
#
# The repository password is the only key to every backup. It is NOT in
# the backups. Keep a copy off the server (a password manager); without
# it the backups cannot be read by anyone, including us.

set -euo pipefail

CONFIG="${LAKKO_BACKUP_CONFIG:-/etc/lakko/backup.env}"
if [[ -f "$CONFIG" ]]; then
    # shellcheck disable=SC1090
    set -a; source "$CONFIG"; set +a
fi

: "${RESTIC_REPOSITORY:?RESTIC_REPOSITORY must be set (in $CONFIG)}"
if [[ -z "${RESTIC_PASSWORD_FILE:-}" && -z "${RESTIC_PASSWORD:-}" ]]; then
    echo "ERR RESTIC_PASSWORD_FILE must be set (in $CONFIG)" >&2
    exit 2
fi

STAGE="${LAKKO_BACKUP_STAGE:-/var/backups/lakko}"
BLOB_DIR="${LAKKO_BACKUP_BLOB_DIR:-/var/lib/lakko}"
EXTRA_PATHS="${LAKKO_BACKUP_EXTRA_PATHS:-/etc/lakko /etc/letsencrypt /etc/nginx/sites-available}"
# Extra arguments for the MariaDB client tools, e.g.
# "--defaults-extra-file=/etc/lakko/backup-my.cnf". Empty on the VPS:
# root reaches MariaDB over the unix socket.
MYSQL_ARGS="${LAKKO_BACKUP_MYSQL_ARGS:-}"
DB_PATTERN="${LAKKO_BACKUP_DB_PATTERN:-lakko_%}"
# Databases left out even when they match the pattern: the test stack
# (lakko_test_*) shares the host with lakko.me and its data is not kept.
DB_EXCLUDE="${LAKKO_BACKUP_DB_EXCLUDE:-lakko\\_test\\_%}"
KEEP="${LAKKO_BACKUP_KEEP:---keep-daily 7 --keep-weekly 4 --keep-monthly 3}"
STATUS_FILE="${LAKKO_BACKUP_STATUS_FILE:-$STAGE/last-success}"

DUMP=$(command -v mariadb-dump || command -v mysqldump)
CLIENT=$(command -v mariadb || command -v mysql)

log() { echo "[backup] $*"; }

umask 077
mkdir -p "$STAGE/db"
rm -f "$STAGE"/db/*.sql

# shellcheck disable=SC2086
mapfile -t DATABASES < <("$CLIENT" $MYSQL_ARGS -N -B \
    -e "SELECT schema_name FROM information_schema.schemata WHERE schema_name LIKE '$DB_PATTERN' AND schema_name NOT LIKE '$DB_EXCLUDE' ORDER BY schema_name")
if [[ ${#DATABASES[@]} -eq 0 ]]; then
    echo "ERR no database matches $DB_PATTERN" >&2
    exit 3
fi

for db in "${DATABASES[@]}"; do
    # shellcheck disable=SC2086
    "$DUMP" $MYSQL_ARGS --single-transaction --quick --routines --triggers \
        --hex-blob --databases "$db" > "$STAGE/db/$db.sql.tmp"
    mv "$STAGE/db/$db.sql.tmp" "$STAGE/db/$db.sql"
    log "OK dumped $db"
done

PATHS=("$STAGE/db")
for p in $BLOB_DIR $EXTRA_PATHS; do
    if [[ -e "$p" ]]; then
        PATHS+=("$p")
    else
        log "WARN skipped missing path $p"
    fi
done

restic backup --tag lakko --host "${LAKKO_BACKUP_HOST:-$(hostname)}" "${PATHS[@]}"
log "OK snapshot written"

# shellcheck disable=SC2086
restic forget --tag lakko --prune $KEEP
log "OK old snapshots pruned"

# A light integrity check every night; a full data read is the monthly
# restore test (infra/README.md).
restic check
log "OK repository check"

date -u +%Y-%m-%dT%H:%M:%SZ > "$STATUS_FILE"
log "OK done"
