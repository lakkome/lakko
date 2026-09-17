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

- The chat header has two buttons: call and video call. Each starts that kind of call,
  and the kind the caller chose is what the callee is told.
- **A voice call never turns the camera on.** The pre-join screen opens with the camera
  off for a voice call and on for a video call, and the toggles there change the call
  that is actually placed.
- Pre-join stays as it is otherwise: a preview, microphone and camera toggles, **Join**
  and **Cancel**. It is skipped entirely when accepting an incoming call.
- While the callee's phone is ringing, the caller sees the same call screen with
  `Ringing...`, and can cancel. After 45 seconds it becomes `No answer`, with **Call
  again** and **Message** on the ended screen.

## 4. The call screen

Layout, in order from the top:

1. **Status line**: the other person's name, and under it the call state in one word or
   the running time (`Ringing...`, `Connecting...`, `12:04`).
2. **A single small lock chip** next to the status line: a lock icon and the word
   `Encrypted`, on a sunken surface. It appears **only** when the call's own encryption
   is armed. Tapping it opens the encryption sheet (section 5). That chip replaces the
   strip of digits shown today.
3. **The stage**: the participant tile or tiles, exactly as they are now for group calls.
4. **Controls** along the bottom: microphone, camera, more, end call, in that order, with
   the end-call button in danger red.

States on that screen: ringing, connecting, connected, reconnecting, ended, and failed.

| State | What the user sees |
|---|---|
| Ringing | `Ringing...`, only the cancel (end) button is active |
| Connecting | `Connecting...`, controls active |
| Connected | The running time, the lock chip if encryption is armed |
| Reconnecting | `Reconnecting...` over the stage, audio muted until it returns |
| Could not connect | `Couldn't connect` with one sentence of plain cause (`Check your connection`), plus **Call again** and **Close** |
| Ended | `Call ended` and the duration, then the screen closes by itself after 2 s |

Connection quality stays as it is: a real measurement or nothing at all.

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

Group call redesign beyond the shared call screen, call recording, call links
(they keep the behaviour of [ADR-0032](../adr/0032-multi-joiner-call-links.md)),
screen sharing, and call history sync between devices.
