// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Opaque per-group push routing id + the in-group
//! secret-distribution envelope (ADR-0042 — close the push
//! social-graph leak, invariant #5).
//!
//! ## The problem this solves
//!
//! The push subsystem routes group wakes by a 32-byte group
//! identifier that is **identical to the Delivery Service (DS)
//! journal key** (`services/ds`'s `/ds/group/{group_id_hex}`
//! path). That one value materialises "these devices are one
//! group" both inside `services/push-gw` (the subscription row
//! and the notify JOIN) and — worse — **in cleartext at Apple /
//! Google** (the delivered payload's `g` field + the APNs
//! `thread-id`). That directly violates invariant #5 (the
//! social graph is not materialised on the server).
//!
//! The fix routes push under a stable **opaque `push_gid`** that
//! is *not* the DS journal key. `push_gid` is a one-way,
//! domain-separated derivation of a per-group
//! **`push_routing_secret`** — 32 bytes of E2EE key material
//! minted by the group owner at creation, re-minted on a member
//! removal (ADR-0043), and distributed in-band over an MLS
//! application message (this module owns that envelope + the
//! derivation). push-gw keeps handling the 32-byte value as the
//! same opaque `BINARY(32)` lookup key it always did, so its
//! code + schema are untouched (invariant #2); only the value's
//! *meaning* shifts from "DS journal key" to "opaque routing id".
//!
//! ## Why reuse the relay-payload envelope pattern
//!
//! The secret rides an MLS application message on the group
//! journal (E2EE — the DS sees ciphertext, invariant #2). It is
//! classified by a leading-byte domain prefix in the receive-loop
//! envelope dispatch — the exact
//! [`crate::media_state`] pattern — so it is **never** rendered
//! as a chat bubble and a malformed envelope is fail-closed
//! dropped rather than mis-parsed. The structural difference from
//! `media_state` is the payload: an 8-byte `mint_epoch` + a
//! **32-byte secret** rather than a one-byte flag, so the codec
//! validates against a longer **exact** length and has no flag
//! byte to range-check (any 8-byte epoch and any 32-byte secret
//! are structurally valid — recency is decided by the total order
//! below, not by the codec).
//!
//! ## Wire shape (distribution envelope, v2)
//!
//! ```text
//!   plaintext_payload =
//!       PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1 || version || mint_epoch || secret
//!      └────────── 29 bytes, ASCII ───────────┘ └─ 0x02 ─┘ └─ 8 B, BE ─┘ └─ 32 B ─┘
//!
//!                   "lakko/push-routing-secret/v1\x00"
//! ```
//!
//! Total envelope length is a fixed 70 bytes. The 29-byte domain
//! separator goes inside the E2EE MLS application-message
//! plaintext, NOT into any visible header — the server learns
//! nothing about what the payload carries. The trailing `\x00` on
//! the separator makes the prefix impossible for a UTF-8
//! free-text payload to begin with, exactly as the sibling
//! envelopes do.
//!
//! **The `_V1` in [`PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1`] names
//! the envelope FAMILY, not the schema version.** Those 29 bytes
//! are frozen forever: [`crate::incoming_envelope`] dispatches on
//! this exact prefix, so changing them would make a peer fall
//! through to the chat parser and render the envelope as garbage
//! text. The in-payload version byte is the knob that moves —
//! today [`PUSH_ROUTING_SECRET_VERSION_V2`].
//!
//! ## Why v2 exists — the F-A replay (G-36 residual 9)
//!
//! v1 carried only the secret, so the ADR-0042 / ADR-0043 receive
//! gate could ask *who sent this* but never *is this newer*. A
//! retry of the current secret and a replay of a superseded one
//! were byte-indistinguishable, which made a **silent, permanent
//! rotation undo** reachable with no malice and no crash: a device
//! whose post-add redistribute broadcast failed holds the
//! **pre-rotation** secret in a `pending` row; another admin
//! rotates the group to `S1` (ADR-0043, on a member removal); the
//! first device's any-holder re-broadcast then re-sends the OLD
//! secret; every member's gate sees an authorised sender and
//! accepts it, retiring `gid(S1)` and migrating the group back
//! onto the routing id the **evicted member still derives**. With
//! no further shrink there is no further rotation, so the undo is
//! permanent.
//!
//! v2 adds the missing recency notion: an 8-byte big-endian
//! `mint_epoch` the minter stamps at mint time, and — in this
//! crate, once — the canonical comparison
//! [`push_routing_secret_is_newer`] every platform must route its
//! accept/refuse decision through.
//!
//! ## The canonical total order
//!
//! Order the pair `(mint_epoch, secret_bytes)`: epoch first, then
//! the 32 secret bytes compared lexicographically. An incoming
//! envelope supersedes a stored one only when it is **strictly
//! greater**.
//!
//! Why the *secret bytes* are the tiebreak — this is load-bearing
//! and non-obvious:
//!
//! * **Available to every receiver by construction.** It IS the
//!   value being ordered; no extra wire field, no lookup.
//! * **Stable across re-broadcast / relay.** Any sender-derived
//!   tiebreak (leaf index, role rank) is wrong here: on a
//!   re-broadcast the sender is the *re-broadcaster*, not the
//!   minter, so the value's identity would change on every relay
//!   and break the `accept-identical` fast path.
//! * **Ties are impossible.** The secret is 32 CSPRNG bytes.
//! * **Convergent regardless of arrival order.** Strict-greater
//!   means every member lands on the maximum whichever envelope it
//!   applies first — including a member offline across both.
//! * **Not worth grinding.** Grinding the secret buys at most one
//!   same-epoch tie; a later rotation is at a strictly greater
//!   epoch, which dominates. And only owner/admin pass the
//!   ADR-0042 / ADR-0043 role gate at all, so a plain member never
//!   gets to play.
//!
//! ### The honest consequence: the owner can lose a tie
//!
//! A total order and unconditional owner-priority cannot both
//! hold. At an **exact** epoch tie the bytes decide, and the owner
//! can lose to an admin. That is deliberate: the property
//! owner-priority existed to provide — arrival-order independence
//! — is preserved here by a strictly stronger mechanism (a total
//! order is arrival-order independent by construction, for every
//! participant, not just for the owner's envelope). In the normal
//! case the owner still wins, because it mints at a later epoch.
//!
//! This is the common path, not a corner case: under ADR-0043
//! option (b) an owner **plus N admins** may each rotate, so one
//! member removal produces **N+1 same-epoch mints**.
//!
//! The role gate is unchanged and still runs first — the order
//! decides between envelopes that already cleared it, never
//! whether one clears it.
//!
//! ## `push_gid` derivation
//!
//! ```text
//!   push_gid = keyed-BLAKE3(push_routing_secret,
//!                           "lakko/push/group-routing-id/v1")
//! ```
//!
//! Keyed BLAKE3 with the 32-byte secret as the key and the domain
//! string as the message. Deterministic, one-way, domain-
//! separated, and **stable** (no epoch input), so the value is a
//! fixed per-group routing id: a subscriber survives membership
//! commits without re-subscribing and the iOS `thread-id` stays
//! constant. `push_gid` is unrelated to any `group_id`, so a
//! push-gw-vs-DS cross-correlation can no longer link a
//! subscription to a group without the group secret.
//!
//! ## Cross-kind isolation
//!
//! The separator MUST be byte-distinct from every other
//! envelope-domain prefix in this crate — the
//! [`tests::separator_distinct_from_sibling_domains`] test
//! asserts this. Dispatch is a byte-prefix match, so a distinct
//! prefix guarantees a push-routing-secret payload never
//! cross-dispatches as a call-welcome / KP-forward / media-state
//! and vice versa.

