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

**State on 2026-10-11.** The crate is built and deployed to the test
stack: signaling, JWT admission, RTP packet forwarding
([ADR-0066](../../docs/adr/0066-sfu-forwards-rtp-packets.md)), and the
call-media work of ADR-0067, ADR-0068 and
[ADR-0070](../../docs/adr/0070-call-media-through-a-loss-burst.md)
described section by section below. The WebRTC stack is `str0m`
`=0.19.0`, a vendored fork with fifteen recorded changes
([`UPSTREAM.md`](./UPSTREAM.md)); the signaling frames come from
`crates/lakko-sfu-proto`, not `lakko-proto`. It has carried live calls
between browsers (Chrome, Firefox, Safari, Edge). The iOS and Android
clients were brought to the current protocol on 2026-10-11 and have not
been on a call against it.

The rest of this section, "Shape (target)" and "Crate boundaries" are
the plan written before any code existed and are kept as history: the
sprint table, the file layout (the code is in `src/media.rs`,
`signaling.rs`, `router.rs` and the modules named in the sections
below) and the lines "No vendored upstream tree. No patch directory."
no longer describe the crate.

**As first written: scaffolding only.** No Rust code is checked in yet.
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
- **No audio level.** The RTP header extension
  `urn:ietf:params:rtp-hdrext:ssrc-audio-level` (RFC 6464)
  is negotiated with nobody; see "Header extensions" below.

## Header extensions

A header extension is outside the SFrame seal, so each one
the SFU negotiates is something the SFU can read. The
session extension map of every peer is built by
`session_extension_map` in `src/media.rs`, never taken from
str0m's default:

| Extension | Negotiated | Use |
|---|---|---|
| `sdes:mid`, `sdes:rtp-stream-id`, `sdes:repaired-rtp-stream-id` | always | routing a packet to its media line and layer; set per leg |
| `abs-send-time` | always | the SFU's own path estimates |
| `transport-wide-cc` | not under server-side bitrate control without downlink control | see "Bitrate control" |
| `urn:3gpp:video-orientation` | always (video) | relayed |
| `generic-frame-descriptor-00` | always (video) | relayed verbatim; where a key frame starts (`src/frame_descriptor.rs`) |
| `playout-delay` | unless `LAKKO_SFU_PLAYOUT_DELAY=0` (video) | stamped by the SFU |
| `ssrc-audio-level` (RFC 6464) | **never** | none |

**The audio level is never negotiated (2026-10-12).** It
would tell the server, in the clear and on every audio
packet, who is speaking and how loudly, which
`docs/ux/group-call-grid.md` section 10 forbids. str0m's
default map has it on id 1; until this date the SFU kept
that default, accepted the extension in its answer to a
publisher, offered it to every subscriber and copied the
value from packet to packet. It was found in the SFU's own
log of an offered downstream track
(`[audio] 1:ssrc-audio-level ...`). Now:

- the answer to a publisher does not echo the extension, so
  a publisher does not send it;
- the SFU's offers to subscribers do not name it;
- a value a client sends anyway is under an id that maps to
  nothing on the SFU: it is not parsed, a packet in transit
  (`RelayPacket`) has no field for it, and the outgoing
  packet's extension values never set it;
- nothing in the SFU ever used the level: no active-speaker
  rule, no forwarding decision, no metric, no log line. The
  only use was the relay itself.

Each client measures the audio it decoded instead (web: an
analyser on the played stream and the receiver's audio
energy; iOS and Android: receiver statistics).

Tests: `no_session_extension_map_holds_the_audio_level`,
`the_answer_to_a_publisher_does_not_accept_the_audio_level`,
`the_offer_to_a_subscriber_does_not_carry_the_audio_level`
and
`egress_extensions_carry_the_streams_own_values_and_nothing_of_the_leg`
(unit, over every way a peer is configured); and in
`tests/synthetic_client_e2e.rs`
`the_audio_level_is_negotiated_with_neither_publisher_nor_subscriber`
and `an_audio_level_sent_anyway_reaches_no_subscriber`, the
second with a publisher that puts the extension on the wire
although the answer did not accept it. Not verified: a
browser or a native client against this SFU. That an
offerer does not send an extension the answer left out is
the offer/answer rule (RFC 8285 section 6, recalled); it
was observed here for the str0m test client only.

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
  change, and not seen on a call). `str0m` hands a subscriber's REMB over
  as `Event::EgressBitrateEstimate`; the SFU does not take it for the
  subscriber's budget in the simulcast layer policy, because it follows
  what the subscriber is being sent (see "A subscriber's REMB is not a
  budget" below).

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

### A cut needs a queue (2026-10-07)

On a live call a publisher on a mobile network was walked down by cuts
for "over-use": 503, 191, 53 kbit/s within fifteen seconds
(07:46:06 to 07:46:16 in the log), on a path that had carried
2.7 Mbit/s in an earlier call, with a loss of 0 to 0.8% in the SFU's own
ingress lines and no cut for loss in the excerpt. Every lowering in the
log is the delay rule (`last_change="overuse"`); the round-trip time the
SFU saw stood at 40 to 96 ms in most intervals and at 388 and 202 ms in
the two where the deepest cuts were made. What the log cannot show is
the queue on the path at each cut: no line said it. It does now.

The rule that cut is the delay detector's (`uplink_estimate.rs`,
`PathEstimator::decrease`): the trend of delay over some twenty groups
of packets crosses its threshold, and the estimate is put at 0.85 of
what arrived in the last half second. Each cut makes the publisher send
less, so less arrives, so the next cut starts lower. In the estimator's
own model a path with a mobile network's jitter (delay climbing by 20 to
60 ms over a tenth of a second and falling back, twice a second), no
limit on the rate and no loss was cut 34 times in a minute, down to
326 kbit/s from 2 Mbit/s
(`jitter_without_a_queue_cuts_nothing`, the second half).

What a publisher's uplink runs on now (`PathEstimator::confirming`):

- **A cut for over-use needs a standing queue**: the smallest delay of
  the packets of the last 200 ms at least 25 ms above the smallest the
  path has shown in the last 15 to 30 s (`STANDING_QUEUE`), for 300 ms
  (`QUEUE_STANDS_FOR`). Delay is arrival time less the packet's
  abs-send-time. The smallest of a window, not its mean: jitter raises
  some packets, a queue raises all of them.
- **The detector's word alone is doubted**: no cut is made, and it is
  counted. If a queue then goes on standing, the cut is made; a queue
  75 ms deep (`DEEP_QUEUE`) that has stood 300 ms is cut on without the
  detector.
- **After a cut the queue has to stand as long again** before the next.
  A cut is therefore never taken from the rate the cut before it imposed
  unless the path is still congested at that rate; and what arrives
  through a standing queue is what the path delivers, so the figure a
  cut is taken from is not one a pause in the sending made small.
- **The way back** is as before: to the estimate held before the first
  cut of an episode at four times a second (`RESTORE_PER_SECOND`), so a
  cut to 0.85 is undone in under 200 ms of quiet and a cut to a quarter
  in a second; a path that is cut again within ten seconds has shown it
  is narrower and is climbed at 8%, then 25%, then 50% a second.
- **Loss is as before**: a burst changes nothing, loss that lasts 3 s
  steps the estimate down once a second, with or without a queue.

The queue watch is one type, `uplink_estimate::QueueWatch`, used by the
uplink and by a subscriber's downlink (step 6 below), which adds its own
model of the SFU's sending on top. All constants chosen, not measured.

Visible: `media: a publisher's bitrate estimate was cut for over-use`
(`rule` = `detector` or `standing-queue`, `from_kbps`, `to_kbps`,
`arriving_kbps`, `queue_ms`, `trend`, `threshold`, `confirmed`), one
line per cut; and on the interval line `queue_ms` and `doubted` (how
often the detector asked for a cut with no queue standing, since the
publisher joined).

Tests (`uplink_estimate.rs`): `jitter_without_a_queue_cuts_nothing`
(a minute of that jitter: no cut, the estimate never under its start
value, the detector doubted; the same path cuts the old rule 34 times);
`a_path_that_narrows_is_cut_and_comes_back_when_it_widens` (2 Mbit/s
into a path narrowed to 800 kbit/s: at or under 800 within 3 s, between
400 and 1000 while it lasts, every cut with a queue of 25 ms or more;
widened again: 2 Mbit/s within 15 s);
`a_stall_on_a_jittery_path_does_not_start_a_walk_down` (half a second
of nothing delivered and then everything at once: at most one cut, at
the start value or above from 2.5 s after);
`the_loss_rules_do_not_depend_on_a_queue`;
`the_queue_watch_reads_the_smallest_recent_delay`. And in
`synthetic_client_e2e.rs`, `a_jittery_uplink_with_no_limit_is_not_cut`:
a publisher sending 1.2 Mbit/s for 30 s through that jitter; no REMB
may be under 1.2 Mbit/s. Seen: 62 REMBs, lowest 1.5 Mbit/s (the start
value), highest 2.07; the detector asked for a cut 35 times and was
doubted each time.

**Second live call with the rule, same day, and two defects in it.**
For the two minutes the mobile publisher was alone on its link its
estimate held at 1.8 to 2.8 Mbit/s. Then:

1. *Cuts without a queue.* 21 of the 51 cut lines of that call carry
   `queue_ms=None`. Cause: after a cut the code re-armed the wait by
   writing "a queue has stood since now", whether one stood or not, so
   0.3 s later the detector's next word passed for confirmed; and a
   queue reading that had become "too few packets to tell" left an old
   "standing since" in place. Now the decision reads the queue at the
   moment it is made (`update_rate_by_the_queue`): a cut needs a reading
   of 25 ms or more now, standing 300 ms, and 300 ms since the last cut;
   with no reading nothing is cut. The line's `queue_ms` is that
   reading. The other 30 cuts had queues of 29 to 296 ms: two
   publishers were sharing the mobile uplink by then and it was full.
2. *No way back.* The same publisher then sat at 291 kbit/s for minutes
   with a queue reading of 8 ms. Its detector went from over-use to
   under-use and back about twice a second (`doubted` rose by about ten
   per five-second interval), and in the rule taken from libwebrtc
   either state stops the rise: over-use asks for a cut, under-use
   holds. With no queue standing neither does now: the estimate rises
   whatever the detector says. While a queue stands nothing rises.

A cut is to 0.85 of what is **arriving**, not of the estimate: the line
"from 2800 to 1854, arriving 2181" is that rule, and it is meant. A
queue that stands while 2181 kbit/s arrives says the path carries no
more than that, whatever the estimate was.

**Two publishers behind one bottleneck** (the two phones on one mobile
link) are not treated as one path: each has its own estimate, each sees
the queue the two build together, and each is cut to 0.85 of its own
arriving rate, so whoever is sending more keeps more. In that call the
Firefox publisher held 2.0 to 4.4 Mbit/s and the Chrome one 0.3. The
log does not show Firefox ignoring its REMB: what arrived from it stayed
within about a quarter of its estimate through cuts and rises alike
(estimate 2206 cut to 1221, then 1437 arriving; 4339 with 4442
arriving). The Chrome publisher lost because it could not climb (defect
2), not because the other would not yield. Sharing a bottleneck fairly
would need the SFU to know that two publishers share one (same remote
address, or queues that rise and fall together) and to divide one
estimate between them; nothing of that is built, and the evidence so
far asks only for defect 2 to be gone.

Not known: what a real mobile uplink's delay looks like between the
smallest and the rest (a path that holds every packet 25 ms longer for
half a second without being congested would be cut on; one whose queue
stays under 25 ms would not be, and is left to loss); and whether the
388 ms round trip of the live call was a queue a cut should answer.
The next call's log says, per cut.

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

## Three-track delivery of the camera (the `~light` and `~mid` contract)

**What a subscriber gets.** For one publisher's camera, every subscriber is
offered up to three video media lines (ADR-0070 steps 1 and 5):

| Track | `a=msid` stream id | Carries | Forwarded |
|---|---|---|---|
| main | the source peer id, as for every other track | the camera as the publisher sends it | when the subscriber shows `main` (the default) |
| middle | the source peer id followed by the literal `~mid` (`media::MIDDLE_STREAM_SUFFIX`) | the same picture at half size, capped at about 500 kbit/s | only when the subscriber shows `middle` |
| light companion | the source peer id followed by the literal `~light` (`media::LIGHT_STREAM_SUFFIX`) | the same picture small and cheap, for as long as the track lives | always |

As it appears in an SFU offer (the second token is the track id, random per
track; taken from `three_senders_and_the_subscribers_wants`):

```
a=msid:p000000000000000118db9cab637bcab0 y8qkL7YxnDFTKe2EzHqG
a=msid:p000000000000000118db9cab637bcab0~light nINZV9iZoFzrI45iZJKN
a=msid:p000000000000000118db9cab637bcab0~mid 51VjERekROvrfHxvbyXs
```

A client keys a main track by the stream id as before and pairs the other
two with it by stripping the suffix. All three are negotiated whenever the
publisher has them; the subscriber says which heavy one it shows (below).
It shows the light one the moment the heavy one it shows stands still; no
key frame is needed at that moment. A client that does not know the contract
sees further video tracks under stream ids that match no participant, and
is sent the main and the light one, as before step 5.

### What a subscriber shows (`video-wants`)

```json
{"kind":"video-wants","wants":[{"peer":"p000000000000000118db9cab637bcab0","quality":"middle"},{"peer":"p000000000000000218db9cab637bd001","quality":"light"}]}
```

`lakko_sfu_proto::ClientFrame::VideoWants`: for each publisher named, the
picture this subscriber shows (`MediaPeer::set_video_wants`).

- `main`: the main track and the light one. `middle`: the middle track and
  the light one. `light`: the light one alone.
- No frame ever sent, or a publisher not named: `main`. A client that knows
  nothing of this is served as before.
- `middle` asked of a publisher that has no middle track open on this
  subscriber: `main`. The want is kept and applies when the track opens.
- **Each frame replaces the whole set.** A publisher the new frame omits is
  back at `main`. A peer named twice has its last entry.
