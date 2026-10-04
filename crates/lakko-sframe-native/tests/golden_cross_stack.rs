// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Cross-stack SFrame golden-vector interop through the REAL C ABI (G-26).
//!
//! The native call-media path seals / opens every frame through the
//! `lakko-sframe-native` `extern "C"` surface — `lakko_sframe_seal_framed` /
//! `lakko_sframe_open_framed` — which the iOS Obj-C++ shim
//! (`SFrameAttach.mm`) and the Android JNI module call into via the shared
//! `apps/shared/sframe-bridge/` C++ bridge before handing bytes to libwebrtc's
//! `FrameEncryptorInterface` / `FrameDecryptorInterface`. The crate's existing
//! `sfu_opacity_e2e.rs` exercises the Rust-side `lakko_calls::sframe` core API
//! directly (with arbitrary per-test seeds), but it does NOT cross the
//! `extern "C"` boundary and does NOT pin the canonical cross-stack golden.
//!
//! This test closes that gap for the automatable (headless) slice of G-26: it
//! drives the actual C ABI with the ONE canonical golden vector that is pinned
//! byte-for-byte in four other places —
//!
//!  * the Rust core (`crates/lakko-calls/src/sframe.rs` golden tests),
//!  * the web worker path (`apps/web/src/calls/sframe-golden.test.ts`),
//!  * iOS (`apps/ios/LakkoTests/SFrameGoldenTests.swift`),
//!  * Android (`apps/android/.../calls/SframeGoldenTest.kt`),
//!
//! and asserts the C ABI agrees: the canonical wire (a frame at counter 1)
//! OPENS here, and a frame the native media shim seals opens under an
//! independently derived key. Since ADR-0069 a key starts at a random 62-bit
//! counter, so the C ABI cannot seal a predictable frame and no function of
//! it sets a counter; the seal direction is proved by opening what was sealed
//! and by reading the header (the composite KID, a 64-bit counter one above
//! the key's start). Because the AEAD AAD is the RFC 9605 §4.3 wire header
//! (KID + CTR) and never host RTP `additional_data` (ADR-0013 / C-55), a frame
//! crosses stacks.
//!
//! What this canNOT cover (genuinely deferred, needs runtimes): a live
//! real-Chrome / real-simulator / deployed-SFU RTP smoke run. That is
//! redundant for *correctness* — the keying math + wire format + SFU
//! forwarding are each already proven (the live `str0m` SFU golden round-trip
//! in `services/sfu/tests/synthetic_client_e2e.rs` pushes THESE exact bytes
//! through a real ICE + DTLS + fanout) — and is a deploy / CI-execution
//! concern, not a development one.

// This integration test drives the crate's `extern "C"` C ABI directly, so it
// carries justified unsafe — the same exemption the lib crate takes from the
// workspace `unsafe_code = "deny"`.
#![allow(unsafe_code)]

use std::ptr;

use lakko_sframe_native::{
    lakko_sframe_buffer_free, lakko_sframe_call_key_free, lakko_sframe_call_key_new,
    lakko_sframe_open_framed, lakko_sframe_participant_key_counter,
    lakko_sframe_participant_key_free, lakko_sframe_participant_key_new, lakko_sframe_seal_framed,
    lakko_sframe_unwrap_frame, CallKey, LakkoSframeStatus, ParticipantKey,
};

/// The canonical cross-stack golden vector — byte-identical to the constants
/// pinned in the Rust core, web, iOS, and Android golden tests. If the C ABI
/// (the path the native media shims call) could not open these bytes, a frame
/// sealed on web / the core would not open natively, breaking ADR-0013 / C-55.
///
/// The golden seals on stream 0 (audio) for leaf 3, so the composite KID is
/// `(3 << 8) | 0 = 0x0300` (ADR-0035) and the header carries the extended
/// 2-byte KID `0x03 0x00`.
const GOLDEN_SEED: [u8; 32] = [0x42; 32];
const GOLDEN_LEAF: u32 = 3;
const GOLDEN_STREAM: u8 = 0;
const GOLDEN_PLAINTEXT: &[u8] = b"lakko sframe golden vector v1";
const GOLDEN_WIRE_HEX: &str =
    "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a";

/// Decode the golden wire hex into bytes.
fn golden_wire() -> Vec<u8> {
    (0..GOLDEN_WIRE_HEX.len() / 2)
        .map(|i| {
            u8::from_str_radix(&GOLDEN_WIRE_HEX[i * 2..i * 2 + 2], 16).expect("golden hex byte")
        })
        .collect()
}

