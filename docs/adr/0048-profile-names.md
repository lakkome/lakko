<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0048: Profile names, carried end-to-end encrypted and bound to the sender's key

- **Status:** Accepted — the maintainer chose "own profile name + username" (2026-09-13);
  the carrier and verification choices below follow from the invariants.
- **Date:** 2026-09-13
- **Context:** Phase 3 product completeness. UX spec:
  [`docs/ux/profile-names.md`](../ux/profile-names.md).

## Context

Lakko has usernames for discovery (OPRF-blinded, ADR-0005) and nothing else. No client lets a
person set a name, no message carries one, and every contact is labelled with
`@` + a hex prefix of their public key — which reads like a username and is not one. A person
who adds `@alice` does not even keep `@alice` as the label.

Constraints:

1. **Invariant #1 / #2:** a name is personal data; the server must never see it.
2. **Invariant #5:** who knows whose name must not be materialised server-side.
3. **Impersonation:** a name and a username claimed by the sender are attacker-controlled. A
   displayed `@pavel` that is not Pavel is worse than no username.
4. **No production:** formats can simply change.

What already exists and is usable:

- `ContactRequest.sealed_envelope` is sealed to the recipient's X25519 key
  (`crates/lakko-contacts/src/request.rs`) and today carries an optional intro message.
- `ContactAck` is **plaintext** (185 bytes, no sealed part) — it cannot carry a name.
- Every chat, 1x1 included, is an MLS group; domain-separated service envelopes already ride
  MLS application messages and are classified by `incoming_envelope` (the push routing
  secret, the committer beacon).
- C-68: an author is the MLS signature key resolved at receipt.

## Decision

**A profile is a small, bounded, domain-separated envelope `{display_name, username,
updated_at_ms}`; it travels inside the contact request's sealed envelope and as an MLS
application message in every group the person is in; receivers bind it to the sender's
authenticated signature key and show a username only after verifying it through discovery.**

### 1. The envelope (`lakko-contacts::profile`)

`domain "lakko/profile/v1\0" ‖ version(1) ‖ updated_at_ms(8 BE) ‖ name_len(1) ‖ name ‖
username_len(1) ‖ username ‖ account_id_len(1) ‖ account_id`, bounded and fail-closed like its
siblings (version `0x02` since 2026-09-19; `0x01`, which had no account id, is refused —
nothing was deployed):

- `display_name`: UTF-8, at most 64 characters and 255 bytes (the one-byte length field),
  trimmed, no line breaks, no control characters, and no format characters **except** ZERO
  WIDTH JOINER / NON-JOINER (emoji sequences, Persian and Indic spelling) and tag characters
  inside a well-formed subdivision-flag sequence. Bidi controls and invisible spacers stay
  refused: a name is not unique, so an invisible joiner buys no more than typing the same
  name twice, but a bidi override can reorder the `· @username` rendered beside it.
  Empty = no name.
- `username`: empty or the normalised form (`lakko_discovery_client::normalize_username`),
  at most 64 bytes.
- `account_id` (added 2026-09-19): the sender's 16-byte auth account id, or absent (length
  byte 0; any other length is refused). It exists for one consumer: the group-call roster.
  The SFU names a participant by the ADR-0021 per-call peer tag, derived from the account
  id, and a client can compute that tag only for people whose account id it holds; until
  this field existed that was its accepted contacts, so every other member's tile showed a
  pseudonym while the members panel showed a name. Like every field it is the sender's
  claim: a receiver keys nothing security-relevant on it, stores it under the sender's
  account key resolved at receipt (C-68), and uses it only to label a tile. Members of an
  E2EE group already see each other's account public keys, which are equally linkable, so
  this widens nothing toward the server (which never sees the envelope) and little toward
  members.
- An envelope violating a bound is **rejected as a whole** (unlike attachment metadata, a
  profile has no "rest" worth keeping).
- `IncomingEnvelope::Profile` so it is never rendered as a chat bubble; a fuzz target.
- Order: a receiver keeps the profile with the greater `updated_at_ms` per signature key,
  equal is not newer. A backwards clock on the sender only delays its own update.

### 2. Carriers

- **Contact request:** the sealed plaintext becomes `profile envelope ‖ intro message`
  (a length-prefixed structure, no dual format — nothing is deployed). The recipient can show
  who is asking before accepting. The DS still sees only ciphertext.
- **In-group:** a member publishes its profile as an MLS application message **on joining a
  group, on changing its profile, and when the group gains a member** (a newcomer cannot read
  earlier epochs, so it would otherwise never learn existing names). The republish on growth
  is throttled and jittered per group so one add does not produce a synchronised burst.
