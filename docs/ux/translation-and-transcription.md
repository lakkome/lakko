<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: translating messages and transcribing voice messages

Status: accepted with ADR-0082, 2026-10-25. Decisions taken by the maintainer
that day: on the device only with no cloud path; for v1 no sync between the
account's devices, no sender-attached transcript, language packs from the
platform. Architecture:
[ADR-0082](../adr/0082-on-device-translation-and-transcription.md).

Two features people know from other messengers: read a message written in a
language you do not read, and read a voice message instead of listening to it.
Other messengers send the text or the audio to a server to do this. Lakko does
both **on the device that shows the message, after it has been decrypted**, and
has no other way to do them. Where the device cannot, the feature is absent,
never quietly done somewhere else.

This changes [`media-messages.md`](media-messages.md) section 9 (transcribing
voice messages was out of scope for v1) and adds one row to the message menu of
[`message-actions.md`](message-actions.md).

## 1. User goals and entry points

- **Read a message in a language I do not read.** Entry points: `Translate` in
  the message menu; a bar at the top of a conversation whose recent messages
  are in another language; automatic translation turned on for that
  conversation.
- **Read a voice message or a video message without playing it.** Entry point:
  a transcript button on the voice-message row and on the video-message bubble.
- **Decide what my device keeps for this.** Entry point: Settings > Language >
  Translation and transcription.

Not goals of v1, and why, in section 10: translating what I am about to send,
live captions in a call, summaries of a conversation.

## 2. The rule every state follows

**Only on this device.** Nothing a person wrote or said is sent anywhere to be
translated or transcribed, not to Lakko's servers and not to anyone else's. If
this device has no way to do it for this language, the control is not offered.
There is no "try online" button, no fallback, no setting that turns one on.

**Machine output is marked as such, always.** A translation or a transcript is
the device's guess. It is drawn so that nobody can mistake it for what the
sender wrote, it is never sent to anyone as if a person wrote it, and the
original is one tap away.

**It stays on this device.** Translations and transcripts are not sent to the
other people in the conversation and not to the person's other devices (open
question 1). They are deleted when the message is deleted, and redone when the
message is edited.

## 3. Translating a message

### 3.1 The action

`Translate` is a row in the message menu, in the group of actions that only
read (after Copy). It is offered when all of these hold:

- the message has text (a caption counts; a voice message's transcript counts
  once it exists);
- the message is not the person's own;
- this device can translate from the message's language into the person's
  language (section 7).

When the device could not tell the language, the row is still offered if it can
translate at all. Choosing it translates at once from the detector's best
guess, even an unsure one, and the quiet line carries `Not Dutch?` (3.4) to
correct it. The source, in this order: a language chosen for this message in
the picker; a confident detection; the conversation's language when the
detection is unsure and lists it among its close candidates (below); the
detector's best guess, even an unsure one, when it is usable; its next close
candidate this device can translate from; the language the person chose for
this conversation (3.4); otherwise the picker. A guess is not usable when it is the language
translated into or one this device cannot translate from: neither line could
be corrected from there, so the conversation's language is used instead, and
without one the picker opens with the guess marked. The picker also opens
first when there is no guess at all (no letters) and no language chosen for
the conversation. The conversation's language comes after an unsure guess
because one pick in a chat must not decide every later message the detector is
unsure about: found live on 2026-11-15, a chat where Dutch was once picked had
a Ukrainian message (`uk` at 0.67, under the confident line) translated from
Dutch. Built on web, iOS and Android (2026-11-15), host-tested only.

Every client detects the language on the device: iOS with the system detector,
Android and a browser without its own detector model with Lakko's own (ADR-0082
note of 2026-11-15; until then Android always opened the picker). On Android
and the web about half of one-sentence messages get a sure answer (more for
Polish or German, fewer for Dutch or Italian, as measured in the ADR); the
rest, and short ones ("ok", "hola"), stay unsure and are translated from the
guess as above. Chinese stays unsure there too: Lakko's detector cannot tell
Simplified from Traditional.

Choosing it replaces the text in the bubble with the translation (3.2). On a
message already showing a translation the row reads `Show original`.

The language is also named up front: an incoming message whose detected
language is not one of the person's reading languages shows the quiet line
`Spanish · Translate` under its text before anything is translated (3.2). The
same rule as the row decides it (not own, has text, the setting on, not a
reading language, and an unsure guess that is a reading language gets no line
either, so a short "ok" stays clean). The language named is always the one
`Translate` would start from, decided once for both by the order above, so
the line and the tap never disagree: for "hola, qué tal?", an unsure Catalan
with Spanish, French and Italian close (below) on a device that cannot
translate from Catalan, the line reads `Spanish · Translate` and the tap
translates from Spanish, which `Not Spanish?` corrects (seen on a Pixel with
no line at all before 2026-11-15). When the detection is sure and this device
cannot translate from its language, the line shows the name alone, with no
`Translate` (`Turkmen`; never relabelled). The only message with no line is
one whose detection is unsure and none of whose close candidates this device
can translate from (seen on a Pixel: "goedendag" named Turkmen).

