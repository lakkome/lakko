<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0070: A call picture does not stop in a burst of loss, in any browser

- **Status:** Accepted (2026-10-04; the maintainer asked for a design that holds on typical bad networks, in every browser, and at ten participants; chose the adaptive playout delay). Steps 1, 3 and 4 built 2026-10-05 on the SFU and the web client, host-tested and deployed to the test stack; nothing here has been seen on a live call or against a browser.
- **Date:** 2026-10-04
- **Context:** group and link calls through the SFU. Builds on
  [ADR-0066](0066-sfu-forwards-rtp-packets.md),
  [ADR-0067](0067-loss-resilient-call-media.md) (whose step 5 this replaces)
  and [ADR-0068](0068-server-side-bitrate-control.md). SFrame keying is
  unchanged ([ADR-0035](0035-sframe-per-stream-kid-key.md),
  [ADR-0069](0069-sframe-random-counter-start.md)); invariants #2 and #4 hold.

## Context

Measured on live calls and on the synthetic dip (2026-10-04):

- A home network delivers 40-75% of a publisher's datagrams for about a second
  every 30 s. The maintainer's position, taken as a requirement: this is an
  ordinary network, not a fault to be explained away.
- A 720p stream, about 250 packets a second, is not repaired inside such a
  second (longest gap with no decodable frame: median 522 ms, up to about a
  second). A stream of 25-50 packets a second is repaired in 280-380 ms.
- Switching a viewer's one track to a light simulcast layer during the dip
  shortens nothing: the switch needs a keyframe through the same dip.
- Two-track delivery built on the browser's simulcast made a live call worse
  on both sides. Below the rate its full layer needs (about 900 kbit/s, read
  from one browser's log) the browser sends the light layer alone and nothing
  in between, so each dip of the SFU's estimate put the publisher on 180p, and
  each attempt to bring the full layer back produced a keyframe burst and
  another cut. A single encoding has no such step.
- Everything so far was tuned to and tested in one browser.

Other calling products do not repair a 720p stream through such a second
either. What they have, and Lakko lacks: viewers in a grid watch small
streams; playout is delayed by a few hundred milliseconds on a bad path; the
small stream is protected harder than the large one; and the server decides
each viewer's streams from that viewer's own downlink.

## Decision

**Rule: the server and Lakko's own client code decide; the browser executes.**
No behaviour may rest on a browser's built-in heuristic (its simulcast layer
allocator, its congestion controller, its default jitter buffer). A mechanism
that needs an API some browser lacks names the browsers, ships a fallback, and
is tested in more than one engine before it is called working.

1. **Two ordinary senders, not simulcast.** The camera is published as a main
   sender (one encoding, as before simulcast) and a light sender: the same
   camera scaled to a quarter, capped at 150 kbit/s, on its own m-line, sealed
   under its own SFrame stream index (3). The offer names the light m-lines
   (`ClientFrame::SdpOffer.light_mids`). The SFU forwards the light track to
   every subscriber as the companion (MSID stream id = source peer id +
   `~light`) and the main track as an ordinary track. No layer selection.
2. **The tile shows whichever track is alive** (`lakko_ui_rules::layer_display`,
   one rule on the three clients): the light one when the main one has
   presented no frame for 150 ms, back after a second of steady frames. Where
   a browser gives no per-frame callback, the rule is fed from the standard
   receiver statistics.
3. **The light track is protected harder.** The SFU asks the publisher again
   for a missing light packet every 20-30 ms (60 ms today for every stream)
   and sends each light packet to a subscriber twice, some tens of
   milliseconds apart. At 150 kbit/s the cost is small.
4. **Playout delay follows the path, set by the server.** A subscriber's
   minimum playout delay is zero on a quiet path. After the SFU has seen a
   burst on the publisher's uplink or on that subscriber's downlink it raises
   the minimum to about 300 ms (the playout-delay header extension, which the
   libwebrtc-based browsers and the native clients honour; the client sets the
   standard `jitterBufferTarget` where the extension is not honoured) and
   lowers it again after some minutes without one. Audio and video of one
   person move together. The cost, accepted: the conversation is delayed by
   that much on a bad path, and the first burst of a call is seen.
5. **A grid shows small streams.** From three people on, a tile shows the
   light track, or a middle one (a third sender, 360p) when the tile is large
   enough, and the main track only for a stage or a pinned person. The SFU
   forwards a main track only to subscribers that show it.