/// Build a fresh `ParticipantKey` for the golden seed + leaf through the C ABI.
/// Returns the `(CallKey, ParticipantKey)` raw pointers; the caller frees both
/// with the matched `_free` symbols.
///
/// # Safety
///
/// The returned pointers are live and owned by the caller, who MUST free the
/// `ParticipantKey` before the `CallKey` (the participant key borrows nothing
/// across the FFI, but freeing in that order matches the shim's teardown).
unsafe fn golden_participant_key() -> (*mut CallKey, *mut ParticipantKey) {
    let mut call_key: *mut CallKey = ptr::null_mut();
    // SAFETY: seed is 32 valid readable bytes; `call_key` out-pointer is a
    // writable local.
    let status = unsafe {
        lakko_sframe_call_key_new(GOLDEN_SEED.as_ptr(), GOLDEN_SEED.len(), &mut call_key)
    };
    assert_eq!(status, LakkoSframeStatus::Ok, "call_key_new must succeed");
    assert!(!call_key.is_null(), "call_key_new must populate the handle");

    let mut pk: *mut ParticipantKey = ptr::null_mut();
    // SAFETY: `call_key` is the live pointer just constructed; `pk` out-pointer
    // is a writable local. The canonical golden is sealed on stream 0 (audio,
    // ADR-0035), matching the core / web / iOS / Android golden vectors.
    let status =
        unsafe { lakko_sframe_participant_key_new(call_key, GOLDEN_LEAF, GOLDEN_STREAM, &mut pk) };
    assert_eq!(
        status,
        LakkoSframeStatus::Ok,
        "participant_key_new must succeed"
    );
    assert!(
        !pk.is_null(),
        "participant_key_new must populate the handle"
    );
    (call_key, pk)
}

/// The C ABI `seal_framed` on the golden seed + leaf seals a frame whose header
/// carries the composite KID `0x0300` and a 64-bit counter one above the key's
/// random start (ADR-0069), and the frame opens under an independently derived
/// key. It is NOT the golden bytes: a fresh key no longer seals a predictable
/// frame, and nothing on the C ABI sets a counter.
#[test]
fn seal_framed_via_c_abi_carries_the_kid_and_a_64_bit_counter() {
    // SAFETY: every pointer below comes from the matched C ABI constructors and
    // is freed before return; the out-pointers are writable locals; the
    // plaintext slice is valid for its length.
    unsafe {
        let (call_key, pk) = golden_participant_key();
        let start = lakko_sframe_participant_key_counter(pk);
        assert!(start < 1 << 62, "a key starts below 2^62 (ADR-0069)");

        let mut wire_ptr: *mut u8 = ptr::null_mut();
        let mut wire_len: usize = 0;
        let status = lakko_sframe_seal_framed(
            pk,
            GOLDEN_PLAINTEXT.as_ptr(),
            GOLDEN_PLAINTEXT.len(),
            &mut wire_ptr,
            &mut wire_len,
        );
        assert_eq!(status, LakkoSframeStatus::Ok, "seal_framed must succeed");
        assert!(!wire_ptr.is_null(), "seal_framed must populate the wire");

        // SAFETY: `seal_framed` populated (ptr, len) for `wire_len` bytes.
        let wire = std::slice::from_raw_parts(wire_ptr, wire_len).to_vec();

        // Read the header back through the C ABI. `unwrap_frame` reports the
        // whole KID in its `sender_leaf` out-parameter (it does not split the
        // composite), so the golden KID 0x0300 reads as 0x0300.
        let mut kid: u32 = 0;
        let mut counter: u64 = 0;
        let mut ct_ptr: *mut u8 = ptr::null_mut();
        let mut ct_len: usize = 0;
        assert_eq!(
            lakko_sframe_unwrap_frame(
                wire.as_ptr(),
                wire.len(),
                &mut kid,
                &mut counter,
                &mut ct_ptr,
                &mut ct_len,
            ),
            LakkoSframeStatus::Ok,
        );
        assert_eq!(kid, 0x0300, "header KID is (leaf 3 << 8) | stream 0");
        assert_eq!(counter, start + 1, "first frame carries start + 1");
        assert_eq!(
            lakko_sframe_participant_key_counter(pk),
            counter,
            "the key's counter advanced to the one in the header"
        );
        // The same header at counter 1 is the golden's 3-byte prefix; here the
        // counter field is as wide as the random start needs.
        assert_eq!(wire[1..3], [0x03, 0x00], "KID bytes follow the config");
        lakko_sframe_buffer_free(ct_ptr, ct_len);

        // A SEPARATE key (same seed + leaf, its own random counter) opens it.
        let (recv_call_key, recv_pk) = golden_participant_key();
        let mut pt_ptr: *mut u8 = ptr::null_mut();
        let mut pt_len: usize = 0;
        assert_eq!(
            lakko_sframe_open_framed(recv_pk, wire.as_ptr(), wire.len(), &mut pt_ptr, &mut pt_len),
            LakkoSframeStatus::Ok,
        );
        assert_eq!(
            std::slice::from_raw_parts(pt_ptr, pt_len),
            GOLDEN_PLAINTEXT,
            "a frame the C ABI sealed must open under an independent key"
        );

        lakko_sframe_buffer_free(pt_ptr, pt_len);
        lakko_sframe_buffer_free(wire_ptr, wire_len);
        lakko_sframe_participant_key_free(recv_pk);
        lakko_sframe_call_key_free(recv_call_key);
        lakko_sframe_participant_key_free(pk);
        lakko_sframe_call_key_free(call_key);
    }
}

