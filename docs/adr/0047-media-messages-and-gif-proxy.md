<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0047: Media messages, emoji picker and a server-side GIF proxy

- **Status:** Accepted — the GIF provider model is the maintainer's choice (2026-09-13:
  "as the old version did", i.e. GIPHY behind Lakko's server); the rest follows from it
  and from the UX spec.
- **Date:** 2026-09-13
- **Context:** Phase 3 product completeness. UX spec:
  [`docs/ux/media-messages.md`](../ux/media-messages.md).

## Context

The first manual end-to-end run of the clients found that voice messages, video notes,
an emoji picker and GIFs do not exist on any client. The attachment pipeline does:
`lakko-attachments` encrypts a file, `services/storage` holds the ciphertext blob, and the
chat `Envelope` carries an `AttachmentRef { blob_id, key, nonce, mime, name, size }`.

Four forces:

1. **The envelope is attacker-controlled.** Every field arrives from a group member inside
   an MLS application message. Any metadata a player trusts must be bounded and
   validated, and inline playback must never be decided by a sender-chosen label alone.
2. **Three independent clients must play each other's recordings** without a transcoder
   on any device and without a server that can see media.
3. **Older clients must not break.** `parse` degrades an unparseable envelope to raw text,
   so a new field that makes old decoders fail would render a voice message as JSON.
4. **GIF search is third-party content.** Any search reaches a provider; the question is
   who learns what. The maintainer's previous app routed search through its own server to
   GIPHY but loaded preview thumbnails straight from GIPHY, leaking every viewer's IP.

## Decision

**Media messages are ordinary encrypted attachments with an optional kind and bounded
metadata; recordings use one MP4 format per kind on every platform; GIF search and GIF
media go through a dedicated, stateless Lakko proxy service, and the picked GIF is sent as
an encrypted attachment.**

### 1. Envelope: optional fields on `AttachmentRef`, no wire-version bump

Added to `AttachmentRef`, each `#[serde(default, skip_serializing_if = "Option::is_none")]`:

| Field | Type | Bound |
|---|---|---|
| `kind` | `Option<String>` | one of `voice`, `video_note`, `gif`; any other value is kept as unknown and renders as a file |
| `duration_ms` | `Option<u32>` | at most 15 min for `voice`, 60 s for `video_note`; out of range is dropped |
| `waveform` | `Option<String>` | base64 of at most 128 bytes; oversize is dropped |
| `width`, `height` | `Option<u32>` | each at most 4096; out of range is dropped |
| `thumb` | `Option<ThumbRef>` | a second encrypted blob `{blob_id, key, nonce, mime, size, width, height}`, `size` at most 256 KiB |

Rules, enforced in `lakko-attachments` so all three clients inherit them:

- **Metadata that fails its bound is dropped; the attachment survives.** A malformed
  optional field must never fail the envelope (force 3).
- `kind` is a **string, not a closed enum**, for the same reason: an unknown future kind
  must decode.
- **`WIRE_VERSION` stays 1.** The crate's existing rule is that later additions ride as
  optional fields and receivers ignore unknown fields.
- **Inline playback is gated on an allowlisted MIME type**, never on `kind`:
  `audio/mp4`, `video/mp4`, `image/gif`, `image/webp` (plus the existing image set).
  A `kind: voice` with `mime: text/html` is a file chip.
- Clients always set `name` (e.g. `voice-<timestamp>.m4a`), so an old client degrades to a
  named file with Save.
- The `message_body_parse` fuzz target is extended to cover the new fields.

"Played" is local state and is never sent; it is not a read receipt.

### 2. Recording format: one per kind, identical everywhere

- Voice: **AAC-LC in MP4** (`audio/mp4`).
- Video note: **H.264 + AAC in MP4** (`video/mp4`), square, at most 480 px, at most 60 s.

iOS (`AVAudioRecorder`, `AVAssetWriter`) and Android (`MediaRecorder`, CameraX) record
these natively. On web, `MediaRecorder.isTypeSupported` was checked in Chrome 152 and
reports `audio/mp4`, `audio/mp4;codecs=mp4a.40.2`, `video/mp4` and
`video/mp4;codecs=avc1.42E01E,mp4a.40.2` as supported. A browser that cannot record MP4
**disables recording** rather than sending WebM/Opus that some clients cannot play;
playback is unaffected.

### 3. Emoji: bundled data, one-grapheme reactions

