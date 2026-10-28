// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Flat C ABI around `lakko_calls::sframe` (G3-sframe-native N-1).
//!
//! This crate is the bottom of the SFrame native-shim stack. It is
//! **not** for direct application use — the iOS and Android client
//! code consumes either `crates/lakko-ffi-uniffi/src/sframe.rs` (the
//! UniFFI surface that wraps `Mutex<ParticipantKey>` into a Swift /
//! Kotlin Object) or, when the platform's WebRTC frame transformer
//! demands a native `webrtc::FrameEncryptorInterface*` pointer (which
//! the Java / Obj-C wrappers do **not** expose), the per-platform
//! C++ shim N-3 that subclasses `FrameEncryptorInterface` and
//! trampolines back through this very C ABI.
//!
//! Why a hand-rolled `extern "C"` layer in addition to UniFFI:
//!
//! - UniFFI's generated JNI / Swift surfaces add a runtime FFI round
//!   trip per call (RustBuffer marshalling + ARC reference counting
//!   on Swift, JNI mode-switching + global-ref bookkeeping on
//!   Android). The codec thread that runs WebRTC's frame transformer
//!   processes audio frames every 10 ms and video frames every 33 ms;
//!   the extra round-trip cost there is observable.
//! - The C++ subclass of `webrtc::FrameEncryptorInterface` only ever
//!   needs a flat C ABI — passing `Mutex<ParticipantKey>` through
//!   Java first just to come back to a C++ pointer is gratuitous.
//!
//! ## Two frame layouts (ADR-0071)
//!
//! The media path of every client uses the ADR-0071 layout, which leaves
//! the codec's frame header in the clear and puts the SFrame header at
//! the end: [`lakko_sframe_media_clear_prefix_len`],
//! [`lakko_sframe_seal_media_frame`] and
//! [`lakko_sframe_call_key_ring_open_media_frame_with_overlap`]. The C++
//! bridge shared by iOS and Android calls only these three for a frame.
//! The header-first functions (`_seal_framed`, `_open_framed`,
//! `_call_key_ring_open_framed_with_overlap`) are still exported and
//! unchanged, have no caller on a media path, and do not open a frame of
//! the other layout.
//!
//! ## Memory ownership
//!
//! Every function that returns a pointer (`*mut CallKey`,
//! `*mut ParticipantKey`, or `*mut u8` ciphertext / plaintext buffer)
//! transfers ownership to the caller. The caller MUST free it via
//! the matching `_free` symbol:
//!
//! - [`lakko_sframe_call_key_free`] for `*mut CallKey`,
//! - [`lakko_sframe_participant_key_free`] for `*mut ParticipantKey`,
//! - [`lakko_sframe_buffer_free`] for `*mut u8` buffers produced by
//!   [`lakko_sframe_seal`] / [`lakko_sframe_open`] /
//!   [`lakko_sframe_wrap_frame`] / [`lakko_sframe_unwrap_frame`].
//!
//! Mixing free routines (calling `free()` from libc on a Rust-
//! produced buffer, or calling [`lakko_sframe_buffer_free`] on a
//! malloc'd buffer) is undefined behaviour — the upstream callers
//! are short enough that the matching pairs are auditable on first
//! read.
//!
//! ## Status codes
//!
//! Every function that can fail returns a [`LakkoSframeStatus`]
//! enum (negative values reserved for future surface bumps; current
//! variants are non-negative). Callers branch on the enum, not on
//! the integer width — cbindgen emits a `typedef` so the C side has
//! a stable name.
//!
//! ## Safety
//!
//! The crate carries `#![forbid(unsafe_code)]` would block the
//! `extern "C"` boundary, so we relax to `#![deny(unsafe_op_in_unsafe_fn)]`
//! and document every `unsafe { ... }` block with a `// SAFETY:`
//! comment per the workspace's CLAUDE.md unsafe-allowance rule for
//! FFI crates.

// FFI surface — every function below is `extern "C"`,
// which inherently requires `unsafe { ... }` blocks at
// the boundary. The workspace-wide `unsafe_code = "deny"`
// lint is overridden here exactly like the rationale in
// `CLAUDE.md`'s "FFI exception": every `unsafe { ... }`
// block carries a `// SAFETY:` comment naming the
// invariant the caller upholds.
#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]
// `out_<thing>_ptr` / `out_<thing>_len` pairs in the C ABI
// signatures naturally trip `clippy::similar_names`. Renaming
// would break the conventional `(ptr, len)` pairing the C
// header surfaces to the platform shims — keep the canonical
// shape, silence the lint.
#![allow(clippy::similar_names)]
// `Vec::from_raw_parts(ptr, len, len)` inside
// `lakko_sframe_buffer_free` matches the symmetric
// `vec_into_raw` which normalises capacity to length;
// `from_raw_parts` with `len == cap` is exactly the
// contract we want, not the lint's suggested alternative.
#![allow(clippy::same_length_and_capacity)]

use std::sync::Mutex;

use lakko_calls::sframe as core;
use lakko_calls::{
    CallKeyRing as CoreRing, Error as CallsError, MediaCodec, MediaFrameParts, SframeHeader,
};

// ---------------------------------------------------------------------
// Status codes
// ---------------------------------------------------------------------

/// Return value of every fallible C ABI function. `Ok` is `0`;
/// every error is a distinct non-zero variant so a future metrics
/// layer on either side of the FFI can count categories without
/// re-parsing prose.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LakkoSframeStatus {
    /// Operation completed; out-pointers are populated.
    Ok = 0,
    /// One of the input pointers was null where it should not be.
    NullPointer = 1,
    /// Seed length was not exactly [`core::CALL_KEY_LEN`] bytes (32).
    BadSeedLength = 2,
    /// HKDF / per-participant key derivation failed (practically
    /// unreachable with valid inputs).
    KeyDerivation = 3,
    /// The 64-bit frame counter stands at its last value
    /// (ADR-0069); nothing was sealed and the counter does not
    /// wrap. The caller derives a new key before continuing.
    /// Re-using a nonce under the same key is an AEAD-fatal
    /// mistake.
    CounterExhausted = 4,
    /// AEAD seal or open returned an error (wrong key, wrong AAD,
    /// tampered ciphertext, wrong counter).
    AeadFailed = 5,
    /// SFrame wire header failed `SframeHeader::decode` (truncated
    /// or reserved bit set).
    WireParse = 6,
    /// Parsed `kid` exceeds `u32::MAX`. Lakko's call-keying layer
    /// pins `sender_leaf` to `u32`; a wider value is malformed or
    /// from a future schema bump this binary does not know how to
    /// dispatch.
    KidOverflow = 7,
    /// No longer returned. Before ADR-0069 a parsed `ctr` above
    /// `u32::MAX` was refused; the counter is 64 bits now and every
    /// value the header can carry is accepted. The variant keeps its
    /// number so the status codes do not shift.
    CtrOverflow = 8,
    /// [`lakko_sframe_seal_media_frame`] was asked to leave more than
    /// [`LAKKO_SFRAME_MAX_CLEAR_PREFIX_LEN`] bytes in the clear
    /// (ADR-0071). Nothing was sealed.
    BadClearPrefix = 9,
}

// ---------------------------------------------------------------------
// Opaque handle types
// ---------------------------------------------------------------------

/// Opaque per-call shared key. The 32-byte exporter output
/// (`Group::export_secret("lakko/call/v1", call_id, 32)`) is bound
/// inside.
///
/// Construct via [`lakko_sframe_call_key_new`]; free via
/// [`lakko_sframe_call_key_free`].
pub struct CallKey {
    inner: core::CallKey,
}

/// Opaque per-participant AEAD key + per-participant nonce salt +
/// per-participant frame counter.
///
/// Construct via [`lakko_sframe_participant_key_new`]; free via
/// [`lakko_sframe_participant_key_free`].
pub struct ParticipantKey {
    inner: core::ParticipantKey,
}

// ---------------------------------------------------------------------
// CallKey lifecycle
// ---------------------------------------------------------------------

