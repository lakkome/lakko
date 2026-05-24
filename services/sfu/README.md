<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `services/sfu` — Lakko's Selective Forwarding Unit

A minimal Rust SFU for Lakko's group calls, channels, and
audio rooms. Speaks Lakko's signaling wire format directly;
forwards SFrame-encrypted RTP between peers; never inspects
media payload.

The strategic rationale lives in
[ADR-0011](../../docs/adr/0011-rust-sfu.md) — "Rust SFU on
`webrtc-rs` / `str0m`, supersedes the LiveKit fork".
[ADR-0003](../../docs/adr/0003-livekit-fork.md) and
[ADR-0008](../../docs/adr/0008-livekit-fork-strategy.md)
described an earlier LiveKit-fork plan that **no longer
applies** — both are marked Superseded.

This README is the entry point for the crate. Read
ADR-0011 first for the "why"; then
[`PLAYBOOK.md`](./PLAYBOOK.md) for build / run / test
commands.

## Status

**Scaffolding only.** No Rust code is checked in yet.
The next sprint pulls the trigger — see
[`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../../docs/ROADMAP-PHASE3-CLOSEOUT.md)
group **B**:

| Sprint | Deliverable |
|---|---|
| **S-3-B-spike** | `webrtc-rs` vs `str0m` echo-test bake-off; decision recorded as an amendment to ADR-0011 |
| **S-3-B-skeleton** | `Cargo.toml` + `src/main.rs`; axum WS signaling endpoint; per-peer registry; echo-test green |
| **S-3-B-routing** | Multi-peer RTP track fanout (A publish → B + C receive) |
| **S-3-B-simulcast** | Simulcast layer extraction (RID / dependency descriptor) + per-receiver layer selection |
| **S-3-B-jwt** | `lakko-call-jwt` admission check — verifies tokens minted by `services/auth` against the JWKS endpoint |
| **S-3-B-twcc** | TWCC aggregation + per-receiver bandwidth estimation; feeds layer switching |
| **S-3-B-metrics** | Prometheus exporter via `metrics` crate; every metric referenced by `infra/prometheus/rules/calls.yml` is exposed |
| **S-3-B-prod** | Config (env-vars per project convention), graceful shutdown, `/health`, `/ready`, structured `tracing` logs |
| **S-3-B-tests** | Integration tests — mock peers, multi-peer fanout, SFrame ciphertext byte-exact passthrough |

## Shape (target)

```text
services/sfu/
├── README.md              ← this file
├── UPSTREAM.md            ← pinned WebRTC stack version + change log
├── PLAYBOOK.md            ← build / run / test / deploy commands
├── Cargo.toml             ← crate manifest (lakko-svc-sfu)
└── src/
    ├── main.rs            ← binary entry; tokio runtime; signal handling
    ├── signaling.rs       ← axum WS endpoint; Lakko wire format
    ├── peer.rs            ← per-peer state (PC, tracks, role)
    ├── room.rs            ← room registry; lifecycle
    ├── fanout.rs          ← incoming track → outgoing-track fanout logic
    ├── simulcast.rs       ← RID / dependency-descriptor parsing; layer pick
    ├── twcc.rs            ← bandwidth estimation aggregator
    ├── jwt.rs             ← lakko-call-jwt verifier wiring
    ├── metrics.rs         ← Prometheus exporter
    └── config.rs          ← env-driven config
```

This is the **end-state** layout — the skeleton sprint
lands `main.rs` + `signaling.rs` + `peer.rs`; the rest
arrive one sprint at a time.

## Crate boundaries

The SFU links directly to Lakko workspace crates — same
as any other service:

| Dep | Why |
|---|---|
| `lakko-call-jwt` | Verify the join JWT minted by `services/auth` |
| `lakko-proto` | Signaling wire types (`schemas/sfu/v1.proto`) |
| `tracing` + `tracing-subscriber` | Structured logging |
| `metrics` + `metrics-exporter-prometheus` | Prometheus surface |
| `axum` + `tokio` + `tokio-tungstenite` | HTTP / WS host |
| `webrtc` (`webrtc-rs`) **or** `str0m` | WebRTC stack — decision in S-3-B-spike |
| `thiserror` | Error types |

No Go. No vendored upstream tree. No patch directory.

## What the SFU does NOT do

These are **architecturally absent**, not "feature gated":

- **No recording.** No code path writes media to disk,
  queues it for an egress process, or hooks a recorder
  side-channel.
- **No RTMP / WHIP ingest.** Broadcasts join via the same
  WebRTC flow as any other participant.
- **No agents framework / webhooks / room dashboards.**
- **No Lakko-specific client SDK.** Clients drive their
  native `RTCPeerConnection` (`WebRTC.framework` on Apple,
  `webrtc-sdk:android` on Android, browser built-in on
  web) and speak the Lakko signaling protocol directly.
- **No plaintext-media fallback.** A peer that fails to
  enable the SFrame transform is refused.

See [ADR-0011](../../docs/adr/0011-rust-sfu.md) §"What the
SFU does NOT do" for the full statement and the
key-disclosure-law context.

## Threat model

The SFU is **assumed-compromised** per
[`docs/security/threat-model.md`](../../docs/security/threat-model.md).
Its surface is intentionally narrow:

- Verify the `lakko-call-jwt` signature against the
  `auth.lakko.me` JWKS.
- Forward SFrame-encrypted RTP between peers.
- Emit Prometheus metrics (counts + bandwidth, never
  plaintext, never participant identifiers — the JWT
  carries an opaque `call_id` and a per-call participant
  pseudonym).

It **cannot** see media keys, plaintext SDP / ICE bodies,
chat-group membership, or participant identity. See
[`docs/security/calls.md`](../../docs/security/calls.md)
§"What the SFU sees vs what the client owns" for the
exhaustive table.

## Cross-links

- [ADR-0011](../../docs/adr/0011-rust-sfu.md) — strategy
- [ADR-0003](../../docs/adr/0003-livekit-fork.md) — superseded; SFrame + MLS-exporter parts still in force
- [ADR-0008](../../docs/adr/0008-livekit-fork-strategy.md) — superseded; auth-JWT shape survives in `crates/lakko-call-jwt`
- [`docs/security/calls.md`](../../docs/security/calls.md) — call-side crypto invariants
- [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../../docs/ROADMAP-PHASE3-CLOSEOUT.md) — sprint sequencing
- [`infra/README-calls.md`](../../infra/README-calls.md) — deployment topology
- [`crates/lakko-call-jwt`](../../crates/lakko-call-jwt) — JWT crate the SFU links
- [`crates/lakko-calls`](../../crates/lakko-calls) — client-side SFrame keying that produces the bytes the SFU forwards opaquely
