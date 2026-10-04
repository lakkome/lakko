// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! SFrame (RFC 9605) media keying — Phase 3 scaffolding.
//!
//! Two layers of key derivation sit between the MLS group's
//! exporter and the per-frame AEAD nonce:
//!
//! ```text
//!   MLS exporter (`lakko/call/v1`, ctx = call_id, len = 32)
//!         │
//!         ▼  CallKey (32 B seed; same on every participant)
//!         │
//!         │  HKDF-SHA-256 over ("lakko/sframe/v1/key" | KID)
//!         ▼        where KID = (sender_leaf << 8) | stream_index  (ADR-0035)
//!   ParticipantKey
//!         │
//!         │  per-frame: nonce = nonce_salt XOR (frame_counter as 8 big-endian
//!         │             bytes in the low 8 bytes of the 12-byte nonce)
//!         ▼  (64-bit counter, random start, +1 per sealed frame — ADR-0069)
//!   AEAD seal / open
//! ```
//!
//! ## The frame counter (ADR-0069)
//!
//! The AEAD `(key, nonce_salt)` is a pure function of `(seed, KID)`, so the
//! only thing that separates two frames under one key is the counter. A
//! sender cannot remember across a reload, a rejoin or a republished track
//! what it already sealed, so a fresh [`ParticipantKey`] does **not** start
//! at a fixed value: it starts at a uniformly random counter in `[0, 2^62)`
//! drawn from the system CSPRNG and counts up. Two keys derived under one
//! `(seed, KID)` reuse a nonce only if their counter ranges overlap; with
//! `L1` and `L2` frames sealed that happens with probability at most
//! `(L1 + L2) / 2^62`. Receivers keep no counter: they take the 64-bit value
//! from the SFrame header.
//!
//! The per-`(participant, stream)` split lets the SFU forward
//! each stream without ever decrypting; the receiver
//! re-derives the sender's [`ParticipantKey`] via the KID
//! carried in the SFrame header (which the SFU forwards
//! verbatim). The KID is a per-`(sender_leaf, stream)` key
//! identifier (ADR-0035, superseding the ADR-0034 per-media-type
//! discriminator): folding the FULL KID into the derivation
//! gives every one of a leaf's concurrent streams — audio,
//! camera, screen-share — a disjoint `(key, nonce_salt)` space,
//! so their independent per-stream counters can never reuse a
//! `(key, nonce)` pair no matter how many streams share a leaf.
//!
//! ## Re-keying on add / remove
//!
//! Per RFC 9420 + ADR-0001, every MLS membership change
//! advances the group epoch. The caller is expected to
//! re-export `lakko/call/v1` after every commit it
//! processes, build a fresh [`CallKey`], and roll the
//! [`ParticipantKey`]s forward with a 1-second overlap so
//! in-flight frames under the old key can still decode. The
//! overlap window itself is a Phase 3.2 concern; this module
//! is responsible for the keying math, not the rotation
//! choreography.

use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce};
use lakko_crypto::hash;
use lakko_crypto::kdf;
use lakko_crypto::secret::SecretBytes;
use lakko_mls::identity::Identity;
use lakko_mls::Group;

use crate::error::{Error, Result};
use crate::header::SframeHeader;

/// MLS exporter label for SFrame call keying. Pinned ASCII;
/// any change is a breaking protocol change to every
/// existing call surface.
///
/// Matches the placeholder in [`crate`] doc-comment +
/// `docs/adr/0001-mls-only.md` §"Звонки" + the audit pack at
/// `docs/security/crypto-overview.md`.
pub const SFRAME_EXPORT_LABEL: &str = "lakko/call/v1";

/// Length of the per-call seed derived from the MLS exporter
/// (and of every derived [`ParticipantKey`] AEAD key).
pub const CALL_KEY_LEN: usize = 32;

/// Length of the AEAD nonce — fixed by RFC 8439 / RFC 9605.
const NONCE_LEN: usize = 12;

/// HKDF info string for the per-participant key derivation.
/// Encodes the SFrame role of the bytes so an attacker who
/// confuses contexts (call-key-as-nonce, nonce-as-key) hits
/// a distinct label.
const PARTICIPANT_KEY_INFO: &[u8] = b"lakko/sframe/v1/key";

/// HKDF info string for the per-frame nonce derivation.
const FRAME_NONCE_INFO: &[u8] = b"lakko/sframe/v1/nonce";

/// Last counter value a key may reach (ADR-0069: the counter is 64 bits, the
/// width RFC 9605 §4.3 encodes). A key whose counter stands here refuses to
/// seal ([`Error::CounterExhausted`]) and never wraps.
const MAX_FRAME_COUNTER: u64 = u64::MAX;

/// Exclusive upper bound of the random start of a fresh key's counter
/// (ADR-0069): the start is uniform in `[0, 2^62)`, which leaves at least
/// `3 * 2^62` values before [`MAX_FRAME_COUNTER`] from the highest start.
pub const COUNTER_START_BOUND: u64 = 1 << 62;

/// Draw the start of a fresh key's frame counter: uniform in
/// `[0, COUNTER_START_BOUND)` from the system CSPRNG (ADR-0069). Eight random
/// bytes with the top two bits cleared are exactly uniform over that range.
fn random_counter_start() -> u64 {
    let mut bytes = [0u8; 8];
    lakko_crypto::random::fill(&mut bytes);
    u64::from_be_bytes(bytes) & (COUNTER_START_BOUND - 1)
}

// =============================================================================
// CallKey
// =============================================================================

/// Per-call symmetric seed.
///
/// Every participant on the same call epoch derives the same
/// [`CallKey`] from the same MLS group at the same epoch via
/// the [`SFRAME_EXPORT_LABEL`] exporter.
pub struct CallKey {
    seed: SecretBytes<CALL_KEY_LEN>,
}

impl CallKey {
    /// Derive a [`CallKey`] from a conversation group's MLS
    /// exporter. `call_id` is opaque per-call routing bytes
    /// the caller chose (typically the random pseudonym the
    /// SFU minted on call-create); receivers reach the same
    /// value because they got the same `call_id` via the
    /// signaling channel.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Exporter`] when the MLS group's
    /// `export_secret` rejects the label or length.
    pub fn derive(group: &Group, identity: &Identity, call_id: &[u8]) -> Result<Self> {
        let secret = group.export_secret(identity, SFRAME_EXPORT_LABEL, call_id, CALL_KEY_LEN)?;
        let mut bytes = [0u8; CALL_KEY_LEN];
        bytes.copy_from_slice(&secret);
        Ok(Self {
            seed: SecretBytes::new(bytes),
        })
    }

    /// Construct a [`CallKey`] from a known-good seed.
    /// Test-only and FFI-only — production callers go through
    /// [`Self::derive`].
    ///
    /// # The same seed may be used again (ADR-0069)
    ///
    /// A second [`CallKey`] built from a seed already used for sealing, and a
    /// second [`ParticipantKey`] derived from it for the same KID, hold the
    /// same AEAD key and nonce salt as the first. That is safe because each
    /// derived key starts its counter at an independent random value in
    /// `[0, 2^62)`; see [`Self::participant_key`] for the bound.
    #[must_use]
    pub fn from_seed(seed: [u8; CALL_KEY_LEN]) -> Self {
        Self {
            seed: SecretBytes::new(seed),
        }
    }

