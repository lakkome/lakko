<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `ci/sfu-e2e/` — G3-4 end-to-end CI gate

Brings up a Dockerised `lakko-svc-sfu` against a fixture
JWKS sidecar, then runs an `#[ignore]`-gated Rust
integration test that drives the WebSocket signaling
contract over real container ports.

## Layout

```
ci/sfu-e2e/
├── README.md              — this file
├── docker-compose.yml     — sfu + nginx-test-jwks stack
├── nginx.conf             — static-content vhost for the JWKS
└── test-jwks/
    ├── README.md          — the fixture seed + regen instructions
    └── jwks.json          — committed JWKS (from the fixed seed)
```

The SFU image itself is built from
[`services/sfu/Dockerfile`](../../services/sfu/Dockerfile).

## Local run

```bash
# Bring up the stack — builds the SFU image on first run.
docker compose -f ci/sfu-e2e/docker-compose.yml up -d --build

# Wait for both containers' healthchecks to flip green.
docker compose -f ci/sfu-e2e/docker-compose.yml ps

# Run the E2E test against the live containers.
cargo test -p lakko-svc-sfu --test e2e_dockerized -- --include-ignored

# Tear down + remove the test-jwks volume.
docker compose -f ci/sfu-e2e/docker-compose.yml down -v
```

The test reads the live URL from `$LAKKO_SFU_E2E_URL`
(default `ws://127.0.0.1:7880/v1/connect`) and the
fixture seed from `$LAKKO_SFU_E2E_SEED_HEX` (default
`4242...42`, the 32-byte all-`0x42` seed pinned in
[`test-jwks/README.md`](./test-jwks/README.md)).

## CI

The same compose stack runs from
[`.github/workflows/sfu-e2e.yml`](../../.github/workflows/sfu-e2e.yml).
That workflow:

1. `cargo build` the SFU on the host (sanity).
2. `docker compose up -d --build` the stack.
3. Wait for both healthchecks.
4. `cargo test --test e2e_dockerized -- --include-ignored`.
5. On failure: collect `docker compose logs`.
6. `docker compose down -v` regardless of outcome.

## Scope — what this gate does and does not check

**Covers** (signaling-layer contract — invariants C-39
in [`docs/security/calls.md`](../../docs/security/calls.md)):

- Real TCP socket bind, real HTTP+WS upgrade.
- Real JWKS fetch with HTTP timing characteristics.
- The full `lakko-call-jwt` verify pipeline against a
  Dockerised binary (not in-process axum).
- Real container-to-container DNS for the JWKS resolver.

**Does NOT cover** (deferred to dedicated sub-sprints
documented in
[`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../../docs/ROADMAP-PHASE3-CLOSEOUT.md)):

- RTP / SRTP / DTLS media plumbing — needs a synthetic
  `str0m`-client docker side too. Tracked under
  S-3-B-routing-tracks.
- Real WebRTC clients (iOS / Android) calling the SFU —
  needs physical devices + a CI-side simulator harness.
  Tracked under G3-8 (acceptance-on-hardware).
- TURN relay path — STUN-only in the fixture; production
  bootstrap config wires real TURN servers per
  [ADR-0011](../../docs/adr/0011-rust-sfu.md).
- E2EE SFrame layer — call surface across all platforms
  is currently SRTP-only. Tracked under G3-{1,2}-sframe.
