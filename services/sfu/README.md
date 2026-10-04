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

## Bitrate control (`LAKKO_SFU_BITRATE_CONTROL`)

Who decides how much a publisher sends
([ADR-0068](../../docs/adr/0068-server-side-bitrate-control.md)).

| Value | Behaviour |
|---|---|
| unset, empty, `client` | The publisher's own send-side congestion controller, from the transport-wide feedback the SFU returns. Exactly the behaviour before ADR-0068. The default. |
| `server` | The SFU. See below. |
| anything else | The SFU refuses to start. |

The mode and its two figures are logged once at startup.

With `server`:

- **Negotiation.** No SDP the SFU writes (its answers and its own offers)
  carries the transport-wide sequence number header extension or
  `a=rtcp-fb:<pt> transport-cc`, on any media line. `abs-send-time` and
  `goog-remb` stay. This is done through str0m's configuration (the session
  extension map and the configured payload parameters); the vendored crate
  is not changed for it. A client then has no packet feedback and runs no
  delay-based or packet-feedback loss estimator of its own.
- **Estimate.** Per publisher, over the packets of all its streams
  (`src/uplink_estimate.rs`): send-time groups, a trend over their delay
  and an over-use decision (cut to 0.85 of the arriving rate); an 8% a
  second rise while the delay is flat, capped at 1.5 times the highest rate
  that arrived in the last 10 s plus 10 kbit/s and at the maximum; loss
  above 10% lowers the estimate only after it has lasted 3 s, then once a
  second to `1 - loss / 2` of what is being sent, and holds the rise while
  it lasts. The floor is 30 kbit/s. The module reads a send time, an arrival
  time and a size per packet, never a payload.
- **REMB.** For every video receive stream of the publisher, naming that
  stream's SSRC and carrying the whole-path estimate: at once when the
  estimate falls below 97% of the last one sent, no sooner than 200 ms
  after the last when it changed otherwise, and at least every 500 ms. The
  first goes out with the first video packet.
- **Subscriber legs** are unchanged. Note that a subscriber no longer gets
  the transport-wide extension from the SFU either. A libwebrtc receiver
  is then expected to run its receive-side estimator over `abs-send-time`
  and send REMB of its own (not re-read in the libwebrtc source for this
  change, and not seen on a call); a REMB from a subscriber reaches the
  simulcast layer policy as `Event::EgressBitrateEstimate`.

| Variable | Default | Meaning |
|---|---|---|
| `LAKKO_SFU_BITRATE_START_KBPS` | `1500` | Where a publisher's estimate starts. At least 30, at most the maximum. |
| `LAKKO_SFU_BITRATE_MAX_KBPS` | `5000` | The estimate never exceeds this. |

Both are checked in either mode. Both defaults are chosen, not measured.

**Diagnostics.** With `LAKKO_SFU_MEDIA_DIAG_SECS` set, one line per
publisher per interval:
`media: publisher bitrate estimate over the last interval` with
`estimate_kbps`, `arriving_kbps`, `state` (`normal` / `overuse` /
`underuse`), `last_change` (`start` / `increase` / `overuse` / `loss`) and
`retransmitted`. The counter `lakko_sfu_bitrate_remb_sent_total` counts
REMB rounds.

### What the publisher does with it (read from libwebrtc M125, not seen on a call)

- Its target is `min(its own estimate, REMB)`
  (`SendSideBandwidthEstimation::target_rate`). REMB is a ceiling.
- Until two seconds after its first receiver report, and only while no
  loss has been reported, it also *raises* its own estimate to the REMB
  (`UpdateEstimate`, the `IsInStartPhase` branch). That is the fast start:
  from its 300 kbit/s start value to the SFU's start value.
- After that its own estimate moves by the receiver-report loss rule alone:
  up 8% a second while reported loss is at most 2%, unchanged between 2%
  and 10%, and above 10% multiplied by `1 - loss / 2` at most once per
  report and per 300 ms plus the round-trip time. The SFU's receiver
  reports are the RFC 3550 figure, once a second for video.

