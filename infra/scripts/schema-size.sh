#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

# On-disk size of each `lakko_*` MariaDB schema, for node_exporter's textfile
# collector (ADR-0096 section 4, P3). Installed as
# /usr/local/bin/lakko-schema-size, run hourly by lakko-schema-size.timer.
#
# Why a file walk and not mysqld_exporter's info_schema.tables collector:
# information_schema lists only the tables a user holds some privilege on, so
# the exporter would need a grant on every Lakko schema, and the cheap grant
# that makes the tables visible is SELECT, which also reads the data. The
# exporter account gets PROCESS and REPLICATION CLIENT only; this root timer
# reads the directory sizes instead. Schema names are a fixed set, never an
# identifier of anyone.
#
# Writes <textfile dir>/lakko_mariadb_schema.prom:
#   lakko_mariadb_schema_bytes{schema="lakko_auth"} 123456
#
#   LAKKO_MARIADB_DATADIR      default /var/lib/mysql
#   LAKKO_BACKUP_TEXTFILE_DIR  default /var/lib/node_exporter/textfile_collector

set -euo pipefail
umask 022

datadir="${LAKKO_MARIADB_DATADIR:-/var/lib/mysql}"
textfile_dir="${LAKKO_BACKUP_TEXTFILE_DIR:-/var/lib/node_exporter/textfile_collector}"
file="${textfile_dir}/lakko_mariadb_schema.prom"
tmp="${file}.$$"
trap 'rm -f "$tmp"' EXIT

mkdir -p "$textfile_dir"
{
    echo "# HELP lakko_mariadb_schema_bytes On-disk size of a lakko_* MariaDB schema directory"
    echo "# TYPE lakko_mariadb_schema_bytes gauge"
    shopt -s nullglob
    for dir in "${datadir}"/lakko_*/; do
        schema="$(basename "$dir")"
        # A schema name is [a-z0-9_]; anything else is skipped rather than
        # written into a label.
        [[ "$schema" =~ ^[a-z0-9_]+$ ]] || continue
        bytes="$(du -sb "$dir" | cut -f1)"
        echo "lakko_mariadb_schema_bytes{schema=\"${schema}\"} ${bytes}"
    done
} >"$tmp"
chmod 0644 "$tmp"
mv "$tmp" "$file"
trap - EXIT
