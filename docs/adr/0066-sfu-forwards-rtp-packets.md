<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0066: The SFU forwards RTP packets; it does not assemble frames

- **Status:** Accepted (maintainer, 2026-10-02). Built the same day in
  `services/sfu`; host-tested (167 unit, 13 signaling and 16 end-to-end tests
  through the live media task); deployed to the test stack; not yet measured
  on a call, and no browser or native client has been run against it.
- **Date:** 2026-10-02
- **Context:** group and link calls. Amends the media half of
  [ADR-0011](0011-rust-sfu.md) (the SFU stays on `str0m`; how it uses it
  changes). Leaves [ADR-0013](0013-sframe-cross-platform-aad.md),
  [ADR-0029](0029-mid-call-key-rotation.md) and
  [ADR-0035](0035-sframe-per-stream-kid-key.md) untouched: SFrame is applied
  to whole frames on the clients, before packetization, and the SFU never
  reads a payload.

## Context

`services/sfu` uses `str0m` in its frame ("sample") mode. For every
publisher `str0m` reassembles RTP packets into whole frames, hands them to the
SFU in order as `Event::MediaData`, and the SFU writes each frame to every
subscriber, where `str0m` packetizes it again under new sequence numbers.

Measured on a live two-laptop call on the test stack, 2026-10-02 (both
laptops behind one home router whose uplink dropped 17-25% of packets for
about a second every 30 seconds; no loss in between):

- In frame mode `str0m` holds everything behind a missing packet until N
  complete frames have queued behind the gap (defaults: 15 audio, 30 video).
  Audio has no retransmission stream, so `str0m` never asks for an audio
  packet again and the wait always ran to its end: a lost 20 ms packet became
  a 300 ms stop and a burst of fifteen frames.
- With the video hold lowered to 10 the SFU still forwarded no video for
  2.3-2.8 s at each loss burst, while the publisher's packets kept arriving
  with no gap above 0.5 s. The count is of complete frames, during a burst
  few frames are complete, and each new gap starts the wait again.
- What is released at the end is old and arrives at once. A subscriber's
  jitter buffer grows to absorb it. The people on the call described the
  result as close to a second of lag and periodic freezes.
- Because the SFU re-packetizes under its own sequence numbers, a subscriber
  never sees the loss as a gap. It cannot ask for a retransmission and cannot
  size its own wait; it only finds a frame missing from the dependency chain
  and asks for a keyframe.

Lowering the two hold values (audio 1, video 3, with a keyframe request when
a frame is released after a gap) removes most of the damage and is in the
tree as an interim fix. It does not remove the cause: a relay that must
decide, in frames, how long to wait for a packet, on behalf of receivers that
each know their own network better than it does.

## Decision

The SFU runs `str0m` in RTP mode and forwards packets.

1. **Packets in, packets out.** Every `Event::RtpPacket` from a publisher is
   written to each subscriber's send stream as it arrives, payload untouched,
   marker bit kept. Nothing is held and nothing is reassembled.
2. **Sequence numbers keep their gaps.** A downstream stream's sequence
   number is the publisher's plus a fixed offset per (subscriber track,
   source stream). A packet the SFU never received is a gap the subscriber
   sees. RTP timestamps are carried the same way.
3. **Loss is repaired where it is seen.** `str0m` on the receive side still
   asks the publisher for a missing packet (video has RTX). A retransmission
   that arrives is forwarded under the sequence number it maps to and fills
   the subscriber's gap. A subscriber's own request for a packet the SFU
   already forwarded is answered from `str0m`'s send buffer, as today.
4. **Header extensions.** The generic frame descriptor
   ([`frame_descriptor.rs`](../../services/sfu/src/frame_descriptor.rs)) is
   carried verbatim, including its two frame-boundary bits, since packets are
   no longer re-cut. Audio level and video orientation are carried when both
   sides negotiated them. `mid`, `rid`, transport-wide sequence number and
   absolute send time belong to each leg and are set by `str0m`.
5. **Payload types** are mapped by codec parameters from the publisher's
   negotiation to the subscriber's, as `match_params` did for frames.
6. **Simulcast.** The per-track layer choice stays. A switch takes effect at
   a frame that starts a keyframe on the new layer (first-packet bit and the
   independent-frame flag of the descriptor), requested from the publisher as
   today, and the sequence-number and timestamp offsets are re-based at that
   packet so the downstream stream stays continuous.