- A name that is no publisher of the call changes nothing (it is kept, and
  applies if that peer's tracks appear). On an SFU that relays signaling
  only the frame is taken and means nothing.
- At most 64 entries, each `peer` 1 to 64 bytes; an unknown `quality` or an
  unknown field is refused like any malformed frame. There is no reply.

**Pausing is immediate**: the next packet of a track the subscriber does not
show is dropped (`not_wanted`), nothing is held, and the subscriber's own
keyframe requests on that track are not relayed.

**Resuming waits for a key frame.** When a subscriber starts showing a track
it was not being sent, the publisher is asked for a key frame on the media
line behind it at once (`reason` `track-resume`, through the per-track rate
limit, so a request inside the window is owed and goes out when the window
ends) and again once a second, and the track starts at the first packet of
a key frame (the frame descriptor's flags; header only). After five requests
with none it starts at the next packet, as a stream without a frame
descriptor must. The downstream stream is continuous across the pause: the
first packet after it takes the next unused sequence number and a timestamp
moved on by the time that passed (`StreamMap::rebase`), so the subscriber
sees no gap and asks for no retransmission. A packet from before the resume
point has no slot (`stale`).

Not built: any choice by the SFU. It forwards what the subscriber asked for
and does not estimate a subscriber's downlink (ADR-0070 step 6).

**Why.** A home uplink that loses 40 to 75% of its datagrams for about a
second does not repair a stream of about 250 packets a second inside that
second; a stream of 25 to 50 packets a second is repaired in a few hundred
milliseconds (`measure_uplink_dip`). Moving a subscriber's one track to a
light layer during the dip (the uplink fallback, below) was measured not to
shorten the stall, because the switch needs a light key frame to get
through the same dip.

There are two ways a publisher produces the main and the light track. The
Lakko clients use the first, and only it has a middle track.

### Three senders (the contract for publishers)

The publisher sends its camera as **ordinary senders on separate media
lines**: the main one exactly as a client without any of this sends it (one
encoding, no rid); a light one (the same camera scaled to a quarter and
capped at about 150 kbit/s; SFrame stream index 3); and, from three people
in the call on (`lakko_ui_rules::call_group::middle_sender_wanted`), a
middle one (the camera at half size, capped at about 500 kbit/s; SFrame
stream index 4). Each has its own key. It tells the SFU which is which in
the offer that carries the media lines:

```json
{"kind":"sdp-offer","sdp":"...","light_mids":["2"],"middle_mids":["3"]}
```

`light_mids` and `middle_mids` (`lakko_sfu_proto::ClientFrame::SdpOffer`)
list the `mid`s of the offer's media lines that carry the light and the
middle copy. Each is absent when empty, and an absent field means none. At
most 4 entries of at most 16 bytes each, per list; a frame outside that is
refused like any malformed frame. A client may repeat the lists in every
later offer.

`middle_mids` follows the rules of `light_mids` below
(`MediaPeer::declare_middle_tracks`): one middle track per peer, the first
that is taken; a mid that is the peer's light track is refused
(`declared-as-the-other-copy`), and so is the reverse; the declared track
opens on every subscriber as the **middle** track only (`~mid`), and its
packets go to that track and nothing else, when the subscriber shows it.

What the SFU does with `light_mids` (`MediaPeer::declare_light_tracks`):

- **A declaration is taken only for a video media line the peer sends on
  that has not been announced yet as an ordinary track.** The lines of the
  offer that carries the declaration always qualify: `str0m` announces a
  line after the offer is applied. Anything else is ignored and logged
  (`media: light-track declaration ignored`, with `reason`: `unknown-mid`,
  `not-video`, `not-sent-by-peer`, `already-a-main-track`,
  `another-track-of-this-kind-declared`, `declared-as-the-other-copy`), and
  the track, if there is one, stays an
  ordinary track of its kind.
- **One light track per peer, the first that is taken**, for good.
- The declared track is opened on every subscriber as a light companion
  **only**, never as a main track. Every packet of it is forwarded to the
  companion, verbatim (C-39), on the companion's own RTP stream: its own
  SSRC, sequence-number and timestamp map (`src/stream_map.rs`),
  retransmission buffer and, with `LAKKO_SFU_VIDEO_FEC=1`, FEC state.
- The main sender is forwarded as any single-encoding video track.
- **No layer is selected anywhere.** No track switches, and the main and
  the light one do not wait for a key frame at the SFU: each is forwarded
  from the first packet that arrives after the track opens. A track that
  was paused and is shown again does wait for one (above); the middle
  track is paused until asked for, so it always begins at one.
- **Key frames.** When a track opens on a subscriber the publisher is asked
  for a key frame on the media line behind it (`reason`
  `downstream-opened`), through the per-track rate limit; a request inside
  the window is owed and goes out when the window ends. A track that opens
  paused (the middle track, or a main track of a publisher the subscriber
  shows at `middle` or `light`) is not asked for. A subscriber's PLI or FIR
  on the companion goes to the publisher's light sender, on the main track
  to the main sender, on the middle track to the middle sender.
- **Bitrate control** (`LAKKO_SFU_BITRATE_CONTROL=server`) is unchanged: the
  estimate covers the publisher's whole uplink, every sender and audio, and
  the REMB carries it on every video stream.

A second video sender that is **not** declared is what it always was: a
second main track under the publisher's own stream id, told from the first
by its track id alone.

### Simulcast (the variant kept for a publisher that sends rids)

A publisher that sends one video media line as simulcast layers (more than
one rid) also reaches every subscriber as the two tracks: the main track on
the heaviest layer and a companion on the light layer, which never
switches. The light layer is rid `q` when the track has one, otherwise the
layer with the lowest measured bitrate once every layer has been measured,
fixed from then on (`MediaPeer::simulcast_layout`). A track is known to be
simulcast from its negotiation when that lists the rids, otherwise from the
moment a second rid arrives on it; the companion is opened then, by a
further offer.

In this variant each track **begins at a whole key frame of its layer**: the
packets of the selected layer are held (`SwitchWait`, at most 64 packets and
one second) until a key frame is whole at the SFU, and the publisher is
asked for one on that layer (`reason` `track-start`) when the track opens,
then once a second, five times at most. A main track that latched another
layer is moved to the heaviest by the default-layer rule of
`drive_uplink_fallback`. The subscriber-bandwidth policy
(`drive_layer_selection`) may lower the main track when the SFU has a
send-side estimate for the subscriber; the companion is exempt from it, from
`select_layer` and from the uplink fallback.

**Why the clients stopped using it (a live call, 2026-10-04).** A browser
under REMB-only bitrate control sends its full simulcast layer only above
about 900 kbit/s and nothing in between: every dip of the SFU's estimate
dropped the publisher to 180p, and every attempt to switch `f` back on (a
720p key frame burst) produced delay growth and another cut. One encoding
has no such cliff, its encoder scales down gradually, and the same publisher
held 720p before simulcast.

**A subscriber's REMB is not a budget** (fixed the same day). On that call a
subscriber's main track sat on `q` while `f` was arriving (`role="main"
sent=161 wrong_layer=1427` per five seconds). Under server bitrate control
no transport-wide feedback is negotiated, so a browser subscriber sends
REMB, and `str0m` hands a REMB over as `Event::EgressBitrateEstimate`
whether or not its own estimator is on. The SFU took it for the subscriber's
budget. A REMB follows what the subscriber is being sent: when the
publisher's heavy layer paused, the figure fell below the heavy layer's
rate, the main track was selected to `q`, and from then on the figure could
not grow past what `q` brings. Only a send-side estimate (`BweKind::Twcc`)
is a budget now (`bwe_kind_bps`); with none, the default-layer rule keeps a
main track on the heaviest layer. That a browser sends REMB here is read
from the negotiation and the code path, not captured from the call.

### Visible

- `media: track announced by peer` carries `role=main|middle|light` and
  `simulcast`; `media: offer declared light tracks` and `media: offer
  declared middle tracks` name the peer's light and middle mid; `media:
  subscriber's video wants changed` says, once per `video-wants` frame, the
  subscriber and how many publishers it named at `main`, `middle` and
  `light`; a track found to be simulcast later logs `media: track found to be
  simulcast from its packets`.
- Each key frame request: `media: keyframe requested from publisher` with
  its `reason`.
- With `LAKKO_SFU_MEDIA_DIAG_SECS` set, the per-interval line `media: fanout
  to subscriber over the last interval` carries `role` (`main` / `middle` /
  `light`), `paused=true|false` (the subscriber is being sent nothing on
  the track: not shown, or waiting for a key frame to resume) and
  `not_wanted` (packets dropped for that reason): one line per downstream
  track, with the packets `sent` on each. For a
  simulcast source a packet of another layer is not an attempt on the
  companion; on the main track it is counted `wrong_layer`.
- `lakko_sfu_media_light_companion_packets_forwarded_total`: packets sent on
  companion tracks. Each is also in
  `lakko_sfu_media_frames_forwarded_total`.
- `lakko_sfu_media_frames_dropped_total{reason="not_wanted"}`: packets of a
  main or middle track a subscriber does not show. Expected traffic from
  three people on, not an alarm.

### Measured (`measure_two_track_dip`)

The publisher sends `q` at 50 packets a second and `f` at 250, 25 frames a
second, and for one second 70% of its media datagrams are lost, both
pictures and their retransmissions. The SFU runs with its defaults. A frame
is decodable when all its packets have arrived and the frame before it on
the same picture was decodable, starting over at a key frame. (a) is the
longest interval in which the main track completes no decodable frame, (b)
the longest in which neither track does: what a client that shows whichever
track is alive would see.

**Two senders, the light one declared** (the default of the test). Twenty
runs on a laptop, seeds 1 to 20 (`LAKKO_DIP_RUNS=20`), the night of 2026-10-04,
milliseconds; a `clippy` build of the workspace ran beside the first runs:

| Seed | Lost / sent in the window | (a) main | light alone | (b) either | b / a |
|---|---|---|---|---|---|
| 1 | 643 / 892 | 1086 | 509 | 509 | 0.47 |
| 2 | 555 / 816 | 477 | 543 | 477 | 1.00 |
| 3 | 639 / 897 | 490 | 418 | 337 | 0.69 |
| 4 | 578 / 844 | 540 | 616 | 540 | 1.00 |
| 5 | 620 / 870 | 447 | 347 | 313 | 0.70 |
| 6 | 584 / 846 | 512 | 684 | 478 | 0.93 |
| 7 | 640 / 900 | 740 | 715 | 550 | 0.74 |
| 8 | 626 / 877 | 1007 | 588 | 588 | 0.58 |
| 9 | 592 / 844 | 420 | 654 | 313 | 0.75 |
| 10 | 560 / 820 | 381 | 344 | 241 | 0.63 |
| 11 | 608 / 853 | 404 | 696 | 369 | 0.91 |
| 12 | 643 / 905 | 828 | 343 | 343 | 0.41 |
| 13 | 527 / 792 | 279 | 375 | 202 | 0.72 |
| 14 | 593 / 839 | 402 | 794 | 343 | 0.85 |
| 15 | 610 / 874 | 438 | 785 | 438 | 1.00 |
| 16 | 589 / 845 | 454 | 587 | 281 | 0.62 |
| 17 | 665 / 916 | 548 | 411 | 411 | 0.75 |
| 18 | 583 / 845 | 291 | 446 | 270 | 0.93 |
| 19 | 626 / 872 | 407 | 280 | 280 | 0.69 |
| 20 | 636 / 886 | 1034 | 246 | 246 | 0.24 |

Over the twenty: the median of (a) is 454 ms and of (b) 343 ms; the longest
(a) is 1086 ms and the longest (b) 588 ms.

What the runs show:

- The displayed stall is shorter with the two tracks in seventeen of the
  twenty runs, by 7 to 76%.
- **In three of the twenty (2, 4, 15) the companion bought nothing**: the
  light stream had a hole of its own at least as long over the same stretch.
  The companion is 50 packets a second through the same loss, not a stream
  that is immune to it: its own longest stall was 246 to 794 ms, and in ten
  of the twenty longer than the main track's.
- (b) is never longer than (a): a client loses nothing by having the second
  track, other than the bitrate of the light picture on its downlink.

**One simulcast sender** (`LAKKO_DIP_SIMULCAST=1`), the same twenty seeds,
the same day: the median of (a) is 537 ms and of (b) 332 ms, the longest (a)
1027 ms and the longest (b) 882 ms; (b) is shorter than (a) in nineteen of
the twenty. The two variants do not send the same datagrams, so a seed is
not the same loss pattern in both, and the two sets of numbers are not a
comparison of the variants.

Not measured: what a browser or a native decoder does with the two tracks
(the subscriber is a `str0m` client in RTP mode; "decodable" is computed
from the packets that arrived), how fast a client notices that the main
track stands still, anything with a real encoder's key frames, and whether
two senders hold their resolution through a dip of the estimate on a real
call, which is the reason for the change.

## Senders nobody watches (`video-needed`, 2026-10-11)

`video_needed.rs` holds the rule, `media::drive_video_needed` and
`MediaPeer::drive_video_needed` act on it. On unless
`LAKKO_SFU_VIDEO_NEEDED=0` (unset, empty or `1`: on; anything else refuses
to start). Off, no `video-needed` frame is ever sent and nothing in this
section runs.

**What was seen.** On a live call of four (2026-10-09) every tile showed
the middle picture, and every browser went on uploading its main sender, up
to 2.7 Mbit/s, which the SFU forwarded to nobody. Two of the browsers shared
one mobile link whose uplink and downlink both stalled repeatedly. A picture
is chosen per tile per viewer, so in a speaker view the speaker's main
sender is wanted and has to keep flowing; in a plain grid of four or more
nobody's is.

### The frame

The SFU tells a publisher which of its **own** camera senders somebody is
being sent, or is about to be sent:

```json
{"kind":"video-needed","main":false,"middle":true}
{"kind":"video-needed","main":true,"middle":true}
```

`lakko_sfu_proto::ServerFrame::VideoNeeded`. Both fields are required
booleans; a frame without one, or with another type in its place, is
refused like any malformed frame. `false`: no subscriber is sent that
sender, the client may stop encoding and uploading it. `true`: the client
must be sending it. The light sender is always needed and is not in the
frame. A client that ignores the frame behaves as before.

### Who is told, and when a sender is needed

A publisher is told at all only when it **declared a light sender**
(`light_mids` of an accepted offer): the three-sender contract above. A
publisher of one video track, and one that sends simulcast, is told nothing.

For a publisher `P`, each other peer `S` of the call that holds a
downstream video track of `P` is counted (`MediaPeer::heavy_wanted_of`),
**only by what it said**: the entry for `P` in its latest `video-wants`.
Not by what it is being forwarded, and not by what downlink control gives
it at the moment.

- `S` named `P` at **`main`**: it wants main (it has a main track of `P`,
  in any state: a track still being negotiated is about to be sent); and,
  **under downlink control**, middle too: the middle picture is what its
  allocation falls back to, and the allocation has to know what it costs.
- `S` named `P` at **`middle`**: it wants middle and never main, also while
  its middle track is still being negotiated or not yet open. (Only of a
  publisher that has no middle sender at all does it want main: that is
  what it is sent, for good.)
- `S` named `P` at **`light`**: neither.
- **`S` said nothing of `P`** (it never sent `video-wants`, or its latest
  set does not name `P`): neither. A subscriber that left counts for
  nothing.

**A subscriber that never sends `video-wants` switches nobody's sender
on.** What it is *forwarded* is unchanged (the main and the light track,
as `video-wants` says above), so it sees a publisher's main picture while
somebody else's wish keeps that sender on, and **the light track only**
when the sender is off. A client that does not speak `video-wants` is not
supported on an SFU with this switch on; there is no deployed client to
stay compatible with, and the native clients were brought up to the
protocol on 2026-10-11 (they send `video-wants`; unit-tested, not run on
a device or a call). The reason for the rule: somebody joining a call of five says
what it shows a fraction of a second after it is offered the tracks (seen
on a live call: `light` for everyone in the same millisecond, `middle`
about 230 ms later), and counting it as wanting main until then would
make five publishers start a 720p encoder for nothing. The same goes for
a subscriber that named `middle` while its middle track is still being
negotiated: meanwhile it is forwarded main if main flows and the light
track if it does not, and main is not switched on for it.

`main` is `true` while at least one subscriber wants main; `middle` while
`P` has a middle sender and at least one wants middle. `main` covers every
video sender of `P` that is neither its declared light nor its declared
middle track.

- **On, at once.** The moment a sender that was told `false` is wanted, the
  publisher is told `true`: the count runs at most every 20 ms
  (`VIDEO_NEEDED_INTERVAL`) and in the pass after every `video-wants`.
- **Off, late.** `false` is sent only after the sender has been unwanted
  for **10 s** without a break (`video_needed::UNNEEDED_AFTER`): a viewer
  flipping between the grid and the speaker view does not stop and start an
  encoder. For the same reason the first frame a publisher gets, right
  after it declared its senders, says `true` for every sender it has.
- The frame is sent when its content changes, and again if it could not be
  queued on the publisher's signaling socket. One line per change:
  `media: which of a publisher's camera senders are needed changed`
  (`publisher`, `main`, `middle`, `want_main`, `want_middle`).

### What a client has to do (the contract)

- **On `false` for a sender**: it may stop encoding and sending on it. The
  media line, the SSRC and the SFrame key stay as they are: no
  renegotiation, no new offer. It must not stop the light sender. If it
  goes on sending, nothing breaks; the packets are dropped as before.
- **On `true` for a sender**: it starts sending on it again at once. Until
  its packets arrive a viewer that asked for the picture sees the light
  one.
- The SFU does **not** rely on the first frame after that being a key
  frame: if the sender's first packet does not begin one, the SFU asks for
  one (PLI) in that pass, and a track that was waiting starts at it. The
  client answers a PLI as it always did.
- Each frame carries both values; the last frame is the whole state.
- The SFU itself asks nothing of a sender that is off (not when a new
  subscriber's track opens either). A subscriber's own PLI is still
  relayed, so one may arrive: it can be dropped or kept for the restart.

### What the SFU does around a sender that is off

A sender the publisher was told to stop and that has then sent nothing for
**300 ms** (`video_needed::IDLE_QUIET`) is **idle** (`media: a sender
nobody is being sent has stopped`); its next packet is its **return**
(`media: a sender that had stopped is sending again`). A sender that was
told to stop and goes on sending is never idle, and nothing below applies
to it.

- **Resuming tracks wait, and do not give up.** A track that is to start
  again from an idle sender is not asked for and uses up none of its five
  requests (`MediaPeer::due_resume_keyframes`): no packet can arrive until
  the client has switched the sender on, and "start at the next packet"
  would begin the picture at whatever comes first. When the sender is back
  the wait is counted as before.
- **Key frame requests.** A track that opens on a subscriber while the
  sender is idle is not asked for (`request_keyframes_for_opened`). The
  count of unanswered requests on the sender
  starts again at its return (they were asked of a sender that was off),
  so the request at the return is not held back by it.
- **No retransmission is asked** of a sender that went idle, or for the
  sequence numbers its return skipped (`give_up_missing`).
- **No burst.** At the moment a sender goes idle its stream is forgotten by
  the burst-end bookkeeping (`BurstEnds::forget`), the uplink burst
  detector behind the playout delay (`UplinkBursts::forget`) and the loss
  counter of the bitrate estimate (`SeqLossCounter::forget`). Its return is
  the first packet of a stream to all three, whatever its sequence number:
  no key frame at a burst's end, no playout delay, no loss.
- **The publisher's bitrate estimate** (`LAKKO_SFU_BITRATE_CONTROL=server`)
  does not come down because less arrives: it is lowered only by over-use
  or lasting loss, and the cap on its rise (1.5 times what arrived lately,
  or the start value) stops a rise and lowers nothing. What can happen is a cut *while* the sender is off, which is taken
  from the little that arrives then. So the estimate of the moment the
  sender was told to stop is remembered, and when it is told to start the
  estimate is put back to at least that, on faith as the start value is
  (`PathEstimator::restore_after_idle`; over-use within 5 s cuts it as
  usual), and a REMB goes out with the next clock tick.
- **Downlink control** costs a track by the rate arriving on it, and an
  idle sender has none; "costs nothing" would make giving it look free. An
  idle sender, and one in the first 2 s after its return, is taken to cost
  what was last measured on it, for 60 s (`RATE_MEMORY`), and after that a
  nominal 2.5 Mbit/s (main) or 500 kbit/s (middle); never zero, and never
  less than what is arriving (`MediaPeer::heavy_rate_bps`).
- **Diagnostics.** The `media: publisher stream over the last interval`
  line carries `needed` (`yes`, `no`, or `not-told` for audio, the light
  sender and publishers that are told nothing); a sender that is not needed
  has its line though nothing arrived on it.

All constants chosen, none measured.

### Tests

- `video_needed.rs` (7): the delay, on at once, a need that comes back
  inside the delay, a publisher without a middle sender, an undelivered
  frame.
- `media.rs` (16): the need rule in every combination, by what was named
  only; a subscriber that named `middle` while its middle track
  negotiates counts for middle only; a joiner that says `light` and then
  `middle` never causes `main: true`, and naming `main` does at once; a
  silent subscriber never switches a sender on; told `true` first, `false`
  after the delay, `true` at once; no middle sender; a subscriber
  leaving; the switch off and a publisher that declared nothing; a sender
  that ignores the frame; going idle and coming back 300 sequence numbers
  on without a burst; the resuming track that waits; the return with and
  without a key frame; the cost of an idle sender; the estimate put back;
  the diagnostics line.
- `synthetic_client_e2e.rs`,
  `a_sender_nobody_watches_is_switched_off_and_comes_back_at_a_key_frame`:
  a publisher of three senders and two subscribers at `middle`, through
  the production media task under server-side bitrate control; a third
  subscriber joins while main is off, says `light` when it is offered the
  tracks and `middle` 230 ms later. Observed (four runs of the final
  build): `main: false` 9.8 s after the publisher's first frame (nobody
  ever named main); the joiner caused no frame at all and was never sent a
  main packet, with main off for 5.1 to 5.2 s after it joined; `main:
  true` 2.7 to 3.9 ms after one subscriber named main; the first packet of
  the key frame on that subscriber's main track 84 to 88 ms after the
  request, one delta frame having gone by first; consecutive sequence
  numbers, no retransmission; no key frame and no retransmission asked of
  the sender while it was off; the REMB 5 Mbit/s before, during and
  after, the first one 0 to 5 ms after the request.

### Not verified

Note, 2026-10-11: the web client acts on the frame since the same day
(`apps/web/src/calls/sender-activity.ts`), and the commit record of that
day states that the main sender going off in a grid and coming back for
a pinned person was seen on live calls in five browsers. No figure from
those calls is recorded here, so the questions below stand as written.
iOS and Android act on the frame too (unit-tested, not run).

As first written: nothing here has been run against a browser. Whether a browser's first
frame after `encoding.active` goes back to `true` is a key frame, whether
it keeps its RTP sequence numbers, and what its own bitrate allocation does
with a sender that returns, are not known from this work; the e2e publisher
is a synthetic `str0m` client that continues its sequence numbers and comes
back with a delta frame. A return on a sequence number far from the last
one is covered in the SFU's own books by unit tests only. With
`LAKKO_SFU_VIDEO_FEC=1` the FEC receive state of an idle stream was not
looked at. The estimate being put back was exercised by a unit test (a
forced cut), not by a cut on a real path.

## The publish budget (`publish-budget`, 2026-10-11)

`publish_budget.rs` holds the rule, `media::drive_publish_budget` and
`MediaPeer::drive_publish_budget` act on it
([ADR-0068](../../docs/adr/0068-server-side-bitrate-control.md), amendment
of 2026-10-11). Only under `LAKKO_SFU_BITRATE_CONTROL=server`, and on unless
`LAKKO_SFU_PUBLISH_BUDGET=0` (unset, empty or `1`: on; anything else refuses
to start). Off, or with the client deciding its own bitrate, no
`publish-budget` frame is ever sent. REMB is the same either way. The
startup line `media: UDP socket bound; ...` carries `publish_budget=true`
only when the frame will actually be sent (the switch on **and** the SFU
deciding the bitrate).

**Why.** Measured on live calls (2026-10-11): with the estimate held at
1000 kbit/s one browser's main sender followed the REMB (720 kbit/s) and
another's did not (a flat 1537 kbit/s), while that same browser kept its
light sender under the ceiling the client had set on it. A publisher that
sends more than its path carries stands a queue in front of its own audio.
So the estimate also goes out in a form every browser obeys: a ceiling per
sender.

### The frame

```json
{"kind":"publish-budget","main_kbps":1922,"middle_kbps":500}
```

`lakko_sfu_proto::ServerFrame::PublishBudget`. Both fields are required
unsigned integers, kilobits per second, each from 1 to 20000
(`MAX_PUBLISH_BUDGET_KBPS`); anything else is refused on decode, and the
encoder refuses to compose it. The ceiling of the publisher's **main** and
**middle** camera sender. The light sender keeps its fixed ceiling and audio
is never limited. A client sets each figure as that sender's maximum
bitrate; one that ignores the frame behaves as before.

### The rule

All of it is the SFU's; a client has no allocation rule.

| Step | What | Figure |
|---|---|---|
| headroom, audio | the rate arriving on the publisher's audio tracks where it is metered (downlink control on), kept within 32 to 128 kbit/s; otherwise a constant | `AUDIO_RESERVE_BPS` 64 kbit/s |
| headroom, light | a constant, when the publisher declared a light sender | `LIGHT_RESERVE_BPS` 150 kbit/s |
| retransmissions and FEC | what is left is divided by 1.15 | `PROTECTION_PERCENT` 15 |
| middle | its plan rate when needed; gives way only to the main floor | `MIDDLE_PLAN_BPS` 500 kbit/s |
| main | the rest when needed | up to `MAIN_PLAN_BPS` 2500 kbit/s |
| floors | never below | `MAIN_FLOOR_BPS` 200, `MIDDLE_FLOOR_BPS` 100 kbit/s |

A sender the publisher was told nobody watches (`video-needed`) takes
nothing from the budget and is given its ceiling anyway: the one it would
have if it were switched on now. A publisher that was told nothing of its
senders is taken to send them all. Every constant is chosen, not measured.

Examples (a light sender declared, audio not metered): an estimate of
3000 kbit/s gives `main 1922, middle 500`; 1000 kbit/s gives
`main 200, middle 483` with both needed and `main 683` with the middle
sender off.

### When a publisher is told

- **First**: once it is connected and has sent video (when its estimate
  first goes out as REMB).
- **Lower**: at once, when a ceiling is under 97% of what was told
  (`DROP_PERCENT`).
- **Senders**: at once, when which senders are needed changed and a
  ceiling with it. A `video-needed` frame makes the budget run in the same
  pass.
- **Higher**: no sooner than 2 s after the last frame (`RAISE_INTERVAL`),
  and only when a ceiling differs by 10% of what was told
  (`RAISE_SHARE_PERCENT`) or reaches its plan maximum.

The rule is looked at every 100 ms (`PUBLISH_BUDGET_INTERVAL`), so a lower
budget is at most that late. A frame that cannot be queued on the
publisher's signaling socket is produced again at the next run.

### Visible

One INFO line per frame sent:

```
media: a publisher was told how much its camera senders may send publisher=<peer id> estimate_kbps=1000 main_kbps=200 middle_kbps=483 main_needed=true middle_needed=true why=lower
```

`why` is `first`, `lower`, `senders` or `higher`. Counter:
`lakko_sfu_publish_budget_sent_total{why}`.

### Tests

- `publish_budget.rs` (14): the headroom, the shares, the bounds over a
  sweep of estimates and needs, monotonicity, and each of the four reasons
  a frame is sent, with the interval and the share.
- `media.rs` (4): told only once connected and sending video, a cut at
  once, the interval, an undelivered frame, the share following a
  `video-needed` change in the same pass, nothing with the switch off or
  under client-side control.
- `config.rs`: the switch.
- `tests/synthetic_client_e2e.rs`,
  `a_sender_nobody_watches_is_switched_off_and_comes_back_at_a_key_frame`:
  the publisher receives the frame on its signaling socket from the live
  media loop, within the bounds.

### Not verified

Nothing here was seen on a call, and nothing was run against a browser.
Whether a browser's main sender obeys the ceiling is the client's side
(`apps/web/src/calls/publish-budget.ts`); iOS and Android do not read the
frame yet. A modified client can ignore the frame, as it could ignore REMB.

## Carrying the light picture through a burst of loss (ADR-0070 steps 3 and 4)

[ADR-0070](../../docs/adr/0070-call-media-through-a-loss-burst.md). The
numbers in the section above were taken before this was built: the light
companion was then repaired no faster than the main track. Two things
changed, both on by default.

### The light track is protected harder (step 3)

**Publisher leg.** A missing packet of a publisher's light video (its
declared light track, or the light layer of a simulcast track once the SFU
knows which that is) is asked for again every 20 ms
(`media::LIGHT_NACK_RETRY`, chosen, not measured); every other stream every
60 ms as before. Both are asked for over the same 1.5 s, so the light track
gets up to 75 requests per packet instead of 25. `str0m` builds NACKs for a
whole session on one tick (33 ms upstream); the vendored fork makes the tick
follow the shortest per-stream interval, so a session with a light track
ticks every 20 ms. Seen in
`a_missing_light_packet_is_asked_for_again_three_times_as_often` (one run,
2026-10-05): the median gap between two requests for one light packet was
21.0 ms and for one main-track packet 63.3 ms. The requests leave at once:
RTCP feedback is the first thing a `str0m` session sends when polled, and
the media loop wakes on the session's own deadline.

**Subscriber leg.** Every packet written to a light companion track is sent
to the subscriber a second time 30 ms after it reached the SFU
(`media::LIGHT_DUPLICATE_DELAY`, chosen), as a retransmission on the
track's RTX stream, exactly as a packet the subscriber had asked for. A
receiver that has the first copy discards the second by its sequence
number. Rules:

- only where the subscriber negotiated RTX on the track: `str0m` keeps no
  packet of a stream without an RTX payload type, so such a track is sent
  once;
- from the stream's retransmission cache (2000 packets, 3 s): a packet that
  has left it is not sent again;
- once per packet;
- at most 2048 copies wait per subscriber (`media::MAX_LIGHT_DUPLICATES`);
  one more is dropped without a log line;
- the main track is never sent twice.

`LAKKO_SFU_LIGHT_DUPLICATE=0` switches the second copy off (unset, empty or
`1`: on; anything else refuses to start).

**A subscriber's own requests.** The vendored `str0m` has no hold-off on
answering a NACK: every request for a packet still in the cache queues a
retransmission, for the companion as for every stream (pinned by a test in
the vendored `streams/send.rs`). Nothing had to be lowered. How often a
subscriber asks again is the subscriber's own choice.

### Note, 2026-10-07: what a browser made of the second copies

A retransmission the SFU sent (a NACK's answer, the second copy of a
light packet, a probe's padding copy: all RTX packets) carried RTP
padding to a multiple of 16 bytes with the padding bit set, as upstream
`str0m` writes every packet. That is correct by RFC 4588 and RFC 3550,
and the synthetic `str0m` clients every measurement here was made with
restore it correctly. A Chrome viewer did not: on a live call it
restored 848 of 12 957 second copies of light frames intact, the one in
sixteen whose RTX packet needed no padding, and 12 109 as the frame less
its last byte (the padding bit kept on the restored packet, the padding
gone, the last byte read as a count); on the middle track 20 intact
against 341, which are NACK-driven retransmissions. Since the fork's
fourteenth change (2026-10-07) a retransmission carries no padding.

So for a browser the second copies and the fast retransmissions of this
section were unusable in fifteen cases of sixteen until then. The
measurements below are from synthetic `str0m` clients, which restore
correctly; they say nothing about a browser before that date. Tests:
`a_retransmission_restores_to_the_payload_that_was_sent` in the vendored
`streams/send.rs` (payload lengths 1 to 64, last bytes 0x00, 0x01, 0x04,
0xff, `resend` and NACK; restored by RFC 4588 / 3550 and the way a
libwebrtc receiver does it, which is recalled, not read for this), and
`a_retransmission_to_a_subscriber_carries_the_original_payload_and_no_padding`
in `synthetic_client_e2e.rs` (a subscriber losing every seventh packet:
57 lost, 57 retransmitted, every payload equal to the original, 0 with
the padding bit; before the change 53 of the 57 carried it).

### Playout delay follows the path (step 4)

`playout_delay.rs` holds the rule, one instance per (subscriber, publisher):
the minimum playout delay is 0 until a burst of loss was seen on the
publisher's uplink or on that subscriber's downlink, 300 ms from then on,
and after 180 s without a burst it comes down by 100 ms every 30 s (200 at
180 s, 100 at 210 s, 0 at 240 s). A burst at any time takes it back to 300.
All of these are named constants, chosen, not measured.

What counts as a burst:

- **uplink**, per publisher, from the sequence numbers of its video packets
  (every video stream: main, light, each simulcast layer; audio is not
  counted; an FEC packet is a packet of its stream, see the note below): a
  packet missing for more than 150 ms
  (`uplink_fallback::HOLE_AGE`), or a one-second interval in which more than
  10% of its video packets did not arrive in order (`SeqLossCounter`, the
  counter bitrate control uses, in an instance of its own so it runs under
  either bitrate mode; at least 20 packets expected in the interval). The
  uplink fallback's own hole detector was not reused: it is private to a
  policy that needs two simulcast layers and ignores the light one;
- **downlink**, per subscriber: a receiver report on any video track the SFU
  sends it whose loss fraction is above 10% (26/256 or more), each report
  read once; or NACKs, over one second, for more than 10% of the video
  packets written to it in that second (at least 20 written). Second copies
  are not counted as NACKs.

On an ordinary path with a steady 2% of loss the 150 ms rule can fire
without any burst (a packet whose first retransmission is lost too): such a
path then stays at 300 ms. That is what the rule as specified does; it has
not been seen on a call.

**Corrected 2026-10-09: FEC packets.** A publisher that sends ULPFEC inside
RED numbers its FEC packets in the sequence of the media stream. The uplink
detector was not handed them, so every FEC packet read as a missing packet
and the uplink of such a publisher read as in a burst all the time. In the
server log of the call of that day the pair of the one publisher that sent
FEC was raised for `uplink-burst` in the very pass in which a joining
subscriber was given its tracks (16:59:57.238), thirteen seconds after one
burst on that uplink and seventeen before the next. The detector is now handed FEC packets like any
other (`MediaPeer::observe_uplink_burst`; test
`fec_packets_in_sequence_are_not_holes_for_the_uplink_burst_detector`).
What remains: an FEC packet that is lost is not sent again, so it stays a
missing packet and the 150 ms rule still reads it as a burst. On a path
that loses anything at all, a publisher that sends FEC is therefore still
held at 300 ms.

The value travels in the playout-delay RTP header extension
(`http://www.webrtc.org/experiments/rtp-hdrext/playout-delay`, minimum and
maximum in 10 ms units), registered in every peer's session under id 5 and
therefore offered to every subscriber. It is stamped on every video packet
of that publisher to that subscriber, main track and companion alike, with
the pair's minimum and a fixed maximum of 5 s
(`playout_delay::MAX_PLAYOUT_DELAY_MS`), and only on a track whose media
line negotiated it; a subscriber that left it out of its answer gets
nothing. Audio packets are not stamped. A retransmitted packet carries the
value it was first sent with.

`LAKKO_SFU_PLAYOUT_DELAY=0` switches it off: the extension is then neither
offered nor answered (unset, empty or `1`: on; anything else refuses to
start).

Not built: a subscriber that did not negotiate the extension is told
nothing. ADR-0070 has the client set `jitterBufferTarget` there, which needs
the value on the signaling socket (a frame in `lakko-sfu-proto` and the
three clients).

### Visible

- `media: minimum playout delay changed` (info): `subscriber`, `publisher`,
  `from_ms`, `to_ms`, `reason` (`uplink-burst`, `downlink-burst`, `quiet`).
- With `LAKKO_SFU_MEDIA_DIAG_SECS` set, `media: fanout to subscriber over
  the last interval` carries `duplicates` (second copies sent on that
  track; not part of `sent`) and `playout_delay_ms` (what is being stamped
  for that publisher; `None` for audio and with the feature off).
- `lakko_sfu_playout_delay_raises_total`: one per pair per raise.
- `lakko_sfu_media_light_duplicates_sent_total`: second copies sent.

A second copy is not counted as a repair anywhere: it does not enter
`str0m`'s NACK count, the SFU's `nacked_packets` or the `retransmitted`
figure of the publisher's bitrate estimate (that one is the publisher's
leg).

### Measured (`measure_two_track_dip`, 2026-10-05)

One second in which each media datagram is lost with probability 0.70,
20 seeds (1 to 20, `LAKKO_DIP_RUNS=20`), two plain senders with the light
one declared, `f` 250 packets a second on the main track and `q` 50 on the
companion, on a laptop over loopback, each table run alone. Per track:
**gap**, the longest interval with no decodable frame, as in the section
above; **late**, the worst frame lateness (how much later than its usual
transit the picture of a moment became decodable; a frame that never
completes is answered by the next decodable frame after it); **stop**, what
a viewer playing 300 ms late sees stand still, `max(0, late - 300)`.
"Either" is a client that shows whichever track is alive. Milliseconds, min
/ median / p95 / max over the 20 seeds, and in how many seeds the stop was
zero.

**Dip on the publisher's uplink, protection off**
(`LAKKO_DIP_PROTECTION=off`: light track asked every 60 ms, sent once, no
playout delay):

| | min | median | p95 | max | zero stops |
|---|---|---|---|---|---|
| main gap | 328 | 602 | 1050 | 1095 |  |
| main late | 640 | 778 | 1011 | 1055 |  |
| main stop | 340 | 478 | 711 | 755 | 0 of 20 |
| light gap | 228 | 453 | 985 | 1037 |  |
| light late | 362 | 578 | 946 | 998 |  |
| light stop | 62 | 278 | 646 | 698 | 0 of 20 |
| either gap | 228 | 324 | 686 | 872 |  |
| either late | 362 | 569 | 830 | 834 |  |
| either stop | 62 | 269 | 530 | 534 | 0 of 20 |

**Dip on the publisher's uplink, protection on** (the defaults):

| | min | median | p95 | max | zero stops |
|---|---|---|---|---|---|
| main gap | 368 | 558 | 722 | 816 |  |
| main late | 578 | 776 | 899 | 900 |  |
| main stop | 278 | 476 | 599 | 600 | 0 of 20 |
| light gap | 152 | 230 | 369 | 462 |  |
| light late | 209 | 278 | 400 | 525 |  |
| light stop | 0 | 0 | 100 | 225 | 12 of 20 |
| either gap | 113 | 227 | 369 | 462 |  |
| either late | 209 | 278 | 400 | 525 |  |
| either stop | 0 | 0 | 100 | 225 | 12 of 20 |

**Dip on the subscriber's downlink** (`LAKKO_DIP_DOWNLINK=1`: every media
datagram that reaches the subscriber, retransmissions included), **protection
off**:

