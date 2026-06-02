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
    #[wasm_bindgen(getter, js_name = "senderLeaf")]
    #[must_use]
    pub fn sender_leaf(&self) -> u32 {
        self.sender_leaf
    }

    #[wasm_bindgen(getter, js_name = "counter")]
    #[must_use]
    pub fn counter(&self) -> u32 {
        self.counter
    }

    #[wasm_bindgen(getter, js_name = "ciphertext")]
    #[must_use]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.ciphertext.clone()
    }
}
