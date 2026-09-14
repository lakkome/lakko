#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Deploy script — runs ON the VPS, invoked by `just deploy` (which ssh-es
# in) or manually: `sudo -u lakkome /home/lakkome/lakko/infra/scripts/deploy.sh`
#
# What it does:
#   1. git pull in /home/lakkome/lakko
#   2. cargo build --release for all service binaries (auth, ds, storage,
#      discovery, push-gw, channels, sfu, gif-proxy)
#   3. install binaries into /usr/local/bin/ (requires sudo, see sudoers below)
#   4. systemctl restart lakko.target
#
# NOTE: the SFU (`lakko-svc-sfu`) needs a one-time setup beyond
# the binary install — its nginx vhost (sfu.lakko.me), TLS cert,
# and the public UDP media port in the firewall. Run
# `infra/scripts/deploy-sfu.sh` once on the main VPS for that;
# subsequent code deploys ride this script.
#
# NOTE: this script installs binaries only, never systemd units. The first
# deploy that adds a service (gif-proxy, ADR-0047) needs its unit copied and
# enabled by hand before `systemctl restart lakko.target` can start it:
#   sudo cp infra/systemd/lakko-svc-gif-proxy.service infra/systemd/lakko.target /etc/systemd/system/
#   sudo systemctl daemon-reload
#
# Sudoers config on the VPS (for passwordless restart):
#   /etc/sudoers.d/lakko-deploy:
#     lakkome ALL=(root) NOPASSWD: /usr/bin/install -m 755 /tmp/lakko-svc-* /usr/local/bin/, /usr/bin/systemctl restart lakko.target, /usr/bin/systemctl status lakko.target

set -euo pipefail

REPO=/home/lakkome/lakko
SERVICES=(auth ds storage discovery push-gw channels sfu gif-proxy)

if [[ "${EUID}" -eq 0 ]]; then
    echo "ERROR: run as lakkome, not as root. The script invokes sudo only for the steps that need it." >&2
    exit 1
fi

echo "==> [$(date -Iseconds)] git pull"
cd "$REPO"
git fetch origin
git checkout main
git pull --ff-only origin main

echo "==> cargo build --release"
for svc in "${SERVICES[@]}"; do
    cargo build --release --locked --bin "lakko-svc-$svc"
done

echo "==> install binaries"
for svc in "${SERVICES[@]}"; do
    src="$REPO/target/release/lakko-svc-$svc"
    cp "$src" "/tmp/lakko-svc-$svc"
done
sudo install -m 755 /tmp/lakko-svc-* /usr/local/bin/
rm -f /tmp/lakko-svc-*

echo "==> systemctl restart lakko.target"
sudo systemctl restart lakko.target

echo "==> waiting 3s for services to start"
sleep 3

echo "==> systemctl status lakko.target"
sudo systemctl status lakko.target --no-pager || true

echo "==> done."
