<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0051: Group and channel names carried end-to-end encrypted to every member and subscriber

- **Status:** Proposed
- **Date:** 2026-09-17
- **Context:** Phase 3 product completeness. UX spec:
  [`docs/ux/group-and-channel-names.md`](../ux/group-and-channel-names.md).

## Context

A group name and a channel name exist only on the device that typed them. On
web the group name is an in-memory label that a reload turns into
`Group aB3xY9`; Android keeps it in local preferences; iOS has no group name
at all. A channel's name and description are local fields on all three
clients; a subscriber's row is titled with the `@handle`. Observed live on
2026-09-16: a group created as "Trio" on Android was "Anna, Web Wanda" on iOS
and "Anna, Ivan IOS" on web, and a channel created as "Lakko News" was
"lakkonews0916" on both natives.

Constraints:

1. **Invariants #1 / #2:** a name is content; the server must never see it.
2. **Invariant #5:** no server-side store that says which accounts share a
   name.
3. **Impersonation:** a group name is a shared fact one person asserts about
   the whole group; only the people who manage the group may set it, and the
   receiver must check that, not the sender's claim.
4. **Newcomers:** MLS gives a member added later no access to earlier
   messages, and a channel subscriber holds seeds for a bounded window of
   epochs (ADR-0031, four) over posts the server keeps for thirty days; a name
   sent once is not enough.
5. **No production:** formats can change without migration.

What already exists and is reusable:

- Domain-separated service envelopes ride MLS application messages and are
  classified by `lakko_contacts::incoming_envelope` before the chat parser
  (profile, push routing secret, committer beacon). ADR-0048's profile
  envelope is the template: bounded, fail-closed, `updated_at_ms` ordering,
  never a bubble, republished on group growth.
- The ADR-0022 role table, read on the receive side exactly as the push
  routing gate reads it (owner, admins, no table, unreadable).
- A channel post is an Ed25519-signed, AEAD-sealed byte string with **no
  content discriminator**: readers decode the plaintext as UTF-8 and render
  it (`crates/lakko-channels/src/post.rs`; web `PostCard.tsx`, iOS
  `ChannelDetailScreen.swift`, Android `ChannelsScreen.kt`).

## Decision

**A group name is a bounded, domain-separated envelope on an MLS application
message, accepted only from the owner or an admin at receipt and ordered by
its stamp; a channel name and description are a bounded, domain-separated
record inside an ordinary channel post, recognised by readers before the text
path and re-issued by the publisher after every rotation.**

### 1. Group name envelope (`lakko-contacts::group_name`)

`domain "lakko/group-name/v1\0" ‖ version(1)=0x01 ‖ updated_at_ms(8 BE) ‖
name_len(1) ‖ name`:

