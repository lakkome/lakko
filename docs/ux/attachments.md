<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec — the attachment menu and the kinds it sends

**Status:** design, 2026-11-15. The input to [ADR-0092](../adr/0092-attachment-kinds.md)
and to the four client builds (web, desktop, iOS, Android). Nothing in this document is
built.
**Owner surface:** the paperclip in every composer (one-to-one, group, channel), the
bubbles of the new message kinds, and the screens that create and read them.
**Pattern parents:** Telegram's paperclip menu (Photo or Video, File, Camera, Poll, Audio,
Checklist, Location, Article; Wallet and Gift have no counterpart in Lakko and are
dropped); the existing media pipeline of [`media-messages.md`](media-messages.md); the
message menu of [`message-actions.md`](message-actions.md); the `LakkoMenu` action sheet
of the design system.

## 1. Why this exists

The paperclip does a different thing on every client today:

| Client | What the paperclip does now |
|---|---|
| Web and desktop | It is a `<label>` around a hidden `<input type="file" multiple>` (`apps/web/src/routes/Chat.tsx`, the channel composer `apps/web/src/channels/ChannelComposer.tsx` the same): one click opens the browser's file chooser, photos and documents mixed |
| iOS | An in-app action sheet with two rows, `Photo library` (the system photo picker) and `File` (the system document picker) (`apps/ios/Lakko/UI/Chat/ChatScreen.swift`) |
| Android | A stock Material `DropdownMenu` with two rows, media (the system photo picker) and file (the system document picker) (`apps/android/.../ui/chat/ChatScreen.kt`) |

Three behaviours for one button, and the Android one is the stock Material menu the design
system bans. The maintainer asked for one menu, the same on every client and in every
conversation kind, carrying every kind Telegram's menu carries, all in the first version.

## 2. The menu

### 2.1 Shape

