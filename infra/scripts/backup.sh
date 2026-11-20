#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

# Lakko backup (ADR-0096 P2): database dumps daily, blobs weekly, to an
# rsync-over-ssh target (a Hetzner Storage Box), with a heartbeat the
# monitoring host alerts on.
#
#   lakko-backup db      dump every `lakko_*` database, compress, encrypt,
#                        keep LAKKO_BACKUP_KEEP_DAYS days locally, mirror
#                        the local set to the target (so the target holds the
#                        same days)
#   lakko-backup blobs   mirror LAKKO_BLOB_DIR to the target (the blobs are
#                        client-encrypted attachments; nothing is added)
#
# Installed as /usr/local/bin/lakko-backup, run by lakko-backup-db.timer and
# lakko-backup-blobs.timer as root (the dump uses MariaDB's unix-socket root
# login). Configuration comes from /etc/lakko/lakko.env:
#
#   LAKKO_BACKUP_TARGET          rsync destination, e.g.
#                                u123456@u123456.your-storagebox.de:lakko (required)
#   LAKKO_BACKUP_SSH_KEY         default /etc/lakko/backup_ssh_key
#   LAKKO_BACKUP_SSH_PORT        default 23 (the Storage Box's ssh port)
#   LAKKO_BACKUP_KNOWN_HOSTS     the target's host key, default
#                                /etc/lakko/backup_known_hosts (an unknown key
#                                refuses the connection)
#   LAKKO_BACKUP_DIR             local staging, default /var/backups/lakko
#   LAKKO_BACKUP_KEEP_DAYS       default 7 (chosen, not measured)
#   LAKKO_BACKUP_AGE_RECIPIENT   an age public key; the dumps are encrypted to
#                                it before they leave the host. Required unless
#                                LAKKO_BACKUP_ALLOW_PLAINTEXT=1 (a database
#                                dump is the at-rest dataset of ADR-0078; it
#                                does not leave the host in the clear by
#                                accident)
#   LAKKO_BLOB_DIR               default /var/lib/lakko/blobs
#   LAKKO_BACKUP_TEXTFILE_DIR    node_exporter's textfile collector directory,
#                                default /var/lib/node_exporter/textfile_collector
#
# Heartbeat: on success the script writes
#   lakko_job_last_success_timestamp_seconds{task="backup_db"|"backup_blobs"}
# to <textfile dir>/lakko_backup_<mode>.prom. A failed run writes nothing, so
# the alert LakkoBackupStale (infra/prometheus/rules/services.yml) fires once
# the last success is too old.

set -euo pipefail
umask 077

mode="${1:-}"
case "$mode" in
    db | blobs) ;;
    *)
        echo "usage: lakko-backup db|blobs" >&2
        exit 64
        ;;
esac

target="${LAKKO_BACKUP_TARGET:?LAKKO_BACKUP_TARGET is not set}"
ssh_key="${LAKKO_BACKUP_SSH_KEY:-/etc/lakko/backup_ssh_key}"
ssh_port="${LAKKO_BACKUP_SSH_PORT:-23}"
known_hosts="${LAKKO_BACKUP_KNOWN_HOSTS:-/etc/lakko/backup_known_hosts}"
stage="${LAKKO_BACKUP_DIR:-/var/backups/lakko}"
keep_days="${LAKKO_BACKUP_KEEP_DAYS:-7}"
textfile_dir="${LAKKO_BACKUP_TEXTFILE_DIR:-/var/lib/node_exporter/textfile_collector}"
blob_dir="${LAKKO_BLOB_DIR:-/var/lib/lakko/blobs}"
recipient="${LAKKO_BACKUP_AGE_RECIPIENT:-}"

rsync_ssh="ssh -i ${ssh_key} -p ${ssh_port} -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=${known_hosts}"

log() {
    echo "lakko-backup ${mode}: $*"
}

heartbeat() {
    local job="backup_${mode}"
    mkdir -p "$textfile_dir"
    local file="${textfile_dir}/lakko_backup_${mode}.prom"
    local tmp="${file}.$$"
    {
        echo "# HELP lakko_job_last_success_timestamp_seconds Unix time of a background job's last successful run"
        echo "# TYPE lakko_job_last_success_timestamp_seconds gauge"
        echo "lakko_job_last_success_timestamp_seconds{task=\"${job}\"} $(date -u +%s)"
    } >"$tmp"
    chmod 0644 "$tmp"
    mv "$tmp" "$file"
}

backup_db() {
    if [[ -z "$recipient" && "${LAKKO_BACKUP_ALLOW_PLAINTEXT:-0}" != "1" ]]; then
        log "refusing: LAKKO_BACKUP_AGE_RECIPIENT is not set (set LAKKO_BACKUP_ALLOW_PLAINTEXT=1 to ship unencrypted dumps)"
        exit 2
    fi
    local day dir
    day="$(date -u +%F)"
    dir="${stage}/db/${day}"
    mkdir -p "$dir"
    # A dump that fails half-way leaves a .tmp; remove it on any exit so it
    # is never mirrored.
    trap 'rm -f "${dir}"/*.tmp' EXIT

    local databases
    databases="$(mariadb -N -B -e "SELECT schema_name FROM information_schema.schemata WHERE schema_name LIKE 'lakko\\_%' ORDER BY schema_name")"
    if [[ -z "$databases" ]]; then
        log "no lakko_* database found"
        exit 1
    fi

    local db out count=0
    for db in $databases; do
        out="${dir}/${db}.sql.zst"
        if [[ -n "$recipient" ]]; then
            out="${out}.age"
            mariadb-dump --single-transaction --routines --triggers --databases "$db" |
                zstd -q -T0 |
                age -r "$recipient" >"${out}.tmp"
        else
            mariadb-dump --single-transaction --routines --triggers --databases "$db" |
                zstd -q -T0 >"${out}.tmp"
        fi
        mv "${out}.tmp" "$out"
        count=$((count + 1))
    done
    log "dumped ${count} databases"

    # Keep the newest days locally; the mirror below makes the target match.
    find "${stage}/db" -mindepth 1 -maxdepth 1 -type d -mtime "+${keep_days}" -exec rm -rf {} +

    rsync -a --delete --exclude '*.tmp' -e "$rsync_ssh" "${stage}/db/" "${target}/db/"
    log "mirrored to the target"
}

backup_blobs() {
    if [[ ! -d "$blob_dir" ]]; then
        log "no blob directory at ${blob_dir}"
        exit 1
    fi
    rsync -a --delete -e "$rsync_ssh" "${blob_dir}/" "${target}/blobs/"
    log "mirrored to the target"
}

"backup_${mode}"
heartbeat