- `name`: UTF-8, trimmed, 0 to 64 scalars and at most 255 bytes, the
  profile display-name character rules (no line break, no control, no format
  character outside the profile's allowed set). **Empty clears the name**;
  the receivers fall back to the member-derived title.
- Rejected as a whole on any bound violation; the composer refuses exactly
  what the decoder refuses; `IncomingEnvelope::GroupName { updated_at_ms,
  name }` so it never renders as a bubble; a fuzz target
  `group_name_decode`; the cross-kind separator test extended.
- Order: `group_name_is_newer(incoming_ms, stored_ms)` is strictly greater,
  **per group**; equal is not newer. A re-send (growth republish) carries the
  original stamp verbatim, so it can never clobber a later rename (the
  ADR-0044 lesson: persist the stamp and re-emit it unchanged).

### 2. Group name authority (receive-side gate, all three clients)

The receiver resolves the MLS sender leaf to the account key at receipt
(C-68) and reads the live role table:

| Role table | Sender | Verdict |
|---|---|---|
| present | owner or admin | accept if newer |
| present | member | drop (`reject-not-authorized`) |
| present | unresolvable sender / table unreadable | drop |
| absent (legacy group) | any member | accept if newer |

The gate runs before the order comparison and the whole read-decide-write
runs under the per-group lock the push routing secret already uses (C-62
sub-invariant 5: a gate must read current state). Locally, the client offers
the rename control only to the owner and admins, read from the live group,
never from a cached role.

### 3. Group name carriers and timing

- Sent through the existing service-envelope path (web `broadcastControl`,
  the native twins): sealed at the live epoch, no push wake.
- **When:** right after the creating commit; on every rename; and **on group
  growth**, re-sent by the owner and admins that hold a name, throttled and
  jittered like the profile republish (`publish-scheduler.ts`), so a newcomer
  learns the name within one poll of admission.
- Stored per `(account, group)` as `{ name, updated_at_ms }` in a device
  store that survives a relaunch; the creator's own name is written to the
  same store, which retires the Android-only `GroupPresentationStore` name
  and the web in-memory `ActiveGroup.label` as sources of truth.
- **Background filters:** the web service worker, the Android background
  decrypt and the iOS NSE (`lakko-ffi-extension`) gain the same
  recognise-and-suppress arm the committer beacon needed (ADR-0045), so a
  rename is never previewed as a notification body reading
  `lakko/group-name/v1`; the slim extension surface gets decode only.

### 4. Channel metadata record (`lakko-channels::meta`)

`domain "lakko/channel-meta/v1\0" ‖ version(1)=0x01 ‖ updated_at_ms(8 BE) ‖
name_len(1) ‖ name ‖ description_len(2 BE) ‖ description`, carried as the
**plaintext of an ordinary channel post**: it consumes a counter from the
publisher's monotone sequence, is sealed under the epoch seed and signed by
the publisher key like every post, so authenticity is the post signature
(only a publisher can post) and confidentiality is the epoch seed.

- `name`: 1 to 64 scalars, at most 255 bytes, the same character rules;
  `description`: 0 to 300 scalars, at most 1024 bytes, line breaks allowed,
  the same control-character rules.
- `classify_post_plaintext(bytes) -> PostContent::{Text, Meta { name,
  description, updated_at_ms }, Malformed}` in `lakko-channels`, exposed on
  both FFI surfaces, so the three readers do not each hand-roll the sniff;
  the domain prefix is checked first, a domain match that fails a bound is
  `Malformed` and dropped, never rendered as text; a fuzz target
  `channel_meta_decode`.
- Readers keep the newest record per channel by `updated_at_ms` (strictly
  greater) and title the channel from it; a record is never shown as a post.
- **Re-issue:** the publisher posts the record at creation, on every edit,
  and **after every rotation** (the rotation cron already runs on the
  publisher), so the newest epoch always holds a record a late subscriber can
  read; the thirty-day post retention is covered by the daily rotation
  default and by a re-issue when the newest record is older than the
  retention window.

### 5. Precedence (all clients)

- Group title: shared group name, then the member-derived title.
- Channel title: shared name, then the `@handle`; description shown only
  when present.

### 6. What this does not do

No "renamed the group" transcript row; no per-user local group rename; names
are not carried in the ADR-0050 pairing bundle (a paired device learns them
at the next re-send or record); no channel name clearing.

## Alternatives

- **Name fields on the channels or discovery service.** One fetch instead of
  a sealed record. Rejected: invariant #2 (the server would hold the name in
  plaintext) and the discovery handle table is deliberately an OPRF output
  the server cannot read.
- **Group name in a `GroupContext` extension** (next to the role table).
  Authenticated by the commit and known to every joiner from the Welcome.
  Rejected: every rename is a commit, which under ADR-0045 is a proposal plus
  an owner sweep and an epoch advance for every member, for a cosmetic
  change; it also touches `lakko-mls` (invariant #3).
- **Group name inside the Welcome only.** Reaches newcomers but not existing
  members on a rename. Rejected; the growth re-send covers newcomers.
- **Channel name inside the seed envelope** fanned out to each subscriber.
  Rejected: a rename would need a full fan-out, and the envelope is the
  wrong place for content.
- **Accept a group name from any member.** Rejected: a plain member could
  rename a group for everyone; the role gate is the same tier that manages
  membership and the push routing secret.
- **Order group names by DS journal position instead of a stamp.** Rejected:
  a growth re-send of an old name would land on a newer row and win.

## Consequences

- **Positive:** the same group and channel names on every device; the server
  learns nothing; no MLS state-machine change; the existing envelope,
  dispatcher, role-gate and republish patterns are reused; iOS gains the
  missing group-name field.
- **Negative:** two more journal rows per rename and per growth event (the
  same anti-pattern ADR-0024 accepted); a channel record post consumes a
  counter and a rotation re-issue adds one post per rotation; three client
  receive loops gain one more arm each.
- **Neutral:** channel handles remain the only unique identifier; names are
  not verifiable by construction and are shown as the setter's statement.

## Open questions

1. Should a rename produce a transcript row ("Anna renamed the group")? Not
   in this version.
2. Carry names in the ADR-0050 pairing bundle once channel subscriptions are
   carried too.

## References

- ADR-0022 (roles), ADR-0044 (stamp persistence lesson), ADR-0045
  (proposal-based commits), ADR-0048 (profile envelope, the template),
  ADR-0031 (epoch seed retention), ADR-0010 (channels).
- `crates/lakko-contacts/src/{profile.rs,incoming_envelope.rs}`,
  `crates/lakko-channels/src/post.rs`.