use core::cmp::Ordering;
use core::result::Result as CoreResult;

use lakko_crypto::hash::blake3_keyed;
use lakko_crypto::signing::SigningKey;
use lakko_crypto::SecretBytes;
use thiserror::Error;

/// Length of a `push_routing_secret` in bytes.
///
/// 32 bytes of E2EE key material minted per group at creation and
/// re-minted on a member removal (ADR-0043). Also the length of
/// every derived [`derive_push_gid`] output, so the value drops
/// straight into push-gw's existing `BINARY(32)` / hex key-space.
pub const PUSH_ROUTING_SECRET_LEN: usize = 32;

/// Length of a derived `push_gid` in bytes (a BLAKE3 digest).
pub const PUSH_GID_LEN: usize = 32;

/// Domain string keyed into the [`derive_push_gid`] BLAKE3
/// keyed-hash as the *message* (the secret is the key).
///
/// Unlike [`PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1`] this is a
/// hashed message, not a dispatch prefix, so it carries **no**
/// trailing `\x00`. Changing this string changes every group's
/// `push_gid` — the golden test [`tests::derive_push_gid_golden`]
/// pins it so an accidental edit fails CI.
pub const PUSH_GID_DERIVE_DOMAIN_V1: &[u8] = b"lakko/push/group-routing-id/v1";

/// 29-byte domain separator the sender prepends to the
/// distribution-envelope payload before it rides the group
/// journal as an MLS application message.
///
/// The trailing `\x00` byte makes the prefix impossible for a
/// UTF-8 free-text payload to start with, mirroring
/// [`crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1`]. The
/// printable prefix is `lakko/push-routing-secret/v1`.
pub const PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/push-routing-secret/v1\x00";

/// Schema-version byte carried as the first byte of the payload
/// (after the domain separator).
///
/// An in-payload forward-compatibility knob distinct from the
/// `/v1` in the domain string: a revision that keeps the same
/// domain but changes the layout bumps this byte so a peer running
/// the other schema fail-closes rather than mis-reading the shape.
///
/// **`0x02` is the only accepted value — this is a hard flag day.**
/// The retired `0x01` (domain ‖ version ‖ secret, no mint epoch)
/// is refused with [`PushRoutingSecretError::UnknownVersion`].
/// Dual-accept was rejected outright: a v1 envelope carries no
/// `mint_epoch`, so it cannot be placed in the total order, and
/// admitting one would re-open the very F-A replay v2 exists to
/// close. Same shape as the [`crate::group_join_attribution`]
/// v1 -> v2 bump, which likewise refused the unsigned predecessor
/// rather than accepting it alongside.
pub const PUSH_ROUTING_SECRET_VERSION_V2: u8 = 0x02;

/// Byte length of the big-endian `mint_epoch` field (a `u64`).
pub const PUSH_ROUTING_SECRET_MINT_EPOCH_LEN: usize = 8;

/// Exact byte length of the payload that follows the domain
/// separator: one version byte + the 8-byte big-endian
/// `mint_epoch` + the 32-byte secret.
///
/// The decoder validates against this **exact** length —
/// anything longer or shorter is fail-closed rejected.
pub const PUSH_ROUTING_SECRET_PAYLOAD_LEN: usize =
    1 + PUSH_ROUTING_SECRET_MINT_EPOCH_LEN + PUSH_ROUTING_SECRET_LEN;

/// A decoded push-routing-secret distribution envelope (v2).
///
/// Field order mirrors the wire and, deliberately, the
/// [`push_routing_secret_is_newer`] total order: `mint_epoch`
/// first, then the secret bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushRoutingSecretEnvelope {
    /// The minter's recency stamp, big-endian on the wire.
    ///
    /// Opaque to this crate: it imposes an order and nothing else.
    /// This module never reads it as a wall-clock time, never
    /// bounds it, and never feeds it to [`derive_push_gid`]. A
    /// caller is free to source it from a millisecond clock, an
    /// MLS epoch, or a counter, so long as one group's mints are
    /// non-decreasing over time — that is the property the order
    /// rests on, and it is the caller's to hold.
    pub mint_epoch: u64,
    /// The 32-byte per-group routing secret. Feed to
    /// [`derive_push_gid`] to get the opaque routing id.
    pub secret: [u8; PUSH_ROUTING_SECRET_LEN],
}

impl PushRoutingSecretEnvelope {
    /// True when `self` (just arrived) must supersede `stored`
    /// under the canonical total order.
    ///
    /// Thin convenience wrapper over
    /// [`push_routing_secret_is_newer`]; identical semantics.
    #[must_use]
    pub fn supersedes(&self, stored: &Self) -> bool {
        push_routing_secret_is_newer(
            self.mint_epoch,
            &self.secret,
            stored.mint_epoch,
            &stored.secret,
        )
    }
}

/// The canonical total order on push-routing secrets: true when
/// `incoming` must supersede `stored`.
///
/// Ordered on the pair `(mint_epoch, secret_bytes)` — epoch first,
/// then the secret bytes lexicographically — and **strictly
/// greater** wins. See the module docs for why the secret bytes
/// are the tiebreak and why the owner can lose an exact tie.
///
/// This function is the whole point of the v2 bump. Three clients
/// each re-deriving a comparison is precisely the drift class that
/// produced the F-A replay, so the order lives here, once, and
/// every platform routes its accept/refuse decision through it via
/// the FFI rather than reimplementing it.
///
/// **Equal is NOT newer.** That is what keeps a benign retry of
/// the current secret from looking like an update, and it is the
/// property that makes the accept-identical fast path safe.
///
/// Comparing the `u64` epochs numerically is identical to
/// comparing their big-endian wire encodings lexicographically, so
/// a platform that finds it more natural to compare the raw
/// `mint_epoch_be(8) ‖ secret(32)` byte strings gets the same
/// answer. [`tests::is_newer_matches_lexicographic_wire_bytes`]
/// pins that equivalence.
///
/// # Wrong-length secrets
///
/// Fail-closed: if **either** secret is not
/// [`PUSH_ROUTING_SECRET_LEN`] bytes the answer is `false` (not
/// newer). A malformed incoming secret must never displace a
/// stored one, and a malformed *stored* secret — unreachable
/// through this module's decoder, which always yields exactly 32
/// bytes, so only a caller bug or a corrupt store produces one —
/// is better left in place than overwritten on the strength of a
/// comparison against a value that was never a secret. Never
/// panics; there is no length precondition to violate.
///
/// # Timing
///
/// The comparison is not constant-time, which is fine here: both
/// operands are values the device already holds, the caller is
/// already gated to owner/admin senders, and the outcome is
/// observable anyway — it is exactly which `push_gid` the device
/// goes on to subscribe under.
#[must_use]
pub fn push_routing_secret_is_newer(
    incoming_epoch: u64,
    incoming_secret: &[u8],
    stored_epoch: u64,
    stored_secret: &[u8],
) -> bool {
    if incoming_secret.len() != PUSH_ROUTING_SECRET_LEN
        || stored_secret.len() != PUSH_ROUTING_SECRET_LEN
    {
        return false;
    }
    match incoming_epoch.cmp(&stored_epoch) {
        Ordering::Greater => true,
        Ordering::Less => false,
        // Exact epoch tie: the 32 CSPRNG secret bytes decide,
        // lexicographically. Ties are impossible in practice.
        Ordering::Equal => incoming_secret > stored_secret,
    }
}

