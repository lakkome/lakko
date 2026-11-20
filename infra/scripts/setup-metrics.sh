#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Metrics side of a Lakko host (ADR-0096 P3 for the main VPS, P4 for the
# test stack). Idempotent; safe to re-run. Run as root on the host:
#
#   sudo MONITORING_IP=203.0.113.7 bash infra/scripts/setup-metrics.sh
#   sudo STACK=test MONITORING_IP=203.0.113.7 bash infra/scripts/setup-metrics.sh
#
# `just setup-metrics` runs it on the main VPS from its checkout of main;
# `just setup-metrics-test` copies infra/ to the test host (which has no
# checkout) and runs it with STACK=test.
#
# STACK=test differs from prod in: the env file (/etc/lakko-test/lakko.env),
# the server name (test.lakko.me, whose certificate the test stack already
# has), and the backup timer check (the test stack's data is disposable and
# not backed up).
#
# What it does:
#   1. journald retention (infra/journald/lakko.conf).
#   2. Host exporters, all on 127.0.0.1: node_exporter (with the textfile
#      collector that carries the backup heartbeat and the schema sizes, and
#      the systemd collector limited to Lakko and coturn units),
#      mysqld_exporter (an account with PROCESS and REPLICATION CLIENT only),
#      nginx-prometheus-exporter (reading stub_status).
#   3. The hourly schema-size timer, and a check that the nightly backup
#      timer (installed by `just backup-setup`) is enabled.
#   4. Every LAKKO_<SVC>_METRICS_BIND in /etc/lakko/lakko.env, added when
#      missing (the services pick them up at their next restart).
#   5. A Let's Encrypt certificate for METRICS_DOMAIN and the metrics server
#      block (infra/nginx/sites-available/metrics.conf) on port 9643, which
#      requires a client certificate from the monitoring CA.
#   6. The firewall: 9643/tcp from MONITORING_IP only.
#
# Before the first run: DNS for METRICS_DOMAIN points at this host, and the
# monitoring CA certificate is at /etc/lakko/mtls/monitoring-ca.crt (made by
# infra/monitoring/make-mtls.sh on the operator's machine).
#
#   MONITORING_IP     the monitoring VPS's public address(es), comma-separated
#                     (required)
#   STACK             prod (default) or test
#   METRICS_DOMAIN    default metrics.lakko.me (prod), test.lakko.me (test)
#   CERTBOT_EMAIL     default ops@lakko.me

set -euo pipefail

MONITORING_IP="${MONITORING_IP:?MONITORING_IP is not set (the monitoring VPS address)}"
STACK="${STACK:-prod}"
case "$STACK" in
    prod)
        ENV_FILE=/etc/lakko/lakko.env
        METRICS_DOMAIN="${METRICS_DOMAIN:-metrics.lakko.me}"
        ;;
    test)
        ENV_FILE=/etc/lakko-test/lakko.env
        METRICS_DOMAIN="${METRICS_DOMAIN:-test.lakko.me}"
        ;;
    *)
        echo "STACK must be prod or test" >&2
        exit 64
        ;;
esac
CERTBOT_EMAIL="${CERTBOT_EMAIL:-ops@lakko.me}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEXTFILE_DIR=/var/lib/node_exporter/textfile_collector

note() { printf '\n==> %s\n' "$*"; }
warn() { printf '\nWARN: %s\n' "$*"; }

if [[ $EUID -ne 0 ]]; then
    echo "must run as root" >&2
    exit 1
fi

# ----------------------------------------------------------------------
# 1. journald
# ----------------------------------------------------------------------
note "journald retention"
install -d -m 755 /etc/systemd/journald.conf.d
install -m 644 "$REPO_ROOT/journald/lakko.conf" /etc/systemd/journald.conf.d/lakko.conf
install -d -m 2755 -g systemd-journal /var/log/journal
systemctl restart systemd-journald

# ----------------------------------------------------------------------
# 2. Exporters
# ----------------------------------------------------------------------
note "installing exporters"
apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    prometheus-node-exporter prometheus-mysqld-exporter prometheus-nginx-exporter

install -d -m 755 "$TEXTFILE_DIR"

cat >/etc/default/prometheus-node-exporter <<EOF
# Written by infra/scripts/setup-metrics.sh (ADR-0096 P3).
ARGS="--web.listen-address=127.0.0.1:9100 --collector.textfile.directory=${TEXTFILE_DIR} --collector.systemd --collector.systemd.unit-include=(lakko.*|coturn.*|mariadb|nginx)\\.service"
EOF

# The exporter account: server status and process list, no data.
MYSQLD_CNF=/etc/prometheus/mysqld-exporter.cnf
install -d -m 755 /etc/prometheus
if [[ ! -f "$MYSQLD_CNF" ]]; then
    note "creating the MariaDB exporter account"
    pw="$(openssl rand -hex 24)"
    mariadb <<SQL
CREATE USER IF NOT EXISTS 'lakko_exporter'@'127.0.0.1' IDENTIFIED BY '${pw}' WITH MAX_USER_CONNECTIONS 3;
ALTER USER 'lakko_exporter'@'127.0.0.1' IDENTIFIED BY '${pw}';
GRANT PROCESS, REPLICATION CLIENT ON *.* TO 'lakko_exporter'@'127.0.0.1';
FLUSH PRIVILEGES;
SQL
    umask 027
    cat >"$MYSQLD_CNF" <<EOF
[client]
user=lakko_exporter
password=${pw}
host=127.0.0.1
port=3306
EOF
    umask 022
    chown root:prometheus "$MYSQLD_CNF"
    chmod 0640 "$MYSQLD_CNF"
    unset pw
