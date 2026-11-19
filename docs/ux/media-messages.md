<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec — voice messages, video notes, emoji picker, GIFs

**Status:** design, the input to the media-message ADR and the three client builds.
**Owner surface:** cross-platform chat composer and message timeline (web SolidJS /
iOS SwiftUI / Android Compose), 1x1 and group chats alike.
**Pattern parents:** the maintainer's previous app (`lakko_me`) — one picker with tabs,
hold-to-record voice, `voice_note` / `video_note` message kinds, GIPHY behind the
server — and the existing Lakko attachment pipeline (encrypt, upload blob, reference it in
the chat envelope).

## 1. User goals and entry points

| Goal | Entry point |
|---|---|
| Say something quickly without typing | Hold the **mic** button in the composer |
| Send a short face-to-camera clip | Switch the mic button to **camera** (tap it), then hold |
| Put an emoji in the text | The **smiley** button left of the text field opens the picker on the Emoji tab |
| React with any emoji, not only the six | The reaction bar's **+** opens the same picker in reaction mode |
| Send a GIF | The picker's **GIF** tab |

The composer's right-hand button follows the established messenger convention: **Send**
while the text field or attachment tray is non-empty, otherwise **mic** (or **camera**,
after a tap toggles the mode). The chosen mode is remembered per device.

## 2. Voice messages

### Flow

1. **Press and hold** mic → recording starts as soon as the press is a hold rather than
   a tap (the threshold of section 3.1), after permission (§6). Haptic tick on native.
2. While holding, the composer is replaced by the **recording bar**: a pulsing red dot,
   the elapsed time `0:07`, a live level meter, and the hint `Slide to cancel`.

   **The recorder is shown as running only once it is running.** Opening a microphone
   takes real time even on a device whose permission was granted long ago, and a person
   who sees a red dot and a counter starts talking - so a bar that runs across that gap
   loses the beginning of every message. The bar appears at the hold, says `Starting…`
   while the device opens, and the dot, the clock and the elapsed time all begin at the
   first captured sample. The duration the message carries and the waveform drawn on it
   describe that same span, never the wait in front of it.
3. **Release** → the message is sent.
4. **Slide left past the threshold** → cancelled, nothing sent, a short "Cancelled" shake.
5. **Slide up** onto the lock target → **locked**: the finger can lift, recording
   continues, the bar shows **Stop** and **Delete**. Stop → preview state; Delete → discard.
6. **Preview state** (only after lock + stop): play/pause the take, a waveform, duration,
   **Delete** and **Send**. No trimming in v1.
7. Hard cap **15 minutes**; at the cap recording stops and moves to preview.
8. A take shorter than **0.5 s** is discarded silently (an accidental tap), with a
   one-line hint `Hold to record`.

Web has no touch-hold on desktop: **mouse-down and hold** works the same way, and the
lock and cancel targets appear under the pointer exactly as they do under a thumb, so a
mouse reaches the whole gesture. A plain **click** does not record - it switches the
mode (section 3.1). `Esc` cancels.

### Timeline bubble

The parts were written down here and their **arrangement** was not, so the three clients
arranged them three ways and only one of them reads as a voice message. The iOS one is
the shape; the other two come to it.

**One row.** A filled square 38 pt across with rounded corners (radius 10) on the leading
edge, carrying a play or pause glyph - never a circle - the square takes the bubble's text colour and the glyph the bubble's own
background, so it reads as a solid button on both the incoming and the outgoing surface.
Beside it a column of fixed width, about 170 pt, holding two things:

- the **waveform**, 26 pt tall, its bars filling with the text colour as playback passes
  them and sitting at low opacity ahead of the playhead, so how far in you are is legible
  without a separate progress bar. It is the scrubber: press or drag on it to seek.
- under it one quiet line, in a small monospaced digit face so the number does not
  shuffle as it counts: the **elapsed** time while this message is the one playing, the
  **total** otherwise; then, on an incoming message not yet played, a small accent dot;
  then the **speed** control pushed to the trailing end - `1x` / `1.5x` / `2x` - shown
  **only while this message is the one playing**, because a speed control on a message
  nobody is listening to is a control for nothing.

