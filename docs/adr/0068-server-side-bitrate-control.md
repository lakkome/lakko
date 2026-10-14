<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0068: The SFU decides a publisher's bitrate

- **Status:** Accepted (2026-10-04; the maintainer asked for Lakko's own bitrate control in place of the browser's). Built the same day behind `LAKKO_SFU_BITRATE_CONTROL` (default `client`), host-tested, switched on on the test stack; not yet measured on a call and never run against a browser.
- **Date:** 2026-10-04
- **Context:** group and link calls through the SFU. Builds on
  [ADR-0066](0066-sfu-forwards-rtp-packets.md) and
  [ADR-0067](0067-loss-resilient-call-media.md). SFrame, the forwarding path
  and the subscriber legs do not change.

## Context

Measured on live calls on the test stack (2026-10-04, two laptops, web):

- A home network dropped its outgoing packets for about a second every 30 s.
  Seen at the server per source address, with the SFU's per-second datagram
  count and with a capture on the network interface: the laptop on that
  network fell from about 250 datagrams a second to 35-70; a laptop on a
  mobile network in the same call did not.
- A second of loss is a second of trouble. What the callers saw was about
  25 s of it: the sending browser cut its bitrate estimate from 1.5-3 Mbit/s
  to 100-250 kbit/s within two seconds and raised it by about 8% a second,
  reaching the old rate just before the next dip. For the first 4-6 s it
  encoded 1-10 frames a second.
- The same two laptops on the same networks show no such freeze in other
  calling products.

Today the publisher's bitrate is decided by the browser's send-side congestion
controller from the transport-wide feedback the SFU returns. That controller
treats a burst of loss as a narrow link (its loss-based bound is of the order
of 75 kbit/s divided by the loss fraction; with half the packets lost that is
about 150 kbit/s, which is what was logged; read from the libwebrtc source,
not confirmed in the running browser), and each browser has its own controller
and its own version of it. Lakko controls none of this.

## Decision

The SFU is the receiver of every publisher's media, so it has what an
estimator needs: every packet's send time (`abs-send-time`), arrival time and
size, and the loss. It estimates the bitrate each publisher's path carries and
tells the publisher, with REMB. One rule for every client: web in any browser,
iOS and Android.

1. **Negotiation.** The SFU does not negotiate transport-wide congestion
   control with a client: neither the header extension nor `transport-cc`
   feedback. It negotiates `abs-send-time` and `goog-remb`. A client then runs
   no delay-based or packet-feedback loss estimator of its own; its target is
   the smaller of the SFU's REMB and its own receiver-report loss rule.
2. **The estimate.** Per publisher, from the packets of all its streams:
   - *delay*: inter-arrival deltas of packet groups against their send-time
     deltas, a trend over them, and an over-use decision. Over-use is the
     evidence of a narrow link: packets queue before they are dropped. On
     over-use the estimate goes to 0.85 of the rate actually arriving.
   - *increase*: while not over-using, the estimate rises multiplicatively,
     capped at 1.5 times the highest rate that arrived in the recent past, so
     a pause in arrival does not lower the cap.
   - *loss*: loss alone lowers the estimate only when it lasts. A burst
     shorter than the window (chosen: 3 s) with no over-use changes nothing.
     Loss that persists past the window lowers the estimate in steps.
   - *bounds*: a configured start value and maximum. No floor above the
     codec's minimum: a publisher on a narrow link must be allowed to go low.
3. **Receiver reports stay true.** The loss fraction in the SFU's receiver
   reports is the RFC 3550 value (as clamped in ADR-0067). A client's own
   receiver-report rule may therefore still cut its rate on a lossy report;
   that cut is a fraction of the rate per report, not a collapse to a bound,
   and it is measured before anything else is considered.
4. **Subscriber legs** are unchanged: the SFU forwards. A subscriber's own
   REMB keeps feeding the simulcast layer policy.
5. **Switch.** `LAKKO_SFU_BITRATE_CONTROL=server|client`, default `client`
   until measured on live calls; the test stack runs `server`.
6. **Visible.** Per publisher per diagnostics interval: the estimate, the
   arriving rate, the detector state and the reason for the last change.

## What must be confirmed before and while building

Read from the libwebrtc source by whoever builds it, and then seen on a call:

- that a sender with `goog-remb` and no `transport-cc` takes REMB as its
  ceiling and, in its start phase, as its estimate;
