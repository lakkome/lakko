// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for SFrame call-key management
//! (G3-sframe step 1 — web mirror of
//! `crates/lakko-ffi-uniffi/src/sframe.rs`).
//!
//! Mirrors the UniFFI shape:
//!
//! - [`WasmCallKey`] (JS `CallKey`) — `new(seed: Uint8Array)`
//!   + `participantKey(sender_leaf: u32, stream_index: u32) → ParticipantKey`
//!     (the seal side; keyed on the composite SFrame KID
//!     `(sender_leaf << 8) | stream_index`, per ADR-0035)
//!   + `seedBytes() → Uint8Array`.
//! - [`WasmParticipantKey`] (JS `ParticipantKey`) — `seal(aad,
//!   plaintext) → SealedFrame`, `open(aad, ciphertext,
//!   counter) → Uint8Array`, plus `senderLeaf()` and
//!   `counter()` getters. The frame counter is 64-bit and
//!   crosses to JS as a `bigint` everywhere (ADR-0069); a
//!   key starts at a random counter below `2^62`, and
//!   nothing on this surface sets or resets a counter.
//! - [`WasmSealedFrame`] (JS `SealedFrame`) — paired
//!   `counter` + `ciphertext` so the platform transformer
//!   embeds the counter in the SFrame wire header.
//!
//! The web call-side wires this through the browser's
//! `RTCRtpScriptTransform` API (a separate sub-sprint
//! `G3-web-sframe step 2`) — same posture iOS and Android
//! follow with `RTCFrameTransformer` and
//! `FrameEncryptor`/`FrameDecryptor`.
//!
//! Errors collapse into `JsError` with messages aligned
//! to the UniFFI variant prose so a cross-platform log
//! line reads the same.

use std::sync::Mutex;

use lakko_calls::link_invite::{
    elect_committer as core_elect_committer, CommitterCandidate as CoreCommitterCandidate,
};
use lakko_calls::sframe as core;
use wasm_bindgen::prelude::*;

/// Per-call shared key — the 32-byte seed each participant
/// derives via the MLS exporter.
#[wasm_bindgen(js_name = "CallKey")]
pub struct WasmCallKey {
    inner: core::CallKey,
}

#[wasm_bindgen(js_class = "CallKey")]
impl WasmCallKey {
    /// Construct from a 32-byte exporter output. The caller
    /// MUST have derived the seed via
    /// `Group.exportSecret("lakko/call/v1", call_id, 32)`
    /// — receivers reach the same bytes because the MLS
    /// state machine guarantees it.
    ///
    /// # Errors
    ///
    /// `JsError` if `seed` is not exactly 32 bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8]) -> Result<WasmCallKey, JsError> {
        let arr: [u8; core::CALL_KEY_LEN] = seed
            .try_into()
            .map_err(|_| JsError::new("call-key seed must be exactly 32 bytes"))?;
        Ok(WasmCallKey {
            inner: core::CallKey::from_seed(arr),
        })
    }

    /// Derive the seal-side [`WasmParticipantKey`] for the supplied
    /// `(sender_leaf, stream_index)` stream (ADR-0035). Keying on the composite
    /// SFrame KID `(sender_leaf << 8) | stream_index` gives every one of a
    /// participant's concurrent streams (audio, camera, screen-share, …) a
    /// distinct key + nonce-salt space, so their independent counters cannot
    /// reuse a `(key, nonce)` pair — the gap the ADR-0034 per-media-TYPE tag
    /// left open for two same-kind streams.
    ///
    /// `stream_index` crosses as a `u32`; a value `> 255` fails closed (it would
    /// overflow the KID's 8-bit stream field) rather than silently truncating.
    ///
    /// The key's frame counter starts at a uniformly random value in
    /// `[0, 2^62)` (ADR-0069), so deriving a key again for the same
    /// `(seed, sender_leaf, stream_index)` — a reload, a rejoin, a republished
    /// track — is safe: two such keys reuse a nonce only if the counter ranges
    /// they seal overlap, with probability at most `(L1 + L2) / 2^62`.
    ///
    /// # Errors
    ///
    /// `JsError` if `stream_index > 255`, or on HKDF failure (practically
    /// unreachable with valid inputs).
    #[wasm_bindgen(js_name = "participantKey")]
    pub fn participant_key(
        &self,
        sender_leaf: u32,
        stream_index: u32,
    ) -> Result<WasmParticipantKey, JsError> {
        let pk = self
            .inner
            .participant_key_for(sender_leaf, stream_index)
            .map_err(|e| JsError::new(&format!("call crypto error: participant_key: {e:?}")))?;
        Ok(WasmParticipantKey {
            inner: Mutex::new(pk),
        })
    }

    /// Borrow the raw 32-byte seed. Test / persistence
    /// only.
    #[wasm_bindgen(js_name = "seedBytes")]
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
/// in-call UI for an out-of-band man-in-the-middle check: matching codes
/// prove a shared E2EE root key (security invariant #4). The format is
/// four space-separated four-digit groups.
///
/// # Errors
///
/// `JsError` if `seed` is not exactly 32 bytes.
#[wasm_bindgen(js_name = "callSafetyNumber")]
pub fn call_safety_number(seed: &[u8]) -> Result<String, JsError> {
    let arr: [u8; core::CALL_KEY_LEN] = seed
        .try_into()
        .map_err(|_| JsError::new("call safety-number seed must be exactly 32 bytes"))?;
    Ok(lakko_calls::safety_number::call_safety_number(&arr))
}

