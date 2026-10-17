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

## As built (2026-10-16, reworked 2026-10-17)

Built in the core, both FFI surfaces, `services/discovery` (one migration)
and on web, iOS and Android on 2026-10-16; the maintainer rejected five
things in that build and they were reworked on 2026-10-17 (the timed
fallback to a card, two queue stores, no signed-out page on the natives, two
devices answering one request, a revoked link staying on the server). This
section describes the reworked state. Host-tested only: nothing was walked on
a device, in a browser against a server, or between two accounts. Where the
build departs from or adds to the text above, it is recorded here.

**Core.**

- `lakko-contacts::invite`: `InvitePreview { display_name, username }`
  inside the signed capability, `SCHEMA_VERSION` 3 (version 2 refused, no
  dual accept). Layout `name_len(u8) ‖ name ‖ username_len(u8) ‖ username`;
  either may be empty. The name follows the ADR-0048 display-name rule
  (`MAX_PREVIEW_NAME_BYTES` 255), the username the canonical rule
  (`MAX_PREVIEW_USERNAME_BYTES` 32). `compose` refuses what `decode`
  refuses. `MAX_WIRE_LEN` is now 663 and `MAX_SEALED_LEN` 693.
- `lakko-contacts::request`: `SCHEMA_VERSION` 5, the sealed plaintext
  `profile_len ‖ profile ‖ grant_len ‖ grant ‖ link_answer_len(u8) ‖
  link_id(16) ‖ link_secret(12) ‖ intro`, the answer 0 or 28 bytes, any
  other length a broken framing. `LinkAnswer` (redacted `Debug`,
  constant-time `matches`), `OpenedRequest.link_answer`.
  `link_seal::link_secrets_match` is the one constant-time compare.
- `lakko-contacts::link_claim` (2026-10-17): the claim of a request, so that
  only one of the inviter's devices answers it. Envelope
  `lakko/link-claim/v1\0 ‖ version(1)=0x01 ‖ link_id(16) ‖
  requester_account_key(32)`, fixed 69 bytes, version checked before length,
  `compose_link_claim` infallible and `decode_link_claim` accepting exactly
  what it produces, its own `IncomingEnvelope::LinkClaim` dispatch kind (never
  a chat bubble), and the pure decision over claims the client FEEDS it:
  `ObservedLinkClaim { message_id, author_key }` (the author is the
  DeviceGroup leaf key the receiver resolved from the MLS sender at receipt,
  or the device's own leaf key for its own claims, never anything read from
  the envelope), `link_claim_winner(claims, roster_keys)` = the claim with
  the lowest DS message id whose author is in the CURRENT DeviceGroup roster,
  `link_claim_outcome(own_key, claims, roster_keys)` → `Won { message_id }`
  when that claim's author is this device, `Lost { winner_message_id }` when
  it is another device, `NoWinner` when no roster device's claim is held, and
  `link_claim_answered_elsewhere(claims, roster_keys)` for a device holding
  no claim. A claim from a device since removed is passed over, so a removed
  device cannot leave a request unanswered. Proptests: canonical decode;
  exactly one roster device wins and it authored the earliest roster claim,
  across any reading order, duplicates and a removed device; an own earlier
  claim the device never recorded the id of (a crash duplicate) still wins
  with exactly one builder; a removed device's earliest claim hands the
  request to the next current claim. The fuzz target `link_claim_decode`
  asserts the same over arbitrary claim sets.
- `lakko-sync::synced::link_record`: the register entry carries more than
  section 4 lists: the ledger id (0 or 6 bytes, so another device can name
  the row and the list can link to it), the ledger row's **revocation seed**
  (0 or 32 bytes, 2026-10-17; SECRET, left out of `Debug`), `minted_at_ms`
  (the list's order), and `answered`, the requester keys already accepted (at
  most 16), so a re-sent request through a link already used for that person
  is answered, not counted again. Label at most 64 bytes. The verdict is
  `LinkRecordEntry::verdict` / `SyncedState::link_answer_verdict` →
  `Accept | Answered | Dead`. Envelope `lakko/sync/link-record/v1\0` at
  version byte `0x02` since the seed (`0x01` refused), one record per
  envelope, carried as a DeviceGroup service envelope like the lineage and
  own-username registers; at most 128 records. The fold orders a revoked
  entry above a live one before the stamp (`beats`), so a revocation is final
  whatever arrives after it. Link records are not in the growth snapshot
  (`snapshot_parts`); a catch-up sends each record's envelope, like the other
  registers. Persisted state version 4 since the seed (version 3 and older
  refused, no migration); the proptest `a_use_and_an_envelope_keep_the_
  revocation_seed` and the unit test `the_revocation_seed_reaches_every_
  device_and_survives_a_revocation` pin it.