/// Construct a [`CallKey`] from a 32-byte seed.
///
/// On success `*out_call_key` is populated with a freshly-allocated
/// pointer the caller owns. On failure `*out_call_key` is left
/// untouched and a non-`Ok` status is returned.
///
/// # Safety
///
/// - `seed_ptr` must point to at least `seed_len` valid bytes for
///   the duration of the call.
/// - `out_call_key` must be a writable pointer to a `*mut CallKey`
///   storage cell.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_call_key_new(
    seed_ptr: *const u8,
    seed_len: usize,
    out_call_key: *mut *mut CallKey,
) -> LakkoSframeStatus {
    if seed_ptr.is_null() || out_call_key.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if seed_len != core::CALL_KEY_LEN {
        return LakkoSframeStatus::BadSeedLength;
    }
    // SAFETY: caller upheld pointer + length validity per the
    // `# Safety` doc above; the slice we materialise lives only for
    // the duration of the `copy_from_slice` below.
    let seed_slice = unsafe { std::slice::from_raw_parts(seed_ptr, seed_len) };
    let mut seed = [0_u8; core::CALL_KEY_LEN];
    seed.copy_from_slice(seed_slice);

    let boxed = Box::new(CallKey {
        inner: core::CallKey::from_seed(seed),
    });
    // SAFETY: `out_call_key` non-null + writable per the caller's
    // contract.
    unsafe {
        *out_call_key = Box::into_raw(boxed);
    }
    LakkoSframeStatus::Ok
}

/// Free a [`CallKey`] previously returned by
/// [`lakko_sframe_call_key_new`]. Passing `null` is a no-op (matching
/// libc `free` semantics).
///
/// # Safety
///
/// - `call_key` must either be null or a pointer this crate produced
///   via [`lakko_sframe_call_key_new`]. Passing a pointer that
///   originated elsewhere (e.g. from libc `malloc`) is undefined
///   behaviour.
/// - The same pointer must NOT be freed twice (double-free
///   protection lives in the caller's bookkeeping — this is C ABI,
///   not Rust).
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_call_key_free(call_key: *mut CallKey) {
    if call_key.is_null() {
        return;
    }
    // SAFETY: caller upheld the matched-pair contract per the
    // `# Safety` doc above.
    let _ = unsafe { Box::from_raw(call_key) };
}

// ---------------------------------------------------------------------
// ParticipantKey lifecycle
// ---------------------------------------------------------------------

/// Derive the seal-side [`ParticipantKey`] for `sender_leaf`'s
/// `stream_index` stream from `call_key` (ADR-0035). The KID is
/// composed `(sender_leaf << 8) | stream_index` and folded whole into
/// the HKDF, so every one of a participant's concurrent streams
/// (`0` = audio, `1` = first video, `2` = second video / screen-share,
/// …) keys into a disjoint `(key, nonce_salt)` space. This is the
/// ENCRYPTOR path — a `FrameEncryptor` bound to one local send track
/// passes that track's stream ordinal; the DECRYPTOR reads the KID
/// from each frame's header instead (see
/// [`lakko_sframe_call_key_ring_open_framed_with_overlap`]) and needs
/// no stream argument. A `stream_index` is a `u8`, so every value is
/// valid (`0..=255` fits the KID's 8-bit stream field) — there is no
/// fail-closed rejection here.
///
/// The key's frame counter starts at a uniformly random value in
/// `[0, 2^62)` (ADR-0069), so deriving a key again for the same
/// `(call key, leaf, stream)` is safe. No function of this ABI sets
/// or resets a counter.
///
/// # Safety
///
/// - `call_key` must be a live pointer from
///   [`lakko_sframe_call_key_new`].
/// - `out_participant_key` must be a writable pointer to a
///   `*mut ParticipantKey` storage cell.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_participant_key_new(
    call_key: *const CallKey,
    sender_leaf: u32,
    stream_index: u8,
    out_participant_key: *mut *mut ParticipantKey,
) -> LakkoSframeStatus {
    if call_key.is_null() || out_participant_key.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: caller passed a live pointer from `_call_key_new`; we
    // borrow it for the duration of `participant_key_for`.
    let ck = unsafe { &*call_key };
    let Ok(pk) = ck
        .inner
        .participant_key_for(sender_leaf, u32::from(stream_index))
    else {
        return LakkoSframeStatus::KeyDerivation;
    };
    let boxed = Box::new(ParticipantKey { inner: pk });
    // SAFETY: out-pointer non-null per the doc above.
    unsafe {
        *out_participant_key = Box::into_raw(boxed);
    }
    LakkoSframeStatus::Ok
}

/// Free a [`ParticipantKey`] previously returned by
/// [`lakko_sframe_participant_key_new`]. Passing `null` is a no-op.
///
/// # Safety
///
/// Same matched-pair contract as
/// [`lakko_sframe_call_key_free`].
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_participant_key_free(participant_key: *mut ParticipantKey) {
    if participant_key.is_null() {
        return;
    }
    // SAFETY: caller upheld the matched-pair contract.
    let _ = unsafe { Box::from_raw(participant_key) };
}

/// Borrow the sender-leaf index baked into a [`ParticipantKey`].
/// Returns `u32::MAX` if `participant_key` is null (sentinel rather
/// than a status code — this getter has no other failure mode).
///
/// # Safety
///
/// `participant_key` must either be null or a live pointer from
/// [`lakko_sframe_participant_key_new`].
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_participant_key_sender_leaf(
    participant_key: *const ParticipantKey,
) -> u32 {
    if participant_key.is_null() {
        return u32::MAX;
    }
    // SAFETY: caller upheld the live-pointer contract.
    unsafe { (*participant_key).inner.sender_leaf() }
}

/// Borrow the current 64-bit frame counter — diagnostic only; the
/// canonical increment lives inside [`lakko_sframe_seal`]. It is the
/// counter of the last frame sealed, or the key's random start
/// (ADR-0069) when none was. Returns `u64::MAX` if `participant_key`
/// is null.
///
/// # Safety
///
/// Same as [`lakko_sframe_participant_key_sender_leaf`].
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_participant_key_counter(
    participant_key: *const ParticipantKey,
) -> u64 {
    if participant_key.is_null() {
        return u64::MAX;
    }
    // SAFETY: caller upheld the live-pointer contract.
    unsafe { (*participant_key).inner.counter() }
}

// ---------------------------------------------------------------------
// AEAD seal / open
// ---------------------------------------------------------------------

/// Seal `plaintext` under the participant key's next counter.
/// Writes `*out_counter` to the consumed 64-bit counter value and
/// `*out_ciphertext_ptr` / `*out_ciphertext_len` to a freshly-allocated
/// buffer containing the AEAD-sealed bytes. The caller MUST free the
/// buffer via [`lakko_sframe_buffer_free`].
///
/// # Safety
///
/// - `participant_key` must be a live pointer from
///   [`lakko_sframe_participant_key_new`].
/// - `aad_ptr` may be null only when `aad_len == 0`; otherwise
///   `aad_len` valid bytes must be readable.
/// - `plaintext_ptr` may be null only when `plaintext_len == 0`;
///   otherwise `plaintext_len` valid bytes must be readable.
/// - The four `out_*` pointers must be writable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_seal(
    participant_key: *mut ParticipantKey,
    aad_ptr: *const u8,
    aad_len: usize,
    plaintext_ptr: *const u8,
    plaintext_len: usize,
    out_counter: *mut u64,
    out_ciphertext_ptr: *mut *mut u8,
    out_ciphertext_len: *mut usize,
) -> LakkoSframeStatus {
    if participant_key.is_null()
        || out_counter.is_null()
        || out_ciphertext_ptr.is_null()
        || out_ciphertext_len.is_null()
    {
        return LakkoSframeStatus::NullPointer;
    }
    if aad_ptr.is_null() && aad_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    if plaintext_ptr.is_null() && plaintext_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: pointer + length guarded above. Empty slices for
    // null-with-zero-length cases are well-defined.
    let aad = if aad_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(aad_ptr, aad_len) }
    };
    let plaintext = if plaintext_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(plaintext_ptr, plaintext_len) }
    };

    // SAFETY: caller upheld the live-pointer contract on
    // `participant_key`.
    let pk = unsafe { &mut *participant_key };
    let (counter, ciphertext) = match pk.inner.seal(aad, plaintext) {
        Ok(out) => out,
        Err(CallsError::CounterExhausted) => return LakkoSframeStatus::CounterExhausted,
        Err(_) => return LakkoSframeStatus::AeadFailed,
    };

    let (ct_ptr, ct_len) = vec_into_raw(ciphertext);
    // SAFETY: out-pointers non-null per the guard above.
    unsafe {
        *out_counter = counter;
        *out_ciphertext_ptr = ct_ptr;
        *out_ciphertext_len = ct_len;
    }
    LakkoSframeStatus::Ok
}