**The message's own time sits on that same line**, at its trailing end, rather than on a
line of its own under the bubble. A voice message is a row, not a paragraph, and putting
the length of the recording and the time it was sent on two different levels makes the
bubble taller than it has any reason to be and reads as two unrelated facts.

The upload and failure states live on the same row rather than replacing it: an upload in
flight draws a progress ring around the play button, a failed one puts `Not sent · Retry`
under the row, and a message that cannot be decoded puts `Couldn't play this message`
there instead.

**Nothing about the row's geometry changes, ever** - not between those states and not
when playback starts. The speed control appears only while this message is the one
playing, so the space it will occupy is reserved whether or not it is there; a bubble
that grows at the moment a person presses play jumps under the eye that is watching it.
Reserving the space is not the same as showing the control: it stays absent to the eye
and to a screen reader until there is something to control.

Only one voice message plays at a time; starting another pauses the first. Playback
continues to the next consecutive unplayed voice message from the same sender.

### States

| State | UI |
|---|---|
| Recording | Recording bar (§ flow step 2) |
| Locked | Recording bar with Stop / Delete |
| Preview | Player + Delete / Send |
| Uploading | The bubble appears in the timeline immediately with a determinate ring over the play button; the message is not yet sent |
| Upload failed | The bubble shows `Not sent · Retry`; nothing reached the other side |
| Received, downloading | Play button shows a spinner on first tap; the file is fetched and decrypted, then plays |
| Download or decrypt failed | `Couldn't play this message` inline; a retry on tap. Never plays partial or unauthenticated bytes |
| Recording format unsupported (web only) | Mic button disabled with tooltip `Voice messages can't be recorded in this browser`. Receiving still works |
| Hearing nothing | The recording bar says `No sound from the microphone` in place of its hint, and clears the moment any level is measured |

**A take that hears nothing says so while it is being made.** When the measured
input level has stayed at zero for two seconds the recording bar says so, and
it says it *during* the take rather than after: a warning that arrives once the
message is already sent has cost the person the whole recording and tells them
only that it was wasted. The take is never blocked and never discarded on that
account - a quiet room is not an error and refusing to send would be the client
overruling the person about their own message - so the warning is a warning,
and the message sends with whatever was captured.

Written down because silence was invisible from inside the app. A chain of
voice messages recorded on an Android emulator whose microphone toggle was off
arrived as digital silence (`mean_volume: -83.3 dB`), and nothing anywhere -
not the bar, not the waveform, not the sent bubble - said the microphone had
given nothing; the cause was found by measuring the file with an external tool.
Every client already samples the level to draw the live meter, so the signal
the warning needs is one each of them is holding and none of them reads.

The two seconds are the spec's number, not a platform's: below about one second
a microphone that is merely slow to warm up would be accused of being dead, and
past three the person has already committed to the take. All three clients use
the same value so the same room produces the same warning on each.

## 3. Video notes (round video)

### Flow

Same gestures as voice, in camera mode:

1. **Hold** → a large **circular live preview** of the front camera appears over the
   chat, with a progress ring around it counting toward the cap, and `Slide to cancel`.
2. Release → send. Slide left → cancel. Slide up → lock (Stop / Delete, and a
   **flip camera** button while locked).
3. Cap **60 seconds**; the ring completes and recording stops into preview.
4. Preview: the round clip loops muted; tap to play with sound; Delete / Send.
5. Web desktop: the same hold, with the lock and cancel targets under the pointer;
   `Esc` cancels. A click switches the mode rather than recording (section 3.1).

### Timeline bubble

A **circle** (about 240 pt / dp / px), no bubble chrome. Incoming clips autoplay
**muted and looping** when scrolled into view only if the user enabled
"Autoplay video notes" (default **off** — it downloads media); otherwise they show the
poster frame with a play glyph. Tap → plays with sound from the start, the ring shows
progress; tap again pauses. A mute glyph shows while playing muted.

### States

Same table as voice (uploading ring, not-sent retry, spinner on first play, couldn't-play,
recording-unsupported on web), plus:

| State | UI |
|---|---|
| Camera permission denied | See §6 |
| No camera present | Camera mode is not offered; the mic/camera toggle does nothing and says `No camera found` |
| Poster frame missing or failed | A neutral circle with a play glyph — never a broken-image icon |

### 3.1 One button, and the same one on every client

The composer's right-hand button, when the text field and the tray are empty, is
**one** button. It carries the current mode's glyph — a mic or a camera — and it
answers two gestures:

- **Tap or click: switch the mode.** Mic becomes camera, camera becomes mic. The
  choice is remembered per device. Nothing records.
- **Press and hold: record** in the mode the glyph is showing, with the slide-to-cancel
  and slide-up-to-lock targets of sections 2 and 3, under a thumb and under a mouse
  pointer alike.

It is written down because the three clients had drifted into three different
answers, and a person carrying two phones met all three in a minute: web showed
**two** buttons side by side, a mode switch and a recorder; Android had one that
toggled but flashed the whole screen doing it; iOS had one that ignored the tap and
started recording a voice message the instant it was touched, so the camera mode
could not be reached at all.

A tap therefore never records, on any client. **The two are told apart by the gesture,
before anything is captured**, and not by how long the take turned out to be: a press
is a hold once it has lasted **200 ms** without travelling more than **10 px / pt / dp**,
and only then does the microphone open. Deciding it on the outcome instead would mean
opening the microphone on every tap and throwing the take away, which is the one thing
a person can hear.

A take that begins and ends under the 0.5 s floor is therefore a hold that was let go
at once - the accidental-press case of section 2, discarded with the same hint. It is
not a mode switch, because by then the microphone had already been opened; a tap that
never reached the threshold is the mode switch, and the two cannot be confused.

A press that travels past the slop is neither: a finger on its way to the cancel target
must not flip the mode on release.

**No press ever ends in a locked recording.** An older desktop note let a short mouse
press continue recording hands-free, which was a second meaning for the one gesture
that now switches the mode, and the only pointer route to the camera before the second
button went away. Locking is reached the way it is on a phone - hold, then slide up
onto the lock target, which a mouse reaches as readily as a thumb - and by the single
activation a keyboard or screen reader makes.

**On native, switching the mode gives a light haptic tick.** Recording start has one
already; a tap that only changes a glyph and is otherwise silent reads as a dead
button. Web has no equivalent and needs none: the glyph changes under a pointer that
is already there.

**The keyboard and the screen reader get both gestures without either one.** Hold and
slide are not reachable that way, so the single button carries two actions in the
accessibility tree: activating it starts a **locked** recording in the current mode
(section 2, step 5 — the state a person can leave with Stop), and a second action
named `Switch to video message` / `Switch to voice message` changes the mode. On iOS
and Android that second action is a custom accessibility action on the same button; on
web it is a focusable control that carries no ink of its own, so the visible composer
is still one button on all three. Without it a screen-reader user could never reach
the camera, because the gesture that switches is the one they do not have.

## 4. Emoji picker

One picker, two tabs: **Emoji** and **GIF**. Opened from the composer's smiley button
(bottom sheet on iOS and Android, a popover anchored above the composer on web).

### Emoji tab

- Search field at the top (`Search emoji`), matching emoji names and keywords.
- A **Recent** row first (most recently used by this device, up to 24), then the
  standard categories with a category strip for jumping.
- **Skin tone:** long-press (native) or right-click (web) on an emoji that supports tones
  shows the tone variants; the chosen tone becomes that emoji's default on this device.
- Tapping an emoji **inserts it at the caret** and keeps the picker open (so several can
  be added); the text field keeps focus on web.
- Emoji data ships **with the app**. The picker never fetches anything from the network.
- Rendering uses the platform's emoji font (Apple Color Emoji / Noto Color Emoji /
  the browser's). No custom emoji images.

### Reaction mode

The existing six-emoji reaction bar gains a trailing **+**. It opens the same picker,
Emoji tab only (GIF hidden), and a tap **reacts and closes**. The reaction appears as a
chip like the fixed six do. A reaction is exactly **one emoji grapheme**; the picker can
only produce those, so the six-emoji bar and the picker stay one model.

