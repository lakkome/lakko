// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for SFrame call-key management (G3-1-sframe /
//! G3-2-sframe step 1).
//!
//! ## What this exposes
//!
//! Three pieces, mirroring `lakko_calls::sframe`:
//!
//! - [`CallKey`] — a per-call shared seed (32 bytes). In
//!   production the seed comes from the call's MLS group
//!   exporter (`Group::export_secret("lakko/call/v1", call_id, 32)`);
//!   the FFI does NOT do that derivation itself. The
//!   platform call-session code computes the exporter
//!   output, then hands the seed to
//!   [`CallKey::from_seed`].
//! - [`ParticipantKey`] — per-participant AEAD key + per-
//!   participant nonce salt + per-participant 64-bit frame
//!   counter, which starts at a random value below `2^62`
//!   (ADR-0069). Nothing on this surface sets or resets a
//!   counter. Sealing increments the counter atomically
//!   (the FFI wraps the underlying `ParticipantKey` in a
//!   `Mutex` so the platform's frame-encryptor delegate
//!   thread does not need to serialise externally).
//! - [`SealedFrame`] — record returned by
//!   [`ParticipantKey::seal`] carrying `(counter,
//!   ciphertext)`. The counter goes into the SFrame
//!   header so the receiver can re-derive the nonce.
//!
//! ## What this does NOT do
//!
//! - **It does not wire SFrame into WebRTC** — that's the
//!   per-platform `RTCFrameTransformer` (iOS) /
//!   `FrameEncryptor` + `FrameDecryptor` (Android) impls
//!   that consume this FFI surface. Tracked as
//!   `G3-1-sframe step 2` + `G3-2-sframe step 2`.
//! - **It does not parse the SFrame wire header**
//!   (RFC 9605 §4). That's
//!   `lakko_calls::sframe::wire_header` (already exists)
//!   and a separate FFI surface that the platform
//!   transformer calls on the bytes the codec emits.
//!   Tracked as `G3-{1,2}-sframe step 2a`.
//!
//! ## Threading
//!
//! WebRTC's frame transformer runs on the codec thread.
//! The seal/open paths through `ParticipantKey` need to
//! be safe from there. The underlying
//! `lakko_calls::sframe::ParticipantKey::seal` takes
//! `&mut self` (it increments the counter), so the FFI
//! wraps it in `Mutex<ParticipantKey>`. Contention is a
//! non-issue: each peer's transformer runs on its own
//! thread.

use std::sync::Mutex;

use lakko_calls::link_invite::{
    elect_committer as core_elect_committer, CommitterCandidate as CoreCommitterCandidate,
};
use lakko_calls::sframe as core;

use crate::FfiError;

/// Per-call shared key — the 32-byte seed each
/// participant derives via the MLS exporter.
#[derive(uniffi::Object)]
pub struct CallKey {
    inner: core::CallKey,
}

