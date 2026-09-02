<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0011: Rust SFU on webrtc-rs / str0m — supersedes the LiveKit fork

- **Status:** Accepted
- **Date:** 2026-05-24
- **Supersedes:** [ADR-0003](./0003-livekit-fork.md) (LiveKit fork as the SFU), [ADR-0008](./0008-livekit-fork-strategy.md) (operational fork strategy)
- **Context:** Phase 3 closeout planning — pivot from a LiveKit fork to a minimal Rust-native SFU.

## Context

[ADR-0003](./0003-livekit-fork.md) (April 2026) decided that
Lakko's group-call SFU would be a fork of
[`livekit/livekit`](https://github.com/livekit/livekit), and
[ADR-0008](./0008-livekit-fork-strategy.md) (May 2026) locked
in the operational shape of that fork: pinned upstream
commit, quarterly rebases, sub-800-line patch surface.

Re-examining both decisions before writing the first patch
in 3b1c, two facts have hardened:

1. **LiveKit is a platform, not an SFU.** Its monorepo
   (~150 000 lines of Go) ships room management, recording
   (`livekit-egress`), RTMP/WHIP ingestion (`livekit-ingress`),
   an AI-agents framework, webhooks, telemetry, cluster
   routing, and a client SDK family. The *actual* SFU
   forwarding logic is a thin slice of all that. Lakko needs
   only the slice; the remaining 95 % is dead weight on our
   audit surface, attack surface, and rebase tax.

2. **`webrtc-rs` and `str0m` are production-ready in 2026.**
   ADR-0008's "Custom Rust SFU" rejection cited "`webrtc-rs`
   ecosystem less mature than `pion/webrtc`" — accurate at
   the time of writing (Q2 2026), no longer accurate now.
   Discord, Cloudflare Calls, Element/Matrix media, and
   several others run Rust-native WebRTC in production. The
   maturity gap closed.

The trade-off therefore inverts: a Rust SFU now costs
**more initial work** (≈ 5–6 sprints) but **zero ongoing
maintenance tax**, with audit surface ~50× smaller and
recording architecturally absent rather than patched-out.

## Decision

### Build a minimal Rust SFU on top of `webrtc-rs` or `str0m`

`services/sfu/` becomes a regular Rust crate in the
workspace (`lakko-svc-sfu`), implemented on one of two
Rust WebRTC stacks:

- **[`webrtc-rs`](https://github.com/webrtc-rs/webrtc)** —
  Rust port of `pion/webrtc`, async / tokio-based, broader
  feature coverage including DataChannel + SCTP.
- **[`str0m`](https://github.com/algesten/str0m)** — pure
  Rust, sync (caller drives the event loop), narrower
  surface, used in production by Eyevinn and others.
  Strong audit story: caller controls scheduling
  completely.

The choice between the two is **deferred** to
**S-3-B-spike** (the first concrete sprint following this
ADR). The spike builds an echo-test on each, measures
throughput, RTT overhead, memory-per-peer, and notes any
deal-breaker API gaps. The spike's findings are recorded
as an amendment to this ADR (`## Spike outcome` section
appended once the spike lands).

### What the SFU does

Strict subset of LiveKit's responsibilities:

1. **WebSocket signaling endpoint** speaking *Lakko's* wire
   format (not LiveKit's JSON-RPC). We are not interoperating
   with anything LiveKit-shaped, so we keep the wire format
   minimal and protobuf-defined under `schemas/sfu/v1.proto`.
2. **Per-peer `RTCPeerConnection`** via the chosen Rust
   WebRTC stack. ICE / DTLS / SRTP handled by the library.
3. **RTP track fanout** — incoming media tracks from one
   peer are forwarded as outgoing tracks on every other
   peer in the room. SFrame ciphertext passes through
   opaque; the SFU never inspects payload.
4. **Simulcast / SVC layer selection** per receiver, using
   TWCC feedback to estimate downlink bandwidth.
5. **`lakko-call-jwt` verification** on every join, against
   the JWKS published by `services/auth`. EdDSA-Ed25519
   (same crate as the auth service mints from —
   `crates/lakko-call-jwt`).
6. **Prometheus metrics** via the `metrics` crate (same
   surface as `services/auth`, `services/ds`, etc.), so
   `infra/prometheus/rules/calls.yml` rules apply
   unchanged.

### What the SFU does NOT do

Explicitly absent — these are *not* "optional features
we're deferring", they are categorically excluded:

- **Recording.** No code path exists that writes media to
  disk, queues it for a recorder process, or hooks an
  egress side-channel. Under a key-disclosure subpoena
  (UK IPA / AU AA Act / RU Yarovaya / India CERT-In) the
  honest legal answer is "this capability does not exist in
  the binary you are pointing at", not "we patched it out".
- **Ingest.** No RTMP / WHIP receiver — broadcasts use the
  same WebRTC join flow as any other participant.
- **Agents framework / webhooks / room dashboards.** Out
  of scope; not part of Lakko's product surface.
- **A Lakko-specific client SDK.** Clients drive their
  native `RTCPeerConnection` (`WebRTC.framework` on
  Apple platforms, `webrtc-sdk:android` on Android, the
  browser-built-in `RTCPeerConnection` on web) and speak
  the Lakko signaling protocol directly. The SFrame
  keys are derived locally from the MLS exporter, exactly
  per [ADR-0001](./0001-mls-only.md) and
  [`docs/security/calls.md`](../security/calls.md).
- **Plaintext-media fallback.** If a peer fails to enable
  the SFrame transform, the SFU refuses to forward its
  tracks. There is no "best-effort plaintext" mode.

### What clients use

Unchanged from [ADR-0003](./0003-livekit-fork.md):

| Platform | WebRTC stack | Reason |
|---|---|---|
| iOS / iPadOS / macOS / watchOS | `WebRTC.framework` (Apple-built or `GoogleWebRTC` SPM) | Hardware H.264 via `VideoToolbox`; `AVAudioSession` device routing; CallKit / `CXProvider` integration |
| Android | `io.github.webrtc-sdk:android` (community fork of the now-archived `org.webrtc:google-webrtc`) | Hardware codecs via `MediaCodec`; `ConnectionService` integration; `AudioManager` device routing |
| Web | Browser-built-in `RTCPeerConnection` | Free; insertable-streams hook for SFrame |
| Desktop (Tauri) | Browser-built-in via the embedded webview | Same as web |

Switching the client to `webrtc-rs` was considered. It
fails the hardware-codecs and OS-integration requirements
(no `VideoToolbox` / `MediaCodec` access, no CallKit /
ConnectionService binding, no `AVAudioSession` routing).
The client-side `WebRTC.framework` / `webrtc-sdk:android`
choice **stays**; only the *server* moves to Rust.

### Topologies (unchanged from ADR-0003)

| Call type | Server | Topology |
|---|---|---|
| P2P 1×1 | **none** (direct ICE + coturn for NAT traversal) | Client A ↔ Client B with optional coturn STUN/TURN |
| Group via link | Rust SFU | Star — each client opens one PC to the SFU |
| Group inside chat | Rust SFU | Same — reuses the chat MLS group for keys |
| Broadcast (Phase 4) | Rust SFU + edge cascade | Origin SFU fans out to edge SFUs; clients connect to nearest edge |
| Audio room | Rust SFU | Star + role distinction (`Speakers` MLS subgroup, `Audience` sender-key) |

The 1×1 path has never touched the SFU. ADR-0003 still
governed it as "Pure WebRTC + ICE/STUN/TURN" — that line
survives unchanged. **What changes** is everything labelled
"LiveKit SFU" above: it is now "Rust SFU on webrtc-rs /
str0m".

## Alternatives considered (this time around)

### Keep the LiveKit fork, just minimise patches

The path we were on. Rejected because:

- The dead-weight code remains in the deployment binary
  even if our patches strip its REST hooks. A bug in
  `pkg/agent/` is still a CVE that lights up our
  `auth.lakko.me`-domain CVE feed.
- Each upstream rebase risks re-introducing capabilities
  we removed (recording, agents). "Patched-out" is weaker
  than "absent".
- Quarterly 4–8 hours/week rebase tax, forever.
- Audit surface 150 k LOC Go we don't write.

### Fork only the SFU package, not the whole repo

Considered: extract `livekit/livekit/pkg/sfu/` + minimal
glue, drop everything else. Rejected because the SFU
package has tendrils into `pkg/rtc`, `pkg/routing`,
`pkg/service` — disentangling is approximately the same
work as writing our own on top of `webrtc-rs`, with
worse final code clarity (Go idioms grafted onto Lakko's
Rust workspace).

### Use [`mediasoup`](https://github.com/versatica/mediasoup) (C++/Node)

Excellent SFU. Rejected because:

- Different language stack (C++ worker + Node.js host).
- Integration with our Rust core needs C++ FFI or a Node
  bridge — both add toolchain weight.
- No security or maturity advantage over `webrtc-rs`
  in 2026.

### Use [`pion/webrtc`](https://github.com/pion/webrtc) directly without LiveKit

The Go stack LiveKit itself is built on. Rejected because
it brings the Go toolchain back into the project for no
material gain over `webrtc-rs` (which is its Rust port).
The monolang win disappears.

### Continue with [`pion/ion-sfu`](https://github.com/pion/ion-sfu)

Considered. Rejected: the project has been
**unmaintained since 2022**.

### Build a fresh SFU on the experimental Rust `medea` stack

Rejected: medea is research-grade, no production
deployments visible, sparse documentation.

## Consequences

### Positive

- **Monolang Rust stack** — same `clippy::pedantic`,
  `#![forbid(unsafe_code)]`, `cargo deny`, `cargo audit`,
  `tracing`, `thiserror`, `metrics` patterns as every other
  service. One toolchain, one CI pipeline, one set of
  conventions.
- **`crates/lakko-call-jwt`** linked directly into the SFU
  via `workspace.dependencies` — the same code that mints
  the JWT in `services/auth` verifies it in `services/sfu`.
  Impossible to drift.
- **`crates/lakko-proto`** linked directly — no Go-side
  protobuf regeneration; the signaling wire types are
  shared with the rest of the platform via
  `workspace.dependencies`.
- **Audit surface ≈ 3 000 LOC Rust** (estimate) vs
  150 000 LOC Go. Externally auditable in a fraction of
  the time and cost.
- **Recording categorically absent** — the legal posture
  under key-disclosure laws becomes "we do not implement
  this capability" rather than "we have disabled it".
- **No ongoing rebase tax.** Library upgrades
  (`webrtc-rs` / `str0m`) are infrequent and bounded —
  `cargo update` is not the same as merging 150 k LOC of
  Go drift.
- **No client SDK fork either way.** Clients keep their
  native WebRTC stacks; the signaling wire format is the
  only contract.

### Negative

- **More initial work.** ~7.5 sprints to ship a feature-
  complete Rust SFU vs ~2 sprints to land patches on a
  LiveKit fork. We accept the up-front cost in exchange
  for the long-run zero-maintenance posture.
- **TWCC / bandwidth estimation / layer-switching logic
  we write ourselves**, instead of inheriting LiveKit's.
  Carries implementation risk: getting layer switching
  smooth under packet loss is real engineering. Spike
  + integration-test budget reflects this.
- **Audit dependency on `webrtc-rs` / `str0m`'s own
  correctness.** Mitigated by their existing production
  deployments and active maintainership. Not different
  in kind from depending on `openmls`.

### Neutral

- **Latency.** SFU forwarding overhead is dominated by
  network RTT, not the implementation language. Go vs
  Rust SFU latency difference is < 1 ms in practice.
- **Throughput.** Both `webrtc-rs` and `str0m`
  benchmark within a small constant factor of `pion`.
  Our deployment ceiling is bandwidth-bound (Hetzner
  CCX22 = 20 TB/month), not CPU-bound.

## Spike outcome

`S-3-B-spike` completed 2026-05-25. Detailed rationale lives
in [`services/sfu/SPIKE.md`](../../services/sfu/SPIKE.md);
the headline:

- **Chosen stack:** **`str0m`**.
- **Decision basis:** audit-friendliness — sync caller-
  driven event loop + ~6 k LOC surface beats `webrtc-rs`'s
  ~30 k LOC of async / tokio-spawned plumbing for a
  forwarding-only SFU under a "server is assumed
  compromised" threat model.
- **What we gave up:** full SCTP / DataChannel — neither
  is used by Lakko (call signaling rides our own WS
  protocol, chat-during-call rides MLS application
  messages from S-3-A-chat).
- **Reversal criteria:** if `S-3-B-routing` hits a `str0m`
  API gap that is hard to work around, the routing
  sprint may either patch upstream + pin to a fork, or
  switch to `webrtc-rs`. Documented as a follow-up
  amendment to this ADR, not as an emergency.
- **Empirical numbers (throughput / memory / RTT)** are
  deferred to the benchmark task inside
  `S-3-B-routing`. An echo-only spike on each stack
  would measure "echo loopback", which is not a useful
  proxy for the N-peer fanout we actually need to
  benchmark.

## Phase ordering inside Phase 3 closeout

Replaces [ADR-0008](./0008-livekit-fork-strategy.md) §"Phase
ordering inside 3b". The new sprint structure is captured
in [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md);
the key headline:

| Sprint | What |
|---|---|
| S-3-B-spike | `webrtc-rs` vs `str0m` echo-test bake-off; decision recorded as an amendment to this ADR |
| S-3-B-skeleton | `services/sfu/` Rust crate; axum WS signaling endpoint; peer registry; echo-test green |
| S-3-B-routing | Multi-peer RTP fanout |
| S-3-B-simulcast | Simulcast layer extraction + per-receiver selection |
| S-3-B-jwt | `lakko-call-jwt` verification at admission |
| S-3-B-twcc | TWCC aggregation + per-receiver bandwidth estimation |
| S-3-B-metrics | Prometheus exporter for `infra/prometheus/rules/calls.yml` rules |
| S-3-B-prod | Config, graceful shutdown, health/ready, structured `tracing` logs |
| S-3-B-tests | Integration tests (mock peers, SFrame passthrough, byte-exact ciphertext) |

The auth-JWT token format from [ADR-0008](./0008-livekit-fork-strategy.md) §"Auth token format"
**survives unchanged** — that decision was sound; it's
captured in `crates/lakko-call-jwt` and validates the
same way against the SFU built on top of either stack.

## Migration notes

> **Done (verified 2026-09-02).** `lakko-patches/` and `lakko-additions/` are
> gone, the three markdown files were rewritten, and `services/sfu/` is now the
> Rust crate (`Cargo.toml`, `src/`, `tests/`, `fuzz/`, `Dockerfile`, plus
> `SPIKE.md` recording the `webrtc-rs` vs `str0m` bake-off). The section below
> is the migration instruction as written, kept as the record of what was to be
> removed.

`services/sfu/` ~~currently contains~~ **contained, at the time this ADR was
written,** scaffolding for a LiveKit fork:

```text
services/sfu/
├── README.md          ← rewritten to describe the Rust crate
├── UPSTREAM.md        ← rewritten as "pinned dependencies"
├── PLAYBOOK.md        ← rewritten as a Rust build/run/test runbook
├── lakko-patches/     ← removed (no upstream to patch)
└── lakko-additions/   ← removed (the whole crate IS Lakko code)
```

The directory's existing markdown files are rewritten in
the same commit as this ADR lands. No upstream Go code
was ever vendored (per [`UPSTREAM.md`](../../services/sfu/UPSTREAM.md):
"STATUS: NOT YET PINNED"), so no Go deletion needs to
happen.

## Cross-links

- [ADR-0001](./0001-mls-only.md) — MLS exporter for call media keys (unchanged)
- [ADR-0002](./0002-rust-core.md) — Rust core decision; this ADR makes the server side monolang too
- [ADR-0003](./0003-livekit-fork.md) — **Superseded** (the SFU choice changes; the SFrame + MLS-exporter parts of 0003 survive in spirit)
- [ADR-0006](./0006-licensing.md) — licensing (a fresh Rust crate is AGPLv3-native; no relicense work)
- [ADR-0008](./0008-livekit-fork-strategy.md) — **Superseded**
- [`docs/security/calls.md`](../security/calls.md) — call-side crypto invariants (unchanged — they were never LiveKit-specific)
- [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md) — sprint decomposition that operationalises this decision
- [`crates/lakko-call-jwt`](../../crates/lakko-call-jwt) — JWT crate the SFU links directly