A short message is often a near tie: "hola, qué tal?" scores Catalan, Spanish,
French and Italian within a few hundredths of each other. So an unsure
detection carries up to three close candidates beside its best guess (ADR-0082
note of 2026-11-15). The conversation's languages decide among those
candidates: a language the person picked here (`Not X?` or the picker) wins,
the most recently picked first; otherwise the candidate with the most
messages in this conversation the detector was sure of; a tie, or none, goes
in the detector's order. The result is used on the line, as the `Translate`
source and as the `X` of `Not X?`, and it is the same however the messages
were loaded or scrolled: "hola, qué tal?" in a chat with one sure Spanish and
one sure Italian message reads Spanish on every launch. It never replaces a
sure detection and never names a language outside the candidates, so one
Spanish chat does not turn a Ukrainian message Spanish. Built on web, iOS and Android
(2026-11-15), unit-tested only.
`Translate` on the line does exactly what the row does, in the order above.

### 3.2 A translated bubble

The translation takes the place of the text inside the same bubble. Under it,
one quiet line in the bubble's secondary text colour:

`Translated from Spanish · Show original`

`Show original` is a button; it puts the original back and the line becomes
`Spanish · Translate`. The same line is shown before any translation on a
message in a language the person does not read (3.1), and a translation,
chosen or automatic (3.7), replaces it with the line above. The language name is in the interface language. Nothing
else in the bubble changes: author, time, reactions, reply quote, attachments
stay where they were.

The bubble keeps its width rule (65% of the pane) and grows or shrinks in
height with the text; that is expected, the text changed.

Other actions on a translated bubble act on the **original**, with one
exception:

| Action | Uses |
|---|---|
| Copy | the text shown (translation while it is shown), because Copy is "the text I see" |
| Reply | the original: the quote snapshot is what the sender wrote |
| Forward | the original, always. A machine translation is never sent as a message |
| Save | the original |
| Select | the original |

### 3.3 While translating, and when it fails

| State | UI |
|---|---|
| Translating | The text stays as it was and the quiet line reads `Translating…`. No spinner over the text |
| Translated | 3.2 |
| Language pack needed | 3.5. Nothing is downloaded without the person choosing to |
| Failed | The line reads `Couldn't translate this message · Try again`. The original stays |
| Same language | The line reads `This message is already in English`, then disappears after a few seconds. Happens when detection was wrong or absent |
| Language not supported | The row is not offered (section 7). If the picker of 3.4 led here: `Lakko can't translate from Tagalog on this device` |

### 3.4 Choosing the language

When detection is unsure or wrong, the quiet line under a translation and the
translating state carry `Not Spanish?`, which opens a short list of the
languages this device can translate from, own names as titles (the rule of
[`localization.md`](localization.md) section 2). The detector's other close
candidates for this message (3.1) come first, then every other language in the
usual order. The choice applies to this message and is kept for the
conversation, where it decides later near ties (3.1).

### 3.5 Language packs

Some devices translate a language only after a pack for it is on the device.
The first time a message needs a pack, a dialog says:

> **Download Spanish for translation?**
> Translation happens on this device. Lakko needs the Spanish pack to do it
> (size). Messages are never sent anywhere to be translated.
> `Not now` · `Download`

The size is the real size the device reports; if the device does not report
one, the parenthesis is left out rather than guessed. Whether the download
itself can reveal which language was asked for, and what the dialog then says,
is decided in the ADR (decision 4); the dialog must not claim more privacy than
the download has.

While anti-censorship is on, no pack is downloaded (the download would leave
the device outside the protected connection): the dialog is replaced by one
line, `Language packs can't be downloaded while anti-censorship is on`, and
packs already on the device keep working.

While downloading, the quiet line reads `Downloading Spanish… 40%` and the
action can be cancelled from it. A pack is downloaded only on Wi-Fi unless the
person allows mobile data in settings (section 6); on mobile data without that
permission the dialog's button reads `Download on Wi-Fi` and the download waits.

A line never stays on `Downloading Spanish…` with nothing watching it. When
the device only accepts the request and fetches the pack later, the line goes
back to `Spanish · Translate` (the transcript closes); the next `Translate`
asks the device again and translates once the pack is there (ADR-0082 note of
2026-11-15). On Android the pack of a pair the system service cannot supply
comes from ML Kit, so the dialog there says `The pack comes from Google.`