### Known limits

1. **A loss burst still costs the publisher seconds, through its own
   receiver-report rule, which the SFU does not control.** One report with
   50% loss takes the sender's own estimate to 0.75 and it needs about 4 s
   at 8% a second to return; one with 95% takes it to about 0.53, about
   8 s; a burst that straddles two reports is cut twice. The SFU's estimate
   does not move in that case (pinned by the unit tests), so the ceiling
   stays where it was and the sender climbs back on its own. Whether this is
   what a live call shows has not been measured.
2. **No browser or native client has been run against this.** The
   end-to-end tests use `str0m` clients, which answer the negotiation and
   surface the REMB but do not act on it. That Chrome, Safari, Firefox and
   the two native clients send `abs-send-time` and follow REMB with this
   negotiation is read from the libwebrtc source, for M125 only; Firefox
   was not read.
3. **A retransmission cannot be told from a reordered packet** by the SFU
   (str0m hands both over as a packet behind the highest sequence number),
   so reordering is counted as loss.
4. **The sender's probe clusters are not used.** A libwebrtc sender still
   sends its start-up probes (their results need transport feedback and
   never come back); libwebrtc's own receive-side estimator reads them to
   set its first estimate, this one does not.
5. **A queue that is held and released** raises the arriving rate for a
   moment, and with it the 1.5x cap; the configured maximum is the bound.

## Two-track delivery of simulcast video (the `~light` contract)

Applies only to a publisher that sends a video track as simulcast layers
(more than one rid). The Lakko web client sends two: rid `q` (the light
one, listed first) and rid `f` (the full one). Audio and a video track
with a single encoding are delivered as before, one downstream track each,
and nothing here touches them.

**Why.** A home uplink that loses 40 to 75% of its datagrams for about a
second does not repair a stream of about 250 packets a second inside that
second; a stream of 25 to 50 packets a second is repaired in a few hundred
milliseconds (`measure_uplink_dip`). Moving a subscriber's one track to the
light layer during the dip (the uplink fallback, below) was measured not to
shorten the stall, because the switch needs a light key frame to get
through the same dip. So the subscriber receives **both layers all the
time, as two tracks**, decodes both, and shows the light one the moment the
full one stands still. No key frame is needed at that moment.

**The contract.** For one simulcast video track of a publisher, every
subscriber is offered two video media lines (one SFU offer carries both):

| Track | `a=msid` stream id | Carries |
|---|---|---|
| main | the source peer id, as for every other track | the heaviest layer the subscriber-bandwidth policy allows |
| light companion | the source peer id followed by the literal `~light` (`media::LIGHT_STREAM_SUFFIX`) | the light layer only, for as long as the track lives; it never switches |

As it appears in an offer (the second token is the track id, random per
track):

```
a=msid:p000000000000000518db68c125428388 xXlpmH6sL9eXgEBa3nab
a=msid:p000000000000000518db68c125428388~light 01dTBrF6miaQ1z4I88hS
```

A client keys a main track by the stream id as before and pairs the
companion with it by stripping the suffix. A client that does not know the
contract sees a second video track under a stream id that matches no
participant.

**Which layer is which** (`MediaPeer::simulcast_layout`, on the publisher).
The light layer is rid `q` when the track has one; otherwise the layer with
the lowest measured bitrate, once every layer has been measured, and it is
fixed from then on. The heaviest layer is, with exactly two layers, the one
that is not the light one; with more, the one with the highest measured
bitrate. A track is known to be simulcast from its negotiation when that
lists the rids (`a=rid` / `a=simulcast`), otherwise from the moment a second
rid arrives on it; in the second case the companion is opened then, by a
further offer, and the main track that is already open is left alone.