/// Deterministically elect the committer-of-record for a multi-joiner link call
/// (ADR-0032): the owner if present, otherwise the present member with the
/// lowest MLS leaf index. Returns the elected leaf index, or `undefined` when no
/// member is present.
///
/// Candidates are passed as three parallel arrays (leaf index, owner flag,
/// presence flag) — the wasm-friendly shape for a small struct array. A length
/// mismatch yields `undefined` (defensive). This is the one canonical
/// implementation every platform shares, so failover + reclaim cannot split-brain.
#[wasm_bindgen(js_name = "electCommitter")]
#[must_use]
pub fn elect_committer(leaf_indices: &[u32], is_owner: &[u8], is_present: &[u8]) -> Option<u32> {
    if is_owner.len() != leaf_indices.len() || is_present.len() != leaf_indices.len() {
        return None;
    }
    let candidates: Vec<CoreCommitterCandidate> = leaf_indices
        .iter()
        .zip(is_owner.iter())
        .zip(is_present.iter())
        .map(|((&leaf, &owner), &present)| CoreCommitterCandidate {
            leaf_index: leaf,
            is_owner: owner != 0,
            is_present: present != 0,
        })
        .collect();
    core_elect_committer(&candidates)
}

/// Per-participant AEAD key + per-participant nonce salt
/// + per-participant frame counter. Sealing increments
/// the counter under a `Mutex` so the transformer's
/// codec-thread call is safe without external
/// serialisation.
#[wasm_bindgen(js_name = "ParticipantKey")]
pub struct WasmParticipantKey {
    inner: Mutex<core::ParticipantKey>,
}

#[wasm_bindgen(js_class = "ParticipantKey")]
impl WasmParticipantKey {
    /// Sender-leaf index baked into this key.
    #[wasm_bindgen(js_name = "senderLeaf")]
    #[must_use]
    pub fn sender_leaf(&self) -> u32 {
        self.inner.lock().expect("sframe lock").sender_leaf()
    }

    /// Current 64-bit frame counter value (a `bigint` in JS): the counter of
    /// the last frame sealed, or the key's random start (ADR-0069) when none
    /// was. Diagnostics only.
    #[wasm_bindgen(js_name = "counter")]
    #[must_use]
    pub fn counter(&self) -> u64 {
        self.inner.lock().expect("sframe lock").counter()
    }

