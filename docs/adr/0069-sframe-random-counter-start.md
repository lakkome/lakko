<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0069: An SFrame sender starts its frame counter at a random 62-bit value

- **Status:** Accepted (2026-10-04; the maintainer chose this over a generation in the KID). Built the same day in the core, the C ABI, both FFI crates, the C++ bridge and the web worker, host-tested; not seen on a live call.
- **Date:** 2026-10-04
- **Context:** call media keys. Builds on
  [ADR-0035](0035-sframe-per-stream-kid-key.md) (one key per `(leaf, stream)`
  through the header KID) and leaves
  [ADR-0013](0013-sframe-cross-platform-aad.md) (the header is the AAD) and
  [ADR-0029](0029-mid-call-key-rotation.md) (the overlap key ring) as they
  are. Security invariants #2 and #3; C-5.

## Context

A sender's AEAD nonce is `nonce_salt XOR counter`, the key and salt are a
function of `(seed, KID)`, and a fresh `ParticipantKey` starts its counter at
zero. The crate documents the consequence as a caller's duty: a counter reset
must come with a fresh seed. A `lakko-security` review (2026-10-04) found two
paths where the callers do not keep it:

1. **Rejoining a chat-group call in the same MLS epoch.** The seed is
   `export_secret("lakko/call/v1", call_id)`, a function of the epoch and the
   call id; join and leave are application messages and advance no epoch.
   Hanging up and joining again, reloading the tab or reconnecting builds a new
   key at counter zero under the same seed and KID, for every stream, audio
   included. All three clients derive the seed this way.
2. **A camera track that ends by itself and is published again** creates a
   second video sender with the same KID and a counter at zero (web and
   Android; iOS never adds a second sender).

In both the SFU holds two ciphertexts under one `(key, nonce)`: ChaCha20
keystream reuse and a forgeable Poly1305 tag. Invariant #2 is broken for the
frames concerned.

The duty is not one a caller can keep: a client that reloads has no memory of
what it sealed.

## Decision

The sender's first counter value is random.

1. **The counter is 64 bits.** `ParticipantKey` counts in `u64`; the nonce is
   `nonce_salt XOR (counter as 8 big-endian bytes in the low 8 bytes of the
   12-byte nonce)`. For a counter below `2^32` this is the nonce the 32-bit
   code produced, so frames sealed before this change still open.
2. **A fresh key starts at a uniformly random value in `[0, 2^62)`**, drawn
   from the system CSPRNG when the key is derived, and counts up. Exhaustion
   (`u64::MAX`) fails closed as before; from the highest possible start there
   are still `3 * 2^62` values.
3. **Receivers accept any 64-bit counter** from the header (RFC 9605 section
   4.3 already encodes it). Opening keeps no state.
4. **The same-seed guard of the key ring stays** (a rekey to an unchanged seed
   is a no-op), now as defence in depth and not as the only thing between a
   caller and a reused nonce.

Two keys derived under one `(seed, KID)` reuse a nonce only if their counter
ranges overlap. With `L1` and `L2` frames sealed the probability is at most
`(L1 + L2) / 2^62`; at a hundred million frames each that is below `2^-34`.

## Alternatives

- **A generation in the free KID bits** (24 of them). Random: `2^-24` per pair
  of keys. Counted and stored on the device: exact until the store is cleared,
  and then certain reuse. Every reader of the KID has to mask the new bits.
- **Advance the MLS epoch on every join.** No wire change, but a call join
  becomes a membership-class commit under ADR-0045, by whoever may commit.
- **Keep the caller's duty.** It is the duty that failed.

## Consequences

- A flag day for call media: a receiver built before this change refuses a
  counter above 32 bits. There is no deployment to migrate.
- The SFrame header carries a counter of up to 8 bytes instead of 1-3: five to
  seven more bytes per frame.
- The cross-stack golden vector is a frame at a small counter and still opens
  on every stack; a stack's seal is proved by opening what it sealed, since a
  fresh key no longer seals a predictable frame.
- `docs/security/calls.md` C-5, G-31 and the "V1 false alarm" note in
  `known-gaps.md` are corrected with the build.
- An `lakko-mls`-adjacent keying change: property tests land with it, and it
  joins the surface of G-39.
