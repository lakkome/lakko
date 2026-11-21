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
| Chat muted | No sound, no vibration, no screen that takes over. The call is still OFFERED and still answerable - see 2.1.1 |
| Do Not Disturb | Notification without sound |
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

Both of those numbers-and-rules are ONE copy, in `lakko_ui_rules::ring`, reached over
the FFI by all three clients. The window in particular is a value two DIFFERENT devices
have to agree on: if it drifts, one side files a missed call while the other is still
ringing, which is the rule breaking silently rather than loudly. It had been written out
four times before that.

**What muting withholds, stated because the row above is short enough to be read two
ways.** A muted conversation does not ring: no ringtone, no vibration, no screen that
takes over the phone. It is **not** dropped. Somebody muted a conversation to stop being
interrupted by it; they did not ask to stop being told who tried to reach them. So a
muted call is recorded in the history exactly as an unmuted one, and a missed one still
reads as missed. A client may leave a quiet, passive notice behind — that is a per-platform
call — but it must not use the platform's call affordance, and it must never demand
attention.

### 2.1.1 A muted conversation: offered, not thrown

Muting takes away the **interruption**, not the call. Somebody who muted a
conversation asked not to be dragged out of what they are doing by it; they did not
ask to stop being reachable by the person, and they did not ask to be told about it
only afterwards.

So a call from a muted conversation makes **no sound and no vibration**, never takes
the screen over, and is offered on two quiet surfaces instead:

- **Inside that conversation** — a panel at the top of the chat: caller name,
  `Voice call` or `Video call`, **Decline** and **Accept**. It sits in the layout
  rather than over it, so nothing the person was reading moves or is covered.
- **In the conversation list** — that chat's own row carries **Accept** and
  **Decline** while the call is ringing. The row is where the person already looks
  to see what is happening, and it needs no navigation to answer from.

Both disappear when the ring window runs out, and the call is then recorded as
missed exactly as an unmuted one is. Nothing about the person's own screen decides
which surface shows: whichever of the two they are looking at is the one that
offers the call, and a person on neither simply finds a missed call, which is what
muting asked for.

**This replaces an earlier, worse reading.** The first pass took "never rings" to
mean "show nothing at all", which made a muted contact unanswerable — muting a
conversation quietly became half-blocking the person. It also landed split: two
clients showed nothing and one still threw the full-screen call across whatever
was on screen, silently, which honoured half the rule. Neither is what mute means.

**When mute cannot be read, ring.** Every ring path has to consult a flag that lives
somewhere else — on iOS in another process, on web in the service worker — and a read can
fail. The fallback is to RING. Failing the other way would swallow calls silently because
a file was missing or a database did not open, and a swallowed call is a far worse defect
than an unwanted one.

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

### 3.2 A video call just calls too

The video button places the call immediately, with the camera on. One press of a call
button is one call, for both kinds.

**This replaced a camera check** (a full-screen live self-view with a Call button
under it), removed 2026-09-21. Its reason was sound — a person about to be seen
deserves to know what they look like — and nothing about that reason is abandoned:
the self-view is already on the calling screen, so the caller sees themselves while it
rings, which is the same look a moment later and without a second press. What the
check cost was the thing section 3.1 refuses for a voice call, one press further
along: a question before a call.

If the device has no camera, the video button starts a voice call and says so in one
line.

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

**The outgoing tone belongs to a one-to-one call, and stops at the answer.** It
plays only while a one-to-one call this device placed is waiting for the other
person, and stops the moment they answer; the seconds of connecting after that
are silent on both sides. **A group call rings nobody**, and neither does a call
joined from a link: the person who starts one is simply in it, and a person who
joins is joining, not calling. Their status line reads `Connecting...`, never
`Calling...`. What remains for a group is the two short cues, one when the call
connects or someone joins and one when someone leaves or the call ends. Before
this every call that was not yet connected looped the tone, so a person alone in
a call they had just started heard a phone ringing, and so did everyone who
arrived by a link (found live 2026-10-02). One shared rule,
`lakko-ui-rules::call_group::ringback_plays`, on all three clients.

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
  evenly, each with a small label under it; section 4.1 lists them. End call is the one
  red control, visually separated from the rest. A control that is off
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
- The row sat glued to the bottom edge of a mostly empty screen, unanchored to anything.

The rules that follow from that:

**Never a switch, a checkbox or a list row inside a call.** Every call control is a
round icon button that toggles on press.

**The whole set is on the screen. There is no More menu in any call.**

| Call kind | The row, left to right |
|---|---|
| One-to-one, voice or video | Microphone · Camera · Share screen · Settings · End |
| Group | Microphone · Camera · Share screen · Participants · Chat · Settings · End |

`Share screen` exists only where the device can send a screen: the web client
in a desktop browser and the desktop client; never on iOS, Android or a phone
browser ([`screen-share.md`](screen-share.md), 2026-10-25).

The row was rebuilt on 2026-10-02 after a live group call showed seven circles
wrapped into two rows on a phone. Speaker left the row (it is a setting, not
something done mid-sentence), the chat control left the product, Participants
and Invite became one entry, and Settings arrived.

**Settings is where the devices are chosen.** It opens a sheet of the app's own
with up to three radio lists, each starting with `System default`: the
microphone, the camera, and the speaker where the platform lets the app move a
call's sound. A list with nothing to choose is absent. A choice applies at once
and holds for the call. Never a system picker or a system route sheet.

**Settings is also where a person changes the name others see in this call**
(2026-11-07, maintainer). At the top of the sheet, above the device lists:

