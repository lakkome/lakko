<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: saved messages, and how long files are kept

Owner: [ADR-0065](../adr/0065-attachment-lifetime-and-access.md). Status: with
the ADR (Proposed). Applies to web, iOS and Android identically. Extends
[`message-actions.md`](message-actions.md) (one new action) and
[`media-messages.md`](media-messages.md) (one new state).

## 1. User goals

- "I want to keep this message where I can find it again." A private
  collection across all chats.
- "I do not want the photos and files that matter to me to disappear." Files
  nobody opens are removed after a year; the person needs one obvious way to
  say "keep this".
- "I deleted my message; its file should be gone too."

## 2. Save: the action

A new row in the message menu, directly under **Pin**:

- **Save** on a message that is not saved; **Remove from saved** on one that
  is. One tap, no sheet, no confirmation. Offered on any message that is not
  deleted, in any chat the person can read.
- A saved message carries a small saved mark (icon and accessibility label
  "Saved") beside its time. Only the person sees it.
- Saving is private and belongs to the account: nothing is sent to the chat,
  and the person's other devices show the same saved messages.

The menu order becomes: Reply, Forward, then Copy, Select, then Pin, Save,
Edit, then Delete.

Select mode gains **Save** beside Delete and Forward, acting on the set.

## 3. Saved: the screen

Reached from the menu of the Chats screen (the one that holds New group), as
**Saved**.

- A list, most recently saved first. Each row: the chat's title, the author,
  the message text or, for media, its thumbnail and kind ("Photo", "Voice
  message", "File: name"), and the message date.
- Tapping a row opens the chat at that message. Long-press (right-click on
  web) offers **Remove from saved**.
- Empty state: "Messages you save appear here."
- A saved message this device does not hold yet (its chat's history has not
  loaded) shows its row from what was saved with it: chat, author, excerpt.
  It opens once the message is there.

What saving does not do: it does not copy the message. If its author deletes
it for everyone, it leaves Saved, as it leaves the pinned bar.

## 4. How long files are kept

The rule the person lives with, stated once:

- A file attached to a message stays available as long as someone opens it at
  least once a year.
- Files of **saved** messages and of **pinned** messages (for everyone or for
  me) are kept regardless, for as long as they stay saved or pinned.
- Deleting your own message for everyone removes its files at once.

The message itself is never removed by this rule; only the file is.

### 4.1 A file that is no longer available

One state, the same for a photo, a video, a voice message, a video note, a
GIF and a file, on all three clients. In place of the media:

> This file is no longer available.
> Files nobody opens for a year are removed. Save or pin a message to keep
> its files.

No Retry button: trying again cannot help. The rest of the message (text,
author, time, reactions) is unchanged. The second and third sentences are
shown under the first in the secondary text style.

This replaces today's behaviour, where a missing file shows the generic
"Couldn't load photo. Try again." and the person retries for nothing.

### 4.2 Limits

Sending and opening files are limited per day so the service cannot be used
as free storage by someone else. The limits are set so that a person does not
meet them in ordinary use.

- Sending, limit reached: the attachment is not sent and the composer shows
  "You have reached today's limit for sending files. Try again tomorrow." The
  text of the message, if any, stays in the composer.
- Opening, limit reached: the media shows "Too many files opened today. Try
  again later." with **Try again**.
- Either, when the limit service cannot be reached: the existing generic
  failure with **Try again**.

## 5. Controls and what each drives

| Control | Effect |
|---|---|
| **Save** / **Remove from saved** | adds or removes the message in the account's saved list; real, carried to the person's other devices |
| Saved row tap | navigation only |
| Pin (unchanged) | additionally keeps the message's files |
| Delete for everyone (unchanged) | additionally removes the message's files from the server |

## 6. Data the UI needs

- Per message: saved or not (for the row label and the mark).
- The saved list: per entry the chat, the message id, when it was saved, and
  a snapshot (author, excerpt, attachment kind and thumbnail reference) so a
  row can be drawn before the message is held.
- Per attachment: one of loading / shown / **gone** / failed (retryable) /
  limited. Gone and failed must be distinguishable, which they are not today.
- For sending: whether an upload was refused for the daily limit.

## 7. Cross-platform shape

One model: a menu row, a mark, a list screen, one "gone" state. The usual
platform differences only (long-press vs right-click; SF Symbol / Material
icon / inline SVG for the mark).

## 8. Copy checklist

No protocol or component names; sentence case; no exclamation marks. All
strings are catalog keys (ADR-0061). The existing key
`chat.attachment.error.gone` is replaced by the three lines of 4.1.