fi

cat >/etc/default/prometheus-mysqld-exporter <<'EOF'
# Written by infra/scripts/setup-metrics.sh (ADR-0096 P3).
ARGS="--web.listen-address=127.0.0.1:9104 --config.my-cnf=/etc/prometheus/mysqld-exporter.cnf"
EOF

cat >/etc/default/prometheus-nginx-exporter <<'EOF'
# Written by infra/scripts/setup-metrics.sh (ADR-0096 P3).
ARGS="--nginx.scrape-uri=http://127.0.0.1:8090/stub_status --web.listen-address=127.0.0.1:9113"
EOF

for unit in prometheus-node-exporter prometheus-mysqld-exporter prometheus-nginx-exporter; do
    systemctl enable "$unit" >/dev/null
    systemctl restart "$unit"
done

# ----------------------------------------------------------------------
# 3. Timers
# ----------------------------------------------------------------------
note "schema-size timer"
install -m 755 "$REPO_ROOT/scripts/schema-size.sh" /usr/local/bin/lakko-schema-size
for unit in lakko-schema-size.service lakko-schema-size.timer; do
    install -m 644 "$REPO_ROOT/systemd/$unit" "/etc/systemd/system/$unit"
done
systemctl daemon-reload
systemctl enable --now lakko-schema-size.timer >/dev/null

if [[ "$STACK" == prod ]]; then
    # The backup itself (restic, lakko-backup.timer) is set up once by
    # `just backup-setup` (infra/scripts/backup-setup.sh); here only the
    # directory its heartbeat goes to is made (step 2) and its state reported.
    note "backup timer"
    if systemctl is-enabled --quiet lakko-backup.timer 2>/dev/null; then
        note "lakko-backup.timer enabled"
    else
        warn "lakko-backup.timer is not enabled: run just backup-setup (LakkoBackupNeverRan will fire)"
    fi
fi

# ----------------------------------------------------------------------
# 4. Service metrics listeners
# ----------------------------------------------------------------------
note "service metrics listeners in $ENV_FILE"
if [[ -f "$ENV_FILE" ]]; then
    added=0
    while read -r var bind; do
        if ! grep -q "^${var}=" "$ENV_FILE"; then
            printf '%s=%s\n' "$var" "$bind" >>"$ENV_FILE"
            added=$((added + 1))
        fi
    done <<'EOF'
LAKKO_AUTH_METRICS_BIND 127.0.0.1:9651
LAKKO_DS_METRICS_BIND 127.0.0.1:9652
LAKKO_DISCOVERY_METRICS_BIND 127.0.0.1:9653
LAKKO_STORAGE_METRICS_BIND 127.0.0.1:9654
LAKKO_PUSH_GW_METRICS_BIND 127.0.0.1:9655
LAKKO_CHANNELS_METRICS_BIND 127.0.0.1:9656
LAKKO_TRANSPARENCY_METRICS_BIND 127.0.0.1:9657
LAKKO_WITNESS_METRICS_BIND 127.0.0.1:9658
LAKKO_GIF_PROXY_METRICS_BIND 127.0.0.1:9659
LAKKO_OBFS_PROXY_METRICS_BIND 127.0.0.1:9660
LAKKO_SFU_METRICS_BIND 127.0.0.1:9641
EOF
    if [[ $added -gt 0 ]]; then
        if [[ "$STACK" == prod ]]; then target=lakko.target; else target=lakko-test.target; fi
        warn "added ${added} metrics listener(s); they start at the next 'systemctl restart ${target}'"
    fi
else
    warn "$ENV_FILE missing: no service will expose metrics"
fi

# ----------------------------------------------------------------------
# 5. Certificate and server block
# ----------------------------------------------------------------------
if [[ ! -f /etc/lakko/mtls/monitoring-ca.crt ]]; then
    echo "ERROR: /etc/lakko/mtls/monitoring-ca.crt missing; copy it from the operator's machine (infra/monitoring/make-mtls.sh) first" >&2
    exit 1
fi
if [[ ! -d "/etc/letsencrypt/live/${METRICS_DOMAIN}" ]]; then
    note "issuing a certificate for ${METRICS_DOMAIN}"
    certbot certonly --nginx -d "$METRICS_DOMAIN" --non-interactive --agree-tos -m "$CERTBOT_EMAIL"
fi
note "metrics server block"
# The server name and the certificate path follow METRICS_DOMAIN. The
# directory name of an existing certificate is used as it is.
sed "s/metrics\\.lakko\\.me/${METRICS_DOMAIN}/g" "$REPO_ROOT/nginx/sites-available/metrics.conf" \
    >/etc/nginx/sites-available/lakko-metrics.conf
ln -sf /etc/nginx/sites-available/lakko-metrics.conf /etc/nginx/sites-enabled/lakko-metrics.conf
nginx -t
systemctl reload nginx

# ----------------------------------------------------------------------
# 6. Firewall
# ----------------------------------------------------------------------
if command -v ufw >/dev/null 2>&1; then
    note "firewall: 9643/tcp from ${MONITORING_IP}"
    IFS=',' read -ra addrs <<<"$MONITORING_IP"
    for addr in "${addrs[@]}"; do
        ufw allow from "$addr" to any port 9643 proto tcp comment 'lakko metrics (mTLS)' >/dev/null
    done
else
    warn "ufw not found: allow 9643/tcp from ${MONITORING_IP} only, by hand"
fi

note "done. From the monitoring VPS:"
echo "  curl --cert /etc/prometheus/mtls/monitoring.crt --key /etc/prometheus/mtls/monitoring.key https://${METRICS_DOMAIN}:9643/node/metrics | head"