- what the sender does between a cut and the ceiling (the rate of increase);
- how the sender's receiver-report loss rule behaves at 50% and at 95% loss;
- that Firefox honours REMB the same way.

## As built (2026-10-04)

`services/sfu/src/uplink_estimate.rs` (pure; the receive-side algorithm of
libwebrtc's delay estimator: send-time groups, a trendline, additive increase
and multiplicative decrease) and its wiring in `media.rs`. No change to the
vendored `str0m`: the extension map and the feedback flags are configuration.

Confirmed from the libwebrtc M125 source (`goog_cc_network_control.cc`,
`send_side_bandwidth_estimation.cc`), not on a call:

- with `goog-remb` and no transport-wide feedback the sender's target is the
  smaller of its own estimate and REMB;
- REMB raises the sender's estimate only in its start phase: until 2 s after
  the first receiver report, and only while reported loss is zero. The sender
  starts at 300 kbit/s, so the SFU's first REMB (start value, chosen:
  1500 kbit/s) is what lifts it;
- afterwards the sender's own estimate rises 8% a second while reported loss
  is at most 2%, holds between 2% and 10%, and above 10% is multiplied by
  `1 - loss/2` once per report (0.75 at 50%, about 0.53 at 95%);
- so after a one-second burst the SFU's estimate does not move, and the
  sender's own rule costs one or two such cuts and about 4 to 8 s of climbing
  back, against a tenfold cut and 25 s before.

The loss rule as built: loss above 10% holds the rise; once it has lasted 3 s
the estimate steps down once a second. Limits recorded in
`services/sfu/README.md`, "Bitrate control": a reordered packet counts as lost
in the estimator's loss feed; the sender's probe clusters are not used;
Firefox and Safari were not read.

## First live call and the rules that followed (2026-10-04)

Two laptops, web, one on a home network and one on a mobile network, server
mode. The start worked as read from the source: the sender stood at
1500 kbit/s within two seconds. The first dip of the home network, eight
seconds in, took the SFU's own estimate from 1500 kbit/s to 106 kbit/s, and
every later dip to 44-88 kbit/s, from where it rose 8% a second: the
estimator as first built was libwebrtc's, and it did what libwebrtc's does.
During a dip next to nothing arrives, and "0.85 of the arriving rate" is then
next to nothing.

A dip and a link that has narrowed cannot be told apart while they happen, so
the cut stays. What follows it is Lakko's own (`uplink_estimate.rs`, constants
chosen, not measured):

- **No cut in a blackout.** Over-use while less than a fifth of the estimate
  arrives is waited out for up to 2 s.
- **Cuts of one episode are bounded.** Each cut makes the publisher send
  less, so less arrives and the next cut is deeper. Within 3 s of the first
  cut the estimate does not go below a quarter of what it was before.
- **A fast way back.** The estimate held before the episode is remembered,
  and below it the rise is fourfold a second, not 8%. Loss still holds the
  rise: a narrowed path with a full queue shows no delay growth, only loss.
- **Giving the way back up.** Over-use again after the episode and within
  10 s of the last cut, or loss that outlasts the 3 s window, means the link
  is narrower: the old rate is forgotten and the rise is 8% a second.

In the estimator's model of the measured dip (a second in which a third of
the packets arrive, later and later) the estimate goes 2500 -> 625 kbit/s and
is back at 2500 three seconds after the dip ends. On a path that narrows from
above 2 Mbit/s to 500 kbit/s it makes one attempt at the way back, gets
over-use, and then stays within 300-650 kbit/s. Not yet seen on a call.

## Alternatives

- **Keep the browser's controller, shape the feedback** (report burst-lost
  packets as received). Rejected: it hides loss that persists, feeds the
  delay estimator invented arrival times, and still leaves one controller per
  browser.
- **A bitrate floor.** Rejected by the maintainer: a user's link can really be
  narrow.
- **Lakko's own media stack in the browser** (WebCodecs for encoding, its own
  transport, pacing, loss repair and jitter buffer): everything is Lakko's,
  and nothing of WebRTC's media path remains in three clients and the SFU.
  Not chosen now because the SFU can take the bitrate decision without it;
  it is the next step if server-side control inside WebRTC proves not enough.
- **Leave it.** A second of loss costs half a minute.

## Consequences

- One bitrate rule for every browser and both native clients, in one place.
- The browser's probing and its packet-feedback estimators no longer take
  part; start-up speed and recovery speed are what the SFU's estimate and the
  sender's ceiling rule give, to be measured.