Activating the paperclip opens one **`LakkoMenu`** (the design system's action sheet):

- **Phone (iOS, Android, narrow web):** an in-app bottom sheet of 48-point rows, icon and
  label, no heading row, no Cancel row (dismissing is Cancel).
- **Wide web and desktop:** a compact popover anchored above the paperclip, opening upwards,
  aligned to the paperclip's start edge: rows 36 px high, 10 px horizontal padding, an
  18 px icon then the label 14/500, both **start-aligned** (never centred), 10 px between
  icon and label, 4 px padding inside the popover, width set by the longest label (about
  200 px in English). It is the same size and look as the message menu (`.lk-menu`).

It is never the system menu, `DropdownMenu`, a SwiftUI `Menu`, or a file chooser opened
directly by the paperclip.

### 2.2 Items, in this order on every client

| # | Label | Icon (meaning) | What it opens |
|---|---|---|---|
| 1 | Photo or Video | picture | The **system** photo picker, several items |
| 2 | Camera | camera | Lakko's own capture screen (section 5) |
| 3 | File | document | The **system** document picker, several files |
| 4 | Audio | music note | The **system** document picker filtered to audio files |
| 5 | Poll | bar chart | Lakko's poll editor (section 7) |
| 6 | Checklist | checked list | Lakko's checklist editor (section 8) |
| 7 | Location | pin | Lakko's location screen (section 9) |
| 8 | Contact | person | Lakko's contact picker (section 10) |
| 9 | Article | page with lines | Lakko's article editor (section 11) |

No separators: the nine rows are one list (amended 2026-11-18; separators added height and
told the person nothing). The order is the same in every conversation kind; a row that does
not apply is **absent**, never disabled, except where a disabled row explains something the
person needs to know (section 2.4).

### 2.3 What is a system surface and what is Lakko's

The design system allows a platform surface only where nothing else can do the job.

| Item | Surface | Why |
|---|---|---|
| Photo or Video | System photo picker (iOS `PhotosPicker`, Android Photo Picker, web file chooser with `accept="image/*,video/*"`) | The system picker needs **no photo-library permission**: Lakko sees only what the person picked. An in-app gallery strip, as Telegram draws, would need access to the whole library. Lakko does not ask for it |
| File, Audio | System document picker (iOS document picker, Android `OpenDocument`, web file chooser) | No app can list a person's files without one |
| Camera | Lakko's own screen; only the permission prompt is the system's | The capture view is ours on every platform, web and desktop included (`getUserMedia`) |
| Poll, Checklist, Location, Contact, Article | Lakko's own screens; Location shows the system permission prompt once | — |

### 2.4 Which items appear where

| Conversation | Who | Menu |
|---|---|---|
| One-to-one | either person | Eight: every row but `Poll`, as in Telegram (polls are for groups and channels; maintainer, 2026-11-18) |
| Group | any member | All nine |
| Group of two people | any member | All nine; the poll editor has no Anonymous switch (one other voter makes anonymity meaningless) |
| Channel | owner, publishers | All nine; a poll in a channel is always anonymous and the switch is not shown (section 7.7) |
| Channel | subscriber | No paperclip. A subscriber writes comments, and comments take no attachments ([ADR-0087](../adr/0087-group-and-channel-handles-and-privacy.md) section 13a); this spec does not change that |
| A conversation that ended, a closed one-to-one | — | No composer, so no menu |
| Editing a message | — | The paperclip is hidden while the composer edits; an edit changes text only |

Two rows may be shown **disabled with a reason** instead of absent, because the person
would otherwise not understand why the thing they look for is missing:

- **Camera** on a device with no camera: `No camera on this device`.
- **Location** where the platform has no location service (a desktop with none):
  `Location isn't available on this device`.

### 2.5 Keyboard and other ways in

- Web and desktop: `Enter` / `Space` on the paperclip opens the menu, arrow keys move,
  `Enter` picks, `Escape` closes (the `LakkoMenu` keyboard rule).
- Dropping files on the conversation attaches them; so does **pasting** (amended
  2026-11-18): any file on the clipboard (a screenshot, a copied picture, a video, a file
  copied in the file manager) attaches as a drop would, sent as Photo or Video when the MIME
  is on the inline list and as File otherwise. Web and desktop: `Cmd+V` / `Ctrl+V` anywhere
  in the open conversation, not only in the composer; a paste into another field stays that
  field's text, and a copy from a document (plain text and HTML together) pastes as text.
  iOS and Android: the composer's Paste offers a copied photo or video and attaches it, as
  the system keyboard's image insertion does.
- The emoji button and the record button are not in this menu ([`media-messages.md`](media-messages.md)).

## 3. Photo or Video

**Flow.** Picker → the **send preview** (one screen for one or many items): thumbnails in a
strip, the current item large, a caption field, a `Send as file` switch off by default, a
delete control per item, `Send`. Cancel returns to the conversation with nothing sent.

**What is sent.**

- Each **photo** is decoded on the device and re-encoded as JPEG with its longest side at
  most 2560 px (chosen, not measured), metadata (EXIF location, camera, time) dropped. The
  thumbnail and dimensions are those of [`media-messages.md`](media-messages.md) section 10.
- Each **video** is sent as picked when it is MP4 with H.264 and at most the attachment cap
  (50 MiB today); a poster thumbnail and dimensions are added. H.264 in another container
  (an iPhone `.mov`) is moved into an MP4 container on the device without re-encoding, so
  what is sent is still MP4 with H.264. A video in another format or
  over the cap is refused in the preview: `This video can't be sent. Videos must be MP4 and
  up to 50 MB. Send it as a file instead.` (with `Send as file`). Re-encoding a picked video
  is not in v1.
- `Send as file` sends every item as its original bytes through the File path (section 4).
- Several items send as several bubbles, one message each, in the order shown, the caption
  on the first. An album bubble is not in v1.

**States.** Picker cancelled: nothing. Decoding (a large photo): the item shows a spinner in
the strip. An item that fails to decode: `Couldn't read this photo` on that item, the rest
still send. Uploading and upload failed: as for any attachment (`Not sent · Retry`).

## 4. File

Picker → the same send preview with file chips instead of thumbnails (name, size, type),
caption field, `Send`. The bytes are sent unchanged. The bubble is the existing file chip
(name, size, `Open`, `Save`). A file over the cap: `Files up to 50 MB can be sent.` and the
item is removed from the preview.

## 5. Camera

Lakko's own full-screen capture view on all four clients.

- **Controls:** the shutter (tap = photo; on native, press and hold = video while held;
  a `Photo` / `Video` switch above the shutter does the same for a mouse or a screen
  reader), switch camera (front / back) where there are two, flash on native, close.
- **After capture:** the send preview of section 3 with `Retake` in place of the strip.
- **Video:** at most 60 s (chosen); the same MP4 / H.264 rule as video notes. A browser
  that cannot record that format hides the Video switch: `Video can't be recorded in this
  browser`, photos still work.
- **The camera background** of [`call-backgrounds.md`](call-backgrounds.md) does not apply
  here; a photo is of what the camera sees.

**States.** Permission not yet asked: the system prompt at the first open of the view, not
at the menu. Denied: the view shows `Lakko needs camera access to take photos` with
`Open Settings` (iOS, Android) or `Allow camera access in your browser's site settings`
(web, desktop). In a call: the row is disabled, `Unavailable during a call` (the call owns
the camera). No camera: the row is disabled (section 2.4).

## 6. Audio

A music or audio file, distinct from a voice message.

- **Picker:** the document picker filtered to audio (`audio/*`). Several files allowed.
- **On the sender's device**, before upload, Lakko reads the file's own tags: title,
  artist, duration, and embedded cover art (scaled to a 320-px JPEG thumbnail). A file
  without tags uses its file name as the title and no artist.
- **The bubble:** a play/pause square on the leading edge (the voice bubble's square), then
  title (one line, ellipsised) and artist (one line, muted), then a progress bar that is
  also the scrubber, elapsed / total time, and the cover art as a 44-point rounded tile at
  the trailing edge when present. `Save` in the message menu. One audio or voice message
  plays at a time; starting one pauses the other.
- **Playback** in the app is offered only when the MIME is on the shared inline list
  (`audio/mpeg`, `audio/mp4`; others to be added after each client is checked). Any other
  audio file is a file chip with `Save` and the same title and artist line.

States as for a voice message: downloading on first tap, `Couldn't play this file` on a
decode failure.

## 7. Poll

### 7.1 The editor (full screen; a dialog-sized sheet on wide web and desktop)

| Field | Rule |
|---|---|
| Question | Required, 1-255 characters (chosen) |
| Options | 2-10 (chosen), each 1-100 characters (chosen). Starts with two empty fields; `Add option` adds one; drag reorders; the remove icon removes (layout: section 7.1a) |
| Anonymous voting | Switch, **on** by default in a group. Absent in a group of two people (one other voter makes anonymity meaningless). Always on and not shown in a channel |
| Multiple answers | Switch, off by default |
| Quiz mode | Switch, off by default; excludes Multiple answers. When on, each option has a radio to mark the correct one (required) and an optional `Explanation` field, 0-200 characters (chosen) |
| Close automatically | Off by default; when on, a picker of 1 hour, 1 day, 1 week (chosen) |

`Create` is enabled once the question and two options are filled (and a correct option in
quiz mode). Leaving with anything typed asks `Discard this poll?` (`LakkoDialog`, `Discard`
danger / `Keep editing`).

**What Anonymous voting promises, said under the switch:**
`Nobody, including you, will see who voted for what. Everyone sees the totals.` In a quiz:
the same. When it is off: `Everyone in this chat will see who voted for what.`

### 7.1a Editor layout (poll and checklist, every client)

Amended 2026-11-18 after the first build looked like a form: the editor is a compact sheet,
not a page of controls.

- **Wide web and desktop:** a dialog 420 px wide, the title centred in its header with a
  close (x) at the start. **Phone:** full screen, the same content.
- **Question / Title:** one field with the placeholder `Question` / `Title`, no label above it.
- **Options / Items:** a small muted section label (`Options` / `Items`), then one row per
  entry: a single-line field with a remove (x) icon button inside its end edge, shown only
  when there is more than the minimum (two options, one item). No up/down buttons and no
  separate minus button. Below the rows, an `Add option` / `Add item` row (a plus icon and
  the label in the accent colour), absent once the maximum is reached. No empty trailing
  row. `Enter` in a row moves to the next row, and on the last row adds one.
- **Reorder:** drag a row by a muted handle at its start edge (web and desktop: shown on
  row hover and focus; iOS and Android: the platform's drag handle). From the keyboard,
  `Alt+Up` / `Alt+Down` in a row moves it; each move is announced (`Moved to position 2`).
- **Switches:** real switches (track and knob, the platform switch on iOS and Android, a
  Lakko switch on web), grouped in one card under the rows: label at the start, switch at
  the end, rows 44 px. Never an `On` / `Off` button. A line explaining a switch sits under
  its row, 13 px muted.
- **Footer:** one full-width primary button (`Create` / `Save`), disabled until the draft is
  valid.

### 7.2 The bubble

A card, full bubble width (max 360 points / 400 px on wide web):

- The question, 15/600.
- A line under it, muted: `Anonymous poll` / `Poll` / `Quiz` / `Anonymous quiz`, and
  `· Multiple answers` when set.
- **Before this person votes:** each option as a row with a radio (single) or checkbox
  (multiple) and its text. Single answer: a tap votes at once. Multiple answers: ticks are
  collected and a `Vote` button appears under the options.
- **After this person votes, or when the poll is closed:** each option shows its share as a
  bar behind the text and the percentage at the trailing edge, the person's own choice
  marked with a check. In a quiz the correct option is green and a wrong own choice red,
  with the explanation in a banner below the card when there is one.
- **Footer:** `{n} votes` (`No votes yet`, `1 vote`), and `Closed` when closed, or
  `Closes in {time}` when it closes automatically.
- **Non-anonymous poll:** after voting, `View results` under the footer opens the results
  screen (7.4).

### 7.3 Changing a vote, closing

- In the message menu of a poll the person voted in: `Retract vote` (back to the
  unvoted card) for any poll not closed. A new vote replaces the old one.
- The creator, and in a group the owner and admins, get `Close poll` in the message menu;
  it asks `Close this poll? Nobody will be able to vote any more.` (`Close poll` /
  `Cancel`). Closing is final.
- `Edit` is not offered on a poll: a question changed after people voted would change
  what their vote means.
- `Delete` works as for any message; the votes go with it.

### 7.4 Results screen (non-anonymous polls only)

One section per option: the option text, its count, and the names of the people who chose
it (avatar + display name, the conversation's usual name resolution). Read only. Anonymous
polls have no results screen; their card is the result.

### 7.5 What a person sees while an anonymous vote is being sent

An anonymous vote leaves the device after a random wait of up to ten seconds (ADR section
3.6):

- The tapped option shows a small spinner in place of its radio, and the footer says
  `Sending your vote…`. The person may leave the conversation; the vote is sent from the
  device's queue.
- Once it is sent, the card shows the results as in 7.2.
- If it cannot be sent (offline, removed from the group): `Your vote wasn't sent` with
  `Try again` in the footer.

### 7.6 Someone who joins while a poll is open

A new member sees every **open** poll and checklist of the group as if they had been
there: the card appears in the conversation at its place, with its current results, and
they can vote or tick like everyone else. Their device receives the open items from the
member who added them (ADR section 3.9). Closed polls and checklists stay part of the
history the newcomer does not read. There is no special state for a newcomer.

A member who leaves keeps their vote counted.

### 7.7 In a channel

A poll is created by the owner or a publisher like any post. Subscribers vote; nobody, the
publishers included, learns who voted for what, and the switch is not shown. The card says
`Anonymous poll`. Results update when the channel's counters are next fetched, as
reactions do.

- **Closing.** `Close poll` (publisher, message menu) freezes the results the publisher's
  app sees at that moment into the post; every reader then shows those. A poll that closes
  automatically shows `Closed` at its time and stops taking votes; its results freeze when
  a publisher's app is next open.
- **One vote per account.** Each account can vote once in a channel poll; voting again
  replaces the earlier vote, from any of the person's devices (ADR section 4). The app
  gets the anonymous credential this needs in the background the first time a person
  votes in a period; if it cannot (offline, the server unavailable), the footer says
  `Your vote wasn't sent` with `Try again`, as in 7.5.

### 7.8 Honest limits

- In a group of three to about ten people an anonymous result can still reveal votes (if
  all three choose the same option). Lakko shows nothing about this; it is what Telegram
  shows too.
- The ten-second wait makes it harder to tell who voted from the moment a vote arrives,
  not impossible; nothing about it is shown.

## 8. Checklist

### 8.1 The editor

| Field | Rule |
|---|---|
| Title | Required, 1-255 characters (chosen) |
| Items | 1-30 (chosen), each 1-100 characters (chosen); starts with one empty field; add, reorder, remove as in the poll editor (section 7.1a) |
| Others can add items | Switch, off by default. Shown in a one-to-one (the other person may add) and a group; not shown in a channel (subscribers have no write path to a post, and publishers may always add) |
| Others can mark items done | Switch, on by default in a one-to-one and a group; in a channel only publishers can |

### 8.2 The bubble

Title, `{done} of {total} done` muted, then the items as rows with a checkbox each. A done
item shows its checkbox ticked, its text in the muted colour (no strike-through: colour
plus the tick carry it), and the first name of whoever ticked it, small and muted, at the
trailing edge (in a one-to-one, only when it was the other person). A tap on an item the
person may tick toggles it at once. `Add item` (when allowed) at the bottom opens an inline
field. The creator's message menu offers `Edit checklist` (title, item texts, order;
existing ticks stay with their items), the two switches, and `Close checklist`, which
freezes it: nobody ticks or adds any more, and it is no longer handed to people who join
(section 7.6).

## 9. Location

### 9.1 What it is in v1

The person sends **where they are now**, once. No map tiles anywhere: no map in the picker,
no map in the bubble. **Live location is not in v1.**

### 9.2 Flow

1. `Location` → Lakko's location screen. Permission is asked here, the first time (iOS
   "while using the app", Android coarse and fine, the browser prompt).
2. The screen shows a large pin icon, the coordinates in the person's locale
   (`52.37022° N, 4.89517° E`), the accuracy (`Accurate to 12 m`), and an optional field
   `Name this place` (0-100 characters, chosen), e.g. "Café at the corner". The position
   updates while the screen is open; `Send` sends the current fix.
3. `Send current location` sends; the screen closes.

Nothing leaves the device before `Send`. Lakko asks no server where the person is and
turns coordinates into no address. Where the fix comes from is the platform's: Apple and
Google location services, and the browser's own provider on web, may use their vendor's
network service to compute it. Lakko says nothing about that in the UI, and the ADR records
it.

### 9.3 The bubble

A card: a pin icon in an accent-soft circle on the leading edge, then the place name (when
given) on the first line, the coordinates on the second line, muted, and `Accurate to
{n} m` small. No image.

A tap opens the message menu's first item at once on native, the menu on web:

| Client | Tap |
|---|---|
| iOS | Opens Apple Maps at the point (the system decides; it is the platform's maps app) |
| Android | Opens a `geo:` link; the person's default maps app, or the system chooser if none is set |
| Web, desktop | The message menu with `Open in OpenStreetMap` (opens `openstreetmap.org` at the point in the browser) and `Copy coordinates` |

The message menu on every client also carries `Copy coordinates`. The first time a person
opens a location on web or desktop, nothing extra is asked: the menu item names the site,
which is the disclosure.

### 9.4 States

| Situation | UI |
|---|---|
| Permission not asked | System prompt on entering the screen |
| Denied | `Lakko needs location access to send where you are` + `Open Settings` (native) / `Allow location access in your browser's site settings` (web) |
| Waiting for a fix | `Finding your location…`, `Send` disabled |
| Fix older than 2 minutes, or accuracy worse than 1 km (chosen) | `Send` stays enabled; the accuracy line turns warning-coloured: `Accurate to 1.4 km` |
| No fix after 30 s (chosen) | `Couldn't find your location. Try again outdoors or near a window.` with `Try again` |
| Approximate location only (iOS precise off, Android coarse only) | The accuracy line says so: `Approximate location` |

## 10. Contact

Shares one of the person's own contacts so the receiver can add them.

### 10.1 Flow

`Contact` → the contact picker (the Contacts list with search, single choice). A contact
without a username is shown disabled with `No username to share`. A contact whose "Who can
find me" is `Nobody` is shown disabled with `{name} doesn't let others share their contact`
(maintainer, 2026-11-18): their card is never sent. Picking shows the card preview and
`Send`.

How the sender knows: every profile a person sends (to a one-to-one and to each group)
carries one flag, "my card may be shared", which is off exactly when their setting is
`Nobody`. A change of the setting republishes the profile everywhere it already goes. The
picker reads the newest profile this device holds from that contact; a contact who never
sent one counts as shareable. The same check runs when forwarding a contact-card message:
if this device holds a profile of the person on the card and it says no, `Forward` is not
offered for that message. Not enforced: a card already sent before the person chose
`Nobody` stays in the chat, and a modified client can still send one (the receiver's
`Add contact` then finds nobody, since `Nobody` answers no lookup).

### 10.2 What the card carries and what the receiver can do

The card carries the contact's display name as the sender knows it, their `@username`, and
their account key, nothing that lets the receiver write to them (ADR section 7).

The bubble: avatar initials, name, `@username`, and one button:

| Situation on the receiver's device | Button / line |
|---|---|
| Already a contact (same key) | `Open chat` |
| The receiver itself | `This is you` (no button) |
| Otherwise | `Add contact` |

`Add contact` runs the ordinary username lookup. Then:

- Found, same account key: the ordinary contact-request flow starts, prefilled. The person
  the card describes receives a request as from any search and their "Who can find me"
  setting decides whether the lookup works at all.
- Found, a different account key: `This username now belongs to someone else.` Nothing is
  sent.
- Not found (the person cannot be found by username under their setting, or the name is
  gone): `{name} can't be found by username. Ask {sender} to introduce you, or ask {name}
  for an invite link.`

## 11. Article

A long post with formatting, read in a reader.

### 11.1 Editor (full screen; on wide web and desktop a centred column 720 px wide)

- A **title** field (required, 1-255 characters, chosen) and a body.
- A **format bar** above the keyboard (native) or above the body (web): `Heading`
  (cycles Heading / Subheading / Text), **Bold**, *Italic*, `Bulleted list`, `Numbered
  list`, `Quote`, `Link`, `Image`, `Divider`. Keyboard shortcuts on web and desktop:
  `Cmd/Ctrl-B`, `-I`, `-K` (link).
- `Link`: a dialog with the URL (`https://` only, the [`message-links.md`](message-links.md)
  rule) and the text.
- `Image`: the system photo picker; the image is processed as a photo (section 3) and
  appears in place with an optional caption.
- No raw HTML or Markdown is accepted as input. Pasting rich text keeps only what the
  format can carry (headings, bold, italic, lists, quotes, links); everything else becomes
  plain text.
- Limits (chosen): 64 KiB of text in all, 20 images, lists nested at most two levels.
  Reaching a limit says so under the bar: `This article is at its length limit.`
- `Publish` sends. Drafts are kept on the device while the editor is open and if it is
  closed with `Save draft`; one draft per conversation.

### 11.2 The bubble

A card: the first image as a banner when there is one (the photo bubble's sizing), the
title (17/600, up to three lines), the first 200 characters of text as the excerpt (muted,
up to four lines), and a `Read article` row. No reading-time estimate is shown. Tap
anywhere opens the reader.

### 11.3 The reader

Full screen, the 720-px column on wide screens, the theme's text sizes (the person's text
size setting applies), headings 22/600 and 17/600, quotes with an accent rule on the
start edge, lists indented, images full column width with caption below, links in the
accent colour opening as message links do. The top bar has the author, the date, and
`Close`. The message menu (react, reply, forward, copy text, save, pin) is available
from the reader's top bar `…` button.

`Edit` on an article reopens the editor; the edit replaces the whole article.

## 11a. Forwarding these messages

Found 2026-11-18: until now every client forwarded only a message's text and files, so a
forwarded contact card, location or article arrived as an empty message with a forward
line. Now:

| Kind | `Forward` |
|---|---|
| Location, Contact, Article | Offered. The receiver gets the same content (an article's images re-encrypted like any attachment) with the ordinary forward line. A contact card is subject to section 10.1. |
| Poll, Checklist | Not offered. Votes and ticks belong to the conversation they were cast in; a copy without them would read as the same poll. |
| Photo or Video, File, Audio, Camera | Unchanged: forwarded as attachments. |

## 12. Data the UI needs (input to the ADR)

| UI need | What the core must carry |
|---|---|
| A bubble per kind, unknown kinds degrade | A body kind per new message type, an unknown one shown as `This message needs a newer version of Lakko.` (the existing unsupported-version sentence) |
| Audio bubble before download | `kind = audio`, `duration_ms`, `title`, `artist`, cover as `thumb` |
| Poll card | question, options, flags (anonymous, multiple, quiz), correct index + explanation (quiz), close time, a per-poll id |
| Poll totals | Fold of votes per (poll, voter) last-writer-wins, the voter's own choice, closed state |
| Anonymous vote | A way to vote that names nobody to the other members, and one vote per person checked by every member |
| A newcomer sees open polls and checklists | A hand-over of every open poll and checklist (body, author, current results or ring) from the member who admits them |
| Checklist | title, items with ids, two permission flags, ticks per item with who ticked (account key), added items |
| Location | latitude, longitude, accuracy, fix time, optional place name |
| Contact card | display name, username, account key |
| Article | title, a bounded structured body, images as attachment references |
| Channel polls | One vote per account through an anonymous credential and a per-poll tag at the channel service; counts per opaque option label; a frozen final tally in the post on close |

## 13. Cross-platform shape

| | Web / desktop | iOS | Android |
|---|---|---|---|
| Menu | `LakkoMenu` popover (wide) / sheet (narrow) | `.lakkoActionSheet` | `LakkoMenu` bottom sheet (replaces the Material `DropdownMenu`) |
| Photo or Video | `<input type="file" accept="image/*,video/*" multiple>` | `PhotosPicker` | Photo Picker (`PickMultipleVisualMedia`) |
| File / Audio | `<input type="file">` / `accept="audio/*"` | `.fileImporter` | `OpenMultipleDocuments` (`*/*` / `audio/*`) |
| Camera | `getUserMedia` view | `AVCaptureSession` view | CameraX view |
| Location | `navigator.geolocation` | `CLLocationManager` (one fix, kept updating while the screen is open) | `LocationManager` (no Play services dependency) |
| Open a location | `openstreetmap.org` in the browser (desktop: the host's `open-url`) | Apple Maps URL | `geo:` intent |
| Editors (poll, checklist, article) | Lakko screens | Lakko screens | Lakko screens |

Accessibility:
- Each menu row is a button labelled with its text; the disabled Camera / Location rows read
  their reason.
- A poll option is a radio or checkbox with its text; after voting each reads
  `{option}, {percent} percent, {count} votes`, and `your choice` when it is.
- A checklist item reads `{text}, done by {name}` or `{text}, not done`, toggled by
  activation.
- The location card reads the place name, then `Location, {coordinates}, accurate to {n}
  metres`.
- The article reader exposes headings as headings, lists as lists, images with their
  caption as the label (or `Image` when none).
- Every editor works with a hardware keyboard; the format bar is reachable by Tab on web.

## 14. Copy (catalog source, English)

| Key | Text |
|---|---|
| `attach.menu.photo_video` | Photo or Video |
| `attach.menu.camera` | Camera |
| `attach.menu.file` | File |
| `attach.menu.audio` | Audio |
| `attach.menu.poll` | Poll |
| `attach.menu.checklist` | Checklist |
| `attach.menu.location` | Location |
| `attach.menu.contact` | Contact |
| `attach.menu.article` | Article |
| `attach.menu.no_camera` | No camera on this device |
| `attach.menu.no_location` | Location isn't available on this device |
| `attach.preview.send_as_file` | Send as file |
| `attach.preview.video_refused` | This video can't be sent. Videos must be MP4 and up to 50 MB. Send it as a file instead. |
| `attach.preview.file_too_big` | Files up to 50 MB can be sent. |
| `attach.preview.photo_unreadable` | Couldn't read this photo |
| `camera.denied` | Lakko needs camera access to take photos |
| `camera.video_unsupported` | Video can't be recorded in this browser |
| `poll.editor.anonymous` | Anonymous voting |
| `poll.editor.anonymous_on` | Nobody, including you, will see who voted for what. Everyone sees the totals. |
| `poll.editor.anonymous_off` | Everyone in this chat will see who voted for what. |
| `poll.editor.multiple` | Multiple answers |
| `poll.editor.quiz` | Quiz mode |
| `poll.editor.explanation` | Explanation |
| `poll.editor.close_auto` | Close automatically |
| `poll.editor.discard` | Discard this poll? |
| `poll.kind.anonymous` | Anonymous poll |
| `poll.kind.public` | Poll |
| `poll.kind.quiz` | Quiz |
| `poll.kind.anonymous_quiz` | Anonymous quiz |
| `poll.votes` | {n, plural, =0 {No votes yet} one {# vote} other {# votes}} |
| `poll.vote` | Vote |
| `poll.closed` | Closed |
| `poll.closes_in` | Closes in {time} |
| `poll.retract` | Retract vote |
| `poll.close` | Close poll |
| `poll.close_confirm` | Close this poll? Nobody will be able to vote any more. |
| `poll.sending_vote` | Sending your vote… |
| `poll.vote_failed` | Your vote wasn't sent |
| `poll.view_results` | View results |
| `checklist.done_of` | {done} of {total} done |
| `checklist.add_item` | Add item |
| `checklist.others_add` | Others can add items |
| `checklist.others_mark` | Others can mark items done |
| `checklist.edit` | Edit checklist |
| `checklist.close` | Close checklist |
| `location.name_place` | Name this place |
| `location.accuracy` | Accurate to {distance} |
| `location.approximate` | Approximate location |
| `location.finding` | Finding your location… |
| `location.not_found` | Couldn't find your location. Try again outdoors or near a window. |
| `location.denied` | Lakko needs location access to send where you are |
| `location.send` | Send current location |
| `location.open_osm` | Open in OpenStreetMap |
| `location.copy` | Copy coordinates |
| `contact.no_username` | No username to share |
| `contact.add` | Add contact |
| `contact.this_is_you` | This is you |
| `contact.username_changed` | This username now belongs to someone else. |
| `contact.not_findable` | {name} can't be found by username. Ask {sender} to introduce you, or ask {name} for an invite link. |
| `audio.unplayable` | Couldn't play this file |
| `article.title` | Title |
| `article.publish` | Publish |
| `article.save_draft` | Save draft |
| `article.at_limit` | This article is at its length limit. |
| `article.read` | Read article |

`{time}` and `{distance}` are formatted by the platform for the person's locale.

## 15. Out of scope (v1)

Live location; choosing a place other than where one is; an album bubble; re-encoding a
picked video; sticker, GIF and emoji items in this menu (they are the emoji picker's);
comments with attachments in channels; scheduled messages; polls with images in options;
editing a poll; Wallet and Gift (no counterpart).