| | min | median | p95 | max | zero stops |
|---|---|---|---|---|---|
| main gap | 250 | 558 | 881 | 1057 |  |
| main late | 627 | 814 | 1005 | 1017 |  |
| main stop | 327 | 514 | 705 | 717 | 0 of 20 |
| light gap | 327 | 438 | 724 | 1034 |  |
| light late | 404 | 656 | 817 | 992 |  |
| light stop | 104 | 356 | 517 | 692 | 0 of 20 |
| either gap | 241 | 334 | 562 | 668 |  |
| either late | 404 | 627 | 762 | 776 |  |
| either stop | 104 | 327 | 462 | 476 | 0 of 20 |

**Dip on the subscriber's downlink, protection on:**

| | min | median | p95 | max | zero stops |
|---|---|---|---|---|---|
| main gap | 342 | 563 | 1027 | 1042 |  |
| main late | 638 | 777 | 985 | 1002 |  |
| main stop | 338 | 477 | 685 | 702 | 0 of 20 |
| light gap | 273 | 361 | 687 | 870 |  |
| light late | 427 | 575 | 828 | 917 |  |
| light stop | 127 | 275 | 528 | 617 | 0 of 20 |
| either gap | 202 | 316 | 499 | 512 |  |
| either late | 427 | 536 | 761 | 762 |  |
| either stop | 127 | 236 | 461 | 462 | 0 of 20 |

