<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec — the profile screen and the profile photo

**Status:** design (2026-10-26), the input to ADR-0084 and the three client builds.
**Owner surface:** Settings > Profile on web, iOS and Android; every place a person's
avatar circle is drawn.
**Decision:** the maintainer's (2026-10-26). The screen is rebuilt around the avatar,
the name and the username, each edited in place; a person has a profile photo or a
short profile video, carried end-to-end encrypted to the people they talk to.
**Replaces:** the Settings > Profile part of [`profile-names.md`](profile-names.md) §4
(the name field, the preview line, the read-only username with a `Change username`
button). Everything else in that spec stands. Its §9 listed avatars as out of scope with
no reason given; this spec brings them in.

## 1. Layout

One column, centred, the same on every platform and every width (on a wide web window
the column sits in the content pane, at most 480 px wide).

```
            ┌──────────┐
            │  photo   │
            │    or    │
            │ initials │
            └──────[✎]─┘          avatar 112 pt, edit button 32 pt on its lower right

          Alice Petrova  [✎]      row 2: the name, title weight

             @alice  [✎]          row 3: the username, secondary colour

        ┌────────────────────┐
        │        Save        │    full width of the column
        └────────────────────┘
```

- The edit buttons are icon buttons (a pencil), each with an accessible label:
  `Change photo`, `Change name`, `Change username`.
- There is no "How others see you" line and no separate username card any more: the
  screen itself is how others see you.
- No name set: row 2 shows `Add your name` in the secondary colour, with the same pencil.
- No username known on this device (a passkey sign-in with no name typed): row 3 shows
  `No username` in the secondary colour, without a pencil.

## 2. The photo

### 2.1 The menu

The avatar's edit button opens an in-app menu anchored to it (a popover on web and iPad,
a bottom sheet on phones; never the OS action sheet):

| Item | Shown when |
|---|---|
| `Photo or video` | always |
| `Camera` | the device has a camera (web: `enumerateDevices` lists a `videoinput`) |
| `Remove photo` | a photo or video is set (saved or pending); destructive colour |

`Photo or video` opens the platform's library picker (the one system surface the
platforms leave no way around without broad photo-library permission), filtered to
images and videos. `Camera` opens Lakko's own capture screen (§2.3).

### 2.2 Choosing the square

A picked or captured photo goes to a crop screen: the image fills the screen, a circle
mask shows what will be kept, the person drags to move and pinches (web: wheel or the
zoom slider) to zoom, between "fills the circle" and 4x. Buttons `Cancel` and `Use`.
`Use` produces a square JPEG of at most 640 x 640 px.

A picked video is accepted when it is an MP4 the device can play; it is shown in the same
crop screen (drag and zoom choose the square, the first frame is shown still), and the
person also chooses where its 10 seconds start with a strip of frames under it. `Use`
produces a square MP4 (H.264, no sound, at most 480 x 480 px, at most 10 s) plus a still
JPEG of its first frame. A device that cannot re-encode video (web without
`MediaRecorder` support for H.264 MP4) accepts a picked video only when it is already
square-cropped by the crop screen's centre default, MP4, at most 10 s; otherwise it says
`This video can't be used as a profile video. Try a shorter MP4 or a photo.`

Sound is never kept.

### 2.3 Camera

Lakko's own camera screen, the same capture code the video-note recorder uses: the front
camera in a circle, a switch-camera button, and one shutter with a `Photo` / `Video`
toggle under it. Photo: tap takes the picture and goes to the crop screen. Video: tap
starts, tap again or 10 s stops; a ring shows the time; then the crop screen. Permission
denied: the screen says `Lakko can't use the camera. Allow it in your settings.` with a
`Close` button.

### 2.4 After `Use`

The avatar shows the new photo (a video plays muted, looped) at once, as a pending
change: it is not sent until Save. `Remove photo` likewise shows the initials at once,
pending.

## 3. The name

- Tap the name's pencil (or the name itself): the row turns into a one-line text field
  holding the current name, focused, the text selected.
- Validation runs as the person types; a reason shows under the field (the reasons of
  [`profile-names.md`](profile-names.md) §4).
- Leaving the field (focus moves elsewhere, Return, Enter): the field closes and the row
  shows the new text. An empty field closes to `Add your name` (no name).
- Leaving while the text is invalid: the field stays open with its reason; nothing else on
  the screen is blocked.