Each client ships emoji data with the app and never fetches emoji over the network.
Reactions accept any single emoji grapheme; the protocol already allows any string up to
`MAX_EMOJI_BYTES` (64), so no wire change — the six-emoji bar becomes the picker's
shortcut rather than a separate model. Clients validate "exactly one emoji grapheme"
before sending.

### 4. GIFs: a dedicated proxy service

A new stateless service, **`services/gif-proxy`** (`lakko-svc-gif-proxy`):

- **Auth:** the same HMAC session token `storage` and `ds` verify
  (`LAKKO_SESSION_TOKEN_SECRET`, `lakko-auth-token`). Only signed-in accounts can search.
- **`GET /gifs/search?q=&offset=`** and **`GET /gifs/trending?offset=`**: query GIPHY
  with the server's key (`LAKKO_GIF_GIPHY_API_KEY`), `rating=pg-13`, a fixed page size;
  return items as `{id, preview_url, full_url, width, height}` where **both URLs point back
  at the proxy**, never at GIPHY.
- **`GET /gifs/media/{id}/{variant}`**: the proxy resolves the id against GIPHY and streams
  the file. No client-supplied upstream URL is ever fetched (the previous app accepted a
  `?u=` URL and relied on a host suffix check, which is an SSRF surface to reason about;
  an id-based route removes it). Response size capped (preview 2 MiB, full 8 MiB);
  content type restricted to `image/gif`, `image/webp`, `video/mp4`.
- **Rate limit per account**, in memory, sized for a debounced search box.
- **No persistence and no query logging.** Queries and account ids must not appear in
  logs at any level; request logs carry route and status only.
- **Not configured** (no API key) → `503 { error: "gif-provider-not-configured" }`, which
  clients map to a hidden GIF tab.
- **Web CSP:** previews load from the proxy's same-origin path (`/api/gifs/...` through
  nginx), so `img-src 'self'` needs no third-party host.

After a pick, the client downloads the full GIF through the proxy, encrypts it, uploads it
to storage and sends an attachment with `kind: gif`. **Recipients never contact the proxy
or GIPHY.**

**What the Lakko server learns, stated once in the UI** (UX spec §5): the search terms and
timing of a signed-in account. GIPHY learns only that Lakko's server searched. That is a
real cost and it is surfaced, not buried.

### 5. History bundles carry the same metadata (added 2026-09-14)

The multi-device history bundle does not replay chat envelopes: it carries a per-message
display projection, `lakko_mls::history_placeholder`, and version 1 of that codec had no
media fields. A voice message, video note or GIF restored on a newly paired device therefore
came back as a plain file.

- The placeholder becomes **version 2**: each attachment gains optional `kind`,
  `duration_ms`, `waveform` (raw bytes), `width`, `height` and a `thumb`
  `{blob_id, key, nonce, size, mime, width, height}`, each behind a presence byte.
  The decoder refuses version 1; nothing is deployed, so there is no dual reader.
- `lakko-mls` enforces **structure only** (presence bytes, `kind` <= 32 bytes, waveform
  <= 128 bytes, the existing caps, no trailing bytes). It does not depend on
  `lakko-attachments`, so it cannot know the semantic bounds.
- The **semantic** bounds are applied in both FFI crates on encode and on decode through
  `lakko_attachments::RawMediaMetadata::bounded`, which shares its checks with the
  envelope sanitizer. A test pins the two routes to the same result, so a restored
  message keeps exactly what the live envelope kept.
- Invariant #3: this is a codec change in `lakko-mls`, not a state-machine change. It
  carries unit tests for the new fields and a property test that any placeholder within
  the caps round-trips byte-stably; the existing `history_placeholder` fuzz target already
  asserts decode/encode round-trip stability over arbitrary input.
- **Version 3 (2026-11-13):** the placeholder also carries the channel invitation card
  (ADR-0087 section 14, opaque, 1 to 2048 bytes), the reply (ADR-0052: target `messageId`,
  author and excerpt snapshot) and the forward (author snapshot, origin group id of 1 to
  96 bytes and `messageId`); version 2 is refused. Seen live: a second device restored an
  invitation as its bare `/g/` link.

### 6. A micro thumbnail inside the message (added 2026-11-19, maintainer)

A notification must show the picture of a photo or video message on every platform, and
the background paths that draw notifications (the iOS notification extension, the web
service worker, Android's background decrypt in a process the wake started, the desktop
host) cannot fetch the `thumb` blob: a blob read needs a read pass bought with an anonymous
token (ADR-0065), and those paths hold no token and may not spend one (C-59). So the
**first** photo or video of a message also carries its picture **inside the envelope**:

- `inline_thumb`: an optional lenient field on that `AttachmentRef` (no wire-version
  bump), standard base64 of a JPEG whose longest side is at most 96 px, at most 4096 bytes
  decoded (`MAX_INLINE_THUMB_BYTES`; size and pixel limit chosen, not measured). Set only on
  the first photo or video of a message; a ref carrying it anywhere else, or a value that
  is not a JPEG (starts `FF D8 FF`) within the bound, has the field dropped alone, as every
  ADR-0047 field is. Composed by the sender from the same decode that makes `thumb`.
- It is end-to-end encrypted with the rest of the message (MLS application message); the
  server sees nothing new. The message grows by at most about 5.5 KB of base64.
- Uses: the notification picture on every platform; the chat list row's 20 px thumbnail
  (no fetch at all); the placeholder of the first mosaic tile until `thumb` arrives.
- The notification shows the picture exactly when it shows the text. iOS and macOS take a
  notification picture only as a file: the iOS extension and the desktop host write it to a
  temporary file inside their own container, hand it to the system, and delete their copy
  once the system has taken it (iOS moves it into its notification store; the desktop host
  deletes after delivery and sweeps its temporary folder at launch). Android and web pass
  the bytes in memory.
- History bundles (section 5) and the conversation vault carry the field like the others:
  `lakko_mls::history_placeholder` version 5 and the `lakko-vault` record version 3 add it
  per attachment (opaque, at most 4096 bytes; flag day, the earlier versions are refused),
  and the FFI crates apply the JPEG check and the first-image-or-video rule on both routes
  (`lakko_attachments::inline_thumb_carrier`). On the FFI it is `inline_thumb` /
  `inlineThumb` on the attachment record, and the notification extension's `ParsedBody`
  carries the first media's bytes as `inline_thumb` (built 2026-11-19, host-tested only).

## Alternatives

- **Search directly from the client to GIPHY.** Simplest; GIPHY (and its owner) learns
  every query with the user's IP and timing, and web CSP would need a third-party
  `connect-src`/`img-src`. Rejected: it is strictly more exposure than the proxy for the
  same feature.
- **No search; GIFs only from the device.** No new server and no search leak. Rejected
  by the maintainer's direction, which asked for the provider the old version had.
- **Put the proxy inside `services/storage`.** No new unit to deploy. Rejected: storage is
  an opaque ciphertext store with no third-party egress today, and giving it an outbound
  HTTP client, a third-party API key and content handling widens the trust surface of the
  one service that holds every encrypted blob.
- **Send the GIPHY URL in the message instead of re-uploading.** Smaller messages;
  every recipient's client would fetch from GIPHY or the proxy on render, leaking who
  viewed what. Rejected.
- **A closed `MediaKind` enum and a wire-version bump.** Rejected per force 3: an old
  decoder failing on an unknown variant shows the envelope as raw JSON.
- **Record WebM/Opus on Chrome and transcode on receipt.** Rejected: no client should carry
  a transcoder, and MP4 recording is available where it was checked.

## Consequences

- **Positive:** the four features exist on all three clients with no change to MLS,
  storage, or the DS; media stays end-to-end encrypted; old clients still show a usable
  file; the previous app's IP leak to GIPHY is not repeated.
- **Negative:** a new service to deploy (systemd unit, nginx route, the deploy list, an
  API key); the Lakko server sees GIF search terms per account; a browser without MP4
  recording cannot send voice messages or video notes; GIPHY's terms require visible
  attribution.
- **Neutral:** invariant #2 is unchanged for message content (the proxy never sees
  message plaintext or keys, and recipients never touch it); invariant #5 is unchanged
  (the proxy learns nothing about who talks to whom). Invariant #3 is untouched — no
  `lakko-mls` change.

## Open questions

- GIPHY API key provisioning and the production rate-limit numbers.
- Whether a failed thumb upload should send the video note without a poster (current
  answer: yes; the bubble shows a neutral circle).

## References

- [`docs/ux/media-messages.md`](../ux/media-messages.md)
- [ADR-0023](0023-message-edit-delete-reactions.md) — the envelope's optional-field extension rule and reactions
- `crates/lakko-attachments/src/lib.rs` — `AttachmentRef`, `Envelope`, `MAX_ATTACHMENT_BYTES`
- The maintainer's previous app: `lakko_me/server/src/messenger/ws.js` (`/api/messenger/gifs`)
