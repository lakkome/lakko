<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0067: Call media survives a burst of packet loss

- **Status:** Proposed (2026-10-03). Step 1 deployed (web); step 2 built on web; step 2b built in `services/sfu`; see "Measured" and "As built" below.
- **Date:** 2026-10-03
- **Context:** group and link calls. Builds on
  [ADR-0066](0066-sfu-forwards-rtp-packets.md) (the SFU forwards packets and
  keeps the publisher's gaps) and on the generic frame descriptor relay
  (`services/sfu/src/frame_descriptor.rs`). Leaves SFrame
  ([ADR-0013](0013-sframe-cross-platform-aad.md),
  [ADR-0035](0035-sframe-per-stream-kid-key.md)) untouched.

## Context

Measured on a live two-laptop call on the test stack after ADR-0066 was
deployed (2026-10-03, about 18:38 UTC):

- The SFU adds no wait: forwarding delay p50 0.12 ms, p99 0.37 ms, max
  0.93 ms; the gaps in what it sends equal the gaps in what it receives.
- The home network under both laptops drops 20-41% of packets for about a
  second roughly every 30 seconds. The same holes appear in plain pings with
  no call running.
- After each hole: retransmission requests, a keyframe request, and the
  publisher's video bitrate collapsing and taking about 25 s to recover.

What our calls negotiate today, read from the code:

- Video: single-layer VP8 (`addTrack`, no `sendEncodings`, no
  `scalabilityMode` anywhere in `apps/`), repaired by retransmission (RTX)
  and keyframe requests. No video FEC.
- Audio: Opus with in-band FEC (`str0m` sets `useinbandfec=1`), which covers
  isolated lost packets, not a run of them. No RED.
- `str0m` 0.19 knows Opus, PCMU, PCMA, H.264, VP8, VP9, AV1 and RTX
  (`src/format/codec.rs`). It has no entry for `red` or `ulpfec`, so an SFU
  answer drops both even when a browser offers them.

In a single-layer stream every frame references the one before it. A frame
lost in a burst that retransmission does not repair in time stops the
picture until the next keyframe, and the keyframe request after the burst is
part of what the bitrate collapse follows.

## Decision

Make the media itself tolerate a burst, in this order, each step measured on
a live call before the next.

1. **Measure first.** Each client reads, per remote video stream, the
   receiver's own counters: freezes and their total duration, frames dropped,
   the average jitter-buffer delay, and per audio stream the concealed
   samples. Web reads them from `getStats()` (`inbound-rtp`: `freezeCount`,
   `totalFreezesDuration`, `framesDropped`, `jitterBufferDelay` /
   `jitterBufferEmittedCount`, `concealedSamples`); the natives from
   libwebrtc's stats report under the same names. They are logged once per
   interval (diagnostics, no user data) and kept for the connection details.
   Without these numbers no later step can be shown to help.
2. **Temporal layers for camera video.** The camera sender encodes VP8 with
   three temporal layers (`scalabilityMode: "L1T3"`, set in `sendEncodings`
   when the transceiver is created; native `RtpEncodingParameters` carry the
   same field). Two thirds of the frames are then referenced by no later
   base-layer frame, so losing one costs that frame alone rather than the
   picture until a keyframe. The SFU changes nothing: it forwards packets and
   relays the generic frame descriptor, which carries the temporal id and
   the dependency list the receiver needs to decode past a lost upper-layer
   frame. Screen share keeps one layer (its content does not change frame to
   frame the way a camera does).
2b. **The SFU asks for the keyframe itself.** `services/sfu/src/loss_watch.rs`
   tracks, per publisher video stream, the sequence numbers skipped on the
   uplink. A hole that retransmission has not filled within
   `UNREPAIRED_LOSS_DEADLINE` (400 ms, chosen) makes the SFU ask the
   publisher for a keyframe (`KeyframeReason::UnrepairedLoss`), through the
   same per-track rate limit as a subscriber's PLI. A keyframe clears every
   hole before it. A subscriber whose picture is stuck decodes that keyframe
   on arrival instead of waiting out its own timer.
3. **Redundant audio (RED, RFC 2198).** Each audio packet also carries the
   previous one or two encodings, so a short run of losses is filled from
   the next packet that arrives, without a retransmission. This needs the
   SFU to negotiate `red/48000/2` (whose `fmtp` names Opus) and to forward
   its payload type like Opus. `str0m` cannot do either today; the change is
   either an upstream contribution or a minimal vendored fork in the
   `crates/vendor/` pattern of [ADR-0014](0014-webauthn-action-binding.md),
   chosen when this step starts.
4. **Not now: video FEC and SFU layer dropping.** ULPFEC and FlexFEC need the
   same `str0m` work as RED, for a smaller gain over RTX at our round-trip
   times; reconsider with step 1's numbers. Dropping upper temporal layers
   for a subscriber short of bandwidth needs a bandwidth estimate the SFU
   deliberately does not run (ADR-0066, decision 8).

## Measured (step 1, 2026-10-03, about 6 minutes, two laptops, web)

- A burst of uplink loss every 30 s, seen by the SFU on both publishers at
  once (19:21:15, :47, 19:22:17, :46, ... UTC): 20-67% of video packets lost
  for about a second. The SFU dropped nothing it received.
- Each burst cost each subscriber one freeze of 3.2-3.6 s: 22 freezes, 41 s
  frozen on one laptop; 18 freezes, 44 s on the other. The subscriber's
  keyframe request reached the SFU 1.5-2 s after the burst began and the
  publisher's keyframe followed in under a second; the frames received in
  between were dropped for a missing reference.
- Jitter-buffer delay between bursts 100-130 ms (video and audio), up to
  0.3-0.4 s right after one. Audio concealment up to 21% of a 5 s interval
  during a burst, none between.
- Sender mode `L1T1` (the build measured had no temporal layers).

## What must be verified before steps 2 and 3 count as done

These are open, not assumed:

- That Chrome, Safari, and libwebrtc 125 on iOS and Android apply `L1T3` to
  a sender that carries an SFrame encoded transform, and that the receiver
  decodes past a lost upper-layer frame when the codec payload is
  ciphertext and only the generic frame descriptor describes the frame.
  Read back as `outbound-rtp.scalabilityMode` and through step 1's freeze
  count under the same network.
- Where in each stack RED is applied relative to the encoded transform.
  If the transform sees the RED block, SFrame encrypts the redundancy with
  the primary encoding and the receiver must open it before splitting; if it
  sees only the primary encoding, RED would carry unencrypted redundant
  audio and must stay off (invariant #4). Step 3 does not ship until this is
  read from the libwebrtc source and observed on the wire.

## Alternatives

- **Tune the jitter buffer for longer waits.** Trades the freeze for a
  standing delay, which the people on the call already reported as the
  other problem.
- **Simulcast.** Two or three full encodings per publisher; helps a
  subscriber short of bandwidth, not a burst on the publisher's own uplink,
  which is what was measured.
- **Video FEC first.** Needs the `str0m` work and protects every frame
  equally, where temporal layers make most frames expendable for free.

## Consequences

- Step 2 is a client change on three platforms and nothing else; it costs a
  few percent of encoder efficiency at the same bitrate.
- Step 3 touches the SFU's media negotiation and a third-party crate.
- Invariants: #2 and #4 hold (the SFU still reads only the descriptor flags
  ADR-0066 names; nothing unencrypted is added). The descriptor already in
  the clear carries the temporal id, which tells a relay how important a
  frame is; it did so before this decision as well, for every frame.

## Verification

Per step: the same two laptops on the same network, a 5 minute call, step 1's
counters before and after, plus the SFU-side capture used for ADR-0066. A
step that does not lower the freeze count or the total freeze duration is
reverted.