6. **The SFU estimates each subscriber's downlink** and gives up the larger
   tracks first when it is short. Without this a call of ten cannot work on an
   ordinary downlink; it is the last step because the first four do not need
   it at two or three people.

Each step is measured on the synthetic dip before it is deployed and judged on
a live call after, in at least two browser engines.

## Alternatives

- **Browser simulcast** (ADR-0067 step 5 as first built). Seen live: the step
  between the layers, and a different allocator in every browser.
- **A floor under the bitrate estimate.** Rejected by the maintainer earlier:
  it hides a narrow path instead of adapting to it.
- **FEC strong enough for half the packets of a 720p stream.** More than
  doubles the stream, all the time.
- **The browser's own congestion controller with transport-wide feedback.**
  ADR-0068's context: one second of loss became 25 s of low bitrate, and each
  browser has its own.

## Consequences

- About 150 kbit/s more uplink per publisher and downlink per watched person,
  and a second encoder on the publisher.
- A new field on the SFU offer frame; clients built before it publish no light
  track and are shown as today.
- The simulcast-based companion path stays in the SFU, unused by Lakko's
  clients.
- iOS and Android move to two senders after the web client has been judged on
  a live call.

## As built (2026-10-05)

- Step 1: `ClientFrame::SdpOffer.light_mids` (at most 4 mids of 16 bytes); a
  declared light track is opened on subscribers only as the `~light`
  companion. A declaration for a track already announced as an ordinary one is
  refused. Web: `addLightSender` / `followCamera` in `apps/web/src/calls/`.
- Step 2: the web frame watch falls back to polling `framesDecoded` every
  50 ms where `requestVideoFrameCallback` is absent.
- Step 3: 20 ms NACK interval for a light receive stream (vendored change 9),
  one second copy to each subscriber after 30 ms (change 10).
- Step 4: `services/sfu/src/playout_delay.rs`; the extension is stamped on
  video only and only for a subscriber that negotiated it. A subscriber that
  did not is told nothing yet.
- Found on the way: a subscriber's REMB had been used as its downlink budget
  for simulcast layer selection and parked main tracks on the light layer;
  only transport-wide feedback is a budget now.

Synthetic dip (30% of datagrams arriving for one second, 20 seeds), the stop a
viewer 300 ms behind sees on the light track, ms, min / median / p95 / max:

| Dip on | Protection | No stop | Stop |
|---|---|---|---|
| publisher uplink | off | 0 of 20 | 62 / 278 / 646 / 698 |
| publisher uplink | on | 12 of 20 | 0 / 0 / 100 / 225 |
| subscriber downlink | off | 0 of 20 | 104 / 356 / 517 / 692 |
| subscriber downlink | on | 0 of 20 | 127 / 275 / 528 / 617 |

The goal is not met for the downlink: one second copy is lost with the first
half the time at this loss, and the rest waits on the subscriber's own NACK
schedule, which the SFU does not control. The loss pattern in the model is an
assumption; the next step is a capture of packet headers during a live call.

## As built, step 5 (2026-10-05, SFU and shared rule; no client yet)

