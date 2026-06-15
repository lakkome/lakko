<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Live cross-runtime SFrame interop smoke (G-26 residual)

The **headless-achievable** half of G-26 is already proven firsthand and gates
every CI run — see
[`golden_vector_round_trips_through_the_live_sfu`](../../services/sfu/tests/synthetic_client_e2e.rs):
it seals the canonical cross-stack golden vector, pushes it through the
production media task over a real `str0m` ICE+DTLS forward, and the subscriber
`open_framed`s it back, so the exact bytes every web / iOS / Android stack
produces are proven to survive the live SFU. Composed with the per-platform
golden-vector tests (Rust / web vitest / iOS XCTest / Android instrumented),
the cross-runtime byte interop is a **proven equality**, not an assumption.

This directory is the **last residual**: re-observing that composition with
REAL runtime media pipelines — a real browser's `RTCRtpScriptTransform` SFrame
worker (the Node/wasm golden test does not exercise the browser Encoded
Transform), and ultimately a real native libwebrtc receiver. It is *redundant
for correctness* (it re-checks the composition of two already-proven facts) and
is a deploy / CI-execution concern, not a development one.

## Status — orchestration scaffold (NOT yet executed firsthand)

| Piece | State |
|---|---|
| `docker-compose.yml` (media-enabled SFU + JWKS) | `docker compose config`-valid |
| `.github/workflows/sframe-interop.yml` | shape mirrors `sfu-e2e.yml` / `g3-4-mobile.yml`; the JWT-mint + wasm-build + compose-up steps are real; the Playwright step is a loud-failing placeholder until armed |
| `apps/web/e2e/sframe-interop/` Playwright harness | design specified below; arming it (`@playwright/test` + the assertion) is the first task |

The maintainer was told up front: the end-to-end browser run **cannot be
verified in this development environment** (it needs a Linux CI runner with
host networking + Chromium + the media-plane UDP). Nothing here claims to be
green; the cargo golden-SFU test above is the firsthand-proven coverage.

## Web leg — design

Two headless-Chrome peers join ONE SFU call through the real
[`SfuCallSession`](../../apps/web/src/calls/sfu-call-session.ts) (which attaches
the real `RTCRtpScriptTransform` SFrame worker on every track). Peer A
publishes a fake-media audio track; peer B subscribes. Both derive the SAME
per-call SFrame seed (the test fixes it — the MLS exporter's stand-in, C-1).

**The assertion needs no plaintext exfiltration and no worker instrumentation.**
SFrame frames that fail to open are DROPPED by the receiver's worker before the
decoder (invariant #4), so if B's worker could NOT open A's frames, B's decoder
receives nothing. The smoke therefore reads B's inbound-RTP `getStats`:

- **positive:** with the matching seed, `inbound-rtp.audio` shows
  `packetsReceived > 0` AND a climbing `totalSamplesReceived` /
  `framesDecoded` (or a LOW `concealmentEvents` ratio) — the worker opened the
  frames the publisher's worker sealed, through the live SFU.
- **negative (anti-no-op):** a second run where B derives a WRONG seed must
  show packets arriving (`packetsReceived > 0`, the SFU still forwarded) but
  effectively NO decoded samples — every frame dropped at the SFrame gate.

The positive ⊕ negative pair is what makes it a real interop proof rather than
"some audio happened to play".

### Why `network_mode: host`

The SFU media plane binds a UDP socket and `str0m` advertises ICE candidates
from the bound address; inside a bridge-networked container that is the
container-internal IP, unreachable from a host Chromium, and the SFU has no
advertised-public-IP setting. Host networking binds the media UDP port on the
host directly, so the offered candidate is host-reachable. This is Linux-only
(Docker Desktop for macOS has no host networking) — hence `runs-on:
ubuntu-latest` and a Linux-only local run.

## Native leg — extension (not built)

The web↔web leg proves the real browser worker over the live SFU. The full
G-26 picture adds a real native libwebrtc receiver (web seals → SFU relays →
iOS / Android shim opens). The infrastructure for that already exists in
[`g3-4-mobile.yml`](../../.github/workflows/g3-4-mobile.yml): it boots an
iPhone simulator + a Pixel-class Android emulator via
`reactivecircus/android-emulator-runner`, mints the same fixture JWT, and runs
the `SfuSignalingClient` round-trip. The extension is to have the native
`SfuCallSession` SUBSCRIBE to the web peer's published track and assert the
native shim decoded it (the same `getStats` shape, via the native pods'
`sampleConnectionStats`). That is the deferred `g3-4-mobile-call-iter2`
("dual-instance live RTP") plus a web publisher.

## Run it (when armed, on a Linux host)

```bash
# 1. Build the wasm core + bring up the media-enabled SFU + JWKS.
cd apps/web && npm ci && npm run build:wasm && cd ../..
docker compose -f ci/sframe-interop/docker-compose.yml up --build --detach --wait

# 2. Mint a fixture-signed JWT.
JWT=$(cargo run -q --release -p lakko-call-jwt --example mint_test_jwt -- \
  --call-id deadbeef0000000000000000000000aa \
  --account cafef00d0000000000000000000000bb --role participant --ttl-secs 600)

# 3. Drive the two browser peers (after the harness is armed).
LAKKO_SFRAME_INTEROP_SFU_URL=ws://127.0.0.1:7880 LAKKO_SFRAME_INTEROP_JWT="$JWT" \
  bash -c 'cd apps/web && npx playwright test e2e/sframe-interop'

# 4. Tear down.
docker compose -f ci/sframe-interop/docker-compose.yml down --volumes
```

## CI

`.github/workflows/sframe-interop.yml` runs on `workflow_dispatch` only (it
needs the Linux + Chromium + media-plane runner). The Playwright step fails
loud until the harness is armed, so an un-armed run cannot masquerade as green.
