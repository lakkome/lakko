<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0084: The profile photo — an encrypted blob referenced from the profile

- **Status:** Accepted (2026-10-26, maintainer: the photo or video is an
  encrypted storage blob and the profile carries a reference to it). Built
  2026-10-26 in `lakko-contacts`, both FFI surfaces, the NSE length
  constants and web / iOS / Android; host-tested only (unit suites and
  builds), never run in a browser, on a device or against a server. Android
  adds Media3 1.4.1 for the video re-encode; Firefox records no H.264 MP4,
  so there a picked video goes through the no-re-encode rule.
- **Date:** 2026-10-26
- **Context:** UX spec [`docs/ux/profile-screen.md`](../ux/profile-screen.md);
  the profile envelope ([ADR-0048](0048-profile-names.md), v3 since
  [ADR-0060](0060-scoped-discovery.md)); encrypted attachments and media
  metadata ([ADR-0047](0047-media-messages-and-gif-proxy.md)); attachment file
  lifetime ([ADR-0065](0065-attachment-lifetime-and-access.md)); servers
  cannot link accounts ([ADR-0078](0078-servers-cannot-link-accounts.md))
- **Amends:** ADR-0048 (the envelope gains an avatar, version 4)

## Context

A Lakko profile carries a name and a username, nothing else. The UX spec for
profile names listed avatars as out of scope without a reason. The maintainer
wants a profile photo, or a short profile video, set from Settings > Profile
and shown wherever a person's circle is drawn.

A profile envelope travels in an MLS application message to every group the
person is in, in contact requests and in channel member cards. It is a few
hundred bytes; a photo is tens to hundreds of kilobytes and a video more.

## Decision

### 1. The bytes live in storage, encrypted as an attachment

The still (a square JPEG, at most 640 x 640 px, at most 256 KiB) and, for a
profile video, the video (a square H.264 MP4, no audio track, at most
480 x 480 px, at most 10 s, at most 4 MiB) are each encrypted with
`lakko_attachments::encrypt_attachment` (a fresh key and nonce each) and
uploaded with `PUT /blob/{hex}` like any attachment. The server stores
ciphertext under its SHA-256 and learns nothing about the content.

### 2. The profile carries a reference, version 4 (flag day)

After the grants, a v4 profile carries one more field:

```text
    || avatar_len (1 B: 0 | 84 | 170) || avatar

    avatar      = still_ref                                  (84 B)
                | still_ref || video_ref || duration_ms      (84 + 84 + 2 B)
    *_ref       = blob_id(32) || key(32) || nonce(12) || size(4 BE) || width(2 BE) || height(2 BE)
    duration_ms = 2 B BE, 1..=10000
```

Bounds, each a whole-envelope refusal as for every other profile field:
`width == height`; the still 1..=640 px and 1..=262144 bytes; the video
1..=480 px and 1..=4194304 bytes; `duration_ms` 1..=10000. The format is
implied by the slot (JPEG still, MP4 video), so no MIME type travels. A
version-3 envelope is refused (no production deployment; no dual accept).

The photo belongs to the profile: it is changed and removed by publishing a
new profile, ordered by the existing `updated_at_ms` (strictly greater wins,
per signature key). Removing the photo is a profile with no avatar.

### 3. Receivers download, verify, cache

A receiver that keeps a profile with an avatar downloads `GET /blob/{hex}`,
checks the size, decrypts, and checks that the still decodes as an image and
the video as an MP4 before showing either; anything else is dropped and the
initials stay. Decrypted avatars are cached per device keyed by blob id; a
blob already held is never fetched again. Downloads are lazy (on first
display), never from the background notification paths, which keep showing
initials.

### 4. One avatar, reused everywhere

The same blob is referenced from the profile in every group, request and
member card. Re-encrypting per conversation would hide from storage that two
downloads are of the same person's photo, at the cost of one upload per group
on every change; not done (see residuals).

## Consequences

- `lakko-contacts::profile`: version 4, `Profile.avatar: Option<Avatar>`,
  `AvatarRef`, the bounds above, fuzz `profile_decode` extended. The NSE
  surface's length constants change.
- Both FFI surfaces carry the avatar on `composeProfileEnvelope` /
  `decodeProfileEnvelope`.
- Every client: the profile screen of the UX spec, capture, crop, encode,
  upload, publish; and a per-device avatar cache behind its one avatar
  component.
- No server change.

## Residuals

- **Storage sees who fetches whose photo, by network address.** Blob reads
  carry no account, but every contact of a person fetches the same blob id,
  so an operator correlating addresses and times can cluster a person's
  contacts. Attachments in one group have the same property; a profile photo
  is a cross-conversation one. Option 4 above would remove the shared id.
- **Lifetime.** ADR-0065's 365-day expiry and `POST /blob/keep` are not
  built; when they are, the owner's devices must keep the current avatar
  blobs alive. An old avatar blob is not deleted on change (ADR-0065's delete
  key is not wired to storage either).
- **Anyone a profile reached keeps the photo** after the owner changes it, as
  with any message.