**The main track** is put on the heaviest layer before its first packet when
that layer is known, instead of latching whichever layer arrives first. A
main track that did latch another layer (the layers were not known yet) is
moved to the heaviest by the default-layer rule of `drive_uplink_fallback`,
at a whole key frame. The subscriber-bandwidth policy
(`drive_layer_selection`) may still lower it; the companion is exempt from
that policy, from `select_layer` and from the uplink fallback.

**Each track begins at a whole key frame of its layer.** The packets of the
selected layer are held (`SwitchWait`, at most 64 packets and one second)
until a key frame is whole at the SFU, and the publisher is asked for one on
that layer (`reason` `track-start`) the moment the track is open, then once a
second until the track has begun. The request goes through the per-track
rate limit; inside the window it is owed and goes out when the window ends.
After five requests without a whole key frame the track stops waiting and
begins at the next packet of its layer (a stream without frame descriptors
never shows the SFU a key frame).

**Key frame requests, retransmission, FEC.** A subscriber's PLI or FIR on
the companion is relayed to the publisher for the light layer, on the main
track for the layer it is selected to. A NACK on either is answered from
`str0m`'s send buffer for that track. With `LAKKO_SFU_VIDEO_FEC=1` each
track has its own FEC state: the publisher's FEC packets for a layer make
the SFU send its own for the track that layer is on.

**Each track is its own RTP stream**: its own SSRC, its own sequence-number
and timestamp map (`src/stream_map.rs`), its own key frame bookkeeping.
Payloads are forwarded verbatim on both (C-39).

**Visible.**

- `media: track announced by peer` carries `simulcast`; a track found to be
  simulcast later logs `media: track found to be simulcast from its packets`.
- Each key frame request: `media: keyframe requested from publisher` with
  `reason` `track-start`.
- With `LAKKO_SFU_MEDIA_DIAG_SECS` set, the per-interval line `media: fanout
  to subscriber over the last interval` carries `role` (`main` / `light`):
  one line per downstream track, with the packets `sent` on each. A packet
  of another layer is not an attempt on the companion; on the main track it
  is counted `wrong_layer` as before.
- `lakko_sfu_media_light_companion_packets_forwarded_total`: packets sent on
  companion tracks. Each is also in
  `lakko_sfu_media_frames_forwarded_total`.

### Measured (`measure_two_track_dip`)

The publisher and the dip of the uplink-fallback measurement below: `q` at
50 packets a second, `f` at 250, 25 frames a second, and for one second 70%
of the publisher's media datagrams lost, both layers and retransmissions.
The SFU runs with its defaults (no uplink fallback). A frame is decodable
when all its packets have arrived and the frame before it on its layer was
decodable, starting over at a key frame. (a) is the longest interval in
which the main track completes no decodable frame, (b) the longest in which
neither track does: what a client that shows whichever track is alive would
see. Twenty runs on a laptop, seeds 1 to 20 (two invocations of ten),
2026-10-04, milliseconds:

| Seed | Lost / sent in the window | (a) main | light alone | (b) either | b / a |
|---|---|---|---|---|---|
| 1 | 644 / 893 | 525 | 739 | 391 | 0.74 |
| 2 | 559 / 821 | 502 | 542 | 502 | 1.00 |
| 3 | 631 / 883 | 519 | 279 | 211 | 0.41 |
| 4 | 567 / 831 | 480 | 506 | 479 | 1.00 |
| 5 | 630 / 883 | 322 | 630 | 322 | 1.00 |
| 6 | 573 / 829 | 387 | 344 | 245 | 0.63 |
| 7 | 619 / 870 | 652 | 311 | 243 | 0.37 |
| 8 | 605 / 845 | 543 | 373 | 368 | 0.68 |
| 9 | 581 / 830 | 413 | 331 | 275 | 0.67 |
| 10 | 559 / 819 | 831 | 304 | 280 | 0.34 |
| 11 | 605 / 849 | 409 | 486 | 302 | 0.74 |
| 12 | 646 / 909 | 1035 | 797 | 792 | 0.76 |
| 13 | 532 / 800 | 326 | 612 | 325 | 1.00 |
| 14 | 610 / 864 | 970 | 1090 | 970 | 1.00 |
| 15 | 619 / 887 | 310 | 488 | 248 | 0.80 |
| 16 | 596 / 853 | 603 | 353 | 327 | 0.54 |
| 17 | 672 / 925 | 428 | 421 | 293 | 0.68 |
| 18 | 587 / 850 | 598 | 520 | 318 | 0.53 |
| 19 | 617 / 860 | 655 | 422 | 344 | 0.52 |
| 20 | 639 / 891 | 723 | 569 | 400 | 0.55 |

