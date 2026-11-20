<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0098: A deleted account is shown as Deleted account, and its username is never shown on someone else

- **Status:** Proposed
- **Date:** 2026-11-20
- **Context:** account deletion and the server purge (`services/auth/src/purge.rs`,
  `services/discovery/src/internal.rs`, commit `c81c1c9d`), profile names and usernames
  (ADR-0048, ADR-0060 section 5, ADR-0076), the conversation kind (ADR-0062), authorship by
  account key resolved at receipt (C-68, ADR-0050), servers that cannot link accounts
  (ADR-0078), key reset and rotation (ADR-0073, ADR-0074)
- **UX:** [`docs/ux/deleted-account.md`](../ux/deleted-account.md)

## Context

Seen on the test stack on 2026-11-20: after W7 Droid deleted their account, the other side
still showed the contact as `W7 Droid @w7droid1422`; a message read `Sent` and arrived
nowhere; and `@w7droid1422` had already been registered by a different new account. Three
defects:

1. **Nobody tells the other side.** The deleting device wipes itself; the conversations keep
   their state. A peer can find out only by asking the directory, and no client asks.
2. **The deleted username is free at once.** `purge_account`
   (`services/discovery/src/account.rs`) deletes the account's `usernames` rows and their
   `name_claims` outright. A rename (ADR-0076) instead *retires* the old name for 30 days
   (`NamePolicy::retired_hold_secs`) precisely so that "a stranger could take the name the
   same hour and be mistaken for the person". Deletion skipped that hold.
3. **A shown username is never re-checked.** A client shows `@name` once the ADR-0060
   username certificate in the person's profile verifies. The certificate binds the name to
   the account id at issue time and verifies forever (an ADR-0076 residual), so it says
   nothing about whether the name is still theirs.

The maintainer's decision (2026-11-20): the people who talked to a deleted account see
`Deleted account`, with no username and no photo; a one-to-one stays readable with no
composer and no call buttons; their messages in groups are shown under `Deleted account`; a
stale username naming another person must never be shown.

What the directory answers today was checked in the code: after the purge,
`GET /discovery/identity/{account_id}` returns `404` (the `identity_bundles` row is deleted).
Nothing else deletes that row: an ADR-0073 key reset and an ADR-0074 rotation update it in
place, so a `404` for an account id this device once fetched means the account is gone. The
discovery unclaimed-account sweep (a guest that never took a name) also deletes it, which is
a deletion too. Account ids are UUIDv7 minted by auth and never reused. A username lookup
cannot serve as the re-check: `POST /discovery/lookup` answers only for a person in
Everyone (ADR-0060), the same `404` otherwise.

## Decision

A deleted account is learned two ways, push and pull, combined by one shared rule; the
deleted username stays held like a renamed one; and a username is shown only while the
account was recently confirmed to exist.

### 1. Push: the deleting device tells every conversation

After `DELETE /account` has succeeded at auth and before this device is wiped, the device
posts one **account-deleted envelope** as an ordinary MLS application message in every
conversation it holds (one-to-ones and groups; not channels, not the DeviceGroup). This is
one more best-effort cleanup in the existing deletion stage (`apps/web/src/auth/account-deletion.ts`
and its iOS / Android twins), within the existing budget; a failure is logged by label and
does not stop the wipe. It is sent after the auth deletion, never before, so it is never a
false statement about an account that still exists (auth may refuse a deletion: the ADR-0074
recall window, a missing fresh proof). The Delivery Service needs no session for it: a group
post is proved by the group's epoch key (ADR-0053).

Wire (`lakko-contacts::account_deleted`), fixed length, 90 bytes:

```text
"lakko/account-deleted/v1\0" (25) || version 0x01 || sig(64)
```

`sig` is Ed25519 by the **account key this device's conversation leaves are bound under**
(the ADR-0050 binding key; after an ADR-0074 rotation, the conversation key the own-binding
verdict chose) over

```text
"lakko/account-deleted/v1\0" || version || group_id_len(1) || group_id || account_key(32)
```

so a statement is bound to one conversation and one key and cannot be moved into another
group. It carries no time and no field: the statement is final, so nothing needs ordering.
Dispatched as `IncomingEnvelope::AccountDeleted` (its own kind, after the key-rotation step);
a non-JSON `lakko/`-prefixed plaintext is already `MessageBody::ServiceEnvelope` in
`lakko-attachments`, so it can never render as a chat bubble even on a client that does not
know the kind.

A receiver accepts it only when (a) the MLS sender, resolved at receipt to an account key
through the ADR-0050 binding (C-68), is the key the signature verifies under, and (b) the
signature verifies over this conversation's group id. Any other sender or a bad signature
drops it. An accepted statement marks that account key `deleted` on this device, finally:
no later answer reverses it. The background readers (web service worker, Android background
decrypt, iOS NSE) recognise it and stay silent (no notification).

