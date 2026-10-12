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

## Note, 2026-10-09: step 6 on its first live call

**Measured on a live call that day** (18:28 to 18:35 UTC, four browsers
on the test stack, `LAKKO_SFU_DOWNLINK_CONTROL=1`; from the SFU's log):
on the two browsers that shared one mobile link the allocation cycled.
One of them was moved from a middle picture to its light one 13 times
and back 13 times in six and a half minutes and spent 89 of its last
287 s on less than it asked for; the other 7 and 7. The two browsers on
a Wi-Fi: 2 and 2, and none. On a synthetic rate-limited link the same
build settles and stays (the third round above).

What the 13 were, by the log (the README's "The fourth round" has every
timestamp):

- 2 were no downgrade: a publisher listed for the first time starts at
  its light picture and the line read "from middle".
- 6 were the mobile link itself stalling: a queue of 103 to 627 ms on
  the downlink and, in the same seconds, of 88 to 857 ms on the same two
  browsers' uplinks. Five times in two minutes. One of the six came a
  second after a picture had been given with a queue already standing.
- 3 came with no cut of the estimate at all, on a path with nothing
  wrong: twice the set being given had come to cost more than a budget
  pinned at an old limit (a fourth person joining; the publishers' rates
  climbing), once the allocation, re-sharing what the set needed at
  every run, moved the budget to a more important publisher whose track
  had come to cost less for a moment (which took a picture and gave
  none).
- 2 followed a clump the SFU itself forwarded when another publisher's
  uplink burst ended (with the key frame the SFU asks for since the note
  above), on a link two subscribers share, so that each one's model of
  its own sending explained half of the queue.

None was a cut on delay variation: the detector spoke 292 times on that
subscriber and 282 cuts were taken back for want of a queue; the ten
kept had 103 to 597 ms standing. And the way back was slow for reasons
of its own: probes given up as `disturbed` within milliseconds because
a key frame of a light picture counted as a clump against an estimate a
stall had left at 130 kbit/s, and three probes carried in a row before
one picture was given, the budget standing exactly at each probe's rate
while the set had come to need a little more.

**Decided and built** (SFU only; host-tested; not deployed, not on a
call since):

- **The limit follows the set being given.** When what that set puts on
  the wire has come to more than the estimate on a quiet path (no queue,
  no cut pending, nothing lowered for 3 s), limit and estimate go to 1.1
  times it. A path that is carrying a set says so; this is the
  downlink's form of the uplink's "the estimate rises whenever no queue
  stands", bounded by what is in fact being sent. A set is taken away by
  the path's word (a kept cut, lasting loss) and by nothing else.
- **A set that fits is kept as it is.** The allocation re-shares only
  when the subscriber changes its order of importance.
- **Nothing is given while a queue stands** or a cut is pending.
- **A carried probe leaves the estimate a tenth above its rate**, as it
  left the limit.
- **A clump is measured against the rate a probe tries**, and no probe
  starts while one is going out.
- **A set that proved too large waits longer each time.** Given, and
  taken away again by the path within 20 s: that set and every larger
  one is not tried, and not given on any proof, for 10 s, then 20, 40
  and 60 s every time. A set is which pictures it holds (one per middle
  picture, six per main one), not what they cost at the moment: the
  rate one set needs moved by a fifth with the publishers' encoders. A
  set that stays 20 s, or five minutes without one proving too large,
  puts the wait back to 10 s; a smaller set is not held. A failed probe is not this and keeps its 5 s, then 8 s. Nor is
  a path that collapsed under the set (the cut left the estimate under
  half of what the set put on the wire): that is a stall, it says
  nothing of the set, and the pictures come back as fast as the probes
  allow. The first build of the rule counted every taking-away and, on
  the model, made three one-second dips cost 70 s of waiting. What
  step 6 said of the way back ("climbs back within 15 s of the limit
  lifting") therefore holds for a set that was never given and taken
  back; one that was is found when its wait ends, **60 s at the worst**.
  All constants chosen.
- **A stall is still answered at once**, by everything the cut calls
  for. Holding the middle pictures through the first second of a stall
  was considered and not built: on a path that has really narrowed it is
  a second more of queue in front of the light pictures.
- The log lines carry why a picture moved (`why`), what the set costs,
  and on every cut what was arriving and being sent.

**Measured on a model** of that link (one test,
`a_mobile_like_downlink_holds_a_picture_instead_of_cycling`; every
figure of the model chosen to resemble the log, none measured on a
mobile network; synthetic `str0m` clients, one subscriber on the link):
the build before (two runs) took a picture away with no cut of the
estimate once in each run and let 7 of 19 and 9 of 21 carried probes end
without giving anything; the build after did neither in any of three
runs (a fourth, without the log, took nothing away once the path was
its own). After each of three 1.2 s dips to a tenth of the path the three
middle pictures were back in 5.4 to 6.9 s in eleven cases of twelve
(before: 6.4 to 12.2 s); no light packet was lost in any run of either build. The
model does **not** show the longer waits curing a cycle: while a
competing flow came and went, both builds took pictures away two or
three times in 108 s and then sat on the smallest set that held; in an
earlier form of the model the build before changed 16 times in that
phase and the final build 13, one run each. That rule rests on its unit
tests and on the two places in the call's log, not on a measured
difference. The five step-6 tests, each alone, three times: 15 of 15,
no attempt discarded. Figures, per run, in the README.

**What remains weak**: after a stall the pictures come back one probe
at a time (5.4 to 6.9 s on the model); a set that cannot be held is still
tried once a minute; two subscribers behind one link disturb each
other's probes and each other's clump model, and the SFU does not know
they share a path; both mobile browsers were sending a main picture
nobody was being sent, over the uplink of the link that stalled; and
none of this has been on a call. The step-6 tests remain `#[ignore]`d
and the switch remains off by default.

## Note, 2026-10-11: senders nobody watches (`video-needed`)

On the live call of 2026-10-09 every tile showed the middle picture while
every browser uploaded its main sender to nobody. The maintainer decided
the SFU tells each publisher which of its camera senders somebody is being
sent, so the client can stop the others. Built the same day on the SFU, the
wire crate and both FFI surfaces; host-tested; **no client reads the frame
yet** and nothing was run against a browser.

- **Frame.** `ServerFrame::VideoNeeded { main, middle }`
  (`{"kind":"video-needed","main":false,"middle":true}`), to a publisher
  about its own senders. The light sender is always needed and is not in
  it. Only a publisher that declared a light sender is told.
- **Rule** (`services/sfu/src/video_needed.rs`, `MediaPeer::heavy_wanted_of`).
  A sender is needed only while some subscriber has **named** the
  publisher at that quality in its latest `video-wants`: `main` for main
  (and, under downlink control, for middle too, which is what its
  allocation falls back to); `middle` for middle and never for main, also
  while that subscriber's middle track is still being negotiated. A
  subscriber that sent no `video-wants`, or whose set does not name the
  publisher, counts for nothing heavy (maintainer's decision, same day,
  after a first build that counted silence as `main`: a sixth person
  joining a call of five would have made five publishers start a 720p
  encoder). What the SFU forwards to such a subscriber is unchanged, so
  it sees the light track only when a publisher's main sender is off; a
  client that does not speak `video-wants` is not supported with the
  switch on. `true` is sent at once; `false` after 10 s unwanted without
  a break, so the first frame says `true`. A publisher without a middle
  sender is always told `middle: false`.
- **Around an idle sender** (told to stop, then silent for 300 ms). A track
  resuming from it waits without using up its key frame requests; at its
  return the SFU asks for a key frame unless the first packet begins one;
  no retransmission is asked of it; its stream is forgotten by both burst
  detectors and the estimate's loss counter, so neither the stop nor the
  return is a burst; the publisher's bitrate estimate is put back to what
  it was when the sender was told to stop, and a REMB sent; downlink
  control takes an idle sender to cost its last measured rate for 60 s,
  then 2.5 Mbit/s (main) or 500 kbit/s (middle), never zero.
- **Switch.** `LAKKO_SFU_VIDEO_NEEDED=0` turns it off; on by default.
- **Tests.** Unit tests of the rule and of each item above, and one
  end-to-end test through the production media task
  (`a_sender_nobody_watches_is_switched_off_and_comes_back_at_a_key_frame`;
  observed: told off 9.8 s after the publisher's first frame, nobody
  having named main; a third subscriber joining with `light` then `middle`
  caused no frame; told on 3 to 4 ms after a subscriber named main, the
  key frame at that viewer 84 to 88 ms after its request).
- **Open.** The web client (it has to act on the frame). A publisher that
  declares no light sender is told nothing, which is every client that is
  not on the three-sender contract of step 5. What a browser does when a
  sender is switched back on (a key frame first or not, the same sequence
  numbers or not, how fast its own allocation gives the sender its rate)
  is not known from this work. All constants chosen, none measured.
  Details and the client contract: `services/sfu/README.md`, "Senders
  nobody watches".

## Note, 2026-10-11: the web client acts on `video-needed`; iOS and Android are ported

Written from the tree at `5b7c89a2` by reading the sources and the commit
record; nothing was built or run for this note.

**Correction to the note above.** "No client reads the frame yet" was true
of the build that note describes. The same day the web client was given
it: `apps/web/src/calls/sender-activity.ts` decides, per camera sender,
from the SFU's last `video-needed`, the people count and the camera
state, whether the sender is active, and the session sets
`encodings[0].active` of the main and the middle sender accordingly,
serialised per sender; the light sender is never switched off. The call
log carries `needed=` and `active=` per sender.

**Seen on live calls since** (web only; the commit record of 2026-10-11,
which gives no figures): the main sender going off in a grid and coming
back for a pinned person, in five browsers; no downlink oscillation; the
burst-end key frame. **Not seen:** the stage following the speaker (why
it did not on the last call was not established) and the pin button.
So the "Open" list of the note above loses its first item, the web
client, and keeps the rest: what a browser sends first when a sender is
switched back on was not measured on those calls as far as this
document knows.

**iOS and Android** now speak the same contract as the web client
(steps 1, 2 and 5, `video-given` of step 6, and `video-needed`):

- **Three ordinary senders** in place of the simulcast code of
  ADR-0067 step 5 (`CameraSender.swift`, `CameraSenders.kt`; Android's
  `CameraLayers.kt` is deleted): main (SFrame stream index 1), light
  (quarter size, 150 kbit/s, index 3) and middle (half size, 500 kbit/s,
  index 4, active from three people by `middle_sender_wanted`). Each
  copy is a second track on the same video source on its own send-only
  transceiver, and a copy whose encryptor cannot be attached is removed
  (invariant #4). The offer names the light and the middle lines
  (`composeSfuSdpOfferWithMiddle`).
- **`video-wants`** per tile by its width through
  `lakko_ui_rules::tile_video_quality` (`VideoWants.swift`,
  `VideoWants.kt`); **`video-given`** is read and a tile shows the lower
  of what it wants and what the SFU gives (`VideoGiven.swift`,
  `VideoGiven.kt`); remote tracks are routed by `<peer>`, `<peer>~mid`
  and `<peer>~light`, and the shared `layer_display` rule picks the
  shown one.
- **`video-needed`** switches the main and the middle encoding on and
  off; the light one stays on (`SenderActivity.swift`,
  `SenderActivity.kt`).
- The codec of each sender is given to its SFrame encryptor after each
  negotiation (ADR-0071 phase 2).
- The stage and pin control of `docs/ux/group-call-grid.md` 6.2 and the
  speaking-floor rule (the own microphone does not take the floor).

**What exists as evidence for the native side:** unit tests of the
sender plan, the mids, needed-to-active, wants, given, stream routing,
the pin reveal and the floor rule (`GroupCallProtocolTests.swift` and
the `calls` test package on Android). **What does not:** a run on a
device or an emulator, a native client in a call against the SFU, a
native publisher switched off and on by `video-needed`, a native viewer
under downlink control, and any web-to-native media since ADR-0071.
The live observations in this ADR are all of browsers.

**Unbuilt, unchanged:** the `jitterBufferTarget` fallback for a receiver
that ignores the playout-delay extension; a fallback to main when a
wanted middle track carries nothing. Downlink control is still off by
default and its acceptance tests are still `#[ignore]`d.

## Note, 2026-10-11: a burst that left media missing on a stream that is half FEC

**Measured on a live call that day** (four to five browsers on the test
stack; the SFU's log and the browsers' own logs):

- A Firefox viewer stood still for 3.3 to 4.0 s, about every 30 s, on the
  pictures of three different publishers. Chrome on the same link had no
  stop of 1 s or more. In each episode Firefox went on receiving packets,
  decoded nothing, went on asking for the missing packets, and after its
  own 3 s timeout asked for a key frame.
- When the SFU itself asked at the end of the publisher's burst
  (`uplink-burst-end`, for example `lost=82 still_missing=25`), the same
  viewer's stop was about 1.6 s. In six episodes the SFU asked for nothing
  and the first request was the viewer's own (`subscriber-pli`) three
  seconds later.
- In those six the publisher's stream carried a large FEC share (per 5 s:
  `packets=329 ... fec_packets=327`, `packets=347 ... fec_packets=202`),
  and the viewer's receiver reports showed 5% to 20% loss in the interval,
  with 18 to 48 NACKs that were never answered.

**Read from the code:** the rule of 2026-10-09 judged a stream that
carries FEC by number alone (`Stream::beyond_fec`): unrepaired only when
more packets stayed missing than 2 plus one and a half times the FEC share
of what the burst lost. With a share near one half that is three quarters
of everything the burst lost, so media that stayed missing was explained
away as lost FEC. The code confirmed the reading; it could not confirm that
this is what happened in each of the six episodes, since a burst that
asked for nothing left no line in the log.

**Decided and built** (SFU only; `services/sfu/src/burst_keyframe.rs`,
`frame_descriptor::packet_position`, `MediaPeer::observe_burst_end`):

- The times, one answer per burst, and a key frame that has started
  arriving closing what was missing before it, are unchanged.
- When a burst is judged, each packet still missing is known media, known
  not media, or unknown:
  - **media** when an FEC packet that arrived lists it as protected, or
    when the frame descriptors of the media packets on the two sides of
    its gap say so: the frame on the left has no end, the frame on the
    right has no beginning, or the frame ids step by more than one;
  - **not media** when it lies between a packet that ends frame `n` and
    one that begins frame `n + 1`;
  - **unknown** otherwise.
- One packet of known media still missing asks for the key frame, whatever
  the FEC share. Packets known not to be media never ask. The count rule
  stays for the unknown ones only, with the FEC packets the burst is
  expected to have lost reduced by those already known not to be media.
- The SFU now reads two more fields of the generic frame descriptor, both
  in the clear already: the end-of-frame flag of every media packet and
  the 16-bit frame id of a frame's first packet. No payload (C-39).
- Frame ids are used on a stream only after eight frames in a row followed
  the frame before with nothing missing between (simulcast layers of one
  libwebrtc sender share a counter (read in libwebrtc M125, `rtp_video_sender.cc`), so a layer's ids
  skip; such a stream stays on the count rule). The boundary inference is
  dropped for a stream on which an FEC packet was seen inside a frame.
- State per stream is bounded: at most 512 missing packets and a fixed
  table of 1024 eight-byte entries for the packets that arrived lately; at
  most 12 streams per publisher.
- The log line of an unrepaired burst gains `known_media`,
  `known_not_media`, `unknown` and `why`; a new INFO line,
  `media: a burst of loss on a publisher's stream ended with packets
  missing and asked for nothing`, carries the same counts and the reason
  (`not-media` / `within-fec-share`), so the next call's log shows every
  burst.

**Evidence:** unit tests only. A modelled stream in which half the packets
are FEC loses 21 in a row and three media packets never come back: by
number alone nothing is asked (the old rule), through the masks one of the
three is found and the key frame is asked, through the descriptors all
three are found and six of the eleven lost FEC packets are known for what
they are. A stream that loses only FEC packets, sixty in a row included,
asks for nothing. The same case runs through `MediaPeer` with real FEC
headers and descriptor bytes.

**Not seen on a call, and not known:** whether what a browser's surviving
packets say names the media that is missing in a real burst (a lost packet
takes its descriptor with it, a lost FEC packet names nothing); whether a
Chrome sender ever puts an FEC packet inside a frame or skips a frame id
on one of Lakko's three senders (either only moves packets to unknown);
how often the new line reports `within-fec-share` with a viewer's own
request following it, which is the case this change is meant to remove.
Firefox's 3 s timeout itself is untouched.

## Note, 2026-10-12: a remote person's sound and picture are kept together by Lakko's own rule

**Read in the libwebrtc M125 source.** The engine synchronises audio with
ONE video stream per sync group, the first (`call/call.cc`,
`Call::ConfigureSync`: "Only sync the first A/V pair within this sync
group"). A sync group is the stream id. A subscriber gets, per remote
person, one audio track and up to three video tracks: main (stream id
`<peer>`), middle (`<peer>~mid`) and light (`<peer>~light`). Main shares
the audio's stream id; the companions of steps 1 and 5 do not. So the
engine synchronises main only, in steps of at most 80 ms
(`video/stream_synchronization.cc`, `kMaxChangeMs`) and with a dead band of
30 ms (`kMinDeltaMs`), and nothing ties middle or light to the sound.

**Measured on a live call on 2026-10-11** (Chrome, the receiver lines of
`apps/web/src/calls/media-counters.ts`): the audio jitter buffer's target
was 320 to 600 ms (the publisher's uplink delivered a tenth of its audio
packets more than 200 ms late); main, synchronised by the engine, followed
it at 420 to 630 ms although its network alone needed 70 to 230 ms; light
sat at 290 ms, the floor of step 4, so a tile showing it ran 100 to 300 ms
ahead of the sound. At one moment the audio buffer was 990 ms against
460 ms of video.

**Decided.** Lakko's own code aligns the delays and the engine executes:
the rule of this ADR, applied to lip sync. One shared rule,
`lakko_ui_rules::av_align` (`AvAlign`, one per remote person), on both FFI
surfaces. Once per reading interval a client gives it, for the audio
receiver and each video track, whether packets flow, the measured average
jitter-buffer delay, the network-only need where the platform reports it,
whether a tile shows the track and whether the engine synchronises it; it
returns the target playout delay to set on each receiver
(`RTCRtpReceiver.jitterBufferTarget` on the web, the receivers' minimum
jitter-buffer delay on the native clients) and, per video track, the skew
(video delay minus audio delay) and the reason.

- The sound is the reference. A track the engine does not synchronise is
  made to wait for the sound: its target follows the audio's measured
  delay, raised by at most 240 ms a second and lowered by at most 80.
- A target is a hint, so it is corrected by what is measured: once a
  track has settled, half of the remaining skew goes into a trim of at
  most 90 ms, until the skew is inside 30 ms. An engine that does not
  follow is noticed after three settled readings and left alone.
- A shown track that stays later than the sound with its target as low as
  the trim allows (its own network, or the floor of step 4) makes the
  sound wait for it, by at most 300 ms above what the audio needs for
  itself. Beyond that the picture is left late: a picture path must not
  be able to drag the conversation delay without limit.
- A track the engine synchronises (main) is observed and never written. A
  track that is not flowing is left alone and starts again at the sound's
  delay. A missing reading changes nothing.

Every constant is chosen, not measured; each is defended in the module's
documentation. This is also the client-side setter the steps above listed
as unbuilt ("the `jitterBufferTarget` fallback for a receiver that ignores
the playout-delay extension"), but used for another purpose: it does not
reproduce the server's floor, it ties the picture to the sound.

**What it costs.** In a grid, where tiles show middle or light, the
picture now waits for the sound: on the measured call that is 100 to
300 ms more picture delay on those tiles, the delay the sound and the main
track already had. After a burst on a quiet audio path the sound waits for
a light track held at the 300 ms floor of step 4, so that floor now costs
the conversation up to 300 ms where it used to cost only the picture.
When the sound's delay falls quickly, a video target is lowered more
slowly (a receiver shortens its delay by playing fast), so the picture is
late for some seconds: in the model, 166 ms at worst and inside 30 ms
again after six readings, for a drop from 600 to 320 ms.

**Evidence: a model only.** Unit tests, a property test over arbitrary
readings, and a deterministic simulation (a receiver whose delay moves
half of the way, or a quarter, towards the largest of its network need,
the server's floor and its target per one-second reading, read with
6 ms of noise). On that model: light at the 290 ms floor under a steady
450 ms sound is inside the dead band after 3 readings (residual 4 ms);
the 990 against 460 moment after 5 readings, 10 with the slower receiver,
without overshoot; a sound wandering between 320 and 600 ms at 10 ms a
second is followed within 32 ms; a light track at the floor under a
120 ms sound makes the sound wait after 8 readings and is inside the band
after 10; a picture needing 700 ms against a 120 ms sound leaves the
sound at 420 ms and the picture 277 ms late; an engine that ignores the
video or the audio target ends with a standing target and no further
changes. None of the settled cases changes a target in its last 30
readings.

**Not known, and nothing seen on a call.** Which browsers have
`jitterBufferTarget` on audio and on video receivers (it must be
feature-detected; the rule is told which setters exist and says what it
then cannot do) and whether the native bindings expose a setter (they do
not; see the next note for how the native clients reach the engine's). How an
engine honours the hint: how fast it moves, whether it lands on the value
or near it, and how a hint on the audio receiver interacts with the
engine's own synchronisation of main. Whether the network-only figure a
browser reports excludes the playout-delay extension, as the measurement
suggests. Whether equal jitter-buffer delays mean lip sync closely enough:
the rule compares buffer delays and does not read capture timestamps. How
a real receiver's lag and noise compare with the model's. No client is
wired to the rule yet.

## Note, 2026-10-12: the native clients set a receiver's delay through the engine's C++ object

**The gap.** Neither native binding wraps a setter for a receiver's
playout delay: `RTCRtpReceiver` of stasel/WebRTC 125.0.0 has `receiverId`,
`parameters`, `track` and `delegate`; `org.webrtc.RtpReceiver` of
`io.github.webrtc-sdk:android:125.6422.07` has `track()`,
`getParameters()`, `id()`, `SetObserver`, `setFrameDecryptor` and
`dispose()`. The engine has
`webrtc::RtpReceiverInterface::SetJitterBufferMinimumDelay(
absl::optional<double> delay_seconds)`. The maintainer chose to reach it
through the native layer that already implements libwebrtc's frame
encryptor and decryptor interfaces. A wrong virtual call is a crash or
memory corruption, so the evidence came first.

**Which source each binary is built from.** stasel/WebRTC 125.0.0: its
release notes name `branch-heads/6422` commit
`8505a9838ea91c66c96c173d30cd66f9dbcc7548`, its README says the source is
unmodified, and the cached archive has the checksum `Package.swift` pins
(`4da2f9c7...13ae8`). The Android AAR is the webrtc-sdk fork, branch
`m125_release`; no tag of the fork names the commit of 125.6422.07, but on
that branch `api/rtp_receiver_interface.h`, `api/ref_count.h`,
`pc/rtp_receiver_proxy.h` and the JNI `rtp_receiver.cc` were never touched
by the fork: they are byte-identical to upstream. The class has, in
declaration order, `AddRef`, `Release`, the destructor (two slots),
`track`, `dtls_transport`, `stream_ids`, `streams`, `media_type`, `id`,
`GetParameters`, `SetParameters`, `SetObserver`,
`SetJitterBufferMinimumDelay` (slot 13), `GetSources`,
`SetFrameDecryptor`, `GetFrameDecryptor`,
`SetDepacketizerToDecoderFrameTransformer`.

**Read from the machine code of every slice Lakko ships**, not reasoned
from the header:

| Slice | Slot encoding | Setter | Read from |
|---|---|---|---|
| iOS device arm64, simulator arm64, simulator x86_64 | 8-byte address | `+0x68` | the framework's Objective-C methods call `id` at `+0x48`, `GetParameters` at `+0x50`, `SetObserver` at `+0x60`, `SetFrameDecryptor` at `+0x78`; slot 13 of the `VideoRtpReceiver` vtable is a function that stores its argument in the cached delay and forwards `GetMs()` to the media channel, slot 14 takes a struct-return pointer |
| Android x86_64 | 8-byte address | `+0x68` | `nativeGetId` `+0x48`, `nativeGetParameters` `+0x50`, `nativeSetObserver` `+0x60`, `nativeSetFrameDecryptor` `+0x78`; slot 13 the same function |
| Android arm64-v8a | signed 32-bit offset from the vtable (relative vtables) | `+0x34` | `nativeGetId` `+0x24`, `nativeGetParameters` `+0x28`, `nativeSetObserver` `+0x30`, `nativeSetFrameDecryptor` `+0x3c`; slot 13 the same function |

The argument (`std::optional<double>` in these builds:
`ABSL_OPTION_USE_STD_OPTIONAL 2`) arrives in every slice as two integer
register words, the double's bits and then the engaged flag (arm64
`x1` / `w2`, x86_64 `rsi` / `dl`), read from the callee
`JitterBufferDelay::Set`. On x86_64 that is not where a plain
`{double, bool}` travels (`xmm0`), which a hand-written struct would have
got wrong. Two things in this table decide the design: the three binaries
do not share one vtable encoding, and the argument's convention is not the
obvious one. So the call is made by slot, with the encoding and the
argument's shape written out per platform
(`apps/shared/sframe-bridge/receiver-delay.h`), not through a class
declaration compiled with whatever the build's flags give.

**How the engine object is reached.** iOS: the framework's non-public
`nativeRtpReceiver` accessor (a third-party framework's selector, not an
Apple one), called only when its Objective-C type encoding is the one the
binary was read with; it returns the `scoped_refptr` in memory, and the
reference it adds is released. Android: the SDK's package-private
`RtpReceiver.getNativeRtpReceiver()`, a raw `RtpReceiverInterface*`, called
from one class placed in package `org.webrtc`
(`LakkoRtpReceiverAccess`); no reflection. The receivers written to are the
ones `onAddTrack` hands out, which live until the peer connection is
disposed; a write is excluded from `close()` by a lock.

**Guards.** (1) Identity: the setter is offered only when the loaded
WebRTC image is one the table was read from, by Mach-O `LC_UUID` (three
slices) or ELF build id (two); any other image is observe-only, logged
once. (2) A bracket probe before the first set of a process: the receiver
is asked `media_type()` (a slot before the setter; must be the track's
kind) and `GetFrameDecryptor()` (a slot after it; must be the decryptor
Lakko attached); only then is the setter's slot called. A wrong answer
stops every set for the process. `media_type()` is asked again on every
set. (3) Drift: the iOS package is pinned to exactly 125.0.0
(`project.yml`), an iOS test compares the loaded image's identity with the
pinned ones, and an Android host test compares the version catalog with the
version the evidence was gathered for.

**Threading.** The object the app holds is a proxy: the setter and
`GetFrameDecryptor` block the caller until the engine's worker thread has
run them; `media_type` does not leave the calling thread. iOS runs the set
on the serial queue every other blocking libwebrtc call uses. Android runs
it on the call's scope, as it already does `RtpSender.setParameters`.

**Evidence of the code itself.** A host test with a fake receiver whose
vtable the host compiler lays out from the engine's declaration order
(`test_receiver_delay.cc`, run for arm64 and x86_64) and a hand-built
relative table; the compiled Android library reads `+0x34` relative on
arm64 and `+0x68` absolute on x86_64. The host test cannot show how the
shipped binaries are laid out; the table above is what shows that.

**Residual risk.** The identity pin makes "another binary" impossible, so
what remains is an error in reading these binaries, which the bracket
probe and the function-shape reading were made to catch. The probe's own
two calls are through slots read the same way, so it is not independent
of that reading. The receiver handed out is assumed to be the proxy of an
audio or video receiver; the framework's own methods make the same
assumption. A set blocks its caller for one hop to the worker thread. On
Android a `dispose()` of the receiver outside `close()` during a set would
be a use-after-free; nothing in Lakko does that.

**Found on the way, not fixed here.** `tools/build-sframe-jni.sh` passes
`-fexperimental-relative-c++-abi-vtables` for both Android ABIs, and the
x86_64 `libsframe_jni.so` is built with relative vtables
(`movslq 0xc(%rax), %rcx; addq %rax, %rcx; callq *%rcx`), while the x86_64
`libjingle_peerconnection_so.so` calls a decryptor's `AddRef` as
`callq *(%rax)`. Read from both binaries: on an x86_64 emulator the first
`AddRef` libwebrtc makes on a Lakko frame encryptor or decryptor jumps to
a garbage address. The flag belongs to arm64 only. Not run to confirm.

**Not seen on a device.** Nothing here has run against a real call. A
device run has to show the capability line with both setters `yes`, the
probe's "passed" line, and `ask=` values followed by the measured delay of
that receiver over the next readings.
