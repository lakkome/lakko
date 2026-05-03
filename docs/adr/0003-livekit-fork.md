<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0003: LiveKit fork as the SFU; SFrame for media

- **Status:** Accepted
- **Date:** 2026-04-28
- **Context:** Phase 0 (decision pinned in advance, implementation lands in Phase 3)

## Context

Lakko must support calls of a quality comparable to Telegram / Zoom / Yandex Telemost: P2P 1×1, group (up to dozens of participants), broadcasts (millions of viewers), audio rooms (a few speakers + audience). Every call is E2EE-protected: the server only sees encrypted frames.

We need a production-grade Selective Forwarding Unit with simulcast / SVC and integration of E2EE keys derived from MLS.

## Decision

### SFU — fork of [LiveKit](https://github.com/livekit/livekit) (Go, Apache 2.0 → forked under AGPLv3)

Used as a base; we fork it under AGPLv3 into `services/sfu/`. We modify it to:

- Bind SFrame keys to an MLS exporter (instead of LiveKit's out-of-band keys)
- Use our auth / handshake protocol (over QUIC or WebTransport)
- Support a cascade topology for broadcasts (Phase 4)

### Media encryption — SFrame (RFC 9605)

Per-frame AES-128-GCM with a counter-derived nonce. Hardware AES acceleration (AES-NI / ARMv8 Crypto Extensions) — overhead < 0.001% CPU at 60fps@1080p.

Keys are derived once at the start of a call via `mls_exporter("lakko/call/v1", call_id, 32)` → HKDF → per-participant SFrame keys keyed by leaf index. Rotation on Add/Remove with a 1-second overlap (the old key remains accepted briefly).

### Call topologies

| Type | Topology | MLS group | Media keys |
|---|---|---|---|
| P2P 1×1 | Pure WebRTC + ICE/STUN/TURN | Existing 1×1 group | exporter from it |
| Group via link | LiveKit SFU | Ad-hoc group, join via embedded one-time KeyPackage | exporter call group |
| Group inside chat | LiveKit SFU | Reuse the existing chat MLS group | `exporter("lakko/call/{id}", idx, 32)` |
| Broadcast | Origin SFU + cascade onto edge SFUs | PublisherGroup (presenter) + sender-key for audience | content key, rotation on membership change |
| Audio room | LiveKit SFU | `Speakers` MLS subgroup + `Audience` (sender-key) | promotion = MLS Add = key rotation |

### Codecs

- **Audio:** Opus 32-64 kbps + DTX + FEC
- **Video:** AV1-SVC (modern), VP9-SVC (fallback), H.264 (Apple legacy)

### Quality budget

- Audio p50 < 80 ms regional / < 200 ms cross-continental, p95 < 250 ms
- Video 30 fps @ 720p baseline, 60 fps @ 1080p when bandwidth allows
- Adaptive jitter buffer 20-120 ms (NetEQ-style)
- SVC layer drop at the SFU based on receiver BWE reports

## Alternatives

### mediasoup

A mature Node.js SFU. Works well, but a Node.js-rooted ecosystem does not match our Rust / Go stack; integrating it into our auth / control plane would require permanent bridges.

**Rejected:** in the long run, a heterogeneous backend stack costs more than forking a single component.

### Janus / Jitsi Videobridge

Mature SFUs. Cons: C (Janus) or Java (Jitsi); the architecture is built around Jitsi Meet's room model, our broadcast topologies would require substantial changes.

**Rejected:** less aligned with our call model than LiveKit.

### A custom SFU written from scratch in Rust

Design freedom. Cons: ~2 person-years to production grade; simple E2EE via SFrame does not justify writing our own SFU.

**Rejected:** not our core differentiator. Forking LiveKit and replacing the E2EE plumbing with MLS takes several person-months.

### Use LiveKit as a library, without forking

LiveKit Cloud SaaS or self-host upstream. Cons: their licence (Apache 2.0) does not protect against a proprietary fork; their E2EE plumbing is not tied to MLS.

**Rejected:** an AGPLv3 fork is needed for licence consistency (see ADR-0006); MLS integration requires our changes anyway.

## Consequences

### Positive

- Production-grade SFU with simulcast / SVC, ICE / TURN, network resilience — courtesy of upstream LiveKit code
- E2EE via the SFrame standard (RFC 9605) — interop is possible with other SFrame-compatible clients
- MLS exporter → SFrame keys — unified identity and group management between chat and call
- Hardware AES → near-zero CPU cost for crypto during a call
- Cascade-friendly architecture for broadcast scaling

### Negative

- A fork has to track upstream changes (cherry-pick fixes) or accept divergence
- Go is added to the backend stack (LiveKit) alongside Rust → two toolchains in CI
- Our AGPL fork of the SFU cannot consume proprietary LiveKit Cloud features

### Neutral

- Extra latency from SFU forwarding (5-15 ms) is acceptable for every call type

## Open questions

- Will we (Phase 3+) simplify to single-codec AV1 everywhere, or will the H.264 fallback for Apple stay forever?
- Call recording — how should it work with E2EE? Open question for Phase 4
- TURN-as-a-Service vs self-hosted coturn — Phase 3 will decide

## References

- [LiveKit](https://github.com/livekit/livekit)
- [RFC 9605 — Secure Frame (SFrame)](https://www.rfc-editor.org/rfc/rfc9605)
- ADR-0001 — MLS exporter for media keys
- ADR-0006 — licensing (explains why a fork is required)
