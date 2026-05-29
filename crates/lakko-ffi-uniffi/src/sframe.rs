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
//!   participant nonce salt + per-participant frame
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

    /// Derive the [`ParticipantKey`] for the supplied
    /// leaf index. Each participant on the call holds
    /// their own [`ParticipantKey`] for sealing (`leaf`
    /// = the local participant's leaf index) and one
    /// per remote sender for opening.
    ///
    /// # Errors
    ///
    /// [`FfiError::Call`] if HKDF rejects the input
    /// (practically unreachable with valid inputs).
    #[uniffi::method]
    pub fn participant_key(
        &self,
        sender_leaf: u32,
    ) -> Result<std::sync::Arc<ParticipantKey>, FfiError> {
        let pk = self
            .inner
            .participant_key(sender_leaf)
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

    /// Current frame counter value. Diagnostics only —
    /// the canonical increment lives inside [`Self::seal`].
    #[uniffi::method]
    #[must_use]
    pub fn counter(&self) -> u32 {
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
    ///   `"counter exhausted"` when the 32-bit counter
    ///   rolls over. Caller MUST trigger a fresh MLS
    ///   commit and re-derive the [`CallKey`] before
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
        counter: u32,
    ) -> Result<Vec<u8>, FfiError> {
        self.inner
            .lock()
            .expect("sframe lock")
            .open(&aad, &ciphertext, counter)
            .map_err(|e| FfiError::Call(format!("sframe open: {e:?}")))
    }
}

/// Output of [`ParticipantKey::seal`] — paired counter +
/// ciphertext so the caller can embed the counter in the
/// SFrame wire header alongside the ciphertext.
#[derive(uniffi::Record, Clone, Debug)]
pub struct SealedFrame {
    /// Frame counter the seal consumed. Receiver uses
    /// this value to re-derive the AEAD nonce when
    /// calling [`ParticipantKey::open`].
    pub counter: u32,
    /// Ciphertext including the trailing AEAD tag.
    pub ciphertext: Vec<u8>,
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
        let pk = ck.participant_key(0).unwrap();
        let aad = b"meta".to_vec();
        let plaintext = b"hello sframe".to_vec();

        let sealed = pk.seal(aad.clone(), plaintext.clone()).unwrap();
        assert_eq!(sealed.counter, 1, "first seal lands on counter=1");

        let opened = pk.open(aad, sealed.ciphertext, sealed.counter).unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn seal_increments_counter_across_calls() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let pk = ck.participant_key(0).unwrap();

        let first = pk.seal(vec![], b"a".to_vec()).unwrap();
        let second = pk.seal(vec![], b"b".to_vec()).unwrap();
        let third = pk.seal(vec![], b"c".to_vec()).unwrap();
        assert_eq!(first.counter, 1);
        assert_eq!(second.counter, 2);
        assert_eq!(third.counter, 3);
        // Counter getter reflects the latest seal.
        assert_eq!(pk.counter(), 3);
    }

    /// Two participants derived from the same `CallKey`
    /// must NOT share keystream — the per-participant
    /// HKDF info-context separates them.
    #[test]
    fn distinct_participants_produce_distinct_ciphertext() {
        let ck = CallKey::from_seed(fixture_seed()).unwrap();
        let alice = ck.participant_key(0).unwrap();
        let bob = ck.participant_key(1).unwrap();
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
        let pk = ck.participant_key(0).unwrap();
        let sealed = pk.seal(vec![], b"hello".to_vec()).unwrap();
        let err = pk
            .open(vec![], sealed.ciphertext, sealed.counter + 1)
            .unwrap_err();
        match err {
            FfiError::Call(msg) => {
                assert!(msg.contains("aead open") || msg.contains("sframe open"))
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
        let alice_send = ck.participant_key(0).unwrap();
        let sealed = alice_send.seal(vec![], b"hi from alice".to_vec()).unwrap();

        // Bob's side reconstructs Alice's participant
        // key off the shared CallKey + alice's
        // sender_leaf.
        let alice_recv = ck.participant_key(0).unwrap();
        let opened = alice_recv
            .open(vec![], sealed.ciphertext, sealed.counter)
            .unwrap();
        assert_eq!(opened, b"hi from alice");
    }
}