### States

| State | UI |
|---|---|
| No search results | `No emoji found` |
| Empty Recent | The Recent row is simply not shown |

## 5. GIFs

### Where GIFs come from

Search goes through **Lakko's own server**, which queries GIPHY with Lakko's API key —
the model the previous app used. Two things change from it:

- **Previews are proxied too.** The previous app loaded preview thumbnails straight from
  GIPHY's CDN, sending every viewer's IP to GIPHY. Here every image the picker shows comes
  through the Lakko server.
- **The sent GIF is an ordinary encrypted attachment.** After the user picks one, the
  client downloads it through the proxy, encrypts it, and uploads it like any file.
  Recipients only ever fetch the encrypted blob from Lakko storage — they never contact
  GIPHY or the proxy.

The Lakko server sees what is searched for. The picker shows no note about it
(maintainer decision, 2026-10-28).

### GIF tab

- Search field `Search GIF`. Empty query shows **Trending**.
- A two-column (mobile) / three-column (web) grid of looping, muted previews, lazily
  loaded, with infinite scroll.
- Search is **debounced** (about 350 ms after typing stops) — every request is a query
  the server sees, so the client does not send one per keystroke.
- Tap a GIF → it is **sent immediately** (the messenger convention), shown in the timeline
  with an uploading overlay, exactly like a picked file.
- A GIPHY attribution mark (`Powered by GIPHY`) sits at the bottom of the tab, as
  GIPHY's terms require.

### Timeline

A GIF message renders as the animation itself, auto-looping and muted, sized to its
aspect ratio within the bubble width, with a small `GIF` label under the picture, never on it (decided 2026-11-04, a label on the image covered GIF captions): a capsule at the leading edge, sunken surface, muted 11/600 text, the same on every client. Tap opens it full-screen.
On iOS and Android it must actually animate (not the first frame only).

### States

| State | UI |
|---|---|
| Loading first page | A grid of neutral placeholder tiles |
| No results | `No GIFs found` |
| Proxy not configured on this server | The GIF tab is **hidden**. Nothing else changes |
| Proxy error / offline | `GIFs are unavailable right now` with Retry |
| Rate limited | `Too many searches — try again in a moment` |
| Picked GIF download failed | Timeline bubble `Not sent · Retry`, as for any upload |

## 6. Permissions

Asked **just in time**, on the first hold of the mic or camera button — never at app
start.

| Situation | UI |
|---|---|
| Not yet asked | The system prompt (native) / browser prompt (web). Recording starts only once granted; the hold that triggered the prompt does not record. A grant that arrives while the pointer is still down records whatever the browser reported: nobody answers a prompt while holding the button |
| Denied | An inline card above the composer: `Lakko needs microphone access to record voice messages` (or camera) with **Open Settings** on iOS/Android; on web, `Allow microphone access in your browser's site settings` |
| Revoked while recording | Recording stops, the take is discarded, the denied card shows |
| In a call | Mic and camera buttons are disabled with `Unavailable during a call` — the call owns the audio session and the camera |
| A call ringing in, not yet answered | The recorder stays usable. Nothing is capturing: an incoming call does not open the callee's microphone until it is accepted. An OUTGOING call is different and is treated as in a call from the moment it is placed, because the caller's microphone opens then |

Web additionally needs the deployment to allow capture: the page's Permissions-Policy
must permit `microphone=(self)` and `camera=(self)`.

## 7. Cross-platform shape

One mental model everywhere: **hold to record, slide to cancel, slide up to lock**;
**one picker with Emoji and GIF tabs**; media and GIFs are **encrypted attachments with
a kind**, rendered by kind.

| | Web | iOS | Android |
|---|---|---|---|
| Record button | ONE button (section 3.1): tap switches mode, hold records | same | same |
| Hold gesture | pointer down + hold, lock and cancel targets under the pointer | long-press gesture | long-press / pointer input |
| Mode switch for a screen reader | a focusable control carrying no ink | custom accessibility action | custom accessibility action |
| Picker | popover above composer | bottom sheet | modal bottom sheet |
| Skin tones | right-click | long-press | long-press |
| Denied → settings | text instruction | **Open Settings** | **Open Settings** |
| Recording API | `MediaRecorder` | `AVAudioRecorder`; `AVCaptureSession` + `AVAssetWriter` | `MediaRecorder`; CameraX video |