#[uniffi::export]
impl CallKey {
    /// Construct from a 32-byte exporter output. The
    /// caller MUST have derived the seed via
    /// `Group::export_secret("lakko/call/v1", call_id, 32)`
    /// on each participant's MLS group at the same epoch
    /// — receivers reach the same bytes because the MLS
    /// state machine guarantees it.
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] if `seed` is not exactly 32 bytes.
    #[uniffi::constructor]
    pub fn from_seed(seed: Vec<u8>) -> Result<std::sync::Arc<Self>, FfiError> {
        let arr: [u8; core::CALL_KEY_LEN] = seed
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::Call("call-key seed must be exactly 32 bytes".into()))?;
        Ok(std::sync::Arc::new(Self {
            inner: core::CallKey::from_seed(arr),
        }))
    }

    /// Derive the seal-side [`ParticipantKey`] for the supplied
    /// `(sender_leaf, stream_index)` stream (ADR-0035). Each participant on the
    /// call holds their own [`ParticipantKey`] per stream for sealing (`leaf`
    /// = the local participant's leaf index) and re-derives every remote
    /// sender's key for opening. Keying on the composite KID
    /// `(sender_leaf << 8) | stream_index` keeps every one of a participant's
    /// concurrent streams (audio, camera, screen-share, …) on a distinct key +
    /// nonce-salt space, so their independent counters cannot reuse a
    /// `(key, nonce)` pair — the gap the ADR-0034 per-media-TYPE tag left open
    /// for two same-kind streams. `stream_index` is a `u8`, so the full
    /// `0..=255` range is valid and cannot overflow the KID's 8-bit stream
    /// field.
    ///
    /// The key's frame counter starts at a uniformly random value in
    /// `[0, 2^62)` (ADR-0069), so deriving a key again for the same
    /// `(seed, sender_leaf, stream_index)` — a rejoin, a republished track —
    /// is safe: two such keys reuse a nonce only if the counter ranges they
    /// seal overlap, with probability at most `(L1 + L2) / 2^62`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] if HKDF rejects the input
    /// (practically unreachable with valid inputs).
    #[uniffi::method]
    pub fn participant_key(
        &self,
        sender_leaf: u32,
        stream_index: u8,
    ) -> Result<std::sync::Arc<ParticipantKey>, FfiError> {
        let pk = self
            .inner
            .participant_key_for(sender_leaf, u32::from(stream_index))
            .map_err(|e| FfiError::Call(format!("participant_key: {e:?}")))?;
        Ok(std::sync::Arc::new(ParticipantKey {
            inner: Mutex::new(pk),
        }))
    }

    /// Borrow the raw 32-byte seed. Test / persistence
    /// only — production paths derive via the MLS
    /// exporter and never read the seed back through the
    /// FFI.
    #[uniffi::method]
    #[must_use]
    pub fn seed_bytes(&self) -> Vec<u8> {
        self.inner.as_bytes().to_vec()
    }
}

/// Derive the human-comparable call **safety number** from the shared
/// 32-byte per-call seed.
///
/// Every participant derives the same seed via the MLS exporter, so the
/// safety number is identical for everyone on the call. Render it in the
/// in-call UI for an out-of-band man-in-the-middle check: two
/// participants read the code aloud and a match proves they share the
/// same E2EE root key (security invariant #4). The format is four
/// space-separated four-digit groups.
///
/// # Errors
///
/// [`FfiError::Call`] if `seed` is not exactly 32 bytes.
#[uniffi::export]
pub fn call_safety_number(seed: Vec<u8>) -> Result<String, FfiError> {
    let arr: [u8; core::CALL_KEY_LEN] = seed
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Call("call safety-number seed must be exactly 32 bytes".into()))?;
    Ok(lakko_calls::safety_number::call_safety_number(&arr))
}

/// One candidate committer for the ADR-0032 deterministic election — an MLS
/// group member as the client observes it: its leaf index, whether it is the
/// call's owner (its account matches the capability `owner_account_id`), and
/// whether it is currently present (connected to the SFU room).
#[derive(uniffi::Record, Clone, Debug)]
pub struct CommitterCandidate {
    /// The member's MLS leaf index in the call group.
    pub leaf_index: u32,
    /// `true` if this member is the call's owner.
    pub is_owner: bool,
    /// `true` if this member is currently present (SFU-connected).
    pub is_present: bool,
}

/// Deterministically elect the committer-of-record for a multi-joiner link call
/// (ADR-0032): the owner if present, otherwise the present member with the
/// lowest MLS leaf index. Returns the elected leaf index, or `None` when no
/// member is present.
///
/// Pure + total over the `(roster, presence)` snapshot, so every platform
/// computes the same committer from the same view — the single canonical
/// implementation that keeps failover + reclaim split-brain-free.
#[uniffi::export]
pub fn elect_committer(candidates: Vec<CommitterCandidate>) -> Option<u32> {
    let core: Vec<CoreCommitterCandidate> = candidates
        .into_iter()
        .map(|c| CoreCommitterCandidate {
            leaf_index: c.leaf_index,
            is_owner: c.is_owner,
            is_present: c.is_present,
        })
        .collect();
    core_elect_committer(&core)
}

/// Per-participant AEAD key + per-participant nonce salt
/// + per-participant frame counter.
///
/// Sealing increments the counter under a `Mutex` so the
/// FFI is safe from WebRTC's codec thread without
/// external serialisation.
#[derive(uniffi::Object)]
pub struct ParticipantKey {
    inner: Mutex<core::ParticipantKey>,
}

#[uniffi::export]
impl ParticipantKey {
    /// Sender-leaf index baked into this key.
    #[uniffi::method]
    #[must_use]
    pub fn sender_leaf(&self) -> u32 {
        self.inner.lock().expect("sframe lock").sender_leaf()
    }

