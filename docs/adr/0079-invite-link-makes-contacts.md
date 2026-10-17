<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0079: An invite link makes two people contacts

- **Status:** Accepted (2026-10-16, maintainer). The maintainer decided the behaviour the
  same day (`docs/ux/contact-invite-link.md`): the invited person writes at
  once, a request through a dead link reaches the inviter as an ordinary
  request card, the page shows no key material.
- **Date:** 2026-10-16
- **Context:** contacts, invite links (ADR-0005 section 13, ADR-0028 sealed
  links, ADR-0060 section 6 link grants, ADR-0078 section 10 ownerless
  ledgers)
- **Amends:** [ADR-0005](0005-contact-invitations.md) (an `/i/` accept is no
  longer a request the owner must answer), [ADR-0078](0078-servers-cannot-link-accounts.md)
  section 10 as built (own link records are no longer kept per device only)

## Context

The walk on `test.lakko.me` on 2026-10-16 found the contact-invite flow wrong
on all three clients:

1. The invite page was headed by the inviter's key fingerprint and a safety
   number. A person can do nothing with either.
2. **Accept** composed an ordinary `ContactRequest` to the inviter. The invited
   person (B) did not get the inviter (A) as a contact, and A had to accept a
   card from someone A had invited.

What the code has today:

- The sealed `/i/` capability (`lakko_contacts::invite::InviteCapability`)
  carries A's Ed25519 and X25519 keys, A's account id, expiry, a soft use
  limit, the referral flag, an optional label, the per-link `link_id` and A's
  `link` reach grant. It carries no name, so the page had nothing to show but
  the key.