**Dip on the subscriber's downlink, protection on, and the subscriber asks
again every 20 ms on every stream** (`LAKKO_DIP_SUB_NACK_MS=20`; the
subscriber in all other tables is a `str0m` client asking every 60 ms):

| | min | median | p95 | max | zero stops |
|---|---|---|---|---|---|
| main gap | 172 | 234 | 281 | 305 |  |
| main late | 261 | 316 | 438 | 467 |  |
| main stop | 0 | 16 | 138 | 167 | 5 of 20 |
| light gap | 131 | 239 | 318 | 323 |  |
| light late | 142 | 279 | 353 | 416 |  |
| light stop | 0 | 0 | 53 | 116 | 13 of 20 |
| either gap | 89 | 158 | 239 | 251 |  |
| either late | 142 | 237 | 299 | 347 |  |
| either stop | 0 | 0 | 0 | 47 | 19 of 20 |

What the runs show:

- **Uplink.** The light track's worst lateness fell from a median of 578 ms
  to 278 ms and its worst case from 998 to 525 ms. A viewer playing 300 ms
  late sees no stop in 12 of the 20 dips, and a stop of 3 to 225 ms in the
  other 8. That is not "nearly every dip". What limits it: a missing packet
  is known to be missing only when a later packet of the same stream
  arrives, which at 50 packets a second with three in ten arriving takes 67
  ms on average and sometimes several times that; and each retransmission
  is itself lost seven times in ten, so some packets need ten or more
  20 ms rounds. The publisher here is a `str0m` client that answers every
  request; a libwebrtc sender does not resend a packet it resent less than
  one round trip ago, so on a real path slower than 20 ms it answers fewer.
- **Downlink.** One second copy changes little at this loss rate: half of
  the packets lose both copies, and what is left is repaired on the
  subscriber's own request schedule (60 ms here), exactly as the main track
  is. The stop is never zero. With the subscriber asking every 20 ms the
  light track's numbers match the uplink case and a client showing either
  track sees no stop in 19 of 20; how often a browser asks again is the
  browser's own rule and was not read from any source in this tree.
- The main track is not helped by any of this on the uplink, by design.

Not measured: a browser or native receiver (whether it honours the
playout-delay extension, what it does with a second copy on the RTX stream,
how often it sends NACKs), loss rates other than 0.70, loss in both
directions at once, a path with delay, and the cost in bitrate on a real
call (in these runs the companion's retransmission stream carried as many
datagrams as its main stream, as intended).

## Key frame requests are bounded

The SFU holds no media, so a subscriber without a reference frame can
only be helped by the publisher sending a key frame, and the SFU asks
for one (PLI) when a track opens, starts or resumes, when a subscriber
asks, and when a burst of loss on the publisher's stream has ended
unrepaired (the next section). Every request for one publisher track goes
through one gate (`MediaPeer::request_keyframe_limited`, `keyframe_ask`):

- **Rate**: at most one per 500 ms per publisher track
  (`KEYFRAME_REQUEST_MIN_INTERVAL`); a request inside the window is owed
  and sent when it ends.
- **Settled by a key frame**: a key frame seen arriving on the track
  (frame descriptor) drops what is owed on it; it is forwarded to every
  subscriber the track is being sent to, the one that asked included.
- **A subscriber's own request** on one of its tracks is relayed at
  once and, while it goes on asking, again after 1 s, 2 s, 4 s and then
  every 8 s (`PLI_RELAY_FIRST_INTERVAL`, `PLI_RELAY_MAX_INTERVAL`).
  A subscriber that has sent none for 2 s (`PLI_RELAY_QUIET`) has what
  it asked for; its next request is a new need and is relayed at once.
  Counted per (subscriber, track), so one receiver that cannot decode a
  stream does not take the others' requests away. A request on a track
  the subscriber is not being sent is dropped. A subscriber's request
  is **not** counted against the bound below: the subscriber is the one
  that knows whether a key frame reached it.
- **The SFU's own requests** (a track opening, starting or resuming, a
  burst ending unrepaired) on
  a track that shows no key frame after 5 (`KEYFRAME_UNANSWERED_MAX`)
  go out once per 10 s (`KEYFRAME_UNANSWERED_RETRY`), until a key frame
  is seen. That is every publisher whose packets carry no frame
  descriptor (Firefox negotiates none: the SFU never sees its key
  frames) and every sender that is switched off.
- **A track that waits for a key frame to resume** starts at the next
  packet when that packet carries no frame descriptor: nothing in it
  says where a key frame begins. With a descriptor it waits for one, at
  most 5 requests a second apart, as before
  (`START_KEYFRAME_REQUESTS`).

All intervals and counts chosen, not measured.

Why: on the first live call (two Chrome, one Firefox, four and a half
minutes) every light and middle sender of every publisher was asked
about 270 times, from the moment the third peer joined (19:42:14 in the
log; before it the two Chrome peers had 3 owed requests in all, at the
start). The only caller of the gate that is not bounded by itself is
the relay of subscribers' own requests (`relay_keyframe_requests`): the
rate limit let two a second through for as long as a subscriber asked,
and nothing counted whether the key frames it was sent stopped it
asking. Which receiver asked is not in the log (a received PLI was not
logged); the timing points at the receivers of the new peer's tracks
and the new peer as a receiver. Chrome answered each request: 263 to 267
key frames per light and middle sender in the SFU's own interval lines.

Tests: `a_track_that_shows_no_key_frame_is_asked_a_bounded_number_of_times`,
`a_key_frame_settles_the_debts_of_its_track`,
`a_subscriber_that_keeps_asking_is_relayed_less_and_less_often`
(unit), and in `synthetic_client_e2e.rs`
`a_subscriber_that_keeps_asking_does_not_cost_a_key_frame_a_second` and
`a_publisher_without_a_frame_descriptor_is_asked_a_bounded_number_of_times`:
three publishers of three senders and a subscriber that sends a PLI on
every track it receives five times a second for 20 s; no publisher
track may be asked more than 10 times. Seen: 5 on each track that was
being sent (6 on one without a descriptor), 1 on a main track that was
paused, 0 on middle tracks nobody wanted. The rule before would have
let about 40 through (two a second); that build was not run.

**Amended 2026-10-07, after a viewer waited more than half a minute
for a Firefox publisher's main picture** (packets arriving and not
decodable, the viewer asking five times a second). Read from the code,
not from that call's server log, which I did not have: as first built,
a subscriber's requests on a track that shows no key frame were subject
to the bound of 5 and then one in 10 s, and to a wait of its own that
doubled to 30 s; and the track had first been held back for the five
seconds its resume waits for a key frame the SFU could not have
recognised. Now a subscriber's request is exempt from the bound, its
own wait stops at 8 s and starts again after 2 s of quiet, and a track
without descriptors resumes at its next packet. Unit tests
`a_subscriber_that_went_quiet_is_relayed_at_once_when_it_asks_again`,
`a_subscribers_request_is_not_counted_against_the_unanswered_bound`,
`a_resuming_track_without_frame_descriptors_starts_at_the_next_packet`;
e2e
`a_subscriber_of_a_publisher_without_a_descriptor_is_asked_for_while_it_asks`
(the subscriber asks for 9 s, then stops): on every track it was being
sent the publisher was asked at 0, 0.4, 1.0, 3.0 and 7.0 s and not
again. The storm tests still hold: 5 per track with descriptors, 6
without, in 20 s.

**Not built, for the maintainer to decide: reading the key frame bit
from the payload.** Since ADR-0071 the VP8 payload descriptor and frame
tag of a web client's frames are in the clear, and the low bit of the
first byte after the payload descriptor says key frame or not. The SFU
could then see key frames on a stream without a frame descriptor: debts
would be settled by observation, and a track could start at a key frame
again. It would take a parser of the VP8 payload descriptor (variable
length: the X, I, L, T, K bits and a one- or two-byte picture id) run on
attacker-chosen bytes, hence a fuzz target; a per-codec switch (VP9,
H.264 and AV1 mark key frames differently); and a rule for clients that
do not leave the header clear (the natives until 2026-10-11, when their
bridge moved to the ADR-0071 layout), where the same bytes
are ciphertext and the bit means nothing. It would change invariant
C-39 from "the SFU never reads a payload" to "the SFU reads the codec's
clear frame header of a payload and nothing after it", and the SFU
would then act on bytes the sender controls and the receiver does not
authenticate separately. What is built instead needs none of that: the
subscriber's asking is the signal.

Changed with it: `a_late_joiners_first_keyframe_is_requested_and_pli_bursts_collapse`
used to assert that the nine repeats of a ten-PLI burst cost a second
request when the window ended. They no longer do; the test now asserts
one request for the burst and one more when the subscriber asks again
after the wait.

Not covered: the simulcast uplink-fallback requests
(`request_layer_keyframe_at`) keep their own retry and are not counted
here. Seen in the same log and not looked into: audio from both Chrome
peers to the Firefox peer was never sent (`no_codec_match=250` per
interval on `Mid(0)`).

## A key frame at the end of an unrepaired burst (2026-10-09)

`burst_keyframe.rs` holds the rule, `media::drive_burst_keyframes` acts on
it. On unless `LAKKO_SFU_BURST_KEYFRAME=0` (unset, empty or `1`: on;
anything else refuses to start).

### What was seen

A live call on the test stack, 2026-10-09, three browsers; the publisher
that matters was Chrome on a home Wi-Fi. From the maintainer's packet
capture of its uplink: a burst of loss every 30 s, each 0.5 to 1.4 s long;
in a burst the audio stream lost about 80% of its packets for a second, the
main video 148 to 223 packets, the light and the middle video 22 to 80
each, and the retransmission stream lost heavily in the same windows. The
viewers counted 19.5 s (Firefox) and 9.2 s (Safari) of stalls, all on that
publisher's picture, on its heavy and its light track alike.

From the SFU's own log of that call (read for this change): once the
third peer had joined, the only key frame requests for that publisher were
`subscriber-pli`, seven of them: six between 4.0 and 5.1 s after the
bursts of 17:00:14, 17:01:14 and 17:02:14 began, and one at 17:01:22. The
SFU asked for none of its own. After the later bursts (17:03:44 to
17:05:14) the log, which ends at 17:05:30, has no key frame request for
that publisher at all; why the viewers did not ask then was not looked
into. Over the ten seconds around the burst of 17:00:14 the count of that
publisher's packets that arrived out of order, which is how a
retransmission arrives, rose by 195 (`retransmitted` 401 at 17:00:10, 596
at 17:00:20, all streams together), against the several hundred the
capture says were lost, and the estimate of its uplink was cut from 5000
to 1250 kbit/s at 17:00:15.2. So the burst was not repaired, the SFU could
know it when the burst ended, and the picture stood until a viewer's own
decoder gave up three seconds later.

### The rule

Per video stream of a publisher (main, middle and light each on its own;
each simulcast layer on its own), from the sequence numbers of its packets
as they reach the SFU, retransmissions, FEC packets and packets rebuilt
from FEC included:

- a **burst** opens with the first missing packet;
- it has **ended** when 5 packets in a row arrived in sequence
  (`BURST_END_PACKETS`) and no packet went missing for 120 ms
  (`BURST_END_QUIET`);
- it ended **unrepaired** when a packet is then still missing 300 ms after
  it was first known missing (`UNREPAIRED_AFTER`: the playout delay a
  viewer is asked to hold), or when the burst outran retransmission: a
  packet missing for more than 1.5 s (`RETRANSMISSION_SPAN`: how long the
  SFU asks for one packet) or more than 512 packets behind the newest
  (`RETRANSMISSION_WINDOW`);
- a burst that ended with every packet there asks for nothing, and neither
  does one whose missing packets all precede a key frame that has started
  arriving;
- one request per burst per stream. A packet that goes missing before the
  request is the same burst going on, and the wait starts over.

All of these are named constants, chosen, not measured. The 120 ms doubles
as retransmission's last chance: when a burst counts as ended, every
packet still missing has been asked for at least twice more (every 60 ms;
every 20 ms on a light stream) on a path that is carrying packets again.

The request:

- goes through the gate of the section above with the reason
  `uplink-burst-end`: at most one per 500 ms per track (inside the window
  it is owed and goes out as `owed`), and none while the track has been
  asked 5 times without showing a key frame;
- is made only for a stream some subscriber is being sent: a downstream
  track of it open and not paused (`MediaPeer::is_sent_stream`). A main
  track nobody shows is not asked for; when it is shown again it is asked
  for as `track-resume`;
- is logged twice: `media: keyframe requested from publisher` and, for
  every burst that ended unrepaired whether or not anything was asked,
  `media: a burst of loss on a publisher's stream ended unrepaired`
  (`lost`, `still_missing`, `known_media`, `known_not_media`, `unknown`,
  `beyond_repair`, `lasted_ms`, `why` = `media-missing` / `unexplained`,
  `key_frame` = `asked` / `owed` / `not-asked` / `nobody-watching`,
  `given_up`).

