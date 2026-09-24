<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Group call grid — UX spec

Written 2026-09-24. Scope: web, iOS, Android. Extends [`calls.md`](calls.md)
section 4.4 ("A group call: the big tile and the grid") and replaces it where the
two disagree; every such place is listed in section 11 with the reason. Everything
else in `calls.md` — the status line, the lock chip, the control row and its
sizes, the collapsed bar, the safety number — holds for a group call unchanged and
is not repeated here.

**How to read this.** Like [`navigation.md`](navigation.md), every requirement is
for all three clients. Where a platform has to differ it says so inline, marked
**Per platform**. What is built, on which client, belongs in
[`PRODUCT-READINESS.md`](../PRODUCT-READINESS.md), not here; section 2 describes
today only so the gap is visible.

Numbers in this document (thresholds, timings, tiles per page) are **chosen, not
measured**. They are a starting point to tune on a live call, and each is marked
where it appears.

## 1. Principles

- **A face does not move because someone coughed.** Tiles keep their place. A
  person moves only when there is a reason a watcher would agree with, and never
  more than one move every few seconds.
- **Every tile is legible without scrolling.** The grid fits the space between
  the status line and the control bar. More people than fit means pages, never a
  scroll view that hides the last row under the bar.
- **A tile never lies about the person.** A camera the person turned off, a
  picture this device stopped asking for, and a picture that stopped arriving are
  three different facts and look like three different things.
- **Who is speaking is worked out on this device.** It is computed from the audio
  after it has been decrypted, so the server never learns it. The UI must not
  imply otherwise, and the protocol must not leak it (section 10).
- **Never pretend** (`calls.md` section 1): no quality dot that was not measured,
  no speaking ring that was not heard.

## 2. What exists today

Read from `apps/web/src/routes/Call.tsx` + `calls/call-screen-model.ts`,
`apps/ios/Lakko/UI/Call/CallScreen.swift`,
`apps/android/app/src/main/kotlin/me/lakko/app/ui/call/CallScreen.kt`.

- **One shared rule, three different grids.** All three pick the large tile with
  the same rule (`featuredParticipant`: pinned, else speaking with video, else
  first with video, else nobody) and put everyone else in a gallery in roster
  order. The gallery itself differs: web is a CSS grid of `minmax(180px, 1fr)`
  columns (`300px` when there is no large tile), iOS a `LazyVGrid` of adaptive
  160 pt columns, Android a fixed two-column `chunked(2)` layout. The same call on
  the same width shows different column counts on each client.
- **It scrolls.** All three put the grid in a vertical scroll area. There is no
  page, no count, and at 8+ people most tiles are below the fold with nothing
  saying so.
- **Tile shape is fixed**: 16:9 for the large tile, 4:3 for the rest, on every
  width. On a phone in portrait that wastes most of the screen for 3–4 people.
- **`speaking` is never set.** Every production path initialises it to `false`
  and nothing updates it; only the design mocks set it. The ring exists in CSS
  (`.lk-call-tile.speaking`, a 2 px accent outline) and on both natives, but no
  real call has ever drawn it. The featured-tile rule therefore always reduces to
  "first with video".
- **The remote quality dot is not a measurement.** Every remote participant is
  created with `quality: "good"` and nothing changes it; only this device's own
  tier comes from `getStats`. A green dot on someone else's tile is exactly the
  invented signal `calls.md` section 1 forbids.
- **Tap on a tile pins it** (web `onClick`, iOS and Android tap gesture). On a
  touch screen that is also the gesture people use to bring back controls.
- **A muted remote gets a 44 px mic-off disc in the centre of the tile** (web),
  over the face, in addition to the glyph in the name strip.
- **No group reconnect.** `calls.md` section 4.3: the transport does not resume a
  session, so a lost connection ends the call.
- **No screen share.** Nothing sends one (`getDisplayMedia` appears nowhere);
  `screenSharing` exists on the roster model and is always false.
- **No video pausing.** Every remote picture is received whether or not its tile
  is on screen.

## 3. The two layouts

A group call has two layouts, and the person can switch between them:

- **Grid** — everyone the same size, filling the space.
- **Focus** — one large tile (the *stage*) and the rest in a single row or column
  of small tiles (the *strip*).

