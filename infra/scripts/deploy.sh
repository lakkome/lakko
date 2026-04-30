#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Deploy script — runs ON the VPS, invoked by `just deploy` (which ssh-ит
# сюда) или вручную: `sudo -u lakkome /home/lakkome/lakko/infra/scripts/deploy.sh`
#
# Что делает:
#   1. git pull в /home/lakkome/lakko
#   2. cargo build --release всех 5 service-bin-ов (auth, ds, storage,
#      discovery, push-gw)
#   3. install бинарей в /usr/local/bin/ (требует sudo, см. sudoers ниже)
#   4. systemctl restart lakko.target
#
# Sudoers config на VPS (для passwordless restart):
#   /etc/sudoers.d/lakko-deploy:
#     lakkome ALL=(root) NOPASSWD: /usr/bin/install -m 755 /tmp/lakko-svc-* /usr/local/bin/, /usr/bin/systemctl restart lakko.target, /usr/bin/systemctl status lakko.target

set -euo pipefail

REPO=/home/lakkome/lakko
SERVICES=(auth ds storage discovery push-gw)

if [[ "${EUID}" -eq 0 ]]; then
    echo "ERROR: запускай как lakkome, а не root. Скрипт сам делает sudo для нужных шагов." >&2
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
