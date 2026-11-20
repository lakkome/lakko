#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# The monitoring host (ADR-0096 section 7, P3; P5 canary). Idempotent; safe
# to re-run after editing any file under infra/prometheus, infra/monitoring
# or infra/grafana. Run as root on the monitoring host (`just
# deploy-monitoring` copies infra/ there and runs it):
#
#   sudo bash /root/lakko-infra/monitoring/setup-monitoring.sh
#
# Settings (environment):
#   LAKKO_MONITORING_STACKS     which stacks to scrape and probe, of "prod"
#                               and "test" (default "test": prod is not
#                               deployed). Only their target files under
#                               infra/prometheus/targets/ are installed.
#   LAKKO_MONITORING_COLOCATED  1 when this host is itself a Lakko host (today
#                               the test stack's VPS, the only server): its
#                               node_exporter belongs to setup-metrics.sh and
#                               is scraped by the `host` job, so this script
#                               leaves it alone and adds no monitoring_host
#                               target. Default 0.
#
# Prometheus, Alertmanager, blackbox_exporter and Grafana run as Docker
# containers of pinned versions, one systemd unit each (lakko-mon-<name>),
# all on the host network and bound to 127.0.0.1. These are the images `just
# monitoring-check` validates the configuration with, and they run the same
# on Ubuntu 22.04 and 24.04 (Ubuntu 22.04's Alertmanager 0.23 cannot send to
# Telegram).
#
#   Prometheus         127.0.0.1:9090   30 days of every series
#   Alertmanager       127.0.0.1:9093   Telegram (+ e-mail if configured)
#   blackbox_exporter  127.0.0.1:9115   public probes
#   Grafana            127.0.0.1:3000   reached over an SSH port forward
#   node_exporter      127.0.0.1:9100   (dedicated host only, apt package)
#   lakko-canary@prod  127.0.0.1:9661   synthetic canary (P5), one per stack
#   lakko-canary@test  127.0.0.1:9662
#
# The client certificate Prometheus presents to the Lakko hosts is made here
# on the first run, with its own CA (/etc/lakko-monitoring/mtls). The CA
# certificate (not its key) goes to /etc/lakko/mtls/monitoring-ca.crt on the
# Lakko hosts: `just push-monitoring-ca-test` (and `push-monitoring-ca` once
# prod exists).
#
# Fill /etc/lakko-monitoring/secrets.env from
# infra/monitoring/secrets.env.example before or after the first run;
# without it Alertmanager is not started and nothing is sent.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONF=/etc/lakko-monitoring
MTLS="$CONF/mtls"
DATA=/var/lib/lakko-monitoring
STACKS="${LAKKO_MONITORING_STACKS:-test}"
COLOCATED="${LAKKO_MONITORING_COLOCATED:-0}"

# Pinned images: the versions `just monitoring-check` runs.
IMG_PROMETHEUS=prom/prometheus:v2.53.2
IMG_ALERTMANAGER=prom/alertmanager:v0.26.0
IMG_BLACKBOX=prom/blackbox-exporter:v0.24.0
IMG_GRAFANA=grafana/grafana:11.2.0
# The users the images run as.
UID_PROM=65534
UID_GRAFANA=472

note() { printf '\n==> %s\n' "$*"; }
warn() { printf '\nWARN: %s\n' "$*"; }

if [[ $EUID -ne 0 ]]; then
    echo "must run as root" >&2
    exit 1
fi
for stack in $STACKS; do
    case "$stack" in
        prod | test) ;;
        *)
            echo "LAKKO_MONITORING_STACKS: unknown stack '$stack' (prod, test)" >&2
            exit 64
            ;;
    esac
done

install -d -m 711 "$CONF"
install -d -m 755 "$DATA"

