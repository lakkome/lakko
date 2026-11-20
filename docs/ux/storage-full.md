<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: the server has no room for new files

**Status:** accepted 2026-11-19 (maintainer). Server built the same day
(`services/storage/src/disk.rs`, route-tested); web and desktop, iOS and Android the same
day, unit-tested, not walked. The profile photo has no file-limit sentence on any client
(the limit there shows the generic upload error); only `storage-full` got its own.
**Surfaces:** web and desktop, iOS, Android.

## 1. What happens

The storage service refuses an upload that would leave less free disk than its floor
(`LAKKO_STORAGE_MIN_FREE_BYTES`, 10 GiB by default): `PUT /blob/{id}` answers
`507 {"error":"storage-full"}`, before the upload pass is charged. `PUT /backup` answers
`507` with the body `storage-full`. Messages, calls and everything else keep working; only
new files wait.

## 2. What the person sees

It is shown exactly where the existing file-sending limit (`429
blob-upload-budget-exhausted`) is shown, with its own words, and behaves the same way
(the message text and the chosen file stay; trying again is the person's choice):

| Where | Key | Text |
|---|---|---|
| Composer note, a message with attachments, a voice or video message, a GIF | `chat.attachment.full.send` | The server has no room for new files right now. Try again later. |
| Group photo | `group_photo.error.full` | The server has no room for new files right now. Try again later. |
| Profile photo | `profile.save.error.full` | The server has no room for new files right now. Try again later. |

Any other upload that already maps the file-sending limit to a sentence maps
`storage-full` to the matching one of these. An upload with no such mapping keeps its
generic error.

## 3. Without a person

The encrypted account backup and other background uploads treat `507` like any other
failed upload: nothing is shown, the next scheduled attempt tries again. Nothing is
retried in a loop.

## 4. What it does not say

No amounts, no "server full" banner, no notice to people who are not uploading.