    /// Borrow the raw seed bytes. Avoid logging or otherwise
    /// leaking — these are the live call key material.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; CALL_KEY_LEN] {
        self.seed.as_bytes()
    }

    /// Constant-time equality of two call-key seeds.
    ///
    /// The rotation path ([`crate::CallKeyRing::rekey`],
    /// [`crate::GroupCallSession::refresh_call_key`]) uses this to detect a
    /// spurious **same-seed** refresh: a rotation to an unchanged seed is a
    /// no-op (defence in depth; since ADR-0069 a key freshly derived under an
    /// already-used seed is safe by its random counter start, and the guard is
    /// no longer the only thing between a caller and a reused nonce).
    /// Constant-time (via the inner [`SecretBytes`] comparison) so the check
    /// never leaks the seed through a variable-time byte compare of live key
    /// material.
    #[must_use]
    pub fn seed_eq(&self, other: &Self) -> bool {
        self.seed == other.seed
    }

    /// Derive a per-`(sender_leaf, stream)` key from a composite SFrame `kid`
    /// (ADR-0035). Each participant on the call holds their own
    /// [`ParticipantKey`] per stream for sealing and re-derives every peer's
    /// [`ParticipantKey`] for opening.
    ///
    /// `kid = (sender_leaf << 8) | stream_index` — compose it with
    /// [`SframeHeader::kid_for`], split it back with [`SframeHeader::leaf_of`] /
    /// [`SframeHeader::stream_of`]. HKDF-SHA-256 folds the **FULL** KID into
    /// both info strings:
    ///
    /// - key:   `"lakko/sframe/v1/key"   || kid_be`
    /// - salt:  `"lakko/sframe/v1/nonce" || kid_be`
    ///
    /// Receivers reach the same value because the SFU forwards the SFrame header
    /// (which carries the KID) verbatim — invariant #2 — so both endpoints see
    /// the identical KID with no local media-type or MID signal (ADR-0035
    /// supersedes the ADR-0034 per-media-TYPE tag, which could not tell two
    /// same-kind streams apart nor survive an SFU). Distinct KIDs — including two
    /// video streams `(leaf, 1)` and `(leaf, 2)` — therefore key into disjoint
    /// `(key, nonce_salt)` spaces, so their independent counters cannot reuse a
    /// `(key, nonce)` pair.
    ///
    /// # The counter starts at a random value (ADR-0069)
    ///
    /// The AEAD `(key, nonce_salt)` is a pure function of `(seed, kid)` and the
    /// nonce is `nonce_salt XOR counter`, so two keys derived under one
    /// `(seed, kid)` would reuse a `(key, nonce)` pair — ChaCha20 keystream
    /// reuse and a forgeable Poly1305 tag — at every counter value they both
    /// seal. Callers do derive such keys: rejoining a call in the same MLS
    /// epoch, reloading a tab, and publishing a camera track again all build a
    /// new key under an unchanged seed and KID, and a client that reloaded has
    /// no memory of what it sealed. So the returned key does not start at a
    /// fixed counter: it starts at a uniformly random value in `[0, 2^62)`
    /// drawn from the system CSPRNG, and the first frame it seals carries
    /// `start + 1` ([`ParticipantKey::seal`] increments before use).
    ///
    /// Two keys under one `(seed, kid)` reuse a nonce only if the ranges of
    /// counters they seal overlap. If one seals `L1` frames and the other `L2`,
    /// that happens with probability at most `(L1 + L2) / 2^62`; at a hundred
    /// million frames each it is below `2^-34`. The bound is per pair of keys.
    ///
    /// The same-seed guard on the rotation path
    /// ([`crate::CallKeyRing::rekey`]) stays, as defence in depth.
    ///
    /// Receivers use a key from this function only to open; opening takes the
    /// counter from the frame header and never reads the key's own.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Crypto`] when HKDF rejects the input
    /// length (practically unreachable with valid inputs).
    pub fn participant_key(&self, kid: u64) -> Result<ParticipantKey> {
        let mut info = Vec::with_capacity(PARTICIPANT_KEY_INFO.len() + 8);
        info.extend_from_slice(PARTICIPANT_KEY_INFO);
        info.extend_from_slice(&kid.to_be_bytes());
        let key_secret = kdf::hkdf_sha256::<CALL_KEY_LEN>(self.seed.as_bytes(), b"", &info)
            .map_err(|e| Error::Crypto(format!("hkdf: {e:?}")))?;
        // Also derive the per-participant nonce salt so the
        // 12-byte AEAD nonce is uniformly random over the
        // counter space, not just the counter byte itself.
        let mut nonce_info = Vec::with_capacity(FRAME_NONCE_INFO.len() + 8);
        nonce_info.extend_from_slice(FRAME_NONCE_INFO);
        nonce_info.extend_from_slice(&kid.to_be_bytes());
        let nonce_secret = kdf::hkdf_sha256::<32>(self.seed.as_bytes(), b"", &nonce_info)
            .map_err(|e| Error::Crypto(format!("hkdf: {e:?}")))?;
        let mut nonce_salt = [0u8; NONCE_LEN];
        nonce_salt.copy_from_slice(&nonce_secret.as_bytes()[..NONCE_LEN]);
        Ok(ParticipantKey {
            kid,
            key: SecretBytes::new(*key_secret.as_bytes()),
            nonce_salt,
            counter: random_counter_start(),
        })
    }

    /// Convenience wrapper over [`Self::participant_key`] that composes the KID
    /// from a `(sender_leaf, stream_index)` pair (ADR-0035). Sender-side callers
    /// that assign a stream ordinal reach for this; receivers that already hold
    /// the KID from the SFrame header use [`Self::participant_key`] directly.
    ///
    /// The key starts at a random counter, as documented on
    /// [`Self::participant_key`] (ADR-0069).
    ///
    /// # Errors
    ///
    /// - [`Error::Crypto`] wrapping a
    ///   [`HeaderError::StreamIndexOverflow`](crate::header::HeaderError::StreamIndexOverflow)
    ///   when `stream_index` does not fit the 8-bit stream field, or an HKDF
    ///   failure (practically unreachable).
    pub fn participant_key_for(
        &self,
        sender_leaf: u32,
        stream_index: u32,
    ) -> Result<ParticipantKey> {
        let kid = SframeHeader::kid_for(sender_leaf, stream_index)
            .map_err(|e| Error::Crypto(format!("kid compose: {e}")))?;
        self.participant_key(kid)
    }
}

// =============================================================================
// ParticipantKey
// =============================================================================

/// Per-participant AEAD key + frame counter for SFrame
/// (RFC 9605).
///
/// Sealing increments the counter; opening accepts an
/// explicit `frame_counter` from the SFrame header so a
/// receiver that joins mid-call can re-derive the matching
/// nonce.
pub struct ParticipantKey {
    /// The composite SFrame KID this key derives from (ADR-0035):
    /// `(sender_leaf << 8) | stream_index`. Stamped verbatim in the header of
    /// every frame this key seals, and checked against the header on open.
    kid: u64,
    key: SecretBytes<CALL_KEY_LEN>,
    /// 12-byte nonce salt derived from the call key. Per-
    /// frame nonce is `salt XOR (counter as 8 big-endian
    /// bytes in the low 8 bytes)` (ADR-0069).
    nonce_salt: [u8; NONCE_LEN],
    /// Counter of the last frame sealed; the next frame carries `counter + 1`.
    /// Starts at a random value in `[0, 2^62)` (ADR-0069). Only sealing reads
    /// or advances it.
    counter: u64,
}

impl ParticipantKey {
    /// The composite SFrame KID this key derives from (ADR-0035).
    #[must_use]
    pub fn kid(&self) -> u64 {
        self.kid
    }

    /// Sender leaf index this key derives from — the high bits of the KID
    /// (ADR-0035: `kid >> 8`).
    #[must_use]
    pub fn sender_leaf(&self) -> u32 {
        SframeHeader::leaf_of(self.kid)
    }

    /// Per-sender stream ordinal this key derives from — the low byte of the
    /// KID (ADR-0035: `kid & 0xFF`).
    #[must_use]
    pub fn stream_index(&self) -> u8 {
        SframeHeader::stream_of(self.kid)
    }