### 2. Pull: the directory answers for a person this device asks about

A receiver that never got the envelope (the deleting device was offline, crashed, ran out of
budget, or this device was not reading) learns it from `GET /discovery/identity/{account_id}`
for that person's account id:

| Answer | Recorded as |
|---|---|
| `404` | `Absent` |
| `200`, and the bundle's key is the key this device holds for the person, or a conversation link in the bundle (ADR-0074 section 3.7) verifies naming that key | `SamePerson` |
| `200`, another key and no link that names the held key | `OtherKey` (a reset or rotation; the ADR-0016 / ADR-0073 gates decide what it means; never "deleted") |
| network error, `5xx`, anything else | nothing recorded |

The account id comes from the contact record (every client stores it, ADR-0021) or, for a
group member who is not a contact, from their profile (ADR-0048 v2 carries it). A profile's
account id is the member's own claim; a false claim can only make that member look deleted
to others (an `Absent` id) or never confirmed (an `OtherKey` id), never make another person
look deleted.

A pull-derived `Absent` is reversible: a later `SamePerson` makes the person active again.
Only the signed statement of section 1 is final.

**When a client asks**, without a storm at scale: only for a person on screen (an open
one-to-one, a person info card, the visible rows of the contacts list or of a group's member
list), only when `directoryCheckDue` says so (no answer for 7 days, and no attempt in the
last hour), at most one request in flight per person, and never for a person already deleted
by a statement. Every existing identity fetch (the ADR-0016 verify gate, contact add,
request accept) also records its answer. A person nobody looks at is never asked about. At
150 contacts that is at most about 150 by-id reads a week per device, and only for people
actually viewed.

### 3. The shared rules (written once, in `lakko-contacts::account_deleted`)

- `account_deletion_status(statement_verified, last_answer) -> Active | Deleted`: a verified
  statement is `Deleted`; otherwise the latest recorded answer decides (`Absent` is
  `Deleted`, `SamePerson` and `OtherKey` are `Active`, no answer is `Active`).
- `may_show_username(status, confirmed_at_ms, now_ms)`: `false` for a deleted person;
  otherwise `true` only when the latest `SamePerson` answer is younger than
  `USERNAME_CONFIRMATION_VALID_MS` (21 days, chosen) by this device's clock; a confirmation
  time in the future (the clock went back) counts as none. The client still requires the
  ADR-0060 certificate to verify; this rule is in addition.
- `directory_check_due(statement_verified, last_answer_at_ms, last_attempt_at_ms, now_ms)`:
  never after a statement; never within `DIRECTORY_RETRY_AFTER_MS` (1 hour, chosen) of the
  last attempt; otherwise due when there is no answer or the last answer is
  `DIRECTORY_RECHECK_AFTER_MS` (7 days, chosen) old, or in the future.

And in `lakko-ui-rules::info`: `deleted_person_info_sections(has_one_to_one, is_contact)` —
`Message` and `DeleteChat` when a one-to-one exists, `RemoveContact` when on the contact
list; no call, safety number, rename, add or share.

### 4. Why the username rule is sound, and the server step it needs

Let `L` be the time of the latest `SamePerson` answer: the account existed then, so it was
deleted (or renamed) at some `D >= L`. If the server holds a deleted or renamed name for `H`
before anyone else may take it, the name cannot belong to anyone else before `D + H >= L + H`.
So a username shown only while `now < L + V` with `V < H` never names someone else, whatever
this device missed. `V` = 21 days against `H` = 30 days leaves a week for clock skew between
this device and the server.

That needs the hold on deletion, which the purge skips today. **Server step (part of this
decision):** `purge_account` retires the account's current username instead of deleting it
(`retired_at = NOW(3)`, `is_public = FALSE`), keeps the name claims, and overwrites
`account_id` on every remaining row of that account with 16 random bytes, so the held rows
name no account and nobody can take the name back by rename; the hourly ADR-0076 pruner
frees them after `H`. Everything else in the purge is unchanged. The guarantee holds only if
the operator keeps `H` (the `retired_hold_secs` setting) above `V`.

The same rule covers a rename (ADR-0076): a renamed name is held `H` too, and the person
republishes their profile with the new name.

### 5. Group membership

An owner's or admin's device that accepted a statement proposes the removal of every leaf
of that account through the ADR-0045 path, so the roster stops carrying a dead member. Only
the signed statement triggers this, never a pull answer (a directory must not be able to make
admins remove a live member). A one-to-one cannot remove a person (ADR-0062) and is left as it
is. A group owned by the deleted account keeps a dead owner; removal of the owner is refused
(C-63) and nobody can change roles (the ADR-0022 lost-owner follow-up, unchanged).