Built on the SFU, the wire crate, the shared UI rule and both FFI surfaces;
host-tested. No client sends a middle track or a `video-wants` frame yet,
and nothing here has been seen on a live call or against a browser. Step 6
(the SFU estimating a subscriber's downlink) is not built: the SFU forwards
what the subscriber asks for.

- **Three ordinary senders.** `ClientFrame::SdpOffer.middle_mids`, with the
  bounds and the validation of `light_mids`; at most one middle track per
  peer (`MediaPeer::declare_middle_tracks`). A mid cannot be declared both
  light and middle. The middle sender is the camera at half size, capped at
  500 kbit/s, SFrame stream index 4 (a client's to honour; the SFU reads
  neither).
- **Three downstream tracks.** MSID stream ids `<peer>`, `<peer>~mid`
  (`TrackRole::Middle`, `MIDDLE_STREAM_SUFFIX`) and `<peer>~light`, all
  negotiated whenever the publisher has them.
- **The subscriber says what it shows.** `ClientFrame::VideoWants { wants:
  [{ peer, quality }] }`, kind `video-wants`, quality `main` / `middle` /
  `light`, at most 64 entries, each frame replacing the whole set. The SFU
  forwards the main track only at `main`, the middle track only at `middle`,
  the light track always. Never sent, or a publisher not named: `main`.
  `middle` asked of a publisher with no middle track open on that
  subscriber: `main` (the want is kept and applies when the track opens).
  A name that is no peer of the call changes nothing.
- **Pause and resume.** Pausing takes effect with the next packet. A track
  wanted again has its publisher asked for a key frame at once (reason
  `track-resume`, through the owed path, repeated once a second) and starts
  at the first packet of a key frame; after five requests with none it
  starts at the next packet. The downstream stream is re-based on resume
  (`StreamMap::rebase`), so the subscriber sees consecutive sequence numbers
  across the pause. A paused track is not asked for a key frame when it
  opens, and a subscriber's own keyframe request on it is not relayed.
- **The rule a tile uses.** `lakko_ui_rules::call_group::tile_video_quality
  (people, tile_width_px, pinned_or_stage)`: a call of two, or a pinned or
  stage tile at least 960 physical pixels wide, `Main`; a tile narrower than
  240, `Light`; otherwise `Middle`. `middle_sender_wanted(people)` is true
  from three people. On UniFFI and wasm. The three numbers are chosen, not
  measured.
- **Logs.** `media: track announced by peer` says `role=main|middle|light`;
  the per-interval fanout line keeps `role` and adds `paused` and
  `not_wanted`; `media: subscriber's video wants changed` is one line per
  frame.

Decided on the way, each a choice the decision left open:

- The middle track falls back to main only while it is not **open** on the
  subscriber, so a want sent before the track is negotiated takes effect by
  itself when it opens.
- Resume waits for the first packet of a key frame, not for the whole key
  frame as a simulcast layer switch does: nothing else is on the wire to
  keep showing, and the light track covers the wait.
- Pausing a track mid-frame leaves the subscriber an unfinished frame it
  never completes. Nothing is retransmitted for it and the SFU drops the
  subscriber's keyframe requests on a paused track.
- A simulcast publisher's main track is paused and resumed by the same
  frame; its companion is not. The layer policies still run for a paused
  simulcast main track. Lakko's clients do not send simulcast.
- `video-wants` on an SFU without a media task is taken and ignored.

Measured in `three_senders_and_the_subscribers_wants` (loopback, no loss,
25 frames a second; main 10 packets a frame, middle 5, light 2; a key frame
three times that): packets handed to the subscriber on main / middle / light
with no wants 370 / 0 / 74, after `middle` 0 / 230 / 90, after `main` again
460 / 0 / 90, identical in three runs. The main track's sequence numbers
were consecutive across the pause and no retransmission arrived on it.

## As built, step 6 (2026-10-05 and 06)

Built on the SFU, the wire crate and both FFI surfaces; host-tested
against synthetic `str0m` clients; never run against a browser or a
native client. The web client reads `video-given` (a tile shows the lower
of what it wants and what the SFU gives; host-tested only); iOS and
Android do not.
`LAKKO_SFU_DOWNLINK_CONTROL` is off unless set to `1` (and only under
`server` bitrate control), because nothing here has met a browser. The
acceptance tests are `#[ignore]`d (about a minute each); the first two
builds failed an independent check on 2026-10-06, the third passed it
(each of the five tests 3 of 3 alone, no attempt discarded).
The full account, with the timelines, is in
`services/sfu/README.md`, "Downlink control".

**The estimator is not the one this step named.** The step was first
built on `str0m`'s send-side estimator and failed both acceptance tests:
a second of tail drops on a 7.5 Mbit/s link took the estimate to about
180 kbit/s, it could not rise above 1.5 times what was being sent while
the heavy tracks were paused, all three publishers were back on main more
than 40 s after the limit was lifted, and an unlimited loopback link was
downgraded 1.2 s after the start. The lesson of ADR-0068 applies to the
downlink as it did to the uplink: that estimator is written for a sender
whose encoder follows it. What is built instead:

- **Negotiation per direction** (vendored change 12,
  `DirectApi::set_twcc_on_local_media_only`): the transport-wide sequence
  number and `transport-cc` feedback on the media lines the SFU offers,
  neither on the lines a peer offers. A publisher's answer is as ADR-0068
  has it.
- **Lakko's own estimate per subscriber**: `uplink_estimate::PathEstimator`
  (the uplink estimator, generalised, not copied), fed from the records
  of the subscriber's transport-wide feedback, which `str0m` now hands
  out (vendored change 13, `DirectApi::keep_twcc_feedback` /
  `take_twcc_feedback`). `str0m`'s own estimator and pacer stay off. The
  budget is unknown until feedback has acknowledged a packet.
- **A cut needs a standing queue** (25 ms above the path's smallest
  delay, for 300 ms), or lasting loss. Not in the decision: a cut is to
  0.85 of what arrived, which is always below what the set being sent
  needs, so a detector set off by jitter cost a picture. A cut made
  while a probe runs or within a second after it is taken back.
- **Probes** (`services/sfu/src/downlink_probe.rs`): before the next
  larger set is given, 1 s of padding (retransmission-stream copies of
  packets already sent, `StreamTx::resend`; never new media) brings what
  is sent up to what that set needs with its headroom; the set is given
  only if at least 90% of that rate was acknowledged, at most 10% of the
  packets were missing and no queue grew by 25 ms, which ends the probe
  at once. After a failure the next probe toward that rate waits 5 s,
  then 8 s every time; one toward at most four fifths of it, 1 s.
- **The allocation** (`services/sfu/src/downlink_allocation.rs`):
  headroom 1.2 to give a track; a track is taken away at once when what
  is on the wire no longer fits the estimate; **a track is given only
  when the path has carried the rate of the resulting set** (a probe, or
  what is arriving on a quiet path), never on the estimate alone.
- **`ServerFrame::VideoGiven`**, kind `video-given`, at most 64 entries,
  on UniFFI as a typed frame and on wasm through `parseSfuVideoGiven`.
- The retransmission caches of a subscriber's heavy tracks are emptied
  when a track is paused and when the path cuts the estimate.

Measured on the final build, three runs of each test alone on an idle
machine, all passing (three publishers at 3.1 / 0.5 / 0.15 Mbit/s, one
subscriber asking for main of all three):

- **7.5 Mbit/s downlink with a 150 ms queue**: first downgrade at about
  0.67 s; settled on one main and two middles 4.7, 5.2 and 3.8 s after
  the start; the link dropped 718 to 802 datagrams in the first two
  seconds and none after; all three on main 7.1, 4.6 and 6.9 s after the
  limit was lifted (the bound asserted is 15 s). Padding in one run:
  2.1 MB in 50 s, about 160 kB per failed probe every 8 s while the
  limit stood.
- **The same with 50 ms one-way delay and 10 ms jitter**: settled at
  5.2, 5.2 and 5.4 s; no drop after the first two seconds; all main 5.3,
  5.2 and 5.4 s after the lift.
- **Unlimited link**: never downgraded.
- **The dip (30% arriving for one second) on an unlimited link**: no
  downgrade in eight of nine runs; in one, less than three mains for
  5.15 s (the test allows 8 s).

What does not hold, plainly:

- Run with the other 44 tests of the file at once, the test's own loop
  stalls for seconds and the runs measure the machine; the tests detect
  that and repeat the run (up to four attempts).
- Intermediate builds failed 12 of 75 runs for reasons that were each
  fixed; the final build has 21 runs behind it, not more.
- After a burst of loss the delay computed from the test client's
  feedback steps up by 12 to 25 ms and stays; the cause was not found,
  and the design works around it (a carried probe resets the delay
  baseline). What a browser's feedback does is not known.
- Not run against any browser: whether a browser accepts transport-wide
  congestion control on some lines of a bundle and not others, how often
  it sends feedback, and what it makes of a second of discarded
  retransmission-stream copies.

### Second round (2026-10-06)

An independent check failed the acceptance (a probe toward 9.0 Mbit/s put
247 kB of padding on the 7.5 Mbit/s link; the budget read 13.7 to
18 Mbit/s). Changed in the SFU; where this differs from the list above,
this is what is built:

- **The estimate never stands above what the path has said.** Until a cut
  is kept or a probe ends it is the start value (1.5 times demand, at
  most 20 Mbit/s). After that a limit holds it: set by a kept cut to the
  larger of the cut estimate and the rate then arriving, lowered by a
  failed probe to just under its target, raised by a carried probe to 1.1
  times its target. Before, the estimator's own climb and the taking back
  of cuts made around a probe carried it to the demand ceiling.
- **A probe climbs and is bounded without feedback.** It rises to its
  target over 400 ms; padding is held back whenever the bytes sent since
  the newest reported packet exceed what the path delivers in the time
  feedback has been silent plus 40 ms; a probe held back by more than
  20 ms worth of its target ends without a verdict in its favour. A queue
  ends a probe only when two reports running show it. After a failure the
  cheapest allowed move is tried first. No probe while a cut is pending
  or a queue stood in the last 3 s. After a carried probe the next may
  start after 0.5 s (not 1 s).
- **The seconds with 1.5 times the usual packets** were the test's
  publishers writing frames late on a loaded machine, and the modelled
  link's queue moving packets across a second's edge; the SFU's count of
  first transmissions per second was level. No pacer was added.

Runs on that build (debug, a loaded machine, synthetic clients): each
narrow test alone by name 10 of 10 (settled 3.7 to 5.8 s, all main 6.0 to
8.9 s after the lift, no link drop after the first two seconds); the
four-package test run with both un-ignored 0 of 3 at first (every attempt
discarded as too loaded to measure, no assertion reached), then 3 of 3
after the downlink tests were made to take turns and given six attempts.
The tests discard an attempt whose own loop stalled more than 250 ms or
whose publishers wrote a frame more than 80 ms late. The two narrow tests
remain `#[ignore]`d and the switch remains off.

### Third round (2026-10-06): a publisher's clump

The second round's tests discarded runs in which a publisher's frames
were written late, and those were the runs that failed. A publisher
whose uplink stalls for 200 ms and delivers its frames together is
ordinary; the SFU forwards on arrival (ADR-0066), so the clump reaches
each subscriber's path as a burst. The discard is gone and the condition
is now made on purpose (two tests: one publisher's clump at 20 s, all
three at 30 s and again 0.3 s after the lift, plain and with delay and
jitter).

What a clump triggered: a cut kept on the queue the clump itself built,
to 0.85 of a half second that held the stall (4.8 and 2.6 Mbit/s on a
path carrying 5.1), which took the main picture away; and, from a small
set, no way back, because every probe ran short of its target by the
rate of the light packets' second copies (counted as sent and also taken
out of the schedule), which no probe toward three middles could pass.

