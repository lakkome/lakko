<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0067: Call media survives a burst of packet loss

- **Status:** Proposed (2026-10-03). Step 1 and step 2 deployed on web (step 2 also built on Android; the iOS binding has no `scalabilityMode`); see "Measured" below.
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
2b. **Not the SFU asking for keyframes on loss (tried, measured, removed).**
   A keyframe is needed only when the receiver can no longer decode: a frame
   later frames depend on is lost for good and no keyframe is already on its
   way. A lost packet that retransmission will still repair, a lost frame no
   other frame depends on (two in three with `L1T3`), or a second request
   while the first keyframe is in flight need none. Only the receiver knows
   the dependencies and what it has already asked for, and libwebrtc already
   decides exactly this; the SFU stays transparent and only relays its
   requests under the existing rate limit. A version that asked the
   publisher for a keyframe whenever an uplink packet stayed missing 400 ms
   (`services/sfu/src/loss_watch.rs`, commit `76064fc3`) sent 34 requests in
   one call, mostly for frames nobody needed, adding oversized frames into
   the loss burst; it was removed the same day.
2c. **The SFU keeps asking for a lost packet until the burst is over.**
   The root cause the three measured calls point to: upstream `str0m`, which
   the SFU receives through, asks a publisher for a missing packet at most
   five times, once per 33 ms tick (about 165 ms in all, whatever the round
   trip), and tracks a hole only within 100 packets of the newest. A loss
   burst lasts about a second, so every request and every retransmission
   fell inside it and the packet never reached the SFU; a subscriber's own
   request for it then had nothing to be answered from, and the subscriber
   waited for a keyframe. The vendored `str0m`
   (`crates/vendor/str0m`, second fork change, pinned by
   `services/sfu/tests/fork_delta_guard.rs`) asks up to ten times on a
   schedule that doubles from 33 ms to a 200 ms cap, about 1.4 s in all, and
   tracks holes within 512 packets. The send side already keeps 2000 packets
   or 3 s for answering subscribers. Proven on the host by
   `a_packet_lost_in_a_burst_is_repaired_after_it` (a packet lost with every
   retransmission for the next 600 ms reaches the subscriber); not yet
   measured on a call. The schedule is chosen, not measured; whether a
   browser keeps a packet long enough to answer the late requests is
   unverified.
3. **Redundant audio (RED, RFC 2198).** Each audio packet also carries the
   previous encoding. Read from libwebrtc M125
   (`audio/audio_send_stream.cc`, `audio/channel_send.cc`,
   `audio/channel_receive.cc`, `neteq/red_payload_splitter.cc`): the RED
   container is built before the frame encryptor and the encoded transform,
   so SFrame seals the primary and the redundant encoding together, and the
   receiver opens the frame before NetEq splits it. Consequences: the SFU
   forwards a RED payload verbatim and can neither strip RED nor rewrite the
   inner Opus payload type, so Opus is payload type 111 for every
   participant (libwebrtc's and `str0m`'s default), and a subscriber that
   did not negotiate red is not sent the packet. Chrome sends RED only when
   the answer lists `red/48000/2` (fmtp `111/111`) before Opus. `str0m` has
   no red codec; the vendored fork gains one (third fork change). Chrome
   carries one redundant frame (20 ms) and the count cannot be raised from a
   page; natives can raise it with the `WebRTC-Audio-Red-For-Opus` field
   trial. One redundant frame repairs isolated losses, not a second of 30-60%
   loss.
