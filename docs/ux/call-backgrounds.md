<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Camera background in calls — UX spec

Written 2026-11-07. People in video calls asked for what Zoom has: a blurred
room, or a picture behind them. Nothing of the kind exists on any client today.

The maintainer's decisions, 2026-11-07:

- **What is offered:** blur (light and strong), a set of built-in backgrounds,
  and the person's own photo.
- **Where the choice lives:** on this device, until the person changes it. It is
  not synced to the account's other devices and never sent to anyone: the others
  see the result, not the setting (the rule of the noise suppressor,
  [`calls.md`](calls.md) section 4.1.3).
- **How the person is cut out of the picture:** one segmentation model run by
  Lakko's own code on every client (web, desktop, iOS, Android); the platform
  engines (Apple Vision, Google ML Kit) are the fallback if the result is not
  good enough. The engine is the ADR's subject, not this spec's: nothing in this
  spec depends on it.

## 1. What the person gets

A person in a video call can replace what is behind them with one of:

| Choice | What the others see |
|---|---|
| `None` | The camera as it is. The default |
| `Blur` | The room blurred enough to keep its colours and hide its details |
| `Strong blur` | The room blurred so that nothing in it can be told apart |
| A built-in background | One of six pictures drawn for Lakko |
| `Your photo` | A picture the person chose from their own library |

The person stays sharp in front of it. The edge around hair and hands will not
be perfect; nothing here promises that it is.

## 2. Where it is chosen

### 2.1 In a call

The call's Settings sheet ([`calls.md`](calls.md) section 4.1) gains a section
headed `Background`, under `Sound` and above the device lists:

```
Background
  [ preview of my camera, as the others see it ]
  ( None ) ( Blur ) ( Strong ) ( img ) ( img ) ( img ) ( img ) ( img ) ( img ) ( my ) ( + )
  The others see this behind you. It stays on this device.
```

- **The preview** is the person's own camera with the chosen background, at the
  sheet's width, 16:9, **not mirrored**: it shows exactly the picture the others
  receive. (The self-view tile on the call screen stays mirrored as it is today,
  background included, as in every other messenger.)
- **The strip** is one horizontal row of round-cornered square thumbnails,
  scrolling sideways when it does not fit: `None`, `Blur`, `Strong blur`, the six
  built-in backgrounds, the person's own photos (newest first), then `+`. The
  selected thumbnail carries the selection ring of the design system and is read
  as selected by the screen reader. `None`, `Blur` and `Strong blur` are drawn
  as a crossed circle and two blurred swatches, with their names under them.
- **A tap applies at once**, to the preview and to what the others see, without
  renegotiating the call and without a gap the person must wait out. There is
  no Save button. The choice holds for every later call on this device.
- **The section is in the sheet on every call kind** (one-to-one, group, link
  call), voice calls included.

### 2.2 Choosing before anyone sees you

A video call starts with the camera on ([`calls.md`](calls.md) section 3.2), so
the first time someone wants a background the others may already have seen the
room. Two things answer that:

- The choice holds across calls, so it is set once.
- **With the camera off, the sheet still shows the preview.** Opening the
  `Background` section with the camera off starts the camera for the preview
  only: nothing is sent, the camera button stays off, and the preview carries
  the line `Only you see this. Your camera is off.` Closing the sheet stops the
  camera again. The person picks a background, closes the sheet, and turns the
  camera on with the background already applied.

A voice call works the same way: pick, close, press the camera button.

No section of the app's own Settings carries the picker in this version; a
person who wants to set it without a call can start a call and keep the camera
off. Revisit if people ask for it.

### 2.3 Their own photo

`+` opens the platform's photo library picker, the same one the profile photo
uses ([`profile-screen.md`](profile-screen.md)); there is no other way to read
a photo from the library. After a pick:

- The photo is added to the strip and selected at once.
- It is stored on this device only, scaled down so its longer side is at most
  1920 pixels. It is never uploaded anywhere and never sent as a file: the
  others receive only the camera picture with the person in front of it.
- It fills the picture edge to edge, cropped from the centre when its shape
  differs from the camera's. No crop editor in this version.
- At most twelve own photos are kept. Adding a thirteenth removes the oldest one
  that is not selected.