    /// Seal `plaintext` under the next counter value.
    /// Returns [`WasmSealedFrame { counter, ciphertext }`].
    ///
    /// # Errors
    ///
    /// `JsError` containing `CounterExhausted` when the
    /// 64-bit counter stands at its last value (ADR-0069);
    /// nothing is sealed and the caller derives a new key
    /// before continuing.
    #[wasm_bindgen(js_name = "seal")]
    pub fn seal(&self, aad: &[u8], plaintext: &[u8]) -> Result<WasmSealedFrame, JsError> {
        let (counter, ciphertext) = self
            .inner
            .lock()
            .expect("sframe lock")
            .seal(aad, plaintext)
            .map_err(|e| JsError::new(&format!("call crypto error: sframe seal: {e:?}")))?;
        Ok(WasmSealedFrame {
            counter,
            ciphertext,
        })
    }

    /// Open `ciphertext` using the 64-bit `counter` (a
    /// `bigint` in JS) from the SFrame header.
    ///
    /// # Errors
    ///
    /// `JsError` on AEAD authentication failure (wrong
    /// key / wrong AAD / tampered ciphertext / wrong
    /// counter).
    #[wasm_bindgen(js_name = "open")]
    pub fn open(&self, aad: &[u8], ciphertext: &[u8], counter: u64) -> Result<Vec<u8>, JsError> {
        self.inner
            .lock()
            .expect("sframe lock")
            .open(aad, ciphertext, counter)
            .map_err(|e| JsError::new(&format!("call crypto error: sframe open: {e:?}")))
    }

    /// Seal `plaintext` into a complete RFC 9605 wire frame —
    /// `SframeHeader(kid, ctr) || ciphertext` — with the SFrame
    /// header bound as the AEAD AAD, per ADR-0013. This is the
    /// path the web `RTCRtpScriptTransform` worker uses on the
    /// outgoing side; the resulting bytes decrypt byte-for-byte on
    /// iOS/Android (and vice versa) because the AAD is the
    /// platform-independent header, not any host RTP metadata.
    ///
    /// # Errors
    ///
    /// `JsError` containing `CounterExhausted` when the 64-bit
    /// counter stands at its last value, else an AEAD error.
    #[wasm_bindgen(js_name = "sealFramed")]
    pub fn seal_framed(&self, plaintext: &[u8]) -> Result<Vec<u8>, JsError> {
        self.inner
            .lock()
            .expect("sframe lock")
            .seal_framed(plaintext)
            .map_err(|e| JsError::new(&format!("call crypto error: sframe seal_framed: {e:?}")))
    }

    /// Open a complete RFC 9605 wire frame produced by
    /// [`Self::seal_framed`] (or any ADR-0013-conformant peer):
    /// parse the header, bind the on-wire header bytes as AAD,
    /// AEAD-open. This key must be derived for the frame's sender
    /// leaf (the worker selects it by the header's KID). The
    /// incoming-side path for the web worker.
    ///
    /// # Errors
    ///
    /// `JsError` on a malformed header, a KID that doesn't match
    /// this key's leaf, or AEAD auth failure. Any 64-bit counter
    /// the header carries is accepted (ADR-0069).
    #[wasm_bindgen(js_name = "openFramed")]
    pub fn open_framed(&self, wire: &[u8]) -> Result<Vec<u8>, JsError> {
        self.inner
            .lock()
            .expect("sframe lock")
            .open_framed(wire)
            .map_err(|e| JsError::new(&format!("call crypto error: sframe open_framed: {e:?}")))
    }
}

/// FFI wrapper over [`lakko_calls::CallKeyRing`] — the mid-call SFrame overlap
/// keyer (ADR-0029, G-18). Holds the current call key plus, during a rotation,
/// the previous one for the overlap window. `Mutex` because a mid-call rekey /
/// prune mutates the ring while the receive-side worker may open frames.
#[wasm_bindgen(js_name = "CallKeyRing")]
pub struct WasmCallKeyRing {
    inner: Mutex<lakko_calls::CallKeyRing>,
}

