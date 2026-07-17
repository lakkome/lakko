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
//! and asserts the C ABI agrees: a frame the native media shim seals is the
//! EXACT wire the web / core / iOS / Android stacks open, and a frame any of
//! them seals opens here. Because the AEAD AAD is the RFC 9605 §4.3 wire header
//! (KID + CTR) and never host RTP `additional_data` (ADR-0013 / C-55), this
//! byte-identicality is what lets a frame cross stacks at all.
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
    lakko_sframe_open_framed, lakko_sframe_participant_key_free, lakko_sframe_participant_key_new,
    lakko_sframe_seal_framed, CallKey, LakkoSframeStatus, ParticipantKey,
    LAKKO_SFRAME_MEDIA_TYPE_AUDIO,
};

/// The canonical cross-stack golden vector — byte-identical to the constants
/// pinned in the Rust core, web, iOS, and Android golden tests. If the C ABI
/// (the path the native media shims call) produced different bytes, a frame
/// sealed natively would not open on web / the core, breaking ADR-0013 / C-55.
const GOLDEN_SEED: [u8; 32] = [0x42; 32];
const GOLDEN_LEAF: u32 = 3;
const GOLDEN_PLAINTEXT: &[u8] = b"lakko sframe golden vector v1";
const GOLDEN_WIRE_HEX: &str =
    "31093c4f19a3b815c0a17a8ad09d2df18c6971987eb1c214484e9ebcc4f92b4c04e5c80bc145a9f23e1c60e2faa4";

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
    // is a writable local. The canonical golden is sealed on the AUDIO stream
    // (ADR-0034), matching the core / web / iOS / Android golden vectors.
    let status = unsafe {
        lakko_sframe_participant_key_new(
            call_key,
            GOLDEN_LEAF,
            LAKKO_SFRAME_MEDIA_TYPE_AUDIO,
            &mut pk,
        )
    };
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

/// The C ABI `seal_framed` on the golden seed + leaf + plaintext produces the
/// EXACT canonical cross-stack wire — so a frame the native media shim seals is
/// byte-identical to what web / iOS / Android / the core emit.
#[test]
fn golden_vector_seals_to_the_canonical_wire_via_c_abi() {
    // SAFETY: every pointer below comes from the matched C ABI constructors and
    // is freed before return; the out-pointers are writable locals; the
    // plaintext slice is valid for its length.
    unsafe {
        let (call_key, pk) = golden_participant_key();

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
        assert_eq!(
            wire,
            golden_wire(),
            "the C ABI seal_framed MUST produce the canonical cross-stack golden \
             wire — a native-sealed frame must be byte-identical to web / iOS / \
             Android / the core (ADR-0013 / C-55)"
        );

        lakko_sframe_buffer_free(wire_ptr, wire_len);
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
/// composition (the two tests above pin each direction against the golden;
/// this pins that the two directions compose).
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