A burst that ended with packets still missing and was **not** taken for
unrepaired is logged too, since 2026-10-11, so the log of a call shows
every burst and not only the ones that asked:
`media: a burst of loss on a publisher's stream ended with packets missing and asked for nothing`
(`lost`, `still_missing`, `known_media`, `known_not_media`, `unknown`,
`beyond_repair`, `lasted_ms`, `why` = `not-media` / `within-fec-share`).
A burst that retransmission repaired whole, and one closed by a key frame
that arrived of its own accord, are still closed without a line.

### What a missing packet was (2026-10-11)

A lost FEC packet takes a sequence number and is not sent again (read in
libwebrtc M125, `ulpfec_generator.cc`), so on a stream that carries FEC a
packet that stays missing is media somebody needs or an FEC packet nobody
does. Until 2026-10-11 nothing said which, and such a stream was judged by
number alone: unrepaired only when more packets stayed missing than 2
(`FEC_SLACK`) plus one and a half times the FEC share of what the burst
lost. With a share near one half that is three quarters of everything the
burst lost: on the live call of 2026-10-11 (the note of that day in
ADR-0070) bursts that left media missing asked for nothing, and a Firefox
viewer stood still for 3.3 to 4.0 s until its own decoder asked.

When a burst is judged, each packet still missing is now one of three
kinds, from facts the SFU already has in the packets that did arrive:

- **Known media.**
  - An FEC packet that arrived lists it as protected (its mask;
    `fec_receive::fec_protected`, `BurstEnds::fec_protects`). FEC protects
    media packets only. The mask is read only while packets are missing on
    the stream.
  - The generic frame descriptors of the packets around it
    (`frame_descriptor::packet_position`: the `B` and `E` flags of every
    media packet and the 16-bit frame id of a frame's first packet).
    Between two media packets that arrived, with only missing packets and
    FEC packets between them: a packet on the left that does not end its
    frame, or one on the right that does not begin its frame, means a
    packet of that frame is missing; frame ids that step by more than one
    across the gap mean whole frames are missing, one packet at least
    each.
- **Known not media.** Everything missing between a packet that ends frame
  `n` and one that begins frame `n + 1`: an FEC packet, or nothing anybody
  needs.
- **Unknown.** Everything else: a neighbour without the extension, a
  packet let go before the burst was judged, a gap whose frames cannot be
  counted.

The answer: one packet of known media still missing is unrepaired
(`why = media-missing`), whatever the FEC share. Packets known not to be
media never ask for anything (`not-media` when they are all that is
missing). The unknown ones fall under the count rule, computed over them
alone: on a stream without FEC one is unrepaired; on a stream with FEC,
unrepaired when more of them stay missing than 2 plus one and a half times
(the FEC share times what the burst lost, less the packets known not to be
media) (`unexplained`, else `within-fec-share`). The share is counted over
the last packets that arrived in sequence (up to 1024). A stream whose
packets carry no descriptor and whose FEC masks name nothing missing is
judged exactly as before.

What the descriptor facts rest on, and when they are not used:

- **Frame ids** wrap at 16 bits and are compared by their wrapping
  difference. A libwebrtc sender numbers one sender's frames one after the
  other, but simulcast layers of one sender share the counter (read in
  libwebrtc M125, `rtp_video_sender.cc`), so the ids are used on a stream only after 8 frames in a row
  (`FRAME_ID_TRUST`) began exactly one id after the frame before with
  nothing missing between, and never again once two such frames differed
  by anything else. Lakko's three camera senders are separate senders; a
  simulcast layer is expected to fail this and stay on the count rule.
- **Frame boundaries** say something about the packet missing next to them
  only if FEC packets sit between frames and never inside one. A stream on
  which an FEC packet was seen inside a frame loses that inference.
- An FEC packet's own descriptor is never read (libwebrtc copies the last
  media packet's onto it); a packet without the extension and a value
  libwebrtc's parser would refuse say nothing.

What the SFU reads for this is header only: sequence numbers, the FEC
header's mask, and three fields of the frame descriptor that are in the
clear already. No payload, no key (C-39).

**Bounds** (an admitted peer chooses every one of these facts): at most 12
streams per publisher (`MAX_STREAMS`); per stream at most 512 missing
packets (`RETRANSMISSION_WINDOW`; a time and a flag each) and a fixed table
of 1024 eight-byte entries (`ARRIVED_WINDOW`) for the packets that arrived
lately, plus counters. A packet costs one table write; the kinds are worked
out once per burst, when it is judged. A missing packet let go before that
(missing for more than 1.5 s, or more than 512 behind the newest) keeps
only what an FEC mask said of it.

**Not known:** whether a browser's FEC masks and descriptors on a real
lossy uplink name enough of what is missing. Every packet lost in a burst
takes its own descriptor with it, and an FEC packet that was lost names
nothing; what is left is what the survivors say. Not seen on a call.

### The packets of the burst are no longer asked for

Decided: yes. When the key frame is asked for or owed, and when the burst
ended unrepaired on a stream nobody is being sent, the SFU stops asking the
publisher for the packets of that stream that are missing at that moment
(`StreamRx::give_up_missing`, the fifteenth change to the vendored
`str0m`; `UPSTREAM.md`). Packets that go missing afterwards, the key
frame's own included, are asked for as before. When nothing is asked (the
unanswered bound) the requests go on.

From the code: without it a packet is asked for up to 1.5 s after its
first request, so for up to half a second after the key frame request the
SFU would go on asking for the whole burst, 25 or 75 times a packet in all.
By the time of the request each of those packets has been asked for at
least twice on the recovered path and has not come; a key frame makes it
useless to every viewer; and each answer is a retransmission. From the
measurement below, with the capped publisher: after the burst the SFU
asked for a median of 2355 sequence numbers before the change (1280 to
3284) and 625 after (331 to 1238), and the publisher put a median of 195
retransmissions on the wire before (149 to 298) and 46 after (0 to 358).
Not measured: whether those retransmissions delay a key frame at a real
sender. A libwebrtc sender paces retransmissions ahead of new video
(recalled, not read for this), and on the call above its allowed rate had
just been cut to a quarter; the test's publisher has no pacer.

### Measured (`measure_burst_end_key_frame`)

`synthetic_client_e2e.rs`, ignored by default, run by name. The burst as
measured on the call: each media datagram the publisher sends, on both
pictures and their retransmission streams, lost with probability 0.80 for
one second (`LAKKO_DIP_P`, default 0.80 here; `measure_two_track_dip`
keeps its 0.70 and takes `LAKKO_DIP_P=0.80` too). Two plain senders, `f`
250 packets a second on the main track and `q` 50 on the light companion,
everything else of the SFU at its defaults, 20 seeds, each run twice:
**before** (the key frame at the end of a burst switched off) and
**after**. On a laptop over loopback, debug build, 2026-10-09, each table
run alone. **gap**: the longest interval with no decodable frame; **stop**:
what a viewer playing 300 ms late sees stand still; **asked**: when the
request reached the publisher, **first** / **whole**: when the first
packet of the key frame, and all of it, had reached the subscriber, all
three from the end of the burst. Milliseconds, min / median / p95 / max.

The test's subscriber never asks for a key frame itself, so "before" a
track that is not repaired stays broken to the end of the run (2.5 s after
the last frame); on the call a browser asked after about three seconds.

**The publisher throws queued retransmissions away above 15% of what it
sends** (`LAKKO_DIP_PUBLISHER=capped`, the default: upstream `str0m`'s
cap, a stand-in for a sender that does not repair a whole burst):

| | before | after |
|---|---|---|
| main gap | 1502 / 5969 / 6086 / 6851 | 1139 / 1217 / 1498 / 1530 |
| main stop | 1867 / 5642 / 5989 / 6546 | 790 / 889 / 1215 / 1294 |
| main decodable at the end of the run | 1 of 20 | 20 of 20 |
| main asked | never | 109 / 156 / 206 / 217 (20 of 20) |
| main key frame first packet | | 150 / 215 / 558 / 1334 |
| main key frame whole | | 169 / 233 / 628 / 1345 |
| light gap | 972 / 1259 / 1807 / 2064 | 792 / 1155 / 1263 / 2340 |
| light stop | 635 / 953 / 1487 / 1717 | 703 / 833 / 922 / 1996 |
| light decodable at the end of the run | 20 of 20 | 20 of 20 |
| light asked | never | 109 / 156 / 1384 / 1384 (12 of 20) |
| light key frame whole | | 154 / 204 / 2018 / 2018 |

**The publisher answers every request** (`LAKKO_DIP_PUBLISHER=all`, as in
`measure_two_track_dip`):

| | before | after |
|---|---|---|
| main gap | 338 / 640 / 1089 / 1093 | 435 / 833 / 1089 / 1089 |
| main stop | 560 / 671 / 747 / 752 | 521 / 637 / 748 / 748 |
| light gap | 210 / 367 / 595 / 680 | 189 / 346 / 803 / 940 |
| light stop | 24 / 148 / 368 / 415 | 34 / 172 / 461 / 600 |
| key frames asked for | 0 | 0 |

What the runs show, plainly:

- **With a publisher that repairs the burst, the change does nothing.**
  No key frame was asked for in any of the 20 runs, which is the rule
  working ("a burst that retransmission repaired asks for nothing"). The
  two columns of the second table are the same SFU behaviour; their
  difference is the spread of the model (a seed is not the same loss
  pattern twice: the publisher's retransmissions fall differently).
- **With a publisher that does not, the main track is back about 1.2 s
  after the burst began** (the second of the burst and about 0.2 s),
  where before it was not back at all without a viewer asking. The
  request reached the publisher 109 to 217 ms after the burst ended and
  the whole key frame the subscriber a median of 233 ms after it ended.
- **The light track gains little in this model**: its 40 or so lost
  packets were repaired in every run before the change (the cap had not
  yet bitten), a key frame was asked for in 12 of 20 runs after, and its
  median stop moved from 953 to 833 ms, which is within the spread.
- **Nothing here shortens the second of the burst itself.** A viewer
  300 ms behind still sees the picture stand for 0.8 to 0.9 s.
- **Two outliers, not explained**: in run 16 the main key frame's first
  packet reached the subscriber 1.2 s after it was asked for and the
  light track was asked for only 1.4 s after the burst; in run 18 the
  delays were 0.35 and 0.3 s. Thirteen further runs (seeds 16 to 18 and 1
  to 10 again, with `LAKKO_DIP_DETAIL=1`) showed none: the key frame was
  written 141 to 249 ms after the burst and was whole at the subscriber
  7 to 90 ms later. The test loop and the SFU share one thread; a stall of
  that thread would look like this. Not established.

Not measured, and not known: what a browser does. Whether Chrome answers
the PLI at once on a path it has just cut its rate on, how large its key
frame is at that rate and how long it takes to get through behind the
retransmissions already queued, and whether a viewer's decoder takes the
key frame up at once or has already asked for another. The live figure to
compare with, once deployed: the time from a burst's end to
`uplink-burst-end` in the log and the viewers' stall counters, against the
three seconds measured before.

### Tests

Unit, `burst_keyframe.rs`: a clean stream, a repaired burst (nothing), an
unrepaired burst (one answer, 120 ms after the stream is in sequence
again), a short burst (answered when its packets are 300 ms late, not
before; repaired in time: nothing), a second burst inside the wait, a lull
of four packets inside a burst, a stream that stops in a burst, a burst
longer than the window, bounds, a key frame that has started arriving,
each stream on its own, and the two FEC rules by number; since 2026-10-11
the measured case (half the stream FEC, 21 lost, three media among 14 left
missing: nothing by number, asked through the masks, asked through the
descriptors), a stream that loses only FEC packets (nothing, sixty in a row
included), a frame without its begin or end, a gap in the frame ids and
their wrap, a hole between consecutive frames, what cannot be classified
(ids that skip, a stream too young, a neighbour without a descriptor, FEC
inside a frame), what a mask may name, a named packet that came back or
was let go, and the bounds under arbitrary input. Unit,
`frame_descriptor.rs`: `packet_position`. Unit, `media.rs`:
`media_left_missing_on_a_stream_that_is_half_fec_is_seen_through_masks_and_descriptors`,
`a_burst_that_left_only_packets_nobody_needs_asks_the_publisher_for_nothing`,
`an_unrepaired_burst_asks_the_publisher_once_after_the_stated_time`,
`a_burst_that_retransmission_repaired_asks_the_publisher_for_nothing`,
`a_stream_no_subscriber_is_being_sent_is_not_asked_for_a_key_frame`
(nobody else in the call, a subscriber of another publisher, a track still
negotiating, a main track the subscriber does not show),
`a_burst_end_request_respects_the_rate_limit_and_the_unanswered_bound`,
`each_stream_of_a_publisher_is_asked_for_on_its_own`,
`a_key_frame_that_arrived_since_and_the_switch_off_ask_for_nothing`. End
to end: `an_unrepaired_uplink_burst_ends_with_a_key_frame_asked_by_the_sfu`
and `a_repaired_uplink_burst_costs_no_key_frame`. Vendored:
`lakko_given_up_packets_are_not_asked_for_again`.

## Downlink control (`LAKKO_SFU_DOWNLINK_CONTROL`, ADR-0070 step 6)

The SFU estimates what each subscriber's downlink carries and, when it is
short, gives up the larger video tracks first. Without it the SFU sends a
subscriber everything it asks for, whatever its path carries.

**Off unless `LAKKO_SFU_DOWNLINK_CONTROL=1`, and only under
`LAKKO_SFU_BITRATE_CONTROL=server`** (unset, empty or `0`: off; anything
else: the SFU refuses to start). With `client` bitrate control it is off
whatever the switch says. Off, nothing of this section happens. It is off
by default. **It was run against browsers once, on the live call of
2026-10-09 (four browsers, see "The fourth round"), and cycled on a
shared mobile downlink; what that call led to is built and host-tested
and has not been on a call since.** The web client reads `video-given`;
iOS and Android do since 2026-10-11 (unit-tested, not run on a device or
a call). Its acceptance tests
against synthetic `str0m` clients (below) failed an independent check
twice on 2026-10-06 (the first build dropped packets on the link once
settled; the second passed only because attempts with late publisher
frames were discarded); on the third build each of the five passed 3 of 3
alone in that check, with no attempt discarded.

### How it works

- **Negotiation per direction.** The transport-wide sequence number
  extension and `a=rtcp-fb:<pt> transport-cc` are negotiated on the media
  lines the SFU offers (the ones it sends on) and on none of the lines a
  peer offers (the ones it publishes on), which stay exactly as
  server-side bitrate control has them: `abs-send-time`, `goog-remb`, no
  transport-wide congestion control. Both directions share one transport.
  Upstream `str0m` negotiates the two per session, on every line alike, so
  this needed a change to the vendored crate
  (`DirectApi::set_twcc_on_local_media_only`, the twelfth; see
  `UPSTREAM.md`). A peer's own receive-only line is a line the SFU could
  send on and never does (it opens a line of its own for every track), so
  it carries neither. Asserted on the SDP in
  `downlink_control_negotiates_transport_cc_only_on_the_lines_the_sfu_offers`,
  `transport_wide_congestion_control_can_be_per_direction` and the
  end-to-end runs.
- **The estimate is the SFU's own**, not `str0m`'s. `str0m`'s send-side
  estimator and its pacer stay off (`enable_bwe` is not called); the
  first attempt at this step ran on it and failed, see "The first
  attempt". The SFU takes the records of the subscriber's transport-wide
  feedback from `str0m` (per packet sent: send time, size, reported
  arrival time or "not received"; the thirteenth vendored change,
  `DirectApi::keep_twcc_feedback` / `take_twcc_feedback`) and feeds them
  to `uplink_estimate::PathEstimator`, the estimator a publisher's uplink
  already runs (`UplinkEstimator` is the same type): the delay trend over
  send-time groups; over-use cuts to 0.85 of the rate **acknowledged as
  arriving**; a burst of loss with no delay growth changes nothing; loss
  that lasts 3 s steps the estimate down. The subscriber's arrival clock
  is placed on the SFU's by its first acknowledged packet; only
  differences matter. The budget is **unknown** until the feedback has
  acknowledged a packet; from then on it is this estimate and nothing
  else.
- **The estimate never stands above what the path has said.** Until the
  path has said anything (no cut kept, no probe ended) the estimate is
  the start value: 1.5 times the largest demand of the last 10 s, at most
  20 Mbit/s (`DOWNLINK_CEILING_BPS`). From the first kept cut on there is
  a limit (`Downlink::limit_bps`): a kept cut sets it to the larger of the
  cut estimate and the rate then acknowledged as arriving; a failed probe
  lowers it to just under the probe's target; a carried probe raises it
  to 1.1 times its target, and the estimate with it (fourth round: the
  set is given when what it needs is within the budget, and that had
  moved by the time the probe ended). **And the limit follows the set
  being given** (fourth round): when what that set puts on the wire has
  come to more than the estimate on a quiet path (no queue standing by
  the delay as it is or beyond the SFU's own sending, no cut pending,
  nothing lowered for 3 s, and not in the run right after the path itself
  cut the estimate), limit and estimate go to 1.1 times what is on the
  wire (`Downlink::follow_what_is_carried`). A path that is carrying a
  set is saying that it carries it; this is the downlink's form of the
  uplink rule "the estimate rises whenever no queue stands", bounded by
  what is in fact being sent. The estimator's own climb stops at the
  smaller of the demand ceiling and that limit. Before this rule the estimate
  climbed on its own after every cut that was taken back (a cut during a
  probe or in the second after it), to the demand ceiling of about
  18 Mbit/s on a 7.5 Mbit/s link, and the allocation's "what is on the
  wire fits the budget" test then held nothing back.
- **A cut needs a queue.** A cut always lands below what the set being
  sent needs, so on the downlink a false alarm costs a picture. A cut is
  kept only when a queue stands on the path: the smallest delay of the
  packets acknowledged in the last 200 ms is at least 25 ms above the
  smallest of the last 15 to 30 s (`DOWNLINK_STANDING_QUEUE`), and has
  been for 300 ms (`DOWNLINK_QUEUE_STANDS_FOR`). Otherwise the estimate
  is put back; if the queue then goes on standing, the cut is made after
  all (a full queue shows no more growth, so the detector would not speak
  again). Lasting loss needs no queue. On a real cut the retransmission
  caches of the subscriber's main and middle tracks are emptied: in the
  first runs the link lost about 1500 retransmissions in the second after
  the first cut.
- **A queue the SFU's own sending built is not that queue** (third
  round). The SFU forwards a packet when it arrives (ADR-0066), so a
  publisher whose frames stall and then arrive together reaches every
  subscriber as a clump, and a clump stands in the queue of any path for
  a while. Three rules keep that from costing a picture:
  - *The model.* For every packet the feedback speaks of, the SFU works
    out how long it would have waited on a path that carries exactly the
    estimate, given when and how much the SFU sent before it
    (`Downlink::explained_delay`). A cut is judged by the queue **beyond**
    that (`excess_queue`); a probe is still judged by the queue as the
    delay shows it.
  - *The shadow.* The model is not exact (in the e2e test, debug build,
    36 ms of an 84 ms queue stayed unexplained while the SFU was writing
    the clump out to four peers). When a packet would have waited 50 ms
    (`DOWNLINK_CLUMP`) and the path has carried what is being sent
    quietly for at least a second of the last four, no queue counts for
    1 s (`DOWNLINK_CLUMP_SHADOW`), at most once in 3 s
    (`DOWNLINK_CLUMP_ONCE_PER`). The key frames a call starts with are a
    clump too; they get no shadow, because nothing is known of the path
    yet.
  - *The first cut is held.* On a path that has been carrying what is
    sent quietly, the first kept cut does not lower the estimate; it is
    made if the queue stands for another 300 ms. (A cut is to 0.85 of
    what arrived in the last half second, which after a 200 ms gap in
    what was sent is about half of what the path carries.) At most once
    in 5 s.
  What this costs: a path that really got narrower during a clump is
  noticed up to 1 s later, and on a steady path the first real cut comes
  300 ms later. No packet is delayed: smoothing the clump toward the
  subscriber was considered and not built: it would hold media back by
  up to the length of the stall, and the packets wait that long either
  way, in the SFU or in the path's queue. What it would save is the
  path's queue overflowing: a clump of all three publishers in the e2e
  test comes to 136 ms of the link's 150 ms queue.