- A wrong estimate is Lakko's fault and Lakko's to fix.
- Invariants #2 and C-39 hold: the estimator reads header fields, sizes and
  arrival times, never a payload.

## Amendment, 2026-10-07: a cut for over-use needs a standing queue

Seen on live calls on the test stack: a publisher on a mobile network was
cut for over-use again and again with no loss (503, 191, 53 kbit/s in
fifteen seconds; in an earlier call 2.7 Mbit/s had arrived from the same
network before the estimate was walked down to 73 to 200 kbit/s for
minutes). The browser sends 320x180 at such an estimate, so that person
is a blur for everyone. The round-trip time stood at 40 to 96 ms except
in the two intervals of the deepest cuts (388 and 202 ms); the log had no
line for the queue at a cut.

Cause, in the rule this ADR took from libwebrtc: the delay detector reads
a trend, a mobile uplink's delay moves by tens of milliseconds with the
radio's scheduling, and a cut is to 0.85 of what is arriving. The
publisher obeys, less arrives, and the next cut is taken from that. In
the estimator's model a path with such jitter, no limit and no loss was
cut 34 times in a minute, to 326 kbit/s from 2 Mbit/s.

Decided:

- A cut for over-use is made only when a queue stands on the path: the
  smallest delay (arrival less abs-send-time) of the last 200 ms at least
  25 ms above the smallest of the last 15 to 30 s, for 300 ms. The
  detector's word without that is doubted and counted, and the cut is
  made if the queue goes on standing. A queue 75 ms deep that has stood
  300 ms is cut on without the detector.
- After a cut the queue has to stand as long again before the next, so a
  cut is never anchored to the rate the previous one imposed unless the
  path is still congested at it.
- Unchanged: the way back (to the estimate before an episode at four
  times a second; slowly on a path cut again within ten seconds), and
  the loss rules (a burst changes nothing, lasting loss steps down). No
  floor.
- The queue watch is shared with the downlink estimate of ADR-0070 step 6
  (`uplink_estimate::QueueWatch`), where the same rule was first needed.
- Every cut is logged with its rule, the queue, the trend and the
  arriving rate.

All constants chosen, not measured. In the model: the jitter above cuts
nothing in a minute; a path narrowed from 2 Mbit/s to 800 kbit/s is at or
under 800 within 3 s and back at 2 Mbit/s within 15 s of widening. Not
known until the next call: whether a real mobile uplink shows queues of
25 ms that are not congestion.

### The same day, after the next call

The rule held a mobile publisher at 1.8 to 2.8 Mbit/s while it was alone
on its link. Two defects showed and are fixed:

- 21 of 51 cuts were logged with no queue reading. A cut re-armed the
  wait by pretending a queue had begun to stand, and a reading of "too
  few packets" left an old one in place. The decision now reads the
  queue when it is made: 25 ms or more at that moment, standing 300 ms,
  300 ms since the last cut; without a reading nothing is cut.
- The publisher then sat at 291 kbit/s for minutes with a queue of
  8 ms: its detector alternated between over-use and under-use about
  twice a second, and either state stops the rise in libwebrtc's rule.
  Decided: **the queue reading decides, the detector only asks for
  cuts.** With no queue standing the estimate rises whatever the
  detector says; while one stands nothing rises.

Two publishers behind one bottleneck each keep their own estimate and
are each cut to 0.85 of their own arriving rate; nothing divides the
path between them. Not built. The log of that call does not show the
Firefox publisher ignoring REMB (what arrived followed its estimate
within about a quarter).

## Amendment, 2026-10-11: the estimate also goes out as a per-sender ceiling

### What was measured

On live calls, packet capture at the SFU:

- With the SFU's estimate held at 1000 kbit/s, Chrome sent its main video
  at 720 kbit/s (1031 in total) and Firefox sent its main video at a flat
  1537 kbit/s (1837 in total). Firefox's main sender did not follow the
  server's REMB. Its light sender, which carries a ceiling the client set
  (150 kbit/s), stayed at 125 kbit/s: Firefox does obey a per-sender
  ceiling. (The note above, that an earlier call did not show a Firefox
  publisher ignoring REMB, stands for that call; this one does show it.)
- On a Wi-Fi publisher, lowering what it sent from about 3.2 to about
  1.1 Mbit/s cut the late tail of its own audio at the SFU: the 99th
  percentile of lateness went from 264 ms to 93 ms, and the half-second
  intervals with audio 200 ms late or more from 27 to 2.