4. **Video FEC (RED + ULPFEC), decided by the maintainer 2026-10-04.** Read
   from libwebrtc M125 (`call/rtp_video_sender.cc`,
   `modules/rtp_rtcp/source/rtp_sender_video.cc`,
   `modules/video_coding/media_opt_util.cc`): ULPFEC is the default scheme
   when negotiated (FlexFEC is behind two field trials) and stays on for VP8
   with NACK; it is computed on packets after encryption, so SFrame does not
   change; only temporal layer 0 is protected; the protection rate follows
   the loss already reported, is zeroed for delta frames under 20 ms round
   trip, and loss above about 50% is not counted. Through the SFU every media
   packet becomes a RED packet whose one-byte header names the media payload
   type, and an FEC packet's recovery fields are computed over the protected
   packets' payload type, sequence numbers and timestamps: the SFU keeps the
   publisher's sequence numbers and timestamps (offset zero unless the source
   stream changes), must map the outer RED payload type and the inner byte,
   and must fix the payload-type recovery field when the media payload type
   differs between publisher and subscriber. Expected effect on a
   one-second burst: partial at most, unmeasured.

   **First attempt, 2026-10-04: the publisher's FEC forwarded to
   subscribers. It cannot repair video at a libwebrtc subscriber, and was
   replaced the same day by step 4a.** Found while building, read from libwebrtc M125
   (`forward_error_correction.cc` `XorPayloads`, `ulpfec_receiver.cc`
   `ProcessReceivedFec`, `RtpPacket::ZeroMutableExtensions`) and RFC 5109
   section 7.3: ULPFEC protects everything after the 12-byte fixed header,
   the header extension block included, and the receiver zeroes only the
   "mutable" extensions (abs-send-time, transport sequence number,
   transmission offset, part of video-timing) before decoding. `mid`, the
   generic frame descriptor and video orientation are protected as sent. The
   SFU does not forward a publisher's extension block: each leg carries its
   own `mid`, extension ids and order. A packet a subscriber recovers from
   forwarded FEC would therefore have a wrong extension block or payload,
   would be counted as received (so the real packet is no longer asked for),
   and its frame should fail SFrame authentication. Making it work needs the
   SFU either to send the publisher's extension block unchanged (which breaks
   `mid` demultiplexing at the subscriber) or to terminate FEC and generate
   its own per subscriber; neither is built. What is built and host-tested:
   negotiation of the pair in the vendored `str0m` (fourth fork change),
   `services/sfu/src/video_fec.rs` (RED header and ULPFEC header handling,
   fuzz target `video_fec`), wrap / unwrap / FEC forwarding per subscriber,
   and the proof of the blocker in the e2e test (the `mid` differs between
   the two legs). Two more defects of forwarding as designed: a subscriber
   not sent an FEC packet sees a hole in the sequence numbers and asks for
   it in vain, and an FEC packet carries a copy of the last media packet's
   frame descriptor.
5. **Not now: SFU layer dropping.** Dropping upper temporal layers for a
   subscriber short of bandwidth needs a bandwidth estimate the SFU
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

## Measured (step 2, 2026-10-03, about 7 minutes, same laptops and network)

The build measured also carried the SFU keyframe requests of 2b, so the two
effects are not separated; a call with layers alone is the next measurement.

- Both senders report `L1T3`.
- Frozen time per minute fell from 7.1 to 4.8 s on one laptop and from 7.4
  to 5.7 s on the other. On the second the freezes got shorter (2.5 s to
  1.1 s on average) and more frequent (3.0 to 5.2 a minute).
- Keyframe requests from the receivers fell from 10 and 13 to 7 and 2.

## Measured (layers alone, 2026-10-03, about 6 minutes, same laptops and network)

The SFU without 2b, both senders `L1T3`:

- Frozen time per minute 9.2 and 7.7 s (before layers: 7.1 and 7.4 s; with
  layers and 2b: 4.8 and 5.7 s); 3.8 and 3.9 freezes a minute, 2.4 and 2.0 s
  on average; 15 and 13 keyframe requests from the receivers.
- So layers alone did not help on this network: a burst takes out
  base-layer frames as well, and nothing after them decodes. The gain in the
  previous call came from the earlier keyframe of 2b, which is the
  observation that led to 2c: the packets were being lost for good at the
  SFU, and asking for them longer removes the need for the keyframe instead
  of hurrying it.

## Step 4a (2026-10-04): the SFU repairs the publisher leg from the publisher's FEC

"Forwarded FEC is invalid" was wrongly recorded above as "FEC must stay off".
FEC is valid on each leg separately, so the SFU terminates it:

- **4a, built.** On the publisher leg the SFU holds every packet as the
  publisher sent it, so the publisher's ULPFEC is valid there. With
  `LAKKO_SFU_VIDEO_FEC=1` the SFU keeps a window of each publisher's recent
  video packets (`services/sfu/src/fec_receive.rs`), rebuilds a lost one as
  soon as an FEC packet allows (RFC 5109 section 9.2, mutable extensions
  zeroed as libwebrtc does), hands it to `str0m` as received so it is no
  longer asked for, and forwards it like any other packet. A rebuilt packet
  whose timestamp does not lie between its held neighbours is refused: a wrong
  packet counted as received is worse than a missing one. The payload is
  SFrame ciphertext and is only XORed (C-39). The publisher's FEC packets are
  not forwarded. Sixth fork change in the vendored `str0m`
  (`DirectApi::keep_received_rtp` / `take_received_rtp` /
  `handle_recovered_rtp`). Counters `lakko_sfu_video_fec_packets_total`,
  `lakko_sfu_video_fec_recovered_total`, and `recovered=` on the per-interval
  publisher log line.
