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
//!         │  HKDF-SHA-256 over (sender_leaf_index | "lakko/sframe/v1/key")
//!         ▼
//!   ParticipantKey
//!         │
//!         │  per-frame: nonce = SHA-256(seed | "nonce" | sender_leaf | frame_counter)[:12]
//!         ▼  (counter increments 1-per-sealed-frame, monotonic per epoch)
//!   AEAD seal / open
//! ```
//!
//! The per-participant split lets the SFU forward each
//! participant's stream without ever decrypting; the
//! receiver re-derives the sender's [`ParticipantKey`] via
//! the same leaf index that's carried in the SFrame header.
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

/// 32-bit per-participant frame counter limit. RFC 9605 §4.4
/// pins the counter to 32 bits; the producer rolls over via
/// an MLS commit (fresh epoch → fresh CallKey → fresh
/// ParticipantKey → counter resets to 0).
const MAX_FRAME_COUNTER: u32 = u32::MAX;

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

    /// Derive a per-participant key for `sender_leaf`. Each
    /// participant on the call holds their own
    /// [`ParticipantKey`] for sealing and re-derives every
    /// peer's [`ParticipantKey`] for opening.
    ///
    /// HKDF-SHA-256 over `(seed, info = "lakko/sframe/v1/key"
    /// || sender_leaf_be)` — receivers reach the same value
    /// because they see the same `sender_leaf` in the SFrame
    /// header.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Crypto`] when HKDF rejects the input
    /// length (practically unreachable with valid inputs).
    pub fn participant_key(&self, sender_leaf: u32) -> Result<ParticipantKey> {
        let mut info = Vec::with_capacity(PARTICIPANT_KEY_INFO.len() + 4);
        info.extend_from_slice(PARTICIPANT_KEY_INFO);
        info.extend_from_slice(&sender_leaf.to_be_bytes());
        let key_secret = kdf::hkdf_sha256::<CALL_KEY_LEN>(self.seed.as_bytes(), b"", &info)
            .map_err(|e| Error::Crypto(format!("hkdf: {e:?}")))?;
        // Also derive the per-participant nonce salt so the
        // 12-byte AEAD nonce is uniformly random over the
        // counter space, not just the counter byte itself.
        let mut nonce_info = Vec::with_capacity(FRAME_NONCE_INFO.len() + 4);
        nonce_info.extend_from_slice(FRAME_NONCE_INFO);
        nonce_info.extend_from_slice(&sender_leaf.to_be_bytes());
        let nonce_secret = kdf::hkdf_sha256::<32>(self.seed.as_bytes(), b"", &nonce_info)
            .map_err(|e| Error::Crypto(format!("hkdf: {e:?}")))?;
        let mut nonce_salt = [0u8; NONCE_LEN];
        nonce_salt.copy_from_slice(&nonce_secret.as_bytes()[..NONCE_LEN]);
        Ok(ParticipantKey {
            sender_leaf,
            key: SecretBytes::new(*key_secret.as_bytes()),
            nonce_salt,
            counter: 0,
        })
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
    sender_leaf: u32,
    key: SecretBytes<CALL_KEY_LEN>,
    /// 12-byte nonce salt derived from the call key. Per-
    /// frame nonce is `salt XOR (counter as u32 BE in the
    /// low 4 bytes)` — same construction as RFC 8439 §2.4 +
    /// RFC 9605 §4.4.
    nonce_salt: [u8; NONCE_LEN],
    counter: u32,
}