/// Open `ciphertext` against the participant key, using the
/// caller-supplied 64-bit `counter` (from the SFrame wire header) to
/// re-derive the AEAD nonce. Writes the recovered plaintext into a
/// freshly-allocated buffer the caller MUST free via
/// [`lakko_sframe_buffer_free`].
///
/// # Safety
///
/// Same shape as [`lakko_sframe_seal`].
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_open(
    participant_key: *const ParticipantKey,
    aad_ptr: *const u8,
    aad_len: usize,
    ciphertext_ptr: *const u8,
    ciphertext_len: usize,
    counter: u64,
    out_plaintext_ptr: *mut *mut u8,
    out_plaintext_len: *mut usize,
) -> LakkoSframeStatus {
    if participant_key.is_null() || out_plaintext_ptr.is_null() || out_plaintext_len.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if aad_ptr.is_null() && aad_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    if ciphertext_ptr.is_null() && ciphertext_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: pointer + length guarded.
    let aad = if aad_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(aad_ptr, aad_len) }
    };
    let ciphertext = if ciphertext_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ciphertext_ptr, ciphertext_len) }
    };

    // SAFETY: caller upheld live-pointer contract.
    let pk = unsafe { &*participant_key };
    let Ok(plaintext) = pk.inner.open(aad, ciphertext, counter) else {
        return LakkoSframeStatus::AeadFailed;
    };

    let (pt_ptr, pt_len) = vec_into_raw(plaintext);
    // SAFETY: out-pointers non-null per the guard.
    unsafe {
        *out_plaintext_ptr = pt_ptr;
        *out_plaintext_len = pt_len;
    }
    LakkoSframeStatus::Ok
}

// ---------------------------------------------------------------------
// Framed seal / open — ADR-0013 header-as-AAD wire frames
// ---------------------------------------------------------------------

/// Seal `plaintext` into a complete RFC 9605 wire frame —
/// `header(kid = sender_leaf, ctr) || ciphertext` — with the SFrame
/// header bound as the AEAD AAD, per
/// [ADR-0013](../../../docs/adr/0013-sframe-cross-platform-aad.md).
/// This is the cross-platform path: a web `RTCRtpScriptTransform`
/// worker sealing under the same contract produces frames this binary
/// opens, and vice versa — no host-specific `additional_data` enters
/// the AEAD. Wraps [`core::ParticipantKey::seal_framed`] so the
/// header-as-AAD logic lives in exactly one place.
///
/// Writes the wire frame into a freshly-allocated buffer the caller
/// MUST free via [`lakko_sframe_buffer_free`].
///
/// # Safety
///
/// - `participant_key` must be a live pointer from
///   [`lakko_sframe_participant_key_new`].
/// - `plaintext_ptr` may be null only when `plaintext_len == 0`;
///   otherwise `plaintext_len` valid bytes must be readable.
/// - `out_wire_ptr` / `out_wire_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_seal_framed(
    participant_key: *mut ParticipantKey,
    plaintext_ptr: *const u8,
    plaintext_len: usize,
    out_wire_ptr: *mut *mut u8,
    out_wire_len: *mut usize,
) -> LakkoSframeStatus {
    if participant_key.is_null() || out_wire_ptr.is_null() || out_wire_len.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if plaintext_ptr.is_null() && plaintext_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: pointer + length guarded above.
    let plaintext = if plaintext_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(plaintext_ptr, plaintext_len) }
    };
    // SAFETY: caller upheld the live-pointer contract.
    let pk = unsafe { &mut *participant_key };
    let wire = match pk.inner.seal_framed(plaintext) {
        Ok(w) => w,
        Err(CallsError::CounterExhausted) => return LakkoSframeStatus::CounterExhausted,
        Err(_) => return LakkoSframeStatus::AeadFailed,
    };
    let (ptr, len) = vec_into_raw(wire);
    // SAFETY: out-pointers non-null per the guard above.
    unsafe {
        *out_wire_ptr = ptr;
        *out_wire_len = len;
    }
    LakkoSframeStatus::Ok
}

/// Open a complete RFC 9605 wire frame produced by
/// [`lakko_sframe_seal_framed`] (or any ADR-0013-conformant peer,
/// including the web worker): parse the header, bind the exact
/// on-wire header bytes as the AEAD AAD, and AEAD-open the trailing
/// ciphertext. Wraps [`core::ParticipantKey::open_framed`].
///
/// `participant_key` must be the key derived for the frame's sender
/// leaf (the platform shim selects it by the header's KID before
/// calling). Every failure mode — malformed header, KID that doesn't
/// match this key's leaf, AEAD authentication failure — collapses to
/// [`LakkoSframeStatus::AeadFailed`]; the shim drops the frame
/// (`kFailedToDecrypt`) on any of them, so the granularity is not
/// load-bearing here.
///
/// Writes the recovered plaintext into a freshly-allocated buffer the
/// caller MUST free via [`lakko_sframe_buffer_free`].
///
/// # Safety
///
/// - `participant_key` must be a live pointer from
///   [`lakko_sframe_participant_key_new`].
/// - `wire_ptr` may be null only when `wire_len == 0`.
/// - `out_plaintext_ptr` / `out_plaintext_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_open_framed(
    participant_key: *const ParticipantKey,
    wire_ptr: *const u8,
    wire_len: usize,
    out_plaintext_ptr: *mut *mut u8,
    out_plaintext_len: *mut usize,
) -> LakkoSframeStatus {
    if participant_key.is_null() || out_plaintext_ptr.is_null() || out_plaintext_len.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if wire_ptr.is_null() && wire_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: pointer + length guarded above.
    let wire = if wire_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(wire_ptr, wire_len) }
    };
    // SAFETY: caller upheld the live-pointer contract.
    let pk = unsafe { &*participant_key };
    let Ok(plaintext) = pk.inner.open_framed(wire) else {
        return LakkoSframeStatus::AeadFailed;
    };
    let (ptr, len) = vec_into_raw(plaintext);
    // SAFETY: out-pointers non-null per the guard above.
    unsafe {
        *out_plaintext_ptr = ptr;
        *out_plaintext_len = len;
    }
    LakkoSframeStatus::Ok
}

// ---------------------------------------------------------------------
// CallKeyRing — mid-call overlap keyer (G-18, ADR-0029)
// ---------------------------------------------------------------------

/// Opaque overlap-window call-key ring — the current call key plus, during a
/// mid-call rotation, the previous one retained for the overlap window so
/// in-flight frames sealed under the old MLS epoch still open (ADR-0029). The
/// per-frame open runs on libwebrtc's codec thread while a rekey / prune comes
/// from the signaling thread, so the ring is `Mutex`-guarded internally — the
/// bridge does not need its own lock.
///
/// Construct via [`lakko_sframe_call_key_ring_new`]; free via
/// [`lakko_sframe_call_key_ring_free`].
pub struct CallKeyRing {
    inner: Mutex<CoreRing>,
}

/// Lock the ring, recovering the guard even if a prior holder panicked (the
/// ring data stays valid — this crate has no panicking paths, so poison is a
/// belt-and-braces case, never surfaced as a status).
fn lock_ring(ring: &CallKeyRing) -> std::sync::MutexGuard<'_, CoreRing> {
    ring.inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Construct a [`CallKeyRing`] from a 32-byte seed (the current epoch).
///
/// # Safety
///
/// - `seed_ptr` must point to at least `seed_len` valid bytes.
/// - `out_ring` must be a writable `*mut CallKeyRing` storage cell.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_call_key_ring_new(
    seed_ptr: *const u8,
    seed_len: usize,
    out_ring: *mut *mut CallKeyRing,
) -> LakkoSframeStatus {
    if seed_ptr.is_null() || out_ring.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if seed_len != core::CALL_KEY_LEN {
        return LakkoSframeStatus::BadSeedLength;
    }
    // SAFETY: pointer + length guarded above.
    let seed_slice = unsafe { std::slice::from_raw_parts(seed_ptr, seed_len) };
    let mut seed = [0_u8; core::CALL_KEY_LEN];
    seed.copy_from_slice(seed_slice);
    let boxed = Box::new(CallKeyRing {
        inner: Mutex::new(CoreRing::from_seed(seed)),
    });
    // SAFETY: `out_ring` non-null + writable per the caller's contract.
    unsafe {
        *out_ring = Box::into_raw(boxed);
    }
    LakkoSframeStatus::Ok
}

/// Free a [`CallKeyRing`] from [`lakko_sframe_call_key_ring_new`]. Null is a
/// no-op.
///
/// # Safety
///
/// Same matched-pair contract as [`lakko_sframe_call_key_free`].
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_call_key_ring_free(ring: *mut CallKeyRing) {
    if ring.is_null() {
        return;
    }
    // SAFETY: caller upheld the matched-pair contract.
    let _ = unsafe { Box::from_raw(ring) };
}