- A `ContactRequest`'s sealed plaintext is `profile_len ‖ profile ‖
  reply_grant_len ‖ reply_grant ‖ intro`. Nothing in it tells A which link, if
  any, B used. The Delivery Service checks that the mailbox write was signed by
  a key A registered for the link (ADR-0060), but A's client never sees that
  check.
- Since ADR-0078 phase 8 a device keeps records only of the links it minted
  (`minted-links.ts`, `OwnLinkStore.swift`, `OwnLinkStore.kt`): link secret,
  `link_id`, revocation seed, facts. They are not shared with A's other
  devices.

## Decision

### 1. The link names its owner

The sealed `/i/` capability gains an **inviter preview**: A's display name
(the ADR-0048 display-name rule, at most 255 bytes) and A's username (the
canonical `[a-z0-9_]{3,32}` rule, or empty). It is inside the capability, so it
is sealed under the link secret like every other field and signed with A's
key; the ledger and the server see ciphertext only (invariant #2). The
capability version is bumped; no dual format (nothing is deployed). The
`MAX_WIRE_LEN` / `MAX_SEALED_LEN` bounds of the `/i/` kind grow by the preview's
maximum, and the three clients' ceilings follow (web `MAX_SEALED_INVITE_BYTES`,
iOS `maxCapabilityBytes`, Android twin).

### 2. The request says which link it answers

The `ContactRequest` sealed plaintext gains, between the reply grant and the
intro, an optional **link answer**: `link_answer_len(u8) ‖ link_id(16) ‖
link_secret(12)` (length 0 or 28). B fills it from the link B opened. It is
sealed to A's key, so only A reads it; it proves B holds the link (the secret
is the URL fragment, never on any server). The outer request version moves; no
dual format.

### 3. A's device accepts a request through a live link by itself

On receiving a request with a link answer, A's client looks the `link_id` up in
its own link records and, when the record is live (not revoked, not expired,
uses left) and the secret matches in constant time, it runs the existing accept
path without showing a card: it creates the one-to-one group, sends the
Welcome, records B as a contact, and counts one use. A single-use link is then
used up.

A request whose link answer names no live record (revoked, expired, used up,
unknown, wrong secret) is shown as an ordinary request card with the note
"Sent with an invite link that no longer works." A request without a link
answer is unchanged.

### 4. Link records travel between A's devices

So that any of A's devices can recognise A's links, the link records (link id,
link secret, expiry, use limit, uses so far, revoked flag, label, kind) become
a register of the DeviceGroup-synced state (`lakko-sync`, ADR-0058), one entry
per link, last-writer-wins per entry on the HLC stamp, with a revocation that
is final (an entry revoked on one device can never come back live). The
revocation seed stays on the device that minted the link; a revoke from
another device marks the register and the minting device then revokes the
ledger row. This replaces the "links made on this device" rule of ADR-0078
phase 8. The register travels end-to-end encrypted to A's own devices only;
the server sees nothing new.

Two of A's devices may accept two different requests through a multi-use link
at the same time and exceed its limit by one. Accepted: the limit is a
courtesy, the inviter's consent was given by sharing the link.

### 5. B has A as a contact before A answers

B's client, after composing the request, records A as a **pending contact**
from the capability (A's keys, account id and the preview name), visible in the
contact and chat lists under A's name. B's messages to A are kept on B's
device, under the account store gate (ADR-0077), bound to A's account key, and
sent in order once B joins the one-to-one from A's Welcome. Calls are disabled
until then. The pending record keeps the time B pressed Add, for the
"has not answered yet" line after 7 days.

### 6. The invite page shows no key material

The page shows the preview name and username. The fingerprint and safety
number are removed from it on all three clients (the safety number stays on
the contact's info screen).

## Alternatives

- **Server-side auto-accept** (the ledger completes the handshake): the server
  would learn that A and B became contacts, materialising the graph
  (invariant #5). Rejected.
- **Accept by the link grant alone** (trust that the DS checked the write):
  A's client sees no proof of that check, and a client must not act on a
  server's word for an E2EE decision. Rejected for the secret in the sealed
  plaintext.
- **Keep link records per device** and let other devices show a card: a
  second device would ask A to accept someone A invited, which is the defect
  being fixed. Rejected.

## Consequences

- `lakko-contacts`: capability preview field and version, request link-answer
  field and version, bounded codecs, the existing fuzz targets extended
  (`invite_capability_decode_and_verify`, `invite_link_open`,
  `contact_request_decode_and_verify`).
- `lakko-sync`: a link-record register, its envelope, fold and persisted
  state version; a fuzz target or the existing `synced_state_decode` extended.
- Both FFI surfaces and the NSE extension (decode only where it already
  decodes requests).
- Three clients: the invite page, the auto-accept path, the pending contact
  and its message queue, the link register.
- No server or `lakko-mls` change (invariant #3 untouched). Invariants #2 and
  #5 hold: every new field is sealed end to end.

## Documentation this ADR implies

CLAUDE.md ADR row; PRODUCT-READINESS contacts row; `docs/ux/contact-invite-link.md`
is the UX source of truth.

## As built (2026-10-16)

Built in the core, both FFI surfaces, `services/discovery` (one migration)
and on web, iOS and Android. Host-tested only: nothing was walked on a
device, in a browser against a server, or between two accounts. Where the
build departs from or adds to the text above, it is recorded here.

**Core.**

- `lakko-contacts::invite`: `InvitePreview { display_name, username }`
  inside the signed capability, `SCHEMA_VERSION` 3 (version 2 refused, no
  dual accept). Layout `name_len(u8) ‖ name ‖ username_len(u8) ‖ username`;
  either may be empty. The name follows the ADR-0048 display-name rule
  (`MAX_PREVIEW_NAME_BYTES` 255), the username the canonical rule
  (`MAX_PREVIEW_USERNAME_BYTES` 32). `compose` refuses what `decode`
  refuses. `MAX_WIRE_LEN` is now 663 and `MAX_SEALED_LEN` 693 (the
  module's own doc comment had said 277 while the constant was 374 before
  this change).
- `lakko-contacts::request`: `SCHEMA_VERSION` 5, the sealed plaintext
  `profile_len ‖ profile ‖ grant_len ‖ grant ‖ link_answer_len(u8) ‖
  link_id(16) ‖ link_secret(12) ‖ intro`, the answer 0 or 28 bytes, any
  other length a broken framing. `LinkAnswer` (redacted `Debug`,
  constant-time `matches`), `OpenedRequest.link_answer`.
  `link_seal::link_secrets_match` is the one constant-time compare.
- `lakko-sync::synced::link_record`: the register entry carries more than
  section 4 lists: the ledger id (0 or 6 bytes, so another device can name
  the row and the list can link to it), `minted_at_ms` (the list's order),
  and `answered`, the requester keys already accepted (at most 16), so a
  re-sent request through a link already used for that person is answered,
  not counted again. Label at most 64 bytes. The verdict is
  `LinkRecordEntry::verdict` / `SyncedState::link_answer_verdict` →
  `Accept | Answered | Dead`. Envelope `lakko/sync/link-record/v1\0`,
  one record per envelope, carried as a DeviceGroup service envelope like
  the lineage and own-username registers; at most 128 records. The fold
  orders a revoked entry above a live one before the stamp (`beats`), so a
  revocation is final whatever arrives after it. Link records are not in
  the growth snapshot (`snapshot_parts`); a catch-up sends each record's
  envelope, like the other registers. Persisted state version 3 (version 2
  refused, no migration).
- FFI: UniFFI `composeInviteCapability(…, inviterDisplayName,
  inviterUsername)`, `InviteCapability.inviterDisplayName()` /
  `inviterUsername()`, `FfiLinkAnswer`, `composeContactRequest(…,
  linkAnswer, intro)`, `OpenedContactRequest.linkAnswer`, and on
  `SyncedState` `setLinkRecord` / `linkRecord` / `allLinkRecords` /
  `revokeLinkRecord` / `recordLinkUse` / `linkAnswerVerdict` /
  `linkRecordEnvelopeFor` / `linkRecordEnvelopes` /
  `applyLinkRecordEnvelope`, free `isLinkRecordEnvelope`. wasm the same,
  with the link answer as two byte arrays (empty for none) and the verdict
  as a string. The NSE extension crate does not decode contact requests and
  was not changed.

**Server.** Section "Consequences" says no server change; one was needed.
The discovery ledger column holding the sealed `/i/` blob was
`VARBINARY(404)`, so migration `0032_invite_preview_width.sql` widens it to
`VARBINARY(693)` (= `MAX_SEALED_LEN`), and the route test pins the width.
The server still stores only the sealed blob.

**Clients.**

- Invite page: the default state, already contacts (Open chat: the
  one-to-one when this device holds it, else the contact), own link, one
  state for revoked / expired / used up / unknown / damaged, network failure
  under the button. Web shows the signed-out state; iOS and Android do not:
  their deep link is parked until a session lands and then opens the default
  state (the signed-out page with Sign in / Create an account is unbuilt on
  the natives).
- Add spends the ledger use, writes the pending contact BEFORE sending the
  request (an answer that comes back at once always finds it) and removes
  it when the send fails. A ledger answer "already consumed by this account"
  still sends: the inviter's device answers it either way. The inviter is
  not founded a group by B: B's device founds nothing; A's device founds the
  one-to-one and its Welcome lands at B (the earlier iOS and Android
  `acceptInviteLink` founded the group from B's side; that path is gone).
- The pending contact is listed in Contacts and in All chats at once under
  the stated name. Its chat is text only (no attachments, voice or video
  notes), shows the waiting line or after 7 days the stale line, and
  disables the call buttons with the hint. Messages are kept on the device:
  web and iOS in the sealed held-send queue under the inviter's account id,
  Android with the pending record in a file sealed under the device key.
  When a one-to-one with that person appears, they move into it in order
  (web and iOS: into the conversation's held-send queue; Android: sent one
  by one, a failure keeping the rest for the next poll), and an open
  pending chat replaces itself with the real one.
- A's device: a request with a link answer is decided by the core's
  verdict once the synced state has loaded (each client waits up to 15 s,
  chosen, not measured; a state that never loads reads as dead, so the
  request becomes a card rather than being lost). Accept runs the ordinary
  accept path (one-to-one, contact row, then the ack) and counts one use. An
  accept that fails is shown as an ordinary card without the note.
- The dead-link note is a flag on the in-memory request; after a reload of
  the stored pending list it is gone (the stored row does not carry it).
- Own links: minting writes the record to the register with the link's
  secret, so every device lists the account's links with their use counts
  and can copy them. Revoke marks the register from any device; the device
  that minted the link (the only one holding its revocation seed, ADR-0078
  section 10) deletes the ledger row after its next poll sees the mark. The
  link's reach key is retired by the device that revokes, where it knows it.
- Preview at mint: the own display name and the own username as the device
  holds them (web from the profile store, iOS from `ProfileDirectory`,
  Android from `OwnProfileStore`); an account with neither mints a link
  whose page reads "this person".

**Residuals.**

- Two devices of A that both see a request before either one's use is
  folded both accept it; the second founds a second one-to-one with B (a
  duplicate conversation, not an extra member).
- A link revoked on a device that did not mint it stays fetchable from the
  ledger until the minting device runs; every device of A already refuses
  requests through it.
- `max_uses` is counted twice: the ledger's soft count at Add (which B can
  inflate with fresh accounts, as before) and the register's count at
  accept, which is the one that decides.