- **In-group, on request (added 2026-09-19):** the three triggers above are push-only, and the
  one a newcomer depends on for everyone else's name fires on the *other* devices. A device
  that is away when the join lands never re-sends, a closed web tab cannot, and a re-send that
  did not arrive has no second chance; live, a web account that joined a five-person group last
  showed `Member` for three of them. So a newcomer also **asks**: a field-less
  `lakko/profile-request/v1` envelope (`lakko-contacts::profile_request`, 26 bytes, the
  committer-beacon shape and for the same reasons — the asker is MLS-authenticated by its
  sender leaf, a timestamp would be the sender's clock, and the answer is the existing profile
  envelope). Every member that reads it publishes its profile through the same throttled,
  jittered growth path, so N answers to one request do not land in one second, and a device
  with no profile at all stays silent as it does on growth. A device asks right after joining,
  and again on opening a group in which some member still has no profile on this device, at
  most once per group per 24 hours (`should_request_profiles`, one rule shared over the FFI so
  the clients cannot drift). A malformed request is dropped, never answered; the NSE and the
  background decrypt paths recognise it only to suppress it (C-59).
- **Not carried:** `ContactAck` (plaintext) and anything the server stores.

### 3. Binding and verification

- A received profile is stored under the sender's **signature key resolved at receipt**
  (C-68), never under a leaf index, never under the key the envelope might name.
- A profile from a contact request is bound to `ContactRequest.from_pubkey`, which the request
  signature authenticates.
- **Username verification:** the client resolves the claimed username through discovery
  (`lookup` → `account_id` → `identityFetch` → signature public key) and shows the username
  only if that key equals the sender's authenticated key. The result is cached per
  `(signature key, username)`; a changed claim re-verifies. Failure or mismatch → the
  username is never shown for that key.
- `display_name` is not verifiable by construction and is shown as the person's own
  statement, exactly as in any messenger; the verified username and the safety number are the
  disambiguators.

### 4. Username character set (maintainer's rule, 2026-09-13)

A username is **ASCII letters, digits and underscore only**, 3 to 32 characters after
normalisation (trim, strip a leading `@`, NFKC, lowercase), so Cyrillic and Latin look-alikes
and dashes cannot mislead. This reverses `normalize.rs`'s earlier stance of not rejecting
confusables.

The server only ever sees a blinded hash, so it **cannot** enforce this. It is enforced in
the shared Rust core instead, at the single chokepoint every client goes through: the OPRF
blinder refuses to blind a name that fails the rule, so registration and lookup both refuse it
by construction, and profile verification (section 3) never shows a username that fails it. A
modified client could still register a non-conforming hash; no conforming client can find it
or display it. With nothing deployed, no existing names need migrating.

### 4a. The own name between the account's own devices (amended 2026-09-30)

The own name was kept per device (`own-profile.ts`, `ProfileStore.swift`, `OwnProfile.kt`) and
nothing carried it to the account's other devices: a second device showed only `@username`,
and a name set there diverged from the first. The own profile envelope now also rides the
account's DeviceGroup, on the ADR-0060 section 7 `ServiceEnvelope` carrier beside the
Visibility setting: composed with the stored stamp (never re-stamped), no username certificate
and no grants, sent when the name is saved or cleared and handed to a device when it is added.
A receiving device keeps only the display name and only when its stamp is strictly greater
than the one stored; the username stays the session's. It does not republish to its groups:
the device that made the change already has.

### 5. Display precedence (all clients)

local rename → profile `display_name` → verified `@username` → fingerprint without `@`.
One resolver per client, used by every surface in the UX spec.

## Alternatives

- **Username only.** No profile screen and less code. Rejected by the maintainer; it also
  leaves guest accounts nameless.
- **Profile stored on the server (encrypted blob per account).** One fetch instead of in-group
  distribution. Rejected: whoever can fetch it learns the name, so it needs its own access
  control, and the fetch pattern materialises who looks up whom (invariant #5).
- **Put the name in `ContactAck`.** Rejected: the ack is plaintext on the DS mailbox.
- **Put the name in the MLS leaf credential.** Rejected: changing a name would need a commit
  per group per rename, and it would sit in every GroupContext and Welcome forever.
- **Show unverified usernames with a warning.** Rejected: the warning is ignored and the
  username is the impersonation.
- **Verify the username via a discovery-signed binding.** Discovery never learns plaintext
  usernames, so it cannot sign one; the lookup-and-compare path uses what discovery already
  serves.

## Consequences

- **Positive:** people have names; the server sees none of them; a username cannot be
  impersonated in the UI; no MLS, storage or DS change.
- **Negative:** each profile change and each group growth costs one application message per
  member per group, and a request costs one message from the asker plus one answer per member
  that holds a profile (the daily floor bounds a device that keeps opening a group whose
  members never answer); username verification costs one discovery lookup (with proof-of-work) per
  new `(key, username)` pair and tells discovery that someone resolved that username; display
  names can collide and can be misleading, as in every messenger.
- **Neutral:** invariant #3 untouched (no `lakko-mls` change); the contact-request sealed
  plaintext format changes, which is fine with nothing deployed.

## Open questions

- Whether the NSE / background-decrypt paths should learn profiles they receive (current
  answer: they only read what the app already stored; they never write — C-59).

## References

- [`docs/ux/profile-names.md`](../ux/profile-names.md)
- [ADR-0005](0005-contact-invitations.md), [ADR-0021](0021-sfu-peer-identity-exposure.md),
  [ADR-0023](0023-message-edit-delete-reactions.md) (C-68)
- `crates/lakko-contacts/src/{request,ack,incoming_envelope,committer_beacon}.rs`
