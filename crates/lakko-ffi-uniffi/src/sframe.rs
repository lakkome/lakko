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
    /// mismatch, a counter out of range, or AEAD auth failure.
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
    /// Frame counter the seal consumed. Receiver uses
    /// this value to re-derive the AEAD nonce when
    /// calling [`ParticipantKey::open`].
    pub counter: u32,
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
pub fn wrap_sframe_frame(sender_leaf: u32, counter: u32, ciphertext: Vec<u8>) -> Vec<u8> {
    let header = lakko_calls::SframeHeader {
        kid: u64::from(sender_leaf),
        ctr: u64::from(counter),
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
///   (truncated, reserved bit set).
/// - [`FfiError::Call`] when the parsed `kid` does not
///   fit in `u32`. Lakko's call-keying layer pins
///   `sender_leaf` to `u32` (see
///   `crates/lakko-calls/src/header.rs` doc comment); a
///   wire frame with a 5-byte+ kid is either malformed or
///   from a future schema bump that this binary does not
///   know how to dispatch.
/// - [`FfiError::Call`] when the parsed `ctr` does not
///   fit in `u32`. Same reasoning — `ParticipantKey`
///   exhausts its counter at `u32::MAX` and triggers a
///   fresh MLS commit; a wire counter that exceeds
///   `u32` is out-of-protocol.
#[uniffi::export]
pub fn unwrap_sframe_frame(wire: Vec<u8>) -> Result<UnwrappedFrame, FfiError> {
    let (header, consumed) = lakko_calls::SframeHeader::decode(&wire)
        .map_err(|e| FfiError::Call(format!("sframe header decode: {e:?}")))?;
    let sender_leaf = u32::try_from(header.kid)
        .map_err(|_| FfiError::Call("sframe kid does not fit in u32".into()))?;
    let counter = u32::try_from(header.ctr)
        .map_err(|_| FfiError::Call("sframe ctr does not fit in u32".into()))?;
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
    /// `ctr` from the SFrame header; the AEAD counter
    /// the sender used.
    pub counter: u32,
    /// Bytes that follow the header — the ciphertext
    /// (including the trailing AEAD tag).
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

    // ============================================================
    // wire wrap/unwrap tests
    // ============================================================

    /// `wrap_sframe_frame` + `unwrap_sframe_frame` round-
    /// trip preserves `(sender_leaf, counter, ciphertext)`.
    #[test]
    fn wire_round_trip_preserves_fields() {
        let ct = b"cipher-bytes-here".to_vec();
        let wire = wrap_sframe_frame(7, 42, ct.clone());
        let unwrapped = unwrap_sframe_frame(wire).expect("round-trip parses");
        assert_eq!(unwrapped.sender_leaf, 7);
        assert_eq!(unwrapped.counter, 42);
        assert_eq!(unwrapped.ciphertext, ct);
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
        let alice_send = ck.participant_key(0).unwrap();
        let aad = b"call-id".to_vec();
        let plaintext = b"voice frame".to_vec();
        let sealed = alice_send.seal(aad.clone(), plaintext.clone()).unwrap();
        let wire = wrap_sframe_frame(0, sealed.counter, sealed.ciphertext);

        // Receiver side. Bob's transformer parses the
        // wire, looks up Alice's participant key by
        // sender_leaf, opens.
        let unwrapped = unwrap_sframe_frame(wire).unwrap();
        assert_eq!(unwrapped.sender_leaf, 0);
        let alice_recv = ck.participant_key(unwrapped.sender_leaf).unwrap();
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

    /// `unwrap_sframe_frame` refuses a reserved-bit-set
    /// first byte — that's the forward-compatibility
    /// schema marker the encoder will never emit.
    #[test]
    fn unwrap_refuses_reserved_bit_set() {
        // First byte 0b1000_0000 → R=1, reserved-set.
        match unwrap_sframe_frame(vec![0x80, 0x00, 0x00]) {
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
}
