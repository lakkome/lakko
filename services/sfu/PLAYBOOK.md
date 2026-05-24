<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `services/sfu` — operational playbook

Exact commands for build, run, test, deploy. Strategy is
in [ADR-0011](../../docs/adr/0011-rust-sfu.md); pinned
dependencies in [`UPSTREAM.md`](./UPSTREAM.md); this file
is the recipe book.

All commands assume `pwd = repo root`.

## Current status

**Crate not yet bootstrapped.** Sprint
[`S-3-B-spike`](../../docs/ROADMAP-PHASE3-CLOSEOUT.md)
runs the `webrtc-rs` / `str0m` bake-off; sprint
`S-3-B-skeleton` lands `Cargo.toml` + `src/main.rs` and
turns the rest of this playbook operative. Until then,
every section below is **the contract for what the
skeleton sprint must produce**.

## 1. Build

After `S-3-B-skeleton`:

```bash
# Workspace build picks up services/sfu automatically once
# it's added to Cargo.toml [workspace] members.
cargo build -p lakko-svc-sfu

# Release build for deployment.
cargo build -p lakko-svc-sfu --release
```

Reproducible-build invariant: the binary's SHA256 is byte-
identical across the two CI runners. The Nix flake pins
the toolchain; `Cargo.lock` pins every transitive.

## 2. Run locally

Pre-requisite: `auth.lakko.me` is reachable (or a local
`services/auth` is running and exposing JWKS at
`/.well-known/jwks.json`).

```bash
# .env entries the SFU consumes — see services/sfu/src/config.rs
# (lands in S-3-B-prod) for the canonical list.
export LAKKO_SFU_BIND=127.0.0.1:7880
export LAKKO_SFU_JWKS_URL=http://127.0.0.1:8081/.well-known/jwks.json
export LAKKO_SFU_JWKS_REFRESH_SEC=300
export LAKKO_SFU_METRICS_BIND=127.0.0.1:6789
export LAKKO_SFU_ICE_TURN_URL=turn:turn.lakko.me:443?transport=tcp
export RUST_LOG=info,lakko_svc_sfu=debug

cargo run -p lakko-svc-sfu --bin lakko-svc-sfu
```

Verify:

```bash
curl -i http://127.0.0.1:7880/health   # → 200 ok
curl -i http://127.0.0.1:7880/ready    # → 200 ready (after JWKS pull)
curl -i http://127.0.0.1:6789/metrics  # → Prometheus exposition
```

## 3. Test

```bash
# Unit + integration tests inside the crate.
cargo test -p lakko-svc-sfu

# Full workspace test (catches cross-crate breakages).
just test

# Lint gates (cargo clippy --workspace -- -D warnings).
just lint

# Cargo audit / cargo deny across the workspace.
just audit

# Integration tests with mock peers — these live under
# services/sfu/tests/, drive two webrtc-rs / str0m peers
# through the real signaling endpoint, and assert SFrame
# ciphertext byte-exact passthrough.
cargo test -p lakko-svc-sfu --test integration
```

CI gates that block merges:

1. `cargo test --workspace` green.
2. `cargo clippy --workspace --all-targets -- -D warnings`
   clean.
3. `cargo deny check` clean.
4. `cargo audit` baselined against the documented
   exceptions in
   [`docs/security/known-gaps.md`](../../docs/security/known-gaps.md).
5. Reproducible-build check: two GitHub Actions runners
   produce a byte-identical `target/release/lakko-svc-sfu`.

## 4. Deploy

Per
[`infra/README-calls.md`](../../infra/README-calls.md) —
Hetzner CCX22 dedicated VPS, systemd unit
[`lakko-sfu.service`](../../infra/systemd/lakko-sfu.service),
nginx vhost
[`sfu.conf`](../../infra/nginx/sites-available/sfu.conf),
deploy script
[`deploy-sfu.sh`](../../infra/scripts/deploy-sfu.sh).