Over the twenty: the median of (a) is 522 ms and of (b) 324 ms; the longest
(a) is 1035 ms and the longest (b) 970 ms.

What the runs show:

- The displayed stall is shorter with the two tracks in fifteen of the
  twenty runs, by 20 to 66%; its median falls from 522 to 324 ms.
- **In five of the twenty (2, 4, 5, 13, 14) the companion bought nothing**:
  the light stream had a hole of its own at least as long over the same
  stretch. In run 14 both stood still for about a second. The companion is
  50 packets a second through the same loss, not a stream that is immune to
  it: its own longest stall was 279 to 1090 ms, and in eight of the twenty
  longer than the main track's.
- (b) is never longer than (a): a client loses nothing by having the second
  track, other than the bitrate of the light layer on its downlink.

Not measured: what a browser or a native decoder does with the two tracks
(the subscriber is a `str0m` client in RTP mode; "decodable" is computed
from the packets that arrived), how fast a client notices that the main
track stands still, and anything with a real encoder's key frames.

## Uplink fallback (simulcast publishers, off by default)

**Off unless `LAKKO_SFU_UPLINK_FALLBACK=1`** (unset, empty or `0`: off;
anything else refuses to load). It costs resolution and was measured not to
shorten the stall (below); a subscriber has the light layer all the time on
the light companion track instead (above). It is kept as an opt-in. When on
it acts on **main tracks only**: a light companion never changes layer.
`LAKKO_SFU_NO_UPLINK_FALLBACK`, the switch of the build in which it ran by
default, is gone and is not read.

Applies only to a publisher that sends a video track as simulcast layers.
The Lakko web client sends two: rid `q` (180p, about 150 kbit/s, the light
one, listed first) and rid `f` (the full one). A publisher that sends one
stream has no rid on its packets and none of this touches it.

**Why it was built.** Measured on live calls and reproduced by `measure_uplink_dip`: a
home uplink that loses 40 to 75% of its datagrams for about a second cannot
repair a stream of about 250 packets a second inside that second; a stream
of 25 to 50 packets a second is repaired in a few hundred milliseconds in
the same dip.

**The policy** (`src/uplink_fallback.rs`, pure bookkeeping, one instance
per simulcast video track of a publisher). It is fed the layer, extended
sequence number and arrival time of every packet received on the track
(retransmissions and packets rebuilt from FEC included) and keeps per layer
the holes: numbers that were skipped and have not arrived. A layer is in
trouble while it has a hole older than `HOLE_AGE`.

| State | Subscribers may be on | Leaves when |
|---|---|---|
| `full` | any layer | a layer other than the light one is in trouble |
| `fallback` | the light layer only | no other layer has been in trouble for `RESTORE_AFTER`, and `MIN_FALLBACK` has passed since it began |

| Constant | Value | |
|---|---|---|
| `HOLE_AGE` | 150 ms | how old a hole must be |
| `RESTORE_AFTER` | 3 s | clean time before going back |
| `MIN_FALLBACK` | 2 s | the least a fallback lasts |
| `HOLE_FORGET` | 2 s | a hole not filled by then is no longer waited for |
| `MAX_HOLES` / `MAX_JUMP` / `MAX_LAYERS` | 1024 / 1024 / 4 | bounds; a larger jump is a stream that started over |