- **A deep queue is cut on without the detector.** A queue 75 ms beyond
  the sending's own (`DOWNLINK_DEEP_QUEUE`) that has stood 300 ms cuts
  the estimate even when the delay detector never spoke, once per
  300 ms. Found on a 2.35 Mbit/s link under 12 Mbit/s of demand: the
  queue filled inside the detector's first window, showed no growth
  after, stood at 108 ms for 28 s, and the SFU cut nothing and answered
  about 3600 retransmission requests a second.
- **The allocation** (`src/downlink_allocation.rs`, pure): audio and every
  light track come first (a light track counted twice while each of its
  packets is sent twice) and are never given up; then, in the subscriber's
  order of importance, each publisher gets the heavy track it asked for if
  what is left of the budget covers its measured rate times 1.2
  (`HEADROOM_NUM` / `HEADROOM_DEN`), else the next smaller one that fits.
  An unknown budget gives everyone what they asked for.
  **Down at once**, when what is being given no longer fits the budget as
  it is on the wire (the headroom is asked for to give a track, not to
  keep it). **Up only on proof**: a publisher is moved up only when the
  whole set after the move, as it is on the wire, is no more than what the
  path was seen to carry: the rate of a probe that just passed, or what is
  arriving on a quiet path (no probe running, no over-use, no trouble for
  3 s). The estimate alone never gives anything back, so the weakness of
  the first attempt (a step reached by arithmetic) does not exist.
  **Nothing is given while a queue stands** on the path or a cut is
  pending, whatever was proved a moment before (fourth round). A
  publisher that has just joined starts at its light track. **While the
  set being given fits it is kept as it is**; only a change of the
  subscriber's order of importance re-shares what the set needs, at
  once (fourth round: re-sharing at every run took a picture from a less
  important publisher whenever a more important one's larger track
  happened to cost less for a moment). A track's measured rate is the mean
  of what arrived on it from its publisher in the last two full seconds
  (from a quarter of a second on while it has none yet), zero after two
  silent ones: with one second alone, the second a publisher stalled in
  made a larger set look affordable and a probe started toward it. It
  runs every 100 ms per subscriber.
- **Probes** (`src/downlink_probe.rs`, pure). What arrives cannot lead
  the estimate up while the heavy tracks are paused, so before the next
  larger set is given the SFU tries its rate on the path: for 1 s
  (`PROBE_DURATION`) it adds padding to the media so that the two
  together run at what that set needs **with** its headroom. Padding is
  never new media: copies of video packets already sent to that
  subscriber, on the retransmission stream (`StreamTx::resend`), which a
  receiver discards by sequence number.
  **A probe climbs.** The rate rises in a straight line from what is
  already on the wire to the target over the first 400 ms (`PROBE_RAMP`)
  and the verdict is taken over the rest.
  **A probe cannot fill a queue while feedback is silent.** Padding is
  held back whenever the bytes sent since the newest packet any feedback
  has reported exceed what the path delivers in the time feedback has
  been silent plus 40 ms (`PROBE_QUEUE_ALLOWANCE`). "What the path
  delivers" is the rate acknowledged in the probe's own reports; the
  silence counted is at most the feedback lag measured on that subscriber
  while its path was quiet (send to report of the same packet, between 30
  and 400 ms, 150 ms until measured). The bound uses no delay figure and
  needs no report to arrive: with none, padding stops after that much.
  A probe whose padding was held back by more than 20 ms worth of its
  target (`PROBE_SHORTFALL`) has not shown the rate and ends `short`.
  **Verdict.** The set is given only if, of the packets sent on the
  probe's plateau, at least 90% of the target rate was acknowledged and
  at most 10% were reported missing, and no queue grew: a queue 25 ms
  deeper than at the probe's start in two reports running ends the probe
  at once (`trouble`). A probe that is carried also resets the delay
  baseline (see "Seen, not explained").
  **Which set.** The most important publisher's next track; once a probe
  has failed, the cheapest single move the waits allow, so a set the path
  does carry is reached before the one it does not is tried again.
  **When.** One probe at a time per subscriber; none while a cut is
  pending, while a queue stands or stood in the last 3 s, or while the
  detector says over-use; none for 2 s after the path itself cut the
  estimate (`PROBE_AFTER_TROUBLE`); after a failed probe the next toward
  that rate or more waits 5 s, then 8 s every time (`PROBE_FIRST_RETRY`,
  `PROBE_MAX_RETRY`), one toward at most four fifths of it 1 s; after a
  carried one 0.5 s (`PROBE_AFTER_SUCCESS`). At most 20 Mbit/s
  (`PROBE_MAX_BPS`), padding in steps of at most 20 ms worth of the
  rate, at most 64 copies a pass. **No padding on top of a clump:** while
  the media just written would itself wait 25 ms on a path that carries
  the estimate, no padding is added, and a clump of 50 ms while a probe
  runs ends the probe as `disturbed`: nothing is held against the rate,
  the limit does not move, and the next probe may start 2 s later (a
  probe that a late clump ended as `trouble` cost the full 8 s wait).
  While a probe runs a clump is measured against the rate the probe is
  trying, when that is more than the estimate, and no probe starts while
  a clump is going out (fourth round).
  **A set that proved too large waits longer each time** (fourth round):
  a set that was given and taken away again by the path within 20 s
  (`TOO_LARGE_WITHIN`) is not tried again, and not given on any proof,
  for 10 s, then 20, 40 and 60 s every time (`TOO_LARGE_FIRST_WAIT`,
  `TOO_LARGE_MAX_WAIT`). "That set" is which pictures it holds, not what
  they cost at the moment (`downlink_allocation::set_level`: one per
  middle picture, six per main one): that set and every larger one is
  held, a smaller one is not. A set that large or larger that stays
  20 s, or five minutes without a set proving too large
  (`TOO_LARGE_FORGOTTEN_AFTER`), puts the wait back to 10 s. A probe
  that fails is not this (nothing was given): its retry stays 5 s, then
  8 s. Nor is a path that collapsed under the set: when the cut left the
  estimate under half of what the set put on the wire
  (`DOWNLINK_COLLAPSE_DEN`) the path stalled under whatever was being
  sent, which says nothing of the set, and it is tried again as soon as
  the path is quiet.
  A cut the estimator makes while a probe
  runs, or within 1 s after it, is taken back: a failed probe changes the
  limit and when the next may start, never what is being given.
  All constants chosen, not measured.
- **Applying it.** The given quality replaces the asked-for one in the
  per-track gate (`MediaPeer::video_quality_given`): pause is immediate,
  resume starts at a key frame, as in step 5. A track paused under
  downlink control has its retransmission cache emptied.
- **Telling the subscriber.** `ServerFrame::VideoGiven { given: [{ peer,
  quality }] }`, kind `video-given`: the whole set, every publisher the
  subscriber has a video track of, most important first, at most 64
  entries; sent when the set changes and after every `video-wants`.
  Example:
  `{"kind":"video-given","given":[{"peer":"p1","quality":"main"},{"peer":"p2","quality":"middle"},{"peer":"p3","quality":"light"}]}`.
  On UniFFI `SfuServerFrame::VideoGiven { given: Vec<SfuVideoGiven> }`
  with `SfuVideoGiven { peer: String, quality: TileVideoQuality }`; on
  wasm `validateSfuServerFrame` returns `"video-given"` and
  `parseSfuVideoGiven(json)` returns `[peer0, quality0, peer1, ...]`.
- **Visible.** Info lines: `media: what a subscriber is given of a
  publisher's video changed` (`subscriber`, `publisher`, `from`, `to`,
  `wanted`, `budget_kbps`, `carried_kbps`, and since the fourth round
  `why`, `wire_kbps`, `need_kbps` of the set now given and
  `standing_queue_ms`; `why` is `probe` or `quiet-path` for a move up,
  and for a move down `path-cut` (the path lowered the estimate under
  the set), `over-budget` (the set outgrew the estimate with a queue
  standing and no cut yet), `importance`, `wants`, or `first-listed`: a
  publisher met while the budget is known starts at its light picture
  and the line reads `from` what was asked for, though nothing was taken
  from the viewer); `media: trying the next larger set on a subscriber's
  downlink` (`target_kbps`, `budget_kbps`); `media: a probe of a
  subscriber's downlink ended` (`outcome`: `carried` / `trouble` /
  `loss` / `short` / `no-feedback` / `disturbed`, `target_kbps`,
  `carried_kbps`, `padding_bytes`, `held_back_bytes`,
  `standing_queue_ms`); `media: the estimate of a subscriber's downlink
  was lowered` (`reason`, `to_kbps`, `standing_queue_ms`,
  `beyond_sending_ms`, `taken_back`, `held_at_steady`, and since the
  fourth round `from_kbps`, `arriving_kbps` (what the feedback
  acknowledged over the last half second) and `sending_kbps`, so that a
  cut can be read as "the path was short of what was sent" or not);
  `media: a set given to a subscriber's downlink proved too large`
  (`level`, `need_kbps`, `wait_secs`). With `LAKKO_SFU_MEDIA_DIAG_SECS` set, one
  line per subscriber per interval: `media: subscriber downlink over the
  last interval` (since the fourth round with `limit_kbps`,
  `held_from_level`, `too_large_wait_secs`, `followed`). Counters:
  `lakko_sfu_downlink_given_changes_total{direction}`,
  `lakko_sfu_downlink_probes_total{outcome}`,
  `lakko_sfu_downlink_probe_padding_bytes_total`.

### Measured (2026-10-05 and 06)

Three publishers, each sending main 3.1 Mbit/s, middle 0.5 and light
0.15 as three declared senders at 25 frames a second, and one subscriber
asking for the main picture of all three (12.1 Mbit/s with headroom),
through the production media task under `server` bitrate control. The
subscriber's downlink is a link model in the test: a rate, a 150 ms
drop-tail queue, optionally a fixed delay and jitter after it, optionally
a dip. Synthetic `str0m` clients, debug build, one idle machine, **no
browser**. **The tables in this section are from the build before the
second round; what the current build did is under "The second round".**
Each test was run three times alone on that build
(`cargo test ... <name> -- --nocapture`); all fifteen runs passed, and an
independent check then failed (see the head of this section).

**A 7.5 Mbit/s downlink**, lifted after 40 s
(`a_narrow_downlink_gives_up_the_larger_pictures_and_gets_them_back`).
It carries one main and two middles (5.9 Mbit/s with headroom, 5.1 on
the wire); two mains and a middle need 9.0.

| Run | First downgrade | Settled on main, middle, middle | Link drops in the first 2 s / after | All main after the lift |
|---|---|---|---|---|
| 1 | 0.67 s | 4.7 s | 802 / 0 | 7.1 s |
| 2 | 0.67 s | 5.2 s | 725 / 0 | 4.6 s |
| 3 | 0.66 s | 3.8 s | 718 / 0 | 6.9 s |

Run 1 in full (`given` is main / middle / light publishers; the times of
cuts and probes are from the SFU's log, whose clock started about 0.3 s
before the test's, the times of what was given are the test's):

| Time | What happened |
|---|---|
| 0 s | given 3/0/0: budget unknown |
| 0.67 and 0.88 s | cuts with a queue of 18 and 97 ms, taken back (not yet standing 300 ms) |
| 0.98 s | cut to 5.6 Mbit/s, queue 106 ms, kept; given 1/0/2 (0.67 s by the test's clock) |
| second 0 | the link dropped 334 main packets, 28 light, 23 light second copies, 417 retransmissions; none in any later second |
| 3.0-4.1 s | probe toward 5.3 Mbit/s carried (5.35 acknowledged, 169 kB of padding); given 1/1/1, then 1/2/0 at 4.7 s |
| 5.1 s | probe toward 9.0 failed after 0.33 s (queue 41 ms, 166 kB) |
| 10.5, 18.9, 27.4, 35.8 s | probes toward 9.0 failed after 0.30, 0.34, 0.30, 0.30 s (150 to 175 kB each); given 1/2/0 throughout, nothing dropped |
| 40.0 s | limit lifted |
| 44.2-45.3 s | probe toward 9.0 carried (508 kB); given 2/1/0 |
| 46.3-47.4 s | probe toward 12.4 carried (607 kB); given 3/0/0 |

Padding in that run: 2.09 MB in 50 s, of which 1.1 MB in the two probes
that got the main tracks back. While the limit stood: about 160 kB every
8 s. From a second after it settled until the lift, every light packet
was handed to the subscriber (asserted in every run).

**The same link with 50 ms of one-way delay and up to 10 ms of jitter**
(`a_narrow_downlink_with_delay_and_jitter_settles_the_same`): first
downgrade at 0.75, 0.77, 0.76 s; settled at 5.2, 5.2, 5.4 s; drops 868 /
0, 888 / 0, 971 / 0; all main 5.3, 5.2, 5.4 s after the lift.

**An unlimited link**
(`a_subscriber_on_an_unlimited_downlink_is_never_downgraded`): three
mains from the first frame to the last in all three runs; no probe
(nobody was given less than asked).

**The dip of ADR-0070 on an unlimited link** (30% of media datagrams
arriving for one second, from the tenth second;
`a_dip_on_an_unlimited_downlink_does_not_leave_the_subscriber_downgraded`,
which allows 8 s in all): no downgrade in the three runs. In six more
runs of the final code, five had none and one gave less than three mains
for 5.15 s (main, main, middle at 12.02 s, main, middle, middle at
12.22 s, back to main, main, middle at 15.06 s and to three mains at
17.17 s): the retransmissions after the dip put a queue of about 33 ms
on the path between the SFU and the test's subscriber for more than
300 ms.

**Downlink control off, the same 7.5 Mbit/s link**
(`downlink_control_switched_off_changes_nothing`): no `video-given`, no
transport-wide congestion control in any SDP, everything goes on being
sent; 65 to 78% of the bytes that got through were on retransmission
streams and the test's own loop fell seconds behind under the flood.

### The second round (2026-10-06)

The independent check's failing run showed a probe toward 9.0 Mbit/s that
put 247 kB of padding on the 7.5 Mbit/s link before the feedback that
would have ended it arrived, with the budget reading 13.7 to 18 Mbit/s.
Three causes, each changed in the SFU:

1. **The budget stood above the link** because the estimator climbs by
   its own rule after a cut, cuts made around a probe were taken back to
   the estimate from before the probe, and the only ceiling was 1.5 times
   demand (about 18 Mbit/s here). Now the limit described above. Seen
   after the change, in the per-second timelines of the runs: 5.8 to
   6.8 Mbit/s while the link was 7.5, and 9.0 to 9.9 only after a probe
   toward 9.0 had been carried once the limit was lifted.
2. **A probe could overflow the queue** because it started at its full
   rate and was stopped only by feedback. Now the climb and the bound on
   unreported bytes described above. In the unit test
   `a_probe_above_the_path_builds_a_bounded_queue` a probe toward
   8 Mbit/s on modelled paths of 4, 5, 6 and 7 Mbit/s never queued more
   than 80 ms and ended `short`; `padding_stops_when_feedback_does` shows
   the bound with no report at all. In the runs, failed probes toward 9.0
   ended after 0.3 to 0.6 s with 130 to 210 kB of padding (`trouble`) or
   ran their second with 64 to 85 kB (`short`), and the link dropped
   nothing.
3. **Seconds in which the link was handed 1.5 times the usual number of
   packets were not the SFU's sending.** The timeline now prints, per
   second, the first transmissions the SFU sent on each track and what
   the test's publishers wrote. Around a probe the SFU's column stayed at
   350 / 250 / 150 while "handed to the subscriber" read 336 and then
   364: the link's queue moving packets across the second's edge. Where
   the SFU's own column moved (336 / 237 / 144, then 368 / 276 / 161) the
   publishers' column showed frames written late (23 frames, then 27; the
   latest 118 to 132 ms late): the test's publishers, on a loaded
   machine. The SFU forwards a packet when it arrives (ADR-0066) and has
   no pacer; none was added.