/// Derive the stable opaque `push_gid` from a group's
/// `push_routing_secret`.
///
/// `push_gid = keyed-BLAKE3(push_routing_secret,
/// "lakko/push/group-routing-id/v1")` — the 32-byte secret is the
/// BLAKE3 key, [`PUSH_GID_DERIVE_DOMAIN_V1`] is the message.
/// Deterministic + one-way + domain-separated + stable (no epoch
/// input). The output is 32 bytes, so it drops straight into
/// push-gw's `BINARY(32)` / hex key-space, and it is unrelated to
/// the group's `group_id` (invariant #5 — no cross-correlation
/// without the secret).
#[must_use]
pub fn derive_push_gid(push_routing_secret: &[u8; PUSH_ROUTING_SECRET_LEN]) -> [u8; PUSH_GID_LEN] {
    blake3_keyed(push_routing_secret, PUSH_GID_DERIVE_DOMAIN_V1)
}

/// Message the wake signing seed is keyed-hashed from (ADR-0078 section 3).
/// Frozen: changing it moves every group's wake key.
pub const PUSH_WAKE_SIGN_DERIVE_DOMAIN_V1: &[u8] = b"lakko/push/wake-sign/v1";

/// Message the wake unlock key is keyed-hashed from (ADR-0078 section 3).
/// Frozen.
pub const PUSH_WAKE_UNLOCK_DERIVE_DOMAIN_V1: &[u8] = b"lakko/push/wake-unlock/v1";

/// Length of a wake signing seed, a wake public key and a wake unlock key.
pub const PUSH_WAKE_KEY_LEN: usize = 32;

/// The group's **wake signing seed** (ADR-0078 section 3):
/// `keyed-BLAKE3(push_routing_secret, "lakko/push/wake-sign/v1")`, the
/// Ed25519 seed of the key every member signs a sealed wake with. push-gw
/// holds only its public half ([`derive_push_wake_public_key`]), registered
/// by each subscriber, so a wake is accepted only from somebody who holds
/// the group's routing secret - a member. Secret: whoever holds it can wake
/// the group.
#[must_use]
pub fn derive_push_wake_signing_seed(
    push_routing_secret: &[u8; PUSH_ROUTING_SECRET_LEN],
) -> SecretBytes<PUSH_WAKE_KEY_LEN> {
    SecretBytes::new(blake3_keyed(
        push_routing_secret,
        PUSH_WAKE_SIGN_DERIVE_DOMAIN_V1,
    ))
}

/// The public half of the group's wake signing key: what a subscriber
/// registers at push-gw, and what a sealed wake must verify under.
#[must_use]
pub fn derive_push_wake_public_key(
    push_routing_secret: &[u8; PUSH_ROUTING_SECRET_LEN],
) -> [u8; PUSH_WAKE_KEY_LEN] {
    SigningKey::from_seed(derive_push_wake_signing_seed(push_routing_secret).as_bytes())
        .verifying_key()
        .to_bytes()
}

/// The group's **wake unlock key** (ADR-0078 section 3):
/// `keyed-BLAKE3(push_routing_secret, "lakko/push/wake-unlock/v1")`. Every
/// subscription row push-gw stores is sealed under it
/// ([`crate::push_wake::seal_push_subscription`]); it reaches push-gw only
/// inside a sealed wake and is never stored there, so the push-gw database
/// at rest holds no readable token. Secret.
#[must_use]
pub fn derive_push_wake_unlock_key(
    push_routing_secret: &[u8; PUSH_ROUTING_SECRET_LEN],
) -> SecretBytes<PUSH_WAKE_KEY_LEN> {
    SecretBytes::new(blake3_keyed(
        push_routing_secret,
        PUSH_WAKE_UNLOCK_DERIVE_DOMAIN_V1,
    ))
}

