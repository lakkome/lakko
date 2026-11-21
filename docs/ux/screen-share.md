<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Screen sharing — UX spec

Written 2026-10-25. Nothing sends a screen today: `getDisplayMedia` appears
nowhere, `screenSharing` on the roster model is always false, and both nginx
configurations send `Permissions-Policy: display-capture=()`, which forbids it.
[`group-call-grid.md`](group-call-grid.md) section 6.3 designed what a receiver
of a group call sees; this spec adds sending, the one-to-one call, and the rule
for more than one person wanting to share.

The maintainer's decisions, 2026-10-25:

- Sharing works in **one-to-one and group calls**.
- **Sending** is for the web client in a desktop browser and, when it exists,
  the desktop client. **iOS, Android and phone browsers never send.** Every
  client, phones included, **receives** and shows a share.
- The sound of the shared tab or system **is sent when the browser offers it**.
- **One share at a time in a call.** A new share stops the one before it.
- A **pin wins** over a new share; the share is offered by a chip
  (`group-call-grid.md` section 6.3, open question 4 there, now closed).

## 1. User goal and entry point

The person is in a call and wants the others to see a document, a window, a
slide or a browser tab, the way Zoom and Google Meet allow.

The only entry point is a button in the call row (`calls.md` section 4.1),
placed after Camera:

| Call kind | The row, left to right |
|---|---|
| One-to-one | Microphone · Camera · **Share screen** · Settings · End |
| Group | Microphone · Camera · **Share screen** · Participants · Settings · End |

- The button is **absent** where this device cannot send: iOS, Android, a
  browser without `navigator.mediaDevices.getDisplayMedia`, and a phone or
  tablet browser (a browser whose primary pointer is coarse and which has no
  hover). Absent, not disabled: a control the person can never use is noise.
- The one-line rule of `calls.md` section 4.1 still holds with the extra
  circle: under 600 pt wide the words go and the circles shrink, never below
  38 pt.
- Not on the pre-call surfaces (incoming call, ringing, the collapsed call
  bar). A share starts from inside a connected call only.

## 2. Starting a share

1. The person presses **Share screen**.
2. The browser shows **its own picker** (screen, window or tab, and the audio
   checkbox where the browser has one). This is the one system surface in the
   flow; the platform leaves no other way, and Lakko does not draw a picker of
   its own before it.
3. The person picks a source and confirms. The share starts at once; there is
   no second confirmation in Lakko.

Lakko asks the browser for audio together with the picture. Whether the
checkbox appears, and what it captures, is the browser's: as far as is known
before testing, Chrome and Edge offer the sound of a tab, and of the whole
system on Windows; Firefox and Safari offer none. Each browser is to be
checked before this is called working.

**The call's own sound must not go back out with the share.** Where the browser
can leave the page's own audio out of a system capture, Lakko asks it to.
Where it cannot, Lakko asks for tab audio only. Which browsers allow which is
not known yet and is part of the build.

### 2.1 When it does not start