| Situation | Default layout |
|---|---|
| 3 or more people, nobody sharing a screen, nothing pinned | Grid |
| Someone starts sharing a screen | Focus, the screen on the stage (section 6.2) |
| The person pins a tile | Focus, the pinned tile on the stage |
| The person chose a layout by hand | That layout, for the rest of this call |
| Focus, but nothing on the stage has a picture and nothing is pinned | Drawn as Grid (`calls.md` 4.4: the large tile shows a picture, or there is no large tile) |

A call with one other person is not a group layout at all: it is the 1x1 portrait
or video fill of `calls.md` section 4, unchanged. The group screen starts at three
people **including this device**. A group call that drops to two keeps the group
screen (the kind of a call does not change mid-call, `call-screen-model.ts`
`groupCall`), laid out as Grid with two tiles.

**Switching.** **Per platform:** on a phone the switch is an item in the group
overflow menu (`calls.md` 4.1 already puts participants and chat there): `Grid`
/ `Focus`, with a check on the current one. On the web and on a tablet it is a
small icon button at the top right, beside the quality dot, labelled
`Change layout`. A manual choice is remembered for the call only; the next call
starts from the defaults.

## 4. Grid layout

### 4.1 Form factors

The form factor is decided by the width of the **call area**, not the device
model, so a narrow browser window behaves like a phone and a split-screen iPad
like whatever width it has.

| Name | Call-area width | Examples |
|---|---|---|
| Compact | < 600 pt | Phone portrait, narrow window |
| Compact-wide | < 600 pt tall **and** wider than tall | Phone landscape |
| Regular | 600–1023 pt | Tablet, half a laptop screen |
| Wide | ≥ 1024 pt | Laptop / desktop browser, iPad landscape full screen |

### 4.2 Columns and rows by count

The grid fills the **call area**: the space below the status line and above the
control bar (`calls.md` 4.4 already reserves both). "Page cap" is the most tiles
drawn at once; more than that and the grid pages (section 4.5).

| People (incl. you) | Compact | Compact-wide | Regular | Wide |
|---|---|---|---|---|
| 3 | 1 on top, full width; 2 side by side below | 3 in a row | 3 in a row (portrait: 1 + 2) | 3 in a row |
| 4 | 2 × 2 | 2 × 2 | 2 × 2 | 2 × 2 |
| 5–6 | 2 cols × 3 rows (5: last row centred) | 3 × 2 | 3 × 2 | 3 × 2 |
| 7–9 | page 1: 2 × 3, page 2: the rest | page 1: 3 × 2, page 2: the rest | 3 × 3 | 3 × 3 |
| 10–16 | 2 × 3 per page | 3 × 2 per page | 3 × 3 per page | 4 × 4 (10–12: 4 × 3) |
| 17+ | 2 × 3 per page | 3 × 2 per page | 3 × 3 per page | 4 × 4 per page |
| **Page cap** | **6** | **6** | **9** | **16** |

The page caps are chosen, not measured. The reasoning: on a 402 pt phone in
portrait a 2 × 3 grid gives tiles about 185 × 200 pt, big enough to read a face;
3 × 3 gives about 125 × 140 pt, where a face is a smudge and the name strip covers
a third of it. The tile minimum below is what enforces this on widths nobody
listed.

**Minimum tile width**: 150 pt on Compact, 200 pt on Regular, 240 pt on Wide. If
the column count in the table would go below it (a very narrow window, a large
text size), the grid drops a column and the page cap drops with it.

**A last row that is not full is centred**, never left-aligned with a hole on the
right. The tiles in it keep the same size as the rows above.

### 4.3 Tile shape

- The tile shape comes from the space, not a fixed ratio: the call area divided
  by the rows and columns above, **clamped between 3:4 (tall) and 16:9 (wide)**.
  So a 2 × 2 grid on a phone gives slightly tall tiles, the same grid on a laptop
  gives wide ones, and the grid always fills its area to within the clamp.
- **Camera video is cropped to fill** (`cover`), keeping the centre. A face is
  what matters and it is usually in the middle.
- **A screen share is never cropped** (`contain`), on a black ground, and never
  mirrored (section 6.2). Cropping a document is losing the document.
- Gaps: 8 pt on Compact, 10 pt on Regular and Wide (the web grid uses 10 px
  today). Corners: the medium radius (`--radius-md`, 12). Tiles do not touch the
  edge of the screen: 12 pt side margin on Compact, 16 pt elsewhere.

### 4.4 Joining and leaving

**Nothing reorders.** When everyone fits on one page, the order is the order
people joined and it never changes for the whole call — the rule `calls.md` 4.4
already had.