- Long-press (touch) or right-click (web, desktop) on an own photo offers
  `Remove`, with no confirmation (the photo is still in the person's library).
  Removing the selected one selects `None`.

A picked file that cannot be read as a picture leaves the strip unchanged and
shows under it: `This photo can't be used. Try another one.`

## 3. States

| State | What the person sees |
|---|---|
| `None` selected | The camera as it is. Nothing is processed |
| A background selected, working | The chosen picture behind them in the preview and on their self-view tile. No badge on the camera button, no indicator on any tile |
| A background selected, the camera off | Nothing is sent, as today. The choice is kept for when the camera comes on |
| A background selected, it is starting (the model loading on the first use in this app run) | The camera button shows its normal pressed state, but **no picture is sent** until the background is ready: the self-view tile shows the person's avatar with a small spinner, and the preview says `Preparing background...`. When it is ready the picture starts with the background already in it. The camera is never sent without the background while one is selected |
| The background failed (the model could not load, the processing stopped, or this device stays too slow to keep up) | The camera is turned off, as if the person had pressed the button, and a line appears on the call screen for a few seconds: `Background stopped working, so your camera was turned off.` The Settings sheet's preview shows the same line with the button `Turn on camera without background`, which selects `None` and turns the camera on. The selection itself is not changed silently: the person chose to hide the room, and only the person decides to show it |
| This device cannot run a background at all (no graphics support the processing needs) | The `Background` section is absent, rather than a picker that does nothing ([`calls.md`](calls.md) section 4.1) |

The failure state is the security-relevant one. Someone who blurred the room did
so to hide it; a background that silently falls back to the raw camera would
show the room at the worst moment. So the rule, on every client: **while a
background is selected, a frame the background was not applied to is never
sent.**

## 4. What it covers

- The camera picture **everyone** in the call receives, at every size (in a
  group call the camera goes out as several copies of different sizes; every
  copy carries the background, there is no size on which the room shows).
- The person's own self-view tile and the preview.
- **Not** a shared screen ([`screen-share.md`](screen-share.md)): a screen is
  sent as it is.
- **Not** what others send: each device processes only its own camera.

## 5. Built-in backgrounds

Six pictures drawn for Lakko, not photographs and not licensed stock: calm
abstract compositions in the brand palette of the design system (soft shapes and
gradients), two light, two mid, two dark, so the person's face reads against
each. They ship inside each client and are the same on all of them. Each has a
spoken name for the screen reader (`Background: morning`, `Background: dusk`,
...); the names are catalog strings.

## 6. Copy

| Where | Text |
|---|---|
| Section heading | `Background` |
| Hint under the strip | `The others see this behind you. It stays on this device.` |
| Thumbnails | `None`, `Blur`, `Strong blur`, the six names, `Your photo`, `Add photo` |
| Preview with the camera off | `Only you see this. Your camera is off.` |
| Starting | `Preparing background...` |
| Failure (call screen and preview) | `Background stopped working, so your camera was turned off.` |
| Failure button | `Turn on camera without background` |
| Unusable photo | `This photo can't be used. Try another one.` |
| Remove action | `Remove` |

No word about models, machine learning, AI or segmentation: the person needs to
know what the others see, not how.

## 7. Accessibility

Every thumbnail is a button with a spoken name and a selected state; the strip
is reachable by keyboard (web, desktop: arrow keys move along it, Enter or Space
selects) and by swipe (VoiceOver, TalkBack). The failure line is announced. The
preview carries the spoken label `Your camera as the others see it`.

## 8. Data this needs

| Need | Why |
|---|---|
| A per-device stored choice: `none`, `blur`, `strong-blur`, `builtin:<id>`, or `photo:<local id>` | Section 2.1: the choice holds across calls on this device |
| A per-device store of up to twelve photos, behind the account's store gate (ADR-0077) and wiped with the account on the device | Section 2.3; a photo of the person's home is personal data |
| A processing stage between the camera and every camera sender, with a state the call screen reads: off, starting, working, failed | Section 3. Sits before the camera copies and before the frame encryption, so the server and the encryption are not touched |
| Nothing on the wire, nothing on any server | The setting is never sent; the others receive an ordinary camera picture |

## 9. Out of scope here

A crop or position editor for own photos, video backgrounds, a picker in the
app's own Settings outside a call, light or appearance correction (Zoom's "touch
up"), avatars or masks in place of the face, syncing the choice or the photos
between the account's devices, and applying a background to anything but the
camera.
