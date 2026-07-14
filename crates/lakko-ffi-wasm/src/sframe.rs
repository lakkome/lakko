// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for SFrame call-key management
//! (G3-sframe step 1 — web mirror of
//! `crates/lakko-ffi-uniffi/src/sframe.rs`).
//!
//! Mirrors the UniFFI shape:
//!
//! - [`WasmCallKey`] (JS `CallKey`) — `new(seed: Uint8Array)`
//!   + `participantKey(sender_leaf: u32) → ParticipantKey`
//!   + `seedBytes() → Uint8Array`.
//! - [`WasmParticipantKey`] (JS `ParticipantKey`) — `seal(aad,
//!   plaintext) → SealedFrame`, `open(aad, ciphertext,
//!   counter) → Uint8Array`, plus `senderLeaf()` and
//!   `counter()` getters.
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

    /// Derive the [`WasmParticipantKey`] for the supplied
    /// leaf index.
    ///
    /// # Errors
    ///
    /// `JsError` on HKDF failure (practically unreachable
    /// with valid inputs).
    #[wasm_bindgen(js_name = "participantKey")]
    pub fn participant_key(&self, sender_leaf: u32) -> Result<WasmParticipantKey, JsError> {
        let pk = self
            .inner
            .participant_key(sender_leaf)
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

    /// Current frame counter value. Diagnostics only.
    #[wasm_bindgen(js_name = "counter")]
    #[must_use]
    pub fn counter(&self) -> u32 {
        self.inner.lock().expect("sframe lock").counter()
    }

    /// Seal `plaintext` under the next counter value.
    /// Returns [`WasmSealedFrame { counter, ciphertext }`].
    ///
    /// # Errors
    ///
    /// `JsError` containing `"counter exhausted"` when the
    /// 32-bit counter rolls over; the caller MUST trigger
    /// a fresh MLS commit and re-derive the `CallKey`
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

    /// Open `ciphertext` using `counter` from the SFrame
    /// header.
    ///
    /// # Errors
    ///
    /// `JsError` on AEAD authentication failure (wrong
    /// key / wrong AAD / tampered ciphertext / wrong
    /// counter).
    #[wasm_bindgen(js_name = "open")]
    pub fn open(&self, aad: &[u8], ciphertext: &[u8], counter: u32) -> Result<Vec<u8>, JsError> {
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
    /// `JsError` containing `"counter exhausted"` when the 32-bit
    /// counter rolls over (re-key required), else an AEAD error.
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
    /// this key's leaf, a counter out of range, or AEAD auth
    /// failure.
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

    /// Derive the seal-side [`WasmParticipantKey`] for `sender_leaf` from the
    /// current epoch. Re-fetched after a `rekeyFromSeedAt` to seal under the
    /// new key.
    ///
    /// # Errors
    ///
    /// `JsError` on HKDF failure (practically unreachable).
    #[wasm_bindgen(js_name = "participantKey")]
    pub fn participant_key(&self, sender_leaf: u32) -> Result<WasmParticipantKey, JsError> {
        let pk = self
            .inner
            .lock()
            .expect("call-key ring lock")
            .participant_key(sender_leaf)
            .map_err(|e| {
                JsError::new(&format!("call crypto error: ring participant_key: {e:?}"))
            })?;
        Ok(WasmParticipantKey {
            inner: Mutex::new(pk),
        })
    }

    /// Open an RFC 9605 framed SFrame with the overlap fall-back: try the
    /// current key for `sender_leaf`, and on an AEAD failure fall back to the
    /// retained previous key while its window is live (ADR-0029). The
    /// receive-side path for the web worker across a mid-call rekey.
    ///
    /// # Errors
    ///
    /// `JsError` on a malformed header, or AEAD auth failure under both the
    /// current and previous keys.
    #[wasm_bindgen(js_name = "openFramedWithOverlap")]
    pub fn open_framed_with_overlap(
        &self,
        sender_leaf: u32,
        wire: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        self.inner
            .lock()
            .expect("call-key ring lock")
            .try_open_framed_with_overlap(sender_leaf, wire)
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
    counter: u32,
    ciphertext: Vec<u8>,
}

#[wasm_bindgen(js_class = "SealedFrame")]
impl WasmSealedFrame {
    /// Frame counter the seal consumed. Goes into the
    /// SFrame header.
    #[wasm_bindgen(getter, js_name = "counter")]
    #[must_use]
    pub fn counter(&self) -> u32 {
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
/// helper at `crates/lakko-ffi-uniffi/src/sframe.rs`.
#[wasm_bindgen(js_name = "wrapSframeFrame")]
#[must_use]
pub fn wrap_sframe_frame_js(sender_leaf: u32, counter: u32, ciphertext: &[u8]) -> Vec<u8> {
    let header = lakko_calls::SframeHeader {
        kid: u64::from(sender_leaf),
        ctr: u64::from(counter),
    };
    let mut out = header.encode();
    out.extend_from_slice(ciphertext);
    out
}

/// Parse an SFrame wire frame.
///
/// # Errors
///
/// `JsError` on header decode failure or oversize
/// `kid` / `ctr` (must fit in u32).
#[wasm_bindgen(js_name = "unwrapSframeFrame")]
pub fn unwrap_sframe_frame_js(wire: &[u8]) -> Result<WasmUnwrappedFrame, JsError> {
    let (header, consumed) = lakko_calls::SframeHeader::decode(wire)
        .map_err(|e| JsError::new(&format!("call crypto error: sframe header decode: {e:?}")))?;
    let sender_leaf = u32::try_from(header.kid)
        .map_err(|_| JsError::new("call crypto error: sframe kid does not fit in u32"))?;
    let counter = u32::try_from(header.ctr)
        .map_err(|_| JsError::new("call crypto error: sframe ctr does not fit in u32"))?;
    Ok(WasmUnwrappedFrame {
        sender_leaf,
        counter,
        ciphertext: wire[consumed..].to_vec(),
    })
}

/// JS-side mirror of `UnwrappedFrame`.
#[wasm_bindgen(js_name = "UnwrappedFrame")]
pub struct WasmUnwrappedFrame {
    sender_leaf: u32,
    counter: u32,
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

    /// The per-sender frame counter, parsed from the SFrame header CTR.
    #[wasm_bindgen(getter, js_name = "counter")]
    #[must_use]
    pub fn counter(&self) -> u32 {
        self.counter
    }

    /// The frame ciphertext body (the bytes after the SFrame header).
    #[wasm_bindgen(getter, js_name = "ciphertext")]
    #[must_use]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.ciphertext.clone()
    }
}
