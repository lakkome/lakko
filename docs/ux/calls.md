<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Calls — UX spec

Written 2026-09-17 after a live 1x1 call between iOS and Android. What exists today
is not a call experience: the callee sees a small banner **only while the matching chat
is open**, a closed or backgrounded app never rings at all, an audio call opens the
pre-join screen with the camera on, the callee is then told it is a video call, the call
safety number sits across the top of the call as a raw strip of digits, and a finished
call leaves no trace in the conversation.

This spec covers the whole call surface: being called, calling, the call itself, and
what a call leaves behind. It replaces the call parts of
[`client-redesign.md`](client-redesign.md) where they disagree.

## 1. Principles

- **A call is an interruption, and it is allowed to be one.** Ringing takes the whole
  screen, over the lock screen, with sound and vibration, like every phone call.
- **The call must arrive when the app is closed.** A call that only rings inside an open
  chat is not a feature anyone can rely on.
- **Say what kind of call it is.** The word on the callee's screen matches what the
  caller chose, always.
- **Encryption is shown, not spelled out.** A small lock with one word, and the code
  itself only when someone asks to compare it.
- **Never pretend.** "Connected" only when media flows, the lock only when the call's
  own encryption is armed, no invented signal bars.

## 2. Receiving a call

### 2.1 The phone rings even with the app closed

| State of the callee's device | What happens |
|---|---|
| App open, any screen | Full-screen incoming call over the app, ringtone, vibration |
| App backgrounded | The system call screen (see below), ringtone, vibration |
| App closed | Same as backgrounded: the push wakes the app, the system call screen appears |
| Phone locked | The system call screen on the lock screen, answer without unlocking |
| Do Not Disturb, chat muted | Notification without sound; a muted chat never rings |
| Second call while already in a call | A compact banner inside the call: caller name, Accept (ends the current call) and Decline |

Native clients use the platform's own call machinery: on iOS the system call screen with
the call reported the moment the wake arrives, on Android the full-screen call
notification with the same. That is what gives a call a ringtone, a lock-screen answer
button, the phone's own call UI, and a place in the phone's call history.

**The wake is a call-specific push**, sent at the highest priority the platform offers
and marked so the background handler treats it as a call and not as a message preview.
Today all three background paths deliberately drop call signalling; that is the single
change that makes everything in this section possible (section 7).

**Ring rules.** Ring for **45 seconds**, then stop and record a missed call for both
sides. A call the caller cancels first stops ringing immediately. A call answered on one
of the callee's devices stops ringing on the others.

**On web** a call arrives as a notification with Accept and Decline; clicking it focuses
the tab and opens the call. A browser that refuses notification permission shows the
in-app banner only, and Settings says so in one line.

### 2.2 The incoming call screen

Caller avatar (96), caller name, one line saying what it is: `Voice call` or
`Video call`, and a small lock with `End-to-end encrypted` under it. Two large round
buttons: **Decline** (danger) and **Accept** (primary); a video call adds
**Accept without video** as a ghost button between them.

Nothing else on that screen. No code, no digits, no protocol words.

## 3. Placing a call

### 3.1 A voice call just calls

Tapping the phone button places the call immediately. No preparation screen, no
"check your microphone", no empty camera box: the call screen opens straight into
`Calling...` with the callee's avatar and name, and the controls are there to change
anything. This is what every phone app does, and asking a question before a voice call
is the thing that made the old screen feel like a settings page.

The camera stays off, and the callee is told `Voice call`.

### 3.2 A video call shows the camera first

The video button opens a **camera check**, because a person about to be seen deserves
to know what they look like. It fills the screen with the live self-view, the callee's
name across the top, and three round controls above the bottom edge: microphone,
camera, and a large **Call** button between them. Cancel is a plain close control in
the top corner. No card, no toggle rows, no labels reading like settings.

Turning the camera off there does not leave a hole: the preview becomes the callee's
avatar on the call background, and the button still reads **Call**. If the device has
no camera, the video button starts a voice call and says so in one line.

### 3.3 Answering

Accepting an incoming call never shows any preparation screen, for either kind.

### 3.4 While it rings

The caller sees the call screen with `Calling...` and can hang up. After 45 seconds it
becomes `No answer`, with **Call again** and **Message**.

