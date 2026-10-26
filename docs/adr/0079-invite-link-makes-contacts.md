<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0079: An invite link makes two people contacts

- **Status:** Accepted (2026-10-16, maintainer). The maintainer decided the behaviour the
  same day (`docs/ux/contact-invite-link.md`): the invited person writes at
  once, a request through a dead link reaches the inviter as an ordinary
  request card, the page shows no key material. **Amended 2026-10-19
  (maintainer):** the moment B pressed Add decides whether a link counts; a
  request through a link that does not count is dropped silently and B is
  sent a signed refusal (this overturns the 2026-10-16 card decision); a
  signed-out visitor sees no inviter card; a dead link opens Contacts. See
  "Amendment (2026-10-19)" at the end.
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
answer is unchanged. *(Overturned 2026-10-19: such a request is dropped
silently and B is refused; "live" is judged at the moment B pressed Add.
See the amendment at the end.)*

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
- `lakko-contacts::link_founded` (2026-10-17): the note the winner posts into
  its own DeviceGroup after founding and BEFORE any Welcome leaves.
  `lakko/link-founded/v1\0 ‖ version(1)=0x01 ‖ link_id(16) ‖
  requester_account_key(32) ‖ group_id(32) ‖ welcome_len(2, BE) ‖ welcome`,
  the Welcome 1 to `LINK_FOUNDED_MAX_WELCOME_LEN` = 32 768 bytes (the note
  106 to 32 873 bytes), version checked before any length,
  `compose_link_founded` refusing exactly what `decode_link_founded`
  refuses, its own `IncomingEnvelope::LinkFounded` kind (never a chat
  bubble), unit tests (round trip, every refusal, sibling domains) and
  proptests (total and canonical decode; every encodable note round-trips),
  fuzz target `link_founded_decode`. The bound is about four times a
  measured maximal one-to-one Welcome: `a_maximal_one_to_one_welcome_fits_the_note`
  in `lakko-ffi-uniffi` founds a one-to-one with 8 bound devices on each side
  (15 added leaves) and measured 7 846 bytes.
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
  The note: `composeLinkFoundedEnvelope(linkId, requesterAccountKey, groupId,
  welcome)`, `decodeLinkFoundedEnvelope` (→ `FfiLinkFounded { linkId,
  requesterAccountKey, requestKey, groupId, welcome }`),
  `isLinkFoundedEnvelope`, `linkFoundedEnvelopeMinLen` / `MaxLen`, and the
  appended dispatch kind `LinkFounded` with its `linkFounded` field, on
  UniFFI and wasm; the NSE extension recognises a note only to suppress it.
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
     claim rows by those bytes: the DeviceGroup receive loop matches every
     row against the recorded bytes (web: also the rows its own-echo set
     skips) and judges the row at its position by the **readability rule**
     below, filing the id only when the row counts, before the batch
     persists. The publish answer files nothing (it does not know the
     row's position). A crash between the publish and the read loses
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
     sealed claims no row carried are dropped (they did not land), and so
     are this device's claims whose row did not count (straddled, below;
     the next pass claims again at the current epoch), and the
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
  3. only the winner builds, and in this order (2026-10-17, so that a
     winner removed after building can never be followed by a second
     one-to-one, and a requester is never left without the Welcome):
     a. **before any device builds** — the first winner and any device that
        took the request over because the winner left the roster — it looks
        for a **link-founded note** for the request (below). If one is held
        it never founds: it writes the note's Welcome to the requester's
        mailbox with the request's reply grant (the Welcome is
        byte-identical for every added leaf, and the requester ignores a
        duplicate), then the contact row, the ack and the use count, and the
        request is removed. This holds whether or not this device is a
        member of the founded group or ever joined it. A failed write leaves
        the request for the next pass;
     b. otherwise the one-to-one is founded in ONE commit adding the
        requester's devices AND every other current device of this account
        (ADR-0050 section 2; it was already one commit on all three), the
        journal claimed and the founder's state persisted, and the founding
        (group id, the Welcome, its targets in delivery order) is recorded
        durably in the won record;
     c. the founder publishes the note into the account's own DeviceGroup —
        `lakko-contacts::link_founded`: `lakko/link-founded/v1\0 ‖ 0x01 ‖
        link_id(16) ‖ requester_account_key(32) ‖ group_id(32) ‖
        welcome_len(u16 BE) ‖ welcome`, the Welcome 1 to 32 768 bytes (a
        maximal one-to-one, 8 devices on each side, measured at 7 846 bytes
        by `a_maximal_one_to_one_welcome_fits_the_note` in `lakko-ffi-uniffi`)
        — recording the sealed note before the publish and publishing it
        once, then reads the DeviceGroup journal past it and continues only
        when the note's row COUNTS (the readability rule): recorded as
        posted, BEFORE any Welcome is written to anyone. A failed publish, a
        note no row carried, or a straddled note writes no Welcome; the
        sealed note is dropped and the next pass publishes it again at the
        current epoch;
     d. only then the Welcome is written to this account's own mailbox FIRST
        and to the requester's only after that write succeeded
        (`welcomeOrder` on all three; the ordinary `startGroupWith` /
        `deliverPreparedWelcomes` follow the same order);
     e. the contact row, the ack and the use count.
     A retry (a failed publish, a restart) finds the founding in the won
     record and resumes it from the step it reached: the same group is made
     a held conversation (from the MLS snapshot after a restart) and its
     Welcome written again in the same order (receivers ignore a duplicate);
     nothing is founded twice. One function per client holds this order
     with the device's steps injected (web `runLinkBuild`, iOS
     `LinkClaims.runBuild`, Android `LinkClaims.runBuild`), so the order is
     unit-tested.
  Every device keeps the earliest note per request (journal order) from a
  sender leaf the DeviceGroup roster resolves at receipt (C-68), at most 64
  notes, in the claim record (cleared by Forget device). The account
  mailbox catch-up and the store of joined Welcomes that an earlier round of
  this rework put before the build are removed: with the note, the only
  case a held one-to-one without a note could arise is a note the other
  devices could not read, and there answering without forwarding could
  leave the requester with nothing, which is worse than a second
  one-to-one.
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