Accessibility: every control has a label ("Record voice message", "Cancel recording",
"Play voice message, 12 seconds"); a VoiceOver/TalkBack/keyboard user can start a
**locked** recording with a single activation, since hold-and-slide is not accessible,
and reaches the mode switch through the second action of section 3.1;
the waveform is exposed as a slider for seeking; duration is announced, not only shown.

## 8. Data the UI needs (input to the ADR)

Carried per attachment in the chat envelope, all optional so older clients degrade to a
plain file chip:

| Field | Why the UI needs it |
|---|---|
| `kind`: `voice` \| `video_note` \| `gif` \| absent (= file) | Chooses the bubble (§2, §3, §5). Unknown values fall back to a file chip |
| `duration_ms` | Shown before download; the recorder's own file may carry no duration |
| `waveform` (up to 128 amplitude bytes) | Draws the voice bubble before the audio is downloaded |
| `width`, `height` | Sizes GIFs and video-note posters without layout jumps |
| `thumb` (a second encrypted blob reference) | Video-note poster frame shown before download |
| A played marker | **Local only**, never sent — "played" is not a read receipt |

Recording formats (one per kind, identical on all three platforms, so every client plays
every other's messages without transcoding): voice **AAC in MP4** (`audio/mp4`);
video note **H.264 + AAC in MP4** (`video/mp4`), square. A client that cannot record the
format disables recording (§2 states) rather than sending one only some clients can play.

Inline playback must be gated on the **MIME type from an allowlist**, never on `kind`
alone — `kind` is sender-chosen, like the file name.

Server-side, the GIF proxy must expose: search (query, offset) and trending (offset)
returning preview and full-size **proxy URLs** plus dimensions; a media fetch restricted
to GIPHY's hosts with a size cap; session-token auth; a per-account rate limit; and a
"not configured" answer the client maps to the hidden tab.

## 9. Out of scope (v1)

Stickers (the previous app had packs; not requested), trimming a recording, transcribing
voice messages (now specified in [`translation-and-transcription.md`](translation-and-transcription.md)), raise-to-listen on native, sending a GIF with a caption, a GIF favourites
list, and video notes longer than 60 s.

## 10. Photos and videos in the timeline

Added 2026-09-17 after a live check: a photo sent from iOS reached Android and web as a
file chip with **Open** / **Save**, and a picked video had no way to play in the app.
A photo or a video is shown in the chat window itself, like every mainstream messenger.

### Which attachments get a media bubble

- **Photo bubble**: MIME on the inline allowlist as an image (`image/jpeg`, `image/png`,
  `image/gif`, `image/webp`) and no `kind`. Decided by MIME, never by `kind` or name (§8).
- **Video bubble**: MIME `video/mp4` and no `kind`.
- Anything else, including HEIC and SVG, stays a file chip. The iOS photo picker converts
  HEIC to JPEG before sending so an iPhone photo is always a photo bubble everywhere.
- Several photos and videos sent in one message render together in one bubble (section 11).

### Sending

The sender attaches, next to the original, everything the receiver needs before download:

| Field | Photo | Video |
|---|---|---|
| `width`, `height` | Pixel size of the image, scaled down proportionally to fit 4096 if larger | Pixel size of the first video track, same scaling |
| `thumb` | JPEG, longest side 320 px, quality chosen so the blob is under 64 KiB (hard wire cap 256 KiB) | JPEG poster of the first frame, same size rule |

If a thumb cannot be produced (decoder failure, unsupported codec) the attachment is sent
without it; never block a send on a preview. The composer shows the same thumbnail in the
upload chip while the upload runs.

### Receiving: states of the bubble

The bubble has a fixed size from `width` / `height` before anything is downloaded: width
at most 260 pt/dp (web 320 px), height at most 360, aspect preserved, never upscaled past
the thumb's native size by more than the bubble box. Without dimensions the bubble uses
the thumb's aspect, and without both a 4:3 placeholder.

| State | Photo | Video |
|---|---|---|
| Thumb loading | Neutral placeholder of the final size | Same, plus a play glyph |
| Thumb ready | Thumb scaled to fill, lightly blurred | Poster with a centred play button and the file size |
| Full loading | Thumb stays, a small progress ring in the corner | Unchanged until tapped |
| Full ready | The photo replaces the thumb in place | n/a |
| Failed | Thumb (or placeholder) with `Couldn't load photo`, tap retries | `Couldn't load video`, tap retries |

- **Photos load automatically** when the bubble comes on screen: first the thumb, then
  the full image. Decrypted bytes are cached for the session so scrolling back does not
  download again.
- **Videos load only on tap.** Tap downloads with a progress ring on the poster, then
  opens the full-screen player.
- Time, ticks and edited marker sit on the media in a small translucent pill, bottom right.
- Only bytes that decrypted and authenticated are ever shown (§4 failure rule).

### Viewer

Tap a photo: full-screen viewer, black background, pinch/scroll zoom on native, close by
swipe down or a close button, **Save** (native share sheet / browser download). Tap a
video: full-screen player with the platform's standard controls and the same Save.
Long-press (native) or right-click (web) keeps the existing message menu (react, reply,
delete own).

### Cross-platform shape

One mental model on all three: the same allowlist decision (the shared `inlineMediaType`
FFI call), the same sizing box, the same auto-load rule (photos yes, videos on tap), the
same viewer. The file chip remains only for non-media files.

## 11. One message with text and several photos and videos

Added 2026-11-19 (maintainer): better than a messenger where a photo is always a message of
its own with a caption. A message is text plus whatever is attached to it: one photo, ten,
videos, files, in any mix. The sender sees what is attached before sending, and the
receiver sees the message as it was composed.

### The composer tray (sender)

- **Ways in:** paste (`Cmd+V` / `Ctrl+V` on web and desktop, Paste or keyboard insertion on
  iOS and Android), drop on the conversation (web, desktop), the paperclip's Photo or Video,
  File, Audio and Camera. All of them add to the tray; none opens a dialog. There is no
  `Send as file` switch anywhere.
- **Where:** a strip directly above the message field, inside the composer, full composer
  width, 8 px from the field. Hidden when empty.
- **Items:** a photo or video is a 64 px square thumbnail (corner radius 8, cropped to fill);
  a video shows its duration bottom left on a translucent pill; a file is a chip 64 px high
  with a document icon, the name (one line, ellipsised) and the size. Order: the order they
  were added, left to right; the strip scrolls sideways past the composer width (web:
  horizontal wheel and shift-wheel; a fade at the cut edge).
- **Remove:** an (x) button in each item's top end corner, 20 px, always visible on touch
  screens, on hover and focus on web and desktop. `Backspace` in an empty field removes the
  last item (web, desktop).
- **Look larger:** click or tap a photo or video thumbnail opens the media viewer (section
  10) on that item, with the tray's other photos and videos to swipe or arrow through.
  Closing returns to the composer with the text and the tray unchanged.
- **Upload starts at once** when an item is added: each thumbnail shows a thin progress
  ring; a failed upload shows a retry glyph on the thumbnail (tap retries) and Send stays
  off until it is retried or removed.
- **Limits:** at most 10 photos and videos, and at most 10 files, per message (chosen, not
  measured). Adding past a limit adds what fits and shows, under the strip, `Up to 10
  photos and videos in one message.` / `Up to 10 files in one message.` for 4 s.
- **Send:** enabled when the field has text or the tray has at least one item, and no item
  is still uploading or failed. Sends ONE message: the text and every item. The field and
  the tray clear together. Editing a sent message changes its text only.
- **Draft:** the tray belongs to the conversation's draft on this device: leaving the
  conversation and coming back keeps it while the app runs (the uploaded items are kept;
  ones still uploading continue).

### The bubble (receiver and sender)

- **One bubble** holds, top to bottom: the media mosaic, the file chips, the text. A reply
  quote or a forward label stays at the top as today.
- **Mosaic** of the photos and videos, in the order sent, 2 px gaps, outer corners rounded
  with the bubble, inner corners square, total width the media bubble width of section 10
  (260 pt/dp, web 320 px):
  - 1 item: as section 10 (its own aspect, within the box: at most the bubble width wide and
    360 high); an extreme aspect is kept at least 80 high and 80 wide and cropped to that;
  - 2: side by side, equal width, the height of the shorter at that width, between 80 and 260;
  - 3: one large on the start side (two thirds of the width less the gap), two stacked on the
    end side; the block is as tall as the large one at its width, between 162 (two stacked
    tiles of at least 80 and the gap) and 300, and the two stacked split that height;
  - 4 to 10: rows of 2 and 3, as even as the count allows (4 = 2+2, 5 = 2+3, 6 = 3+3,
    7 = 2+2+3, 8 = 3+3+2, 9 = 3+3+3, 10 = 3+2+2+3), equal widths in a row; each row is as
    tall as its most landscape item at its tile width, between 80 and 200 px. Every tile
    crops to fill.
  - Every row fills the width exactly: when it does not divide evenly, the first tiles of
    the row (start side) are one pixel wider. A photo or video of unknown size is laid out
    as 4:3; a client with no `width` / `height` but a thumbnail of known size passes the
    thumbnail's size.
  - A received message carrying more than 10 photos and videos (a sender cannot attach
    more) shows the first 10 in the mosaic and the viewer, in the order sent, and nothing
    of the rest.
  - The layout is one shared rule, `media_mosaic` in `lakko-ui-rules` (on both FFI
    surfaces); its numbers are chosen, not measured.
  - Each tile follows section 10's states (thumb, full, failed, video poster with play
    glyph and duration) on its own.