**The 45 seconds are for the answer, not for the connection.** Once the other side
answers, both sides get a fresh window of 30 seconds to connect; a call that was
answered late is not cut off by a clock that started when the caller dialled (found
live 2026-09-20: an answer at 33 seconds left 12 seconds to connect, and the caller
ended the call as a timeout while the callee's screen said `Connecting...`). A call
answered but never connected ends as `Call failed` on both sides, never as
`No answer`.

## 4. The call screen

One screen for both kinds, laid out for the eye and not for the debugger.

**A voice call** is a portrait: the other person's avatar large and centred in the upper
half, their name under it in the largest type on the screen, and under that one quiet
line with the state or the running time. Nothing else competes with it. The background
is the call surface, not a tile.

**A video call** fills the screen with the other person, and the self-view is a small
rounded rectangle in a corner, draggable, about a quarter of the width. The name and
the state float over the video at the top, on a soft shade so they stay readable.

Both share:

- **The status line**: name, then state or duration. `Calling...`, `Connecting...`,
  `Reconnecting...`, or `12:04`. Never two of them at once, and never a floating pill
  repeating what the line already says.
- **One small lock chip** beside the status line: a lock and the word `Encrypted`, shown
  only when the call's own encryption is armed, opening the encryption sheet (section 5).
- **Controls**, bottom, one row of round buttons of equal size, 64 pt across, spaced
  evenly, each with a small label under it: microphone, camera, speaker, more, end call.
  End call is a wider red pill, visually separated from the rest. A control that is off
  is filled; a control that is on is outlined. Nothing but the end button is red.
- The controls fade out after 5 seconds of a connected video call and come back on a
  tap; on a voice call they never hide.

### 4.1 Which controls exist, and where they are

The real problem with the old screens was not their colour. It was that they used the
wrong **kind** of control in the wrong **place**:

- The pre-call screen used **switches in a settings list** — a label on the left, a
  toggle on the right. A switch says "this is a preference you are configuring". A call
  needs a button you press.
- The call screen hid real actions behind an **ellipsis menu**. Switching to the other
  camera, or moving the sound to the speaker, is something a person does mid-sentence
  with one thumb; a menu makes it two taps and a reading task.
- **Speaker was not on the screen at all**, although it is the control people reach for
  most on a voice call.
- The row sat glued to the bottom edge of a mostly empty screen, unanchored to anything.

The rules that follow from that:

**Never a switch, a checkbox or a list row inside a call.** Every call control is a
round icon button that toggles on press.

**The whole set is on the screen. There is no More menu in a 1x1 call.**

| Call kind | The row, left to right |
|---|---|
| Voice | Speaker · Mute · Camera · End |
| Video | Speaker · Mute · Camera · End |

**The camera button is on a voice call too, and it turns the call into a video one.**
People start a call with their voice and decide to show something a minute later; every
messenger allows that, and taking it away to keep the row short would be a feature lost
to tidiness. Pressing it starts the camera, tells the other side, and the screen becomes
the video layout for both. The other person is not forced into video: their own camera
stays off until they press their own button, and their screen shows the incoming picture
with their self-view absent. Turning the camera off again returns the call to the
portrait.

- **Flip camera does not belong in the row.** It lives on the self-view itself, as a
  small button in that rectangle's corner, where the thing it changes is visible.
- A group call keeps its participants and chat entries. On a phone those live behind
  a single overflow button, because six circles do not fit a 400 pt row at a size a
  thumb can hit; on the web there is room, so the row shows them all — Speaker ·
  Mute · Camera · Participants · Chat · End — at slightly smaller circles (56 pt, End
  64 pt), shrinking further on a narrow window and never below the 44 pt floor. Mute,
  camera, speaker and end stay in the row everywhere.
- **No switches for echo cancellation, noise suppression or gain control.** The
  engine keeps all three on; the settings sheet once offered three toggles that
  changed a stored flag nothing read, and a switch that does nothing is worse than
  none. Echo between two devices in one room is not something a switch fixes: each
  device cancels only what it played itself.
- The row sits inside one rounded translucent bar, floating above the bottom safe area
  with a margin, not welded to the edge. On a voice call it is vertically placed in the
  lower third, under the portrait, not at the very bottom of an empty screen.
- **The pre-call camera check** (section 3.2) uses the same round buttons, not switches:
  microphone and camera either side of the Call button.

### 4.1.1 How each button looks

- Equal circles, 64 pt across, evenly spaced in one row; the end button is a 72 pt red
  circle set apart by twice the gap. Nothing else on the screen is red.
- **The off state is the loud one**: a live microphone or a sending camera is an outline
  (translucent white fill about 12 percent, white icon); muted or stopped is solid white
  with a dark slashed icon. The old screen highlighted the off state in the accent
  colour, which read as "on".
- One short muted label under each circle: `Speaker`, `Mute`, `Camera`, `End`. The label
  names the action, never the state, and does not change size when the state does.
- Disabled is 40 percent opacity, never a hidden button. Touch targets stay at least
  44 pt. Spoken labels name the action and the resulting state (`Mute microphone`,
  `Unmute microphone`).

### 4.2 Measured against the messengers people already use

Checked against Telegram, WhatsApp, Signal and the iPhone's own call screen, because a
call screen is the one surface where being unusual is a cost, not a feature.

| Question | What they do | What we do |
|---|---|---|
| Background of a voice call | Telegram animates a gradient built from the contact's colour; Signal and WhatsApp blur the contact's photo | The contact's avatar, heavily blurred and darkened, behind the portrait. Falls back to the initials colour when there is no photo. Flat surface reads as an empty page, which is what made ours look unfinished |
| Where the controls live | WhatsApp, Telegram and FaceTime all float them in one rounded container over the content, not loose on the background | One rounded translucent bar holding the row, with the end button inside it at the end. That container is what our screen was missing |
| Labels under the buttons | Telegram labels them, WhatsApp does not, Signal does not | Keep the labels. They cost one line and remove all guessing, and they are what a screen reader reads anyway |
| Which state is highlighted | All of them make the **off** state the solid, high-contrast one | Same, and it is the correction our screen needed most: it highlighted the off state in the accent colour, which read as "this is on" |
| Encryption | Telegram shows four emoji in the corner during a call, expandable; Signal keeps the safety number in the contact screen; WhatsApp shows a lock line | Our lock chip with the number behind a sheet, which is Telegram's idea with our own words |
| Ending a call | All of them: one red circle, nothing else red on screen | Same |
| Pre-call screen | None of them shows a settings card before a voice call. Telegram and WhatsApp dial straight; only a **video** call shows the self-view first | Same, which is what sections 3.1 and 3.2 now say |
| While connecting | A short status word under the name, never a badge floating elsewhere | Same |

Two things they do that we deliberately do not: an animated gradient that keeps moving
during the whole call (decoration with a battery cost), and hiding the controls on a
voice call (nothing to look at underneath, so hiding them only makes the user tap).

### 4.3 When the connection or the other people go away

A call must never sit on screen pretending to be a call. Two cases, both decided
by rules the clients share:

- **The connection to the call server is lost.** The phone notices within 45
  seconds (it exchanges a small keepalive with the server every 15 seconds and
  gives up after three misses) and the call ends as `Call ended`, with the usual
  row in the conversation. The server drops a phone that has stopped answering
  within a minute, so the others see that person's tile go rather than a frozen
  face for the rest of the hour. There is no `Reconnecting...` on a group call
  today: the transport does not resume a session, so the honest word is the end.
- **Everyone else has left a group call.** When the last other person leaves a
  call that had other people in it, the call ends by itself 5 seconds later,
  as `Call ended`; someone rejoining inside those 5 seconds cancels it. The
  person who started a call and is still waiting for the first one to join is
  not in this case: nobody left, so they wait until they hang up.
- **The last person in a call left or dropped, and nobody was there to
  notice.** A phone whose app dies publishes no `Leave`, so on every other
  phone the call it was alone in stayed "in progress" until its last sign of
  life aged out, and whoever tapped Join sat alone in an empty room. The call
  server decides this case, once: a room that has stood empty for 5 seconds is
  over. The 5 seconds are for a bad connection — a socket that blips and comes
  back inside them finds the call still there, and anyone who joins inside
  them keeps it alive (they are then a participant like any other; a group
  call has no admin, and a link call's owner duties pass by the election of
  ADR-0032 to whoever is present and back when the owner returns). Anyone who
  tries to join after that is told the call ended, and the first member device
  to learn it — on a refused join, or by asking the server every 10 seconds
  while it shows a call as joinable — closes the record for the whole chat.
  The same applies to a link call from the moment its first participant
  connected; before that, the link's own expiry (an hour by default, a week at
  most) bounds how long it can be joined.