#[wasm_bindgen(js_class = "CallKeyRing")]
impl WasmCallKeyRing {
    /// Build a ring from a 32-byte call-key seed (the current epoch).
    ///
    /// # Errors
    ///
    /// `JsError` if `seed` is not exactly 32 bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8]) -> Result<WasmCallKeyRing, JsError> {
        let arr: [u8; core::CALL_KEY_LEN] = seed
            .try_into()
            .map_err(|_| JsError::new("call-key ring seed must be exactly 32 bytes"))?;
        Ok(WasmCallKeyRing {
            inner: Mutex::new(lakko_calls::CallKeyRing::from_seed(arr)),
        })
    }

    /// Rotate to a new-epoch seed on a mid-call MLS epoch change, retaining the
    /// previous key until `now_ms + clamp(overlap_ms)` so in-flight frames
    /// sealed under it still open (ADR-0008; C-8 / C-9 / C-10). `now_ms` +
    /// `overlap_ms` cross to JS as `BigInt`.
    ///
    /// # Errors
    ///
    /// `JsError` if `seed` is not exactly 32 bytes.
    #[wasm_bindgen(js_name = "rekeyFromSeedAt")]
    pub fn rekey_from_seed_at(
        &self,
        seed: &[u8],
        now_ms: u64,
        overlap_ms: u64,
    ) -> Result<(), JsError> {
        let arr: [u8; core::CALL_KEY_LEN] = seed
            .try_into()
            .map_err(|_| JsError::new("call-key ring seed must be exactly 32 bytes"))?;
        self.inner
            .lock()
            .expect("call-key ring lock")
            .rekey_from_seed_at(arr, now_ms, overlap_ms);
        Ok(())
    }

    /// Drop the retained previous key once it has expired at `now_ms`.
    #[wasm_bindgen(js_name = "pruneExpired")]
    pub fn prune_expired(&self, now_ms: u64) {
        self.inner
            .lock()
            .expect("call-key ring lock")
            .prune_expired(now_ms);
    }

    /// `true` when a previous key is retained AND still inside its overlap
    /// window at `now_ms`.
    #[wasm_bindgen(js_name = "hasLivePrevious")]
    #[must_use]
    pub fn has_live_previous(&self, now_ms: u64) -> bool {
        self.inner
            .lock()
            .expect("call-key ring lock")
            .has_live_previous(now_ms)
    }

    /// Derive the seal-side [`WasmParticipantKey`] for a `(sender_leaf,
    /// stream_index)` stream from the current epoch (ADR-0035). Re-fetched after
    /// a `rekeyFromSeedAt` to seal under the new key. Keying on the composite
    /// SFrame KID `(sender_leaf << 8) | stream_index` keeps every one of a
    /// participant's concurrent streams on a distinct key + nonce-salt space.
    /// `stream_index` crosses as a `u32`; a value `> 255` fails closed rather
    /// than silently truncating. The key starts at a random counter in
    /// `[0, 2^62)` (ADR-0069).
    ///
    /// # Errors
    ///
    /// `JsError` if `stream_index > 255`, or on HKDF failure (practically
    /// unreachable).
    #[wasm_bindgen(js_name = "participantKey")]
    pub fn participant_key(
        &self,
        sender_leaf: u32,
        stream_index: u32,
    ) -> Result<WasmParticipantKey, JsError> {
        let kid = lakko_calls::SframeHeader::kid_for(sender_leaf, stream_index).map_err(|e| {
            JsError::new(&format!(
                "call crypto error: ring participant_key kid compose: {e}"
            ))
        })?;
        let pk = self
            .inner
            .lock()
            .expect("call-key ring lock")
            .participant_key(kid)
            .map_err(|e| {
                JsError::new(&format!("call crypto error: ring participant_key: {e:?}"))
            })?;
        Ok(WasmParticipantKey {
            inner: Mutex::new(pk),
        })
    }

    /// Open an RFC 9605 framed SFrame with the overlap fall-back: try the
    /// current key for `kid` (ADR-0035; the composite `(sender_leaf,
    /// stream_index)` identifier the worker read from the frame header), and on
    /// an AEAD failure fall back to the retained previous key while its window
    /// is live (ADR-0029). The receive-side path for the web worker across a
    /// mid-call rekey. `kid` crosses to JS as a `BigInt`; the decryptor takes it
    /// straight from the wire — no separate media discriminator.
    ///
    /// # Errors
    ///
    /// `JsError` on a malformed header, or on AEAD auth failure under both the
    /// current and previous keys.
    #[wasm_bindgen(js_name = "openFramedWithOverlap")]
    pub fn open_framed_with_overlap(&self, kid: u64, wire: &[u8]) -> Result<Vec<u8>, JsError> {
        self.inner
            .lock()
            .expect("call-key ring lock")
            .try_open_framed_with_overlap(kid, wire)
            .map_err(|e| {
                JsError::new(&format!(
                    "call crypto error: ring open_framed_with_overlap: {e:?}"
                ))
            })
    }
}

