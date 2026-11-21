<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0083: Screen sharing — one share per call, ordered the same on every device

- **Status:** Accepted (2026-10-25, maintainer: as written). Built 2026-10-25
  in `lakko-calls`, `lakko-sfu-proto`, `lakko-ui-rules`, both FFI surfaces,
  the NSE classifier, `services/sfu`, web (sends and receives) and iOS /
  Android (receive only); host-tested only, never run in a browser, on a
  device or in a live call, not deployed.
- **Date:** 2026-10-25
- **Context:** UX spec [`docs/ux/screen-share.md`](../ux/screen-share.md)
  (maintainer decisions of 2026-10-25), group-call grid
  ([`docs/ux/group-call-grid.md`](../ux/group-call-grid.md) section 6.3),
  per-stream SFrame keys ([ADR-0035](0035-sframe-per-stream-kid-key.md)),
  the sealed call relay ([ADR-0078](0078-servers-cannot-link-accounts.md)
  section 2), the camera's senders and the SFU's track roles
  ([ADR-0070](0070-call-media-through-a-loss-burst.md)), the publish budget
  ([ADR-0068](0068-server-side-bitrate-control.md)), the media frame layout
  ([ADR-0071](0071-codec-header-in-the-clear.md)), the mutual-ordering lesson
  of [ADR-0044](0044-push-routing-secret-recency.md)
- **Amends:** ADR-0035 (stream index 2 is used, stream index 5 is added),
  ADR-0070 (a fourth track role at the SFU)

## Context

Nobody can show a screen in a Lakko call. `getDisplayMedia` is not called
anywhere, the roster's `screenSharing` is always false, and both web nginx
configurations send `Permissions-Policy: display-capture=()`, which forbids
the browser from offering it. The UX spec decided the product shape:

- sending from the web client in a desktop browser and from the desktop
  client when it exists; never from iOS, Android or a phone browser;
- receiving and showing a share on every client, phones included;
- the shared tab or system sound sent when the browser offers it;
- one-to-one and group calls;
- **one share per call: a new share stops the one before it**, and two
  starts at the same moment leave exactly one share on every device;
- a viewer's pin wins over a new share (a local layout rule, not protocol).

What already exists and what this decision rests on:

- **Group calls** go through the SFU. Each participant's media is sealed
  with SFrame under a key derived from the KID `(sender_leaf << 8) |
  stream_index` (ADR-0035). Stream indices in use: 0 microphone, 1 camera
  main, 3 camera light, 4 camera middle; **2 is reserved for a screen share**
  and nothing seals under it. A receiver derives the key from the KID it
  reads off each frame, so a decryptor needs no stream index of its own.
- The SFU knows each camera sender's role from the offer: `light_mids` and
  `middle_mids` name the light and middle lines, the SFU forwards them under
  the stream ids `<peer>~light` and `<peer>~mid`, and the main line under
  `<peer>` (`services/sfu/src/media.rs`, `TrackRole`).
- In-call state between participants travels on the SFU `signal-relay`,
  sealed under the relay key every member derives from the call seed
  (`lakko_calls::relay`). Two plaintexts exist: the **peer announcement**,
  signed by the sender's MLS leaf key over the call id and its SFU peer id,
  and the **camera media state** (`lakko_contacts::media_state`, one flag,
  unsigned), re-sent to a peer that joins.
- **One-to-one calls** are peer to peer. Their signalling is
  `CallSignalingPayload` (`schemas/mls-app/call_signaling_v1.proto`), an MLS
  application message in the conversation, authenticated by the sender's
  leaf; a camera added mid-call is a further track and a renegotiation. On
  web and native alike the one-to-one camera is sealed with SFrame under the
  call key (web `call-session.ts` `SFrameAttachers`; a comment there that
  still calls the path "no SFrame" is stale). A share follows each path
  exactly as the camera does, under its own stream indices.

The hard part is the one-share rule. Every device sees the starts in its own
order: two people pressing Share within the relay delay each hear the other
second. A rule like "the start I heard last wins" leaves two shares alive, or
none, depending on who is looking. ADR-0044 recorded the same trap for the
push routing secret: an order is only useful if every device computes the
same answer from the same set, whatever order it arrived in.

## Decision

### 1. A share announcement, signed and ordered

A share is announced by a **share state** the sharer signs with its MLS leaf
key:

```text
"lakko/call/screen-share/v1\0" (27) || version(1) = 0x01 ||
call_id(16) || peer_id_len(1) || peer_id(1..=64) ||
seq(8, BE u64) || state(1: 0x01 sharing, 0x02 stopped) ||
flags(1: bit 0 = carries sound, other bits zero) ||
signer_key(32) || signature(64)
```