- **Someone joins**: their tile appears at the end of the order, just before your
  own (section 5.3). If the new count changes the grid shape (3 → 4 becomes
  2 × 2), every tile moves to its new cell with a short slide (200 ms, ease out)
  and the new tile fades in at its cell. Nobody's relative order changes.
- **Someone leaves**: their tile shows the *left* state for 2 seconds (section
  7), then fades out, and the tiles after it slide up to close the gap.
- **Reduced motion**: no slide. Tiles cross-fade to their new cells in 150 ms.
- Several joins or leaves within one second are applied as one layout change, so
  ten people arriving at the start of a meeting is one reflow, not ten.

### 4.5 Pages

- Beyond the page cap the grid pages. **Page 1 is special**: it holds the people
  chosen by the ordering rule of section 5 and always holds your own tile. Pages
  2 and on hold everyone else in join order.
- **The page indicator** sits centred between the grid and the control bar: one
  dot per page, the current one filled. More than 5 pages and the dots become a
  count, `3 / 9`. It is also a control: on the web each dot is a button; on the
  natives tapping left or right of the current dot moves one page.
- **Per platform — changing pages**: a horizontal swipe on iOS and Android, with
  the page following the finger. On the web: arrow buttons appear at the left and
  right edges of the grid on hover or focus, and `Page Up` / `Page Down` work while
  the call has focus.
- Leaving a page and coming back finds it as it was, except where section 5 moved
  a speaker onto page 1. A page never goes empty under the person: if the last
  person on the last page leaves while it is showing, the view slides back one
  page.
- **Only the page on screen receives video** (section 7.4). Moving to a page
  shows each tile's paused state until its picture arrives.

## 5. Who gets the visible slots

This matters only when there are more people than tiles on page 1 (Grid), or
more than the strip shows (Focus). When everyone is visible, section 4.4 holds and
nothing moves.

### 5.1 Priority

Page 1 (Grid) and the stage then the front of the strip (Focus) are filled in this
order:

1. The **pinned** tile (Focus only — a pin always means Focus).
2. A **screen share**.
3. The **active speaker** (5.2).
4. **Recent speakers**: people who were the active speaker in the last 60 seconds,
   most recent first. (Chosen, not measured.)
5. People with their **camera on**.
6. Everyone else, in **join order**.

Your own tile is never ranked: it has a fixed place (5.3).

### 5.2 Stability

A slot is only given away under these rules. All of them are chosen, not
measured, and all of them are one shared rule (section 9), so the three clients
move the same person at the same moment.

- **Speaking onset.** A person becomes the active speaker after **1.5 seconds**
  of continuous speech. A cough, a laugh, a "mm" never qualifies.
- **Already visible means no move.** If the new active speaker is already on
  screen, nothing moves; their tile only shows the speaking ring.
- **One swap, not a shuffle.** Someone coming onto page 1 takes the slot of the
  **lowest-priority visible person** by the list in 5.1, who goes to where the
  newcomer was. Nobody else moves.
- **Cooldown.** A tile that has just arrived stays at least **10 seconds**.
  Across the whole grid there is **at most one swap every 3 seconds**.
- **A swap is a cross-fade in place** (200 ms; reduced motion: instant), not a
  slide across the screen. The eye sees one face become another in the same
  frame, not the grid rearranging.
- **The stage in Focus** follows the active speaker with the same onset and a
  **3-second hold**: it does not leave a speaker until someone else has been the
  active speaker for 3 seconds. The stage only ever holds someone sending a
  picture, as `calls.md` 4.4 requires; a speaker without a camera is promoted into
  the strip instead.
- **Pinned or sharing overrides all of this** and does not move until unpinned
  or stopped.

### 5.3 Your own tile

- **Grid**: always on page 1, **always the last cell** (bottom right; on a
  centred last row, the right end of it). A fixed place is how a person finds
  themselves without looking. It is labelled with your profile name, `You` only
  when no name is set (`calls.md` 4.4).
- **Focus**: the last tile of the strip.
- **Alone in the call**: your tile fills the call area, as `calls.md` 4.4 says.
- It is never on the stage while anyone else is present, and it cannot be pinned.
- Your camera picture is mirrored; everyone else's never is.
- Your tile shows your own speaking ring too: it is the quickest check that your
  microphone works. **When you speak while muted**, the notice line under the
  status line says `You're muted` for 3 seconds, at most once a minute. This
  needs only the local microphone level and never leaves the device.