## As built (2026-11-20, host-tested only)

- `crates/lakko-contacts/src/account_deleted.rs`: the envelope (`compose_account_deleted`,
  `decode_account_deleted`, `try_unwrap_account_deleted`, `verify_account_deleted`, error
  codes), the three rules and their constants; `IncomingEnvelope::AccountDeleted` after the
  key-rotation arm; fuzz `account_deleted_decode` (compile-checked).
- `crates/lakko-ui-rules/src/info.rs`: `deleted_person_info_sections`.
- UniFFI (`crates/lakko-ffi-uniffi/src/account_deleted.rs`, `ui_rules.rs`): the dispatch kind
  `FfiIncomingEnvelopeKind::AccountDeleted` appended last; no new `FfiError` variant (a
  compose refusal is `FfiError::Contacts` with `accountDeleted.<code>`).
- wasm (`crates/lakko-ffi-wasm/src/account_deleted.rs`, `ui_rules.rs`): dispatch kind 37.
- NSE surface (`crates/lakko-ffi-extension/src/account_deleted.rs`): recognise and length
  only.
- `services/discovery/src/account.rs` `purge_account`: the account's username rows are
  retired and re-keyed to a random id (`hold_usernames`) instead of deleted, the name claims
  stay, the identity bundle and the owned rows are deleted as before. This reverses the
  earlier test assertion that a deleted account's name "is free at once"; the registry test
  now asserts the name is refused to a stranger until the pruner frees it.
- Not built: everything on the clients (sending on deletion, the receive arm, the per-person
  record, the checks, the screens, the copy keys of the UX spec), the admin removal of
  section 5.

## Alternatives

1. **Push only.** Smallest change, but a deleting device that is offline, killed, or out of
   budget (a person in several hundred groups) leaves every peer with the stale name forever.
   Rejected as the only mechanism; kept as the fast path.
2. **Pull only.** Every peer eventually learns, but only for people it looks at, and the
   username must then rest on the time bound alone. Rejected as the only mechanism; kept as
   the fallback.
3. **A server notice to contacts.** The server would have to know who the contacts are
   (invariant #5, ADR-0078). Rejected.
4. **Re-check the username by name.** `POST /discovery/lookup` answers only for Everyone, so
   it cannot confirm a hidden person's name; a new by-name check would need an OPRF
   evaluation (a token) per shown name and would tell the server which names a device shows.
   Rejected; the account-existence check plus the server hold gives the same guarantee.
5. **Free the name at once and accept the window.** That is what produced the defect.
6. **Signature by the device leaf only.** The MLS sender authentication already proves the
   device; the account-key signature makes the final state need the account key itself and
   binds the statement to the conversation. Cheap; kept.

## Consequences

- **Positive:** a deleted person reads `Deleted account` on every client, the same way; a
  stale username naming another person is never shown while the operator keeps the hold
  above 21 days; the server learns nothing new (a statement is an ordinary sealed group
  message; the pull uses the existing by-id read).
- **Negative:**
  - A username is shown only after a recent confirmation, so a person not confirmed in 21
    days shows their name alone until the next check returns (on view, within a second on a
    working network).
  - The pull adds by-id directory reads: the directory sees, from the device's network
    address, which account ids it asks about and when (class (c) of ADR-0078, already true
    of every identity fetch; this makes it periodic for viewed people).
  - A thief holding the account key can post a statement and make that account read
    `Deleted account` in conversations, finally. They could already impersonate the person.
  - The directory is trusted for liveness: a malicious directory can make a live person
    look deleted until the next `SamePerson`, and can confirm a dead account as alive (it
    cannot defeat a statement). An availability attack, not a confidentiality one.
  - Messages sent before a device learned stay `Sent` and never arrive.
  - The held rows of a deleted account stay on the server for `H` (the name's hash and a
    random id; no account id).
- **Neutral:** the deleting device does one more MLS send per conversation inside the
  existing cleanup budget.

## Open questions

- Should a deleting owner be asked to hand over the groups they own first (the dead-owner
  case)? A UX question for the deletion screen, not answered here.
- Channels the deleted account owned: unchanged (ADR-0087 handles are retired by the client
  before deletion); subscribers learn nothing new from this ADR.

## References

- ADR-0016, ADR-0021, ADR-0045, ADR-0048, ADR-0050, ADR-0053, ADR-0060, ADR-0062, ADR-0073,
  ADR-0074, ADR-0076, ADR-0078
- Invariant index C-68, C-95 (new)