- FFI: UniFFI `composeInviteCapability(…, inviterDisplayName,
  inviterUsername)`, `InviteCapability.inviterDisplayName()` /
  `inviterUsername()`, `FfiLinkAnswer`, `composeContactRequest(…,
  linkAnswer, intro)`, `OpenedContactRequest.linkAnswer`, and on
  `SyncedState` `setLinkRecord` / `linkRecord` / `allLinkRecords` /
  `revokeLinkRecord` / `recordLinkUse` / `linkAnswerVerdict` /
  `linkRecordEnvelopeFor` / `linkRecordEnvelopes` /
  `applyLinkRecordEnvelope`, free `isLinkRecordEnvelope`; `LinkRecord` gains
  `revocationSeed`. The claim: `composeLinkClaimEnvelope`,
  `decodeLinkClaimEnvelope` (→ `FfiLinkClaim { linkId, requesterAccountKey,
  requestKey }`), `isLinkClaimEnvelope`, `linkClaimRequestKey`,
  `linkClaimOutcome(ownKey, claims: [FfiObservedLinkClaim { messageId,
  authorKey }], rosterKeys)` → the enum `FfiLinkClaimOutcome { Won { messageId
  }, Lost { winnerMessageId }, NoWinner }`, `linkClaimAnsweredElsewhere(claims,
  rosterKeys)`, `linkClaimEnvelopeLen`, and the appended dispatch kind
  `LinkClaim` with its `linkClaim` field. wasm the same, with the link answer
  as two byte arrays (empty for none), the verdict as a string, the
  `LinkRecord` constructor taking the seed after the ledger id, and
  `linkClaimOutcome(ownKey, claimIds, claimAuthors, rosterKeys)` (authors and
  roster as concatenated 32-byte keys) returning a `LinkClaimDecision` with
  `won` / `lost` / `noWinner` / `messageId`. The NSE extension crate decodes a claim only to suppress it
  (`isLinkClaimEnvelope`, `linkClaimEnvelopeLen`, golden 69 bytes / first
  byte `0x6c`).

**Server.** Section "Consequences" says no server change; one was needed.
The discovery ledger column holding the sealed `/i/` blob was
`VARBINARY(404)`, so migration `0032_invite_preview_width.sql` widens it to
`VARBINARY(693)` (= `MAX_SEALED_LEN`), and the route test pins the width.
The server still stores only the sealed blob. The rework needed no server
change.

**Clients.**

- Invite page: the default state, already contacts (Open chat: the
  one-to-one when this device holds it, else the contact), own link, one
  state for revoked / expired / used up / unknown / damaged, network failure
  under the button, and the **signed-out** state on all three (2026-10-17 on
  iOS `SignedOutInviteScreen` and Android `SignedOutInviteScreen`, web
  `Invite.tsx` since 2026-10-16): the same page with **Sign in to add
  <name>** and **Create an account**, the catalog strings web uses
  (`invite.contact.page.sign_in`, `invite.contact.page.create_account`). The
  natives open the link with no session (a ledger fetch needs none; nothing
  is spent or sent); the slug stays parked, so after either choice the
  session that lands opens the invite page in its default state.
- Add spends the ledger use, writes the pending contact BEFORE sending the
  request (an answer that comes back at once always finds it) and removes it
  when the send fails. A ledger answer "already consumed by this account"
  still sends: the inviter's device answers it either way. B's device founds
  nothing; A's device founds the one-to-one and its Welcome lands at B.
- The pending contact is listed in Contacts and in All chats at once under
  the stated name. Its chat is text only, shows the waiting line or after 7
  days the stale line, and disables the call buttons with the hint. **One
  queue on all three clients (2026-10-17):** what B writes goes into the
  device's ONE held-send queue (ADR-0073 R.13b) under the pending contact's
  queue key, the inviter's account id (32 hex characters, never a
  conversation id), which the held-send flush never sends from (web
  `heldSends` mirrored into the message log, iOS `HeldSendQueue`, Android
  the sealed held-send store; Android's pending record no longer holds
  messages, and neither does web's, which also kept them in the record
  until then). When a one-to-one with that person appears, the messages
  move, in order, into its held-send slot with their give-up clock started
  then, and the ordinary flush sends them; a send that fails keeps that
  message and the rest for the next attempt (on Android this made the
  ADR-0073 held flush stop at the first failure instead of failing it and
  moving on, as web and iOS already did, and a held message is now tried
  again at the next poll of its conversation). The pending record goes once
  the messages are handed over. Removing a pending contact clears its queue
  (`removePendingContact` on all three; no client offers B a control for it
  yet, see the residuals). Forget device clears it with the held-send store.