    /// Current 64-bit frame counter value: the counter of the
    /// last frame sealed, or the key's random start (ADR-0069)
    /// when none was. Diagnostics only — the canonical
    /// increment lives inside [`Self::seal`].
    #[uniffi::method]
    #[must_use]
    pub fn counter(&self) -> u64 {
        self.inner.lock().expect("sframe lock").counter()
    }

    /// Seal `plaintext` under the next counter value.
    /// Returns [`SealedFrame { counter, ciphertext }`].
    /// The platform transformer embeds `counter` in the
    /// SFrame header so the receiver can re-derive the
    /// nonce.
    ///
    /// # Errors
    ///
    /// - [`FfiError::Call`] containing
    ///   `CounterExhausted` when the 64-bit counter
    ///   stands at its last value (ADR-0069). Nothing is
    ///   sealed; the caller derives a new key before
    ///   continuing — re-using a nonce under the same key
    ///   is an AEAD-fatal mistake.
    /// - [`FfiError::Call`] on AEAD failure (practically
    ///   unreachable).
    #[uniffi::method]
    pub fn seal(&self, aad: Vec<u8>, plaintext: Vec<u8>) -> Result<SealedFrame, FfiError> {
        let (counter, ciphertext) = self
            .inner
            .lock()
            .expect("sframe lock")
            .seal(&aad, &plaintext)
            .map_err(|e| FfiError::Call(format!("sframe seal: {e:?}")))?;
        Ok(SealedFrame {
            counter,
            ciphertext,
        })
    }

    /// Open `ciphertext` (which includes the trailing
    /// AEAD tag) using `counter` from the SFrame header.
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] on AEAD authentication failure
    /// (wrong key / wrong AAD / tampered ciphertext /
    /// wrong counter).
    #[uniffi::method]
    pub fn open(
        &self,
        aad: Vec<u8>,
        ciphertext: Vec<u8>,
        counter: u64,
    ) -> Result<Vec<u8>, FfiError> {
        self.inner
            .lock()
            .expect("sframe lock")
            .open(&aad, &ciphertext, counter)
            .map_err(|e| FfiError::Call(format!("sframe open: {e:?}")))
    }

    /// Seal `plaintext` into a complete RFC 9605 wire frame —
    /// `SframeHeader(kid, ctr) || ciphertext` — with the header
    /// bound as the AEAD AAD, per ADR-0013. The cross-platform
    /// path: the bytes decrypt byte-for-byte on web (via the wasm
    /// `sealFramed`) and the SFU never sees plaintext. The native
    /// frame-encryptor shim uses the parallel `lakko_sframe_seal_framed`
    /// C ABI; this UniFFI method is the in-process Swift / Kotlin
    /// equivalent.
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] on counter exhaustion (re-key required)
    /// or AEAD failure.
    #[uniffi::method]
    pub fn seal_framed(&self, plaintext: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        self.inner
            .lock()
            .expect("sframe lock")
            .seal_framed(&plaintext)
            .map_err(|e| FfiError::Call(format!("sframe seal_framed: {e:?}")))
    }

    /// Open a complete RFC 9605 wire frame produced by
    /// [`Self::seal_framed`] (or any ADR-0013-conformant peer):
    /// parse the header, bind the on-wire header bytes as AAD,
    /// AEAD-open. This key must be derived for the frame's sender
    /// leaf (the caller selects it by the header's KID).
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] on a malformed header, a KID/leaf
    /// mismatch, or AEAD auth failure. Any 64-bit counter the
    /// header carries is accepted (ADR-0069).
    #[uniffi::method]
    pub fn open_framed(&self, wire: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        self.inner
            .lock()
            .expect("sframe lock")
            .open_framed(&wire)
            .map_err(|e| FfiError::Call(format!("sframe open_framed: {e:?}")))
    }
}

/// Output of [`ParticipantKey::seal`] — paired counter +
/// ciphertext so the caller can embed the counter in the
/// SFrame wire header alongside the ciphertext.
#[derive(uniffi::Record, Clone, Debug)]
pub struct SealedFrame {
    /// 64-bit frame counter the seal consumed (ADR-0069).
    /// Receiver uses this value to re-derive the AEAD nonce
    /// when calling [`ParticipantKey::open`].
    pub counter: u64,
    /// Ciphertext including the trailing AEAD tag.
    pub ciphertext: Vec<u8>,
}