## 6. Focus layout, pinning and screen share

### 6.1 Focus

| Form factor | Stage | Strip |
|---|---|---|
| Compact | Full width, top, ≤ 60 % of the call area height | One row under it, horizontal scroll, tiles 110 × 110 pt |
| Compact-wide | Left, full height | One column on the right, vertical scroll, tiles 16:9, 120 pt wide |
| Regular | Top, full width | One row under it, tiles 16:9, 160 pt wide |
| Wide | Left | One column on the right, 220 px wide, tiles 16:9 |

The strip scrolls — it is the one place scrolling is allowed, because its tiles
are secondary and the stage never moves when it scrolls. Tiles scrolled out of the
strip are off screen for section 7.4.

### 6.2 Pin and unpin

- **Per platform — opening a tile's menu**: long press on iOS and Android; on the
  web a small `⋯` button that appears at the tile's top right on hover or focus,
  and right-click. **A plain tap on a tile does not pin.** On touch a tap shows the
  controls, the gesture people already use for that; the pin-on-tap of today
  (section 2) goes.
- The menu holds `Pin` (or `Unpin`). Pinning switches to Focus with that tile on
  the stage and shows a pin glyph on it. Double-tap (touch) or double-click (web)
  on a tile is a shortcut for the same.
- **One pin at a time.** Pinning someone else moves the pin.
- **Unpin**: the same menu, or the `Unpin` chip at the top left of the stage.
  After unpinning, the layout goes back to what it was before the pin.
- A pin is local. Nobody else learns that you pinned them.
- If the pinned person leaves, the pin is cleared and the layout returns to what
  it was.

### 6.3 Screen share (receiving)

Sending a screen is still out of scope (section 12). What a receiver sees is
designed here so the grid does not have to change again when sending lands.

- A shared screen is **its own tile**, separate from the person's camera tile.
  The person stays in the grid or strip with their camera; the share is labelled
  `Anna's screen` and carries a screen glyph instead of a mic glyph.
- **A new share goes to the stage**, switching the layout to Focus — unless the
  person has pinned someone. A pin is an explicit choice and wins; in that case a
  chip on the stage says `Anna is sharing their screen` with a `View` button.
- When the share stops, the layout goes back to what it was before it started.
- Two shares at once: the newest on the stage, the other first in the strip.
- On the stage a share can be zoomed (pinch on touch, `+` / `-` on web) and
  panned; the zoom resets when it leaves the stage.

## 7. Tile states

A tile has one **picture state** and any number of **badges**. The picture state
is what fills the tile; the badges sit in the name strip along the bottom.

### 7.1 Picture states

| State | What fills the tile | How it differs from its neighbours |
|---|---|---|
| **Video** | The person's picture | — |
| **Camera off** | The person's tone colour, initials in a circle centred (64 pt on Compact, 88 on Wide) | No video glyph anywhere. This is the only state that means "the person chose this" |
| **Video paused** | The last frame received, blurred and darkened; a small `pause` glyph in a video-camera outline at the top left | Never the initials circle. Section 7.4 |
| **Waiting for video** | The tone colour, a video glyph, a small spinner | A tile just scrolled or paged into view whose picture has not arrived yet |
| **Reconnecting** | The last frame (or the initials), darkened, a spinner and `Reconnecting...` centred | Section 8.2 |
| **Left** | The initials, faded to 40 %, and `Left` centred | 2 seconds, then the tile goes |
| **Screen** | The shared screen, uncropped, on black | Section 6.3 |

### 7.2 Badges (name strip)

The strip is a scrim along the bottom edge (`--call-scrim`, as today) with, in
this order: mic-off glyph · name · connection glyph · pin glyph. Anything that is
in its normal state is absent — the strip of a healthy, unmuted, unpinned person
is only their name.

- **Muted**: a mic-off glyph in the danger colour, at the start of the strip. The
  mic-off disc over the middle of the tile (web today) goes: it covers the face,
  and the strip already says it.
- **Name**: one line, truncated with an ellipsis. On tiles narrower than 150 pt
  the first name only.
- **Connection**: shown only for **Fair** or worse, as a small signal glyph with
  one, two or three bars in the warning (fair) or danger (poor, bad) colour. A
  good connection shows nothing: a dot on every tile says nothing and costs
  attention. And it is shown **only when measured** (section 10); until each
  client measures remote connections the glyph is absent, never a constant.