/// Rotate the ring to a new-epoch 32-byte seed on a mid-call MLS epoch change,
/// retaining the previous key until `now_ms + clamp(overlap_ms)` (ADR-0029).
///
/// # Safety
///
/// - `ring` must be a live pointer from [`lakko_sframe_call_key_ring_new`].
/// - `seed_ptr` must point to at least `seed_len` valid bytes.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_call_key_ring_rekey(
    ring: *const CallKeyRing,
    seed_ptr: *const u8,
    seed_len: usize,
    now_ms: u64,
    overlap_ms: u64,
) -> LakkoSframeStatus {
    if ring.is_null() || seed_ptr.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if seed_len != core::CALL_KEY_LEN {
        return LakkoSframeStatus::BadSeedLength;
    }
    // SAFETY: pointer + length guarded above.
    let seed_slice = unsafe { std::slice::from_raw_parts(seed_ptr, seed_len) };
    let mut seed = [0_u8; core::CALL_KEY_LEN];
    seed.copy_from_slice(seed_slice);
    // SAFETY: caller upheld the live-pointer contract.
    let ring = unsafe { &*ring };
    lock_ring(ring).rekey_from_seed_at(seed, now_ms, overlap_ms);
    LakkoSframeStatus::Ok
}

/// Drop the retained previous key once it has expired at `now_ms`. Null is a
/// no-op.
///
/// # Safety
///
/// `ring` must be null or a live pointer from
/// [`lakko_sframe_call_key_ring_new`].
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_call_key_ring_prune(ring: *const CallKeyRing, now_ms: u64) {
    if ring.is_null() {
        return;
    }
    // SAFETY: caller upheld the live-pointer contract.
    let ring = unsafe { &*ring };
    lock_ring(ring).prune_expired(now_ms);
}

/// Derive the seal-side [`ParticipantKey`] for `sender_leaf`'s
/// `stream_index` stream (ADR-0035: KID = `(sender_leaf << 8) |
/// stream_index`) from the ring's CURRENT epoch. The bridge re-fetches
/// this after a rekey to seal under the new key. On success `*out_pk`
/// is a freshly-allocated pointer the caller owns (free via
/// [`lakko_sframe_participant_key_free`]). A `stream_index` is a `u8`,
/// so every value composes a valid KID — there is no fail-closed
/// rejection here. The key starts at a random counter in `[0, 2^62)`
/// (ADR-0069).
///
/// # Safety
///
/// - `ring` must be a live pointer from [`lakko_sframe_call_key_ring_new`].
/// - `out_pk` must be a writable `*mut ParticipantKey` cell.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_call_key_ring_participant_key(
    ring: *const CallKeyRing,
    sender_leaf: u32,
    stream_index: u8,
    out_pk: *mut *mut ParticipantKey,
) -> LakkoSframeStatus {
    if ring.is_null() || out_pk.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    // Compose the KID from `(leaf, stream)`; a `u8` stream always fits the
    // KID's 8-bit field, so this never overflows.
    let Ok(kid) = SframeHeader::kid_for(sender_leaf, u32::from(stream_index)) else {
        return LakkoSframeStatus::KeyDerivation;
    };
    // SAFETY: caller upheld the live-pointer contract.
    let ring = unsafe { &*ring };
    let Ok(pk) = lock_ring(ring).participant_key(kid) else {
        return LakkoSframeStatus::KeyDerivation;
    };
    let boxed = Box::new(ParticipantKey { inner: pk });
    // SAFETY: `out_pk` non-null + writable per the caller's contract.
    unsafe {
        *out_pk = Box::into_raw(boxed);
    }
    LakkoSframeStatus::Ok
}

/// Open an RFC 9605 framed wire frame with the overlap fall-back
/// (ADR-0029, ADR-0035). The KID — `(sender_leaf << 8) | stream_index`
/// — is read from the frame's own SFrame header, so the DECRYPTOR
/// needs NO leaf / stream argument: the header is the single
/// per-stream discriminator that survives an SFU (the SFU forwards it
/// verbatim, invariant #2), and the ring derives the sender's per-KID
/// key from it. The current epoch's key is tried first, and on an AEAD
/// failure the retained previous key is tried while its overlap window
/// is live. Writes the recovered plaintext into a freshly-allocated
/// buffer the caller MUST free via [`lakko_sframe_buffer_free`]. A
/// malformed header returns [`LakkoSframeStatus::WireParse`]; an AEAD
/// failure under both keys collapses to
/// [`LakkoSframeStatus::AeadFailed`] — the shim drops the frame on
/// either.
///
/// # Safety
///
/// - `ring` must be a live pointer from [`lakko_sframe_call_key_ring_new`].
/// - `wire_ptr` may be null only when `wire_len == 0`.
/// - `out_plaintext_ptr` / `out_plaintext_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_call_key_ring_open_framed_with_overlap(
    ring: *const CallKeyRing,
    wire_ptr: *const u8,
    wire_len: usize,
    out_plaintext_ptr: *mut *mut u8,
    out_plaintext_len: *mut usize,
) -> LakkoSframeStatus {
    if ring.is_null() || out_plaintext_ptr.is_null() || out_plaintext_len.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if wire_ptr.is_null() && wire_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: pointer + length guarded above.
    let wire = if wire_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(wire_ptr, wire_len) }
    };
    // Read the composite KID from the frame's own header — the receiver
    // derives the sender's per-stream key from it (ADR-0035).
    let Ok((header, _consumed)) = SframeHeader::decode(wire) else {
        return LakkoSframeStatus::WireParse;
    };
    // SAFETY: caller upheld the live-pointer contract.
    let ring = unsafe { &*ring };
    let Ok(plaintext) = lock_ring(ring).try_open_framed_with_overlap(header.kid, wire) else {
        return LakkoSframeStatus::AeadFailed;
    };
    let (ptr, len) = vec_into_raw(plaintext);
    // SAFETY: out-pointers non-null per the guard above.
    unsafe {
        *out_plaintext_ptr = ptr;
        *out_plaintext_len = len;
    }
    LakkoSframeStatus::Ok
}

// ---------------------------------------------------------------------
// Sealed media frame — codec header in the clear, SFrame header at
// the end (ADR-0071)
// ---------------------------------------------------------------------
//
// These entry points sit BESIDE the header-first `_seal_framed` /
// `_open_framed` / `_ring_open_framed_with_overlap`, whose behaviour is
// unchanged. The two layouts do not open as each other. The web client
// and the native bridge send and accept only the layout below.

// The four constants below are written as literals, not as
// `MediaCodec::Vp8 as u8` or a path to the core's constant: cbindgen
// emits a `#define` only for a constant whose value it can read off the
// source, and with a computed value the generated header carried none of
// them, so a C++ bridge had nothing to name a codec with. The assertions
// that follow tie each literal to the core's definition at compile time.

/// [`lakko_sframe_media_clear_prefix_len`] codec value: any codec without
/// a rule, or one the sender could not determine. Nothing is left clear.
pub const LAKKO_SFRAME_MEDIA_CODEC_OTHER: u8 = 0;
/// [`lakko_sframe_media_clear_prefix_len`] codec value: VP8 video.
pub const LAKKO_SFRAME_MEDIA_CODEC_VP8: u8 = 1;
/// [`lakko_sframe_media_clear_prefix_len`] codec value: Opus audio.
pub const LAKKO_SFRAME_MEDIA_CODEC_OPUS: u8 = 2;
/// [`lakko_sframe_media_clear_prefix_len`] codec value: an RFC 2198 RED
/// container of Opus frames (`audio/red`). Nothing is left clear, and
/// [`lakko_sframe_seal_media_frame_for_codec`] marks the frame as RED in
/// its trailer so a receiver without RED can take the primary frame out.
pub const LAKKO_SFRAME_MEDIA_CODEC_RED_OPUS: u8 = 3;
/// The most bytes [`lakko_sframe_seal_media_frame`] leaves in the clear.
pub const LAKKO_SFRAME_MAX_CLEAR_PREFIX_LEN: usize = 16;

const _: () = {
    assert!(LAKKO_SFRAME_MEDIA_CODEC_OTHER == MediaCodec::Other as u8);
    assert!(LAKKO_SFRAME_MEDIA_CODEC_VP8 == MediaCodec::Vp8 as u8);
    assert!(LAKKO_SFRAME_MEDIA_CODEC_OPUS == MediaCodec::Opus as u8);
    assert!(LAKKO_SFRAME_MEDIA_CODEC_RED_OPUS == MediaCodec::RedOpus as u8);
    assert!(LAKKO_SFRAME_MAX_CLEAR_PREFIX_LEN == lakko_calls::MAX_CLEAR_PREFIX_LEN);
};