In the desktop app on macOS a translation pack is downloaded by macOS in its
own settings, never by Lakko (ADR-0082 note of 2026-11-18). The dialog keeps
its title and says where:

> **Download Spanish for translation?**
> Translation happens on this device. macOS downloads the Spanish pack in
> System Settings: Language & Region, then Translation Languages. Messages
> are never sent anywhere to be translated.
> `Not now` · `Open System Settings`

`Open System Settings` opens Language & Region. Nothing waits on it: the line
goes back to `Spanish · Translate`, and the next `Translate` asks macOS again.
The names are the ones macOS shows in the interface language. A transcription
pack on macOS downloads in the app, as on iOS. The desktop app can arm
anti-censorship, so the line `Language packs can't be downloaded while
anti-censorship is on` applies there too, for both kinds of pack.

### 3.6 The conversation bar

When most of the recent incoming messages in a conversation are in a language
the person does not read and this device can translate it, a banner appears at
the top of the conversation:

`Messages here are in Spanish.` `Translate` · `Not now` · close

- `Translate` turns on automatic translation for this conversation (3.7).
- `Not now` and close hide the bar for this conversation; it does not come back
  for that language. A different language may bring it back.

"Most of the recent" is decided by one shared rule (ADR, data section). The bar
is never shown for a language in the person's reading list (section 6).

### 3.7 Automatic translation in a conversation

A per-conversation switch, `Translate automatically`, in the conversation's
info screen ([`conversation-info.md`](conversation-info.md)) and turned on by
the bar. While on, every incoming message in another language is translated as
it is shown, each with its quiet line, each with `Show original`. Messages that
are in a reading language are left alone. Turning it off shows every message in
its original again.

It is a setting of this device only, like the translations themselves (open
question 1).

## 4. Transcribing a voice message or a video message

### 4.1 The control

On the voice-message row, a transcript button sits at the trailing end of the
row, beside the speed control's reserved space: a small `Aa` glyph button,
accessibility label `Show transcript`. On a video message it sits on the
bubble's trailing bottom edge.

The row geometry rule of [`media-messages.md`](media-messages.md) section 2
holds: the button's space is part of the row in every state, so the row does not
move when the button appears, changes state or disappears. The transcript
itself opens **below** the row, inside the same bubble (4.2); the row keeps its
height.

The button is offered on incoming **and** own voice messages (a person may want
to check what the other side reads). It is not offered when this device cannot
transcribe (section 7); the space stays reserved and empty.

### 4.2 The transcript

Tapping the button transcribes the message and opens the transcript under the
row, in the bubble's text style. Under the transcript, one quiet line:

`Transcript · Hide`

Tapping the button again, or `Hide`, closes it. A transcript once made is kept
(section 2) and opens instantly the next time.

The transcript is text like any other for the menu: Copy copies it, Translate
(3.1) translates it, and a translated transcript carries both labels:
`Transcript · Translated from Spanish · Show original`.

The audio does not have to be downloaded and played first; transcribing fetches
and decrypts the file the same way playing does, and never transcribes partial
or unauthenticated bytes (the media-messages rule).

### 4.3 States

| State | Button | Under the row |
|---|---|---|
| Not transcribed | `Aa` | nothing |
| Downloading the audio | progress ring on `Aa` | nothing |
| Transcribing | `Aa` pulses | `Transcribing…`, then the text as it arrives if the device produces partial results |
| Transcript | `Aa` selected | the transcript + `Transcript · Hide` |
| Nothing heard | `Aa` selected | `No speech in this message` |
| Language pack needed | `Aa` | the 3.5 dialog, worded for transcription |
| Failed | `Aa` | `Couldn't transcribe this message · Try again` |
| File unavailable | `Aa` disabled | the media-messages state for an unavailable file; no transcript is possible |

A long message is transcribed in full; the transcript shows the first lines
with `Show more`. Playback and transcription are independent: a person can play
while transcribing.

### 4.4 Which language

The device transcribes in the language it detects, if it can; otherwise in the
language last chosen for this conversation; otherwise in the interface
language. `Not Spanish?` under a transcript opens the 3.4 picker, with the
languages this device can transcribe.

## 5. What is never translated or transcribed

- Notification previews. A notification shows what the message says, never a
  translation, and nothing runs in the background to produce one.
- Names, `@username`, group and channel names, links, the safety number, the
  recovery phrase.
- Messages a person wrote themselves (translation; transcription of own voice
  messages is allowed, 4.1).
- Calls. Live captions are out of scope (section 10).

## 6. Settings > Language > Translation and transcription

A sub-screen of Settings > Language (the IA of the design system, section 4):