- **Pinned**: a pin glyph at the end.

The connection glyph describes what **this device** receives from that person —
the only thing it can measure. So its spoken and tooltip text is
`Anna's video and audio are arriving poorly`, not a claim about Anna's network.

### 7.3 Speaking

- **The ring**: a 3 pt accent-coloured border inside the tile edge (so the tile
  does not change size), with a soft outer glow. On a camera-off tile the initials
  circle also gets a ring that swells with the voice level in **three steps**, not
  continuously, so it reads as a pulse rather than a flicker.
- **The glyph**: three small bars beside the name in the strip, their heights
  following the level in the same three steps. The glyph is what makes speaking
  readable without colour; the ring is what makes it findable at a glance.
- **Timing**: on after 200 ms of speech, off after 800 ms of silence (chosen, not
  measured). This smoothing is separate from the 1.5-second promotion onset of
  5.2: the ring may flicker on for a cough, a tile never moves for one.
- **Latency is expected**: the level is computed from audio that has already been
  received, decrypted and decoded, so the ring trails the voice by a few hundred
  milliseconds. That is acceptable and must not be "fixed" by using a level the
  server supplies (section 10).
- **Reduced motion**: the ring is on or off, never pulsing, and the bars are
  static at full height while speaking.
- More than one person speaking: every one of them gets the ring. Only the
  promotion rule picks one.

### 7.4 Video paused, and why it must not look like camera off

Once video pausing exists (section 10, the receiver asking the call server not to
send a picture it is not showing), a tile can be without a picture for a reason
that has nothing to do with the person:

- **Off screen** — on another page, or scrolled out of the strip. Nobody sees
  these tiles, so their look matters only at the moment they come back: they show
  *waiting for video* until the first frame arrives, then cross-fade to it.
- **Paused to save bandwidth** — this device or the server decided the connection
  cannot carry every picture. The tile shows *video paused*: the last frame,
  blurred, with the pause glyph. Long press / hover explains it:
  `Video paused to keep the call smooth. It will come back when the connection
  improves.`

The camera-off look (initials, no video glyph) is reserved for a person who
turned their camera off, which this device knows from that person's own camera
signal. A person whose picture this device stopped receiving has **not** turned
their camera off, and showing initials would tell the room something false about
them.

Two more cases fall into *video paused* rather than camera off:

- The camera signal says on, but no frames have arrived for 3 seconds (chosen,
  not measured) while the person is still in the call.
- This device's own connection is **Poor**: it may pause every tile except the
  stage and the active speaker. The notice line then says
  `Some videos are paused to keep the call smooth.`

## 8. Call-level connection states

`calls.md` 4.3 ends a group call on any lost connection, because the transport
cannot resume a session. This section is the target once it can; until then the
`calls.md` behaviour stands.

### 8.1 This device

| State | Status line | Notice line (under it) | Tiles | Controls |
|---|---|---|---|---|
| **Reconnecting** — the connection dropped and the device is resuming the same session | `Reconnecting...` | `Your connection dropped. Getting you back.` | Keep their last picture and state, darkened slightly; no promotions, no swaps | All usable; a mute or camera change is applied when the connection is back |
| **Rejoining** — resuming failed and the device joins the call afresh | `Rejoining the call...` | `Joining the call again. This can take a few seconds.` | Cleared to *waiting for video*, then refilled as people are re-learnt, in their old order where they are the same people | All usable |
| **Failed** | `Couldn't reconnect` | — | Frozen | Replaced by two buttons: **Rejoin** and **Leave** |

Timing (chosen, not measured): up to 10 seconds of *Reconnecting*, then up to 20
seconds of *Rejoining*, then *Failed*. The running time keeps counting through
both, because the call did not end.

*Reconnecting* and *Rejoining* are different words because they promise
different things: the first keeps everything, the second starts over and may
briefly show empty tiles. Using one word for both would make the second look
like a bug in the first.

Rejoining is a new admission to the call. The encryption chip goes away while it
runs and comes back only when the call's encryption is armed again (`calls.md`
section 5): the chip never stays up across a rejoin on the strength of the old
session.

### 8.2 Someone else