    /// Borrow the raw AEAD key bytes. Test-only — production
    /// call sites pass the key to [`Self::seal`] /
    /// [`Self::open`].
    #[must_use]
    pub fn key_bytes(&self) -> &[u8; CALL_KEY_LEN] {
        self.key.as_bytes()
    }

    /// 12-byte nonce salt — exposed for test / interop with
    /// alternative SFrame implementations that compose the
    /// nonce externally.
    #[must_use]
    pub fn nonce_salt(&self) -> &[u8; NONCE_LEN] {
        &self.nonce_salt
    }

    /// Current counter value — exposed for diagnostics: the counter of the
    /// last frame this key sealed, or its random start (ADR-0069) when it has
    /// sealed none. The canonical increment happens inside [`Self::seal`].
    #[must_use]
    pub fn counter(&self) -> u64 {
        self.counter
    }

    /// Seal `plaintext` under the next counter value. Returns
    /// `(frame_counter, ciphertext_with_tag)`. The caller
    /// embeds `frame_counter` in the SFrame header so the
    /// receiver can re-derive the nonce.
    ///
    /// # Errors
    ///
    /// - [`Error::CounterExhausted`] when the 64-bit counter
    ///   stands at its last value. Nothing is sealed and the
    ///   counter does not move; the caller derives a new key.
    /// - [`Error::Crypto`] on AEAD failure (practically
    ///   unreachable).
    pub fn seal(&mut self, aad: &[u8], plaintext: &[u8]) -> Result<(u64, Vec<u8>)> {
        if self.counter == MAX_FRAME_COUNTER {
            return Err(Error::CounterExhausted);
        }
        self.counter += 1;
        let frame_counter = self.counter;
        let nonce = self.compose_nonce(frame_counter);
        let key = AeadKey::new(*self.key.as_bytes());
        let ct = aead::seal(Algorithm::ChaCha20Poly1305, &key, &nonce, aad, plaintext)
            .map_err(|e| Error::Crypto(format!("aead seal: {e:?}")))?;
        Ok((frame_counter, ct))
    }

    /// Open `ciphertext` (which includes the trailing AEAD
    /// tag) using `frame_counter` from the SFrame header.
    ///
    /// # Errors
    ///
    /// - [`Error::Crypto`] on AEAD authentication failure
    ///   (wrong key / wrong AAD / tampered ciphertext /
    ///   wrong counter).
    pub fn open(&self, aad: &[u8], ciphertext: &[u8], frame_counter: u64) -> Result<Vec<u8>> {
        let nonce = self.compose_nonce(frame_counter);
        let key = AeadKey::new(*self.key.as_bytes());
        aead::open(Algorithm::ChaCha20Poly1305, &key, &nonce, aad, ciphertext)
            .map_err(|e| Error::Crypto(format!("aead open: {e:?}")))
    }

    /// Seal `plaintext` into a complete RFC 9605 wire frame:
    /// `SframeHeader(kid, ctr) || ciphertext`, with the
    /// **encoded header bytes bound as the AEAD AAD**.
    ///
    /// This is the cross-platform contract from
    /// [ADR-0013](../../../docs/adr/0013-sframe-cross-platform-aad.md):
    /// the AAD is the SFrame header (KID = sender leaf, CTR =
    /// this frame's counter) and nothing else — no host RTP
    /// `additional_data`. Because the header is computed from
    /// `(kid, ctr)` alone, a web (`RTCRtpScriptTransform`) and a
    /// native (`FrameEncryptorInterface`) client feed identical
    /// AAD into the same AEAD and interop byte-for-byte.
    ///
    /// The counter the header carries is the same one [`Self::seal`]
    /// is about to consume, so the AAD and the on-wire header match
    /// exactly. [`Self::open_framed`] is the inverse.
    ///
    /// # Errors
    ///
    /// - [`Error::CounterExhausted`] when the 64-bit counter stands at
    ///   its last value (checked before any state mutation; nothing
    ///   is sealed).
    /// - [`Error::Crypto`] on AEAD failure (practically unreachable).
    pub fn seal_framed(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        // Peek the counter `seal` will use (it pre-increments) so
        // the header we bind as AAD matches the one we ship. Guard
        // exhaustion here too, so we never build a header for a
        // counter `seal` will then refuse.
        if self.counter == MAX_FRAME_COUNTER {
            return Err(Error::CounterExhausted);
        }
        let next_ctr = self.counter + 1;
        let mut wire = SframeHeader {
            kid: self.kid,
            ctr: next_ctr,
        }
        .encode();
        let (frame_counter, ciphertext) = self.seal(&wire, plaintext)?;
        debug_assert_eq!(
            frame_counter, next_ctr,
            "seal consumed a different counter than the header bound"
        );
        wire.extend_from_slice(&ciphertext);
        Ok(wire)
    }

    /// Open a complete RFC 9605 wire frame produced by
    /// [`Self::seal_framed`] (or any conformant peer on the
    /// [ADR-0013](../../../docs/adr/0013-sframe-cross-platform-aad.md)
    /// contract): parse the header, bind the **exact on-wire header
    /// bytes** as the AEAD AAD, and AEAD-open the trailing
    /// ciphertext.
    ///
    /// The AAD is the literal header slice from the wire, NOT a
    /// re-encode of the parsed `(kid, ctr)` — [`SframeHeader::decode`]
    /// accepts non-minimal encodings, so re-encoding could differ
    /// from what the sender authenticated.
    ///
    /// This key must be the one derived for the frame's KID (the caller selects
    /// it by the header's KID before calling — ADR-0035); a KID that doesn't
    /// match this key's `kid` (its `(sender_leaf, stream)`) is rejected up front.
    ///
    /// # Errors
    ///
    /// - [`Error::Crypto`] on a malformed header, a KID that doesn't
    ///   match this key's `(leaf, stream)`, or an AEAD authentication failure
    ///   (wrong key / wrong AAD / tampered header or ciphertext / wrong
    ///   counter). Any 64-bit counter the header carries is accepted
    ///   (ADR-0069); this key's own counter is not consulted.
    pub fn open_framed(&self, wire: &[u8]) -> Result<Vec<u8>> {
        let (header, consumed) = SframeHeader::decode(wire)
            .map_err(|e| Error::Crypto(format!("sframe header decode: {e}")))?;
        if header.kid != self.kid {
            return Err(Error::Crypto(format!(
                "sframe header kid {} does not match participant kid {}",
                header.kid, self.kid
            )));
        }
        // AAD = the literal header bytes the sender authenticated.
        let aad = &wire[..consumed];
        let ciphertext = &wire[consumed..];
        self.open(aad, ciphertext, header.ctr)
    }

    /// Test-only — force the internal counter to a given value: the only
    /// deterministic start there is (ADR-0069). The exhaustion-edge tests use
    /// it to land near `u64::MAX`, and the tests that pin exact wire bytes use
    /// it to seal a known counter. Compiled out of the release crate via
    /// `cfg(test)`, so no production caller and no FFI surface can set or
    /// reset a counter (which could reuse a nonce under the same key — a
    /// critical AEAD violation).
    #[cfg(test)]
    pub(crate) fn set_counter_for_test(&mut self, c: u64) {
        self.counter = c;
    }

    /// Nonce composition (ADR-0069): `nonce = salt XOR (frame_counter as 8
    /// big-endian bytes in the low 8 bytes of the 12-byte nonce)`. The high 4
    /// bytes of the salt stay. Injective in the counter for a fixed salt, and
    /// for a counter below `2^32` equal to the earlier 32-bit composition
    /// (the four added bytes XOR in zero). Stable across implementations.
    fn compose_nonce(&self, frame_counter: u64) -> Nonce {
        const COUNTER_LEN: usize = 8;
        let mut nonce: Nonce = self.nonce_salt;
        for (byte, ctr) in nonce[NONCE_LEN - COUNTER_LEN..]
            .iter_mut()
            .zip(frame_counter.to_be_bytes())
        {
            *byte ^= ctr;
        }
        nonce
    }
}