The signature is Ed25519 by `signer_key` over
`"lakko/call/screen-share/sig/v1\0" || call_id || peer_id_len || peer_id ||
seq || state || flags`. A one-to-one call has no SFU peer id, so there
`peer_id` is the fixed literal `p2p` and the MLS sender of the carrying
message is checked instead (section 4).

A receiver accepts a share state only when the call id is its own, the
signer key is a leaf of the call's group, the signature verifies, and in a
group call the peer id equals both the relay's stamped sender and the peer id
that signer announced (the existing peer announcement). Signing it, unlike
the camera flag, is deliberate: the camera flag only changes how a tile is
drawn, while a share state makes **another person's capture stop**, so the
SFU must not be able to re-attribute one participant's sealed state to
another by changing the relay's `from`.

### 2. The order: a Lamport sequence, ties broken by key

Each device keeps `max_seq`, the highest `seq` it has accepted or sent in
this call (0 at join). Starting a share sends `state = sharing` with
`seq = max_seq + 1`. Stopping sends `state = stopped` with the **same** `seq`
as the share it stops.

Each device folds every accepted state into **the current share**, by one
rule:

- a share is identified by `(seq, signer_key)`;
- a `stopped` for `(seq, signer_key)` ends that share for good, whichever
  arrives first (a late `sharing` for an id already stopped is ignored);
- among the live shares the current one is the greatest by `seq`, then by
  `signer_key` compared bytewise; every other live share is **superseded**.

A device whose own share is superseded stops its capture and sends
`stopped` for it; that is the "your share stopped" line of the UX spec.
Because the fold depends only on the set of states received, two devices
that received the same states agree on the current share whatever the
order; two people starting at the same moment both send `seq = n + 1`, both
compute the same winner, and the loser stops. A start made after hearing
another share has a higher `seq` and wins, which is the "newer wins" rule
for every case that is not a tie. Pinned in code as **C-87**.

A participant that leaves the call ends its share as if it had sent
`stopped`. A device that joins learns the live share because the sharer
re-sends its current `sharing` state to every peer that joins, as the camera
media state is re-sent today.