# ----------------------------------------------------------------------
# 1. Packages and images
# ----------------------------------------------------------------------
note "packages"
apt-get update -qq
pkgs=(gettext-base openssl ufw)
command -v docker >/dev/null 2>&1 || pkgs+=(docker.io)
[[ "$COLOCATED" == 1 ]] || pkgs+=(prometheus-node-exporter)
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${pkgs[@]}"
systemctl enable --now docker >/dev/null
for img in "$IMG_PROMETHEUS" "$IMG_ALERTMANAGER" "$IMG_BLACKBOX" "$IMG_GRAFANA"; do
    docker pull -q "$img" >/dev/null
done

# ----------------------------------------------------------------------
# 2. journald (container logs go to the journal too)
# ----------------------------------------------------------------------
install -d -m 755 /etc/systemd/journald.conf.d
install -m 644 "$REPO_ROOT/journald/lakko.conf" /etc/systemd/journald.conf.d/lakko.conf
install -d -m 2755 -g systemd-journal /var/log/journal
systemctl restart systemd-journald

# ----------------------------------------------------------------------
# 3. mTLS: the monitoring CA and Prometheus's client certificate
# ----------------------------------------------------------------------
PROM_CONF="$CONF/prometheus"
install -d -m 755 "$PROM_CONF"
install -d -m 700 "$MTLS"
if [[ ! -f "$MTLS/monitoring-ca.key" ]]; then
    note "making the monitoring CA"
    openssl ecparam -name prime256v1 -genkey -noout -out "$MTLS/monitoring-ca.key"
    chmod 600 "$MTLS/monitoring-ca.key"
    openssl req -x509 -new -key "$MTLS/monitoring-ca.key" -sha256 -days 3650 \
        -subj "/CN=Lakko monitoring CA" \
        -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
        -addext "keyUsage=critical,keyCertSign,cRLSign" \
        -out "$MTLS/monitoring-ca.crt"
fi
install -d -m 750 -o "$UID_PROM" -g "$UID_PROM" "$PROM_CONF/mtls"
if [[ ! -f "$PROM_CONF/mtls/monitoring.key" ]]; then
    note "making the Prometheus client certificate"
    openssl ecparam -name prime256v1 -genkey -noout -out "$PROM_CONF/mtls/monitoring.key"
    openssl req -new -key "$PROM_CONF/mtls/monitoring.key" \
        -subj "/CN=lakko-monitoring" -out "$MTLS/monitoring.csr"
    cat >"$MTLS/client.ext" <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=clientAuth
EOF
    openssl x509 -req -in "$MTLS/monitoring.csr" \
        -CA "$MTLS/monitoring-ca.crt" -CAkey "$MTLS/monitoring-ca.key" -CAcreateserial \
        -days 825 -sha256 -extfile "$MTLS/client.ext" \
        -out "$PROM_CONF/mtls/monitoring.crt"
    rm -f "$MTLS/monitoring.csr"
fi
chown "$UID_PROM:$UID_PROM" "$PROM_CONF/mtls/monitoring.key" "$PROM_CONF/mtls/monitoring.crt"
chmod 400 "$PROM_CONF/mtls/monitoring.key"
chmod 444 "$PROM_CONF/mtls/monitoring.crt"
if ! openssl x509 -checkend $((30 * 86400)) -noout -in "$PROM_CONF/mtls/monitoring.crt" >/dev/null; then
    warn "the Prometheus client certificate expires within 30 days: delete $PROM_CONF/mtls/monitoring.key and re-run"
fi

# ----------------------------------------------------------------------
# 4. Prometheus configuration and its targets
# ----------------------------------------------------------------------
note "Prometheus configuration (stacks: $STACKS)"
install -m 644 "$REPO_ROOT/prometheus/prometheus.yml" "$PROM_CONF/prometheus.yml"
rm -rf "$PROM_CONF/rules" "$PROM_CONF/targets"
install -d -m 755 "$PROM_CONF/rules" "$PROM_CONF/targets"
install -m 644 "$REPO_ROOT"/prometheus/rules/*.yml "$PROM_CONF/rules/"
for jobdir in "$REPO_ROOT"/prometheus/targets/*/; do
    job="$(basename "$jobdir")"
    install -d -m 755 "$PROM_CONF/targets/$job"
    for stack in $STACKS; do
        if [[ -f "$jobdir/$stack.yml" ]]; then
            install -m 644 "$jobdir/$stack.yml" "$PROM_CONF/targets/$job/$stack.yml"
        fi
    done