// =============================================================================
// Diagnostics
// =============================================================================

/// Hash a [`CallKey`] for diagnostic display — never the raw
/// bytes themselves. Same hash both peers can compute, so
/// "matching SAS"-style verification is possible when needed.
#[must_use]
pub fn call_key_fingerprint(key: &CallKey) -> [u8; 32] {
    hash::blake3(key.as_bytes())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_mls::Identity as MlsIdentity;

    fn make_pair() -> (Identity, Identity, lakko_mls::Group) {
        // 1×1 MLS group: A creates, B joins via Welcome. Both
        // sides hold a live Group handle at the post-add
        // epoch.
        let id_a = MlsIdentity::generate("device-A").unwrap();
        let id_b = MlsIdentity::generate("device-B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let mut group_a = lakko_mls::Group::create(&id_a, lakko_mls::GroupKind::Group).unwrap();
        let addition = group_a.add_member(&id_a, kp_b).unwrap();
        let welcome_bytes = lakko_mls::group::encode_message(&addition.welcome).unwrap();
        let _group_b = lakko_mls::Group::join_from_welcome(&welcome_bytes, &id_b).unwrap();
        (id_a, id_b, group_a)
    }

    #[test]
    fn call_key_derives_32_bytes() {
        let (id_a, _id_b, group_a) = make_pair();
        let call_id = b"call/12345";
        let key = CallKey::derive(&group_a, &id_a, call_id).unwrap();
        assert_eq!(key.as_bytes().len(), CALL_KEY_LEN);
    }

    #[test]
    fn participant_key_round_trips() {
        let (id_a, _id_b, group_a) = make_pair();
        let call_id = b"call/abc";
        let key_a = CallKey::derive(&group_a, &id_a, call_id).unwrap();

        // A seals; B (re-deriving the same CallKey via its
        // own group view at the same epoch) opens. Here we
        // simulate by re-deriving from the same call key
        // bytes on both sides — the cross-device exporter-
        // agreement property is covered by lakko-mls's tests.
        let key_b = CallKey::from_seed(*key_a.as_bytes());

        let mut sender = key_a.participant_key_for(0, 0).unwrap();
        let receiver = key_b.participant_key_for(0, 0).unwrap();

        let aad = b"sframe-header-bytes";
        let pt = b"hello, p2p audio frame";
        let start = sender.counter();
        let (ctr, ct) = sender.seal(aad, pt).unwrap();
        assert_eq!(ctr, start + 1, "first sealed frame carries start + 1");

        let opened = receiver.open(aad, &ct, ctr).unwrap();
        assert_eq!(opened.as_slice(), pt.as_ref());
    }

    #[test]
    fn participant_keys_differ_across_leaves() {
        let (id_a, _id_b, group_a) = make_pair();
        let key = CallKey::derive(&group_a, &id_a, b"call/x").unwrap();
        let k0 = key.participant_key_for(0, 0).unwrap();
        let k1 = key.participant_key_for(1, 0).unwrap();
        assert_ne!(k0.key_bytes(), k1.key_bytes());
        assert_ne!(k0.nonce_salt(), k1.nonce_salt());
    }

    /// ADR-0035 security property (the real fix ADR-0034 could not deliver): a
    /// participant's THREE concurrent streams — audio (stream 0) plus TWO video
    /// streams (camera = 1, screen-share = 2) — all share ONE MLS leaf, yet each
    /// must derive a **distinct** `(key, nonce_salt)` space. Under the ADR-0034
    /// per-media-TYPE tag the two video streams both mapped to `Video` and
    /// re-collided the exact `(key, nonce)` reuse it set out to fix; keying on
    /// the FULL KID `(leaf << 8) | stream_index` closes it for ANY number of
    /// concurrent streams per leaf.
    #[test]
    fn three_streams_of_one_leaf_get_distinct_keys_and_nonces() {
        let leaf = 4;
        let key = CallKey::from_seed([0x5A; CALL_KEY_LEN]);
        let pk_audio = key.participant_key_for(leaf, 0).unwrap(); // audio
        let pk_cam = key.participant_key_for(leaf, 1).unwrap(); // camera video
        let pk_screen = key.participant_key_for(leaf, 2).unwrap(); // screen-share video

        // (a) all three AEAD keys are pairwise distinct, (b) all three nonce
        // salts are pairwise distinct — every stream keys into its own space
        // despite sharing the leaf. The audio-vs-video pair held under ADR-0034;
        // the camera-vs-screen pair (both video) is the pair it MISSED.
        assert_ne!(pk_audio.key_bytes(), pk_cam.key_bytes());
        assert_ne!(pk_audio.key_bytes(), pk_screen.key_bytes());
        assert_ne!(
            pk_cam.key_bytes(),
            pk_screen.key_bytes(),
            "two video streams from one leaf must not share an AEAD key (the ADR-0034 gap)"
        );
        assert_ne!(pk_audio.nonce_salt(), pk_cam.nonce_salt());
        assert_ne!(pk_audio.nonce_salt(), pk_screen.nonce_salt());
        assert_ne!(
            pk_cam.nonce_salt(),
            pk_screen.nonce_salt(),
            "two video streams from one leaf must not share a nonce salt (the ADR-0034 gap)"
        );

        // (c) the whole point: sealing the SAME plaintext at the SAME counter
        // (each key forced to counter 0, so all three seal counter 1) yields
        // THREE pairwise-different ciphertexts, proving no two streams share a
        // `(key, nonce)` pair.
        let mut audio = key.participant_key_for(leaf, 0).unwrap();
        let mut cam = key.participant_key_for(leaf, 1).unwrap();
        let mut screen = key.participant_key_for(leaf, 2).unwrap();
        audio.set_counter_for_test(0);
        cam.set_counter_for_test(0);
        screen.set_counter_for_test(0);
        let aad = b"same-header";
        let pt = b"identical media plaintext";
        let (audio_counter, audio_cipher) = audio.seal(aad, pt).unwrap();
        let (cam_counter, cam_cipher) = cam.seal(aad, pt).unwrap();
        let (screen_counter, screen_cipher) = screen.seal(aad, pt).unwrap();
        assert_eq!(audio_counter, 1, "first audio frame is counter 1");
        assert_eq!(cam_counter, 1, "first camera frame is counter 1");
        assert_eq!(screen_counter, 1, "first screen frame is counter 1");
        assert_ne!(audio_cipher, cam_cipher);
        assert_ne!(audio_cipher, screen_cipher);
        assert_ne!(
            cam_cipher, screen_cipher,
            "same plaintext at the same counter must not seal to the same \
             ciphertext across two video streams (no (key, nonce) reuse)"
        );

        // Regression: the pre-existing per-leaf separation still holds WITHIN a
        // single stream index — stream-0-vs-stream-0 across two leaves differs.
        let a_leaf4 = key.participant_key_for(4, 0).unwrap();
        let a_leaf7 = key.participant_key_for(7, 0).unwrap();
        assert_ne!(a_leaf4.key_bytes(), a_leaf7.key_bytes());
        assert_ne!(a_leaf4.nonce_salt(), a_leaf7.nonce_salt());
        let v_leaf4 = key.participant_key_for(4, 1).unwrap();
        let v_leaf7 = key.participant_key_for(7, 1).unwrap();
        assert_ne!(v_leaf4.key_bytes(), v_leaf7.key_bytes());
        assert_ne!(v_leaf4.nonce_salt(), v_leaf7.nonce_salt());
    }

    /// The `(leaf, stream)` KID mapping is injective the way the wire needs it:
    /// a `ParticipantKey` reports back the leaf + stream it was derived for, and
    /// two different `(leaf, stream)` pairs that happen to be numerically
    /// adjacent (leaf 1/stream 0 = kid 256 vs leaf 0/stream 255 = kid 255) do
    /// NOT alias. Guards against a future off-by-one in the bit packing.
    #[test]
    fn participant_key_reports_its_leaf_and_stream() {
        let key = CallKey::from_seed([0x5A; CALL_KEY_LEN]);
        let pk = key.participant_key_for(4, 2).unwrap();
        assert_eq!(pk.sender_leaf(), 4);
        assert_eq!(pk.stream_index(), 2);
        assert_eq!(pk.kid(), SframeHeader::kid_for(4, 2).unwrap());

        // Adjacent-but-distinct KIDs do not alias.
        let boundary_a = key.participant_key_for(1, 0).unwrap(); // kid 256
        let boundary_b = key.participant_key_for(0, 255).unwrap(); // kid 255
        assert_ne!(boundary_a.kid(), boundary_b.kid());
        assert_ne!(boundary_a.key_bytes(), boundary_b.key_bytes());
    }

    /// Why the counter start is random (ADR-0069). Two keys derived from the
    /// SAME seed and the SAME KID hold the identical AEAD key and nonce salt.
    /// Forced to the SAME counter (test-only), sealing on each reuses the
    /// identical `(key, nonce)`: the same plaintext seals to the same
    /// ciphertext, which is the keystream reuse a fixed start produced on
    /// every rejoin. Nothing outside tests can put two keys on one counter.
    #[test]
    fn two_keys_of_one_seed_and_kid_forced_to_one_counter_reuse_the_nonce() {
        let seed = [0x77; CALL_KEY_LEN];
        let key = CallKey::from_seed(seed);
        let kid = SframeHeader::kid_for(2, 0).unwrap();

        let mut first = key.participant_key(kid).unwrap();
        let mut second = key.participant_key(kid).unwrap();
        assert_eq!(first.key_bytes(), second.key_bytes());
        assert_eq!(first.nonce_salt(), second.nonce_salt());

        first.set_counter_for_test(41);
        second.set_counter_for_test(41);
        let aad = b"same-header";
        let pt = b"same media plaintext";
        let (c1, ct1) = first.seal(aad, pt).unwrap();
        let (c2, ct2) = second.seal(aad, pt).unwrap();
        assert_eq!(c1, 42);
        assert_eq!(c2, 42);
        assert_eq!(
            ct1, ct2,
            "two keys off one (seed, kid) at one counter reuse (key, nonce)"
        );

        // Contrast: a DIFFERENT seed at the same kid and counter is a disjoint
        // key space.
        let other = CallKey::from_seed([0x88; CALL_KEY_LEN]);
        let mut safe = other.participant_key(kid).unwrap();
        safe.set_counter_for_test(41);
        let (_c, ct_safe) = safe.seal(aad, pt).unwrap();
        assert_ne!(ct1, ct_safe);
    }

    // -----------------------------------------------------
    // ADR-0069 — 64-bit counter, random start.
    // -----------------------------------------------------

    /// The 32-bit nonce composition this crate used before ADR-0069, kept
    /// here only to compare against.
    fn legacy_nonce(salt: &[u8; NONCE_LEN], frame_counter: u32) -> Nonce {
        let mut nonce: Nonce = *salt;
        let ctr = frame_counter.to_be_bytes();
        for i in 0..4 {
            nonce[NONCE_LEN - 4 + i] ^= ctr[i];
        }
        nonce
    }

    /// For every counter below `2^32` the 64-bit composition is the nonce the
    /// 32-bit code produced, so frames sealed before ADR-0069 still open.
    ///
    /// Both compositions XOR the salt bytewise with the counter's big-endian
    /// bytes and have no carry between positions, so they agree on all of
    /// `[0, 2^32)` exactly when (1) the four added high bytes XOR in zero and
    /// (2) each of the low four positions agrees for each of its 256 byte
    /// values. The test checks both exhaustively (4 x 256 single-byte
    /// counters), then the edges, then a strided sweep of the whole range
    /// (step 65_521, about 65 thousand values) as a check on that argument.
    #[test]
    fn nonce_below_2_pow_32_equals_the_32_bit_composition() {
        let pk = CallKey::from_seed([0x42; CALL_KEY_LEN])
            .participant_key_for(3, 0)
            .unwrap();
        let salt = *pk.nonce_salt();
        let check = |c: u32| {
            assert_eq!(
                pk.compose_nonce(u64::from(c)),
                legacy_nonce(&salt, c),
                "counter {c:#x}"
            );
        };
        for position in 0..4u32 {
            for byte in 0..=255u32 {
                check(byte << (8 * position));
            }
        }
        for c in [0, 1, 0xFFFF, 0x0001_0000, u32::MAX - 1, u32::MAX] {
            check(c);
        }
        let mut c: u32 = 0;
        loop {
            check(c);
            match c.checked_add(65_521) {
                Some(next) => c = next,
                None => break,
            }
        }
        // One past the 32-bit range: the high bytes now carry the counter.
        let over = pk.compose_nonce(1u64 << 32);
        assert_ne!(over, pk.compose_nonce(0));
        assert_eq!(over[..4], salt[..4], "the high 4 nonce bytes stay the salt");
    }

    /// (a) The nonce is injective in the counter: `nonce XOR salt` gives the
    /// counter back (its 8 big-endian bytes in the low 8 nonce bytes, the high
    /// 4 untouched), so two different counters cannot share a nonce. Checked
    /// on every single-bit counter, the edges, and random counters; flipping
    /// any one counter bit changes the nonce.
    #[test]
    fn nonce_is_injective_in_the_counter() {
        let pk = CallKey::from_seed([0x5A; CALL_KEY_LEN])
            .participant_key_for(9, 1)
            .unwrap();
        let salt = *pk.nonce_salt();
        let recover = |nonce: &Nonce| -> u64 {
            assert_eq!(nonce[..4], salt[..4], "high 4 bytes are the salt");
            let mut ctr = [0u8; 8];
            for (i, byte) in ctr.iter_mut().enumerate() {
                *byte = nonce[4 + i] ^ salt[4 + i];
            }
            u64::from_be_bytes(ctr)
        };
        let mut counters: Vec<u64> = vec![0, 1, u64::from(u32::MAX), 1 << 32, u64::MAX];
        counters.extend((0..64).map(|bit| 1u64 << bit));
        for _ in 0..4096 {
            let mut bytes = [0u8; 8];
            lakko_crypto::random::fill(&mut bytes);
            counters.push(u64::from_be_bytes(bytes));
        }
        for &c in &counters {
            let nonce = pk.compose_nonce(c);
            assert_eq!(recover(&nonce), c, "the counter is recoverable");
            for bit in 0..64 {
                assert_ne!(nonce, pk.compose_nonce(c ^ (1u64 << bit)));
            }
        }
    }

    /// A fresh key's counter start is below `2^62`, and starts differ.
    #[test]
    fn fresh_keys_start_below_2_pow_62() {
        let key = CallKey::from_seed([0x31; CALL_KEY_LEN]);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..2048 {
            let start = key.participant_key_for(1, 0).unwrap().counter();
            assert!(start < COUNTER_START_BOUND);
            seen.insert(start);
        }
        // 2048 draws from 2^62 values collide with probability about 2^-41.
        assert_eq!(seen.len(), 2048, "random starts repeated");
    }

    /// (b) Two keys derived from one `(seed, kid)` start at different counters
    /// and, sealing 4096 frames each, never seal the same counter — so they
    /// never share a `(key, nonce)` pair although they share the key.
    ///
    /// This is probabilistic: the two ranges overlap with probability at most
    /// `(4096 + 4096) / 2^62 = 2^-49`. A failure here therefore means the
    /// counter start is not coming from a working CSPRNG, not bad luck.
    #[test]
    fn two_keys_of_one_seed_and_kid_never_seal_the_same_counter() {
        const FRAMES: usize = 4096;
        let key = CallKey::from_seed([0x77; CALL_KEY_LEN]);
        let kid = SframeHeader::kid_for(2, 0).unwrap();
        let mut first = key.participant_key(kid).unwrap();
        let mut second = key.participant_key(kid).unwrap();
        assert_eq!(first.key_bytes(), second.key_bytes());
        assert_eq!(first.nonce_salt(), second.nonce_salt());
        assert_ne!(first.counter(), second.counter(), "starts must differ");

        let mut counters = std::collections::BTreeSet::new();
        let mut nonces = std::collections::BTreeSet::new();
        for pk in [&mut first, &mut second] {
            for _ in 0..FRAMES {
                let (ctr, _ct) = pk.seal(b"aad", b"frame").unwrap();
                assert!(counters.insert(ctr), "counter {ctr} sealed twice");
                assert!(nonces.insert(pk.compose_nonce(ctr)), "nonce reused");
            }
        }
        assert_eq!(counters.len(), 2 * FRAMES);
    }

    /// (c) Receivers hold no counter: a frame sealed by one key opens under a
    /// second key derived independently from the same `(seed, kid)`, whose own
    /// counter is some other random value, and opening does not move it.
    #[test]
    fn a_frame_opens_under_an_independently_derived_key() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(5, 1).unwrap();
        let receiver = CallKey::from_seed([0x13; CALL_KEY_LEN])
            .participant_key_for(5, 1)
            .unwrap();
        assert_ne!(sender.counter(), receiver.counter());
        let before = receiver.counter();
        for i in 0..8u8 {
            let wire = sender.seal_framed(&[i; 32]).unwrap();
            assert_eq!(receiver.open_framed(&wire).unwrap(), vec![i; 32]);
        }
        let (ctr, ct) = sender.seal(b"aad", b"raw").unwrap();
        assert_eq!(receiver.open(b"aad", &ct, ctr).unwrap(), b"raw");
        assert_eq!(receiver.counter(), before, "opening keeps no state");
    }

    /// (f) A header with an 8-byte counter round-trips: a frame sealed at a
    /// counter above `2^56` carries a header whose CTR field is 8 bytes, the
    /// header decodes to the same counter, and the frame opens. Also at
    /// `2^32`, the first counter the 32-bit code refused.
    #[test]
    fn a_frame_with_an_8_byte_counter_round_trips() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let receiver = key.participant_key_for(5, 0).unwrap();
        for start in [
            (1u64 << 32) - 1,
            1u64 << 56,
            COUNTER_START_BOUND - 1,
            u64::MAX - 1,
        ] {
            let mut sender = key.participant_key_for(5, 0).unwrap();
            sender.set_counter_for_test(start);
            let wire = sender.seal_framed(b"wide counter").unwrap();
            let (header, consumed) = SframeHeader::decode(&wire).unwrap();
            assert_eq!(header.ctr, start + 1);
            assert_eq!(header.kid, SframeHeader::kid_for(5, 0).unwrap());
            if start >= 1u64 << 56 {
                // config byte + 2 KID bytes (0x0500) + 8 CTR bytes.
                assert_eq!(consumed, 1 + 2 + 8);
                assert_eq!(wire[0] & 0x0F, 0x0F, "Y=1, CCC=7: an 8-byte CTR");
            }
            assert_eq!(
                SframeHeader::decode(&header.encode()).unwrap(),
                (header, consumed)
            );
            assert_eq!(receiver.open_framed(&wire).unwrap(), b"wide counter");
        }
    }

    #[test]
    fn counter_increments_strictly() {
        let key = CallKey::from_seed([0x42; CALL_KEY_LEN]);
        let mut p = key.participant_key_for(0, 0).unwrap();
        let start = p.counter();
        let (c1, _) = p.seal(b"aad", b"f1").unwrap();
        let (c2, _) = p.seal(b"aad", b"f2").unwrap();
        let (c3, _) = p.seal(b"aad", b"f3").unwrap();
        assert_eq!(c1, start + 1);
        assert_eq!(c2, start + 2);
        assert_eq!(c3, start + 3);
        assert_eq!(p.counter(), start + 3);
    }

    #[test]
    fn wrong_counter_rejects_open() {
        let key = CallKey::from_seed([0x42; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(0, 0).unwrap();
        let receiver = key.participant_key_for(0, 0).unwrap();
        let (ctr, ct) = sender.seal(b"aad", b"frame").unwrap();
        // Bump the counter by one → wrong nonce → AEAD
        // rejects.
        match receiver.open(b"aad", &ct, ctr + 1) {
            Ok(_) => panic!("wrong counter must fail authentication"),
            Err(Error::Crypto(_)) => {},
            Err(other) => panic!("expected Crypto error, got {other:?}"),
        }
    }

    #[test]
    fn wrong_aad_rejects_open() {
        let key = CallKey::from_seed([0x42; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(0, 0).unwrap();
        let receiver = key.participant_key_for(0, 0).unwrap();
        let (ctr, ct) = sender.seal(b"aad-A", b"frame").unwrap();
        match receiver.open(b"aad-B", &ct, ctr) {
            Ok(_) => panic!("wrong AAD must fail authentication"),
            Err(Error::Crypto(_)) => {},
            Err(other) => panic!("expected Crypto error, got {other:?}"),
        }
    }

    #[test]
    fn fingerprint_is_stable() {
        let k1 = CallKey::from_seed([0x42; CALL_KEY_LEN]);
        let k2 = CallKey::from_seed([0x42; CALL_KEY_LEN]);
        assert_eq!(call_key_fingerprint(&k1), call_key_fingerprint(&k2));
    }

    // -----------------------------------------------------
    // G-17 — frame-counter exhaustion edge.
    //
    // The counter is 64 bits (ADR-0069). A key whose counter
    // stands at u64::MAX MUST refuse to seal another frame (a
    // wrap would reuse a nonce), and must seal nothing. From
    // the highest random start (2^62 - 1) that is 3 * 2^62
    // frames away, so the edge is reached only through the
    // test-only counter accessor.
    // -----------------------------------------------------

    #[test]
    fn counter_exhaustion_returns_typed_error() {
        let key = CallKey::from_seed([0xCC; CALL_KEY_LEN]);
        let mut p = key.participant_key_for(0, 0).unwrap();
        // (d) Land at u64::MAX - 2 — two seals must still
        // succeed (counter increments to MAX - 1 then MAX),
        // the third must refuse and seal nothing.
        p.set_counter_for_test(u64::MAX - 2);
        let (c1, _) = p.seal(b"aad", b"frame-A").expect("seal at MAX-2");
        let (c2, _) = p.seal(b"aad", b"frame-B").expect("seal at MAX-1");
        assert_eq!(c1, u64::MAX - 1);
        assert_eq!(c2, u64::MAX);
        // Counter is now at u64::MAX. Third attempt MUST
        // refuse with the typed variant — no panic, no
        // silent wrap, no nonce reuse.
        match p.seal(b"aad", b"frame-C") {
            Err(Error::CounterExhausted) => {},
            Ok((ctr, _)) => panic!("must refuse to wrap, got counter={ctr}"),
            Err(other) => panic!("expected CounterExhausted, got {other:?}"),
        }
        // Counter accessor reflects the saturated state so
        // a UI / metrics surface can see the exhaustion.
        assert_eq!(p.counter(), u64::MAX);
    }

    #[test]
    fn counter_max_frame_still_opens_on_receiver() {
        // The last legal seal lands at frame_counter =
        // u64::MAX. A receiver re-deriving the same
        // ParticipantKey at the same epoch MUST authenticate
        // it — no off-by-one at the nonce-composition
        // boundary.
        let key = CallKey::from_seed([0xCD; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(0, 0).unwrap();
        let receiver = key.participant_key_for(0, 0).unwrap();
        sender.set_counter_for_test(u64::MAX - 1);
        let (ctr, ct) = sender
            .seal(b"aad-edge", b"last-frame")
            .expect("seal at MAX-1");
        assert_eq!(ctr, u64::MAX);
        let opened = receiver
            .open(b"aad-edge", &ct, ctr)
            .expect("open at MAX must authenticate");
        assert_eq!(opened.as_slice(), b"last-frame");
    }

    #[test]
    fn counter_exhaustion_is_idempotent() {
        // After the first CounterExhausted, the producer
        // must keep refusing on every subsequent call —
        // never silently wrap around to 0 (which would
        // re-use the salt-XOR-0 nonce already consumed by
        // the very first seal in this key's lifetime).
        let key = CallKey::from_seed([0xCE; CALL_KEY_LEN]);
        let mut p = key.participant_key_for(0, 0).unwrap();
        p.set_counter_for_test(u64::MAX);
        for attempt in 0..16 {
            match p.seal(b"aad", b"junk") {
                Err(Error::CounterExhausted) => {},
                Ok(_) => panic!("attempt {attempt} unexpectedly succeeded"),
                Err(other) => panic!("attempt {attempt}: expected CounterExhausted, got {other:?}"),
            }
            assert_eq!(p.counter(), u64::MAX, "counter must not wrap");
        }
    }

    // -- ADR-0013: seal_framed / open_framed (header-as-AAD) -------------

    /// G-26 cross-stack interop golden vector. The SAME `(seed, leaf,
    /// plaintext, counter=0)` MUST seal to these EXACT bytes on every stack
    /// that consumes the `lakko_calls::sframe` core — the Rust core (this
    /// test), the wasm/web `RTCRtpScriptTransform` worker
    /// (`apps/web/src/calls/sframe-golden.test.ts`), and the native iOS /
    /// Android FFI (`LakkoTests/SFrameGoldenTests.swift`,
    /// `SframeGoldenTest.kt`). All four pin THIS hex, so a frame sealed on
    /// any stack opens on any other (the deferred-no-longer cross-runtime
    /// half of G-26 — proven by construction without a live SFU, since the
    /// SFU forwards the wire verbatim, which the live str0m e2e already
    /// shows). A drift on any platform fails its test loudly.
    ///
    /// The golden is the stream-0 vector for leaf 3 (ADR-0035 keys the
    /// derivation on the FULL KID = `(leaf << 8) | stream_index`, so the KID is
    /// `0x0300` and the header carries the extended 2-byte KID `0x03 0x00`). Any
    /// other stream index differs.
    ///
    /// Since ADR-0069 a fresh key starts at a random counter, so only this
    /// crate (through the test-only counter accessor) still SEALS the golden;
    /// every other stack proves its seal by opening what it sealed and proves
    /// the wire by OPENING this vector, a frame at counter 1.
    ///
    /// To regenerate after an intentional wire change: seal `(seed=[0x42;32],
    /// leaf=3, stream_index=0, plaintext=b"lakko sframe golden vector v1")` on a
    /// `ParticipantKey` forced to counter 0 and update the hex on all five stacks.
    const GOLDEN_SEED: [u8; CALL_KEY_LEN] = [0x42; CALL_KEY_LEN];
    const GOLDEN_LEAF: u32 = 3;
    const GOLDEN_STREAM: u32 = 0;
    const GOLDEN_PLAINTEXT: &[u8] = b"lakko sframe golden vector v1";
    const GOLDEN_WIRE_HEX: &str =
        "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a";

    fn golden_wire() -> Vec<u8> {
        (0..GOLDEN_WIRE_HEX.len() / 2)
            .map(|i| u8::from_str_radix(&GOLDEN_WIRE_HEX[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn golden_vector_seals_to_the_canonical_cross_stack_wire() {
        let mut sender = CallKey::from_seed(GOLDEN_SEED)
            // ADR-0035: keyed on the full KID (leaf 3, stream 0 → KID 0x0300).
            .participant_key_for(GOLDEN_LEAF, GOLDEN_STREAM)
            .unwrap();
        // The golden is the frame at counter 1: force the start to 0.
        sender.set_counter_for_test(0);
        let wire = sender.seal_framed(GOLDEN_PLAINTEXT).expect("seal_framed");
        assert_eq!(
            wire,
            golden_wire(),
            "Rust core sealed a different wire than the cross-stack golden — \
             either a wire-format regression or an intentional change that \
             must be mirrored on web + iOS + Android + native"
        );
    }

    #[test]
    fn golden_vector_opens_to_the_plaintext() {
        let receiver = CallKey::from_seed(GOLDEN_SEED)
            // ADR-0035: keyed on the full KID (leaf 3, stream 0 → KID 0x0300).
            .participant_key_for(GOLDEN_LEAF, GOLDEN_STREAM)
            .unwrap();
        // (e) The receiver's own counter is random; the golden still opens.
        let opened = receiver.open_framed(&golden_wire()).expect("open_framed");
        assert_eq!(opened.as_slice(), GOLDEN_PLAINTEXT);
        let (header, _) = SframeHeader::decode(&golden_wire()).unwrap();
        assert_eq!(header.kid, 0x0300);
        assert_eq!(header.ctr, 1);
    }

    #[test]
    fn seal_framed_round_trips_through_open_framed() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(5, 0).unwrap();
        // Receiver re-derives the SENDER's key by the same leaf —
        // exactly the cross-participant open path.
        let receiver = key.participant_key_for(5, 0).unwrap();
        let pt = b"e2ee media frame";
        let wire = sender.seal_framed(pt).expect("seal_framed");
        let opened = receiver.open_framed(&wire).expect("open_framed");
        assert_eq!(opened.as_slice(), pt);
    }

    #[test]
    fn seal_framed_wire_starts_with_the_canonical_header() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        // leaf 5, stream 0 → composite KID 5<<8 = 0x0500 (ADR-0035).
        let expected_kid = SframeHeader::kid_for(5, 0).unwrap();
        let mut sender = key.participant_key_for(5, 0).unwrap();
        sender.set_counter_for_test(0);
        let wire = sender.seal_framed(b"x").expect("seal_framed");
        // Counter forced to 0 → the frame carries counter 1; header decodes
        // to (kid=0x0500, ctr=1) and is the minimal canonical encoding.
        let (header, consumed) = SframeHeader::decode(&wire).expect("decode");
        assert_eq!(header.kid, expected_kid);
        assert_eq!(SframeHeader::leaf_of(header.kid), 5);
        assert_eq!(SframeHeader::stream_of(header.kid), 0);
        assert_eq!(header.ctr, 1);
        assert_eq!(
            &wire[..consumed],
            SframeHeader {
                kid: expected_kid,
                ctr: 1
            }
            .encode()
            .as_slice()
        );
    }

    #[test]
    fn seal_framed_counters_advance_monotonically() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(0, 0).unwrap();
        let receiver = key.participant_key_for(0, 0).unwrap();
        let start = sender.counter();
        for expected_ctr in start + 1..=start + 4 {
            let wire = sender.seal_framed(b"frame").expect("seal_framed");
            let (header, _) = SframeHeader::decode(&wire).expect("decode");
            assert_eq!(header.ctr, expected_ctr);
            // A fresh receiver opens any frame by its header counter.
            assert_eq!(
                receiver.open_framed(&wire).expect("open").as_slice(),
                b"frame"
            );
        }
    }

    #[test]
    fn open_framed_rejects_wrong_leaf_key() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(5, 0).unwrap();
        let wire = sender.seal_framed(b"frame").expect("seal_framed");
        // A key derived for a DIFFERENT leaf must refuse — the kid
        // guard fires before any AEAD work.
        let wrong = key.participant_key_for(6, 0).unwrap();
        match wrong.open_framed(&wire) {
            Err(Error::Crypto(msg)) => assert!(msg.contains("kid"), "got {msg}"),
            other => panic!("expected kid-mismatch Crypto error, got {other:?}"),
        }
    }

    #[test]
    fn open_framed_rejects_tampered_header() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(0, 0).unwrap();
        let receiver = key.participant_key_for(0, 0).unwrap();
        let mut wire = sender.seal_framed(b"frame").expect("seal_framed");
        // Change the first counter byte in the header (kid=0 is inline and
        // the random counter is above 7, so the header is the config byte
        // followed by the CTR bytes). The AEAD AAD no longer matches what
        // was sealed AND the nonce changes → open fails.
        let (header, _) = SframeHeader::decode(&wire).unwrap();
        assert!(header.ctr > 7, "an extended CTR field follows the config");
        wire[1] = wire[1].wrapping_add(1);
        assert!(receiver.open_framed(&wire).is_err());
    }

    #[test]
    fn open_framed_rejects_tampered_ciphertext() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(0, 0).unwrap();
        let receiver = key.participant_key_for(0, 0).unwrap();
        let mut wire = sender.seal_framed(b"frame").expect("seal_framed");
        // Flip the last ciphertext byte (inside the AEAD tag region).
        let last = wire.len() - 1;
        wire[last] ^= 0x01;
        assert!(receiver.open_framed(&wire).is_err());
    }

    #[test]
    fn open_framed_rejects_truncated_wire() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let receiver = key.participant_key_for(0, 0).unwrap();
        // Config 0x0B (RFC 9605 §4.3: X=0 K=0 Y=1 C=3) declares a 4-byte CTR
        // with no bytes present → the header decode fails as truncated.
        match receiver.open_framed(&[0x0B]) {
            Err(Error::Crypto(msg)) => assert!(msg.contains("header decode"), "got {msg}"),
            other => panic!("expected header-decode Crypto error, got {other:?}"),
        }
    }

    #[test]
    fn seal_framed_honours_counter_exhaustion() {
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let mut p = key.participant_key_for(0, 0).unwrap();
        p.set_counter_for_test(u64::MAX);
        match p.seal_framed(b"junk") {
            Err(Error::CounterExhausted) => {},
            other => panic!("expected CounterExhausted, got {other:?}"),
        }
        assert_eq!(
            p.counter(),
            u64::MAX,
            "counter must not advance on exhaustion"
        );
    }

    #[test]
    fn open_framed_rejects_a_header_spliced_onto_another_frames_body() {
        // Audit-pack C-7, at the framed layer: the counter is bound into BOTH
        // the AAD (the on-wire header bytes) and the nonce, so a valid header
        // from one frame cannot be reused on a DIFFERENT frame's ciphertext
        // body — the receiver rejects the splice. Complements
        // `wrong_counter_rejects_open` (the raw counter-vs-nonce binding) and
        // `open_framed_rejects_tampered_header` (a single mutated byte).
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(0, 0).unwrap();
        let receiver = key.participant_key_for(0, 0).unwrap();

        let wire1 = sender.seal_framed(b"frame body A").expect("seal_framed A");
        let wire2 = sender.seal_framed(b"frame body B").expect("seal_framed B");
        let (_h1, consumed1) = SframeHeader::decode(&wire1).expect("decode A");
        let (_h2, consumed2) = SframeHeader::decode(&wire2).expect("decode B");
        // B's header (the next counter) onto A's ciphertext body.
        let mut spliced = wire2[..consumed2].to_vec();
        spliced.extend_from_slice(&wire1[consumed1..]);
        assert!(
            receiver.open_framed(&spliced).is_err(),
            "a header spliced onto a different frame's body must not open"
        );
        // Sanity: each intact frame still opens on its own.
        assert_eq!(
            receiver.open_framed(&wire1).expect("A opens"),
            b"frame body A"
        );
        assert_eq!(
            receiver.open_framed(&wire2).expect("B opens"),
            b"frame body B"
        );
    }

    #[test]
    fn open_framed_is_stateless_so_replay_is_an_app_layer_concern() {
        // The `ParticipantKey` open path is deliberately STATELESS: it derives
        // the nonce from the header counter and AEAD-opens, without tracking a
        // receive high-water. So the SAME sealed wire opens repeatedly — the
        // sframe layer does NOT provide replay protection. Dropping a replayed
        // or out-of-order counter is an APP-LAYER responsibility (the receiver
        // tracks the monotone header counter, like the SFU's per-sender state).
        // This test PINS that boundary: if a future change makes open stateful,
        // it breaks here and forces a conscious decision.
        let key = CallKey::from_seed([0x13; CALL_KEY_LEN]);
        let mut sender = key.participant_key_for(0, 0).unwrap();
        let receiver = key.participant_key_for(0, 0).unwrap();
        let wire = sender.seal_framed(b"replayed frame").expect("seal_framed");
        assert_eq!(
            receiver.open_framed(&wire).expect("first open"),
            b"replayed frame"
        );
        assert_eq!(
            receiver
                .open_framed(&wire)
                .expect("replayed open still succeeds"),
            b"replayed frame",
            "stateless open opens a replayed wire again — replay guard is app-layer"
        );
    }

    #[test]
    fn frame_sealed_under_one_call_key_does_not_open_under_another() {
        // Cross-call (and cross-epoch) crypto isolation: two DIFFERENT per-call
        // seeds derive different ParticipantKeys for the SAME leaf, so a frame
        // sealed on call A cannot be opened by a receiver holding call B's key —
        // even at the same leaf + counter. A different seed models either a
        // different call OR a different MLS epoch of one call (each re-export
        // yields a fresh CallKey). This is the crypto backstop behind the SFU's
        // routing-level cross-call isolation: a mis-routed frame is
        // undecryptable, not merely undelivered (invariant #4).
        let leaf = 3;
        let mut sender_a = CallKey::from_seed([0xA1; CALL_KEY_LEN])
            .participant_key_for(leaf, 0)
            .unwrap();
        let receiver_b = CallKey::from_seed([0xB2; CALL_KEY_LEN])
            .participant_key_for(leaf, 0)
            .unwrap();

        let wire = sender_a.seal_framed(b"call A media").expect("seal_framed");
        // The wire is well-formed — the header parses and carries A's composite
        // KID (leaf 3, stream 0) — so the rejection is the AEAD key, not a
        // malformed frame.
        let (header, _) = SframeHeader::decode(&wire).expect("header parses");
        assert_eq!(header.kid, SframeHeader::kid_for(leaf, 0).unwrap());
        assert_eq!(SframeHeader::leaf_of(header.kid), leaf);
        assert!(
            receiver_b.open_framed(&wire).is_err(),
            "a frame sealed under call A must not open under call B's key"
        );

        // Sanity: call A's own receiver DOES open it — the failure above is the
        // seed, not the frame.
        let receiver_a = CallKey::from_seed([0xA1; CALL_KEY_LEN])
            .participant_key_for(leaf, 0)
            .unwrap();
        assert_eq!(
            receiver_a
                .open_framed(&wire)
                .expect("A's key opens A's frame"),
            b"call A media"
        );
    }
}