- **The readability rule (order of 2026-10-17).** A DeviceGroup message
  published while another DeviceGroup commit is in flight can land on the
  journal after that commit; every other device is then at the next epoch
  and cannot read it (OpenMLS keeps no past epochs; `max_past_epochs` stays
  unset and `lakko-mls` is untouched). Every row's epoch is in its public
  framing, so every device computes the same answer: a claim or note row
  **counts only if the epoch it was sealed at equals the journal's epoch at
  that row's position**, the epoch after the last commit before it that
  took effect. A commit takes effect when it was sealed at the journal's
  epoch at its row and the device's group moved past that epoch after the
  row (another device's applied commit, or this device's own commit merged
  early); a commit everyone refuses, or the losing one of two commits at the
  same epoch, does not move it. The rule is `lakko-contacts::journal_epoch`
  (`JournalEpochs`: `epoch_at`, `row_counts`, `observe`; at most 64 commits
  kept, older ones folded into the base) on both FFI surfaces
  (`journalEpochsStartingAt` / `journalRowCounts` / `journalEpochsObserve`
  on UniFFI, the `JournalEpochs` class on wasm, and `DeviceGroup.epoch()`
  read through `as_group()`), so no client re-derives it. Each client keeps
  the record next to the claim record, per DeviceGroup id (a re-founded
  group starts again), starting at the group's epoch when none is held, and
  observes every row it reads (web `journal-readability.ts`, called from the
  apply path and the own-echo path; iOS and Android in the DeviceGroup poll
  loop). Because a device that merged its own commit is ahead of the journal
  until its loop reaches that commit's row, the rule reads the journal, not
  the device's current epoch: judging by the current epoch would make an
  own claim sealed before that commit look straddled while every other
  device reads it, and nobody would build. Consequences: a straddled claim
  never wins, for its author too (it is dropped and claimed again); a
  straddled note never counts (the founder posts it again before any
  Welcome); another device's claim or note that decrypts counts by
  definition. Property tests: `a_straddled_claim_never_wins_and_one_device_builds`
  (exactly one builder across interleavings of claims and commits, every
  device holding exactly the counting claims) and `a_straddled_note_never_counts`.

**Residuals.**

- The readability rule assumes the DeviceGroup did not fork: two of this
  account's devices committing at the same epoch with both commits taking
  effect on different devices is the ADR-0045 hazard on the DeviceGroup and
  is not addressed here.
- A device that holds no journal record (its first run, or the first after
  a re-founding) starts at the group's epoch when its batch starts; a claim
  or note of its own published before an own commit it had already merged
  when that batch started is judged at that epoch.
- A request a device lost (or saw another device claim) leaves the store
  only when the register says it was answered or the link is dead. If the
  winner's use never reaches the register while the winner stays in the
  roster (or the link's answered list, at most 16 requester keys, dropped
  this requester after later uses), the request stays stored, unshown; the
  iOS and Android undecided stores hold at most 64 rows.
- No client offers B a control to delete a pending contact (maintainer,
  2026-10-19: none is to be built); the store-level removal (record and
  queue) runs when a verified refusal arrives (amendment below).