- **A link is one call.** Once the call it names has ended, the link is spent
  for everyone, the starter included: opening it says the call has ended, and
  a new call means a new link. The starter's own page never restarts a call
  on a spent link.
- **Whoever leaves a link call can come back, the starter included.** Leaving
  (or dropping) takes the person out of the call's group: whoever holds the
  committer duty at that moment removes their leaf, so the key rotates away
  from them and the group holds only the people in the room, and forgets
  their admission, so the next time that person opens the link they are
  admitted afresh. Before this a person who left sat at `Waiting for the
  owner to admit you...` for good, because the one-admission-per-person guard
  kept re-serving a welcome their new page could not open. The starter comes
  back the same way: opening their own link, they ask to be admitted like
  anyone else when people are in the room, the person keeping the call alive
  admits them, and the committer duty returns to them; only an empty room
  makes them found the call. A pending joiner re-sends its request every few
  seconds until admitted, so an admission is never lost to who arrived first.

Neither rule changes what is announced to the group: the leaving device tells the
group it left, and ends the call for the chat when its record shows nobody else
still in it, exactly as a hang-up does.

**The row says how long the two sides were connected, never how long a screen sat
open.** When a device notices the end only after the keepalive deadline has passed
since the server last answered (it was asleep, or the link died and it found out
late), the call ended when the link did, and the row's duration runs to that
moment. Two devices left in a call overnight each wrote `8:20:53` the next morning
at the moment the laptop woke; the same rule on every client stops that.

