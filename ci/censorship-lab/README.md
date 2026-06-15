<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `ci/censorship-lab/` — G5-7 censorship lab (first wave)

The lab is a Docker-Compose-driven harness for exercising
Lakko's pluggable transports against environments that
mimic the on-wire observation capabilities a real
state-level adversary brings to the table.

The lab is **additive** — every emulator is a sidecar
service the existing `docker-compose.yml` grows with;
matrix tests in `crates/lakko-transport/tests/`
(`#[ignore]`-gated) target each. The CI workflow at
[`.github/workflows/censorship-lab.yml`](../../.github/workflows/censorship-lab.yml)
runs the matrix on `workflow_dispatch` + a
`censorship-lab` PR label + a nightly cron.

## What landed in this wave

- `sni-observer/` — a small Rust binary that listens
  for TLS on `127.0.0.1:8443` (TCP) **AND for QUIC v1
  Initial packets on the same port over UDP** (G5-7-
  transport-matrix-iter3, May 2026), parses the inbound
  ClientHello, extracts the `server_name` extension
  **plus JA3 (Salesforce 2017) and JA4 (FoxIO 2023
  compact form)** passive fingerprints, appends one JSON
  line per observation to `./logs/observed.jsonl` with a
  `transport: "tcp"|"udp"` discriminator, drops the
  connection. JA3 = MD5 over `version,ciphers,
  extensions,groups,ec_point_formats` decimal CSV;
  GREASE values (RFC 8701) are filtered. JA4 =
  `t<ver><sni><nc><ne><alpn>_<hash_a>_<hash_b>` where
  the two hashes are SHA-256-truncated-to-12-hex over
  sorted unique ciphers and sorted extensions+sigalgs
  respectively. QUIC Initial packets are decrypted per
  RFC 9001 §5.2 (well-known initial salt + HKDF-Expand-
  Label deriving `quic key` / `quic iv` / `quic hp` from
  the client-side DCID, then AES-ECB-128 header-
  protection removal + AES-128-GCM AEAD); the embedded
  CRYPTO frame's TLS handshake bytes feed the same
  ClientHello parser the TCP path uses. RFC 9001 §A.2
  test vectors gate the crypto math. This
  is the unencrypted picture an on-path passive
  adversary sees on the very first packet of a TLS
  flow.
- `docker-compose.yml` brings the observer up; the host
  bind-mount at `./logs/` lets the integration test
  read the observation log out-of-band.
- [`crates/lakko-transport/tests/sni_observer_e2e.rs`](../../crates/lakko-transport/tests/sni_observer_e2e.rs)
  `#[ignore]`-gated test connects `tokio-rustls` to the
  observer with a known SNI, then asserts the matching
  line appeared in the log.

## Capability matrix

The matrix below names every adversary capability the
lab covers (or plans to cover). Cells marked **✅** are
shipped in this wave; **❌ deferred** rows track named
follow-up sub-sprints.

| Capability                                          | Status        | Tracked as                                       |
|-----------------------------------------------------|---------------|--------------------------------------------------|
| Passive SNI observation                             | ✅            | this wave — `sni-observer/`                      |
| Passive TLS-fingerprint observation (JA3 / JA4)     | ✅            | `G5-7-fingerprint` (landed)                      |
| Active probing against REALITY endpoints            | ❌ deferred   | `G5-7-reality-probe` follow-up                   |
| SNI string-match blocking (GFW-style censor)        | ✅            | `G5-7-gfw-string` (`detector` module)            |
| Connection throttling (IR-style speed cap)          | ❌ deferred   | `G5-7-throttle` follow-up                        |
| Forwarded-to-target obfuscation effectiveness       | ❌ deferred   | `G5-7-transport-matrix` follow-up                |

Each follow-up reuses the same Dockerfile + compose
shape — additional services, additional
`#[ignore]`-gated tests, no fork.

## Local run

```bash
# Make the log dir + ensure the file exists so the
# container's non-root user can write to it without a
# permission dance.
mkdir -p ci/censorship-lab/logs
touch ci/censorship-lab/logs/observed.jsonl

# Build + start. `--wait` blocks on the observer
# healthcheck so the test doesn't race startup.
docker compose -f ci/censorship-lab/docker-compose.yml up \
    --build --detach --wait --wait-timeout 180

# Run the matrix. Currently one test in this wave.
cargo test -p lakko-transport --test sni_observer_e2e \
    -- --include-ignored --nocapture

# Tear down.
docker compose -f ci/censorship-lab/docker-compose.yml down --volumes
rm -f ci/censorship-lab/logs/observed.jsonl
```

The test reads its target from
`$LAKKO_CENSORSHIP_OBSERVER_ADDR` (default
`127.0.0.1:8443`) and the log path from
`$LAKKO_CENSORSHIP_OBSERVER_LOG` (default
`<workspace>/ci/censorship-lab/logs/observed.jsonl`).

## Why a custom Rust observer + not tshark / mitmproxy?

- **Auditability.** The 200-line observer is
  reviewable in one sitting; an attacker who modified
  it would have to land a PR through `git diff` review.
  Pulling in tshark / mitmproxy means trusting a much
  bigger dependency tree at the lab's trust boundary.
- **Reproducible builds.** The observer goes through
  the same `cargo build --release` reproducibility
  posture the production binaries take. tshark binaries
  vary by distro / version / capture-card hardware.
- **No application-layer surprises.** The observer
  parses only what's necessary (record header +
  ClientHello header + extension walk) — no validation,
  no rustls-like state machine, no surprise upgrades
  hiding regressions.

## Security posture

The observer is **never** deployed to production. It
lives under `ci/` only and the production Dockerfile
for `services/sfu` does not COPY the `ci/` tree (the
`.dockerignore` rule that landed with G3-4 excludes
it). A reviewer reading any `services/*/Dockerfile`
will not find a path through which the observer could
end up in a shipped image.