/// Compose a push-routing-secret distribution envelope (v2) for
/// the given mint epoch + 32-byte secret.
///
/// The output is the **inner** plaintext the sender hands to the
/// MLS group journal as an application message (E2EE — the DS
/// sees ciphertext). Returns a fixed 70-byte `Vec<u8>`:
/// `domain-separator || version-byte || mint_epoch(8, BE) ||
/// secret(32)`.
///
/// `mint_epoch` is stamped **at mint time** and then travels with
/// the secret forever — a re-broadcast MUST replay the epoch the
/// secret was minted under, never a fresh one. Re-stamping on
/// re-broadcast would make a stale secret out-rank the live one
/// and reintroduce the F-A replay this envelope closes.
#[must_use]
pub fn compose_push_routing_secret(
    mint_epoch: u64,
    secret: &[u8; PUSH_ROUTING_SECRET_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len() + PUSH_ROUTING_SECRET_PAYLOAD_LEN,
    );
    out.extend_from_slice(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
    out.push(PUSH_ROUTING_SECRET_VERSION_V2);
    out.extend_from_slice(&mint_epoch.to_be_bytes());
    out.extend_from_slice(secret);
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`]
/// dispatcher.
///
/// Returns:
///   * `Ok(Some(envelope))` — `plaintext` started with the
///     push-routing-secret domain separator AND the trailing
///     payload decoded cleanly (known version + exact length).
///   * `Ok(None)` — the prefix doesn't match; the dispatcher
///     falls through to the next classification step.
///   * `Err(PushRoutingSecretError)` — the prefix matched but the
///     payload is malformed (unknown version / wrong length). A
///     syntactically-marked but semantically-invalid envelope —
///     the caller logs + drops rather than silently mis-handling
///     it (fail-closed, mirroring the sibling `try_unwrap_*`
///     helpers).
///
/// ## Gate order (deliberate)
///
/// Empty-payload check, then **version**, then exact length. The
/// version byte is the schema discriminator: once it disagrees,
/// the remaining bytes cannot be interpreted at all, so a length
/// complaint computed under *this* schema's expectations would be
/// noise. Version-first is also what gives the v1 -> v2 flag day
/// its clean signal — a retired 62-byte v1 envelope reports
/// [`PushRoutingSecretError::UnknownVersion`], naming the actual
/// problem, rather than a `BadLength { expected: 41, got: 33 }`
/// that invites a reader to think the envelope was truncated.
///
/// The mirror case is not symmetric and does not need to be: an
/// already-shipped v1 decoder meeting a v2 envelope checks length
/// first and so reports `BadLength`. Both are a fail-closed drop,
/// which is the only property that matters there.
///
/// # Errors
///
/// * [`PushRoutingSecretError::UnknownVersion`] — the version
///   byte is not [`PUSH_ROUTING_SECRET_VERSION_V2`]. Notably
///   covers every retired v1 envelope.
/// * [`PushRoutingSecretError::BadLength`] — the payload is empty
///   (no version byte to read), or is not exactly
///   [`PUSH_ROUTING_SECRET_PAYLOAD_LEN`] bytes.
pub fn try_unwrap_push_routing_secret(
    plaintext: &[u8],
) -> CoreResult<Option<PushRoutingSecretEnvelope>, PushRoutingSecretError> {
    if !plaintext.starts_with(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1) {
        return Ok(None);
    }
    let payload = &plaintext[PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len()..];
    // Empty payload carries no version byte to discriminate on, so
    // it can only be reported as a length failure.
    let Some((&version, rest)) = payload.split_first() else {
        return Err(PushRoutingSecretError::BadLength {
            expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
            got: 0,
        });
    };
    if version != PUSH_ROUTING_SECRET_VERSION_V2 {
        return Err(PushRoutingSecretError::UnknownVersion(version));
    }
    if payload.len() != PUSH_ROUTING_SECRET_PAYLOAD_LEN {
        return Err(PushRoutingSecretError::BadLength {
            expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
            got: payload.len(),
        });
    }
    // The exact-length gate above guarantees `rest` is exactly
    // MINT_EPOCH_LEN + SECRET_LEN bytes, so neither split nor
    // either copy below can panic.
    let (epoch_bytes, secret_bytes) = rest.split_at(PUSH_ROUTING_SECRET_MINT_EPOCH_LEN);
    let mut epoch = [0u8; PUSH_ROUTING_SECRET_MINT_EPOCH_LEN];
    epoch.copy_from_slice(epoch_bytes);
    let mut secret = [0u8; PUSH_ROUTING_SECRET_LEN];
    secret.copy_from_slice(secret_bytes);
    Ok(Some(PushRoutingSecretEnvelope {
        mint_epoch: u64::from_be_bytes(epoch),
        secret,
    }))
}

/// Strict standalone decoder: parse a push-routing-secret
/// envelope, treating a non-matching plaintext as a hard error.
///
/// This is the fail-closed entry point a caller uses when it
/// already knows the payload is *supposed* to be a
/// push-routing-secret envelope (e.g. a group-journal application
/// message it pre-classified). A plaintext that does not carry
/// the domain separator is rejected with
/// [`PushRoutingSecretError::WrongDomain`] rather than silently
/// falling through — the dispatcher-facing
/// [`try_unwrap_push_routing_secret`] is the helper that returns
/// `Ok(None)` for the fall-through case.
///
/// # Errors
///
/// * [`PushRoutingSecretError::WrongDomain`] — `bytes` does not
///   start with [`PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1`].
/// * [`PushRoutingSecretError::BadLength`] /
///   [`PushRoutingSecretError::UnknownVersion`] — see
///   [`try_unwrap_push_routing_secret`].
pub fn decode_push_routing_secret(
    bytes: &[u8],
) -> CoreResult<PushRoutingSecretEnvelope, PushRoutingSecretError> {
    match try_unwrap_push_routing_secret(bytes)? {
        Some(envelope) => Ok(envelope),
        None => Err(PushRoutingSecretError::WrongDomain),
    }
}

/// Errors specific to the push-routing-secret envelope decode
/// path.
///
/// Dedicated (rather than reusing a sibling's error) because the
/// fixed-shape 32-byte-secret payload has a distinct failure set
/// (no flag byte to range-check, unlike
/// [`crate::media_state::MediaStateError`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PushRoutingSecretError {
    /// The plaintext does not start with the push-routing-secret
    /// domain separator. Only surfaced by
    /// [`decode_push_routing_secret`]; the dispatcher-facing
    /// [`try_unwrap_push_routing_secret`] returns `Ok(None)` for
    /// this case instead so the dispatch chain can continue.
    #[error("plaintext does not carry the push-routing-secret domain separator")]
    WrongDomain,

    /// The domain separator matched but the trailing payload is
    /// not exactly [`PUSH_ROUTING_SECRET_PAYLOAD_LEN`] bytes.
    #[error("push-routing-secret payload is {got} bytes, expected exactly {expected}")]
    BadLength {
        /// Mirror of [`PUSH_ROUTING_SECRET_PAYLOAD_LEN`] at the
        /// time the error fired — kept on the variant so the log
        /// line is self-contained.
        expected: usize,
        /// Observed payload length (after the separator). Always
        /// `!= expected`.
        got: usize,
    },

    /// The version byte is not
    /// [`PUSH_ROUTING_SECRET_VERSION_V2`]. This is the variant a
    /// retired v1 (`0x01`) envelope lands on — the flag day is
    /// deliberate, see [`PUSH_ROUTING_SECRET_VERSION_V2`].
    #[error("push-routing-secret payload carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SECRET: [u8; PUSH_ROUTING_SECRET_LEN] = [0x42u8; PUSH_ROUTING_SECRET_LEN];
    const SAMPLE_EPOCH: u64 = 0x0102_0304_0506_0708;

    /// The retired v1 schema byte. Kept only here: the flag-day
    /// refusal test must name the exact byte it refuses, and no
    /// production path may re-mint it.
    const RETIRED_VERSION_V1: u8 = 0x01;

    /// Build a raw v1 envelope (domain ‖ 0x01 ‖ secret(32)) — the
    /// exact 62 bytes a pre-v2 peer would emit.
    fn v1_envelope(secret: &[u8; PUSH_ROUTING_SECRET_LEN]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        out.push(RETIRED_VERSION_V1);
        out.extend_from_slice(secret);
        out
    }

    // -------------------------------------------------------------------------
    // derive_push_gid
    // -------------------------------------------------------------------------

    #[test]
    fn derive_push_gid_is_deterministic() {
        let a = derive_push_gid(&SAMPLE_SECRET);
        let b = derive_push_gid(&SAMPLE_SECRET);
        assert_eq!(a, b);
    }

    #[test]
    fn derive_push_gid_golden() {
        // Golden vector pins the exact derivation for seed 0x42*32.
        // A change to `PUSH_GID_DERIVE_DOMAIN_V1`, the keyed-hash
        // primitive, or the key/message argument order surfaces
        // here as a mismatch — so every platform (Rust core + FFI)
        // is proven to derive the same push_gid. Pinned as a byte
        // array to keep the crate dependency-free (no `hex`).
        const GOLDEN_PUSH_GID: [u8; PUSH_GID_LEN] = [
            0x61, 0xf2, 0xd2, 0x24, 0xca, 0x4d, 0x00, 0xe4, 0x32, 0x7d, 0x98, 0x39, 0x02, 0x74,
            0xa4, 0x90, 0xdc, 0x2d, 0x0f, 0x03, 0x21, 0x98, 0xd0, 0x3c, 0xb7, 0x27, 0x44, 0xeb,
            0xa4, 0x01, 0x2a, 0x8a,
        ];
        let gid = derive_push_gid(&SAMPLE_SECRET);
        assert_eq!(gid, GOLDEN_PUSH_GID);
    }

    fn hex32(bytes: &[u8; 32]) -> String {
        use core::fmt::Write as _;
        let mut s = String::with_capacity(64);
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    #[test]
    fn golden_pins_the_wake_derivations() {
        // ADR-0078 section 3. A change here moves every group's wake key
        // and unlock key; the FFI surfaces pin the same values.
        assert_eq!(
            hex32(derive_push_wake_signing_seed(&SAMPLE_SECRET).as_bytes()),
            "c2b7e3f9f1879679b1a79d2e32d5bba52e3fb03e9ff1e22a8de363188cfae362",
        );
        assert_eq!(
            hex32(&derive_push_wake_public_key(&SAMPLE_SECRET)),
            "2153e575c8abc4a91af2b70258982068480edc0e0451f6b6bdf6c0692e5c8e56",
        );
        assert_eq!(
            hex32(derive_push_wake_unlock_key(&SAMPLE_SECRET).as_bytes()),
            "d76a3ce72be511a512c6afce89447fa73d9595c5b0fe0942780137a7d1f4cd0a",
        );
    }

    #[test]
    fn the_wake_derivations_are_separated_from_each_other_and_the_gid() {
        let gid = derive_push_gid(&SAMPLE_SECRET);
        let seed = *derive_push_wake_signing_seed(&SAMPLE_SECRET).as_bytes();
        let unlock = *derive_push_wake_unlock_key(&SAMPLE_SECRET).as_bytes();
        let pk = derive_push_wake_public_key(&SAMPLE_SECRET);
        for (a, b) in [
            (gid, seed),
            (gid, unlock),
            (seed, unlock),
            (gid, pk),
            (unlock, pk),
        ] {
            assert_ne!(a, b);
        }
        let other = [0x43u8; 32];
        assert_ne!(derive_push_wake_public_key(&other), pk);
        assert_ne!(*derive_push_wake_unlock_key(&other).as_bytes(), unlock);
    }

    #[test]
    fn derive_push_gid_differs_from_secret() {
        // The routing id is one-way: it is not the secret echoed.
        let gid = derive_push_gid(&SAMPLE_SECRET);
        assert_ne!(gid, SAMPLE_SECRET);
    }

    #[test]
    fn derive_push_gid_differs_from_group_id() {
        // The whole point of ADR-0042: push_gid must not equal the
        // DS journal key (group_id). A group_id is unrelated bytes
        // (an opaque MLS GroupId), so the derived gid must differ.
        let group_id = [0x42u8; 32]; // pretend the group_id == the secret bytes
        let gid = derive_push_gid(&SAMPLE_SECRET);
        assert_ne!(gid, group_id, "push_gid must never equal the group_id");
    }

    #[test]
    fn derive_push_gid_secret_separation() {
        // Two distinct secrets yield distinct routing ids.
        let a = derive_push_gid(&[0x01u8; 32]);
        let b = derive_push_gid(&[0x02u8; 32]);
        assert_ne!(a, b);
    }

    #[test]
    fn derive_push_gid_ignores_the_mint_epoch() {
        // C-60's stability property: `push_gid` has NO epoch input.
        // That is what lets a subscriber survive membership commits
        // without re-subscribing and keeps the iOS `thread-id`
        // constant. The v2 `mint_epoch` orders envelopes and MUST
        // NOT reach the derivation — structurally guaranteed by
        // `derive_push_gid` taking only the secret, and asserted
        // here end-to-end: the same secret minted under two very
        // different epochs routes to the same gid.
        let early = decode_push_routing_secret(&compose_push_routing_secret(1, &SAMPLE_SECRET))
            .expect("v2 envelope should decode");
        let late =
            decode_push_routing_secret(&compose_push_routing_secret(u64::MAX, &SAMPLE_SECRET))
                .expect("v2 envelope should decode");
        assert_ne!(early.mint_epoch, late.mint_epoch);
        assert_eq!(early.secret, late.secret);
        assert_eq!(
            derive_push_gid(&early.secret),
            derive_push_gid(&late.secret)
        );
    }

    // -------------------------------------------------------------------------
    // envelope round-trip
    // -------------------------------------------------------------------------

    #[test]
    fn compose_then_decode_round_trips() {
        let wrapped = compose_push_routing_secret(SAMPLE_EPOCH, &SAMPLE_SECRET);
        assert!(wrapped.starts_with(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1));
        assert_eq!(
            wrapped.len(),
            PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len() + PUSH_ROUTING_SECRET_PAYLOAD_LEN,
        );
        let decoded = decode_push_routing_secret(&wrapped).unwrap();
        assert_eq!(decoded.secret, SAMPLE_SECRET);
        assert_eq!(decoded.mint_epoch, SAMPLE_EPOCH);
    }

    #[test]
    fn compose_then_try_unwrap_round_trips() {
        let wrapped = compose_push_routing_secret(SAMPLE_EPOCH, &SAMPLE_SECRET);
        let decoded = try_unwrap_push_routing_secret(&wrapped).unwrap();
        assert_eq!(
            decoded,
            Some(PushRoutingSecretEnvelope {
                mint_epoch: SAMPLE_EPOCH,
                secret: SAMPLE_SECRET,
            }),
        );
    }

    #[test]
    fn round_trips_at_the_mint_epoch_extremes() {
        // Saturating the u64 must not wrap, truncate, or reorder
        // the big-endian field.
        for epoch in [0u64, 1, u64::from(u32::MAX), u64::MAX - 1, u64::MAX] {
            let wrapped = compose_push_routing_secret(epoch, &SAMPLE_SECRET);
            let decoded = decode_push_routing_secret(&wrapped).unwrap();
            assert_eq!(decoded.mint_epoch, epoch);
            assert_eq!(decoded.secret, SAMPLE_SECRET);
        }
    }

    #[test]
    fn compose_layout_is_domain_version_epoch_secret() {
        // Pin the exact v2 wire layout — the format is frozen.
        let wrapped = compose_push_routing_secret(SAMPLE_EPOCH, &SAMPLE_SECRET);
        let sep = PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(&wrapped[..sep], PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        assert_eq!(wrapped[sep], PUSH_ROUTING_SECRET_VERSION_V2);
        assert_eq!(
            &wrapped[sep + 1..sep + 1 + PUSH_ROUTING_SECRET_MINT_EPOCH_LEN],
            &SAMPLE_EPOCH.to_be_bytes(),
        );
        assert_eq!(
            &wrapped[sep + 1 + PUSH_ROUTING_SECRET_MINT_EPOCH_LEN..],
            &SAMPLE_SECRET,
        );
        assert_eq!(wrapped.len(), 70);
        assert_eq!(PUSH_ROUTING_SECRET_PAYLOAD_LEN, 41);
    }

    #[test]
    fn compose_encodes_the_mint_epoch_big_endian() {
        // Explicit endianness pin — three clients mirror this field
        // and a little-endian mirror would silently invert the
        // order for every pair whose byte-swapped epochs compare
        // the other way.
        let wrapped = compose_push_routing_secret(1, &SAMPLE_SECRET);
        let sep = PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len();
        let epoch = &wrapped[sep + 1..sep + 1 + PUSH_ROUTING_SECRET_MINT_EPOCH_LEN];
        assert_eq!(epoch, &[0, 0, 0, 0, 0, 0, 0, 1]);
    }

    // -------------------------------------------------------------------------
    // fail-closed decode paths
    // -------------------------------------------------------------------------

    #[test]
    fn try_unwrap_returns_none_for_non_matching_plaintext() {
        let intro = b"hi alice it's bob";
        assert!(try_unwrap_push_routing_secret(intro).unwrap().is_none());
    }

    #[test]
    fn decode_rejects_non_matching_plaintext() {
        let intro = b"hi alice it's bob";
        assert_eq!(
            decode_push_routing_secret(intro).unwrap_err(),
            PushRoutingSecretError::WrongDomain,
        );
    }

    #[test]
    fn decode_empty_falls_through_via_try_unwrap() {
        assert!(try_unwrap_push_routing_secret(&[]).unwrap().is_none());
        assert_eq!(
            decode_push_routing_secret(&[]).unwrap_err(),
            PushRoutingSecretError::WrongDomain,
        );
    }

    /// Domain separator + version + `tail`, bypassing `compose` so
    /// a test can craft an otherwise-unreachable payload.
    fn raw_envelope(version: u8, tail: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        out.push(version);
        out.extend_from_slice(tail);
        out
    }

    #[test]
    fn decode_rejects_empty_payload() {
        // Domain separator and nothing else: no version byte to
        // discriminate on, so this can only be a length failure.
        assert_eq!(
            decode_push_routing_secret(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1).unwrap_err(),
            PushRoutingSecretError::BadLength {
                expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
                got: 0,
            },
        );
    }

    #[test]
    fn decode_rejects_version_byte_only() {
        assert_eq!(
            decode_push_routing_secret(&raw_envelope(PUSH_ROUTING_SECRET_VERSION_V2, &[]))
                .unwrap_err(),
            PushRoutingSecretError::BadLength {
                expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
                got: 1,
            },
        );
    }

    #[test]
    fn decode_rejects_truncated_mint_epoch() {
        // Version + 7 of the 8 epoch bytes, no secret.
        assert_eq!(
            decode_push_routing_secret(&raw_envelope(PUSH_ROUTING_SECRET_VERSION_V2, &[0u8; 7]))
                .unwrap_err(),
            PushRoutingSecretError::BadLength {
                expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
                got: 8,
            },
        );
    }

    #[test]
    fn decode_rejects_truncated_payload() {
        // Version + full epoch + only 4 secret bytes.
        let mut tail = SAMPLE_EPOCH.to_be_bytes().to_vec();
        tail.extend_from_slice(&[0u8; 4]);
        assert_eq!(
            decode_push_routing_secret(&raw_envelope(PUSH_ROUTING_SECRET_VERSION_V2, &tail))
                .unwrap_err(),
            PushRoutingSecretError::BadLength {
                expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
                got: 13,
            },
        );
    }

    #[test]
    fn decode_rejects_missing_secret() {
        // Version + full epoch, no secret at all.
        assert_eq!(
            decode_push_routing_secret(&raw_envelope(
                PUSH_ROUTING_SECRET_VERSION_V2,
                &SAMPLE_EPOCH.to_be_bytes(),
            ))
            .unwrap_err(),
            PushRoutingSecretError::BadLength {
                expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
                got: 9,
            },
        );
    }

    #[test]
    fn decode_rejects_overlong_payload() {
        // A well-formed v2 envelope with one trailing byte. Exact
        // length, not a floor — a padded envelope is refused.
        let mut wrapped = compose_push_routing_secret(SAMPLE_EPOCH, &SAMPLE_SECRET);
        wrapped.push(0xAB);
        assert_eq!(
            decode_push_routing_secret(&wrapped).unwrap_err(),
            PushRoutingSecretError::BadLength {
                expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
                got: PUSH_ROUTING_SECRET_PAYLOAD_LEN + 1,
            },
        );
    }

    #[test]
    fn decode_rejects_v1_envelope_as_unknown_version() {
        // The flag day. A pre-v2 peer's 62-byte envelope carries no
        // `mint_epoch`, so it cannot be placed in the total order;
        // accepting it would re-open the F-A replay. It must be
        // refused, and — because the version gate runs before the
        // length gate — refused by NAME, not as a length complaint.
        let v1 = v1_envelope(&SAMPLE_SECRET);
        assert_eq!(v1.len(), 62, "a v1 envelope is 62 bytes");
        assert_eq!(
            decode_push_routing_secret(&v1).unwrap_err(),
            PushRoutingSecretError::UnknownVersion(RETIRED_VERSION_V1),
        );
        // And the dispatcher-facing helper must surface the same
        // error rather than falling through to the chat parser.
        assert_eq!(
            try_unwrap_push_routing_secret(&v1).unwrap_err(),
            PushRoutingSecretError::UnknownVersion(RETIRED_VERSION_V1),
        );
    }

    #[test]
    fn decode_refuses_every_other_version() {
        // Only 0x02 is accepted — including a would-be "future" v3,
        // which must fail closed rather than be parsed optimistically.
        let mut tail = SAMPLE_EPOCH.to_be_bytes().to_vec();
        tail.extend_from_slice(&SAMPLE_SECRET);
        for version in 0u8..=u8::MAX {
            let wrapped = raw_envelope(version, &tail);
            if version == PUSH_ROUTING_SECRET_VERSION_V2 {
                assert!(
                    decode_push_routing_secret(&wrapped).is_ok(),
                    "v{version} is the accepted schema",
                );
            } else {
                assert_eq!(
                    decode_push_routing_secret(&wrapped).unwrap_err(),
                    PushRoutingSecretError::UnknownVersion(version),
                    "version {version:#04x} must be refused",
                );
            }
        }
    }

    #[test]
    fn version_gate_runs_before_the_length_gate() {
        // Deliberate ordering, pinned: a wrong-version payload that
        // is ALSO the wrong length reports the version, because the
        // version is what makes the length unknowable.
        assert_eq!(
            decode_push_routing_secret(&raw_envelope(0x7F, &[0u8; 3])).unwrap_err(),
            PushRoutingSecretError::UnknownVersion(0x7F),
        );
    }

    // -------------------------------------------------------------------------
    // format pins + cross-kind isolation
    // -------------------------------------------------------------------------

    #[test]
    fn domain_separator_is_29_bytes_and_ends_with_null() {
        assert_eq!(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len(), 29);
        assert_eq!(*PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.last().unwrap(), 0u8);
        let printable = &PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1[..28];
        assert_eq!(
            core::str::from_utf8(printable).unwrap(),
            "lakko/push-routing-secret/v1",
        );
    }

    #[test]
    fn domain_separator_bytes_survived_the_v2_bump() {
        // The v1 -> v2 bump moved the VERSION BYTE, never these 29
        // bytes. `incoming_envelope` dispatches on this exact
        // prefix, so a changed separator would send the envelope
        // through to the chat parser and render it as garbage text
        // on every peer. Pinned literally so an edit fails here
        // rather than in a client's message list.
        assert_eq!(
            PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            b"lakko/push-routing-secret/v1\x00",
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        // Cross-kind isolation across every envelope-domain
        // constant in the crate — a future collision surfaces here.
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            b"lakko-invite-revoke/v1\x00",
            b"lakko-call-invite-revoke/v1\x00",
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
        ];
        for d in other_domains {
            assert_ne!(
                *d, PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
                "push-routing-secret separator must be byte-distinct from {d:?}",
            );
        }
    }

    #[test]
    fn does_not_cross_dispatch_as_sibling() {
        // A push-routing-secret envelope MUST NOT decode as a call
        // welcome / KP-forward / media-state, and vice versa.
        use crate::call_kp_forward::{try_unwrap_call_kp_forward, wrap_call_kp_forward};
        use crate::call_welcome::{try_unwrap_call_welcome, wrap_call_welcome};
        use crate::media_state::{compose_media_state, try_unwrap_media_state};

        let envelope = compose_push_routing_secret(SAMPLE_EPOCH, &SAMPLE_SECRET);
        assert!(try_unwrap_call_welcome(&envelope).unwrap().is_none());
        assert!(try_unwrap_call_kp_forward(&envelope).unwrap().is_none());
        assert!(try_unwrap_media_state(&envelope).unwrap().is_none());

        let welcome = wrap_call_welcome(&vec![0xABu8; 256]);
        assert!(try_unwrap_push_routing_secret(&welcome).unwrap().is_none());
        let kp = wrap_call_kp_forward(&vec![0xCDu8; 512]);
        assert!(try_unwrap_push_routing_secret(&kp).unwrap().is_none());
        let media = compose_media_state(true, true);
        assert!(try_unwrap_push_routing_secret(&media).unwrap().is_none());
    }

    // -------------------------------------------------------------------------
    // the canonical total order (F-A / G-36 residual 9)
    // -------------------------------------------------------------------------

    const LOW_SECRET: [u8; PUSH_ROUTING_SECRET_LEN] = [0x11u8; PUSH_ROUTING_SECRET_LEN];
    const HIGH_SECRET: [u8; PUSH_ROUTING_SECRET_LEN] = [0x99u8; PUSH_ROUTING_SECRET_LEN];

    #[test]
    fn is_newer_when_the_epoch_is_strictly_greater() {
        assert!(push_routing_secret_is_newer(
            9,
            &LOW_SECRET,
            8,
            &HIGH_SECRET
        ));
    }

    #[test]
    fn is_not_newer_when_the_epoch_is_older() {
        assert!(!push_routing_secret_is_newer(
            7,
            &HIGH_SECRET,
            8,
            &LOW_SECRET
        ));
    }

    #[test]
    fn epoch_dominates_the_secret_tiebreak() {
        // An older envelope whose secret sorts LARGER is still not
        // newer — the epoch is the primary key, and this is the
        // exact shape of the F-A stale re-broadcast (an old secret
        // arriving after a rotation).
        assert!(!push_routing_secret_is_newer(
            1,
            &HIGH_SECRET,
            2,
            &LOW_SECRET
        ));
        assert!(push_routing_secret_is_newer(
            2,
            &LOW_SECRET,
            1,
            &HIGH_SECRET
        ));
    }

    #[test]
    fn same_epoch_tiebreaks_on_the_secret_bytes() {
        assert!(push_routing_secret_is_newer(
            5,
            &HIGH_SECRET,
            5,
            &LOW_SECRET
        ));
        assert!(!push_routing_secret_is_newer(
            5,
            &LOW_SECRET,
            5,
            &HIGH_SECRET
        ));
    }

    #[test]
    fn same_epoch_tiebreak_is_lexicographic_not_numeric() {
        // Compared as a byte STRING, left to right: a secret whose
        // first byte is larger wins regardless of the tail. A
        // client comparing, say, the trailing bytes first would
        // diverge here.
        let mut a = [0u8; PUSH_ROUTING_SECRET_LEN];
        a[0] = 0x02;
        let mut b = [0xFFu8; PUSH_ROUTING_SECRET_LEN];
        b[0] = 0x01;
        assert!(push_routing_secret_is_newer(5, &a, 5, &b));
        assert!(!push_routing_secret_is_newer(5, &b, 5, &a));
    }

    #[test]
    fn an_identical_pair_is_not_newer() {
        // THE F-A property. A benign retry of the secret a device
        // already holds must not read as an update: equal is not
        // greater. Without this, a re-broadcast is indistinguishable
        // from a rotation and the accept-identical fast path would
        // churn the routing id on every retry.
        assert!(!push_routing_secret_is_newer(
            5,
            &SAMPLE_SECRET,
            5,
            &SAMPLE_SECRET
        ));
    }

    #[test]
    fn a_stale_rebroadcast_never_undoes_a_rotation() {
        // The end-to-end F-A scenario, at the ordering layer. A
        // device holds the pre-rotation secret S0 (minted at epoch
        // 10) in a failed-broadcast `pending` row; an admin has
        // since rotated the group to S1 at epoch 11. The stale
        // re-broadcast of S0 must be refused by every member that
        // already applied S1 — for BOTH tiebreak directions, so the
        // result cannot hinge on which secret happens to sort
        // higher.
        for (older, newer) in [(LOW_SECRET, HIGH_SECRET), (HIGH_SECRET, LOW_SECRET)] {
            assert!(
                !push_routing_secret_is_newer(10, &older, 11, &newer),
                "a stale re-broadcast must never supersede the rotation",
            );
            assert!(
                push_routing_secret_is_newer(11, &newer, 10, &older),
                "the rotation must supersede the secret it replaced",
            );
        }
    }

    #[test]
    fn the_owner_can_lose_an_exact_epoch_tie() {
        // Documented consequence, asserted so it cannot be "fixed"
        // by accident: a total order and unconditional
        // owner-priority cannot both hold. Under ADR-0043 option
        // (b) one removal yields N+1 same-epoch mints, so this is
        // the common path. Model the owner minting the
        // lower-sorting secret at the same epoch as an admin — the
        // admin's wins, and every member converges on it.
        let owner_mint = LOW_SECRET;
        let admin_mint = HIGH_SECRET;
        assert!(push_routing_secret_is_newer(
            42,
            &admin_mint,
            42,
            &owner_mint
        ));
        assert!(!push_routing_secret_is_newer(
            42,
            &owner_mint,
            42,
            &admin_mint
        ));
    }

    #[test]
    fn is_newer_rejects_a_wrong_length_secret() {
        // Fail-closed on both sides: a malformed incoming secret
        // never displaces a stored one, and a corrupt stored value
        // is left in place rather than overwritten on the strength
        // of a comparison against something that was never a
        // secret. Must not panic.
        let short = [0u8; PUSH_ROUTING_SECRET_LEN - 1];
        let long = [0xFFu8; PUSH_ROUTING_SECRET_LEN + 1];
        assert!(!push_routing_secret_is_newer(99, &short, 1, &SAMPLE_SECRET));
        assert!(!push_routing_secret_is_newer(99, &long, 1, &SAMPLE_SECRET));
        assert!(!push_routing_secret_is_newer(99, &SAMPLE_SECRET, 1, &short));
        assert!(!push_routing_secret_is_newer(99, &SAMPLE_SECRET, 1, &long));
        assert!(!push_routing_secret_is_newer(99, &[], 1, &[]));
    }

    /// A spread of `(epoch, secret)` pairs exercising both order
    /// keys, for the algebraic checks below.
    fn order_sample() -> Vec<(u64, [u8; PUSH_ROUTING_SECRET_LEN])> {
        let mut out = Vec::new();
        for epoch in [0u64, 1, 7, u64::MAX] {
            for first in [0x00u8, 0x11, 0x99, 0xFF] {
                let mut secret = [0x55u8; PUSH_ROUTING_SECRET_LEN];
                secret[0] = first;
                out.push((epoch, secret));
            }
        }
        out
    }

    #[test]
    fn is_newer_is_a_strict_total_order() {
        // Irreflexive, antisymmetric, total (exactly one of a>b,
        // b>a, a==b holds), and transitive. Totality is what makes
        // the order usable as a merge rule at all; without it two
        // members could each refuse the other's envelope and
        // permanently disagree.
        let sample = order_sample();
        for (ae, asec) in &sample {
            assert!(
                !push_routing_secret_is_newer(*ae, asec, *ae, asec),
                "irreflexive",
            );
            for (be, bsec) in &sample {
                let ab = push_routing_secret_is_newer(*ae, asec, *be, bsec);
                let ba = push_routing_secret_is_newer(*be, bsec, *ae, asec);
                let eq = ae == be && asec == bsec;
                assert!(!(ab && ba), "antisymmetric");
                assert_eq!(
                    u8::from(ab) + u8::from(ba) + u8::from(eq),
                    1,
                    "exactly one of greater / less / equal must hold",
                );
                for (ce, csec) in &sample {
                    if ab && push_routing_secret_is_newer(*be, bsec, *ce, csec) {
                        assert!(
                            push_routing_secret_is_newer(*ae, asec, *ce, csec),
                            "transitive",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn is_newer_matches_lexicographic_wire_bytes() {
        // Anti-drift equivalence for the client mirrors: comparing
        // the `(u64, [u8;32])` pair is identical to comparing the
        // concatenated big-endian wire bytes
        // `mint_epoch_be(8) ‖ secret(32)` as one byte string. A
        // platform that finds the flat-bytes form easier gets the
        // same answer, so neither implementation can drift from the
        // other.
        let sample = order_sample();
        for (ae, asec) in &sample {
            for (be, bsec) in &sample {
                let mut a_wire = ae.to_be_bytes().to_vec();
                a_wire.extend_from_slice(asec);
                let mut b_wire = be.to_be_bytes().to_vec();
                b_wire.extend_from_slice(bsec);
                assert_eq!(
                    push_routing_secret_is_newer(*ae, asec, *be, bsec),
                    a_wire > b_wire,
                    "structured and flat-wire comparison must agree",
                );
            }
        }
    }

    #[test]
    fn convergence_is_arrival_order_independent() {
        // Two envelopes A and B, applied in either order by two
        // members (and by a member that was offline across both),
        // must land every one of them on the same winner. This is
        // the property the whole design rests on, and the one
        // ADR-0042's owner-priority rule provided only for the
        // owner's own envelope.
        fn apply(
            stored: Option<PushRoutingSecretEnvelope>,
            incoming: PushRoutingSecretEnvelope,
        ) -> PushRoutingSecretEnvelope {
            match stored {
                None => incoming,
                Some(cur) if incoming.supersedes(&cur) => incoming,
                Some(cur) => cur,
            }
        }

        let sample = order_sample();
        for (ae, asec) in &sample {
            for (be, bsec) in &sample {
                let a = PushRoutingSecretEnvelope {
                    mint_epoch: *ae,
                    secret: *asec,
                };
                let b = PushRoutingSecretEnvelope {
                    mint_epoch: *be,
                    secret: *bsec,
                };
                let ab = apply(Some(apply(None, a)), b);
                let ba = apply(Some(apply(None, b)), a);
                assert_eq!(ab, ba, "arrival order must not change the winner");
                // And re-applying either envelope is a no-op, so a
                // retry after convergence cannot move anyone.
                assert_eq!(apply(Some(ab), a), ab);
                assert_eq!(apply(Some(ab), b), ab);
            }
        }
    }

    #[test]
    fn supersedes_matches_the_free_function() {
        // The convenience method must not become a second,
        // subtly-different implementation of the order.
        let sample = order_sample();
        for (ae, asec) in &sample {
            for (be, bsec) in &sample {
                let a = PushRoutingSecretEnvelope {
                    mint_epoch: *ae,
                    secret: *asec,
                };
                let b = PushRoutingSecretEnvelope {
                    mint_epoch: *be,
                    secret: *bsec,
                };
                assert_eq!(
                    a.supersedes(&b),
                    push_routing_secret_is_newer(*ae, asec, *be, bsec),
                );
            }
        }
    }

    #[test]
    fn order_survives_a_wire_round_trip() {
        // The order must be decided on decoded envelopes exactly as
        // it is on the values that were composed — no field gets
        // truncated or reordered in transit.
        let sample = order_sample();
        for (ae, asec) in &sample {
            for (be, bsec) in &sample {
                let a = decode_push_routing_secret(&compose_push_routing_secret(*ae, asec))
                    .expect("composed envelope must decode");
                let b = decode_push_routing_secret(&compose_push_routing_secret(*be, bsec))
                    .expect("composed envelope must decode");
                assert_eq!(
                    a.supersedes(&b),
                    push_routing_secret_is_newer(*ae, asec, *be, bsec),
                );
            }
        }
    }
}
