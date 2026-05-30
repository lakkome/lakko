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

use lakko_calls::sframe as core;
use lakko_calls::{Error as CallsError, SframeHeader};

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
    /// 32-bit frame counter rolled over; caller MUST trigger a
    /// fresh MLS commit and re-derive the call key before
    /// continuing. Re-using a nonce under the same key is an
    /// AEAD-fatal mistake.
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
    /// Parsed `ctr` exceeds `u32::MAX`. Same reasoning as
    /// [`Self::KidOverflow`].
    CtrOverflow = 8,
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

/// Derive the [`ParticipantKey`] for `sender_leaf` from `call_key`.
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
    out_participant_key: *mut *mut ParticipantKey,
) -> LakkoSframeStatus {
    if call_key.is_null() || out_participant_key.is_null() {
        return LakkoSframeStatus::NullPointer;
    }
    // SAFETY: caller passed a live pointer from `_call_key_new`; we
    // borrow it for the duration of `participant_key`.
    let ck = unsafe { &*call_key };
    let Ok(pk) = ck.inner.participant_key(sender_leaf) else {
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

/// Borrow the current frame counter — diagnostic only; the
/// canonical increment lives inside [`lakko_sframe_seal`].
///
/// # Safety
///
/// Same as [`lakko_sframe_participant_key_sender_leaf`].
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_participant_key_counter(
    participant_key: *const ParticipantKey,
) -> u32 {
    if participant_key.is_null() {
        return u32::MAX;
    }
    // SAFETY: caller upheld the live-pointer contract.
    unsafe { (*participant_key).inner.counter() }
}

// ---------------------------------------------------------------------
// AEAD seal / open
// ---------------------------------------------------------------------

/// Seal `plaintext` under the participant key's next counter.
/// Writes `*out_counter` to the consumed counter value and
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
    out_counter: *mut u32,
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
/// caller-supplied `counter` (from the SFrame wire header) to
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
    counter: u32,
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
// Wire frame wrap / unwrap (RFC 9605 §4)
// ---------------------------------------------------------------------

/// Encode an SFrame wire frame: `header(kid=sender_leaf, ctr=counter)
/// || ciphertext`. The platform shim hands this back to libwebrtc
/// as the encrypted-frame payload.
///
/// # Safety
///
/// - `ciphertext_ptr` may be null only when `ciphertext_len == 0`.
/// - `out_wire_ptr` and `out_wire_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn lakko_sframe_wrap_frame(
    sender_leaf: u32,
    counter: u32,
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
        ctr: u64::from(counter),
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
    out_counter: *mut u32,
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
    let Ok(counter) = u32::try_from(header.ctr) else {
        return LakkoSframeStatus::CtrOverflow;
    };

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
            unsafe { lakko_sframe_participant_key_new(ck_ptr, 7, &mut pk_ptr) },
            LakkoSframeStatus::Ok
        );

        // Seal
        let mut counter: u32 = 0;
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
        assert_eq!(counter, 1, "first seal lands on counter=1");
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

    /// `sender_leaf` + `counter` getters return sentinel `u32::MAX`
    /// on null input (and the right values on a live handle).
    #[test]
    fn getters_handle_null_and_live() {
        assert_eq!(
            unsafe { lakko_sframe_participant_key_sender_leaf(std::ptr::null()) },
            u32::MAX
        );
        assert_eq!(
            unsafe { lakko_sframe_participant_key_counter(std::ptr::null()) },
            u32::MAX
        );

        let seed = fixture_seed();
        let mut ck_ptr: *mut CallKey = std::ptr::null_mut();
        unsafe {
            lakko_sframe_call_key_new(seed.as_ptr(), seed.len(), &mut ck_ptr);
        }
        let mut pk_ptr: *mut ParticipantKey = std::ptr::null_mut();
        unsafe {
            lakko_sframe_participant_key_new(ck_ptr, 9, &mut pk_ptr);
        }
        assert_eq!(
            unsafe { lakko_sframe_participant_key_sender_leaf(pk_ptr) },
            9
        );
        assert_eq!(unsafe { lakko_sframe_participant_key_counter(pk_ptr) }, 0);
        unsafe {
            lakko_sframe_participant_key_free(pk_ptr);
            lakko_sframe_call_key_free(ck_ptr);
        }
    }

    /// Wire wrap/unwrap round trip through the C ABI.
    #[test]
    fn wire_wrap_unwrap_round_trip() {
        let ct = b"opaque-aead-bytes".to_vec();
        let mut wire_ptr: *mut u8 = std::ptr::null_mut();
        let mut wire_len: usize = 0;
        assert_eq!(
            unsafe {
                lakko_sframe_wrap_frame(5, 42, ct.as_ptr(), ct.len(), &mut wire_ptr, &mut wire_len)
            },
            LakkoSframeStatus::Ok
        );

        let mut leaf: u32 = 0;
        let mut counter: u32 = 0;
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
        assert_eq!(counter, 42);
        let unwrapped = unsafe { std::slice::from_raw_parts(out_ct_ptr, out_ct_len) };
        assert_eq!(unwrapped, ct.as_slice());

        unsafe {
            lakko_sframe_buffer_free(wire_ptr, wire_len);
            lakko_sframe_buffer_free(out_ct_ptr, out_ct_len);
        }
    }

    /// Empty wire → WireParse; reserved-bit-set first byte →
    /// WireParse. These pin the loud-fail surface for the C++
    /// shim N-3 / Obj-C++ shim N-4 / JNI shim N-5.
    #[test]
    fn unwrap_loud_fails_on_malformed_wire() {
        let mut leaf = 0_u32;
        let mut counter = 0_u32;
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

        let bad = [0x80_u8, 0x00, 0x00];
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