```bash
# 1. Build the release binary on a CI runner (or laptop
#    with the pinned Rust toolchain).
cargo build -p lakko-svc-sfu --release

# 2. Ship to the SFU VPS.
scp target/release/lakko-svc-sfu \
    root@sfu.lakko.me:/usr/local/bin/lakko-sfu

# 3. Restart.
ssh root@sfu.lakko.me 'systemctl restart lakko-sfu'

# 4. Tail logs.
ssh root@sfu.lakko.me 'journalctl -u lakko-sfu -f'
```

The first-time bootstrap (systemd unit install, nginx
vhost, TLS cert, ufw) is the
[`infra/scripts/deploy-sfu.sh`](../../infra/scripts/deploy-sfu.sh)
one-shot — see
[`infra/README-calls.md`](../../infra/README-calls.md)
§"First-time deploy" for the sequence.

## 5. Bumping the WebRTC stack

See [`UPSTREAM.md`](./UPSTREAM.md) §"Bumping the pin".
Short version:

```bash
# Update the exact version in services/sfu/Cargo.toml.
$EDITOR services/sfu/Cargo.toml
cargo update -p webrtc        # or -p str0m
just audit
just lint
just test
# Append changelog entry to UPSTREAM.md.
git add services/sfu/Cargo.toml Cargo.lock services/sfu/UPSTREAM.md
git commit -m "chore(sfu): bump webrtc-rs to 0.X.Y"
```

Major bumps (1.x → 2.x) ride their own PR with the diff
explained in the commit message.

## 6. Troubleshooting

### "Health check fails right after deploy"

Most likely the SFU can't reach the auth JWKS endpoint.

```bash
ssh root@sfu.lakko.me 'curl -sv https://auth.lakko.me/.well-known/jwks.json | head'
ssh root@sfu.lakko.me 'journalctl -u lakko-sfu -n 50 | grep jwks'
```

Fix: confirm `LAKKO_SFU_JWKS_URL` in
`/etc/lakko/lakko.env` points to a reachable, valid JWKS
endpoint. The `ready` probe stays red until the first
successful pull.

### "Peers connect but no media flows"

Almost always an ICE problem — coturn unreachable, or the
SFU's advertised ICE candidates resolve to the wrong
interface.

```bash
# Verify coturn from outside the VPS.
openssl s_client -connect turn.lakko.me:443 -servername turn.lakko.me < /dev/null

# Tail SFU logs at debug level — webrtc-rs / str0m emit
# ICE candidate lines we propagate via tracing.
ssh root@sfu.lakko.me 'RUST_LOG=lakko_svc_sfu=debug,webrtc=debug systemctl restart lakko-sfu'
ssh root@sfu.lakko.me 'journalctl -u lakko-sfu -f | grep -i ice'
```

### "Metrics endpoint missing alarms-referenced metric"

Cross-reference
[`infra/prometheus/rules/calls.yml`](../../infra/prometheus/rules/calls.yml).
Every PromQL `expr:` in there names a metric the SFU
**must** expose. The `S-3-B-metrics` sprint enumerates
them — if a rule lights up with `NoData`, that's a bug in
the metrics module, not in the alert.

### "Memory grows during a long call"

Check whether `RTCPeerConnection` instances drop cleanly
when peers leave. The `S-3-B-routing` sprint adds an
integration test that opens + closes 100 peers in a loop
and asserts steady-state RSS — that test must stay green.

```bash
ssh root@sfu.lakko.me '
  while sleep 30; do
    ps -o rss,vsz,pid,cmd -p "$(systemctl show -p MainPID lakko-sfu | cut -d= -f2)"
  done
'
```

## 7. What used to be here

This file previously described a `vendor + patches +
quarterly rebase` workflow for a LiveKit Go fork. That
strategy is **superseded** by
[ADR-0011](../../docs/adr/0011-rust-sfu.md). The Git
history retains the old playbook (commit
`a9fb877` and prior); current operations follow the
sections above.