- **Show Translate button** (on by default). Off removes the menu row and the
  conversation bar.
- **Languages I read**: the interface language plus any others the person adds.
  Never offered for translation, never trigger the bar.
- **Translate into**: the interface language by default.
- **Show transcript button** (on by default).
- **Download packs on mobile data** (off by default). Shown only where Lakko
  itself starts a pack download and the device can tell a mobile connection
  from another: iOS, Android, and a browser that reports its connection type
  (Chrome on Android). Not shown in the desktop app (macOS fetches a
  translation pack in System Settings, and the app cannot tell a mobile
  connection) nor in a browser that reports no connection type: there the
  switch would govern nothing (amended 2026-11-18).
- **Language packs on this device**: each pack with its size, for translation,
  transcription or both, with `Remove`. Only packs Lakko itself downloaded are
  listed; packs the operating system manages are listed with `Managed by
  iOS` / `Managed by Android` and no Remove. In the desktop app on macOS the
  list also holds the languages macOS reports ready on this Mac (translation
  into the target, and the installed speech models), each `Managed by your
  system` with no Remove, as iOS lists Apple's packs (amended 2026-11-18).
- **Delete all translations and transcripts**: removes every one stored on this
  device. A confirmation dialog says how many.
- One paragraph at the bottom: "Translation and transcription happen on this
  device. Messages and voice recordings are never sent anywhere to be
  translated or transcribed."

When this device cannot do one of the two at all, its rows are replaced by one
line, e.g. `Transcription isn't available on this device` (web in a
browser: `in this browser`; the desktop app uses the device wording), with no
action. A pack the system manages reads `Managed by your browser` in a browser
and `Managed by your system` in the desktop app. The desktop app on macOS 26.4 and
later translates and transcribes with Apple's on-device engines (section 7,
case 1 or 2); on Windows and Linux, and on older macOS, it shows the
`isn't available on this device` lines until Lakko's own engine exists.

These settings are per device and never reach a server, like the interface
language ([`localization.md`](localization.md) section 4).

## 7. When the device cannot

What "can" means per platform is the ADR's. The UX has three cases:

1. **Can, now.** The control is offered and works.
2. **Can, after a pack.** The control is offered; choosing it leads to 3.5.
3. **Cannot.** The control is not offered for that language, or at all, and
   Settings says so (section 6). There is no message saying "available in the
   app" or "available online".

On web, case 3 is expected in some browsers; the web client does not pretend a
browser has an engine it has not.

## 8. Data the UI needs (input for the ADR)

- Per device and per message: the translation (text, source language, target
  language), the transcript (text, language, complete or partial), keyed so an
  edit invalidates them and a deletion removes them.
- The detected language of a message's text, and a confidence the bar rule can
  use; the rule for "most of the recent messages", written once for the three
  clients.
- Per conversation: automatic translation on/off, the chosen source language
  for translation and for transcription, the languages for which the bar was
  dismissed.
- Per device: the section 6 settings.
- Per device: which languages can be translated from and to, which can be
  transcribed, which need a pack, the pack sizes when known.
- Download state of a pack (queued for Wi-Fi, progress, failed).
- No field on the wire. Nothing in the message envelope changes for v1.

## 9. Cross-platform shape

One model on the three clients: the same menu row, the same quiet line under a
translated bubble, the same `Aa` button and transcript block, the same settings
screen. The engines differ by platform (ADR) and that difference shows only as
section 7's case 2 or 3, never as different controls. The menu opening gesture
and the card look are those of [`message-actions.md`](message-actions.md).

## 10. Out of scope (v1)

- **Translating before sending.** Sending a machine translation puts words in
  the person's mouth that they cannot read; it needs its own design (preview,
  label for the recipient).
- **Live captions in calls.** Same engines, different latency and a different
  surface; later, native first.
- **Summaries of a conversation.** An on-device language model is a separate
  decision.
- **A transcript added by the sender** for every recipient (open question 2).

## 11. Open questions

Both were decided for v1 on 2026-10-25 by the maintainer: no to each. They
stay here as the questions to revisit after v1.

1. Should translations, transcripts and the per-conversation switch follow the
   person to their other devices through the account's own synced state? That
   is encrypted and never reaches a server in the clear, but it carries the
   text of the messages a second time. v1 says no.
2. Should the sender be able to attach a transcript to a voice message, so that
   recipients without an engine can read it? It would be the sender's claim
   about their own audio and would need its own label.

## 12. Copy checklist

No protocol or model names in the UI ("speech recognition model", "neural",
the names of the platform frameworks). "On this device" is the phrase. The
privacy sentence appears in the pack dialog and at the bottom of settings, and
nowhere else.