All chosen, none measured. The light layer is the one with the lowest
bitrate (by the highest rate each layer has shown, so a rate that collapses
during a dip does not change the answer), or rid `q` until every layer has
been measured; it is fixed for as long as a fallback lasts. Holes on the light layer change
nothing.

**What the SFU does with it** (`media.rs`, `drive_uplink_fallback`, every
pass of the media loop, before the subscriber-bandwidth policy):

- *To fallback:* every subscriber's main track of the track that is selected
  to another layer is selected to the light one, and the publisher is asked for a key
  frame on the light layer at once, outside the keyframe rate limit. While
  a subscriber's switch has not happened the request is repeated once a
  second.
- *To full:* each subscriber that was held goes to what the
  subscriber-bandwidth policy wants (with an estimate for the subscriber,
  the best layer that fits, starting from the layer it had before; without
  one, the heaviest layer), with a key frame request on that layer.
- The two policies combine as the lower of the two: `drive_layer_selection`
  leaves a track in fallback alone.

**When a switch happens** (`SwitchWait` in `media.rs`; this applies to
every layer switch, not only the fallback's). A track switches at a key
frame of the selected layer once every packet of that key frame has reached
the SFU; until then the layer on the wire keeps being forwarded, and the
selected layer's packets are held (at most 64 per subscriber track, at most
one second). The key frame then goes out in stream order with whatever
arrived behind it. A decoder cannot show a key frame before its last packet
arrives, so holding the earlier ones delays nothing a viewer sees. Before
this the switch happened at the key frame's first packet: on a lossy uplink
that packet is often resent and arrives after the rest of its frame, which
had then been dropped, and the subscriber's new stream began with a key
frame it could never complete. A key frame of more than 64 packets is not
waited for whole: the switch happens at its first packet once 64 are held.
What is read is header fields only (sequence number, timestamp, marker bit,
the frame descriptor's independent-frame flag); payloads are held and sent,
never read.

**A subscriber without a bandwidth estimate is put on the heaviest layer.**
Send-side estimation is off in production (see `MediaPeer::with_bitrate_control`),
so `drive_layer_selection` never acts. A main track whose layers were not
known when its first packet arrived latched whichever layer reached the SFU
first; for a publisher that lists `q` first that is `q`, and nothing would
ever move it. So while a track is `full`, a main track of a subscriber with
no estimate that sits on another layer than the heaviest is selected to the
heaviest (known at once for the two layers `q` and one other, otherwise once
every layer has been measured, about a second into the stream). This runs
with the fallback switched off as well, which is the default.

**Visible.**

- A log line per change of state: `media: publisher uplink fallback changed
  state` with `origin`, `mid`, `from`, `to`, `light`, `reason`
  (`hole-older-than-limit` / `uplink-clean`) and `oldest_hole_ms`.
- Each key frame request: `media: keyframe requested from publisher` with
  `reason` `uplink-fallback`, `uplink-restore` or `full-layer-default`.
- `lakko_sfu_uplink_fallback_switches_total{direction}` (`fallback` /
  `full`), one per publisher track per change.
- With `LAKKO_SFU_MEDIA_DIAG_SECS` set, the per-interval line `media:
  publisher stream over the last interval` carries `uplink` (`full`,
  `fallback`, or `not-simulcast`).

**Off (the default).** No track has a policy and none is ever held on its
light layer; the heaviest-layer default and the whole-key-frame switch
stay.

### Measured (`the_uplink_fallback_carries_a_subscriber_through_a_dip`)

A `str0m` publisher sends `q` at 50 packets a second (2 packets a frame)
and `f` at 250 (10 a frame), 25 frames a second; a key frame is three times
a delta frame; it answers a key frame request for a layer with a key frame
on that layer. For one second 70% of its media datagrams are lost, both
layers and retransmissions. The subscriber counts a frame as showable when
all its packets have arrived and the frame before it on that layer was
showable, starting over at each key frame. The stall is the longest gap
between two showable frames, on the subscriber's one track of that build
(now its main track). Each line is one run with the fallback and one
without, same seed (the generator is drawn per datagram, so the two runs do
not lose the same packets).

Six runs on a laptop, seeds 1 to 6, 2026-10-04 (milliseconds; "frozen" is
the time the picture stood still in the two seconds from the start of the
dip, "late" how long the latest frame took from written to showable, "on
q" when the subscriber was switched, from the start of the dip):

| Seed | Stall with / without | Frozen with / without | Late with / without | On q at |
|---|---|---|---|---|
| 1 | 608 / 589 | 947 / 925 | 802 / 864 | 1024 |
| 2 | 382 / 667 | 881 / 880 | 690 / 776 | 369 |
| 3 | 310 / 399 | 800 / 844 | 693 / 851 | 834 |
| 4 | 465 / 569 | 781 / 860 | 529 / 787 | 632 |
| 5 | 390 / 540 | 764 / 969 | 433 / 893 | 383 |
| 6 | 582 / 841 | 910 / 801 | 599 / 810 | 340 |

Five earlier runs of the same switching rule gave stalls of 565, 546, 618,
549, 583 with and 599, 619, 476, 376, 328 without. Over the eleven the
median stall is 549 ms with and 569 ms without.

What the runs show:

- **The fallback is not shown to shorten the stall.** Over the eleven runs
  the longest gap is the same with and without, within the spread between
  runs; in the six tabled it was shorter in five, in the five before them
  in two.
- **Why.** The switch needs a key frame on the light layer, and that key
  frame crosses the same uplink. Six packets, each lost with probability
  0.7 at every attempt, took about 150 to 850 ms from the request to be whole;
  by then most of the one-second dip is over. In the runs where it was
  whole early, the light layer's own frames (two packets each) then stalled
  on their own repairs.
- **What it costs.** After each dip the subscriber is on the 180p layer for
  about three seconds (`RESTORE_AFTER`).
- **What moved.** In nine of the eleven runs the latest frame was less late
  with the fallback (by 60 to 460 ms in the six tabled): the chain starts
  over at the key frame and the backlog on the full layer is skipped.
- With the switch at the key frame's first packet (the rule before
  `SwitchWait`) three runs gave stalls of 1031, 1019 and 1014 ms with the
  fallback against 743, 392 and 480 ms without: the light stream began
  with a key frame whose other packets had been dropped.

### Known limits

1. **Not shown to help**, which is why it is off by default. See above. A
   light layer the subscriber can switch to without a key frame through the
   dip is what the two-track delivery now gives it; a faster repair of the
   key frame is not built. The interval at
   which the SFU repeats a request for a missing packet is `str0m`'s and was
   not changed.
2. **No browser or native client has been run against this.** The test
   publisher is `str0m` in RTP mode with no cap on retransmitted bytes; how
   large a browser's `q` key frame is, how fast it answers a retransmission
   request, and whether a key frame request for one simulcast layer makes
   it key-frame that layer alone were not observed.
3. **One old hole is enough.** A publisher whose round trip plus the SFU's
   retransmission request takes longer than `HOLE_AGE` falls back on every
   lost packet and, with losses less than `RESTORE_AFTER` apart, stays
   there.
4. **Three or more layers**: the fallback goes to the lightest, whichever
   heavier layer is in trouble, and the restore without an estimate goes to
   the heaviest.
5. **Not tested with video FEC.** The publisher's FEC packets are counted
   as received sequence numbers and are stepped over when a key frame is
   checked for wholeness; no test publishes simulcast with FEC.

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