// ============================================================
// SFrame wire-format helpers (RFC 9605 §4)
// ============================================================

/// Encode an SFrame wire frame: `header(kid=sender_leaf,
/// ctr=counter) || ciphertext`. The platform transformer
/// returns these bytes to libwebrtc as the encrypted
/// frame payload.
///
/// Mirrors `lakko_calls::SframeHeader::encode()` + a
/// trailing append of the ciphertext. The choice to live
/// behind the FFI rather than at the platform layer keeps
/// the RFC 9605 wire encoder in one place — both iOS and
/// Android cannot drift.
#[uniffi::export]
#[must_use]
pub fn wrap_sframe_frame(sender_leaf: u32, counter: u64, ciphertext: Vec<u8>) -> Vec<u8> {
    let header = lakko_calls::SframeHeader {
        kid: u64::from(sender_leaf),
        ctr: counter,
    };
    let mut out = header.encode();
    out.extend_from_slice(&ciphertext);
    out
}

/// Parse an SFrame wire frame back into
/// `(sender_leaf, counter, ciphertext)`.
///
/// # Errors
///
/// - [`FfiError::Call`] when the header parse fails
///   (a truncated header — an extended field declaring
///   bytes that are not present).
/// - [`FfiError::Call`] when the parsed `kid` does not
///   fit in `u32`. Lakko's call-keying layer pins
///   `sender_leaf` to `u32` (see
///   `crates/lakko-calls/src/header.rs` doc comment); a
///   wire frame with a 5-byte+ kid is either malformed or
///   from a future schema bump that this binary does not
///   know how to dispatch.
///
/// Any 64-bit `ctr` is accepted (ADR-0069).
#[uniffi::export]
pub fn unwrap_sframe_frame(wire: Vec<u8>) -> Result<UnwrappedFrame, FfiError> {
    let (header, consumed) = lakko_calls::SframeHeader::decode(&wire)
        .map_err(|e| FfiError::Call(format!("sframe header decode: {e:?}")))?;
    let sender_leaf = u32::try_from(header.kid)
        .map_err(|_| FfiError::Call("sframe kid does not fit in u32".into()))?;
    let counter = header.ctr;
    let ciphertext = wire[consumed..].to_vec();
    Ok(UnwrappedFrame {
        sender_leaf,
        counter,
        ciphertext,
    })
}

/// Output of [`unwrap_sframe_frame`]. Carries everything
/// the receiver needs to look up the right
/// [`ParticipantKey`] and call its `open`.
#[derive(uniffi::Record, Clone, Debug)]
pub struct UnwrappedFrame {
    /// `kid` from the SFrame header; the
    /// `ParticipantKey`'s sender-leaf index.
    pub sender_leaf: u32,
    /// `ctr` from the SFrame header; the 64-bit AEAD
    /// counter the sender used (ADR-0069).
    pub counter: u64,
    /// Bytes that follow the header — the ciphertext
    /// (including the trailing AEAD tag).
    pub ciphertext: Vec<u8>,
}

/// FFI wrapper over [`lakko_calls::CallKeyRing`] — the mid-call SFrame overlap
/// keyer (ADR-0029, G-18). Holds the current call key plus, during a rotation,
/// the previous one for the overlap window. Wrapped in a `Mutex` because a
/// mid-call rekey / prune mutates the ring while the platform's frame-decryptor
/// delegate may concurrently open frames.
#[derive(uniffi::Object)]
pub struct CallKeyRing {
    inner: Mutex<lakko_calls::CallKeyRing>,
}