/// Output of [`WasmParticipantKey::seal`] — paired
/// counter + ciphertext so the JS transformer can embed
/// the counter in the SFrame wire header alongside the
/// ciphertext.
#[wasm_bindgen(js_name = "SealedFrame")]
pub struct WasmSealedFrame {
    counter: u64,
    ciphertext: Vec<u8>,
}

#[wasm_bindgen(js_class = "SealedFrame")]
impl WasmSealedFrame {
    /// 64-bit frame counter the seal consumed (a `bigint`
    /// in JS). Goes into the SFrame header.
    #[wasm_bindgen(getter, js_name = "counter")]
    #[must_use]
    pub fn counter(&self) -> u64 {
        self.counter
    }

    /// Ciphertext including the trailing AEAD tag.
    #[wasm_bindgen(getter, js_name = "ciphertext")]
    #[must_use]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.ciphertext.clone()
    }
}

// ============================================================
// SFrame wire-format helpers (RFC 9605 §4)
// ============================================================

/// Encode an SFrame wire frame: `header(kid=sender_leaf,
/// ctr=counter) || ciphertext`. Mirror of the UniFFI
/// helper at `crates/lakko-ffi-uniffi/src/sframe.rs`. `counter` is 64-bit
/// (a `bigint` in JS).
#[wasm_bindgen(js_name = "wrapSframeFrame")]
#[must_use]
pub fn wrap_sframe_frame_js(sender_leaf: u32, counter: u64, ciphertext: &[u8]) -> Vec<u8> {
    let header = lakko_calls::SframeHeader {
        kid: u64::from(sender_leaf),
        ctr: counter,
    };
    let mut out = header.encode();
    out.extend_from_slice(ciphertext);
    out
}

/// Parse an SFrame wire frame.
///
/// # Errors
///
/// `JsError` on header decode failure or an oversize
/// `kid` (must fit in u32). Any 64-bit `ctr` is accepted
/// (ADR-0069).
#[wasm_bindgen(js_name = "unwrapSframeFrame")]
pub fn unwrap_sframe_frame_js(wire: &[u8]) -> Result<WasmUnwrappedFrame, JsError> {
    let (header, consumed) = lakko_calls::SframeHeader::decode(wire)
        .map_err(|e| JsError::new(&format!("call crypto error: sframe header decode: {e:?}")))?;
    let sender_leaf = u32::try_from(header.kid)
        .map_err(|_| JsError::new("call crypto error: sframe kid does not fit in u32"))?;
    Ok(WasmUnwrappedFrame {
        sender_leaf,
        counter: header.ctr,
        ciphertext: wire[consumed..].to_vec(),
    })
}

/// JS-side mirror of `UnwrappedFrame`.
#[wasm_bindgen(js_name = "UnwrappedFrame")]
pub struct WasmUnwrappedFrame {
    sender_leaf: u32,
    counter: u64,
    ciphertext: Vec<u8>,
}

#[wasm_bindgen(js_class = "UnwrappedFrame")]
impl WasmUnwrappedFrame {
    /// The sender's MLS leaf index, parsed from the SFrame header KID.
    #[wasm_bindgen(getter, js_name = "senderLeaf")]
    #[must_use]
    pub fn sender_leaf(&self) -> u32 {
        self.sender_leaf
    }

    /// The per-sender 64-bit frame counter (a `bigint` in JS), parsed from
    /// the SFrame header CTR.
    #[wasm_bindgen(getter, js_name = "counter")]
    #[must_use]
    pub fn counter(&self) -> u64 {
        self.counter
    }

    /// The frame ciphertext body (the bytes after the SFrame header).
    #[wasm_bindgen(getter, js_name = "ciphertext")]
    #[must_use]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.ciphertext.clone()
    }
}

