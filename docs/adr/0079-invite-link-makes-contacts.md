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