The fold and the composer live once in `lakko-calls` (`screen_share`
module: `compose_screen_share`, `verify_screen_share`, a `ScreenShareState`
that takes accepted states and answers the current share and whether this
device's own share is superseded), on UniFFI and wasm, so the three clients
cannot drift.

### 3. Group calls: two more tracks at the SFU

- The share picture is an ordinary video sender sealed under **stream index
  2**; its sound, when there is any, an ordinary audio sender under the new
  **stream index 5**. Both are armed with SFrame before the offer that adds
  them, and a share whose sender cannot be armed is not sent (invariant #4).
- `ClientFrame::SdpOffer` gains `screen_mids`: the mids of the share's
  video and audio lines, bounded like `light_mids`. The SFU forwards them
  under the stream id **`<peer>~screen`**, role `TrackRole::Screen`, to
  every subscriber, always; a screen is not paused by `video-wants` and has
  no light or middle variant in this version.
- When downlink control is on (ADR-0070 step 6) a screen is allocated
  before any camera: a subscriber's link is first given the screen, then
  cameras in their present order.
- The publish budget (ADR-0068) gains `screen_kbps`: while a share is live
  the SFU reserves room for it from the publisher's estimate before the
  camera ceilings are computed. The constants are to be chosen during the
  build and stated there as chosen.
- The sender sets `contentHint = "detail"` and prefers keeping resolution
  over frame rate; the frame-rate and size ceilings are chosen in the build.
- A key frame request for a screen track goes through the same gate as a
  camera's.

The SFU learns that a peer sends a screen and its bitrate, as it learns that
a peer sends a camera; it reads no payload (C-39 unchanged).

### 4. One-to-one calls: the existing signalling, one more body

`CallSignalingPayload` gains `ScreenShareState screen_share = 17` carrying
the signed state of section 1 as bytes. It rides the conversation as every
call signal does, so the MLS sender is authenticated; a receiver also checks
that the signer key is that sender's leaf. The share picture and sound are
further tracks on the existing connection, added in a `MediaStream` whose id
ends in `~screen`, through the renegotiation the camera already uses. They
are sealed or not exactly as the camera is on that path.

The NSE classifier (`lakko-ffi-extension`) reports the new body as `Other`
and the iOS byte peek gains its tag, pinned by the existing golden test, so
a share signal never rings or previews as chat.

### 5. Receiving on iOS and Android

Neither client captures a screen. Both:

- treat a remote track whose stream id ends in `~screen` as the share of
  the person that stream names, render it in a share tile (uncropped,
  zoomable) and play its sound with the call;
- decrypt it with the decryptor they already arm on every receiver, which
  reads the KID off the frame and so needs no change for stream indices 2
  and 5;
- accept and fold share states through the shared `ScreenShareState`;
- rank the share on the stage through `lakko_ui_rules::call_stage`, which
  gains a "sharing" input ahead of the speaker (`group-call-grid.md`
  section 5.1).

### 6. The web and the desktop client

The web client sends through `getDisplayMedia({ video, audio: true })`,
shows the button only where the API exists and the primary pointer is fine
(UX spec section 1), and treats the capture track's `ended` as a stop. Where
the browser lets the page leave its own sound out of a system capture, it
asks for that; which browsers do is measured in the build, not assumed.
`Permissions-Policy` becomes `display-capture=(self)` on the production and
test-stack web configurations. The desktop client, when it exists, uses this
code if its webview offers screen capture; otherwise it needs a native
capture path, decided then.

**The sender's own tile (amendment, 2026-11-21, maintainer).** The person
sharing sees the live picture of their share, and no picture only when the
share contains the call itself (UX spec section 3 and 3.1). No browser says
which window or screen it captured, so the device decides from what each
engine reports (`apps/web/src/calls/screen-share-mirror.ts`, pure, judged at
the start, every 2 s and on a resize): a tab is a mirror only when Chromium's
Capture Handle names this page; a window when the captured picture has this
window's size; a screen when there is one screen or the picture has this
screen's size; anything else shows the picture. The desktop app's host adds
the screen count and the sizes in device pixels (`GET /__host/app/surfaces`).
What each engine allows, and how much of it was measured:

| Engine | `displaySurface` | Capture Handle | `screen.isExtended` | Which window or screen |
|---|---|---|---|---|
| Chromium (Chrome, Edge) | yes | yes | yes | not reported |
| Firefox | not measured | no (not implemented) | no | not reported |
| Safari | not measured | no | no | not reported |
| WKWebView (desktop app, macOS) | not measured | no | no; the host reports the screen count | not reported to the page; the host knows its own window, which does not help without the captured one |

Measured 2026-11-21 in the Chromium 156 browser pane of the Claude app:
`displaySurface`, `restrictOwnAudio` and `CaptureController` present,
`setCaptureHandleConfig` accepted, `screen.isExtended` true on a Mac with two
screens; `getDisplayMedia` itself was refused there, so no captured track was
read. Nothing was measured in Firefox, Safari or the desktop app: the system
pickers need a person's click, and the share was not walked. The
"not reported" column is from the engines' public APIs: no engine exposes a
captured window's or screen's identity to the page (`CaptureController`
steers focus and zoom, it does not identify). The size rule therefore errs
both ways in stated cases: another window or screen of exactly this window's
or screen's size reads as a mirror (the tile then shows the line, as before);
a capture the engine scaled reads as no mirror (the tile shows a small
repeating picture).

## Alternatives considered

- **Last start heard wins.** Simple and wrong: two devices can each hear the
  other's start last, so both stop or both keep sharing, differently on
  every screen.
- **The SFU decides who shares.** The SFU orders the relay and could name a
  winner, but it would then hold a decision about who speaks to the group,
  and the one-to-one path has no SFU. Rejected.
- **Wall-clock timestamps.** Ordering by the sender's clock lets a device
  with a fast clock win every tie and lets anyone win by lying about time.
- **Unsigned share state, like the camera flag.** The relay seal stops
  outsiders, but the SFU can re-stamp the sender of a sealed blob; with a
  share that makes another person's capture stop, attribution matters.
- **The share as a layer of the camera, or as the camera's track replaced.**
  Loses the camera while sharing and needs a role switch everywhere; a share
  of its own is what the UX spec asks (`Anna's screen` beside Anna).
- **Several shares at once.** Declined by the maintainer (UX spec).

## Consequences

- The relay and the call signalling gain one signed format; G-39's surface
  grows by its decoder, and a fuzz target `screen_share_decode` covers it.
- New wire fields: `SdpOffer.screen_mids`, `PublishBudget.screen_kbps`,
  `CallSignalingPayload.screen_share = 17`. No deployment exists, so no
  compatibility path.
- Every SFrame stack accepts stream indices 2 and 5 on the send side; the
  receive side already does.
- **Invariants.** #2: the SFU sees one more opaque track per sharer. #3:
  untouched (no `lakko-mls` change; the leaf key signs, as for the peer
  announcement). #4: a share is never sent unsealed in a group call. #5: no
  server state about who shared. New invariant-index entry **C-87**: every
  device in a call that received the same share states names the same
  current share; a device whose share is superseded stops it.
- A `lakko-security` pass is due before merge (a new signed format that
  makes a remote device act).

## Open questions

1. **Re-announce cadence.** The sharer re-sends on peer join. Should it also
   re-send on every relay-key rotation, so a member who missed the join-time
   copy recovers within an epoch?
2. **A light variant of the screen.** A phone on a poor link receives the
   full screen. A quarter-size companion, as the camera has, would help and
   costs a second sender; left out of this version.