| What happened | What the person sees |
|---|---|
| The person closed the picker or pressed Cancel | Nothing. The row is as it was. Never an error for a choice |
| The browser refused (site permission blocked, or the OS refused screen recording, e.g. macOS Screen Recording off for the browser) | One line above the row for 6 seconds: `Can't share your screen. Allow screen sharing for this site in your browser settings.` On macOS the line names the system setting instead: `Can't share your screen. Allow screen recording for your browser in System Settings.` |
| The secure media path for the share could not be armed | The share is not sent at all: `Can't share your screen securely right now. Try again.` No unencrypted fallback (invariant #4) |
| The call is not connected yet | The button is not there yet (section 1) |

## 3. While sharing (the person who shares)

- **The button** becomes the filled, loud state of the row's toggles and its
  spoken name becomes `Stop sharing`.
- **A strip across the top of the call screen**: `You're sharing your screen`
  and a `Stop` button. With tab or system sound: `You're sharing your screen
  and its sound`.
- **Their own tile for the share** (decided by the maintainer on 2026-11-21,
  closing open question 1) shows the live picture of what they share, small,
  in the strip (group) or next to the self-view (one-to-one), uncropped on
  black like every share, with no zoom controls and no sound. The one
  exception is the **mirror**: when the share contains the call itself (the
  tab, the window or the screen Lakko is in), the tile shows no picture, to
  avoid the endless mirror, and keeps the share tile's black ground with the
  screen glyph and one line saying what is shared:
  `You're sharing this tab`, `You're sharing this window` or
  `You're sharing this screen`. When the browser does not say what kind of
  surface is shared and the share is a mirror, the line is
  `You're sharing your screen`.

  Whether a share is a mirror is decided on the device from what the browser
  or the desktop app reports (section 3.1), checked again every 2 seconds
  and when the window changes size, so moving the call to another screen or
  switching the shared surface in the browser's own bar changes the tile.
  When nothing reported can tell, the tile shows the picture: a mirror shown
  by mistake is a small repeating picture, a picture hidden by mistake is the
  defect this replaced.
- **The camera and the microphone are unchanged.** A share is a separate
  picture; the camera tile goes on as it was.
- **Stopping**: the row button, the strip's `Stop`, or the browser's own
  "Stop sharing" bar, which Lakko treats exactly like its own Stop. Closing
  the shared window or tab ends the share the same way.
- **The collapsed call bar** (`calls.md` section 4.5) says `Sharing your
  screen` as its status line while the share is live; it keeps the bar's
  one-thing-at-a-time rule (the share outranks the duration, never the
  `Reconnecting...` line).
- **The connection drops and comes back**: the share is still running in the
  browser; it is re-sent when the call reconnects. If the browser ended the
  capture meanwhile, the share is over and the person is not asked again.

### 3.1 What each engine lets the device know

The rule, in order (pure, in `apps/web/src/calls/screen-share-mirror.ts`):

1. **The desktop app** asks its host how many screens there are and the
   sizes of its window and of the screen that window is on, in device pixels
   (`GET /__host/app/surfaces`); the webview's page cannot tell the first.
   The rules below then run on the host's figures.
2. **A tab** (`displaySurface` `browser`): a mirror only when the captured
   tab is this one, which Chromium reports through the Capture Handle this
   page sets (`setCaptureHandleConfig` / `getCaptureHandle()`). Lakko already
   asks Chromium to leave its own tab out of the picker
   (`selfBrowserSurface: "exclude"`), so this is the rare case of a browser
   that ignored the option.
3. **A window** (`window`): a mirror when the captured picture has the size of
   this browser window (within 2 pixels on each side, in CSS pixels, device
   pixels or the other way round), since
   the browser does not say which window it captured. Another window of
   exactly the same size reads as a mirror: the tile then shows the line,
   which is what it did before.
4. **A screen** (`monitor`): a mirror when the captured picture has the size
   of the screen this window is on in device pixels (within 2 pixels), or
   when the browser says there is only one screen (`screen.isExtended ===
   false`). Two screens of the same size read as a mirror for either.
5. **No surface reported**: the size checks of 3 and 4; neither matching
   means the picture is shown.

What was measured per engine on 2026-11-21 is recorded in
[ADR-0083](../adr/0083-screen-share.md) section 6.

## 4. One share at a time

A share started by someone stops the share that was running.

- **The person whose share was stopped** sees one line above the row for 6
  seconds: `Boris is sharing now, so your share stopped.` Their capture is
  ended, so the browser's own bar goes away too.
- **Everyone else** sees the new share take the place of the old one (section
  5); there is no line for them beyond the ordinary announcement.
- **Two people starting within the same moment**: exactly one share must
  survive, and every device in the call must agree which. The rule that picks
  it belongs to the protocol (section 9) and must not depend on the order in
  which each device happened to hear the two starts. The person whose share
  lost sees the line above.
- No one can stop someone else's share, and starting a share asks nobody's
  permission. Moderated sharing is out of scope (section 11).

## 5. Receiving a share

The picture is always shown whole, uncropped, on black, on every client.

### 5.1 One-to-one

- The share becomes the **large picture**. The other person's camera, if on,
  moves to a small tile in the corner opposite the self-view; if off, their
  initials circle takes that tile.
- A voice call becomes the video layout for both while a share is live, as
  the camera does (`calls.md` section 4.1), and returns to the portrait when
  it stops and nobody's camera is on.
- The share is labelled `Anna's screen`.

### 5.2 Group

As `group-call-grid.md` section 6.3, with the one-share rule:

- The share is its own tile, `Anna's screen`, with a screen glyph in the name
  strip instead of a microphone glyph. Anna's camera tile stays where it was.
- A new share goes to the **stage** and switches the layout to Focus, unless
  the viewer has pinned someone. Then a chip on the stage says `Anna is
  sharing their screen` with `View`; `View` puts the share on the stage and
  clears the pin.
- A share that replaces another takes its place on the stage without a
  layout change.
- When the share stops, the layout returns to what it was before the first
  share began.
- A share has one picture only: the call server sends it in full wherever it
  is drawn, stage or strip, and gives it the link before any camera
  (ADR-0083 section 3; a light variant is ADR-0083 open question 2).

### 5.3 Every receiver

- **Zoom and pan** on the stage or the one-to-one large picture: pinch on
  touch, `+` / `-` and drag on web; a double tap or `0` returns to whole. The
  zoom resets when the share leaves the large place.
- **A phone in portrait** shows the share letterboxed; turning the phone to
  landscape gives the share the whole width. The call screen allows landscape
  while a share is on the large place.
- **The share's sound** plays with the call; it has no separate control.
- **Waiting for the picture**: from the announcement until the first frame,
  the share tile shows the screen glyph, a spinner and `Waiting for Anna's
  screen`.
- **Picture stops while the share is on** (a stalled source, a minimised
  window): the last frame stays; nothing claims the share ended.
- **Announced, polite, once each**: `Anna started sharing their screen`,
  `Anna stopped sharing`.

## 6. The picture

A screen is read, not watched, so it favours sharpness over motion: the
source's own size up to a ceiling, a low frame rate, the encoder told the
content is detail. A shared tab playing a video will look choppy; a setting to
trade this the other way is not in this version (open question 2). The
ceiling and frame rate are to be chosen during the build and stated there as
chosen, not measured.

## 7. Encryption, stated honestly

- The share and its sound are sealed end to end like the camera, each with
  its own key space, before they leave the browser, in group and one-to-one
  calls alike. The call's lock and safety number are unchanged and need no
  new wording.
- If the share cannot be sealed, it is not sent (section 2.1).

## 8. States

| Who | State | What shows |
|---|---|---|
| Sender | Can share | `Share screen` button |
| Sender | Picker open | The browser's picker; the row unchanged behind it |
| Sender | Sharing | Filled button, top strip, own share tile with the live picture, bar status |
| Sender | Sharing the tab, window or screen Lakko is in | As above, the own tile shows no picture and says `You're sharing this tab` / `window` / `screen` |
| Sender | Sharing with sound | As above, strip names the sound |
| Sender | Stopped by a newer share | Line `Boris is sharing now, so your share stopped.` |
| Sender | Refused / could not arm | Line from section 2.1 |
| Receiver | Share announced, no picture yet | `Waiting for Anna's screen` |
| Receiver | Share on the large place | The share, uncropped, zoomable |
| Receiver | Share offered while a pin holds | Chip `Anna is sharing their screen` · `View` |
| Receiver | Share in the strip | Small share tile, light picture |
| Receiver | Share replaced | The new share in the same place |
| Receiver | Share stopped | Tile gone, layout restored |

## 9. Data and platform work this needs

Decided in [ADR-0083](../adr/0083-screen-share.md); what this spec needed:

1. **A share announcement** each participant can trust: who shares, that it
   started or stopped, whether it carries sound, and something that orders two
   starts the same way on every device, so the newer one wins and a tie
   resolves to one share everywhere. Group calls: the signed, sealed peer
   announcement / media state that already carries the camera state
   (`lakko_calls::relay`). One-to-one calls: the call signalling between the
   two people.
2. **Group media**: the share picture as its own track (the reserved SFrame
   stream index 2, ADR-0035) and the share sound as another, with its own
   stream index; the offer names the share lines so the call server forwards
   them as `Anna's screen` and not as a camera layer; the share is one more
   entry in `video-wants`, and the call server's downlink rule gives the share
   on a stage priority over cameras.
3. **One-to-one media**: the share as a further track on the existing
   connection, added mid-call the way the camera is.
4. **Receive on every client**: iOS and Android open, decrypt, render and play
   a share and its sound; they never capture one.
5. **`Permissions-Policy: display-capture=(self)`** on the production and the
   test-stack web configurations; today both say `()`.
6. **The desktop client**, when it exists, sends through the same web code if
   its webview offers screen capture; whether it does is unknown and decides
   whether it needs a native capture path.
7. **The rules written once**: which tile is on the stage now includes "a
   share" ahead of the speaker (`call_stage`), and the one-share winner rule,
   belong in `lakko-ui-rules` / `lakko-calls` so the three clients cannot
   disagree.

## 10. Accessibility

- `Share screen` / `Stop sharing` are spoken names with the pressed state.
- The top strip is a live region read once when it appears.
- The share tile is named `Anna's screen`; its zoom buttons are reachable by
  keyboard on web.
- The announcements of section 5.3 are polite and never interrupt speech.

## 11. Out of scope

Sending from iOS, Android or a phone browser; letting a host stop or approve a
share; several shares at once; remote control; drawing on a share; sharing a
region of the screen; a choice of picture quality; a separate volume for the
share; recording.

## 12. Open questions for the maintainer

1. **The sender's own preview.** Decided 2026-11-21: the live picture, and
   no picture only for a mirror (section 3).
2. **Motion versus detail.** A share favours sharpness. Should a shared tab
   that plays a video switch to smooth motion automatically?