- A text field labelled `Your name in this call`, holding the name this
  person typed for this call, or empty when they typed none. Its placeholder
  is the name others see by default: the profile name, or nothing when there
  is none. Under it one line: `Everyone in the call sees this name. Your
  profile does not change.`
- A `Save` button beside the field, enabled only when the field differs from
  the saved call name and passes the display-name rule of
  [`profile-names.md`](profile-names.md) (trimmed, 1-64 characters, no line
  break, no control or invisible formatting character). A text that fails the
  rule shows the profile screen's sentence for that failure under the field
  and keeps `Save` disabled.
- `Save` applies at once: the person's own tile and, within moments, every
  other participant's tile for them show the new name. Saving an empty field
  takes the person back to the default (the profile name, or the viewer's own
  name for them when they are a contact).
- The name holds for this call only. It is not written to the profile, to the
  conversation or to any server: it travels sealed to the call's participants
  and is forgotten when the call ends (a rejoin of the same call from the same
  app run keeps it). The profile name never changes because of it.
- Any participant can do it: an account holder, a guest, the person who
  started the call.
- Not yet on a one-to-one call: the field is absent there until the
  one-to-one carrier exists (the sealed relay of ADR-0078 section 2 exists
  only on a call through the SFU, i.e. a group call or a link call).

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
- **A group call adds one entry, `Participants`.** On a call that can hand out
  a link to itself it opens a menu: `Participants` (only once someone else is
  in the call), `Copy invite link`, `Invite contacts` (`call-links.md` section
  3). On a call placed inside a conversation, which has no link, it opens the
  list directly, and is absent while the person is alone. Participants and
  Invite were two buttons until 2026-10-02; alone in a call the first had
  nothing to list.
- **A group call has its own chat** (section 4.1.4, maintainer 2026-11-07).
  Until then there was none: the chat control of the first group screen sent
  nothing anywhere and was removed on 2026-10-02, and a person was told to
  collapse the call and write in the conversation. People in group calls asked
  for one, and a guest in a link call has no conversation to write in at all.
  A one-to-one call has no chat: the two people already share a conversation,
  and the carrier the call chat uses exists only on a call through the SFU.
- **The group row is always one line.** On a narrow screen (under 600 pt) or a
  short one (under 520 pt) the words under the circles go (each circle keeps
  its spoken name), and the circles are sized from the width and the number of
  controls, never below 38 pt.
- **No switches for echo cancellation or gain control.** The engine keeps both
  on; the settings sheet once offered three toggles that changed a stored flag
  nothing read, and a switch that does nothing is worse than none. Echo between
  two devices in one room is not something a switch fixes: each device cancels
  only what it played itself. The one audio switch the sheet does carry,
  `Noise suppression`, drives Lakko's own suppressor and is specified in
  section 4.1.3.
- The row sits inside one rounded translucent bar, floating above the bottom safe area
  with a margin, not welded to the edge. On a voice call it is vertically placed in the
  lower third, under the portrait, not at the very bottom of an empty screen.

### 4.1.1 How each button looks

- Equal circles, 64 pt across, evenly spaced in one row; the end button is a 72 pt red
  circle set apart by twice the gap. Nothing else on the screen is red.
- **The off state is the loud one**: a live microphone or a sending camera is an outline
  (translucent white fill about 12 percent, white icon); muted or stopped is solid white
  with a dark slashed icon. The old screen highlighted the off state in the accent
  colour, which read as "on".
- One short muted label under each circle: `Microphone`, `Camera`, `Participants`,
  `Chat`, `Settings`, `End`. The label names the thing, never the state, and does not change
  size when the state does.
- Disabled is 40 percent opacity, never a hidden button. Touch targets stay at least
  44 pt. Spoken labels name the action and the resulting state (`Mute microphone`,
  `Unmute microphone`).

### 4.1.2 End, for the person who started a group call

Decided by the maintainer on 2026-10-02. It is the one place a call control
opens a choice, and the reason is that the two things it could mean are not
the same thing for the other people.

**Who.** The person who started the call: in a chat group call the account
whose device announced it, on any of that account's devices that is in the
call; in a link call the account that made the link. Nobody else, whatever
their role in the group. A one-to-one call has no such choice: ending it ends
it for both.

**When.** Only while at least one other person is in the call. Alone in the
call (still waiting for the first person, or everyone else has left), `End`
ends it as before, with no choice shown: both items would do the same thing.

**What it looks like.** Pressing `End` opens a small menu above the control
row, drawn by the app (not a system dialog), with two rows:

| Row | What it does |
|---|---|
| `Leave call` | This person leaves. The call goes on for the others, exactly as `End` does for everyone else in the call. |
| `End call for everyone` | The call ends for every person in it. Red text. |

Pressing outside the menu, or `End` again, closes it and nothing happens; the
call is never ended by the first press. There is no second confirmation: the
menu is the confirmation. Both rows are at least 44 pt tall; with a keyboard,
`Esc` closes it and the first row has focus.

**For the others.** Their call closes at once, and one line says why for a
few seconds wherever they are in the app: `{name} ended the call for
everyone` (the name by the tile rule of section 4.4; when the device cannot
name the person, `The call was ended for everyone`). The conversation gets
the usual call row. A person who was not in the call sees the offer to join
disappear.

**Afterwards.** The call is over: it cannot be joined again, and a link call's
link is spent as after any ended call. The starter can start a new one.