- **4b, built the same day.** For each subscriber that negotiated red +
  ulpfec the SFU sends its own FEC packet in the slot the publisher's FEC
  packet occupied: same protected set, computed over the packets exactly as
  that subscriber received them (its payload type, the leg's sequence numbers,
  timestamps and header-extension block with the mutable extensions zeroed,
  payload verbatim), so the leg's sequence space is not renumbered and the
  protection rate is the publisher's (`services/sfu/src/fec_send.rs`). The
  leg's extension block is decided inside `str0m` at serialization (`mid` is
  written only until the receiver acknowledges the stream), so the seventh
  fork change hands the serialized first transmission out
  (`StreamTx::keep_sent_rtp`, `DirectApi::take_sent_rtp`); the FEC packet is
  written only after every packet it protects was serialized, is not kept for
  retransmission, and takes a transport-wide sequence number like any packet.
  Counter `lakko_sfu_video_fec_sent_total`, `fec_sent=` on the per-interval
  fanout log line. Still a gap: a slot stays empty when any protected packet
  did not reach that subscriber as a first transmission (another simulcast
  layer, a source change, not yet connected); a subscriber without the pair
  gets no FEC; a lost SFU FEC packet is not resent.

Host-tested (unit tests against an encoder written from RFC 5109 and the
libwebrtc M125 source; an end-to-end test in which a packet withheld on the
uplink reaches every subscriber and is never asked for; two more in which a
decoder written in the test rebuilds every protected packet of a subscriber
leg byte for byte, and a packet withheld on the downlink with every
retransmission dropped). Not verified: byte
agreement with a real Chrome sender, and how much FEC Chrome sends at the
round-trip times seen on live calls.

## Found (2026-10-04): the receiver report claimed loss that had been repaired

Two laptops, web, about 6 minutes. One laptop's sending direction collapsed
about every 30 s: its bitrate estimate fell from 2-3.5 Mbps to 90-300 kbps
within 2 s and climbed back over about 25 s; the other direction was never
disturbed (2500 kbps, 1280x720, 0% reported loss for the whole call). The SFU
log for the same seconds shows the cause: the video ingress of the collapsing
publisher reported `loss=0.9725` (248/255) and `loss=0.945` (241/255) while
that stream arrived at a full 30 frames per second and the same peer's audio
reported 0.4%.

`str0m` computes the RFC 3550 interval loss, `(expected - received) * 256 /
expected`, and casts it to a byte without the appendix A.3 clamp. A packet lost
in one interval and repaired by NACK in the next makes that interval receive
more than it expected; the small negative result wraps to a byte near 255, and
the receiver report tells the sender that almost everything was lost. The
sending browser then halves its estimate per report. A publisher with no loss
at all never triggers it, which is why only one direction was affected, and
step 2c (more repairs) produced more such reports.

Fixed in the vendored `str0m` (`src/streams/register.rs`, the fifth fork
change): a negative interval reports zero and the value saturates at 255. Unit
tests `lakko_a_repaired_loss_is_not_reported_as_loss` and
`lakko_total_loss_in_an_interval_saturates`. Not yet measured on a call.

## What is verified and what is not

- `L1T3` is applied by Chrome under the SFrame encoded transform: both
  senders reported it on the measured calls. Safari and the natives are not
  checked (the iOS binding has no `scalabilityMode` at all).
- RED is applied before the transform and the frame encryptor: read from the
  libwebrtc M125 source (step 3). Not yet observed on the wire: a live call
  must show `audio/red` as the sent codec and audio that still decrypts on
  every platform. Whether the stasel/WebRTC and webrtc-sdk 125 builds differ
  from upstream here is unchecked.
- A subscriber stack that does not negotiate red (a browser without it)
  cannot be served a RED publisher's audio, since the SFU cannot unwrap an
  encrypted container. Which browsers that excludes is unchecked.

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
