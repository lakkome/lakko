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
//! Since ADR-0071 the web media path seals and opens the layout
//! `clear_prefix || ciphertext_and_tag || sframe_header || trailer`
//! (`sealMediaFrame` / `openMediaFrameWithOverlap` / `parseMediaFrame`,
//! `MediaCodec`): the codec's own frame header stays in the clear so a
//! receiver without the generic frame descriptor finds key frames. The
//! header-first `sealFramed` / `openFramed` / `openFramedWithOverlap`
//! remain for the golden vector the native stacks still pin; the web
//! worker no longer calls them.
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

use std::sync::{Mutex, PoisonError};

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

    /// Seal an encoded media frame into the ADR-0071 layout,
    /// `clear_prefix || ciphertext_and_tag || sframe_header || trailer`, with
    /// the clear prefix the sender's rule gives `codec` for this frame: 10
    /// bytes of a VP8 key frame, 3 of any other VP8 frame, none for Opus or an
    /// unknown codec. This is the web worker's outgoing path. Key, nonce and
    /// counter are those of ADR-0035 / ADR-0069; the header, the clear prefix
    /// and the trailer are the AEAD additional data.
    ///
    /// # Errors
    ///
    /// `JsError` containing `CounterExhausted` when the 64-bit counter stands
    /// at its last value, else an AEAD error. Nothing is sealed on either.
    #[wasm_bindgen(js_name = "sealMediaFrame")]
    pub fn seal_media_frame(
        &self,
        frame: &[u8],
        codec: WasmMediaCodec,
    ) -> Result<Vec<u8>, JsError> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .seal_media_frame_for_codec(frame, codec.into())
            .map_err(|e| JsError::new(&format!("call crypto error: seal_media_frame: {e:?}")))
    }

    /// [`Self::seal_media_frame`] with the clear prefix length stated by the
    /// caller. A length beyond the frame is cut to the frame.
    ///
    /// # Errors
    ///
    /// `JsError` when `clear_prefix_len` exceeds 16, on counter exhaustion,
    /// or on an AEAD error. Nothing is sealed.
    #[wasm_bindgen(js_name = "sealMediaFrameWithPrefix")]
    pub fn seal_media_frame_with_prefix(
        &self,
        frame: &[u8],
        clear_prefix_len: u32,
    ) -> Result<Vec<u8>, JsError> {
        let clear = usize::try_from(clear_prefix_len).unwrap_or(usize::MAX);
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .seal_media_frame(frame, clear)
            .map_err(|e| JsError::new(&format!("call crypto error: seal_media_frame: {e:?}")))
    }

    /// Open a frame in the ADR-0071 layout and return the encoded frame
    /// `clear_prefix || plaintext`. This key must be the one derived for the
    /// frame's KID. A frame in the header-first layout is refused.
    ///
    /// # Errors
    ///
    /// `JsError` on a malformed frame, a KID that is not this key's, or an
    /// AEAD authentication failure.
    #[wasm_bindgen(js_name = "openMediaFrame")]
    pub fn open_media_frame(&self, wire: &[u8]) -> Result<Vec<u8>, JsError> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .open_media_frame(wire)
            .map_err(|e| JsError::new(&format!("call crypto error: open_media_frame: {e:?}")))
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

    /// Open a sealed media frame in the ADR-0071 layout with the overlap
    /// fall-back (ADR-0029) and return the encoded frame
    /// `clear_prefix || plaintext`. The KID is read from the frame's own
    /// SFrame header, which sits before the trailer at the end of the frame;
    /// the caller passes nothing but the bytes. The receive-side path for the
    /// web worker.
    ///
    /// # Errors
    ///
    /// `JsError` on a malformed frame, or on AEAD authentication failure under
    /// both the current and the previous key.
    #[wasm_bindgen(js_name = "openMediaFrameWithOverlap")]
    pub fn open_media_frame_with_overlap(&self, wire: &[u8]) -> Result<Vec<u8>, JsError> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_open_media_frame_with_overlap(wire)
            .map_err(|e| {
                JsError::new(&format!(
                    "call crypto error: ring open_media_frame_with_overlap: {e:?}"
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

// ============================================================
// Sealed media frame layout (ADR-0071)
// ============================================================

/// The codec of a frame, as far as the clear prefix of a sealed media frame
/// depends on it (ADR-0071). Mirrors [`lakko_calls::MediaCodec`]; the values
/// are frozen.
#[wasm_bindgen(js_name = "MediaCodec")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmMediaCodec {
    /// Any codec without a rule, and a codec the sender could not determine:
    /// nothing is left in the clear.
    Other = 0,
    /// VP8 video: 10 clear bytes of a key frame, 3 of any other frame.
    Vp8 = 1,
    /// Opus audio: nothing is left in the clear.
    Opus = 2,
    /// An RFC 2198 RED container of Opus frames (`audio/red`): nothing is
    /// left in the clear, and the trailer says the frame holds RED.
    RedOpus = 3,
}

impl From<WasmMediaCodec> for lakko_calls::MediaCodec {
    fn from(codec: WasmMediaCodec) -> Self {
        match codec {
            WasmMediaCodec::Other => Self::Other,
            WasmMediaCodec::Vp8 => Self::Vp8,
            WasmMediaCodec::Opus => Self::Opus,
            WasmMediaCodec::RedOpus => Self::RedOpus,
        }
    }
}

impl From<lakko_calls::MediaCodec> for WasmMediaCodec {
    fn from(codec: lakko_calls::MediaCodec) -> Self {
        match codec {
            lakko_calls::MediaCodec::Other => Self::Other,
            lakko_calls::MediaCodec::Vp8 => Self::Vp8,
            lakko_calls::MediaCodec::Opus => Self::Opus,
            lakko_calls::MediaCodec::RedOpus => Self::RedOpus,
        }
    }
}

/// Map a negotiated RTP codec MIME type (`"video/VP8"`, `"audio/opus"`,
/// `"audio/red"`, any case) to a `MediaCodec`. Anything else is `Other`.
#[wasm_bindgen(js_name = "mediaCodecFromMime")]
#[must_use]
pub fn media_codec_from_mime(mime: &str) -> WasmMediaCodec {
    lakko_calls::MediaCodec::from_mime(mime).into()
}

/// How many leading bytes of `frame` a sender leaves in the clear for
/// `codec` (ADR-0071). Never more than the frame holds.
#[wasm_bindgen(js_name = "mediaClearPrefixLen")]
#[must_use]
pub fn media_clear_prefix_len(codec: WasmMediaCodec, frame: &[u8]) -> u32 {
    // At most 16 by the core's bound.
    u32::try_from(lakko_calls::clear_prefix_len(codec.into(), frame)).unwrap_or(0)
}

/// Whether a VP8 frame is a key frame by its own frame tag (the low bit of
/// the first byte is 0). `undefined` for an empty frame. Only the first byte
/// is read, so a caller may pass a one-byte view.
#[wasm_bindgen(js_name = "vp8IsKeyFrame")]
#[must_use]
pub fn vp8_is_key_frame(frame: &[u8]) -> Option<bool> {
    lakko_calls::vp8_is_key_frame(frame)
}

/// The primary (current) frame of an RFC 2198 RED container, which a
/// receiver whose browser does not decode RED hands to its Opus decoder in
/// place of the container. Call it on an OPENED frame whose `MediaFrameInfo`
/// said `isRedOpus`. `undefined` when the bytes are not a container.
#[wasm_bindgen(js_name = "redPrimary")]
#[must_use]
pub fn red_primary(container: &[u8]) -> Option<Vec<u8>> {
    lakko_calls::red_primary(container).map(<[u8]>::to_vec)
}

/// Read the trailer and the SFrame header of a sealed media frame without a
/// key: the KID, the counter, the clear prefix length and whether the frame
/// holds a RED container. Nothing here is authenticated until the frame
/// opens; it is for selecting a key and for diagnostics.
///
/// # Errors
///
/// `JsError` when the bytes are not a well-formed sealed media frame.
#[wasm_bindgen(js_name = "parseMediaFrame")]
pub fn parse_media_frame(wire: &[u8]) -> Result<WasmMediaFrameInfo, JsError> {
    media_frame_info(wire)
        .map_err(|e| JsError::new(&format!("call crypto error: media frame: {e}")))
}

fn media_frame_info(wire: &[u8]) -> Result<WasmMediaFrameInfo, lakko_calls::MediaFrameError> {
    let parts = lakko_calls::MediaFrameParts::split(wire)?;
    Ok(WasmMediaFrameInfo {
        kid: parts.header.kid,
        counter: parts.header.ctr,
        // At most 16 by the split's bound.
        clear_prefix_len: u32::try_from(parts.clear_prefix.len()).unwrap_or(0),
        red_opus: parts.content == lakko_calls::MediaContent::RedOpus,
    })
}

/// What [`parse_media_frame`] reads off a sealed media frame.
#[wasm_bindgen(js_name = "MediaFrameInfo")]
pub struct WasmMediaFrameInfo {
    kid: u64,
    counter: u64,
    clear_prefix_len: u32,
    red_opus: bool,
}

#[wasm_bindgen(js_class = "MediaFrameInfo")]
impl WasmMediaFrameInfo {
    /// The whole SFrame KID `(sender_leaf << 8) | stream_index` (ADR-0035), a
    /// `bigint` in JS.
    #[wasm_bindgen(getter, js_name = "kid")]
    #[must_use]
    pub fn kid(&self) -> u64 {
        self.kid
    }

    /// The 64-bit frame counter (a `bigint` in JS).
    #[wasm_bindgen(getter, js_name = "counter")]
    #[must_use]
    pub fn counter(&self) -> u64 {
        self.counter
    }

    /// How many bytes at the start of the frame are in the clear.
    #[wasm_bindgen(getter, js_name = "clearPrefixLen")]
    #[must_use]
    pub fn clear_prefix_len(&self) -> u32 {
        self.clear_prefix_len
    }

    /// The trailer says the frame holds an RFC 2198 RED container of Opus.
    /// Authenticated once the frame opens (the trailer is additional data).
    #[wasm_bindgen(getter, js_name = "isRedOpus")]
    #[must_use]
    pub fn is_red_opus(&self) -> bool {
        self.red_opus
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

    // ---- ADR-0071 sealed media frame ---------------------------------------

    const GOLDEN_MEDIA_FRAME_HEX: &str =
        "1002009d012a8002e0012dc56b47f2bfd3ebf051d8893d6d2b2b355f29f3980b60\
         e19c2555c9ad3a6ca9e95f08b228768c0f59a6509103010a0301";

    fn golden_media_frame() -> Vec<u8> {
        (0..GOLDEN_MEDIA_FRAME_HEX.len() / 2)
            .map(|i| u8::from_str_radix(&GOLDEN_MEDIA_FRAME_HEX[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    fn vp8_key_frame() -> Vec<u8> {
        let mut frame = vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01];
        frame.extend_from_slice(b"lakko media frame golden v1");
        frame
    }

    /// The core's golden media frame (leaf 3, stream 1, counter 1) opens
    /// through both wasm open paths.
    #[test]
    fn golden_media_frame_opens_to_the_frame() {
        let wire = golden_media_frame();
        let pk = ok(ok(WasmCallKey::new(&GOLDEN_SEED)).participant_key(GOLDEN_LEAF, 1));
        assert_eq!(ok(pk.open_media_frame(&wire)), vp8_key_frame());
        let ring = ok(WasmCallKeyRing::new(&GOLDEN_SEED));
        assert_eq!(
            ok(ring.open_media_frame_with_overlap(&wire)),
            vp8_key_frame()
        );
        let info = media_frame_info(&wire).expect("well-formed");
        assert_eq!(info.kid(), 0x0301);
        assert_eq!(info.counter(), 1);
        assert_eq!(info.clear_prefix_len(), 10);
    }

    /// Seal through the wasm surface: the VP8 header stays at the start, the
    /// KID and a 64-bit counter sit in the header at the end, and the frame
    /// opens through the ring.
    #[test]
    fn sealed_media_frame_keeps_the_codec_header_and_opens() {
        let ring = ok(WasmCallKeyRing::new(&GOLDEN_SEED));
        let sender = ok(ring.participant_key(GOLDEN_LEAF, 1));
        let key_frame = vp8_key_frame();
        let delta_frame = [&[0x11u8, 0x02, 0x00][..], b"delta"].concat();

        let start = sender.counter();
        let key_wire = ok(sender.seal_media_frame(&key_frame, WasmMediaCodec::Vp8));
        let delta_wire = ok(sender.seal_media_frame(&delta_frame, WasmMediaCodec::Vp8));
        assert_eq!(&key_wire[..10], &key_frame[..10]);
        assert_eq!(&delta_wire[..3], &delta_frame[..3]);
        assert_eq!(vp8_is_key_frame(&key_wire), Some(true));
        assert_eq!(vp8_is_key_frame(&delta_wire), Some(false));

        let key_info = media_frame_info(&key_wire).expect("well-formed");
        assert_eq!(key_info.kid(), 0x0301);
        assert_eq!(key_info.counter(), start + 1);
        assert_eq!(key_info.clear_prefix_len(), 10);
        let delta_info = media_frame_info(&delta_wire).expect("well-formed");
        assert_eq!(delta_info.counter(), start + 2);
        assert_eq!(delta_info.clear_prefix_len(), 3);

        assert_eq!(ok(ring.open_media_frame_with_overlap(&key_wire)), key_frame);
        assert_eq!(
            ok(ring.open_media_frame_with_overlap(&delta_wire)),
            delta_frame
        );
    }

    /// Opus and an unknown codec leave nothing clear; an explicit prefix is
    /// honoured and cut to the frame.
    #[test]
    fn media_frame_prefix_per_codec_and_explicit() {
        let call_key = ok(WasmCallKey::new(&GOLDEN_SEED));
        let audio = ok(call_key.participant_key(GOLDEN_LEAF, 0));
        let receiver = ok(call_key.participant_key(GOLDEN_LEAF, 0));
        for codec in [WasmMediaCodec::Opus, WasmMediaCodec::Other] {
            let wire = ok(audio.seal_media_frame(b"\x78opus", codec));
            assert_eq!(media_frame_info(&wire).expect("ok").clear_prefix_len(), 0);
            assert_eq!(ok(receiver.open_media_frame(&wire)), b"\x78opus");
        }
        let wire = ok(audio.seal_media_frame_with_prefix(&[1, 2, 3], 10));
        assert_eq!(media_frame_info(&wire).expect("ok").clear_prefix_len(), 3);
        assert_eq!(ok(receiver.open_media_frame(&wire)), [1, 2, 3]);

        assert_eq!(media_codec_from_mime("video/VP8"), WasmMediaCodec::Vp8);
        assert_eq!(media_codec_from_mime("audio/opus"), WasmMediaCodec::Opus);
        assert_eq!(media_codec_from_mime("video/H264"), WasmMediaCodec::Other);
        assert_eq!(
            media_clear_prefix_len(WasmMediaCodec::Vp8, &vp8_key_frame()),
            10
        );
        assert_eq!(
            media_clear_prefix_len(WasmMediaCodec::Vp8, &[0x11, 0, 0, 9]),
            3
        );
        assert_eq!(
            media_clear_prefix_len(WasmMediaCodec::Other, &vp8_key_frame()),
            0
        );
        assert_eq!(vp8_is_key_frame(&[]), None);
    }

    /// A RED container seals with the trailer saying so, opens whole, and
    /// its primary frame comes out of `redPrimary`.
    #[test]
    fn a_red_frame_is_marked_and_its_primary_can_be_taken() {
        assert_eq!(media_codec_from_mime("audio/red"), WasmMediaCodec::RedOpus);
        let call_key = ok(WasmCallKey::new(&GOLDEN_SEED));
        let sender = ok(call_key.participant_key(GOLDEN_LEAF, 0));
        let receiver = ok(call_key.participant_key(GOLDEN_LEAF, 0));
        let mut container = vec![0xef, 0x0f, 0x00, 0x02, 0x6f, 0xaa, 0xbb];
        container.extend_from_slice(b"primary");
        let wire = ok(sender.seal_media_frame(&container, WasmMediaCodec::RedOpus));
        let info = media_frame_info(&wire).expect("ok");
        assert!(info.is_red_opus());
        assert_eq!(info.clear_prefix_len(), 0);
        let opened = ok(receiver.open_media_frame(&wire));
        assert_eq!(opened, container);
        assert_eq!(red_primary(&opened), Some(b"primary".to_vec()));
        let plain = ok(sender.seal_media_frame(b"opus", WasmMediaCodec::Opus));
        assert!(!media_frame_info(&plain).expect("ok").is_red_opus());
    }

    /// The parser refuses a header-first frame and garbage without a key.
    #[test]
    fn media_frame_info_refuses_malformed_input() {
        assert!(media_frame_info(&[]).is_err());
        assert!(media_frame_info(&[0u8; 40]).is_err());
        let mut truncated = golden_media_frame();
        truncated.pop();
        assert!(media_frame_info(&truncated).is_err());
    }
}