- Audio and video lateness of one publisher correlate at 0.91 to 1.00:
  they wait in the same queue. The receiving browser sizes its audio
  buffer to that tail and holds the main video to it, so a publisher's
  standing queue is the conversation's delay.

So the estimate the SFU already computes has to reach every browser in a
form every browser obeys. REMB is a hint a browser's own controller
weighs; a sender's maximum bitrate is a setting.

### Decision

The server decides, the client executes.

- **A frame.** `ServerFrame::PublishBudget { main_kbps, middle_kbps }`,
  serde kind `publish-budget`, on Lakko's own signaling: the ceiling for
  the receiving publisher's main and middle camera sender, kilobits per
  second, each from 1 to 20000 (`MAX_PUBLISH_BUDGET_KBPS`; a frame outside
  the bounds is refused on decode and never composed). The light sender
  keeps its fixed ceiling and audio is never limited; neither is in the
  frame.
- **The rule is the SFU's alone** (`services/sfu/src/publish_budget.rs`,
  pure: no I/O, no clock of its own). There is no allocation rule in any
  client, so three clients cannot drift. From the estimate it takes a
  headroom for audio (the rate arriving on the publisher's audio tracks
  where the SFU already meters it, that is under downlink control, kept
  within 32 to 128 kbit/s; otherwise 64 kbit/s, a constant) and for the
  light sender (150 kbit/s, its ceiling, a constant, when the publisher
  declared one), and divides the rest by 1.15 for retransmissions and
  FEC, which ride on top of an encoder's ceiling (a constant share;
  nothing new is measured). The middle sender gets its plan rate
  (500 kbit/s) when it is needed, giving way only to the main sender's
  floor; the main sender gets the rest when it is needed. Never below a
  floor (main 200 kbit/s, middle 100 kbit/s), never above the plan
  maximum (main 2500 kbit/s, middle 500 kbit/s). A sender `video-needed`
  says nobody watches takes nothing from the budget and is given its
  ceiling anyway, the one it would have if it were switched on now.
- **When.** The first budget as soon as the publisher is connected and
  has sent video (when its estimate first goes out as REMB too). A lower
  one at once (a ceiling under 97% of what was told). A change of which
  senders are needed at once. A higher one no sooner than 2 s after the
  last frame and only when a ceiling differs by 10% of what was told, or
  reaches its plan maximum. The rule is looked at every 100 ms.
- **Who.** Every publisher whose bitrate the SFU decides
  (`LAKKO_SFU_BITRATE_CONTROL=server`), behind `LAKKO_SFU_PUBLISH_BUDGET`
  (on unless `0`). With the client deciding its own bitrate there is no
  estimate and no frame.
- **REMB goes on exactly as before.** The frame is beside it, not in
  place of it.
- **The client** sets each figure as `encodings[0].maxBitrate` of that
  sender with `setParameters`, serialised with every other write to the
  sender's parameters, the last frame winning; before the first frame a
  sender keeps the ceiling it was created with. Web is built
  (`apps/web/src/calls/publish-budget.ts`); iOS and Android obey it too
  since 2026-10-12 (`PublishBudget.swift`, `PublishBudget.kt`), host-tested
  only.

Every constant is chosen, not measured.

### Limits

- **The client must cooperate.** A modified client can ignore the frame,
  as it could ignore REMB. Nothing here polices what a publisher sends;
  it makes the well-behaved clients behave alike.
- **The floors can exceed the path.** Below about 560 kbit/s of estimate
  with both senders needed, the two floors, the light sender and audio
  add up to more than the estimate. The alternative is no picture.
- **The ceiling bounds the encoder, not the wire.** Retransmissions and
  FEC are allowed for by a constant share, not counted.
- **The first budget is made from the start value** of the estimate
  (1500 kbit/s by default: a main ceiling of 618 kbit/s with a middle
  sender needed), and the next higher one comes 2 s later at the
  earliest.
- **The estimate now sees what it limited.** A ceiling holds what arrives
  under the estimate, and the estimate's rise is capped at 1.5 times what
  arrived. The headroom is well inside that factor, so the climb goes on;
  this was reasoned, not observed.
- **Nothing here was seen on a call yet.** Host tests only: the rule, the
  frame on both FFI surfaces, the frame arriving over the SFU's signaling
  in the synthetic-client test, and the web client against a fake
  `RTCRtpSender`. Not run against any browser. That Firefox obeys
  `maxBitrate` on its main sender is inferred from its light sender; that
  Safari does is not known.