### 4.4 A group call: the big tile and the grid

A group call is a grid of tiles with, sometimes, one tile drawn large above the
rest. The same rule decides that tile on every client, because the three screens
were each picking someone else and one of them was picking wrong:

- **The large tile shows a picture, or there is no large tile.** It goes to the
  person the user pinned; otherwise to the person who is speaking and sending
  video; otherwise to the first person in the roster who is sending video. When
  nobody else is sending video there is no large tile at all: everyone sits in
  the grid at the same size, camera or no camera. A large tile holding an
  avatar while other people's cameras run in small tiles was the defect - the
  first person to join took the big slot for the whole call, whatever they were
  sending.
- **The user sees themself the moment they are in the call.** The person who
  starts a group call is its first participant, so there is no waiting screen
  and no message about others joining: the screen is the group screen from the
  first second, with the user's own tile, their own camera when it is on, and
  their own name on it (the profile name; `You` only when no name is set). The
  status line already carries the count.
- **The user's own tile is never the large one** unless they are alone in the
  call, when their own preview is the only thing there is to show. With others
  present it sits in the grid like everyone else's.
- **Pinning wins over everything.** A pinned tile stays large whether or not it
  is sending video, until the user unpins it; that is what a pin is for.
- **The grid keeps roster order** and never reorders on its own. When the person
  in the large tile stops sending video, the next one sending video takes the
  slot and they drop into the grid where their roster position puts them.
- **The control bar fits the screen it is on.** Five circles at the sizes of
  section 4.1.1 do not fit a 402 pt phone, so when the room is short the
  circles shrink together, keeping their ratio and never going under the 44 pt
  touch target; with room to spare they stay at the spec sizes. The same rule
  on every client, so the rows shrink alike on the same width. A row wider than
  the screen was found on the iPhone 17 with the Speaker circle clipped at the
  left edge and the End button past the right one.
- **The tiles never sit under the status line or the control bar.** The scroll
  area reserves the height of both, so the last row of tiles is fully visible
  above the bar and the first row starts below the status line. The bar still
  fades on a tap, but a tile is never legible only while it is faded.

- **Every tile carries the person's name, in a link call too.** A tile
  labelled with eight hex characters (`01a0a045`) named nobody. In a chat group
  call the name comes from the profile the group already carries. In a link
  call the joiners are strangers to each other and the guest has no contact
  list, so each participant seals its own profile into the call's own group
  and every member relays it once it has joined and again whenever someone new
  is admitted (the newcomer can only read from that moment on). The server
  forwards ciphertext; a name never crosses it in clear. A name learnt this way
  is what the person calls themself and outranks the contact-list lookup; the
  contact list stays the fallback, and `Peer` is the last resort.