Also changed: no probe while a cut is pending or a queue is younger than
3 s (one started into the first second's overflow); the cheapest allowed
move after a failure and the two-report rule (one run with delay and
jitter stayed on main, middle, light for the whole 40 s); the feedback
lag and its floor are measured per subscriber instead of a constant 30 ms
(on the 50 ms path every probe after the lift ended `short`).

Runs on the current build, debug, one machine with a browser and other
sessions running (load average about 6.5), all with
`LAKKO_SFU_DOWNLINK_CONTROL` set by the test:

| What | Runs | Passed | Figures |
|---|---|---|---|
| narrow test alone by name (`--ignored --exact`) | 10 | 10 | settled 3.7 to 5.5 s; all main 6.0 to 7.9 s after the lift; 644 to 942 link drops in the first 2 s, 0 after |
| the same with 50 ms delay and 10 ms jitter, alone | 10 | 10 | settled 3.8 to 5.8 s; all main 7.0 to 8.9 s after the lift; 0 link drops after the first 2 s |
| dip on an unlimited link, alone | 3 | 3 | two runs never downgraded; one had less than three mains for 5.54 s (bound 8 s) |
| unlimited link, alone | 3 | 3 | never downgraded |
| the four-package `cargo test` with the two narrow tests un-ignored, first batch | 3 | 0 | all three failed with "too loaded to measure in 4 attempts": every attempt was discarded, no assertion about the SFU was reached |
| the same after the tests were made to take turns (below), six attempts | 3 | 3 | narrow: settled 3.8 to 5.5 s, all main 6.3 to 7.8 s; with jitter: settled 3.9 to 5.9 s, all main 6.9 to 8.6 s; 0 link drops after the first 2 s |

Four of the ten narrow runs alone and two of the ten jitter runs needed a
discarded attempt. Of twelve discarded attempts looked at, eleven had no
link drop after the first two seconds; the twelfth was a 15 s stall of
the machine. Earlier builds of this round failed runs: the jitter test
0 of 8 (probes `short` after the lift), then 9 of 10 (a cut caused by
publisher frames written 118 to 132 ms late), then 7 of 8 (stuck on
main, middle, light); each led to a change named above.

**The two narrow tests stay `#[ignore]`d and the switch stays off**; that
is the maintainer's to change. Twenty-six passing runs on one machine
against a synthetic client do not show the link cannot drop a packet.

### The third round (2026-10-06): a publisher's clump

The second round's tests discarded any run in which a publisher's frame
was written more than 80 ms late. An independent check found that the
four attempts so discarded were the ones that would have failed (a
subscriber left on main, middle, light 20 s after the lift; all main
17 s after it). A publisher whose uplink stalls for 150 to 250 ms and
then delivers its frames together is an ordinary condition, so the
discard hid the case that matters. It is gone; the condition is now made
on purpose.

**What a clump triggered**, reproduced with every publisher's frames
withheld for 200 ms (`DL_CLUMP`) and written together:

1. *A kept cut from the queue the clump itself built.* 1.0 Mbit of
   clump is 136 ms of queue on the 7.5 Mbit/s link; it stood above 25 ms
   for more than 300 ms, and the cut was kept at 4.8 Mbit/s (queue 73 ms)
   with the set on the wire needing 5.1: the main picture was taken away
   0.3 s later and came back 2.6 s after that by a probe; in another run
   the steady set was back 6.3 s after the clump.
2. *A limit set far too low.* The cut is to 0.85 of what arrived in the
   last half second, and the half second held the stall: 2.6 Mbit/s in
   the independent check's run, on a path that had been carrying 5.1.
3. *And then no way back* (the run that ended on two middles and a
   light): every probe from a small set ended `short`. The second copies
   of the light packets were counted in what the probe had sent **and**
   taken out of its schedule, so every probe ran short of its target by
   their rate, 0.47 Mbit/s: nothing at 9 Mbit/s, a third of the target
   at 1.5. No probe toward three middles could pass on any path. This
   was a defect of the second round's probe, not of the clump.
4. *A probe started in the stall.* With a track's cost measured over one
   second, the second of the stall made two mains look like 7.1 Mbit/s
   instead of 9.0; a probe started toward it, and its padding on top of
   the clump overflowed the link's queue by two packets (one of twelve
   runs of an intermediate build).
5. *A probe ended by a clump* counted as a failure of its rate and cost
   the 8 s wait (a run of an intermediate build settled after 13.9 s).

Fixed in the SFU as described above (the model of the SFU's own sending,
the clump's shadow, the held first cut, the two-second cost of a track,
no padding on a clump, `disturbed` probes), and in the probe (second
copies counted once; a probe held back during its climb passes when the
whole target was acknowledged on the plateau; the delivered rate that
bounds the padding rises at once instead of by halves).

**Found on the way, with a link that carries a fifth of the demand**
(2.35 Mbit/s, `a_very_narrow_downlink_gets_everything_back_when_it_is_lifted`):
the SFU never cut at all (now the deep-queue rule); an intermediate
build of that rule cut at every pass of the media loop, 1089 times in a
second, to the 30 kbit/s floor (now once per 300 ms, and to a fraction
of the rate as of the last arrival, not "as of now"); and with an
estimate that low the padding rule saw every media packet as a clump
(the model's rate is now never below what is steadily written).

**Runs on the final build** (debug, one machine, load average 3 to 4
during the batch, each test alone by name with a 420 s kill):

| Test | Passed | Settled | All main after the lift | Link drops after 2 s | Discarded attempts |
|---|---|---|---|---|---|
| narrow | 10 of 10 | 3.7 to 5.6 s | 5.8 to 7.5 s | 0 | 1 (loop stood 307 ms) |
| narrow, delay and jitter | 10 of 10 | 5.5 to 5.8 s | 6.1 to 6.7 s | 0 | 1 (266 ms) |
| clumps (one publisher at 20 s, all at 30 s, all 0.3 s after the lift) | 10 of 10 | 5.4 to 5.5 s | 5.8 to 6.1 s | 0 | 0 |
| clumps, delay and jitter | 10 of 10 | 5.5 to 5.8 s | 6.1 to 6.8 s | 0 | 0 |
| very narrow (2.35 Mbit/s, lifted at 20 s) | 3 of 3 | on two middles and a light | 9.4, 9.5, 11.1 s | 0 after the first 8 s | 0 |
| dip on an unlimited link | 3 of 3 | never downgraded | | | 0 |
| unlimited link | 3 of 3 | never downgraded | | | 0 |

In all twenty clump runs no `video-given` frame followed a clump before
the lift: the set did not move. The two discarded attempts had the same
timelines as passing ones.

The four-package `cargo test` with the five step-6 tests un-ignored for
the run: 3 of 3 passed (46 passed, 2 ignored in
`synthetic_client_e2e.rs`). In them: narrow settled 5.5 s, all main 5.9
to 6.3 s; with jitter 3.8 to 5.6 s and 6.4 to 7.9 s; clumps, both
variants, no `video-given` after a clump before the lift, all main 5.8
to 6.6 s; very narrow all main 8.4 to 10.7 s; no link drop after the
first two seconds anywhere. One attempt was discarded: the very narrow
test's first, whose loop stood still for 7.1 s (the first seconds of a
link that carries a fifth of the demand, with the rest of the file
starting beside it; not looked into further).

Runs of intermediate builds of this round that failed, each leading to
a change above: 11 of 12 passed (the four narrow and clump tests, three
runs each; the failure was the probe started in the stall); 39 of 40
(ten runs each; the failure was the probe ended by a clump, settled
after 13.9 s); very narrow 2 of 3 (the cut at every pass); the plain
clump test 9 of 10 (the first cut a second late behind the key frames'
shadow, then the cut to the floor);
and one batch that was killed after a run was found alive 2 h 13 min in
(see below), on a machine whose load average was 25 to 40 from other
work: its two finished attempts had the test's loop standing still for
2.2 and 2.4 s, and in one the subscriber went to three light pictures
at 34 s. That was the lasting-loss rule: for more than two seconds
nothing was handed to the subscriber at all (the timeline shows 0
packets in seconds 32 and 33, frames written up to 2.9 s late), the
estimate was stepped to the floor for loss, and the probes brought the
pictures back from 37 s on. A subscriber whose path delivers nothing
for seconds is given the light pictures; that is the rule working, on a
stall the machine made.

**What remains weak**

- A clump is forwarded as it arrives. All three publishers stalling
  together for 200 ms is 136 ms of the test link's 150 ms queue; a
  longer stall, or a shorter queue, overflows it, and nothing in the SFU
  prevents that.
- In a clump's shadow (1 s) and behind a held first cut (300 ms) a path
  that really got narrower is noticed late. Clumps more often than once
  in 3 s get no shadow and are judged by the model alone, which in the
  debug build left 36 ms of 84 unexplained.
- After a failed probe the next toward the same rate waits 8 s, so a
  limit lifted just after a failure is found 8 s later; the very narrow
  link was back on three mains 9.4 to 11.1 s after the lift, and from
  three lights the same walk can exceed 15 s.
- A path that delivers nothing for about three seconds sends the
  estimate to its floor (the loss rule); the pictures come back one
  probe at a time.
- Twice after the lift a run gave the third publisher its main picture
  for about 0.1 s before the second (`MmM` then `MMm`): the order of
  importance is not kept for a moment when two moves are affordable.
  Not looked into.
- The frame-rate of feedback, the size of real clumps and everything
  else about a browser are still unknown; no browser has been run.

### The fourth round (2026-10-09): the first live call

**The call.** 18:28 to 18:35 UTC on the test stack, four browsers,
downlink control on for the first time against browsers: Chrome on a
home Wi-Fi whose uplink loses about four packets in five for about a
second every 30 s (`P18` below), Firefox (`P11`) and Chrome (`P21`) on
one laptop sharing one mobile link, each sending three camera senders up
and receiving three people down, and Edge (`P31`) on the same Wi-Fi as
`P18` from 18:30:10 to 18:33:21. Four people, so every tile asks for the
middle picture. Everything below is from the SFU's log of that call and
from the viewers' own counters; nothing was captured on the mobile link.

**What it did.** On the two subscribers behind the mobile link the
allocation cycled. `P11`: 13 log lines "middle to light" and 13 back in
six and a half minutes, 28 probes (13 carried, 8 `disturbed`, 6
`trouble`, 1 `short`), the budget between 145 and 5019 kbit/s; from
18:30:17 to the end of the log at 18:35:04, 89 of 287 s on less than
what it asked for, in four stretches of 3, 47, 6 and 33 s. `P21`: 7 and
7. The two on Wi-Fi: `P18` 2 and 2, `P31` 3 up and none down. By the
viewers' own counters (as reported with the log, not in it) Firefox
showed the light picture in 64 of 172, 40 of 94 and 34 of 159
two-second intervals for its three peers.

**What it was not.** Not cuts on delay variation: the delay detector
spoke of over-use 292 times on `P11` and 323 on `P21`, and 282 and 317
of those cuts were taken back because no queue stood; every one of the
ten and six that were kept had a queue of 103 to 627 ms. The downlink
reads the queue at the moment of the decision with the shared
`QueueWatch`, as the uplink does. (The downlink estimator is not built
`confirming`: the confirmation is `Downlink::feed`'s, with the same
constants.) What the uplink has and the downlink lacked is the second
rule, "the estimate rises whenever no queue stands": see cause 3.

**The causes**, with the lines of `P11` (and of `P21`) each accounts
for:

| Cause | `P11`, of 13 | `P21`, of 7 |
|---|---|---|
| 0. Not a downgrade: a publisher listed for the first time starts at its light picture, and the line read `from="middle"` (what was asked for). | 2 (18:29:26.98, 18:30:15.58) | 1 (18:30:15.52) |
| 1. The mobile link stalled: a queue of 103 to 627 ms on the downlink at the cut, and in the same seconds a queue of 88 to 857 ms on the same two browsers' uplinks. The cut is to 0.85 of what was acknowledged in the last half second, which in a stall is little: 130, 178 and 715 kbit/s in three of them, and everything was taken. | 5, in 3 episodes (18:32:05.47 two, 18:32:14.47, 18:34:04.31 two) | 3 (18:32:05.47, 18:32:06.07, 18:32:14.99) |
| 2. The same stall, a second or four after a picture had been given: at 18:32:34.56 `P11` was given a middle picture half a second after its probe was carried (1585 of 1593 kbit/s), with a queue of 98 ms already standing, and lost it at 18:32:35.56 (270 ms). `P21` the same at 18:32:35.33, 4.1 s after its own. | 1 | 1 |
| 3. No cut at all: the set being given had come to cost more than the budget, which was pinned at a limit (2041 kbit/s since the cut of 18:29:18; 2761, a tenth over what a probe had tried 68 s before). At 18:30:16.49 a fourth person had just joined (key frames for it, one more light picture); at 18:31:24.46 the publishers' rates had climbed (demand 3069, the highest so far). A probe gave the picture back 0.2 and 3.1 s later. | 2 | 0 |
| 4. No cut at all: the allocation re-shared what the set needed, in order of importance, at every run. The more important publisher's middle track had come to cost less than the one a less important publisher held; the latter lost its picture and the former got none (it waits for proof). 18:32:47.92 on `P11` (the other publisher got the picture 1.2 s later by a probe). On `P21` at 18:32:40.63 (a swap in one run) and 18:32:43.96, which cost it one publisher's picture for 11 s; for the second the log cannot exclude cause 3. | 1 | 2 |
| 5. A clump the SFU forwarded: `P18`'s uplink burst ended at 18:33:46.77 and the SFU asked it for a key frame; from 18:33:47.2 the queue on both mobile subscribers went to 278 ms, at first all of it explained by the SFU's own sending, then 125 to 135 ms beyond it; cuts were kept at 48.43 and 49.76. No queue on the mobile uplinks in those seconds, and between the two cuts the queue fell to 15 ms with the same set on the wire. The set had stood 59 to 61 s. Each subscriber's model explains its own share of a clump; two subscribers behind one link each put the other's share on it. | 2, in 1 episode (18:33:49.76) | 0 (its cut at 48.43 took nothing; the one at 49.76 was taken back) |

So of `P11`'s eleven real downgrades six were a stall of the mobile link
itself (causes 1 and 2), which the path answered for, three were the
allocation's own doing with nothing wrong on the path (3 and 4), and two
were a clump from another publisher's uplink (5). For none of the
eleven was the path too narrow for the set in the time before it: in
the interval lines the feedback had acknowledged more than was being
sent (2.3 to 3.6 Mbit/s against 1.7 to 2.0 in the half minute before
18:32:05). It was too narrow during the stalls, which lasted from a few
seconds (18:34:02 to 18:34:06) to about twelve (18:32:04 to 18:32:16).