/// How many leading bytes of an encoded frame a sender leaves in the
/// clear for `codec` (one of the `LAKKO_SFRAME_MEDIA_CODEC_*` values;
/// an unknown value is treated as `OTHER`): 10 for a VP8 key frame, 3
/// for any other VP8 frame, 0 for Opus and for `OTHER`. For VP8 the
/// frame's own tag bit decides whether it is a key frame. Never more
/// than `frame_len`. A null `frame_ptr` yields 0.
///
/// # Safety
///
/// `frame_ptr` may be null only when `frame_len == 0`; otherwise
/// `frame_len` valid bytes must be readable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_media_clear_prefix_len(
    codec: u8,
    frame_ptr: *const u8,
    frame_len: usize,
) -> usize {
    if frame_ptr.is_null() || frame_len == 0 {
        return 0;
    }
    // SAFETY: pointer + length guarded above.
    let frame = unsafe { std::slice::from_raw_parts(frame_ptr, frame_len) };
    lakko_calls::clear_prefix_len(MediaCodec::from_u8(codec), frame)
}

/// Seal an encoded media frame into the ADR-0071 layout —
/// `clear_prefix || ciphertext_and_tag || sframe_header || trailer` —
/// leaving the first `clear_prefix_len` bytes of the frame in the clear
/// (cut to `frame_len` when the frame is shorter). The key, nonce and
/// counter are those of [`lakko_sframe_seal_framed`]; the AEAD
/// additional data is `sframe_header || clear_prefix || trailer`. Wraps
/// [`core::ParticipantKey::seal_media_frame`]. The caller takes
/// `clear_prefix_len` from [`lakko_sframe_media_clear_prefix_len`].
///
/// Writes the sealed frame into a freshly-allocated buffer the caller
/// MUST free via [`lakko_sframe_buffer_free`].
///
/// # Safety
///
/// - `participant_key` must be a live pointer from
///   [`lakko_sframe_participant_key_new`].
/// - `frame_ptr` may be null only when `frame_len == 0`.
/// - `out_wire_ptr` / `out_wire_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_seal_media_frame(
    participant_key: *mut ParticipantKey,
    frame_ptr: *const u8,
    frame_len: usize,
    clear_prefix_len: usize,
    out_wire_ptr: *mut *mut u8,
    out_wire_len: *mut usize,
) -> LakkoSframeStatus {
    if participant_key.is_null() || out_wire_ptr.is_null() || out_wire_len.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if frame_ptr.is_null() && frame_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    if clear_prefix_len > LAKKO_SFRAME_MAX_CLEAR_PREFIX_LEN {
        return LakkoSframeStatus::BadClearPrefix;
    }
    // SAFETY: pointer + length guarded above.
    let frame = if frame_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(frame_ptr, frame_len) }
    };
    // SAFETY: caller upheld the live-pointer contract.
    let pk = unsafe { &mut *participant_key };
    let wire = match pk.inner.seal_media_frame(frame, clear_prefix_len) {
        Ok(w) => w,
        Err(CallsError::CounterExhausted) => return LakkoSframeStatus::CounterExhausted,
        Err(_) => return LakkoSframeStatus::AeadFailed,
    };
    let (ptr, len) = vec_into_raw(wire);
    // SAFETY: out-pointers non-null per the guard above.
    unsafe {
        *out_wire_ptr = ptr;
        *out_wire_len = len;
    }
    LakkoSframeStatus::Ok
}

/// Seal an encoded media frame of `codec` (one of the
/// `LAKKO_SFRAME_MEDIA_CODEC_*` values; an unknown value is treated as
/// `OTHER`) into the ADR-0071 layout, with the clear prefix the sender's
/// rule gives that codec ([`lakko_sframe_media_clear_prefix_len`]) and the
/// trailer stating what the frame holds: a RED container for
/// [`LAKKO_SFRAME_MEDIA_CODEC_RED_OPUS`], the codec's own frame otherwise.
/// Wraps [`core::ParticipantKey::seal_media_frame_for_codec`]. Otherwise as
/// [`lakko_sframe_seal_media_frame`], whose buffer contract it shares.
///
/// # Safety
///
/// As [`lakko_sframe_seal_media_frame`].
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_seal_media_frame_for_codec(
    participant_key: *mut ParticipantKey,
    frame_ptr: *const u8,
    frame_len: usize,
    codec: u8,
    out_wire_ptr: *mut *mut u8,
    out_wire_len: *mut usize,
) -> LakkoSframeStatus {
    if participant_key.is_null() || out_wire_ptr.is_null() || out_wire_len.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if frame_ptr.is_null() && frame_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: pointer + length guarded above.
    let frame = if frame_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(frame_ptr, frame_len) }
    };
    // SAFETY: caller upheld the live-pointer contract.
    let pk = unsafe { &mut *participant_key };
    let wire = match pk
        .inner
        .seal_media_frame_for_codec(frame, MediaCodec::from_u8(codec))
    {
        Ok(w) => w,
        Err(CallsError::CounterExhausted) => return LakkoSframeStatus::CounterExhausted,
        Err(_) => return LakkoSframeStatus::AeadFailed,
    };
    let (ptr, len) = vec_into_raw(wire);
    // SAFETY: out-pointers non-null per the guard above.
    unsafe {
        *out_wire_ptr = ptr;
        *out_wire_len = len;
    }
    LakkoSframeStatus::Ok
}

/// Open a sealed media frame in the ADR-0071 layout with the overlap
/// fall-back (ADR-0029) and write the encoded frame
/// `clear_prefix || plaintext` into a freshly-allocated buffer the
/// caller MUST free via [`lakko_sframe_buffer_free`]. The KID is read
/// from the frame's own SFrame header (which sits before the 3-byte
/// trailer at the end), so the decryptor needs no leaf, stream or
/// codec argument. A malformed frame — including one in the
/// header-first layout — returns [`LakkoSframeStatus::WireParse`]; an
/// authentication failure under both keys returns
/// [`LakkoSframeStatus::AeadFailed`]. The shim drops the frame on
/// either.
///
/// # Safety
///
/// - `ring` must be a live pointer from [`lakko_sframe_call_key_ring_new`].
/// - `wire_ptr` may be null only when `wire_len == 0`.
/// - `out_frame_ptr` / `out_frame_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_call_key_ring_open_media_frame_with_overlap(
    ring: *const CallKeyRing,
    wire_ptr: *const u8,
    wire_len: usize,
    out_frame_ptr: *mut *mut u8,
    out_frame_len: *mut usize,
) -> LakkoSframeStatus {
    if ring.is_null() || out_frame_ptr.is_null() || out_frame_len.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if wire_ptr.is_null() && wire_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: pointer + length guarded above.
    let wire = if wire_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(wire_ptr, wire_len) }
    };
    if MediaFrameParts::split(wire).is_err() {
        return LakkoSframeStatus::WireParse;
    }
    // SAFETY: caller upheld the live-pointer contract.
    let ring = unsafe { &*ring };
    let Ok(frame) = lock_ring(ring).try_open_media_frame_with_overlap(wire) else {
        return LakkoSframeStatus::AeadFailed;
    };
    let (ptr, len) = vec_into_raw(frame);
    // SAFETY: out-pointers non-null per the guard above.
    unsafe {
        *out_frame_ptr = ptr;
        *out_frame_len = len;
    }
    LakkoSframeStatus::Ok
}

// ---------------------------------------------------------------------
// Wire frame wrap / unwrap (RFC 9605 §4)
// ---------------------------------------------------------------------

/// Encode an SFrame wire frame: `header(kid=sender_leaf, ctr=counter)
/// || ciphertext`. The platform shim hands this back to libwebrtc
/// as the encrypted-frame payload.
///
/// # KID note — bare leaf, NOT the ADR-0035 composite
///
/// This function stamps the header KID as the **bare**
/// `sender_leaf` (`kid = u64::from(sender_leaf)`), NOT the
/// ADR-0035 keyed composite `(sender_leaf << 8) | stream_index`.
/// It touches no key or nonce — it only frames bytes the caller
/// has ALREADY sealed — so this is not a `(key, nonce)`-reuse
/// hazard: the KID here is cosmetic / routing metadata. It is
/// intended only for pre-sealed opaque bytes. Do NOT mix it with
/// the raw [`lakko_sframe_seal`] on the keyed media path: a frame
/// whose key was derived under a composite KID but whose header is
/// stamped with a bare-leaf KID would carry a header disagreeing
/// with the key-derivation KID, so the receiver's header-as-AAD
/// open would fail. The live media path uses
/// [`lakko_sframe_seal_framed`] / [`lakko_sframe_open_framed`]
/// (and the ring's `_open_framed_with_overlap`), which compose and
/// stamp the correct composite KID in one place — callers on that
/// path MUST use those, never `seal` + `wrap_frame`. As of this
/// writing `wrap_frame` has NO production caller (test-only).
///
/// # Safety
///
/// - `ciphertext_ptr` may be null only when `ciphertext_len == 0`.
/// - `out_wire_ptr` and `out_wire_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_wrap_frame(
    sender_leaf: u32,
    counter: u64,
    ciphertext_ptr: *const u8,
    ciphertext_len: usize,
    out_wire_ptr: *mut *mut u8,
    out_wire_len: *mut usize,
) -> LakkoSframeStatus {
    if out_wire_ptr.is_null() || out_wire_len.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    if ciphertext_ptr.is_null() && ciphertext_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: pointer + length guarded.
    let ciphertext = if ciphertext_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ciphertext_ptr, ciphertext_len) }
    };

    let header = SframeHeader {
        kid: u64::from(sender_leaf),
        ctr: counter,
    };
    let mut wire = header.encode();
    wire.extend_from_slice(ciphertext);
    let (ptr, len) = vec_into_raw(wire);
    // SAFETY: out-pointers non-null per the guard.
    unsafe {
        *out_wire_ptr = ptr;
        *out_wire_len = len;
    }
    LakkoSframeStatus::Ok
}