**What makes it hold.** The end is a message inside the conversation's
encryption (inside the call's own group for a link call), and each device
acts on it only when it comes from the person who started that call; the
same message from anyone else is treated as an ordinary end notice and takes
nobody out of a call. The server is not asked and cannot do it. A device that
does not receive the message (asleep, offline) stays in until it notices it
is alone, which ends the call for it by the rule of section 4.3.

**States.**

| State | What the starter sees |
|---|---|
| Others in the call | `End` opens the menu |
| Alone in the call | `End` ends the call, no menu |
| `End call for everyone` pressed | The call screen closes as on any hang-up; the message is sent in the background |
| The message could not be sent | The starter has still left; the others stay until they hang up or the call empties. No error is shown: there is nothing the person can do about it from outside the call |

### 4.1.3 Noise suppression

Decided by the maintainer on 2026-11-07 after a group call where typing and room
noise reached everyone. Each client runs Lakko's own noise suppressor on the
microphone before the sound is encoded ([ADR-0088](../adr/0088-client-noise-suppression-rnnoise.md));
the platform's echo canceller stays on underneath it.

**Where.** One switch in the Settings sheet, above the device lists, in its own
section headed `Sound`:

```
Sound
  Noise suppression                          [on]
  Removes typing and background noise from your microphone.
```

The sheet is the only place it appears. The rule "never a switch inside a call"
(section 4.1) is about the control row; the Settings sheet is a settings page and
uses the platform's switch there, like any other preference. The section shows
even when no device list is shown (before a permission is granted the device
lists are absent, the switch is not).

| Property | Rule |
|---|---|
| Default | On |
| Scope | This device only. Stored on the device, not synced to the account's other devices, never sent to anyone in the call: the others hear the result, not the setting |
| When it applies | At once, mid-call, without renegotiating and without a gap the person must wait out. It also holds for the next call on this device |
| Off | The microphone is sent as it was before this feature: the platform's own echo cancellation, noise suppression and gain control, nothing of Lakko's |
| Who sees it | Every call kind (one-to-one, group, link call), on every client that can run the suppressor |

**States.**

| State | What the person sees |
|---|---|
| On, suppressor running | The switch on. Nothing else: no badge on the microphone button, no indicator on the tile |
| Off | The switch off |
| On, but the suppressor could not start on this device (no audio worklet, the module failed to load, the platform hook is missing) | The switch stays on and the call goes on with the platform's own processing, exactly as with the switch off. No error, no toast: there is nothing the person can do about it, and the call is not worse than it was before this feature. The client logs one line saying which processing is applied |
| A client that cannot run the suppressor at all (no platform support yet) | The section is absent rather than a switch that does nothing (section 4.1's rule) |

**Copy.** `Sound` (section heading), `Noise suppression` (switch label), the hint
line above. Spoken name of the switch is its label; its state is read by the
platform as on / off. No word about models, machine learning, AI, or RNNoise:
the person needs to know what it removes, not how.

**Not here.** No strength slider, no per-participant suppression of what others
send (each device cleans its own microphone), no "voice isolation" mode.

### 4.1.4 Chat in a group call

Decided by the maintainer on 2026-11-07, after people in group calls asked for
it. One chat, the same on a group call placed in a conversation and on a link
call (`call-links.md`), for account holders and guests alike.

**What it is.** Text messages between the people in the call, for as long as
the call lasts:

- Seen only by the people in the call when the message is sent. Someone who
  joins later does not see what was written before they came; someone who
  left keeps what they saw until their call ends.
- Gone when the call ends for this device (End, or the call ending). Nothing is
  kept on the device after that, nothing is written into the conversation the
  call came from, nothing is stored on any server (a picture's encrypted file
  stays in the file store a few days at most, section 4.1.4.1). A connection that drops and
  comes back inside the same call keeps the messages; joining the same call
  again after End starts empty.
- Sealed for the people in the call and signed by the sender; the server
  forwards ciphertext and cannot read or forge a message.
- Text, emoji and pasted pictures (the emoji button of the composer, below,
  and section 4.1.4.1; both added 2026-11-21). No files, voice messages,
  stickers, GIFs, replies,
  reactions, edits or deletions. Links in a message are live, by the rule of
  [`message-links.md`](message-links.md). Pictures pasted from the clipboard
  since 2026-11-21 (section 4.1.4.1).

It is not the conversation the call came from, on purpose: a message written in
the call reaches only the people in the call and leaves no trace in the group.
A person who wants to write to the whole group collapses the call (section 4.5)
and writes in the conversation, as before.

**Entry.** The `Chat` circle in the group row, after `Participants`. It is
there from the first second, alone in the call too (the panel then shows its
empty state). Its label never changes; its spoken name is `Open chat` /
`Close chat`.

- **Unread count.** While the panel is closed, a message from someone else puts
  a count on the circle: the accent badge of the design system, `1` to `9`,
  then `9+`. Opening the panel clears it.
- **A short preview.** While the panel is closed, a message from someone else
  also shows for 4 seconds as one line above the control bar: the sender's name
  in 13/600 and the start of the text, cut with an ellipsis. Tapping it opens
  the panel. A second message replaces the first and restarts the 4 seconds.
  The preview is not shown while the control bar is faded, and never over the
  settings sheet or the participants list. No sound, no vibration, no system
  notification.
- **The collapsed window** (section 4.5) does not change: one state, one place.
  The count waits on the circle for when the call is expanded.

**The panel.**

| Width of the call screen | Shape |
|---|---|
| Under 900 (every phone, a narrow window) | A panel over the whole call screen, sliding up from the bottom. The call keeps running behind it |
| 900 and wider (desktop, a wide browser window, an iPad in landscape) | A column 360 wide at the end side of the call screen (right in a left-to-right language). The tiles reflow into the width that is left, by the same grid rule |

Units are pt (iOS), dp (Android), px (web).

Top to bottom:

1. **A header**: `Chat` (17/600), and two controls. Close (a chevron down on
   the full panel, a cross on the column; spoken `Close chat`). And the round
   mute button of the collapsed bar, drawn on and off exactly as the call
   screen draws it, because muting is the one control a person needs in the
   second they need it, and the full panel hides the control row. The column
   (wide screens) leaves the control row visible and has no mute button.
2. **The messages**, newest at the bottom, scrolled to the newest when the
   panel opens and when a message arrives while the person is at the bottom;
   while they have scrolled up, a new message does not move the list and a
   small `New messages` pill at the bottom takes them there. Each message is a
   message bubble of the design system: the person's own on the end side in the
   accent fill, others' on the start side in the sunken fill. Above the first
   of a run of bubbles from one other person, their name in 13/600 muted: the
   same name their tile shows (section 4.4, steps 0-3), and it follows a later
   change of that name. Inside the bubble corner the time the message arrived
   on this device, `14:05`, 11 muted. No delivery or read state.
3. **The composer**: the design system's composer without the attach button
   and without the microphone: a growing text field (up to 5 lines, then it
   scrolls) with the placeholder `Message everyone in the call`, the emoji
   button, and a send button, disabled while the field is empty or only white
   space. On a keyboard Enter sends and Shift+Enter starts a new line; on a
   phone the keyboard's return key starts a new line and the button sends.

   **The emoji button** (added 2026-11-21, after people in calls asked for
   it): the smiling-face button of the conversation composer, between the
   field and Send, spoken `Emoji`. It opens the conversation composer's own
   emoji picker with the Emoji tab only: no GIF tab, since the call chat
   carries text. Search, categories, Recent and skin tones are the picker's
   own, shared with the conversation composer (one Recent list).

   - **Web and desktop**: the picker opens as the conversation composer's
     popover, anchored above the composer, inside the panel or the column.
     A click on an emoji puts it at the caret and keeps the picker open;
     `Esc`, a click outside it, or the button again closes it, and `Esc`
     then does not close the panel too. Focus stays in the text field.
   - **iOS and Android**: the picker is the conversation composer's bottom
     sheet, over the call chat panel; a tap puts the emoji at the caret and
     keeps the sheet open, `Done` (iOS) or a swipe down closes it. The call
     keeps running behind it; the mute button of the panel header is under
     the sheet while it is open, as it is under the keyboard.
   - **The limit**: an emoji counts toward the 800 bytes like any text (an
     emoji is 4 to about 30 bytes). An emoji that would pass the limit is not
     put in, and the `A message in a call can't be longer than this.` line
     shows, as when typing reaches the limit.
   - **Sending** is unchanged: the picker never sends anything by itself. A
     message of emoji only is an ordinary message.

   Copy: the button's label and spoken name `Emoji` (catalog key
   `call.chat.emoji`); the picker's own strings are the conversation
   composer's.

**Empty state.** Centred in the message area, muted, two lines and no action:
`Messages here are seen only by people in the call.` /
`They disappear when the call ends.` The same two lines stay as a small muted
note at the top of the list once there are messages, so a person who opens the
chat late knows why they see nothing earlier.

**States of one message.**

| State | What the person sees |
|---|---|
| Sent | The bubble, nothing more. A sent message is shown at once and is not confirmed: the server forwards it to the people in the call and nothing comes back |
| Could not be sent (the call is reconnecting, or the connection to the server is down) | The bubble at 60% opacity with `Not sent.` under it in 13 danger, followed by a ghost `Send again` button that sends it again. A message not sent when the call ends is gone with the rest |
| Too long | The field takes no more text once the message reaches the limit, and one line under the field says `A message in a call can't be longer than this.` (13 muted). The limit is 800 bytes of text, which is about 800 Latin letters or 400 Cyrillic ones |
| From someone the device cannot yet name | Held, not shown, until that person's announcement reaches the device (section 4.4: the same moment their tile gets a name), then shown in arrival order. A message from a sender the device never verifies is never shown |

**Order.** Messages appear in the order they reach this device. Two people
writing at the same moment may see the two messages in a different order on
their two screens; that is accepted, as in every call chat people know. The same
message never shows twice.

**What a person can do with a message.** Long-press (phone) or right-click
(web, desktop) opens the action sheet with one row, `Copy`. Nothing else: there
is no edit, delete, reply, forward, pin, save or reaction in a call chat.

**When the call ends** the panel closes with the call screen and every message
goes with it. The `Call ended` overlay says nothing about the chat.

**Copy** (catalog keys `call.control.chat*`, `call.chat.*`): `Chat`, `Open chat`,
`Close chat`, `Open chat, 3 unread`, `Call chat` (the panel's spoken name),
the two notice lines above, `Message everyone in the call`, `Send`, `Not sent.`,
`Send again`, `A message in a call can't be longer than this.`, `New messages`,
`Anna: text` (the announcement), `Copy` (the shared `common.copy`).

**Keyboard and screen readers.** Web and desktop: `Esc` closes the panel when it
is open (and only then collapses the call, section 4.5); focus moves to the text
field when the panel opens and back to the `Chat` circle when it closes. A
message that arrives while the panel is open is announced politely (VoiceOver,
TalkBack, an ARIA live region) as `Anna: text`; with the panel closed only the
count changes, read as part of the circle's spoken name
(`Open chat, 3 unread`).

**What the core must carry** (flagged for the ADR step):

| Need | Why |
|---|---|
| A call-chat payload on the sealed call relay: call id, the sender's peer id, a sequence number per sender, the text | The relay already exists on every call through the SFU and reaches exactly the people in the call; a conversation journal would keep the messages and reach people who are not in the call |
| Signed by the sender's MLS leaf key, verified against the peer's verified announcement, as the name for the call is | So the name over a bubble is the person who wrote it, and the server cannot write as anyone |
| A text bound, checked by the composer and by every receiver | The relay carries at most 1024 bytes of plaintext |
| De-duplication by (sender, sequence number) | A relay frame re-sent after a reconnect must not show twice |
| The same verify and bound on every client, written once in the core | The three clients must agree on what a valid message is |

### 4.1.4.1 Pictures pasted into the call chat

Asked by the maintainer on 2026-11-21 and decided the same day (option 1
below, with saving allowed and nothing for later joiners).

**What a person does.** On web and desktop, paste (`Cmd+V` / `Ctrl+V`) with a
picture on the clipboard while the call chat's text field has focus, or drop
a picture file on the panel. On iOS and Android, paste a picture into the
field (the keyboard's or the field's own Paste). The picture goes into a
small tray above the field: a thumbnail 64 high with a cross to remove it,
one picture at a time (a second paste replaces the first). Send is enabled
while the tray holds a picture, with or without text.

- **Sending.** The picture is sent first, as a message of its own; text in
  the field, if any, follows as a second message. Both bubbles show at once;
  the text waits until the picture's file has uploaded and the picture is on
  its way (or failed), drawn at 60% meanwhile, so everyone receives the two
  in that order. A failed picture does not hold the text back. The picture is prepared on
  the device the way a photo in a conversation is: decoded, its longest side
  brought down to the conversation's limit (2560), re-encoded as JPEG, which
  drops every piece of metadata (location, camera, time).
- **Refused.** Anything that does not decode as a picture: one line under
  the field, `Only pictures can be sent in a call.` (13 muted), the tray
  stays as it was. A picture larger than 10 MB after preparing (not reachable
  with the 2560 limit, kept as a bound): `This picture is too large to send in
  a call.`
- **While it uploads** the bubble shows at once with the thumbnail at 60%
  opacity and a small spinner; when it is sent, full opacity. If the upload
  fails: the bubble at 60% with `Not sent.` and `Send again`, as for text.

**Why it is not just more text.** A message in the call chat travels on the
call's sealed relay, which carries at most 1024 bytes; a picture does not fit.
It goes the way a picture goes in a conversation: encrypted on the sender's
device with a fresh key (`lakko-attachments`), uploaded to the file store as
an unreadable blob, and the message carries only its address, key, nonce,
size and pixel size, sealed and signed like a text message
(`lakko_calls::call_chat` kind `picture`). Each receiver downloads it at once,
decrypts it and keeps it in memory only.

**What everyone sees.** The picture in the bubble, fitted into at most 240 x
240 keeping its shape, each side at least 80 (a very long or very tall picture
is cropped by the box), no caption (text is its own bubble). While it downloads: a box of the picture's shape on the
sunken fill with a spinner. One that cannot be downloaded or opened: the box
with `Picture not available.` (13 muted). A tap (click) opens it full size over
the call screen on black, closed by a tap, `Esc` or the close button; the
call runs on behind it.

**What a person can do with a picture.** The action sheet (long-press, right
click) has `Save picture` in place of `Copy`: web saves `picture.jpg` through
the browser, the desktop app through its own save dialog, iOS and Android
through the same save the conversation's photo viewer uses. Nothing else.

**What happens to the picture when the call ends** (option 1 of the three
offered on 2026-11-21):

- The sender's device deletes every picture it uploaded in this call when its
  call ends (End, or the call ending), through the same owed delete a message
  deleted for everyone uses (ADR-0065: kept on the device until the file store
  confirms it, retried at the next start).
- Every upload asks the file store for an idle life of one day
  (`x-lakko-blob-idle-days: 1`, ADR-0087 section 13), so the store's daily
  sweep removes it even when the sender never deletes it (a crash, a lost
  connection): once a full day has passed since it was last downloaded, which
  is within three days of the call at most. This is not a 24-hour timer: the
  store keeps days, not hours, and a precise timer would need a store change.
- On every device the decrypted picture lives in memory and goes with the
  call's messages; nothing is written to disk unless the person used
  `Save picture`.
- Someone in the call who had not downloaded a picture when its sender left
  sees `Picture not available.`
- Someone who joins after a picture was sent does not see it, as for text.

**What the file store sees.** That a blob of this size was uploaded and
downloaded by N anonymous passes at these times; never the picture, the call,
or who is in it. Several people downloading one blob within seconds tells the
operator that they are probably in one conversation; the same holds for a
picture in a group conversation today (ADR-0084 records it for profile
photos).

Copy (catalog keys `call.chat.picture_*`): `Only pictures can be sent in a
call.`, `This picture is too large to send in a call.`, `Picture not
available.`, `Save picture`, `Remove picture` (the tray's cross, spoken),
`Picture` (a picture bubble's spoken name; the preview line above the control
bar shows `Anna: Picture`).

### 4.1.5 Camera background

Decided by the maintainer on 2026-11-07: blur, built-in backgrounds and the
person's own photo behind them, chosen in a `Background` section of the Settings
sheet under `Sound`, stored on this device until changed. Specified in
[`call-backgrounds.md`](call-backgrounds.md). Its one rule that touches the rest
of the call: while a background is selected, a camera frame without it is never
sent; if the background fails, the camera is turned off.

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
  their own name on it (the name they typed for this call, else the profile
  name; `You` only when neither is set). The
  status line already carries the count.
- **The user's own tile is never the large one** unless they are alone in the
  call, when their own preview is the only thing there is to show. With others
  present it sits in the grid like everyone else's.
- **Pinning wins over everything.** A pinned tile stays large whether or not it
  is sending video, until the user unpins it; that is what a pin is for.
- **The grid keeps roster order** and never reorders on its own. When the person
  in the large tile stops sending video, the next one sending video takes the
  slot and they drop into the grid where their roster position puts them.
- **The layout of the tiles is `group-call-grid.md`**: a grid of equal tiles
  that fills the screen without scrolling, or one large tile with the others in
  a row above it. The paragraphs below about the large tile describe the rule
  it replaced where the two disagree.
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
  always shows a name. One rule on every client and for both kinds of call, in
  this order (step 0 added 2026-11-07, maintainer):
  0. the name the person typed for this call (section 4.1, Settings; a guest's
     name from the link page), as every device received it. Once a person has
     typed one, every device shows it, whatever that device calls them
     otherwise. Saving an empty name withdraws it and the steps below apply
     again;
  1. what the one resolver of [`profile-names.md`](profile-names.md) calls the
     person behind the tile: the viewer's own name for a contact, then the
     person's profile name, then their verified `@username`;
  2. the display name the person announced inside the call (link calls), for
     the moment before the device has filed their profile;
  3. the username the person claims while this device has not verified it
     yet, as plain text without `@`. The `@` form is shown only for a verified
     username; a bare word claims nothing a typed display name could not.

  There is no `Participant 2` and no other made-up name. Everyone has
  something to announce: an account has a username, and a guest has the name
  typed on the link page. A tile is without a name only for the moments
  before that announcement reaches the device, and then it shows the call
  screen's plain unnamed state, with no number.

  A tile never shows a piece of an account id or of a key. A tile labelled
  with eight hex characters (`01a0a045`) named nobody, and it is what a person
  who had set no profile name was shown as in a link call until 2026-10-02:
  the tile read only the profile name and fell back to the start of the
  account id the server reports.

  How the device learns who is behind a tile. In a chat group call the group
  already carries everyone's profile. In a link call the joiners are strangers
  to each other and a guest has no contact list, so each participant seals its
  own profile into the call's own group and every member relays it once it
  has joined and again whenever someone new is admitted (the newcomer can only
  read from that moment on). The server forwards ciphertext; a name never
  crosses it in clear. An announcement that arrives before the newcomer can
  read it (it can outrun the admission) is kept and read right after, not
  dropped.

  The receiver files an announced profile under the person who sealed it,
  exactly as it files a profile received in a chat, which is what checks a
  claimed username and what lets the viewer's own name for a contact apply.
  It does so only when the account the profile claims is the account the
  server admitted that participant to the call as; otherwise the profile is
  not kept and the tile shows the announced name as plain text. The tiles are
  named again whenever the stored profiles change.

  The name typed for the call (step 0) travels the same way and only that
  way: signed with the person's MLS leaf key, sealed under the call's relay
  key, never to a server in clear and never into the profile store. Each
  participant re-sends it whenever it re-announces itself (someone joins, a
  rekey, the periodic announcement), so a person who joins later sees it too.
  A device takes it only from the peer whose verified announcement named that
  leaf key, and keeps the newest one per peer (a number that grows with each
  change), so an old name re-sent late cannot replace a newer one.
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

### 4.5 Putting the call away without ending it

A call that owns the whole screen owns the whole app. A person on a call has to be able
to look up the address they are being asked for, read the message that just arrived and
answer the one question the call is about, without hanging up. The first answer to that
was a bar across the top of the app. It kept the call reachable but hid every face: a
person who put a video call away to look something up could no longer see who was
talking. Every video-calling app people already use answers with a small window that
keeps the picture, and since 2026-11-09 so does this one (maintainer decision: the window
always, for voice calls too; the top bar is removed on all three clients).

**A live call collapses into one small window that floats over the app.** It sits above
whatever screen is showing and survives every navigation: another chat, Contacts, Calls,
Settings. The call is owned by the app-level host and not by a screen, so collapsing
changes nothing about the call itself: the media, the keys and the timer are untouched,
only the picture gets smaller.

#### 4.5.1 What the window shows

- **The picture is the person the call screen would put on the stage**, chosen by the
  same rule (`call_stage`, section 4.4): the pinned person, else a screen being shared,
  else whoever is speaking, else the most recent speaker, and so on. In a one-to-one call
  that is the other person. It is their camera when it is sending, else their avatar on
  the call surface, drawn exactly as their tile draws it. A person alone in a call sees
  themselves, as on the call screen.
- **Your own camera, while it sends, is a small inset** in the bottom corner of the
  window away from the screen edge it sits on, mirrored as on the call screen. It is the
  reminder that a collapsed video call is still a camera, so no separate camera glyph
  is needed. No inset when your camera is off, and none when the picture already is you.
- **At the bottom, the name of the person shown**, from the same label their tile uses,
  and in a group call `+N` for everyone else beyond that person and you (`+16` with 18
  people).
- **At the top left, the state or the running time**, in the words of the status line
  and never two at once: `Calling...`, `Connecting...`, `Reconnecting...`, `12:04`. The
  call's label (the one the call screen uses) is not printed in the window: the picture
  and the name say who it is with, and the label stays the window's spoken name.
- **A small lock beside the time, only when the call screen would show its chip.** The
  window never carries the word.
- **One round mute button at the top right**, drawn on and off exactly as the call screen
  draws it.

**Tapping the window anywhere but the mute button brings the call back.**

**There is no End on the window.** Ending is the one irreversible control in a call; the
call screen sets it apart and paints it the only red on the screen, and a small window a
thumb crosses on the way to a chat is the wrong place for it. Coming back costs one tap.

**The window asks for the picture at the size it draws it.** While collapsed, the client
tells the SFU it wants the shown person only, at the quality `tile_video_quality` gives
for the window's width (section 4.4: under 240 px that is the light picture), and asks
nothing more for anyone else, who then arrive only as the light picture the SFU always
forwards. A collapsed group call therefore costs about what a voice call costs
plus one small picture. When the shown person changes, the wants change with it.

#### 4.5.2 Size and place

| | Shape | Size |
|---|---|---|
| Phone (the narrow side of the screen under 600 points) | 3:4, upright | 36% of the screen width, at least 120 points |
| Tablet, desktop browser, a wide web window | 16:9, lying | 320 points wide |
| Any client, while the picture is a screen share | 16:9 | the width above |

Corners are rounded with `--radius-md` (12) and the window carries `--shadow-2`, the
dialog shadow, because it floats over content the same way. The call surface colour
fills it behind an avatar; it does not follow the light theme, as the call screen does
not.

**It starts in the top right corner**, 16 points in from the safe edges and below the
screen's own header, so it covers neither the composer nor the send button.

**It can be dragged anywhere and lands in the nearest corner.** On release it moves to
the closest of the four corners, measured from where it would come to rest given how
fast it was moving, so a flick towards a corner reaches it. The corner it lands in is
kept while the app runs and is where the next collapsed call opens.

**On a phone, a flick past the left or right edge tucks it away.** Only a strip with a
chevron stays at that edge, as the system's own picture-in-picture does; the call goes on
and the mute state is unchanged. Tapping the strip, or dragging it back, returns the
window to the corner it came from. Web has no tuck: a pointer does not throw.

**It never covers the system bars or the software keyboard.** When the keyboard opens and
the window sits in a bottom corner, it moves up with the keyboard and goes back when the
keyboard closes.

Where a window lands is a fact about one device and one hand, never shared with another
device or person, so each client writes its own placement in its own gesture system; the
numbers above are the rule all three follow. What is shown in the window is shared
information and comes from the shared rules (`call_stage`, `tile_video_quality`).

#### 4.5.3 Collapsing and coming back

| | Collapse | Expand |
|---|---|---|
| iOS | a chevron at the top left of the call screen, or a downward swipe on the screen | tap the window |
| Android | the same chevron, or the system back, which collapses and never ends | tap the window |
| Web | the same chevron, or `Esc` | click the window, or `Enter` when it has focus |

Android's back collapses: a call is not a dialog to dismiss, and back has never meant
hang up anywhere else in the app.

**The window is never shown over the call screen.** One state, one place.

**A call that is only ringing does not collapse.** An incoming call is answered or
declined, and a muted conversation's call is already offered without covering anything
(section 2.1.1). An outgoing call collapses from the moment it is placed, and the window
shows the person called, their avatar and `Calling...`.

**A guest who joined from a link has no app to collapse into.** The guest root shows the
call and nothing else (section 9 of `call-links.md`), so the chevron is absent there; the
system window of 4.5.4 still applies.

**When a collapsed call ends, the window goes away and the conversation gets its usual
row.** Nothing is thrown across the screen: the `Call ended` overlay belongs to the call
screen, and a person who put the call away asked not to be interrupted by it. A call that
ends badly is not silent either way: the row and, when it was missed, the notification
say so exactly as they do now.

#### 4.5.4 Leaving the app during a call: the system window (iOS, Android)

**When the app leaves the screen during a call** (home, the app switcher, another app,
following a link out), whether the call was full screen or collapsed, the same picture
goes into the system's own floating window, over the home screen and other apps. It shows
what 4.5.1 shows: the stage person, your own inset while your camera sends, the name and
`+N`. Coming back into the app returns the call exactly as it was left, full screen or
collapsed. A call that is only ringing has no system window, the same as it does not
collapse.

| | How it opens | Its controls |
|---|---|---|
| iOS | by itself when the app goes to the background, through the system's video-call picture-in-picture | the system's own: back to the app, close. Mute is in the system call controls the call already has (lock screen, the call indicator), not in the window |
| Android | by itself on Android 12 and later; on 8 to 11 when the person leaves the app with home or a switch | one action, `Mute` / `Unmute`, drawn on and off. No End, for the reason in 4.5.1 |

**Closing the system window never ends the call.** The call goes on with sound; the
system's call indicator (iOS) or the ongoing-call notification (Android) brings it back.

**Your own camera while the app is not on the screen.** iOS stops the camera of an app
that is not in front unless the app is allowed to keep it (multitasking camera access,
which Apple grants per app). Without it, the moment iOS interrupts the camera the client
tells the call your camera is off, so the others see your avatar and not a frozen frame,
and turns it back on by itself when you return; the camera button on your screen keeps
saying on, because you did not turn it off. Android keeps the camera running while its
window is on screen. Neither client ever shows the others a still picture of you as if it
were live.

**A voice call gets the system window too**, showing the stage person's avatar, because
the maintainer chose the window for every call. On iOS this rests on the system drawing
an avatar view in its call window; it has not been tried on a device, and if it does not
draw, a voice call on iOS has no system window and goes on with sound and the call
indicator only.

#### 4.5.4a The desktop app: the window is a window of its own

Added 2026-11-10 (maintainer's decision). In the desktop app the collapsed call is not
drawn inside the app's window: it is **a separate small window of the operating system**,
without a frame or title bar, that stays above every other program's windows. So the call
keeps its picture while the person works in another program or minimises Lakko, which is
what a desktop user collapses a call for.

- It shows exactly what 4.5.1 shows (the stage person, your own inset while your camera
  sends, the name and `+N`, the state or running time with the lock, the mute button) at
  the size of 4.5.2 for a desktop (16:9, 320 points wide), with the same corners and
  shadow.
- It opens in the top right corner of the screen the app's window is on, 16 points in
  from the edges of that screen's work area (never under the menu bar or the taskbar).
  It can be dragged anywhere on any screen and stays where it is dropped (no snapping to
  a corner: other programs' windows are under it, not the app's own layout). Where it was
  left is kept while the app runs and is where the next collapsed call opens.
- **Clicking it anywhere but the mute button** brings the app's window to the front (out
  of the Dock or taskbar if it was minimised) and returns the call to full screen, as
  clicking the in-app window does. The mute button mutes and does nothing else.
- It is never shown at the same time as the call screen, and the in-app floating window
  of 4.5.1 is never drawn in the desktop app: one state, one place. It does not appear in
  the taskbar or the window list of its own, and it has no close button (no End, 4.5.1);
  quitting the app ends the call as it does today.
- When the call ends, the window goes away and the app's window is not brought forward;
  the conversation gets its usual row (4.5.3).
- The picture in it is the same picture the app receives, handed to the window inside
  the app; nothing new leaves the device.

Web in a browser keeps the in-app floating window of 4.5.1 to 4.5.3.

#### 4.5.5 The recorder stays blocked

A live call holds the microphone, so the composer records neither a voice message nor a
video note while one is running, collapsed or not. The disabled recorder says why when it
is pressed, in one line. The fact behind the rule is "a call is live", not "a call is live
in this chat": a person can stand in chat B with a call running from chat A.

#### 4.5.6 What each client has to expose

The call lives in an app-level host on all three (`CallHostContext` on web,
`CallHostOverlay` on iOS, `HostedCallOverlays` and `OneToOneCallOverlay` on Android), so
the window needs no new owner:

| Need | Why |
|---|---|
| The shared `call_bar` rule (`crates/lakko-ui-rules/src/call_bar.rs`), kept under its name: whether the window is on screen and which one thing its status line says | Written once; the window replaces the bar's picture, not its decisions |
| `collapsed` on the call host, with an expand and a collapse (kept from the bar) | The presentation mode is a property of the call, not of a screen; a route change must never touch it |
| The stage choice, the shown person's track or avatar, the own camera track, the people count, phase, elapsed seconds, mic state and whether encryption is armed | The window shows a subset of the call screen and reads it from the same state, so the two can never disagree |
| The window's width fed to the same `video-wants` path the grid uses | 4.5.1: one small picture, not the grid's |
| iOS: a video-call picture-in-picture controller whose view draws the remote picture from frames (a sample-buffer layer), started automatically from the call host | 4.5.4 |
| iOS: the camera interruption mapped to the camera-off signal, and back | 4.5.4 |
| Android: `supportsPictureInPicture` on the activity that hosts calls, auto-enter parameters while a call is live, a compact layout in picture-in-picture mode, and the mute action | 4.5.4 |
| A collapse reachable without the pointer | Keyboard on web, the back gesture on Android, the swipe on iOS |

**Spoken.** The window is one button whose label names who is on the call and how long it
has been running (`Call with Anna, 12 minutes 4 seconds, return to call`), with the mute
button separate and naming the action and its result. State changes announce as a live
region, so `Reconnecting...` is heard and not only seen. The tucked strip is a button
named `Return to call`.

## 5. The safety number

Today the call safety number is printed across the top of the call as sixteen digits.
It is unreadable, it means nothing to anyone who has not been told what it is, and it
occupies the most valuable line on the screen.

**New rule: the code is never shown unless the user asks for it.**

- On the call, encryption is one chip: a lock and the word `Encrypted`.
- Tapping the chip opens the **encryption sheet**: a title `This call is encrypted`, one
  sentence in plain words, the safety number in four groups of four digits, large and
  grouped for reading aloud, and one line telling the user what it is for. Buttons:
  **Copy** and **Done**.
- The sentence and the line depend on the kind of call, never on who happens to be
  first in the roster:

  | Call | Sentence | Line under the number |
  |---|---|---|
  | One-to-one, the other person has a name on this device | `Only you and Anna can hear this call.` | `Read the number aloud to each other. If it matches on both devices, no one else is listening.` |
  | One-to-one, no name for the other person | `Only the people on this call can hear it.` | the one-to-one line above |
  | Group call or call from a link, whatever the number of people in it | `Only the people on this call can hear it.` | `Everyone in this call sees the same number. Read it aloud. If it matches on every device, no one else is listening.` |

  A group or link call never names one person: every participant compares the same
  number, and naming one of them claims the call has two people. A call is treated as
  a group call as soon as more than one other person is in it, even if it was placed as
  a one-to-one. The copy says "device", not "phone": people join from computers and
  browsers too.
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
| A collapsed presentation on the app-level call host, and the capture fact widened to "a call is live" | Section 4.5 — the app stays usable during a call, and a chat other than the calling one must still refuse to record |
| A call-chat payload on the sealed call relay | Section 4.1.4 — the chat in a group call |

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
4.4, a call window outside the browser tab (Document Picture-in-Picture, Chrome
and Edge only; the web window of section 4.5 stays inside the page), call recording, call links
(they keep the behaviour of [ADR-0032](../adr/0032-multi-joiner-call-links.md)),
and call history sync between devices.