#[cfg(test)]
mod tests {
    //! Host tests of the success paths only: a `JsError` cannot be built off
    //! the wasm target, so the refusals are covered by the web vitest suites
    //! (`apps/web/src/calls/sframe-*.test.ts`) and the core's own tests.

    use super::*;

    const GOLDEN_SEED: [u8; core::CALL_KEY_LEN] = [0x42; core::CALL_KEY_LEN];
    const GOLDEN_LEAF: u32 = 3;
    const GOLDEN_PLAINTEXT: &[u8] = b"lakko sframe golden vector v1";
    const GOLDEN_WIRE_HEX: &str =
        "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a";

    fn golden_wire() -> Vec<u8> {
        (0..GOLDEN_WIRE_HEX.len() / 2)
            .map(|i| u8::from_str_radix(&GOLDEN_WIRE_HEX[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    fn ok<T>(result: Result<T, JsError>) -> T {
        let Ok(value) = result else {
            panic!("the call was refused");
        };
        value
    }

    /// (1) The canonical cross-stack golden wire (a frame at counter 1) opens
    /// through this surface, by a participant key and by the ring.
    #[test]
    fn golden_wire_opens_to_the_plaintext() {
        let pk = ok(ok(WasmCallKey::new(&GOLDEN_SEED)).participant_key(GOLDEN_LEAF, 0));
        assert_eq!(ok(pk.open_framed(&golden_wire())), GOLDEN_PLAINTEXT);

        let header = ok(unwrap_sframe_frame_js(&golden_wire()));
        assert_eq!(header.sender_leaf(), 0x0300, "the header's whole KID");
        assert_eq!(header.counter(), 1_u64);

        let ring = ok(WasmCallKeyRing::new(&GOLDEN_SEED));
        assert_eq!(
            ok(ring.open_framed_with_overlap(0x0300, &golden_wire())),
            GOLDEN_PLAINTEXT
        );
    }

    /// (2) A frame this surface seals carries the composite KID and a 64-bit
    /// counter one above the key's random start (ADR-0069), and opens under an
    /// independently derived key. Nothing here sets a counter, so a fresh key
    /// does not seal the golden bytes.
    #[test]
    fn sealed_frame_carries_the_kid_and_a_64_bit_counter() {
        let call_key = ok(WasmCallKey::new(&GOLDEN_SEED));
        let sender = ok(call_key.participant_key(GOLDEN_LEAF, 0));
        let start: u64 = sender.counter();
        assert!(start < 1 << 62, "a key starts below 2^62");

        let wire = ok(sender.seal_framed(GOLDEN_PLAINTEXT));
        let header = ok(unwrap_sframe_frame_js(&wire));
        assert_eq!(header.sender_leaf(), 0x0300, "KID = (leaf 3 << 8) | 0");
        assert_eq!(header.counter(), start + 1);
        assert_eq!(sender.counter(), start + 1);

        let receiver = ok(call_key.participant_key(GOLDEN_LEAF, 0));
        assert_ne!(receiver.counter(), start, "starts are independent");
        assert_eq!(ok(receiver.open_framed(&wire)), GOLDEN_PLAINTEXT);
    }

    /// The raw pair and the wire helpers carry a counter that needs all 8
    /// bytes.
    #[test]
    fn raw_seal_open_and_wrap_carry_a_64_bit_counter() {
        let call_key = ok(WasmCallKey::new(&GOLDEN_SEED));
        let sender = ok(call_key.participant_key(1, 1));
        let sealed = ok(sender.seal(b"aad", b"raw"));
        let counter: u64 = sealed.counter();
        let receiver = ok(call_key.participant_key(1, 1));
        assert_eq!(
            ok(receiver.open(b"aad", &sealed.ciphertext(), counter)),
            b"raw"
        );

        let eight_byte_counter = 0x3FED_CBA9_8765_4321_u64;
        let wire = wrap_sframe_frame_js(7, eight_byte_counter, b"body");
        let header = ok(unwrap_sframe_frame_js(&wire));
        assert_eq!(header.sender_leaf(), 7);
        assert_eq!(header.counter(), eight_byte_counter);
        assert_eq!(header.ciphertext(), b"body");
    }
}