done
if [[ "$COLOCATED" != 1 ]]; then
    install -m 644 "$REPO_ROOT/prometheus/targets/monitoring_host/self.yml" \
        "$PROM_CONF/targets/monitoring_host/self.yml"
fi
install -m 644 "$REPO_ROOT/monitoring/blackbox.yml" "$CONF/blackbox.yml"
docker run --rm --entrypoint promtool \
    -v "$PROM_CONF:/etc/prometheus:ro" -v /etc/ssl/certs:/etc/ssl/certs:ro \
    "$IMG_PROMETHEUS" check config /etc/prometheus/prometheus.yml

# ----------------------------------------------------------------------
# 5. Alertmanager configuration
# ----------------------------------------------------------------------
AM_CONF="$CONF/alertmanager"
install -d -m 750 -o "$UID_PROM" -g "$UID_PROM" "$AM_CONF"
alertmanager_ready=0
if [[ -f "$CONF/secrets.env" ]]; then
    note "Alertmanager configuration"
    set -a
    # shellcheck disable=SC1091
    source "$CONF/secrets.env"
    set +a
    tmpl="$REPO_ROOT/monitoring/alertmanager.yml.tmpl"
    if [[ -z "${LAKKO_ALERT_SMTP_HOST:-}" ]]; then
        note "no LAKKO_ALERT_SMTP_HOST: alerts go to Telegram only"
        sed '/# BEGIN email/,/# END email/d' "$tmpl" >"$AM_CONF/alertmanager.yml.tmpl"
        tmpl="$AM_CONF/alertmanager.yml.tmpl"
    fi
    # envsubst takes the variable list literally.
    # shellcheck disable=SC2016
    vars='${LAKKO_ALERT_TELEGRAM_BOT_TOKEN} ${LAKKO_ALERT_TELEGRAM_CHAT_ID} ${LAKKO_ALERT_SMTP_HOST} ${LAKKO_ALERT_SMTP_USER} ${LAKKO_ALERT_SMTP_PASSWORD} ${LAKKO_ALERT_EMAIL_FROM} ${LAKKO_ALERT_EMAIL_TO}'
    umask 077
    envsubst "$vars" <"$tmpl" >"$AM_CONF/alertmanager.yml"
    umask 022
    rm -f "$AM_CONF/alertmanager.yml.tmpl"
    chown "$UID_PROM:$UID_PROM" "$AM_CONF/alertmanager.yml"
    chmod 400 "$AM_CONF/alertmanager.yml"
    docker run --rm --entrypoint amtool -v "$AM_CONF:/etc/alertmanager:ro" \
        "$IMG_ALERTMANAGER" check-config /etc/alertmanager/alertmanager.yml >/dev/null
    alertmanager_ready=1
else
    warn "$CONF/secrets.env missing: Alertmanager is not started and no alert is sent (see infra/monitoring/secrets.env.example)"
fi

# ----------------------------------------------------------------------
# 6. Grafana configuration
# ----------------------------------------------------------------------
GF_CONF="$CONF/grafana"
install -d -m 755 "$GF_CONF" "$GF_CONF/provisioning" \
    "$GF_CONF/provisioning/datasources" "$GF_CONF/provisioning/dashboards" \
    "$GF_CONF/provisioning/plugins" "$GF_CONF/provisioning/alerting" "$GF_CONF/dashboards"
install -m 644 "$REPO_ROOT/grafana/provisioning/datasources/prometheus.yml" \
    "$GF_CONF/provisioning/datasources/lakko-prometheus.yml"