#[uniffi::export]
impl CallKeyRing {
    /// Build a ring from a 32-byte call-key seed (the current epoch).
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] if `seed` is not exactly 32 bytes.
    #[uniffi::constructor]
    pub fn from_seed(seed: Vec<u8>) -> Result<std::sync::Arc<Self>, FfiError> {
        let arr: [u8; core::CALL_KEY_LEN] = seed
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::Call("call-key ring seed must be exactly 32 bytes".into()))?;
        Ok(std::sync::Arc::new(Self {
            inner: Mutex::new(lakko_calls::CallKeyRing::from_seed(arr)),
        }))
    }

    /// Rotate to a new-epoch seed on a mid-call MLS epoch change, retaining the
    /// previous key until `now_ms + clamp(overlap_ms)` so in-flight frames
    /// sealed under it still open (ADR-0008; C-8 / C-9 / C-10). `overlap_ms` is
    /// clamped to `[OVERLAP_MIN_MS, OVERLAP_MAX_MS]`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] if `seed` is not exactly 32 bytes.
    #[uniffi::method]
    pub fn rekey_from_seed_at(
        &self,
        seed: Vec<u8>,
        now_ms: u64,
        overlap_ms: u64,
    ) -> Result<(), FfiError> {
        let arr: [u8; core::CALL_KEY_LEN] = seed
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::Call("call-key ring seed must be exactly 32 bytes".into()))?;
        self.inner
            .lock()
            .expect("call-key ring lock")
            .rekey_from_seed_at(arr, now_ms, overlap_ms);
        Ok(())
    }

    /// Drop the retained previous key once it has expired at `now_ms`. The
    /// platform ticks this on its clock so the core owns the 1-second decision.
    #[uniffi::method]
    pub fn prune_expired(&self, now_ms: u64) {
        self.inner
            .lock()
            .expect("call-key ring lock")
            .prune_expired(now_ms);
    }

    /// `true` when a previous key is retained AND still inside its overlap
    /// window at `now_ms`.
    #[uniffi::method]
    #[must_use]
    pub fn has_live_previous(&self, now_ms: u64) -> bool {
        self.inner
            .lock()
            .expect("call-key ring lock")
            .has_live_previous(now_ms)
    }

    /// Derive the seal-side [`ParticipantKey`] for a `(sender_leaf,
    /// stream_index)` stream from the current epoch (ADR-0035). The caller
    /// re-fetches this after a `rekey_from_seed_at` to seal under the new key.
    /// Keying on the composite KID `(sender_leaf << 8) | stream_index` keeps
    /// every one of a participant's concurrent streams on a distinct key +
    /// nonce-salt space, so their independent counters cannot reuse a
    /// `(key, nonce)` pair. `stream_index` is a `u8`, so the full `0..=255`
    /// range is valid. The key starts at a random counter in `[0, 2^62)`
    /// (ADR-0069).
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] if HKDF rejects the input (practically unreachable).
    #[uniffi::method]
    pub fn participant_key(
        &self,
        sender_leaf: u32,
        stream_index: u8,
    ) -> Result<std::sync::Arc<ParticipantKey>, FfiError> {
        let kid = lakko_calls::SframeHeader::kid_for(sender_leaf, u32::from(stream_index))
            .map_err(|e| FfiError::Call(format!("ring participant_key kid compose: {e}")))?;
        let pk = self
            .inner
            .lock()
            .expect("call-key ring lock")
            .participant_key(kid)
            .map_err(|e| FfiError::Call(format!("ring participant_key: {e:?}")))?;
        Ok(std::sync::Arc::new(ParticipantKey {
            inner: Mutex::new(pk),
        }))
    }

    /// Open an RFC 9605 framed SFrame with the overlap fall-back: try the
    /// current key for `kid` (ADR-0035; the composite `(sender_leaf,
    /// stream_index)` identifier the receiver read from the frame header), and
    /// on an AEAD failure fall back to the retained previous key while its
    /// window is live (ADR-0029). The decryptor takes the KID straight from the
    /// wire — no separate media discriminator.
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] on a malformed header, or AEAD auth failure under both
    /// the current and previous keys.
    #[uniffi::method]
    pub fn open_framed_with_overlap(&self, kid: u64, wire: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        self.inner
            .lock()
            .expect("call-key ring lock")
            .try_open_framed_with_overlap(kid, &wire)
            .map_err(|e| FfiError::Call(format!("ring open_framed_with_overlap: {e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_seed() -> Vec<u8> {
        vec![0x42_u8; core::CALL_KEY_LEN]
    }

    #[test]
    fn call_key_from_seed_round_trip() {
        let ck = CallKey::from_seed(fixture_seed()).expect("fixture seed has correct length");
        assert_eq!(ck.seed_bytes(), fixture_seed());
    }

    #[test]
    fn call_key_from_seed_rejects_wrong_length() {
        // `Arc<CallKey>` does not implement `Debug`,
        // so `unwrap_err` is unavailable — branch on
        // the Result directly.
        match CallKey::from_seed(vec![0x42; 16]) {
            Ok(_) => panic!("16-byte seed must be rejected"),
            Err(FfiError::Call(msg)) => assert!(msg.contains("32 bytes")),
            Err(other) => panic!("expected FfiError::Call, got {other:?}"),
        }
    }

    #[test]
    fn seal_open_round_trip_under_same_participant_key() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let pk = ck.participant_key(0, 0).unwrap();
        let aad = b"meta".to_vec();
        let plaintext = b"hello sframe".to_vec();

        let start = pk.counter();
        assert!(start < 1 << 62, "a key starts below 2^62 (ADR-0069)");
        let sealed = pk.seal(aad.clone(), plaintext.clone()).unwrap();
        assert_eq!(sealed.counter, start + 1, "first seal lands on start + 1");

        let opened = pk.open(aad, sealed.ciphertext, sealed.counter).unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn seal_increments_counter_across_calls() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let pk = ck.participant_key(0, 0).unwrap();

        let start = pk.counter();
        let first = pk.seal(vec![], b"a".to_vec()).unwrap();
        let second = pk.seal(vec![], b"b".to_vec()).unwrap();
        let third = pk.seal(vec![], b"c".to_vec()).unwrap();
        assert_eq!(first.counter, start + 1);
        assert_eq!(second.counter, start + 2);
        assert_eq!(third.counter, start + 3);
        // Counter getter reflects the latest seal.
        assert_eq!(pk.counter(), start + 3);
    }

    /// Two participants derived from the same `CallKey`
    /// must NOT share keystream — the per-participant
    /// HKDF info-context separates them.
    #[test]
    fn distinct_participants_produce_distinct_ciphertext() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let alice = ck.participant_key(0, 0).unwrap();
        let bob = ck.participant_key(1, 0).unwrap();
        let aad = b"shared".to_vec();
        let pt = b"hello".to_vec();
        let a_sealed = alice.seal(aad.clone(), pt.clone()).unwrap();
        let b_sealed = bob.seal(aad.clone(), pt.clone()).unwrap();
        assert_ne!(
            a_sealed.ciphertext, b_sealed.ciphertext,
            "distinct sender_leaf must produce distinct ciphertext"
        );
        // Alice's ciphertext refuses to open against
        // Bob's key (different per-leaf AEAD key).
        assert!(bob
            .open(aad, a_sealed.ciphertext, a_sealed.counter)
            .is_err());
    }

    /// Wrong counter → AEAD auth failure (nonce no
    /// longer matches what the seal used).
    #[test]
    fn open_with_wrong_counter_fails() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let pk = ck.participant_key(0, 0).unwrap();
        let sealed = pk.seal(vec![], b"hello".to_vec()).unwrap();
        let err = pk
            .open(vec![], sealed.ciphertext, sealed.counter + 1)
            .unwrap_err();
        match err {
            FfiError::Call(msg) => {
                assert!(msg.contains("aead open") || msg.contains("sframe open"));
            },
            other => panic!("expected FfiError::Call, got {other:?}"),
        }
    }

    /// The receiver-side path: re-derive Alice's
    /// participant key from the same `CallKey` on a
    /// fresh instance, then open her sealed bytes. This
    /// is exactly what a remote peer does.
    #[test]
    fn receiver_can_open_with_freshly_derived_participant_key() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let alice_send = ck.participant_key(0, 0).unwrap();
        let sealed = alice_send.seal(vec![], b"hi from alice".to_vec()).unwrap();

        // Bob's side reconstructs Alice's participant
        // key off the shared CallKey + alice's
        // sender_leaf.
        let alice_recv = ck.participant_key(0, 0).unwrap();
        let opened = alice_recv
            .open(vec![], sealed.ciphertext, sealed.counter)
            .unwrap();
        assert_eq!(opened, b"hi from alice");
    }

    // ============================================================
    // wire wrap/unwrap tests
    // ============================================================

    /// `wrap_sframe_frame` + `unwrap_sframe_frame` round-
    /// trip preserves `(sender_leaf, counter, ciphertext)`,
    /// for a small counter and for one that needs all 8
    /// bytes (ADR-0069).
    #[test]
    fn wire_round_trip_preserves_fields() {
        let ct = b"cipher-bytes-here".to_vec();
        for counter in [42_u64, 1 << 32, 0x3FED_CBA9_8765_4321, u64::MAX] {
            let wire = wrap_sframe_frame(7, counter, ct.clone());
            let unwrapped = unwrap_sframe_frame(wire).expect("round-trip parses");
            assert_eq!(unwrapped.sender_leaf, 7);
            assert_eq!(unwrapped.counter, counter);
            assert_eq!(unwrapped.ciphertext, ct);
        }
    }

    // ============================================================
    // cross-stack golden vector (ADR-0013 / ADR-0069)
    // ============================================================

    const GOLDEN_LEAF: u32 = 3;
    const GOLDEN_PLAINTEXT: &[u8] = b"lakko sframe golden vector v1";
    const GOLDEN_WIRE_HEX: &str =
        "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a";

    fn golden_wire() -> Vec<u8> {
        (0..GOLDEN_WIRE_HEX.len() / 2)
            .map(|i| u8::from_str_radix(&GOLDEN_WIRE_HEX[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    /// (1) The canonical cross-stack golden wire (seed `0x42` x 32, leaf 3,
    /// stream 0, a frame at counter 1) OPENS through this surface, by a
    /// participant key and by the ring.
    #[test]
    fn golden_wire_opens_to_the_plaintext() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let pk = ck.participant_key(GOLDEN_LEAF, 0).unwrap();
        assert_eq!(pk.open_framed(golden_wire()).unwrap(), GOLDEN_PLAINTEXT);

        let unwrapped = unwrap_sframe_frame(golden_wire()).unwrap();
        assert_eq!(unwrapped.sender_leaf, 0x0300, "the header's whole KID");
        assert_eq!(unwrapped.counter, 1);

        let ring = CallKeyRing::from_seed(fixture_seed()).unwrap();
        assert_eq!(
            ring.open_framed_with_overlap(0x0300, golden_wire())
                .unwrap(),
            GOLDEN_PLAINTEXT
        );
    }

    /// (2) A frame this surface seals carries the composite KID and a 64-bit
    /// counter one above the key's random start, and opens under an
    /// independently derived key. Since ADR-0069 a fresh key does not seal the
    /// golden bytes, and nothing here sets a counter.
    #[test]
    fn sealed_frame_carries_the_kid_and_a_64_bit_counter() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let sender = ck.participant_key(GOLDEN_LEAF, 0).unwrap();
        let start = sender.counter();
        assert!(start < 1 << 62);
        let wire = sender.seal_framed(GOLDEN_PLAINTEXT.to_vec()).unwrap();

        let unwrapped = unwrap_sframe_frame(wire.clone()).unwrap();
        assert_eq!(unwrapped.sender_leaf, 0x0300, "KID = (leaf 3 << 8) | 0");
        assert_eq!(unwrapped.counter, start + 1);
        assert_eq!(sender.counter(), start + 1);

        let receiver = ck.participant_key(GOLDEN_LEAF, 0).unwrap();
        assert_ne!(receiver.counter(), start, "starts are independent");
        assert_eq!(receiver.open_framed(wire).unwrap(), GOLDEN_PLAINTEXT);
    }

    /// Full send-side / receive-side path: alice
    /// seals-and-wraps; bob unwraps-and-opens. This is the
    /// shape the platform transformers (RTCFrameTransformer
    /// on iOS, FrameEncryptor + FrameDecryptor on Android)
    /// stage when they implement
    /// `transform(encryptedFrame)` and
    /// `transform(decryptedFrame)` respectively.
    #[test]
    fn send_then_receive_full_pipeline() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let alice_send = ck.participant_key(0, 0).unwrap();
        let aad = b"call-id".to_vec();
        let plaintext = b"voice frame".to_vec();
        let sealed = alice_send.seal(aad.clone(), plaintext.clone()).unwrap();
        let wire = wrap_sframe_frame(0, sealed.counter, sealed.ciphertext);

        // Receiver side. Bob's transformer parses the
        // wire, looks up Alice's participant key by
        // sender_leaf, opens.
        let unwrapped = unwrap_sframe_frame(wire).unwrap();
        assert_eq!(unwrapped.sender_leaf, 0);
        let alice_recv = ck.participant_key(unwrapped.sender_leaf, 0).unwrap();
        let opened = alice_recv
            .open(aad, unwrapped.ciphertext, unwrapped.counter)
            .unwrap();
        assert_eq!(opened, plaintext);
    }

    /// `unwrap_sframe_frame` refuses an empty input
    /// (header decoder bails on the first byte).
    #[test]
    fn unwrap_refuses_empty_wire() {
        match unwrap_sframe_frame(vec![]) {
            Err(FfiError::Call(msg)) => assert!(msg.contains("decode")),
            other => panic!("expected decode error, got {other:?}"),
        }
    }

    /// `unwrap_sframe_frame` refuses a truncated header — an
    /// extended field declaring trailing bytes that are not
    /// present.
    #[test]
    fn unwrap_refuses_a_truncated_header() {
        // 0x0B = RFC 9605 §4.3 X=0 K=0 Y=1 C=3 → declares a 4-byte CTR with
        // none present → the header decode fails as truncated.
        match unwrap_sframe_frame(vec![0x0B]) {
            Err(FfiError::Call(msg)) => assert!(msg.contains("decode")),
            other => panic!("expected decode error, got {other:?}"),
        }
    }

    /// The FFI wrapper reproduces the core safety-number golden
    /// vector (so the Swift / Kotlin binding renders byte-identical
    /// strings to the web wasm binding) and refuses a wrong-length
    /// seed.
    #[test]
    fn call_safety_number_matches_core_golden() {
        let out = call_safety_number(vec![0u8; 32]).expect("32-byte seed");
        assert_eq!(out, "6397 5175 0657 3748");
        assert!(call_safety_number(vec![0u8; 31]).is_err());
    }

    #[test]
    fn call_key_ring_from_seed_rejects_wrong_length() {
        assert!(CallKeyRing::from_seed(vec![0u8; 31]).is_err());
        assert!(CallKeyRing::from_seed(vec![0u8; 33]).is_err());
        assert!(CallKeyRing::from_seed(vec![0u8; 32]).is_ok());
    }

    #[test]
    fn call_key_ring_rekey_opens_both_epochs() {
        let ring = CallKeyRing::from_seed(vec![0x11u8; 32]).expect("ring a");
        let leaf = 3u32;
        // The composite KID the receiver reads from the frame header (stream 0).
        let kid = lakko_calls::SframeHeader::kid_for(leaf, 0).expect("kid");
        // Seal an old-epoch frame under seed A.
        let pk_old = ring.participant_key(leaf, 0).expect("pk old");
        let old_wire = pk_old.seal_framed(b"old".to_vec()).expect("seal old");

        // Rotate to epoch B with the default overlap window at t=1000.
        ring.rekey_from_seed_at(vec![0x22u8; 32], 1_000, 1_000)
            .expect("rekey");
        assert!(ring.has_live_previous(1_500));

        // Seal a new-epoch frame under seed B.
        let pk_new = ring.participant_key(leaf, 0).expect("pk new");
        let new_wire = pk_new.seal_framed(b"new".to_vec()).expect("seal new");

        // Both open through the ring's overlap fall-back (keyed by the KID).
        assert_eq!(
            ring.open_framed_with_overlap(kid, old_wire).expect("old"),
            b"old".to_vec()
        );
        assert_eq!(
            ring.open_framed_with_overlap(kid, new_wire).expect("new"),
            b"new".to_vec()
        );
    }

    #[test]
    fn call_key_ring_prune_closes_the_overlap_window() {
        let ring = CallKeyRing::from_seed(vec![0x11u8; 32]).expect("ring");
        let leaf = 3u32;
        let kid = lakko_calls::SframeHeader::kid_for(leaf, 0).expect("kid");
        let pk_old = ring.participant_key(leaf, 0).expect("pk");
        let old_wire = pk_old.seal_framed(b"old".to_vec()).expect("seal");
        ring.rekey_from_seed_at(vec![0x22u8; 32], 1_000, 1_000)
            .expect("rekey"); // expires at 2000

        // Inside the window the old frame still opens.
        assert!(ring.open_framed_with_overlap(kid, old_wire.clone()).is_ok());

        // At/after expiry, prune closes the window and the old frame is refused.
        assert!(!ring.has_live_previous(2_000));
        ring.prune_expired(2_000);
        assert!(ring.open_framed_with_overlap(kid, old_wire).is_err());
    }
}
