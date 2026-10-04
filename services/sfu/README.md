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

## Video FEC (`LAKKO_SFU_VIDEO_FEC`)

Off unless `LAKKO_SFU_VIDEO_FEC=1` ([ADR-0067](../../docs/adr/0067-loss-resilient-call-media.md)
step 4). Off, `red/90000` and `ulpfec/90000` are never offered or answered
and nothing below applies.

On, the SFU negotiates the pair with every peer that offers both. A
libwebrtc publisher then sends every video packet inside a one-byte RED
header, and FEC packets (RFC 5109) on the same SSRC and sequence numbers.

**The SFU is the FEC receiver on the publisher's leg** (`src/fec_receive.rs`,
`MediaPeer::fec_receive`). On that leg it holds every packet byte for byte
as the publisher sent it, so the publisher's FEC is valid there. Per
publisher video stream it keeps a window of the last 256 sequence numbers of
media packets and up to 48 FEC packets; when an FEC packet protects exactly
one packet the SFU does not hold, it rebuilds that packet, registers it with
`str0m` as received (the publisher is no longer sent a NACK for it and it is
not reported lost) and forwards it to subscribers exactly like a packet that
arrived. A packet that arrived as a retransmission is not used as FEC input
(its header extension block is the RTX packet's own).

Per packet toward a subscriber (`src/video_fec.rs`,
`MediaPeer::forward_packet`):

- a RED-wrapped media packet, received or rebuilt, goes RED-wrapped to a
  subscriber whose track negotiated the pair (outer payload type and RED
  header byte in that subscriber's numbers) and unwrapped to any other
  subscriber;
- **a publisher's FEC packet is forwarded to nobody.** It covers the
  publisher's header extension block (RFC 5109 section 7.3; libwebrtc
  `forward_error_correction.cc`), and the SFU writes each leg's own block
  (that leg's `mid`, extension ids, timing of when `mid` is present), so a
  subscriber that used it would rebuild a wrong packet.

**The SFU is the FEC sender on each subscriber's leg** (`src/fec_send.rs`,
`MediaPeer::offer_publisher_fec`, `MediaPeer::send_leg_fec`). A publisher
numbers media and FEC in one sequence-number space and a subscriber's stream
keeps that numbering plus an offset, so each publisher FEC packet leaves one
free number on the subscriber's stream. For a subscriber whose track
negotiated the pair, the SFU reads the mask of the publisher's FEC packet
and builds a new FEC packet over the same media packets **as that subscriber
was sent them**, and sends it at that free number:

- the bytes protected are each packet as `str0m` serialized it for its first
  transmission on that leg, before SRTP (the vendored fork hands them out,
  `StreamTx::keep_sent_rtp` / `DirectApi::take_sent_rtp`): this leg's header
  extension block, with `mid` present or not as it was at that moment, the
  subscriber's sequence numbers and timestamps, payload and padding. They
  are put into the form a libwebrtc receiver feeds its decoder: RED header
  byte removed, the subscriber's media payload type, absolute send time,
  transport-wide sequence number, transmission offset and the tail of
  video-timing zeroed;
- the packet is RFC 5109 with one level; `SN base` is the lowest protected
  number on the leg and the mask is the publisher's, 16 or 48 bits by span.
  How much protection a stream gets is whatever its publisher chose;
- it is written RED-wrapped (outer payload type the subscriber's `red`,
  header byte its `ulpfec`) after every packet it protects has gone on the
  wire, so it follows them. The SFU runs no pacer, so it leaves on the next
  loop pass. It is not kept for retransmission (libwebrtc does not
  retransmit FEC either) and takes a transport-wide sequence number like
  any packet;
- **no FEC packet is sent, and the number stays unused**, when any packet
  the publisher's FEC protected was not sent to that subscriber as a first
  transmission: it never reached the SFU and could not be rebuilt, another
  simulcast layer was on the wire, the stream changed source in between, the
  subscriber was not connected. A request waits until the stream is 64
  sequence numbers past the slot, then is dropped;
- a subscriber without the pair is sent media unwrapped and no FEC, as
  before.

Counters: `lakko_sfu_video_fec_packets_total` (FEC packets received from
publishers), `lakko_sfu_video_fec_recovered_total` (media packets rebuilt
and forwarded) and `lakko_sfu_video_fec_sent_total` (FEC packets computed
for and sent to subscribers). With `LAKKO_SFU_MEDIA_DIAG_SECS` set, the
per-interval publisher line carries `fec_packets=` and `recovered=`, and the
per-interval subscriber fanout line carries `fec_sent=`.

What the SFU reads: the RED header byte, the RTP headers of a video packet
(to find and zero the extensions a sender fills in after computing FEC), and
the FEC header of a publisher's FEC packet. Media payloads are XORed whole
and never interpreted, on the way in and on the way out; they are SFrame
ciphertext before and after.

Cost while on: one extra copy of every packet received from every peer and
of every RED-wrapped video packet sent to a subscriber with the pair
(`str0m` keeps the bytes for the FEC receiver and sender); per publisher
video stream up to 304 held packets of at most 1500 bytes; per subscribed
video track with the pair up to 128.

Known and not solved:

1. **A subscriber still sees a hole wherever the SFU sent no FEC packet**
   (the cases listed above), and asks for that number until it gives up; the
   SFU has nothing to answer with. The same happens when an FEC packet the
   SFU did send is lost: it is not retransmitted.
2. **The SFU adds no protection of its own.** A subscriber's leg gets
   exactly the publisher's FEC rate and grouping, chosen by the publisher
   for the publisher's uplink; a lossy downlink behind a clean uplink gets
   little.
3. **A packet a subscriber got only as a retransmission is useless to its
   FEC decoder** (libwebrtc leaves such packets out, since their header
   extension block is the retransmission's), so FEC and NACK repair do not
   combine within one protected group.
4. **No browser or native client has been run against this.** The receiver
   is tested against an ULPFEC encoder written in the test code from
   RFC 5109 and from reading libwebrtc M125, the sender against a decoder
   written the same way, both end to end with `str0m` test clients. Whether
   Chrome accepts the SFU's FEC packets and whether the SFU matches what
   Chrome sends byte for byte has not been observed.
5. **Simulcast is not tested with FEC.** The code sends no FEC across a
   layer switch; no test publishes simulcast with the pair.

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