- Escape (web, hardware keyboard): the field closes and the name returns to what it was
  before this edit.

## 4. The username

- Tap the username's pencil (or the username): the row turns into a text field holding
  the current username without `@`, focused, with `@` shown as a fixed prefix.
- Validation as at signup (`[a-z0-9_]`, 3-32, the reasons shown); uppercase is shown
  lowercased.
- No explanatory lines under the field (maintainer, 2026-10-26). There is no limit on how
  often a username can be changed (ADR-0076 D2 removed).
- Leaving the field: it closes and the row shows `@newname`, as a pending change.
  Leaving with an invalid value: the field stays open with its reason. Escape: revert.
- Whether a new username is free is checked on Save, not while typing (a check needs a
  token and reveals what was typed to the directory).

## 5. Save

- Disabled until something differs from what is saved (photo, name or username) and every
  open field is valid. Changing a field back to its saved value disables it again.
- Tap: the button shows `Saving…` and the edit buttons are disabled. In order:
  1. The photo (if changed) is encrypted and uploaded.
  2. The username (if changed) is changed. When the account needs a fresh proof
     (ADR-0073 §17.1) the existing proof block opens over the screen (password or
     passkey); cancelling it stops the save there, with the username still pending.
  3. The profile (name and photo) is published.
- Success: `Saved. Your contacts will see the change.` under the button for a few
  seconds; everything shown is now the saved state.
- A step fails: what already succeeded stays saved, the rest stays pending and the error
  shows under the button: `Couldn't upload the photo. Check your connection and try
  again.`, `@alice is taken. Choose another username.` (the username row reopens),
  `Couldn't save. Try again.`

## 6. Leaving with unsaved changes

When anything is pending and the person leaves the screen (back, another settings
section, another tab of the bottom bar or rail, a link), Lakko shows its own dialog, not
the system one:

> **Save changes?**
> Your changes to your profile will be lost if you don't save them.
>
> [Save] [Don't save] [Cancel]

- `Save` runs §5; when it succeeds the navigation continues; when it fails the person
  stays on the screen with the error.
- `Don't save` drops every pending change and continues.
- `Cancel` (and tapping outside, Escape, the Android back gesture on the dialog) stays.
- Closing or reloading the web tab: the browser's own "leave site?" prompt, the one case
  the platform leaves no other way.
- An app moved to the background keeps the pending state; it is not a navigation.

## 7. Where the photo shows

Every circle that today shows a person's initials (chat list, chat header, contacts list,
contact detail, members lists, people search, request cards, message authors in groups,
call tiles without video, incoming-call screen) shows the person's photo when this device
holds it, else the initials. A profile video never has sound. It plays looped:

| Where | Mouse or trackpad (can hover) | Touch only |
|---|---|---|
| Large views (contact detail, a person's info sheet, Settings > Profile) | always | always |
| Chat header | while the pointer is over the header's avatar and name | always |
| Chat list row, contacts list row | while the pointer is over the row | never, the still |
| Everywhere else | never, the still | never, the still |

A touch on a hover-capable screen does not count as hovering. The video is downloaded the
first time it is to play; until it is ready the still stays, no spinner. When the pointer
leaves, the still comes back at once. The same rule applies to group and channel photos
where they appear in these places.

| State | UI |
|---|---|
| Profile says there is a photo, not downloaded yet | Initials; the photo replaces them when it arrives, no spinner |
| Download or decryption failed | Initials; tried again on the next display after 1 minute |
| Photo removed by its owner | Initials on next receipt of the profile |

The owner's own avatar shows in the Settings header, the rail and the profile screen.

## 8. Cross-platform shape

One state model on every client: `saved` (photo, name, username) and `pending` (each of
the three either unchanged or a new value), with Save enabled by `pending != saved` and
every open field valid. Avatar rendering goes through each client's one avatar component
(web `ui/Avatar.tsx`, iOS `LakkoTheme` avatar, Android `LakkoAvatar.kt`), which takes the
person's key and looks the photo up.

## 9. Data the UI needs (input to the ADR)

- In the profile record: an optional photo reference (the still) and, beside it, an
  optional video reference; each is an encrypted blob in storage with its key.
- A local, per-device cache of downloaded avatars keyed by blob id.
- The time of the last username change (exists, ADR-0076).