- A's device, **no timer (2026-10-17):** a request carrying a link answer is
  kept undecided — not shown, not dropped — in the device's stored inbox
  (web: the `undecided` state of the inbox row; iOS `LinkRequestInbox` and
  Android `LinkRequestInbox`, sealed under the device key because they carry
  the link secret) and decided the moment the synced state is loaded, by
  the core's verdict, through the claim:
  1. `accept` → seal a claim, record the sealed bytes durably, then publish
     them into the account's DeviceGroup, building nothing. A device cannot
     decrypt its own MLS application messages, so it recognises its OWN
     claim rows by those bytes: the publish answer files the row's id at
     once (web before the id enters the own-echo set), and the DeviceGroup
     receive loop matches every row against the recorded bytes before it
     processes the row, filing the id before the batch persists. A crash
     between the publish and the recording of its id therefore loses
     nothing: the row is still recognised as this device's on the next
     read;
  2. wait until a DeviceGroup fetch that STARTED after the publish has read
     the journal to its head (web: the catch-up of the poll loop; iOS
     `waitForJournalReadPastNow`; Android `awaitJournalReadPastNow`). A wait
     that times out or finds the device offline leaves the request
     undecided — kept, unshown, its claim record intact — and the next pass
     waits again (web: after every poll tick; iOS: at every inbox refresh
     while undecided requests are held; Android: after every poll; and on
     each client when the synced state loads or a claim arrives). Then the
     sealed claims no row carried are dropped (they did not land), and the
     core decides over every claim held for the request — this device's own
     under its own leaf key, the others under the leaf key resolved at
     receipt — against the CURRENT DeviceGroup roster: the earliest roster
     claim wins and its author builds. Won → recorded durably; no winner
     (this device's claim did not land and no roster device claimed) → the
     own record is cleared and the next pass claims again; lost → the claim
     is marked settled and the request is KEPT, unshown, until the register
     says the request was answered (the winner counted the use) or the link
     is dead; meanwhile every pass re-checks it against the current roster
     without reading the journal again (every claim of this device is known,
     a later claim cannot be earlier, only the roster can change), so if the
     device that beat it is removed before it builds, the next earliest
     current claim wins — this device's when it is next;
  3. only the winner runs the ordinary accept path (one-to-one, contact row,
     ack) and counts the use; a build that fails is retried at the next pass
     from the won record, whatever the verdict reads by then.
  A claim from another device is honoured only from a sender leaf the
  DeviceGroup roster resolves at receipt (C-68) and recorded with that
  leaf's key (at most 16 per request, the lowest ids) before the batch
  persists. A device without a claim of its own that sees one from a device
  in the current roster keeps an undecided request unshown until the
  register says it was answered (or the link is dead), and removes a
  dead-link card already shown; if every such claimer leaves the roster
  first, it claims the request itself. Each
  client runs one decision pass at a time (web a promise chain, iOS a
  main-actor flag, Android a mutex), and the claim record's reads and writes
  are single synchronous operations, so a decision never interleaves with
  another or with the receive loop's write (C-62 sub-invariant 5). Answered
  → removed. Dead → the card with the note, and the note is now **stored with
  the request** (web: the row state `dead`; iOS and Android: `viaDeadLink`
  and the dead link's id kept in the stored pending list), so it survives a
  reload.
- Own links: minting writes the record to the register with the link's
  secret and (2026-10-17) the ledger row's revocation seed, so every device
  lists the account's links with their use counts, can copy them, and **any
  device that revokes deletes the ledger row itself, immediately**, with the
  record's seed (web `deleteLedgerRowOfRecord`; iOS and Android
  `revokeInviteLink`, the device's own minted-link record as the fallback
  for a record without a seed). On web a deletion the ledger could not be
  reached for is owed and retried at every change of the register; the
  minting device still drops its own local record when it sees the mark.
- Preview at mint: the own display name and the own username as the device
  holds them; an account with neither mints a link whose page reads "this
  person".

**Residuals.**

- A device that won and built, then failed to record the use in the
  register, and was then removed from the account before the record went
  out: the other devices see the winner gone with the request still
  unanswered in the register, and the next claim's author builds a second
  one-to-one with the requester. The request is answered twice, never not
  at all.
- A request a device lost (or saw another device claim) leaves the store
  only when the register says it was answered or the link is dead. If the
  winner's use never reaches the register while the winner stays in the
  roster (or the link's answered list, at most 16 requester keys, dropped
  this requester after later uses), the request stays stored, unshown; the
  iOS and Android undecided stores hold at most 64 rows.
- No client offers B a control to delete a pending contact; the store-level
  removal (record and queue) exists on all three for when the UX defines it.
- `max_uses` is counted twice: the ledger's soft count at Add (which B can
  inflate with fresh accounts, as before) and the register's count at
  accept, which is the one that decides. Two devices that accept two
  DIFFERENT requests through one multi-use link at once can still exceed its
  limit by one (section 4, accepted).
- iOS and Android unit tests were compiled but not run (`xcodebuild test` is
  not run here; the Android host JVM suite does not load the native core).