install -m 644 "$REPO_ROOT/grafana/provisioning/dashboards/lakko.yml" \
    "$GF_CONF/provisioning/dashboards/lakko.yml"
rm -f "$GF_CONF"/dashboards/*.json
install -m 644 "$REPO_ROOT"/grafana/dashboards/*.json "$GF_CONF/dashboards/"
if [[ ! -f "$CONF/grafana-admin-password" ]]; then
    umask 077
    openssl rand -base64 24 >"$CONF/grafana-admin-password"
    umask 022
fi
chown "$UID_GRAFANA:$UID_GRAFANA" "$CONF/grafana-admin-password"
chmod 400 "$CONF/grafana-admin-password"

# ----------------------------------------------------------------------
# 7. Data directories and the container units
# ----------------------------------------------------------------------
note "containers"
install -d -m 750 -o "$UID_PROM" -g "$UID_PROM" "$DATA/prometheus" "$DATA/alertmanager"
install -d -m 750 -o "$UID_GRAFANA" -g "$UID_GRAFANA" "$DATA/grafana"

# unit <name> <description> <docker run arguments...>
unit() {
    local name="$1" desc="$2"
    shift 2
    cat >"/etc/systemd/system/lakko-mon-$name.service" <<EOF
# Written by infra/monitoring/setup-monitoring.sh (ADR-0096).
[Unit]
Description=Lakko monitoring: $desc
After=docker.service network-online.target
Requires=docker.service

[Service]
ExecStartPre=-/usr/bin/docker rm -f lakko-mon-$name
ExecStart=/usr/bin/docker run --rm --name lakko-mon-$name --network host --log-driver journald $*
ExecStop=/usr/bin/docker stop lakko-mon-$name
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF
}

unit prometheus "Prometheus" \
    -v "$PROM_CONF:/etc/prometheus:ro" -v /etc/ssl/certs:/etc/ssl/certs:ro \
    -v "$DATA/prometheus:/prometheus" "$IMG_PROMETHEUS" \
    --config.file=/etc/prometheus/prometheus.yml --storage.tsdb.path=/prometheus \
    --storage.tsdb.retention.time=30d --web.listen-address=127.0.0.1:9090
unit blackbox "blackbox_exporter" \
    -v "$CONF/blackbox.yml:/etc/blackbox.yml:ro" -v /etc/ssl/certs:/etc/ssl/certs:ro \
    "$IMG_BLACKBOX" --config.file=/etc/blackbox.yml --web.listen-address=127.0.0.1:9115
unit alertmanager "Alertmanager" \
    -v "$AM_CONF:/etc/alertmanager:ro" -v "$DATA/alertmanager:/alertmanager" "$IMG_ALERTMANAGER" \
    --config.file=/etc/alertmanager/alertmanager.yml --storage.path=/alertmanager \
    --web.listen-address=127.0.0.1:9093 --cluster.listen-address=
unit grafana "Grafana" \
    -v "$GF_CONF/provisioning:/etc/grafana/provisioning:ro" \
    -v "$GF_CONF/dashboards:/var/lib/grafana/dashboards/lakko:ro" \
    -v "$CONF/grafana-admin-password:/run/secrets/grafana-admin-password:ro" \
    -v "$DATA/grafana:/var/lib/grafana" \
    -e GF_SERVER_HTTP_ADDR=127.0.0.1 -e GF_SERVER_HTTP_PORT=3000 \
    -e GF_SECURITY_ADMIN_PASSWORD__FILE=/run/secrets/grafana-admin-password \
    -e GF_USERS_ALLOW_SIGN_UP=false -e GF_AUTH_ANONYMOUS_ENABLED=false \
    -e GF_ANALYTICS_REPORTING_ENABLED=false -e GF_ANALYTICS_CHECK_FOR_UPDATES=false \
    -e GF_ANALYTICS_CHECK_FOR_PLUGIN_UPDATES=false -e GF_NEWS_NEWS_FEED_ENABLED=false \
    "$IMG_GRAFANA"
systemctl daemon-reload
for name in prometheus blackbox grafana; do
    systemctl enable "lakko-mon-$name" >/dev/null
    systemctl restart "lakko-mon-$name"
done
if [[ "$alertmanager_ready" == 1 ]]; then
    systemctl enable lakko-mon-alertmanager >/dev/null
    systemctl restart lakko-mon-alertmanager
else
    systemctl disable --now lakko-mon-alertmanager >/dev/null 2>&1 || true
fi

if [[ "$COLOCATED" != 1 ]]; then
    cat >/etc/default/prometheus-node-exporter <<'EOF'
# Written by infra/monitoring/setup-monitoring.sh (ADR-0096 P3).
ARGS="--web.listen-address=127.0.0.1:9100"
EOF
    systemctl enable prometheus-node-exporter >/dev/null
    systemctl restart prometheus-node-exporter
fi

# ----------------------------------------------------------------------
# 8. The synthetic canary (ADR-0096 P5)
# ----------------------------------------------------------------------
# The binary is copied to /usr/local/bin/lakko-canary by `just deploy-canary`
# and each stack's state is made once by `just canary-init <stack>`. This
# step installs the user, the unit and the settings, and (re)starts every
# instance whose state exists.
note "canary"
if ! id lakko-canary >/dev/null 2>&1; then
    useradd --system --home-dir /var/lib/lakko-canary --shell /usr/sbin/nologin lakko-canary
fi
install -d -m 700 -o lakko-canary -g lakko-canary /var/lib/lakko-canary
install -m 644 "$REPO_ROOT/monitoring/lakko-canary@.service" /etc/systemd/system/lakko-canary@.service
systemctl daemon-reload
for stack in $STACKS; do
    case "$stack" in
        prod) bind=127.0.0.1:9661 ;;
        test) bind=127.0.0.1:9662 ;;
    esac
    install -d -m 700 -o lakko-canary -g lakko-canary "/var/lib/lakko-canary/$stack"
    if [[ ! -f "$CONF/canary-$stack.env" ]]; then
        cat >"$CONF/canary-$stack.env" <<EOF
# Written by infra/monitoring/setup-monitoring.sh (ADR-0096 P5). Edit freely;
# re-running the setup keeps this file.
LAKKO_CANARY_METRICS_BIND=$bind
LAKKO_CANARY_INTERVAL_SECS=600
LAKKO_CANARY_CALL_EVERY=3
RUST_LOG=info
EOF
        chmod 644 "$CONF/canary-$stack.env"
    fi
    if [[ -x /usr/local/bin/lakko-canary && -f "/var/lib/lakko-canary/$stack/state.json" ]]; then
        systemctl enable "lakko-canary@$stack" >/dev/null
        systemctl restart "lakko-canary@$stack"
    else
        warn "canary for $stack not running: needs /usr/local/bin/lakko-canary (just deploy-canary) and its state (just canary-init $stack)"
    fi
done

# ----------------------------------------------------------------------
# 9. Firewall
# ----------------------------------------------------------------------
# Every monitoring port is bound to 127.0.0.1. On a dedicated host the
# firewall admits ssh only; on a Lakko host its own rules stay as they are.
if [[ "$COLOCATED" != 1 ]]; then
    note "firewall: ssh only"
    ufw default deny incoming >/dev/null
    ufw default allow outgoing >/dev/null
    ufw allow 22/tcp comment 'ssh' >/dev/null
    ufw --force enable >/dev/null
fi

note "done"
echo "  Grafana:   just grafana, then http://127.0.0.1:3000 (admin, password in $CONF/grafana-admin-password)"
echo "  Targets:   curl -s 127.0.0.1:9090/api/v1/targets | head"
echo "  CA cert:   $MTLS/monitoring-ca.crt goes to /etc/lakko/mtls/ on the Lakko hosts (just push-monitoring-ca-test)"