impl ParticipantKey {
    /// Sender leaf index this key derives from.
    #[must_use]
    pub fn sender_leaf(&self) -> u32 {
        self.sender_leaf
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

    /// Current counter value — exposed for diagnostics. The
    /// canonical increment happens inside [`Self::seal`].
    #[must_use]
    pub fn counter(&self) -> u32 {
        self.counter
    }

    /// Seal `plaintext` under the next counter value. Returns
    /// `(frame_counter, ciphertext_with_tag)`. The caller
    /// embeds `frame_counter` in the SFrame header so the
    /// receiver can re-derive the nonce.
    ///
    /// # Errors
    ///
    /// - [`Error::CounterExhausted`] when the 32-bit counter
    ///   rolls over. Caller MUST trigger a fresh MLS commit
    ///   and re-derive the [`CallKey`].
    /// - [`Error::Crypto`] on AEAD failure (practically
    ///   unreachable).
    pub fn seal(&mut self, aad: &[u8], plaintext: &[u8]) -> Result<(u32, Vec<u8>)> {
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
    pub fn open(&self, aad: &[u8], ciphertext: &[u8], frame_counter: u32) -> Result<Vec<u8>> {
        let nonce = self.compose_nonce(frame_counter);
        let key = AeadKey::new(*self.key.as_bytes());
        aead::open(Algorithm::ChaCha20Poly1305, &key, &nonce, aad, ciphertext)
            .map_err(|e| Error::Crypto(format!("aead open: {e:?}")))
    }

    /// Test-only — force the internal counter to a given
    /// value so the exhaustion-edge tests can land on
    /// `u32::MAX - 1` without running 4 billion seals.
    /// Compiled out of the release crate via `cfg(test)` so
    /// no production caller can reset the counter (which
    /// would re-use a nonce under the same key — a critical
    /// AEAD violation).
    #[cfg(test)]
    fn set_counter_for_test(&mut self, c: u32) {
        self.counter = c;
    }

    /// RFC 9605 §4.4 nonce composition. `nonce = salt XOR
    /// (frame_counter as u32 BE in the low 4 bytes)`. The
    /// high 8 bytes of the salt stay; the low 4 bytes XOR in
    /// the counter. Stable across implementations.
    fn compose_nonce(&self, frame_counter: u32) -> Nonce {
        let mut nonce: Nonce = self.nonce_salt;
        let ctr = frame_counter.to_be_bytes();
        // XOR the counter into the trailing 4 bytes of the
        // nonce. Salt provides the entropy in the leading
        // 8 bytes; counter provides the per-frame variance.
        for i in 0..4 {
            nonce[NONCE_LEN - 4 + i] ^= ctr[i];
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
        let mut group_a = lakko_mls::Group::create(&id_a).unwrap();
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

        let mut sender = key_a.participant_key(/* sender_leaf */ 0).unwrap();
        let receiver = key_b.participant_key(/* sender_leaf */ 0).unwrap();

        let aad = b"sframe-header-bytes";
        let pt = b"hello, p2p audio frame";
        let (ctr, ct) = sender.seal(aad, pt).unwrap();
        assert_eq!(ctr, 1, "first sealed frame has counter == 1");

        let opened = receiver.open(aad, &ct, ctr).unwrap();
        assert_eq!(opened.as_slice(), pt.as_ref());
    }

    #[test]
    fn participant_keys_differ_across_leaves() {
        let (id_a, _id_b, group_a) = make_pair();
        let key = CallKey::derive(&group_a, &id_a, b"call/x").unwrap();
        let k0 = key.participant_key(0).unwrap();
        let k1 = key.participant_key(1).unwrap();
        assert_ne!(k0.key_bytes(), k1.key_bytes());
        assert_ne!(k0.nonce_salt(), k1.nonce_salt());
    }

    #[test]
    fn counter_increments_strictly() {
        let key = CallKey::from_seed([0x42; CALL_KEY_LEN]);
        let mut p = key.participant_key(0).unwrap();
        let (c1, _) = p.seal(b"aad", b"f1").unwrap();
        let (c2, _) = p.seal(b"aad", b"f2").unwrap();
        let (c3, _) = p.seal(b"aad", b"f3").unwrap();
        assert_eq!(c1, 1);
        assert_eq!(c2, 2);
        assert_eq!(c3, 3);
    }

    #[test]
    fn wrong_counter_rejects_open() {
        let key = CallKey::from_seed([0x42; CALL_KEY_LEN]);
        let mut sender = key.participant_key(0).unwrap();
        let receiver = key.participant_key(0).unwrap();
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
        let mut sender = key.participant_key(0).unwrap();
        let receiver = key.participant_key(0).unwrap();
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
    // Per RFC 9605 §4.4 the SFrame counter is 32-bit. After
    // u32::MAX successful seals the producer MUST refuse to
    // emit another frame (a 33rd-bit overflow would re-use
    // the AEAD nonce and break confidentiality). The
    // production rotation path is an MLS commit — fresh
    // epoch → fresh CallKey → fresh ParticipantKey → counter
    // resets to 0. These tests use the test-only counter
    // accessor to land near the edge in O(1) rather than
    // grinding 4 billion seals.
    // -----------------------------------------------------

    #[test]
    fn counter_exhaustion_returns_typed_error() {
        let key = CallKey::from_seed([0xCC; CALL_KEY_LEN]);
        let mut p = key.participant_key(0).unwrap();
        // Land at u32::MAX - 2 — two seals must still
        // succeed (counter increments to MAX - 1 then MAX),
        // the third must refuse.
        p.set_counter_for_test(u32::MAX - 2);
        let (c1, _) = p.seal(b"aad", b"frame-A").expect("seal at MAX-2");
        let (c2, _) = p.seal(b"aad", b"frame-B").expect("seal at MAX-1");
        assert_eq!(c1, u32::MAX - 1);
        assert_eq!(c2, u32::MAX);
        // Counter is now at u32::MAX. Third attempt MUST
        // refuse with the typed variant — no panic, no
        // silent wrap, no nonce reuse.
        match p.seal(b"aad", b"frame-C") {
            Err(Error::CounterExhausted) => {},
            Ok((ctr, _)) => panic!("must refuse 33rd-bit roll, got counter={ctr}"),
            Err(other) => panic!("expected CounterExhausted, got {other:?}"),
        }
        // Counter accessor reflects the saturated state so
        // a UI / metrics surface can see the exhaustion.
        assert_eq!(p.counter(), u32::MAX);
    }

    #[test]
    fn counter_max_frame_still_opens_on_receiver() {
        // The last legal seal lands at frame_counter =
        // u32::MAX. A receiver re-deriving the same
        // ParticipantKey at the same epoch MUST authenticate
        // it — no off-by-one at the nonce-composition
        // boundary.
        let key = CallKey::from_seed([0xCD; CALL_KEY_LEN]);
        let mut sender = key.participant_key(0).unwrap();
        let receiver = key.participant_key(0).unwrap();
        sender.set_counter_for_test(u32::MAX - 1);
        let (ctr, ct) = sender.seal(b"aad-edge", b"last-frame").expect("seal at MAX-1");
        assert_eq!(ctr, u32::MAX);
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
        let mut p = key.participant_key(0).unwrap();
        p.set_counter_for_test(u32::MAX);
        for attempt in 0..16 {
            match p.seal(b"aad", b"junk") {
                Err(Error::CounterExhausted) => {},
                Ok(_) => panic!("attempt {attempt} unexpectedly succeeded"),
                Err(other) => panic!("attempt {attempt}: expected CounterExhausted, got {other:?}"),
            }
            assert_eq!(p.counter(), u32::MAX, "counter must not wrap");
        }
    }
}