A remote person whose connection dropped but who has not yet been removed by the
server shows the *reconnecting* picture state on their tile, for as long as the
server says they are within their grace period. When the grace ends their tile
goes through *left*. Their name is announced politely as `Anna is reconnecting`.
This state must come from the server (section 10); a receiver guessing it from
missing frames would show `Reconnecting...` for someone who simply turned their
camera off with a bad signal, which is the confusion 7.4 exists to prevent.

## 9. Edge cases

- **Only you**: your tile fills the call area (`calls.md` 4.4). No layout switch,
  no page dots. The status line counts 1.
- **Everyone camera off**: the same grid with initials. Speaking is then the only
  motion on screen, which is what the ring is for. Focus has nothing to put on the
  stage and is drawn as Grid.
- **50 people**: Compact shows 6 per page over 9 pages (`1 / 9`); Wide 16 per page
  over 4 pages. Page 1 is what 5.1 says, so the people talking are on it. Only one
  page of pictures is received. The participants list (overflow menu) gains a
  search field once the call has more than 16 people, and a person in the list
  has `Show` (jump to their page) in their row.
- **Unresolved name**: the tile says `Participant` and shows a generic person
  glyph instead of initials (a `P` would look like someone called Pat). Two
  unresolved people are told apart by tone colour. They are **not numbered**:
  `Participant 3` becomes wrong the moment someone before them leaves.
- **Long names**: truncated in the strip; the full name is in the spoken label and
  the tile menu's title.
- **Text size**: at the largest accessibility text sizes the strip grows to two
  lines at most and the minimum tile width of 4.2 drops a column. The strip never
  covers more than a third of the tile.
- **Rotation**: the grid re-lays in place with the 4.4 slide. The page shown
  stays the same page.

## 10. Data and platform work this needs