Decided and built:

- **The queue the SFU's own sending explains is not evidence.** For each
  packet the feedback speaks of, the SFU computes how long it would have
  waited on a path that carries exactly the estimate, from when and how
  much it sent; a cut needs a standing queue beyond that. For 1 s after
  a clump (a packet that would have waited 50 ms), on a path that had
  been carrying what is sent quietly, no queue counts; at most once in
  3 s.
- **The first cut on a quiet path is held**: the estimate stays and the
  cut is made only if the queue stands another 300 ms.
- **A deep queue (75 ms beyond the sending's own, for 300 ms) is cut on
  without the delay detector**, once per 300 ms. A 2.35 Mbit/s link
  under 12 Mbit/s of demand was never cut before this.
- **Probes**: second copies counted once; no padding on top of a clump;
  a clump while a probe runs ends it as `disturbed` with nothing held
  against the rate.
- **A track's cost is the mean of its last two seconds**, so the second
  of a stall does not make a larger set look affordable.
- Smoothing a clump toward the subscriber was considered and not built:
  the packets wait as long either way; what it would save is an overflow
  of the path's queue, which remains possible (a 200 ms clump of all
  three publishers is 136 ms of the test link's 150 ms).
- Every downlink test attempt has a deadline and the test a wall-clock
  watchdog.

Runs on the final build, each test alone: both narrow tests 10 of 10,
both clump tests 10 of 10 (the set did not move after any clump before
the lift; all main 5.8 to 6.8 s after it; no link drop after the first
two seconds), very narrow 3 of 3 (all main 9.4 to 11.1 s after the
lift), dip 3 of 3, unlimited 3 of 3. The four-package test run with the five step-6 tests
un-ignored: 3 of 3. The failures of
intermediate builds and what remains weak are in the README. The step-6
tests remain `#[ignore]`d and the switch remains off.

## Note, 2026-10-07: second copies and retransmissions to a browser

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

What that means here: step 3 (the light track asked for again every
20 ms and every packet of it sent twice) did nothing for a browser
until that date in fifteen cases of sixteen, and cost it the bandwidth
of the second copies. The figures of step 3 and step 4 ("no stop in 12
of 20 uplink dips", "median 275 ms") are from the synthetic model, whose
receiver restores correctly, and stand as model results; they were never
seen on a browser. Step 6's probe padding is made of the same copies; a
receiver discards them by sequence number either way.

## Note, 2026-10-09: the SFU asks for a key frame when a burst ends unrepaired

**Measured on a live call that day** (three browsers on the test stack;
the publisher that matters Chrome on a home Wi-Fi; from the maintainer's
packet capture of its uplink): a burst of loss every 30 s, each 0.5 to
1.4 s, mostly about a second; in a burst the audio stream lost 38 to 52 of
50 packets a second, the main video 148 to 223 packets in 0.7 to 0.9 s,
the light and the middle video 22 to 80 each, and the retransmission
stream lost heavily in the same windows (2004 of 6056 of its sequence
numbers ever arrived). Viewers counted 19.5 s (Firefox) and 9.2 s (Safari)
of stalls, on that publisher's heavy and light track alike. In the SFU's
log every key frame request for that publisher in that time was a
viewer's own (`subscriber-pli`), four to five seconds after a burst began:
the picture stood until a browser's decoder gave up, about three seconds
after the burst had ended. Step 3 and step 4 do not cover this case: they
carry a stream through a burst that retransmission repairs within the
playout delay, and this one was not repaired at all.

**Decided and built** (SFU only; host-tested against synthetic `str0m`
clients; not deployed, not seen on a call or against a browser):

- Per video stream of a publisher (main, middle, light each on its own),
  from sequence numbers alone: a burst has **ended** when 5 packets in a
  row arrived in sequence and none went missing for 120 ms; it ended
  **unrepaired** when a packet is then still missing 300 ms after it was
  first known missing, or the burst outran retransmission (a packet
  missing for 1.5 s, or 512 packets behind the newest). Then the SFU asks
  the publisher for a key frame on that stream, once per burst, reason
  `uplink-burst-end`, through the existing bounded path (one per 500 ms
  per track, the bound on unanswered requests), and only for a stream some
  subscriber is being sent. A burst that was repaired asks for nothing.
  `services/sfu/src/burst_keyframe.rs`, `media::drive_burst_keyframes`;
  `LAKKO_SFU_BURST_KEYFRAME=0` switches it off. Every number chosen, not
  measured.
- **The packets of the burst are no longer asked for** once the key frame
  is asked for, or when nobody is being sent the stream (the fifteenth
  change to the vendored `str0m`, `StreamRx::give_up_missing`). Decided
  from the code (they would be asked for up to another half second, each
  answer a retransmission ahead of the key frame) and from the count in
  the measurement below; the effect on a real sender's key frame was not
  measured.
- **A stream that carries FEC is judged by number, not exactly.** A lost
  FEC packet takes a sequence number and is not sent again, so a packet
  that stays missing there may be one nobody needs. The rule there needs
  more missing packets than the FEC share of the burst explains (the
  README has it). The publisher on the call sent FEC.
- **Found on the way and corrected**: the uplink burst detector of step 4
  was not handed FEC packets, so for a publisher that sends FEC every FEC
  packet read as a missing one and its viewers were held at 300 ms from
  the moment they joined, burst or no burst (in the log of the call:
  raised in the very pass a subscriber was given its tracks). It is handed
  them now. A lost FEC packet still reads as a missing packet there.

**Measured** (`measure_burst_end_key_frame`: each of the publisher's
media datagrams lost with probability 0.80 for one second, 20 seeds, each
run before and after, a viewer 300 ms behind, milliseconds as min / median
/ p95 / max; the test's subscriber never asks for a key frame, a browser
on the call did after about three seconds):

| Publisher | Track | Longest gap, before | Longest gap, after | Asked after the burst's end | Key frame whole at the viewer |
|---|---|---|---|---|---|
| throws retransmissions away above 15% | main | 1502 / 5969 / 6086 / 6851, not back in 19 of 20 | 1139 / 1217 / 1498 / 1530, back in 20 of 20 | 109 / 156 / 206 / 217 | 169 / 233 / 628 / 1345 |
| the same | light | 972 / 1259 / 1807 / 2064 | 792 / 1155 / 1263 / 2340 | 109 / 156 / 1384 / 1384 (12 of 20) | 154 / 204 / 2018 / 2018 |
| answers every request | main | 338 / 640 / 1089 / 1093 | 435 / 833 / 1089 / 1089 | never asked | |
| the same | light | 210 / 367 / 595 / 680 | 189 / 346 / 803 / 940 | never asked | |

What that says: where the publisher repairs the burst the change does
nothing (no request in 20 runs; the two columns differ by the spread of
the model). Where it does not, the main picture is back about 0.2 s after
the burst instead of whenever a viewer asks. The light track gained little
in the model, where its few packets were repaired anyway. The second of
the burst itself is not shortened by any of this. Two of the twenty runs
after took 0.35 and 1.2 s from the request to the key frame's first
packet at the viewer; not explained, not seen in thirteen further runs.
Neither model publisher is a browser: which of the two a Chrome sender on
a path it has just cut its rate on resembles is what the next live call
has to show (the time from a burst's end to `uplink-burst-end` in the SFU
log, and the viewers' stall counters).