**And the way back was slow** for reasons of its own. After the stall of
18:32:14 `P11` stood on three light pictures until 18:32:34: a probe
ended `disturbed` 8 ms after it started and another after 3 ms (key
frames going out; with the estimate at 130 kbit/s the model of the SFU's
sending drained at 0.6 Mbit/s and a key frame of a light picture was a
"clump"), one was given up at its very end after 1460 of 1739 kbit/s had
been acknowledged, and one ended `short` at 1486 of 1559 (95%) and cost
the 8 s wait. After 18:34:04 three probes were carried one after the
other (872, 966, 1084 kbit/s) before one picture was given: the set
needed a little more each time than the probe before it had tried, and
the budget stood exactly at the probe's rate. Of `P11`'s thirteen
carried probes two gave nothing and four gave 0.2 to 0.5 s late.

**Changed** (`downlink_allocation.rs`, `downlink_probe.rs`, `media.rs`;
all constants chosen):

1. *The limit follows the set being given* (cause 3):
   `Downlink::follow_what_is_carried`, described under "How it works".
2. *A set that fits is kept as it is* (cause 4): the allocation
   re-shares only when the subscriber changed its order of importance.
3. *Nothing is given while a queue stands or a cut is pending* (cause
   2), whatever a probe showed a moment before.
4. *A carried probe leaves the estimate a tenth above its rate*, as it
   always left the limit, so the set it was run for is given when it
   ends.
5. *A clump is measured against the rate a probe is trying*, and no
   probe starts while one is going out.
6. *A set that proved too large waits longer each time*: given, and
   taken away again by the path within 20 s, it is not tried and not
   given for 10, 20, 40, 60, 60 s; nor is any larger one, and a smaller
   one is. (A set is which pictures it holds. A first build kept the
   wait by the rate the set needed, with "smaller" as four fifths of it;
   the rate of one and the same set moved by more than that with the
   publishers' encoders, on the call as on the model, and the set was
   given again 15 s into a wait of 40.) Not when the path collapsed
   under it:
   a cut that leaves the estimate under half of what the set put on the
   wire is a stall of the path, not the set's doing, and the set is
   tried again as before. (The first build of this rule counted every
   taking-away, and on the model the three dips, 17 to 19 s after the
   last picture had come back, cost 10, 20 and 40 s of waiting: 41 of
   87 s on three middles where the build before had 64.) Of the call's
   episodes cause 2 is the case (taken 1.0 s after given, the cut to
   1013 kbit/s under a set of about 1.3 Mbit/s); the stall of 18:34:04
   (taken 9 and 11 s after given, the cut to 715 under about 1.5 Mbit/s)
   is on the line between the two and counts as a collapse; the others
   had stood 38 s and more. A path that widens while a wait runs is
   found when it ends, 60 s at the worst; a failed probe's retry
   (nothing was given) stays 5 s, then 8 s.
7. *The log says why.* Every line the reconstruction above needed and
   did not have: `why`, `wire_kbps`, `need_kbps` and the queue on every
   change, `from_kbps`, `arriving_kbps` and `sending_kbps` on every cut,
   the queue on every probe's end, the limit and the wait in the
   interval line. Cause 0 now reads `why="first-listed"`.

**Not changed, and why.** A stall is still answered at once with
everything the cut calls for: holding the middle pictures through the
first second of a stall would keep them through a short one, and on a
path that has really narrowed it is a second more of queue in front of
the light pictures. The cut is still to 0.85 of what was acknowledged in
the last half second, which in a stall is next to nothing. The clump
model is still per subscriber (cause 5).

**The model** (`a_mobile_like_downlink_holds_a_picture_instead_of_cycling`,
`MobileLink` in `tests/synthetic_client_e2e.rs`; every figure chosen to
resemble that log, none measured on a mobile network). Three publishers
that send a key frame of six frames' size on the middle and light
senders every 30 s; a subscriber asking for the middle picture of all
three; its downlink 4 Mbit/s with a two-second queue, 30 ms of delay,
5 ms of jitter and a wave of 40 ms of extra delay twice a second.
*0 to 90 s:* the publishers' middle senders move between 400 and
700 kbit/s (a step every 8 s, 1.4 to 1.9 Mbit/s together; three middles
need 2.7 to 3.3 Mbit/s with headroom and put 2.4 to 2.9 on the wire),
and the path dips to 0.4 Mbit/s for 1.2 s at 20, 50 and 78 s.
*90 to 198 s:* the rates stand still and a competing flow leaves the
subscriber 2.2 Mbit/s for 10 s and all 4 for 8 s, six times; two middles
put 1.9 Mbit/s on the wire and three 2.4. *198 to 275 s:* the path as at
first, rates moving, no dip.

The build before this round (the three source files as committed,
built in a copy of the tree) and the build after it, on that model:
two runs and three, taken in turn (debug, one machine, load average 4
to 6; "changes" are changes of one publisher's given quality, "taken
away" counts moments, changes less than a second apart being one):

| | Before, 2 runs | After, 3 runs |
|---|---|---|
| A picture taken away with no cut of the estimate in the second before | 1, 1 (at 200.7 and 200.4 s, the rates having moved; given back 2 s later) | 0, 0, 0 |
| Probes carried that gave nothing when they ended | 7 of 19, 9 of 21 | 0 of 15, 0 of 15, 0 of 14 |
| 0 to 90 s: changes a minute | 13.1, 13.1 | 13.1, 13.1, 13.1 |
| taken away | 3, 3 (the dips) | 3, 3, 3 (the dips) |
| back on three middles after each dip | 7.2, 12.2, 6.4 s; 7.0, 11.5, 8.4 s | 6.7, 6.7, 6.7 s; 6.4, 6.5, 6.4 s; 6.7, 12.9, 6.3 s |
| all three on middle, of 87 s | 60, 59 s | 66, 67, 60 s |
| 90 to 198 s: changes a minute | 4.4, 4.4 | 4.4, 4.4, 4.4 |
| taken away | 2, 2 | 3, 3, 2 |
| 198 s on: taken away | 1, 1 | 0, 0, 0 |
| three middles after the path is its own (198 s) | there since 196.3 and 193.6 s | 1.9 s, 2.0 s, there since 193.5 s |
| light datagrams lost by the link / missing at the subscriber after 3 s | 0 / 0 | 0 / 0 |

**What the model shows.** The two things the build before did with
nothing wrong on the path are gone in every run: a picture taken away
with no cut of the estimate (causes 3 and 4), and a probe carried that
gave nothing. After a dip the three middles are back in 5.4 to 6.9 s in
eleven cases of twelve (the fourth run below included); the twelfth
(12.9 s) is a first probe that ended `short` at 98.3% of its rate and
waited 5 s, the weakness named below. A fourth run of the final build,
with the test as it is committed (it also asserts that nothing is taken
away after 198 s) and without the SFU's log, passed: 13.8 and 3.3
changes a minute in the first two phases, taken away 3 and 2 times,
back after the dips in 5.4, 6.9 and 6.4 s, nothing taken away after
198 s, three middles 2.0 s after it, no light packet lost.

**What it does not show**, plainly. It does not show the build before
cycling while the competing flow comes and goes, and so it does not show
the longer waits curing that: in that phase both builds took pictures
away two or three times and then sat on one middle picture for 67 to
85 s, because under the competing flow a probe toward two middles fails
(they need 2.16 Mbit/s with headroom of a path that carries 2.2). The
waits are there in the after-runs (10, 20 and 40 s in turn in two of
them, 10 and 20 s in the third, each on the line `a set ... proved too
large`), and the count is the same. An
earlier form of the model (the competing flow leaving 2.5 Mbit/s, the
rates moving in that phase too; one run each) had the build before at 8
given and 8 taken away in those 108 s (8.9 changes a minute, taken away
5 times, twice with no cut) and the final build at 6 and 7 (7.2 a
minute, 4 times, never without a cut, three middles back 25 s after the
path was its own, its wait then being 40 s); builds between the two
gave 4.4, 5.0 and 6.1. One run each does not carry a claim. What the rule
"a set that proved too large waits longer each time" rests on is
therefore its unit tests and the two places in the call's log where a
picture was given and gone a second and four seconds later, not a
measured difference on a model.

Three things the first builds of this round did on the model, each
changed: the first counted every taking-away within 20 s of a giving as
"too large", dips included, and the three dips cost 10, 20 and 40 s of
waiting (41 of 87 s on three middles); the second kept the wait by the
rate the set needed, and the same set, cheaper for a moment, was given
15 s into a wait of 40; and an earlier form of the model (a full stop
of the path for a second in place of a dip) did not make either build
take anything away in a dip at all: after a full stop everything
arrives together and the cut lands above what is being sent. The dip to
a tenth is what resembles the log (cuts to 130 to 715 kbit/s with a
queue of half a second).

The model has one subscriber on the link (cause 5, and two subscribers
probing one link at once, are not in it), its feedback is a `str0m`
client's, its competing flow is a rate that changes on a period of 18 s
(the build before happened to probe every 9 s, always into it), and its
"mobile link" is a queue with a rate.

**The five step-6 tests on the final build**, each alone by name with
`--ignored --exact` and a 420 s kill, three times: 15 of 15 passed, no
attempt discarded. Narrow: settled 5.4 s, all main 5.8 to 5.9 s after
the lift. With delay and jitter: 5.7 to 5.8 s and 6.2 to 6.7 s. Clumps:
5.4 to 5.5 s and 5.6 to 6.0 s; with delay and jitter 5.6 to 5.7 s and
6.1 to 6.3 s. Very narrow: all main 8.1 to 8.9 s after the lift, no
link drop after the first 8 s. (On the two builds of this round before
it, 5 of 5 and 6 of 6 passed, one attempt discarded: the jitter test's
own loop stood still for 298 ms.)

**What remains weak**

- A stall of the path takes every larger picture at once, and they come
  back one probe at a time: 5.4 to 6.9 s per dip on the model, 5.6 s at
  best on the call. One probe straight toward the set that had stood before
  the stall would about halve that; not built.
- A set that proved too large is tried again when its wait is over, not
  when the path has changed: on a path that cannot hold it, once a
  minute for good, each time a picture given and taken back. What
  tells a set too large from a stall is one number (the estimate after
  the cut against half of what the set put on the wire); a stall that
  leaves more than that through costs the set its wait though the set
  was not at fault (cause 2 on the call), and a path with a stall every
  few seconds is given its pictures back after each one.
- Two subscribers behind one link: each one's probe is the other's
  competing flow (18:32:33: `P21` probing toward 2161 kbit/s while
  `P11`'s probe toward 1593 ran), and each one's model explains only
  its own share of a clump (cause 5). The SFU does not know that two
  peers share a path. Not built.
- A probe that was held back and acknowledged 95 to 97% of its rate
  ends `short` and waits 5 to 8 s (twice on the call, once on the
  model). With light pictures a large part of the set, what the set
  puts on the wire is nine tenths of the probe's rate, so the verdict
  was left as it is.
- On the call both mobile browsers were sending a main picture nobody
  was being sent (`key_frame="nobody-watching"`; `P11`'s at 2.7 Mbit/s
  at 18:32:49) over the uplink of the link whose stalls cost the
  pictures. Whether that load has to do with the stalls is not known;
  stopping a sender nobody watches is the client's to do.
- The cause of the second queue wave of cause 5 (18:33:49.35, a second
  after the first had drained) was not found.
- None of this has been on a call. The next one shows it in the `why`
  of each change and in `too_large_wait_secs`.

### What does not pass, and what the runs depend on

- **The tests discard a run they could not measure**: one in which their
  own loop stood still for more than 250 ms (`DL_WORST_STALL`). Up to
  three attempts (`DL_ATTEMPTS`). The second round also discarded a run
  in which a publisher's frame was written more than 80 ms late; that
  rule hid exactly the runs that mattered and is gone (third round). The
  downlink tests take turns (`DL_ONE_AT_A_TIME`). A discarded attempt is
  printed with its reason; its assertions are not evaluated. Every
  attempt has a deadline (`run_for` + 90 s) and the test a watchdog on
  the wall clock that ends the process: one run was found alive 2 h
  13 min after it started, its log ending 131 s in; the monotonic clock
  the run's own deadline used does not move while the machine sleeps,
  which is the likeliest cause and was not proven.

- **Run together with the rest of the file, the tests are not
  measurements.** `cargo test -p lakko-svc-sfu` runs the 45 tests of
  `synthetic_client_e2e.rs` at once; the downlink tests' own loop (the
  three publishers, the link and the subscriber live in it) then stood
  still for whole seconds and both acceptance tests failed on that run.
  The tests now measure their own loop: a run in which it stood still for
  more than 250 ms is printed, discarded and repeated (four attempts then, six now;
  see the item above). Alone, the longest stall seen then was 28 to 179 ms.
- Earlier builds of this step failed runs that the final one passes, each
  for a reason that was then fixed: a failed probe cut the estimate
  (now taken back); the first move tried was always the most important
  publisher's, so a cheaper one was never tried; a drifting estimate took
  a middle track away for a main one the path does not carry; jitter
  alone cut the estimate; a set given at exactly what it needs was taken
  away again by the next measurement; a moment's stall of the publishers
  lowered the ceiling and with it the estimate; a step of the delay
  after a dip kept every later probe from starting. Rounds of fifteen
  runs on intermediate builds had 3, 5, 2, 0 and 2 failures. The final
  build has been through one round of fifteen and the six extra dip
  runs.
- On the limited link with delay and jitter, one run of an intermediate
  build lost 21 datagrams to a failed probe at 4 to 6 s (its target had
  been measured in the second that held the resumed tracks' key frames).
  The wait after a carried probe was raised from 0.5 to 1 s and "a
  smaller rate" made to mean four fifths; the three runs since had no
  drop after the first two seconds. Three runs do not show it cannot
  happen.

### Seen, not explained

After the dip, in every dip run, the delay the SFU computes from the
subscriber's feedback (reported arrival less send time) stepped up by 12
to 25 ms and stayed there for the rest of the run, on a loopback link
with no queue. Before the dip it stood a steady 16 to 17 ms above the
smallest value of the run. Whether that is how the `str0m` client in the
test builds its feedback after a gap, or something in how the SFU places
the arrival clock, was not found. It is why a carried probe resets the
delay baseline and why a probe measures the queue from where it stood
when it started; without those the subscriber stayed downgraded for the
rest of the run (seen: 13 s). What a browser's feedback does here is not
known.

**2026-10-07.** The step was seen at 26 to 34 ms, above the 25 ms a
standing queue is read at, and it stayed there for eight seconds on an
unlimited link; a frame one of the test's publishers wrote 118 ms late
then set the detector off, the "queue" confirmed it, and the subscriber
lost a picture (`a_dip_on_an_unlimited_downlink_...`, twice in the full
test run that day). Now a delay that has stood above the path's
smallest for 3 s, under 75 ms, with no cut kept or pending, no probe
and no over-use, is taken as the path's own
(`take_a_settled_delay_as_the_paths_own`; unit test
`a_delay_that_settles_higher_becomes_the_baseline`). The cause of the
step is still not found. The cost: a real queue that stands shallow and
steady for 3 s is no longer evidence; one that grows or goes deep is.

### The first attempt (2026-10-05)

The step was first built on `str0m`'s own send-side estimator and failed
both acceptance tests: a second of tail drops on the 7.5 Mbit/s link took
the estimate to 178 and 185 kbit/s (its loss-based bound is 100 kbit/s
divided by the loss above 5%), it could not rise above 1.5 times what was
being sent while the heavy tracks were paused (three light pictures for
39 s in one run), all three were back on main 41.4 and 43.4 s after the
lift, and on an unlimited loopback link it lowered the estimate 1.16 s
after the start on the word of its first probe. With its loss-based part
switched off it stood at 10.7 Mbit/s on the 7.5 Mbit/s link. The
thirteenth vendored change of that attempt (`Bwe::current_estimate`) was
taken out again.

### Not verified

- **No browser, no native client** was the state until 2026-10-09; that
  day's call (Chrome, Firefox, Edge; "The fourth round") is the only one,
  and what was built after it has not been on a call. Of the questions
  below the call's log answers one: all four browsers sent transport-wide
  feedback for what the SFU sent them (the budget was known in every
  interval line of three of them and in 73 of 77 of the first to join,
  which was alone at the start). The log does not
  show an SDP, so what each browser negotiated on its own lines was not
  seen. The rest stand. A browser is expected to keep taking
  the SFU's REMB as the ceiling of what it publishes and to send
  transport-wide feedback for what it receives on the SFU's lines. Not
  run. Not read: whether libwebrtc accepts the transport-wide extension
  on some media lines of a bundle and not on others, what Firefox and
  Safari do, how often each sends feedback (a probe ends on the feedback
  it gets, and the test's client reports about every 100 ms), and whether
  a browser's jitter buffer or statistics are disturbed by a second of
  retransmission-stream copies it discards.
- Paths other than the ones above: more than three publishers, a queue
  shorter than the 25 ms a cut needs (such a path shows loss and no
  delay, and is cut only by the 3 s loss rule), a path whose rate changes
  slowly, competing traffic.
- The fuzz target of the SFU was compile-checked, not run.

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