- **The person joins with what the device has.** A video call asks for a
  camera and a microphone; a device without a camera, or a browser that
  blocks capture, used to fail the whole request and print the browser's own
  words (`Failed: Invalid constraint`). Now the camera failing leaves the
  microphone: the person joins with the camera off and one line says `No
  camera was found. You joined with your microphone.` (or `Lakko was not
  allowed to use the camera...`). The microphone failing too still lets them
  in, to listen and watch, with the line saying so. The browser's error text
  never reaches the screen. Nothing changes about encryption: whatever is
  sent is sealed as before, and a person who sends nothing seals nothing.
- **The link landing names the host or says nothing.** `Host:` followed by a
  sixty-digit fingerprint, or by `Member`, was found on every landing; a string
  of digits names nobody. The line shows the host's name when the device knows
  one (a contact or a received profile) and is absent otherwise; the owner's
  own page (`Start your call`) never has it.

The one line of the spec every client implements is: pinned, else speaking with
video, else first with video, else nobody. `speaking` is a hint the transport may
never set; the rule must read the same when it is always false.

## 5. The safety number

Today the call safety number is printed across the top of the call as sixteen digits.
It is unreadable, it means nothing to anyone who has not been told what it is, and it
occupies the most valuable line on the screen.

**New rule: the code is never shown unless the user asks for it.**

- On the call, encryption is one chip: a lock and the word `Encrypted`.
- Tapping the chip opens the **encryption sheet**: a title `This call is encrypted`, one
  sentence in plain words (`Only you and Anna can hear this call.`), the safety number in
  four groups of four digits, large and grouped for reading aloud, and one line telling
  the user what it is for (`Read these numbers aloud. If they match on both phones, no
  one is listening.`). Buttons: **Copy** and **Done**.
- The same number is reachable outside a call from the contact screen as
  **Verify security code**, which is where the design system already puts it.
- The chip is absent when the call's encryption is not armed, and the call refuses to
  start rather than continuing unencrypted. That refusal is a call state
  (`Couldn't connect`), never a silent fallback.

## 6. What a call leaves behind

- Every finished call writes a row into the conversation: `Outgoing call, 12:04`,
  `Incoming call, 12:04`, `Missed call`, `Declined`. Tapping the row calls back.
- A group call writes the same row, once, on the device that was in it. Direction is
  whose call it was from that device's side: one it started is outgoing, one it joined
  is incoming. Only a device whose media flowed writes a row: a join that never
  connected rang nowhere and missed nothing, and the banner is what announces a live
  group call, so there is no missed row.
- A missed call also raises a normal notification, and the chat list shows the chat as
  unread with `Missed call`.
- The **Calls** section of the app lists recent calls with the same wording, newest
  first, each row tappable to call back.

## 7. Data and platform work this needs

Flagged explicitly, because the current transport cannot support the spec:

| Need | Why |
|---|---|
| A call-kind wake that survives the background filters | All three background paths drop call signalling today, so a closed app never learns about a call |
| Highest-priority delivery for that wake | A normal push is delayed or coalesced; a call is worth the battery |
| Caller identity in the wake, resolved locally | The system call screen needs the name before any chat is open |
| Call kind (voice or video) on the offer, matching the caller's choice | So the callee is told the truth |
| A ring timeout shared by both sides (45 s) | So one side does not ring on after the other gave up |
| An end reason (answered elsewhere, declined, missed, cancelled, failed) | Drives the transcript row and stops the other devices ringing |
| A relay for media when a direct path fails | A call between two phones on different networks does not connect at all today |

Platform mechanisms: on iOS the system call integration plus its dedicated call push, on
Android the full-screen call notification plus the phone's call integration, on web a
notification with actions from the service worker.

## 8. Accessibility

The incoming call screen and the call controls carry spoken labels
(`Accept voice call from Anna`, `Mute microphone`); the running time is announced as a
duration, not read digit by digit; the safety number is read in groups of four; nothing
relies on colour alone (the end-call button has an icon and a label, the lock has a
word).

## 9. Out of scope here

Group call redesign beyond the shared call screen and the tile rule of section
4.4, call recording, call links
(they keep the behaviour of [ADR-0032](../adr/0032-multi-joiner-call-links.md)),
screen sharing, and call history sync between devices.