/// The C ABI `open_framed` recovers the golden plaintext from the canonical
/// cross-stack wire — so a frame web / iOS / Android / the core seals opens on
/// the native media path.
#[test]
fn golden_wire_opens_to_the_plaintext_via_c_abi() {
    let wire = golden_wire();
    // SAFETY: every pointer below comes from the matched C ABI constructors and
    // is freed before return; the wire slice is valid for its length; the
    // out-pointers are writable locals.
    unsafe {
        let (call_key, pk) = golden_participant_key();

        let mut pt_ptr: *mut u8 = ptr::null_mut();
        let mut pt_len: usize = 0;
        let status =
            lakko_sframe_open_framed(pk, wire.as_ptr(), wire.len(), &mut pt_ptr, &mut pt_len);
        assert_eq!(status, LakkoSframeStatus::Ok, "open_framed must succeed");
        assert!(!pt_ptr.is_null(), "open_framed must populate the plaintext");

        // SAFETY: `open_framed` populated (ptr, len) for `pt_len` bytes.
        let plaintext = std::slice::from_raw_parts(pt_ptr, pt_len).to_vec();
        assert_eq!(
            plaintext, GOLDEN_PLAINTEXT,
            "the C ABI open_framed MUST recover the canonical golden plaintext \
             from the cross-stack wire"
        );

        lakko_sframe_buffer_free(pt_ptr, pt_len);
        lakko_sframe_participant_key_free(pk);
        lakko_sframe_call_key_free(call_key);
    }
}

/// Round-trip through the C ABI alone: seal then open on independent keys
/// derived from the same seed + leaf, recovering the plaintext. Guards the
/// composition of the two directions on the golden seed.
#[test]
fn golden_round_trips_through_the_c_abi() {
    // SAFETY: all pointers come from matched C ABI constructors and are freed
    // before return; out-pointers are writable locals; slices are valid.
    unsafe {
        let (sender_call_key, sender_pk) = golden_participant_key();

        let mut wire_ptr: *mut u8 = ptr::null_mut();
        let mut wire_len: usize = 0;
        assert_eq!(
            lakko_sframe_seal_framed(
                sender_pk,
                GOLDEN_PLAINTEXT.as_ptr(),
                GOLDEN_PLAINTEXT.len(),
                &mut wire_ptr,
                &mut wire_len,
            ),
            LakkoSframeStatus::Ok,
        );
        let wire = std::slice::from_raw_parts(wire_ptr, wire_len).to_vec();

        // A SEPARATE receiver key (same seed + leaf) opens what the sender
        // sealed — mirroring the encryptor / decryptor split in the shim.
        let (recv_call_key, recv_pk) = golden_participant_key();
        let mut pt_ptr: *mut u8 = ptr::null_mut();
        let mut pt_len: usize = 0;
        assert_eq!(
            lakko_sframe_open_framed(recv_pk, wire.as_ptr(), wire.len(), &mut pt_ptr, &mut pt_len),
            LakkoSframeStatus::Ok,
        );
        let plaintext = std::slice::from_raw_parts(pt_ptr, pt_len).to_vec();
        assert_eq!(plaintext, GOLDEN_PLAINTEXT);

        lakko_sframe_buffer_free(wire_ptr, wire_len);
        lakko_sframe_buffer_free(pt_ptr, pt_len);
        lakko_sframe_participant_key_free(sender_pk);
        lakko_sframe_call_key_free(sender_call_key);
        lakko_sframe_participant_key_free(recv_pk);
        lakko_sframe_call_key_free(recv_call_key);
    }
}