7. **Keyframes.** The SFU relays a subscriber's keyframe request to the
   publisher under the existing rate limit. The interim "frame released after
   a gap" request goes away with the frames.
8. **What the SFU still never does.** It does not read or alter a payload,
   it holds no media key, and it derives nothing from SFrame headers
   (invariant #2, C-39). Send-side bandwidth estimation and pacing stay off,
   as decided on 2026-09-19.

## What does not change

Signaling, admission, call lifetime, cross-call isolation, the track and
renegotiation model, the metrics names, and every client. The clients
negotiate the same codecs and extensions and see the same streams; what
changes for them is that loss on a publisher's uplink now reaches them as
loss, which their own jitter buffer and retransmission logic are built for.

## Alternatives

- **Keep frame mode with small hold values.** In the tree as the interim
  fix. Bounded, but still a wait decided by the relay in units that stretch
  under loss, and still hides loss from the receiver.
- **Frame mode with hold 0 for video.** Every lost packet drops a frame and
  forces a keyframe, since the subscriber cannot repair what it cannot see.
- **Another SFU library.** ADR-0011 chose `str0m`; RTP mode is part of it and
  no new dependency is needed.

## Consequences

- No media waits at the SFU. A network hole costs its own length plus what
  the receiver needs to recover, not a multiple.
- The SFU's media path is rewritten around packets: forwarding, the
  per-stream sequence mapping, the simulcast switch, the fanout statistics.
  The end-to-end tests in `services/sfu/tests/synthetic_client_e2e.rs` are
  the regression gate; the SFrame round trips, the frame-descriptor relay,
  the simulcast test and the late-joiner keyframe test must pass unchanged in
  what they assert.
- Audio and video from one publisher are timestamped at the SFU by arrival
  time, the simplest mapping `str0m` documents. Lip-sync drift under jitter
  is a possible residual; using the publisher's sender reports for the
  wall-clock mapping is the follow-up if it is seen.
- `str0m` documents RTP mode as a lower-level API than its primary one.

## Invariants

- **#2** holds: the relay forwards ciphertext and a descriptor it does not
  interpret beyond the flags already in the clear (see
  `frame_descriptor.rs`). No change in what the server can read.
- **#4** holds: there is still no path that forwards unencrypted media.
- **C-39** (SFU opacity, cross-call isolation, backpressure) carries over.

## Verification

Host: the SFU unit and end-to-end suites. Live: a two-participant call on
the test stack with a packet capture on the server, comparing gaps between
packets from a publisher with gaps in what is forwarded to the subscriber;
they should match. The interim fix was measured the same way.

## As built (2026-10-02)

`src/stream_map.rs` holds the sequence-number and timestamp mapping;
`media.rs` consumes `Event::RtpPacket` and writes with `StreamTx::write_rtp`;
`frame_descriptor.rs` gained `PacketDescriptor`, the value exactly as one
packet carried it. What the build added to the decision:

- **The media loop drives `str0m`'s clock.** A packet written with
  `write_rtp` is stamped and released on a timeout tick, so the loop now feeds
  every peer a timeout on every pass and goes round once more when it wrote a
  packet. The frame-mode loop never fed a timeout; reports, retransmission
  requests and ICE timers ran only when a datagram arrived.
- **Nothing is written to a peer before its transport is connected.**
  `str0m`'s RTP-mode send queue is unbounded, so a subscriber whose transport
  never came up would have accumulated the whole call.
- **A late or retransmitted packet** is given the newest packet's arrival
  time minus its timestamp distance, not its own arrival time: `str0m` builds
  each sender report from the last packet written, and an old timestamp paired
  with a new arrival would skew the report by the repair delay.
- **Numbering starts at the publisher's own** sequence number and timestamp
  (only the rollover count is dropped), and re-bases on any change of the
  source stream, not only on a layer switch.
- **A new drop reason, `stale`:** a packet of a new layer older than the
  switch packet, or older than the cycle the stream started in.
- **Audio level and video orientation are now relayed.** The frame path
  carried only the descriptor.
- **The `lakko_sfu_media_frames_*` counters count packets** per destination.

Proven on the host by two end-to-end tests that fail against the frame-mode
SFU: one lost video packet arrives at the subscriber as one missing sequence
number with every later packet delivered, and a retransmitted packet fills
exactly that number. Not covered: a decoder's reaction to the gap, loss on
the leg to the subscriber, a simulcast switch end to end (bandwidth
estimation is off, so the policy never fires in the suite), and everything
about libwebrtc.