- `max_uses` is counted twice: the ledger's soft count at Add (which B can
  inflate with fresh accounts, as before) and the register's count at
  accept, which is the one that decides. Two devices that accept two
  DIFFERENT requests through one multi-use link at once can still exceed its
  limit by one (section 4, accepted).
- iOS and Android unit tests were compiled but not run (`xcodebuild test` is
  not run here; the Android host JVM suite does not load the native core).

## Amendment (2026-10-19): when a link counts, signed-out and dead links, a refusal back to B

The maintainer decided seven rules on 2026-10-19 (UX spec
`docs/ux/contact-invite-link.md` sections 3-5 and 7 amended the same day).
Built the same day in `lakko-contacts`, `lakko-sync`, both FFI surfaces and on
web, iOS and Android. Host-tested only; nothing was walked on a device, in a
browser against a server, or between two accounts.

**1. The moment B pressed Add decides.** A link counts for a request when, at
the moment the request was written, it was not expired, and, now, it is not
revoked and has uses left. A's app accepts whenever it next comes online,
before or after the expiry. "The moment" is the server stamp the DS gives the
mailbox row (`created_at`, `TIMESTAMP(3) DEFAULT CURRENT_TIMESTAMP(3)`, set by
MariaDB, served RFC 3339 on every fetch, `services/ds/src/journal.rs`), never
B's clock. It survives a restart of A's app because each client keeps it with
the undecided request: web in the stored mailbox row, iOS and Android in the
`IncomingContactRequest` inside the sealed `LinkRequestInbox` row. A stamp
that does not parse counts as written after any expiry (fail-closed: the link
is judged dead). The rule is one function in the core,
`LinkRecordEntry::counts_for_request(pressed_at_ms)` behind
`SyncedState::link_answer_verdict(link_id, secret, requester, pressed_at_ms)`
on both FFI surfaces; no client re-derives it.

*The use limit.* The link's ledger row counts uses at Add (the existing
consume; at most `max_uses` Adds are admitted). The decision at A still uses
the register's count of answered requests, now: an honest request the ledger
admitted is within the limit, so it is never refused by that count, and the
count stops a client that skipped the consume. No server change was made.
*(Superseded the same day: this paragraph first described an inline link,
which had no ledger row and was counted at A's register only, and proposed
dropping the inline fallback. The maintainer decided it; the inline form is
removed on all three clients (ADR-0028 head note of 2026-10-19), every
contact link has a ledger row, the register requires a 6-byte ledger id and
a 32-byte revocation seed (`lakko-sync`), and Add always consumes.)*

A new verdict `Unknown` (wire code `unknown`) says this device holds no record
of the link at all. A device in that state waits and never refuses: the
record may still be on its way from another of A's devices.

**2. A revocation wins until A's app has answered.** "Answered" means the
one-to-one was founded for the request: a won claim with a recorded founding,
or a held link-founded note from another device. Until then a revocation makes
the request refused, even for the device that won the claim. After it, the
founding is resumed and the Welcome is delivered (a revocation does not undo a
contact already made).

**3. Signed out: no inviter card.** The first screen is the ordinary sign-in
(web `/login` with the link as the return, its "Create account" link carrying
the same return; iOS and Android the anon root with the link parked). After a
session lands the link is opened again: live, the invite page; dead, rule 4.
The signed-out preview (`previewInviteLink` on the natives, the signed-out
page on all three) is removed.

**4. A dead link opens Contacts.** Revoked, expired, used up, unknown,
damaged: the client goes to the Contacts screen and shows nothing else. The
"This link no longer works" page and its two catalog strings are removed.

**5. Add on a dead link keeps nothing.** The ledger consume runs first; when
it answers exhausted or not-found, no pending contact is written, nothing is
held, and the page goes to Contacts. The pending contact is written only after
the consume answered live (and still before the request is sent, so an
answer that comes back at once finds it). The order is one injected-steps
function per client with its own tests: web `contacts/invite-add.ts`, iOS and
Android `InviteAddOrder`.