- **Text** below the mosaic and chips, inside the bubble, as an ordinary text message. With
  text, the time and ticks sit at the end of the text; without text, in the translucent pill
  bottom end on the mosaic.
- **Tap** a tile opens the viewer on it with every photo and video of the message to swipe
  (native) or arrow through (web, desktop), a position line `2 / 5`, Save for the current
  one. A video tile plays in the viewer.
- **Chat list row** (added 2026-11-19, maintainer): a message with photos or videos shows,
  at the start of its preview line, a 20 px thumbnail of the first one (corner radius 4,
  cropped to fill; a video's thumbnail carries a small play glyph). With several, the
  thumbnail is drawn as a **stack**: one or two offset edges behind it (2 px right and up
  each, the row's muted tone), one edge for 2 items, two for 3 or more. Then the message
  text; without text, the words `Photo`, `3 photos`, `Video`, `2 videos`, `4 photos and
  videos`. Files only: no thumbnail, the words `File` / `3 files`, then ` · ` and the text when there
  is text (`2 files · Weekend plans`). The
  thumbnail is the attachment's own sent thumb (section 10), already decrypted for the
  timeline or fetched and decrypted when the row is drawn; until it is there the space shows
  the row's neutral placeholder, never a broken image. A failed fetch keeps the placeholder.
- **Notifications:** the same words and text as the list row, plus the first photo or
  video's picture on every platform (iOS, Android, web, desktop), taken from the micro
  thumbnail carried inside the message (ADR-0047 section 6), so nothing is downloaded and a
  notification shown in the background has it too. The picture is shown exactly when the
  text is: where the system hides notification content on the lock screen, it hides both.
  The list row's thumbnail comes from the same micro thumbnail, so it appears at once.
- **Reactions, reply, forward, delete, save** act on the whole message. Forwarding a message
  with several items forwards them all, re-encrypted (ADR-0052).

### Cross-platform shape

The same tray, mosaic rule and viewer on web and desktop (SolidJS), iOS (SwiftUI) and
Android (Compose). The mosaic layout is one rule computed the same way on every client.
Nothing changes on the wire: a chat envelope already carries the text and a list of
attachment references (`lakko-attachments`), each with its own dimensions and thumbnail.