/// Parse an SFrame wire frame back into `(sender_leaf, counter,
/// ciphertext)`. Writes the parsed scalars + a freshly-allocated
/// ciphertext buffer the caller MUST free.
///
/// # Safety
///
/// - `wire_ptr` may be null only when `wire_len == 0` (which will
///   always parse as [`LakkoSframeStatus::WireParse`]).
/// - The four `out_*` pointers must be writable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_unwrap_frame(
    wire_ptr: *const u8,
    wire_len: usize,
    out_sender_leaf: *mut u32,
    out_counter: *mut u64,
    out_ciphertext_ptr: *mut *mut u8,
    out_ciphertext_len: *mut usize,
) -> LakkoSframeStatus {
    if out_sender_leaf.is_null()
        || out_counter.is_null()
        || out_ciphertext_ptr.is_null()
        || out_ciphertext_len.is_null()
    {
        return LakkoSframeStatus::NullPointer;
    }
    if wire_ptr.is_null() && wire_len != 0 {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: pointer + length guarded.
    let wire = if wire_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(wire_ptr, wire_len) }
    };

    let Ok((header, consumed)) = SframeHeader::decode(wire) else {
        return LakkoSframeStatus::WireParse;
    };
    let Ok(sender_leaf) = u32::try_from(header.kid) else {
        return LakkoSframeStatus::KidOverflow;
    };
    let counter = header.ctr;

    let ciphertext = wire[consumed..].to_vec();
    let (ct_ptr, ct_len) = vec_into_raw(ciphertext);
    // SAFETY: out-pointers non-null per the guard.
    unsafe {
        *out_sender_leaf = sender_leaf;
        *out_counter = counter;
        *out_ciphertext_ptr = ct_ptr;
        *out_ciphertext_len = ct_len;
    }
    LakkoSframeStatus::Ok
}

// ---------------------------------------------------------------------
// Buffer ownership transfer
// ---------------------------------------------------------------------