| Need | Why |
|---|---|
| A **local speaking level** per remote participant, computed on the receiving device from decoded (post-decryption) audio, at ≥ 10 samples a second | Sections 5 and 7.3. Candidates: the inbound audio `audioLevel` in `getStats`, or an audio analyser on the decoded track; either is local |
| The **same level for this device's own microphone** | 5.3: your own ring, and `You're muted` |
| **No audio-level RTP header extension** (RFC 6464) negotiated in either direction, and a check that keeps it out | It carries the speaking level in clear to the call server, which would hand the server exactly what section 1 says it never learns |
| A **per-participant receive quality** from inbound stats (loss, jitter, freeze counts per person) | 7.2. Until it exists the remote connection glyph is absent — and the constant `good` of today is removed |
| A **server-announced "peer reconnecting"** with the grace period | 8.2 |
| A **session resume** for this device, and a clean re-admission path | 8.1. Today a lost connection ends the call (`calls.md` 4.3) |
| **Video pause / resume per track**, sent by the receiver to the call server, plus a preferred simulcast layer by tile size | 4.5, 6.1, 7.4. The server already selects simulcast layers by bandwidth; asking per track by visibility does not exist |
| A way for the receiver to tell **"not sent to me"** from **"camera off"** | 7.4. Camera off is already signalled by the person themself (the call media-state signal); "not sent" has to come from the server or from this device's own pause request |
| **Join order** as a stable roster property | 4.4, 5.1 |
| **Which track is a screen** and whose it is | 6.3. The per-stream index already separates a screen from a camera in the media key (ADR-0035); the roster must expose it |
| **One shared layout-and-slot rule**, in `lakko-ui-rules` beside the existing call rules | Section 5 is exactly the kind of rule that drifts when written three times. Input: the roster with speaking, camera, share, pin and join order, the call-area size, the clock; output: layout, page cap, cells and the slot assignment with the hysteresis state |

## 11. Changes to `calls.md`

| `calls.md` says | This spec says | Why |
|---|---|---|
| 4.4: whenever someone else sends video, a large tile shows them above the grid | Grid is the default; the large tile exists in Focus, which comes from a pin, a screen share or the person's choice | Now that the grid fills the screen, every camera is already large. An automatic large tile that follows "first with video" (the rule's real behaviour, since `speaking` is never set) takes half the screen for one person and shrinks the rest for no reason the room agreed on. The rule itself survives unchanged inside Focus |
| 4.4: the grid keeps roster order and never reorders | Unchanged while everyone fits. With pages, the section 5 rules may bring a speaker onto page 1 by swapping one tile | Someone on page 4 talking for five minutes with nobody able to see them is worse than one cross-fade |
| 4.4: `Peer` is the last-resort name | `Participant`, with a person glyph instead of initials | `Participant` is what the clients already render, and `Peer` is a protocol word the design system's copy rules keep off the screen |
| 4.3: no `Reconnecting...` on a group call | Reconnecting and Rejoining exist, once the transport can resume (section 8) | Section 8 is a target; `calls.md` 4.3 stays true until the transport changes |
| 9: screen sharing out of scope | Receiving a share is designed here; sending stays out | The grid had to decide where a share goes; deciding it later means moving tiles again |
| (Built behaviour, not text) a tap on a tile pins it | A tap shows the controls; pinning is in the tile's menu or a double-tap | On touch, a tap on a video surface already means "show me the controls" everywhere else |

`calls.md` section 4.4 will point to this document for the grid; that edit is
not made in this change.

## 12. Accessibility

- **Each tile is one element** with a composed label, in this order and with
  only the parts that apply: name, `speaking`, `muted`, `camera off` or
  `video paused`, connection (`poor connection` / `weak connection`),
  `pinned`, `sharing screen`. For example: `Anna, speaking, camera off, poor
  connection`. Your own tile starts with `You,`.
- The tile's actions (Pin / Unpin) are exposed as accessibility actions (iOS
  custom actions, Android `customActions`, web menu button), so no gesture is the
  only way in.
- **Speaker changes are not announced**: in a busy call that is a constant
  stream. The label updates, so moving to a tile reads the current state. Joins,
  leaves, reconnects and a new screen share are announced politely, no more than
  one announcement every 2 seconds.
- **Not colour alone**: speaking has the bars glyph as well as the ring; muted,
  connection and pause each have a glyph; *left* has a word.
- **Reduced motion** (all three platforms' system setting): no slides, no pulse,
  cross-fades only (4.4, 5.2, 7.3).
- **Touch targets**: tiles are targets in their own right; the web `⋯` button and
  page-dot buttons are at least 44 × 44 even when drawn smaller. Page dots on the
  natives are one 44 pt high hit area split left / right.
- **Focus order** (keyboard, switch control): status line, layout switch, tiles
  in reading order of the current page, page indicator, control bar.

## 13. Cross-platform shape

Identical on all three: the two layouts and when each is the default, the table
of 4.2, the page caps, tile shape clamping, the ordering and stability rules of
section 5 (one shared rule), every tile state and badge and its words, section 8's
states and words, and the spoken labels.

Platform idiom, and only this:

| | iOS | Android | Web |
|---|---|---|---|
| Tile menu | Long press (context menu) | Long press (dropdown) | `⋯` on hover / focus, right-click |
| Change page | Swipe | Swipe | Edge arrows, `Page Up` / `Page Down`, dot buttons |
| Layout switch | Overflow menu (Compact) / top-right button (Regular) | Same | Top-right button |
| Pin feedback | Light haptic | Light haptic | None |

## 14. Out of scope

Sending a screen share; a floating picture-in-picture window; muting someone
else for everyone (a group-admin action, not a layout one); reactions and raised
hands on tiles; captions; audio rooms with hundreds of listeners (a different
product shape, ADR-0010 territory); virtual backgrounds.

## 15. Open questions for the maintainer

1. **Phone page cap: 6 or 9?** 6 keeps faces readable; 9 shows more people and
   is what some messengers use. The spec says 6.
2. **An audio-only density.** When nobody sends video, circles in a 3 × 4 grid
   would fit 12 people on a phone page. It makes the layout jump when the first
   camera turns on. Worth it?
3. **Hide self-view.** Should the person be able to take their own tile out of
   the grid (it is still sent)? Many people ask for it on long calls.
4. **Share versus pin.** The spec lets a pin win over a new screen share, with a
   `View` chip. The opposite (the share always takes the stage) is simpler and is
   what the brief suggested.
5. **Maximum call size.** Is there a product cap on participants? The 50-person
   case is designed, but nothing states what the call server is meant to carry.
6. **The stability numbers** (1.5 s onset, 10 s stay, 3 s between swaps, 3 s
   stage hold, 60 s recent-speaker memory) are guesses to tune on a real call.
   Who runs that tuning, and on which call size?
7. **`You're muted`** uses the microphone level while muted. It never leaves the
   device, but the microphone is being read while the person believes it is off.
   Acceptable, or opt-in?
8. **Remote quality meaning.** What this device can measure is how a person's
   media arrives here, which mixes their network and ours. Is that worth showing
   per tile, or should only this device's own connection be shown?
