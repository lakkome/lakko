#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# End-to-end test of backup.sh and restore-check.sh on a laptop, in
# Docker (`just backup-test`). It:
#   1. starts a scratch MariaDB with two lakko_* databases holding rows,
#      and a blob directory with files;
#   2. runs backup.sh in a Debian container (restic + the MariaDB client,
#      the packages the VPS uses) into a local restic repository;
#   3. runs restore-check.sh against that repository;
#   4. loads the restored dumps into a second, empty MariaDB and compares
#      the row count of every table with the source;
#   5. removes everything it started.
# Nothing here touches the dev database or any server.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VOL="lakko-backup-test-$$"
NET="lakko-backup-test-$$"
SRC="lakko-backup-src-$$"
DST="lakko-backup-dst-$$"
IMAGE="mariadb:11"
TOOLS="debian:bookworm"

cleanup() {
    docker rm -f "$SRC" "$DST" >/dev/null 2>&1 || true
    docker network rm "$NET" >/dev/null 2>&1 || true
    docker volume rm "$VOL" >/dev/null 2>&1 || true
}
trap cleanup EXIT

log() { echo "[backup-test] $*"; }

docker network create "$NET" >/dev/null
for c in "$SRC" "$DST"; do
    docker run -d --name "$c" --network "$NET" -e MARIADB_ROOT_PASSWORD=testroot "$IMAGE" >/dev/null
done
for c in "$SRC" "$DST"; do
    until docker exec "$c" mariadb -uroot -ptestroot -e 'SELECT 1' >/dev/null 2>&1; do sleep 1; done
done
log "OK two scratch MariaDB servers up"

docker exec -i "$SRC" mariadb -uroot -ptestroot <<'SQL'
CREATE DATABASE lakko_auth;
USE lakko_auth;
CREATE TABLE lakko_auth.accounts (id BINARY(16) PRIMARY KEY, created_at DATETIME(3) NOT NULL DEFAULT NOW(3));
INSERT INTO lakko_auth.accounts (id) SELECT UNHEX(MD5(seq)) FROM seq_1_to_500;
CREATE DATABASE lakko_storage;
USE lakko_storage;
CREATE TABLE lakko_storage.blobs (id BINARY(32) PRIMARY KEY, size BIGINT NOT NULL, data BLOB);
INSERT INTO lakko_storage.blobs SELECT UNHEX(SHA2(seq, 256)), seq, RANDOM_BYTES(64) FROM seq_1_to_200;
CREATE DATABASE other_app;
CREATE DATABASE lakko_test_auth;
CREATE TABLE other_app.t (x INT);
SQL
log "OK source data written"

run_tools() {
    docker run --rm --network "$NET" \
        -v "$ROOT/infra/scripts:/scripts:ro" \
        -v "$VOL:/w" \
        -e RESTIC_REPOSITORY=/w/repo \
        -e RESTIC_PASSWORD_FILE=/w/password \
        -e LAKKO_BACKUP_CONFIG=/nonexistent \
        -e LAKKO_BACKUP_STAGE=/w/stage \
        -e LAKKO_BACKUP_BLOB_DIR=/w/lakko \
        -e LAKKO_BACKUP_EXTRA_PATHS="/w/etc/lakko /w/no-such-dir" \
        -e LAKKO_BACKUP_MYSQL_ARGS="-h$SRC -uroot -ptestroot" \
        -e LAKKO_BACKUP_HOST=backup-test \
        -e LAKKO_BACKUP_EXPECT_ETC=0 \
        -e LAKKO_BACKUP_TEXTFILE_DIR=/w/textfile \
        "$TOOLS" bash -euo pipefail -c "apt-get -qq update >/dev/null && \
            DEBIAN_FRONTEND=noninteractive apt-get -qq install -y restic mariadb-client >/dev/null 2>&1 && $1"
}

# Files live in a Docker volume: a bind mount of a macOS temp directory
# reads back as I/O errors inside the container.
run_tools '
    mkdir -p /w/lakko/blobs /w/etc/lakko /w/repo /w/stage
    for i in $(seq 1 20); do head -c 4096 /dev/urandom > /w/lakko/blobs/blob-$i; done
    echo LAKKO_FAKE=1 > /w/etc/lakko/lakko.env
    echo test-repository-password > /w/password
    restic init >/dev/null
    /scripts/backup.sh
    /scripts/backup.sh
    /scripts/restore-check.sh
    grep -q "^lakko_job_last_success_timestamp_seconds{task=\"backup_db\"} [0-9]" /w/textfile/lakko_backup_db.prom
    grep -q "^lakko_job_last_success_timestamp_seconds{task=\"backup_blobs\"} [0-9]" /w/textfile/lakko_backup_blobs.prom
    echo "[backup-test] OK heartbeat files written"
    restic snapshots --compact
    mkdir -p /w/restored
    restic restore latest --tag lakko --target /w/restored >/dev/null
    test -f /w/restored/w/etc/lakko/lakko.env
    test ! -e /w/stage/db/other_app.sql
    test ! -e /w/stage/db/lakko_test_auth.sql
    diff -r /w/lakko/blobs /w/restored/w/lakko/blobs
    echo "[backup-test] OK blob files identical"
    for f in /w/restored/w/stage/db/*.sql; do mariadb -h'"$DST"' -uroot -ptestroot < "$f"; done
    echo "[backup-test] OK dumps loaded into the empty server"
'

QUERY="SELECT CONCAT(table_schema,'.',table_name) FROM information_schema.tables WHERE table_schema LIKE 'lakko\\_%' ORDER BY 1"
TABLES=$(docker exec "$SRC" mariadb -uroot -ptestroot -N -B -e "$QUERY")
for t in $TABLES; do
    a=$(docker exec "$SRC" mariadb -uroot -ptestroot -N -B -e "SELECT COUNT(*), COALESCE(SUM(CRC32(CONCAT_WS('|',HEX(id)))),0) FROM $t")
    b=$(docker exec "$DST" mariadb -uroot -ptestroot -N -B -e "SELECT COUNT(*), COALESCE(SUM(CRC32(CONCAT_WS('|',HEX(id)))),0) FROM $t")
    [[ "$a" == "$b" ]] || { echo "ERR $t differs: source $a, restored $b" >&2; exit 1; }
    log "OK $t matches ($a)"
done
b=$(docker exec "$DST" mariadb -uroot -ptestroot -N -B -e "SELECT SUM(LENGTH(data)) FROM lakko_storage.blobs")
a=$(docker exec "$SRC" mariadb -uroot -ptestroot -N -B -e "SELECT SUM(LENGTH(data)) FROM lakko_storage.blobs")
[[ "$a" == "$b" ]] || { echo "ERR binary column differs" >&2; exit 1; }
log "OK binary column survives"
log "OK all checks passed"