/// Free a buffer this crate produced — the `(ptr, len)` pair returned
/// by [`lakko_sframe_seal`] / [`lakko_sframe_open`] /
/// [`lakko_sframe_wrap_frame`] / [`lakko_sframe_unwrap_frame`].
/// Passing `null` is a no-op.
///
/// # Safety
///
/// - `ptr` must either be null or a pointer this crate handed out
///   (paired with the matching `len` from the same call). Mixing
///   with libc `malloc` / `free` is undefined behaviour.
/// - The same `(ptr, len)` pair must NOT be freed twice.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_buffer_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: caller upheld matched-pair contract per the doc.
    // Reconstructing the `Vec` with capacity == length matches
    // what `vec_into_raw` shipped out (it normalises capacity to
    // length before leaking).
    let _ = unsafe { Vec::from_raw_parts(ptr, len, len) };
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// Leak a `Vec<u8>` as a `(ptr, len)` pair the caller eventually
/// reclaims via [`lakko_sframe_buffer_free`]. Normalises capacity to
/// length so the reverse [`Vec::from_raw_parts`] inside `_buffer_free`
/// passes a matching capacity argument — the global allocator
/// requires that `capacity` matches what was allocated.
fn vec_into_raw(mut v: Vec<u8>) -> (*mut u8, usize) {
    v.shrink_to_fit();
    let len = v.len();
    let ptr = v.as_mut_ptr();
    std::mem::forget(v);
    (ptr, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_seed() -> Vec<u8> {
        vec![0x42_u8; core::CALL_KEY_LEN]
    }

    /// Build a CallKey via the C ABI; the resulting pointer must
    /// be non-null.
    #[test]
    fn call_key_new_round_trip() {
        let seed = fixture_seed();
        let mut ck_ptr: *mut CallKey = std::ptr::null_mut();
        let status = unsafe { lakko_sframe_call_key_new(seed.as_ptr(), seed.len(), &mut ck_ptr) };
        assert_eq!(status, LakkoSframeStatus::Ok);
        assert!(!ck_ptr.is_null());
        unsafe { lakko_sframe_call_key_free(ck_ptr) };
    }

    /// Wrong seed length must produce `BadSeedLength` and leave
    /// the out-pointer untouched.
    #[test]
    fn call_key_new_rejects_wrong_length() {
        let bad_seed = [0x42_u8; 16];
        let mut ck_ptr: *mut CallKey = std::ptr::null_mut();
        let sentinel = ck_ptr;
        let status =
            unsafe { lakko_sframe_call_key_new(bad_seed.as_ptr(), bad_seed.len(), &mut ck_ptr) };
        assert_eq!(status, LakkoSframeStatus::BadSeedLength);
        assert_eq!(ck_ptr, sentinel, "out-ptr untouched on failure");
    }

    /// Null arguments must surface as `NullPointer`.
    #[test]
    fn call_key_new_refuses_null_args() {
        let mut ck_ptr: *mut CallKey = std::ptr::null_mut();
        let status = unsafe { lakko_sframe_call_key_new(std::ptr::null(), 32, &mut ck_ptr) };
        assert_eq!(status, LakkoSframeStatus::NullPointer);
        // Free with null is a no-op.
        unsafe { lakko_sframe_call_key_free(std::ptr::null_mut()) };
    }

    /// Full seal/open round trip through the C ABI.
    #[test]
    fn seal_open_round_trip() {
        let seed = fixture_seed();
        let aad = b"meta".to_vec();
        let plaintext = b"hello sframe c-abi".to_vec();

        // CallKey + ParticipantKey
        let mut ck_ptr: *mut CallKey = std::ptr::null_mut();
        assert_eq!(
            unsafe { lakko_sframe_call_key_new(seed.as_ptr(), seed.len(), &mut ck_ptr) },
            LakkoSframeStatus::Ok
        );
        let mut pk_ptr: *mut ParticipantKey = std::ptr::null_mut();
        assert_eq!(
            // stream_index 0 = audio (ADR-0035).
            unsafe { lakko_sframe_participant_key_new(ck_ptr, 7, 0, &mut pk_ptr) },
            LakkoSframeStatus::Ok
        );

        // Seal. The key starts at a random counter below 2^62 (ADR-0069);
        // the first frame carries start + 1.
        let start = unsafe { lakko_sframe_participant_key_counter(pk_ptr) };
        assert!(start < 1 << 62, "random start is below 2^62");
        let mut counter: u64 = 0;
        let mut ct_ptr: *mut u8 = std::ptr::null_mut();
        let mut ct_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_seal(
                    pk_ptr,
                    aad.as_ptr(),
                    aad.len(),
                    plaintext.as_ptr(),
                    plaintext.len(),
                    &mut counter,
                    &mut ct_ptr,
                    &mut ct_len,
                )
            },
            LakkoSframeStatus::Ok
        );
        assert_eq!(counter, start + 1, "first seal lands on start + 1");
        assert_eq!(
            unsafe { lakko_sframe_participant_key_counter(pk_ptr) },
            counter,
            "the getter reports the counter of the last frame sealed"
        );
        assert!(!ct_ptr.is_null());
        assert!(ct_len > plaintext.len(), "ciphertext carries AEAD tag");

        // Open back through the same ParticipantKey.
        let mut pt_ptr: *mut u8 = std::ptr::null_mut();
        let mut pt_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_open(
                    pk_ptr,
                    aad.as_ptr(),
                    aad.len(),
                    ct_ptr,
                    ct_len,
                    counter,
                    &mut pt_ptr,
                    &mut pt_len,
                )
            },
            LakkoSframeStatus::Ok
        );
        let recovered = unsafe { std::slice::from_raw_parts(pt_ptr, pt_len) };
        assert_eq!(recovered, plaintext.as_slice());

        unsafe {
            lakko_sframe_buffer_free(ct_ptr, ct_len);
            lakko_sframe_buffer_free(pt_ptr, pt_len);
            lakko_sframe_participant_key_free(pk_ptr);
            lakko_sframe_call_key_free(ck_ptr);
        }
    }

    /// ADR-0013 header-as-AAD framed seal / open round-trip through
    /// the C ABI: a sender's `seal_framed` wire frame opens on a
    /// receiver key re-derived for the same leaf, with no external
    /// AAD passed anywhere.
    #[test]
    fn seal_framed_open_framed_round_trip() {
        let seed = fixture_seed();
        let plaintext = b"framed e2ee media".to_vec();

        let mut ck_ptr: *mut CallKey = std::ptr::null_mut();
        assert_eq!(
            unsafe { lakko_sframe_call_key_new(seed.as_ptr(), seed.len(), &mut ck_ptr) },
            LakkoSframeStatus::Ok
        );
        // Sender + receiver keys, both derived for leaf 7 on stream 0 (audio).
        let mut sender_ptr: *mut ParticipantKey = std::ptr::null_mut();
        let mut receiver_ptr: *mut ParticipantKey = std::ptr::null_mut();
        unsafe {
            lakko_sframe_participant_key_new(ck_ptr, 7, 0, &mut sender_ptr);
            lakko_sframe_participant_key_new(ck_ptr, 7, 0, &mut receiver_ptr);
        }

        // seal_framed → header || ciphertext (no AAD argument).
        let mut wire_ptr: *mut u8 = std::ptr::null_mut();
        let mut wire_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_seal_framed(
                    sender_ptr,
                    plaintext.as_ptr(),
                    plaintext.len(),
                    &mut wire_ptr,
                    &mut wire_len,
                )
            },
            LakkoSframeStatus::Ok
        );
        assert!(!wire_ptr.is_null());
        // header (>=2 bytes) + ciphertext (plaintext + AEAD tag).
        assert!(wire_len > plaintext.len() + 1);

        // open_framed on the receiver recovers the plaintext.
        let mut pt_ptr: *mut u8 = std::ptr::null_mut();
        let mut pt_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_open_framed(receiver_ptr, wire_ptr, wire_len, &mut pt_ptr, &mut pt_len)
            },
            LakkoSframeStatus::Ok
        );
        let recovered = unsafe { std::slice::from_raw_parts(pt_ptr, pt_len) };
        assert_eq!(recovered, plaintext.as_slice());

        // A tampered header byte fails to open (AAD + nonce both shift).
        let mut tampered = unsafe { std::slice::from_raw_parts(wire_ptr, wire_len) }.to_vec();
        tampered[1] = tampered[1].wrapping_add(1);
        let mut bad_ptr: *mut u8 = std::ptr::null_mut();
        let mut bad_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_open_framed(
                    receiver_ptr,
                    tampered.as_ptr(),
                    tampered.len(),
                    &mut bad_ptr,
                    &mut bad_len,
                )
            },
            LakkoSframeStatus::AeadFailed
        );

        unsafe {
            lakko_sframe_buffer_free(wire_ptr, wire_len);
            lakko_sframe_buffer_free(pt_ptr, pt_len);
            lakko_sframe_participant_key_free(sender_ptr);
            lakko_sframe_participant_key_free(receiver_ptr);
            lakko_sframe_call_key_free(ck_ptr);
        }
    }

    /// Three streams of one leaf — audio (stream 0) + two video streams
    /// (stream 1 camera, stream 2 screen-share) — derive DISTINCT key
    /// spaces (ADR-0035, mirroring the core's
    /// `three_streams_of_one_leaf_get_distinct_keys_and_nonces`). A frame
    /// sealed under one stream's key does NOT open under either of the
    /// other two for the same seed + leaf, and the two VIDEO streams are
    /// pairwise-distinct too — the pair the ADR-0034 per-media-TYPE tag
    /// re-collided. So the `stream_index` byte genuinely reaches the HKDF
    /// via the full composite KID, it is not decorative.
    #[test]
    fn audio_and_two_video_streams_are_distinct() {
        let seed = fixture_seed();
        let plaintext = b"stream-scoped media".to_vec();
        let leaf = 4;

        let mut ck_ptr: *mut CallKey = std::ptr::null_mut();
        assert_eq!(
            unsafe { lakko_sframe_call_key_new(seed.as_ptr(), seed.len(), &mut ck_ptr) },
            LakkoSframeStatus::Ok
        );

        // Seal the same plaintext under each of the three streams.
        let seal_on_stream = |stream_index: u8| -> Vec<u8> {
            let mut pk: *mut ParticipantKey = std::ptr::null_mut();
            assert_eq!(
                unsafe { lakko_sframe_participant_key_new(ck_ptr, leaf, stream_index, &mut pk) },
                LakkoSframeStatus::Ok
            );
            let mut wire_ptr: *mut u8 = std::ptr::null_mut();
            let mut wire_len: usize = 0;
            assert_eq!(
                unsafe {
                    lakko_sframe_seal_framed(
                        pk,
                        plaintext.as_ptr(),
                        plaintext.len(),
                        &mut wire_ptr,
                        &mut wire_len,
                    )
                },
                LakkoSframeStatus::Ok
            );
            let wire = unsafe { std::slice::from_raw_parts(wire_ptr, wire_len) }.to_vec();
            unsafe {
                lakko_sframe_buffer_free(wire_ptr, wire_len);
                lakko_sframe_participant_key_free(pk);
            }
            wire
        };
        let audio_wire = seal_on_stream(0);
        let cam_wire = seal_on_stream(1);
        let screen_wire = seal_on_stream(2);

        // The three wires are pairwise distinct. (Each key starts at its own
        // random counter since ADR-0069, so this alone no longer shows key
        // separation; the cross-stream open refusals below do.)
        assert_ne!(audio_wire, cam_wire);
        assert_ne!(audio_wire, screen_wire);
        assert_ne!(
            cam_wire, screen_wire,
            "two video streams from one leaf must not seal identically (the ADR-0034 gap)"
        );

        // A key for stream 2 (screen) must NOT open a stream-1 (camera) frame,
        // and vice versa — cross-stream isolation, the exact pair ADR-0034
        // missed.
        let open_on_stream = |stream_index: u8, wire: &[u8]| -> LakkoSframeStatus {
            let mut pk: *mut ParticipantKey = std::ptr::null_mut();
            assert_eq!(
                unsafe { lakko_sframe_participant_key_new(ck_ptr, leaf, stream_index, &mut pk) },
                LakkoSframeStatus::Ok
            );
            let mut pt_ptr: *mut u8 = std::ptr::null_mut();
            let mut pt_len: usize = 0;
            let status = unsafe {
                lakko_sframe_open_framed(pk, wire.as_ptr(), wire.len(), &mut pt_ptr, &mut pt_len)
            };
            if status == LakkoSframeStatus::Ok {
                unsafe { lakko_sframe_buffer_free(pt_ptr, pt_len) };
            }
            unsafe { lakko_sframe_participant_key_free(pk) };
            status
        };
        assert_eq!(open_on_stream(2, &cam_wire), LakkoSframeStatus::AeadFailed);
        assert_eq!(
            open_on_stream(1, &screen_wire),
            LakkoSframeStatus::AeadFailed
        );
        assert_eq!(
            open_on_stream(1, &audio_wire),
            LakkoSframeStatus::AeadFailed
        );

        // A matching stream-0 key opens the audio frame (same stream round-trips).
        let mut audio_rx: *mut ParticipantKey = std::ptr::null_mut();
        unsafe {
            lakko_sframe_participant_key_new(ck_ptr, leaf, 0, &mut audio_rx);
        }
        let wire = audio_wire;
        let mut pt_ptr: *mut u8 = std::ptr::null_mut();
        let mut pt_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_open_framed(
                    audio_rx,
                    wire.as_ptr(),
                    wire.len(),
                    &mut pt_ptr,
                    &mut pt_len,
                )
            },
            LakkoSframeStatus::Ok
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(pt_ptr, pt_len) },
            plaintext.as_slice()
        );

        unsafe {
            lakko_sframe_buffer_free(pt_ptr, pt_len);
            lakko_sframe_participant_key_free(audio_rx);
            lakko_sframe_call_key_free(ck_ptr);
        }
    }

    /// Seal `plaintext` for `leaf` through the ring's current participant key
    /// (stream 0 = audio).
    fn seal_via_ring(ring: *mut CallKeyRing, leaf: u32, plaintext: &[u8]) -> Vec<u8> {
        let mut pk: *mut ParticipantKey = std::ptr::null_mut();
        assert_eq!(
            unsafe { lakko_sframe_call_key_ring_participant_key(ring, leaf, 0, &mut pk) },
            LakkoSframeStatus::Ok
        );
        let mut wire_ptr: *mut u8 = std::ptr::null_mut();
        let mut wire_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_seal_framed(
                    pk,
                    plaintext.as_ptr(),
                    plaintext.len(),
                    &mut wire_ptr,
                    &mut wire_len,
                )
            },
            LakkoSframeStatus::Ok
        );
        let wire = unsafe { std::slice::from_raw_parts(wire_ptr, wire_len) }.to_vec();
        unsafe {
            lakko_sframe_buffer_free(wire_ptr, wire_len);
            lakko_sframe_participant_key_free(pk);
        }
        wire
    }

    /// Open `wire` through the ring's overlap fall-back — the KID (and thus the
    /// sender leaf + stream) is read from the frame's own header (ADR-0035), so
    /// no leaf / stream argument is passed. `None` on any non-`Ok` status (the
    /// frame would be dropped).
    fn open_via_ring(ring: *mut CallKeyRing, wire: &[u8]) -> Option<Vec<u8>> {
        let mut pt_ptr: *mut u8 = std::ptr::null_mut();
        let mut pt_len: usize = 0;
        let status = unsafe {
            lakko_sframe_call_key_ring_open_framed_with_overlap(
                ring,
                wire.as_ptr(),
                wire.len(),
                &mut pt_ptr,
                &mut pt_len,
            )
        };
        if status != LakkoSframeStatus::Ok {
            return None;
        }
        let pt = unsafe { std::slice::from_raw_parts(pt_ptr, pt_len) }.to_vec();
        unsafe { lakko_sframe_buffer_free(pt_ptr, pt_len) };
        Some(pt)
    }

    /// The C ABI CallKeyRing overlap path (G-18, ADR-0029): a frame sealed under
    /// epoch A still opens through the ring after a rekey to epoch B (overlap
    /// window), a fresh epoch-B frame opens under the current key, and pruning
    /// the expired window closes the fall-back.
    #[test]
    fn call_key_ring_overlap_round_trip() {
        let seed_a = [0x11_u8; core::CALL_KEY_LEN];
        let seed_b = [0x22_u8; core::CALL_KEY_LEN];
        let leaf = 3_u32;

        let mut ring: *mut CallKeyRing = std::ptr::null_mut();
        assert_eq!(
            unsafe { lakko_sframe_call_key_ring_new(seed_a.as_ptr(), seed_a.len(), &mut ring) },
            LakkoSframeStatus::Ok
        );

        // Seal an epoch-A frame, still in flight when the rotation lands.
        let old_wire = seal_via_ring(ring, leaf, b"old-epoch");

        // Rotate to epoch B with a 1 s overlap window at t=1000 (expires 2000).
        assert_eq!(
            unsafe {
                lakko_sframe_call_key_ring_rekey(ring, seed_b.as_ptr(), seed_b.len(), 1_000, 1_000)
            },
            LakkoSframeStatus::Ok
        );

        // A fresh epoch-B frame.
        let new_wire = seal_via_ring(ring, leaf, b"new-epoch");

        // Both open through the ring's overlap fall-back inside the window.
        assert_eq!(open_via_ring(ring, &old_wire), Some(b"old-epoch".to_vec()));
        assert_eq!(open_via_ring(ring, &new_wire), Some(b"new-epoch".to_vec()));

        // Prune past the window → the old frame no longer opens; the new one does.
        unsafe { lakko_sframe_call_key_ring_prune(ring, 2_000) };
        assert_eq!(open_via_ring(ring, &old_wire), None);
        assert_eq!(open_via_ring(ring, &new_wire), Some(b"new-epoch".to_vec()));

        // A wrong-length seed is refused.
        let mut bad: *mut CallKeyRing = std::ptr::null_mut();
        let short = [0u8; 31];
        assert_eq!(
            unsafe { lakko_sframe_call_key_ring_new(short.as_ptr(), short.len(), &mut bad) },
            LakkoSframeStatus::BadSeedLength
        );

        unsafe { lakko_sframe_call_key_ring_free(ring) };
    }

    /// The `sender_leaf` getter returns the sentinel `u32::MAX` and the
    /// `counter` getter `u64::MAX` on null input (and the right values on
    /// a live handle: the leaf, and a random start below 2^62).
    #[test]
    fn getters_handle_null_and_live() {
        assert_eq!(
            unsafe { lakko_sframe_participant_key_sender_leaf(std::ptr::null()) },
            u32::MAX
        );
        assert_eq!(
            unsafe { lakko_sframe_participant_key_counter(std::ptr::null()) },
            u64::MAX
        );

        let seed = fixture_seed();
        let mut ck_ptr: *mut CallKey = std::ptr::null_mut();
        unsafe {
            lakko_sframe_call_key_new(seed.as_ptr(), seed.len(), &mut ck_ptr);
        }
        let mut pk_ptr: *mut ParticipantKey = std::ptr::null_mut();
        unsafe {
            // stream 0 = audio (ADR-0035).
            lakko_sframe_participant_key_new(ck_ptr, 9, 0, &mut pk_ptr);
        }
        assert_eq!(
            unsafe { lakko_sframe_participant_key_sender_leaf(pk_ptr) },
            9
        );
        let start = unsafe { lakko_sframe_participant_key_counter(pk_ptr) };
        assert!(start < 1 << 62, "a fresh key starts below 2^62 (ADR-0069)");
        // A second key for the same (call key, leaf, stream) starts elsewhere
        // (equal with probability 2^-62).
        let mut pk2_ptr: *mut ParticipantKey = std::ptr::null_mut();
        unsafe {
            lakko_sframe_participant_key_new(ck_ptr, 9, 0, &mut pk2_ptr);
        }
        assert_ne!(
            unsafe { lakko_sframe_participant_key_counter(pk2_ptr) },
            start
        );
        unsafe {
            lakko_sframe_participant_key_free(pk2_ptr);
            lakko_sframe_participant_key_free(pk_ptr);
            lakko_sframe_call_key_free(ck_ptr);
        }
    }

    /// Wire wrap/unwrap round trip through the C ABI, with a counter that
    /// needs all 8 bytes (ADR-0069: the counter is 64-bit end to end).
    #[test]
    fn wire_wrap_unwrap_round_trip() {
        const WIDE_COUNTER: u64 = 0x3FED_CBA9_8765_4321;
        let ct = b"opaque-aead-bytes".to_vec();
        let mut wire_ptr: *mut u8 = std::ptr::null_mut();
        let mut wire_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_wrap_frame(
                    5,
                    WIDE_COUNTER,
                    ct.as_ptr(),
                    ct.len(),
                    &mut wire_ptr,
                    &mut wire_len,
                )
            },
            LakkoSframeStatus::Ok
        );
        // config byte (KID 5 inline) + 8 CTR bytes + ciphertext.
        assert_eq!(wire_len, 1 + 8 + ct.len());

        let mut leaf: u32 = 0;
        let mut counter: u64 = 0;
        let mut out_ct_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_ct_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_unwrap_frame(
                    wire_ptr,
                    wire_len,
                    &mut leaf,
                    &mut counter,
                    &mut out_ct_ptr,
                    &mut out_ct_len,
                )
            },
            LakkoSframeStatus::Ok
        );
        assert_eq!(leaf, 5);
        assert_eq!(counter, WIDE_COUNTER);
        let unwrapped = unsafe { std::slice::from_raw_parts(out_ct_ptr, out_ct_len) };
        assert_eq!(unwrapped, ct.as_slice());

        unsafe {
            lakko_sframe_buffer_free(wire_ptr, wire_len);
            lakko_sframe_buffer_free(out_ct_ptr, out_ct_len);
        }
    }

    /// Empty wire → WireParse; a truncated header (an extended
    /// field declaring bytes that are not present) → WireParse.
    /// These pin the loud-fail surface for the C++ shim N-3 /
    /// Obj-C++ shim N-4 / JNI shim N-5.
    #[test]
    fn unwrap_loud_fails_on_malformed_wire() {
        let mut leaf = 0_u32;
        let mut counter = 0_u64;
        let mut ct_ptr: *mut u8 = std::ptr::null_mut();
        let mut ct_len: usize = 0;

        assert_eq!(
            unsafe {
                lakko_sframe_unwrap_frame(
                    [].as_ptr(),
                    0,
                    &mut leaf,
                    &mut counter,
                    &mut ct_ptr,
                    &mut ct_len,
                )
            },
            LakkoSframeStatus::WireParse
        );

        // 0x0B = RFC 9605 §4.3 X=0 K=0 Y=1 C=3 → declares a 4-byte CTR with
        // none present → the header decode fails as truncated.
        let bad = [0x0B_u8];
        assert_eq!(
            unsafe {
                lakko_sframe_unwrap_frame(
                    bad.as_ptr(),
                    bad.len(),
                    &mut leaf,
                    &mut counter,
                    &mut ct_ptr,
                    &mut ct_len,
                )
            },
            LakkoSframeStatus::WireParse
        );
    }
}
