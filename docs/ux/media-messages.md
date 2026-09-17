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

1. **Press and hold** mic → recording starts immediately (after permission, §6). Haptic
   tick on native.
2. While holding, the composer is replaced by the **recording bar**: a pulsing red dot,
   the elapsed time `0:07`, a live level meter, and the hint `Slide to cancel`.
3. **Release** → the message is sent.
4. **Slide left past the threshold** → cancelled, nothing sent, a short "Cancelled" shake.
5. **Slide up** onto the lock target → **locked**: the finger can lift, recording
   continues, the bar shows **Stop** and **Delete**. Stop → preview state; Delete → discard.
6. **Preview state** (only after lock + stop): play/pause the take, a waveform, duration,
   **Delete** and **Send**. No trimming in v1.
7. Hard cap **15 minutes**; at the cap recording stops and moves to preview.
8. A take shorter than **0.5 s** is discarded silently (an accidental tap), with a
   one-line hint `Hold to record`.

Web has no touch-hold on desktop: **mouse-down and hold** works the same way; a plain
**click** on the mic starts a locked recording directly (desktop has no slide gesture).
`Esc` cancels.

### Timeline bubble

Play/pause button, waveform (tap or drag on it to seek), duration, a "played" state (the
waveform fills as it plays; an unplayed incoming voice message shows a small dot until
first play), and a speed toggle `1x / 1.5x / 2x`. Only one voice message plays at a time;
starting another pauses the first. Playback continues to the next consecutive unplayed
voice message from the same sender.

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

## 3. Video notes (round video)

### Flow

Same gestures as voice, in camera mode:

1. **Hold** → a large **circular live preview** of the front camera appears over the
   chat, with a progress ring around it counting toward the cap, and `Slide to cancel`.
2. Release → send. Slide left → cancel. Slide up → lock (Stop / Delete, and a
   **flip camera** button while locked).
3. Cap **60 seconds**; the ring completes and recording stops into preview.
4. Preview: the round clip loops muted; tap to play with sound; Delete / Send.
5. Web desktop: click the camera button → locked recording directly with the round
   preview; `Esc` cancels.

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

**What the Lakko server learns is stated in the UI once:** the first time the GIF tab is
opened, a one-line note above the grid reads
`GIF search goes through Lakko's server, which sees what you search for. Sent GIFs are encrypted like any file.`
with a dismiss **Got it** (remembered per device). This is the honest cost of the
feature and the user sees it before searching.

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
aspect ratio within the bubble width, with a small `GIF` label. Tap opens it full-screen.
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
| Not yet asked | The system prompt (native) / browser prompt (web). Recording starts only once granted; the hold that triggered the prompt does not record |
| Denied | An inline card above the composer: `Lakko needs microphone access to record voice messages` (or camera) with **Open Settings** on iOS/Android; on web, `Allow microphone access in your browser's site settings` |
| Revoked while recording | Recording stops, the take is discarded, the denied card shows |
| In a call | Mic and camera buttons are disabled with `Unavailable during a call` — the call owns the audio session and the camera |

Web additionally needs the deployment to allow capture: the page's Permissions-Policy
must permit `microphone=(self)` and `camera=(self)`.

## 7. Cross-platform shape

One mental model everywhere: **hold to record, slide to cancel, slide up to lock**;
**one picker with Emoji and GIF tabs**; media and GIFs are **encrypted attachments with
a kind**, rendered by kind.

| | Web | iOS | Android |
|---|---|---|---|
| Hold gesture | pointer down + hold; click = locked | long-press gesture | long-press / pointer input |
| Picker | popover above composer | bottom sheet | modal bottom sheet |
| Skin tones | right-click | long-press | long-press |
| Denied → settings | text instruction | **Open Settings** | **Open Settings** |
| Recording API | `MediaRecorder` | `AVAudioRecorder`; `AVCaptureSession` + `AVAssetWriter` | `MediaRecorder`; CameraX video |

Accessibility: every control has a label ("Record voice message", "Cancel recording",
"Play voice message, 12 seconds"); a VoiceOver/TalkBack/keyboard user can start a
**locked** recording with a single activation, since hold-and-slide is not accessible;
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
voice messages, raise-to-listen on native, sending a GIF with a caption, a GIF favourites
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
- Several photos sent together still render as separate bubbles in v1.

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