**6. A request that reaches A as dead is dropped silently, and B is
refused.** No card. The dead-link card, its note and its catalog string are
removed (this overturns section 3's 2026-10-16 decision). A's device sends B a
**link refusal**, `lakko-contacts::link_refusal`:

```
"lakko/link-refusal/v1\0" (22) ‖ version 0x01 ‖ link_id (16) ‖ requester_account_key (32) ‖ sig (64)
```

135 bytes, fixed; the signature is Ed25519 by A's account key over everything
before it. Decode checks the domain, then the version, then the length;
`verify_link_refusal(wire, inviter_key, expected_link_id, own_account_key)`
also checks the link id and the requester key, so a refusal of another link or
of another person removes nothing (errors `WrongDomain`, `BadLength`,
`UnknownVersion`, `OtherLink`, `OtherRequester`, `BadKey`, `BadSignature`).
Fuzz target `lakko-contacts/link_refusal_decode`; proptests that any one
changed byte is refused and that decode round-trips. It travels as a sealed
mailbox row of a new kind, `MailboxKind::LinkRefusal` (wire byte 5), to B's
account mailbox through the reply grant the request carried (the path a
contact ack takes). On UniFFI `composeLinkRefusal` / `verifyLinkRefusal` /
`linkRefusalLen` and the `LinkRefusal` mailbox kind; on wasm the same names.
No background reader opens mailbox rows (the iOS NSE, the web service worker
and the Android background decrypt act on group wakes only), so the refusal
is suppressed by construction and the NSE surface needs nothing.

B's client checks each refusal against its pending contacts: the stated
account key of the inviter, the link id the pending contact now stores (web
`linkIdHex`, store version 3; iOS `linkId`, key prefix v2; Android
`linkIdHex`), and B's own account key. On a match the pending contact and its
held messages are removed silently and its reply key is closed. There is no
delete button. A refusal that does not verify, or that arrives after the
one-to-one exists (there is no pending contact any more), changes nothing.

*Who refuses.* The step rule (`linkRequestStep` on web, `LinkClaims.step` on
iOS and Android) has a `refuse` step in place of the card: with no own claim
and a dead verdict the device refuses; with a won claim not yet answered and a
dead verdict it refuses (rule 2); a device that lost the claim, or that sees
another device's claim answered, drops the request without a refusal (the
claimer answers). Two of A's devices that both judge a request dead may each
send a refusal; B removes the pending contact on the first and finds nothing
on the second. A refusal write that fails keeps the request for the next pass.

**7. Kept.** The 7-day "has not answered yet" line, the already-contacts and
own-link states, and one device answering a request (the claims, sections
"As built" above).

**Residuals of the amendment.**

- A device whose register has not yet received a revocation may build while
  another device, which has, refuses the same request. B can then receive the
  refusal and the Welcome in either order: a refusal first removes the pending
  contact and the Welcome then lands as an ordinary one-to-one; a Welcome
  first makes the refusal find nothing. Not closed; it needs the claim to
  carry the verdict, which was not decided.
- The expiry is judged at the DS stamp, so a request that sat in A's mailbox
  for longer than the DS retention (30 days) is gone before A sees it, as any
  mailbox row.
- An unparseable stamp (none observed; the DS always serves one) refuses a
  request that might have counted.

## Amendment (2026-10-24): a link from a person already in the contacts still answers

Seen on test.lakko.me: A removed B from contacts (the one-to-one remains and
the removal stays) and sent B a new link; B's page said "A is already in your
contacts", Open chat sent nothing, and A never got B back. The maintainer
chose both remedies.

1. **Open chat sends the link answer.** On the "already in your contacts"
   page, Open chat runs the ledger consume and, unless the link no longer
   works, sends the ordinary contact request carrying the link answer
   (section 2), writes no pending contact (A already is one) and opens the
   chat. A network failure keeps B on the page with a retryable line.
2. **A's device restores instead of founding.** In the decision of section 3,
   a request from a person this device already holds a live one-to-one with,
   whose link counts or was already answered for that person, takes a new
   step, `restore`: the removal is forgotten, the person is re-added with that
   one-to-one, a live link records its use, and nothing is founded or sent.
   Every device of the account restores on its own, so no claim decides
   between them; a claim this device already won keeps building. A dead link
   is refused as before.

Separately, the person card of a removed person offers **Add to contacts**
(local, sends nothing); the shared rule `lakko_ui_rules::info` takes
`is_contact` again for that one case and gains `InfoSection::AddContact`
(UX `docs/ux/conversation-info.md` section 2.1).

## Amendment (2026-10-26): a signed-out visitor sees who invites

The maintainer reversed rule 3 of the 2026-10-19 amendment: a person with no
Lakko account who opened a link landed on a sign-in screen with nothing to
say that anyone invited them. A signed-out visitor now sees an inviter card
(name, `@username`, the label, one line about Lakko) with **Create an
account** and **I already have an account**, both returning to the link; a
dead link opened signed out says so with the same two buttons. Signed in,
rules 4 and 5 stand (a dead link opens Contacts). The card is opened on the
device from the ledger's sealed blob, which the ledger serves without a
session; nothing is consumed before Add. UX spec
`docs/ux/contact-invite-link.md` section 3, "Signed out".
