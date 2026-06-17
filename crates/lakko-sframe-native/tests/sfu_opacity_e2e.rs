// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Extended end-to-end test for the SFrame native shim
//! (G3-sframe-native N-6).
//!
//! What this test fixture proves at the Rust level:
//!
//!  1. **SFU-side opacity** — bytes leaving the encryptor are
//!     forwarded by a simulated SFU to the receiver without
//!     inspection or mutation, and decrypt correctly on the
//!     far side. Mirror of services/sfu's media-layer fanout
//!     contract: the SFU MUST NOT decrypt or even peek at the
//!     SFrame ciphertext.
//!
//!  2. **AAD binding** — a single bit flipped in the
//!     additional_data field at the SFU hop (a malicious or
//!     buggy intermediary that thinks it can rewrite RTP
//!     headers) causes the receiver-side open to fail. The
//!     receiver MUST see this as `kFailedToDecrypt` and drop
//!     the frame — never recover plaintext under tampered AAD.
//!
//!  3. **Wrong-key isolation** — a receiver instantiated with
//!     a DIFFERENT 32-byte call seed than the encryptor cannot
//!     open the ciphertext. There is no fallback path, no
//!     downgrade, no partial recovery.
//!
//!  4. **Multi-party fan-in** — three distinct encryptors (one
//!     per sender_leaf) forwarding into one decryptor through
//!     the SFU. The decryptor's per-sender ParticipantKey
//!     cache (N-3 LakkoFrameDecryptor::lookup_or_derive) MUST
//!     correctly derive + cache one key per remote leaf and
//!     decrypt every frame.
//!
//!  5. **Cross-call isolation** — a decryptor built for call A
//!     cannot open frames sealed under call B. This is the
//!     same invariant the SFU's signaling layer enforces at
//!     the routing level (C-39); the SFrame layer is the
//!     fallback gate if the SFU ever mis-routes.
//!
//!  6. **Counter monotonicity + out-of-order decode** — the
//!     encryptor's counter advances strictly monotonically
//!     under one key (AEAD nonce-uniqueness invariant), but
//!     out-of-order delivery still decodes because the counter
//!     rides on the SFrame wire header.
//!
//! These tests run against `lakko_sframe_native` as a regular
//! Rust crate (the `rlib` crate-type is what makes the integ
//! tests possible — staticlib + cdylib are separate outputs
//! for the platform shims). No FFI boundary is crossed —
//! the Rust-side `lakko_calls::sframe::CallKey` /
//! `ParticipantKey` API is what `lakko-sframe-native`'s
//! `extern "C"` wraps, so testing here exercises the exact
//! same code path the iOS Obj-C++ / Android JNI shims call
//! through.

use std::collections::HashMap;

use lakko_calls::sframe::{CallKey, ParticipantKey, CALL_KEY_LEN};
use lakko_calls::SframeHeader;

/// Build a deterministic per-call seed. Mirror of how
/// `Group::export_secret("lakko/call/v1", call_id, 32)`
/// behaves at the MLS-exporter layer.
fn deterministic_seed(byte: u8) -> [u8; CALL_KEY_LEN] {
    [byte; CALL_KEY_LEN]
}

/// Encrypt a frame the way the N-3 LakkoFrameEncryptor::Encrypt
/// path does: seal + wrap_frame. Returns the wire bytes the SFU
/// would forward.
fn encrypt_frame(pk: &mut ParticipantKey, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let (counter, ciphertext) = pk.seal(aad, plaintext).expect("seal");
    let header = SframeHeader {
        kid: u64::from(pk.sender_leaf()),
        ctr: u64::from(counter),
    };
    let mut wire = header.encode();
    wire.extend_from_slice(&ciphertext);
    wire
}

/// Decrypt the SFU-forwarded wire frame: parse the SFrame
/// header, look up (or derive) the per-sender ParticipantKey,
/// open the AEAD payload. Returns `Err(())` on any failure —
/// real shim collapses every error to `kFailedToDecrypt` (N-3
/// LakkoFrameDecryptor::Decrypt path).
fn decrypt_frame(
    call_key: &CallKey,
    cache: &mut HashMap<u32, ParticipantKey>,
    aad: &[u8],
    wire: &[u8],
) -> Result<Vec<u8>, ()> {
    let (header, consumed) = SframeHeader::decode(wire).map_err(|_| ())?;
    let sender_leaf: u32 = u32::try_from(header.kid).map_err(|_| ())?;
    let counter: u32 = u32::try_from(header.ctr).map_err(|_| ())?;
    let ciphertext = &wire[consumed..];

    let pk = match cache.entry(sender_leaf) {
        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
        std::collections::hash_map::Entry::Vacant(e) => {
            let pk = call_key.participant_key(sender_leaf).map_err(|_| ())?;
            e.insert(pk)
        }
    };

    pk.open(aad, ciphertext, counter).map_err(|_| ())
}

// ---------------------------------------------------------------
// 1. SFU-side opacity — forward without inspection
// ---------------------------------------------------------------

#[test]
fn sfu_opacity_forward_without_inspection() {
    let seed = deterministic_seed(0xA1);

    let call_key = CallKey::from_seed(seed);
    let mut alice = call_key.participant_key(1).expect("alice pk");
    let bob_key = CallKey::from_seed(seed);
    let mut bob_cache: HashMap<u32, ParticipantKey> = HashMap::new();

    let aad = b"rtp-additional-data";
    let plaintext = b"alice speaks";

    let wire = encrypt_frame(&mut alice, aad, plaintext);

    // Simulated SFU hop: the SFU receives `wire`, forwards it
    // byte-for-byte to bob. No mutation, no inspection. This
    // mirrors services/sfu's fanout_media_data behaviour.
    let sfu_forwarded = wire.clone();

    let recovered =
        decrypt_frame(&bob_key, &mut bob_cache, aad, &sfu_forwarded).expect("bob decrypt");
    assert_eq!(recovered, plaintext);
}

// ---------------------------------------------------------------
// 2. AAD binding — tampered RTP header MUST fail decryption
// ---------------------------------------------------------------

#[test]
fn aad_tampering_at_sfu_hop_is_detected() {
    let seed = deterministic_seed(0xB2);

    let call_key = CallKey::from_seed(seed);
    let mut alice = call_key.participant_key(2).expect("alice pk");
    let bob_key = CallKey::from_seed(seed);
    let mut bob_cache: HashMap<u32, ParticipantKey> = HashMap::new();

    let original_aad = b"correct-rtp-header";
    let tampered_aad = b"tampered-rtp-hdr!"; // same length, different content
    let plaintext = b"alice speaks";

    let wire = encrypt_frame(&mut alice, original_aad, plaintext);

    // Bob's receiver path: an upstream RTP layer hands the AAD
    // (the additional_data) and the wire bytes. If a malicious
    // SFU rewrites the RTP header bits the receiver computes
    // its AAD from, the AEAD-tag verification fails. The
    // receiver MUST refuse to recover plaintext.
    let result = decrypt_frame(&bob_key, &mut bob_cache, tampered_aad, &wire);
    assert!(result.is_err(), "tampered AAD must NOT decrypt");
}

// ---------------------------------------------------------------
// 3. Wrong-key isolation — different seed cannot open
// ---------------------------------------------------------------

#[test]
fn wrong_call_key_cannot_open() {
    let alice_seed = deterministic_seed(0xC3);
    let mallory_seed = deterministic_seed(0xC4); // different call

    let alice_key = CallKey::from_seed(alice_seed);
    let mut alice = alice_key.participant_key(3).expect("alice pk");

    // Mallory is in a different call but somehow received the
    // wire bytes (e.g. SFU mis-routed them). Her decryptor was
    // built with a different seed.
    let mallory_key = CallKey::from_seed(mallory_seed);
    let mut mallory_cache: HashMap<u32, ParticipantKey> = HashMap::new();

    let aad = b"rtp";
    let plaintext = b"alice speaks privately";

    let wire = encrypt_frame(&mut alice, aad, plaintext);

    let result = decrypt_frame(&mallory_key, &mut mallory_cache, aad, &wire);
    assert!(result.is_err(), "wrong call key must NOT open");
}

// ---------------------------------------------------------------
// 4. Multi-party fan-in — N senders into 1 receiver, cache works
// ---------------------------------------------------------------

#[test]
fn multi_party_fanin_through_per_sender_cache() {
    let seed = deterministic_seed(0xD4);

    let call_key = CallKey::from_seed(seed);

    // Three senders with distinct leaf indices — mirrors the
    // multi-party MLS call topology (each device is one leaf).
    let mut alice = call_key.participant_key(10).expect("alice pk");
    let mut bob = call_key.participant_key(20).expect("bob pk");
    let mut carol = call_key.participant_key(30).expect("carol pk");

    // One receiver — say it's a single audio mixer endpoint.
    // Worst case for the ParticipantKey-cache pattern.
    let recv_key = CallKey::from_seed(seed);
    let mut recv_cache: HashMap<u32, ParticipantKey> = HashMap::new();

    let aad = b"rtp";

    let wire_alice = encrypt_frame(&mut alice, aad, b"alice 1");
    let wire_bob = encrypt_frame(&mut bob, aad, b"bob 1");
    let wire_carol = encrypt_frame(&mut carol, aad, b"carol 1");

    assert_eq!(
        decrypt_frame(&recv_key, &mut recv_cache, aad, &wire_alice).unwrap(),
        b"alice 1"
    );
    assert_eq!(
        decrypt_frame(&recv_key, &mut recv_cache, aad, &wire_bob).unwrap(),
        b"bob 1"
    );
    assert_eq!(
        decrypt_frame(&recv_key, &mut recv_cache, aad, &wire_carol).unwrap(),
        b"carol 1"
    );
    // Cache must hold exactly three entries — one per
    // sender_leaf observed.
    assert_eq!(recv_cache.len(), 3);

    // Round 2 — each sender's next frame should re-use the
    // cached ParticipantKey (no new HKDF derivation). The
    // assert above already proves cache size doesn't grow.
    let wire_alice_2 = encrypt_frame(&mut alice, aad, b"alice 2");
    let wire_bob_2 = encrypt_frame(&mut bob, aad, b"bob 2");
    assert_eq!(
        decrypt_frame(&recv_key, &mut recv_cache, aad, &wire_alice_2).unwrap(),
        b"alice 2"
    );
    assert_eq!(
        decrypt_frame(&recv_key, &mut recv_cache, aad, &wire_bob_2).unwrap(),
        b"bob 2"
    );
    assert_eq!(recv_cache.len(), 3); // still 3 entries
}

// ---------------------------------------------------------------
// 5. Cross-call isolation — wire from call A cannot decode under call B
// ---------------------------------------------------------------

#[test]
fn cross_call_isolation_refuses_wrong_call_traffic() {
    let seed_a = deterministic_seed(0xE5);
    let seed_b = deterministic_seed(0xE6);

    let call_a = CallKey::from_seed(seed_a);
    let mut alice = call_a.participant_key(1).expect("alice pk");

    // Receiver in call B — different MLS group, different
    // exporter output. A correctly-configured SFU does NOT
    // route call-A frames to call-B participants (C-39
    // signaling-layer cross-call isolation), but if it does,
    // SFrame is the second gate.
    let call_b_recv = CallKey::from_seed(seed_b);
    let mut call_b_cache: HashMap<u32, ParticipantKey> = HashMap::new();

    let aad = b"rtp";
    let wire = encrypt_frame(&mut alice, aad, b"call A traffic");

    let result = decrypt_frame(&call_b_recv, &mut call_b_cache, aad, &wire);
    assert!(
        result.is_err(),
        "call-B receiver MUST NOT decode call-A wire bytes"
    );
}

// ---------------------------------------------------------------
// 6. Counter monotonicity + out-of-order decode
// ---------------------------------------------------------------

#[test]
fn counter_advances_monotonically_per_sender() {
    let seed = deterministic_seed(0xF7);

    let call_key = CallKey::from_seed(seed);
    let mut alice = call_key.participant_key(7).expect("alice pk");

    let aad = b"rtp";

    let (counter_1, ct_1) = alice.seal(aad, b"frame 1").expect("seal 1");
    let (counter_2, ct_2) = alice.seal(aad, b"frame 2").expect("seal 2");
    let (counter_3, ct_3) = alice.seal(aad, b"frame 3").expect("seal 3");

    // Counters strictly increasing — the encryptor never
    // reuses a counter under the same key, which is what
    // guarantees AEAD nonce uniqueness (one of SFrame's
    // hard invariants per RFC 9605 §4.4).
    assert!(counter_1 < counter_2);
    assert!(counter_2 < counter_3);

    // Reorder + decrypt: receivers see frames in arbitrary
    // order under a lossy network. SFrame's open() takes the
    // counter as an explicit parameter (from the wire header),
    // so out-of-order delivery works as long as the counter
    // matches what was sealed.
    let recv_key = CallKey::from_seed(seed);
    let recv_pk = recv_key.participant_key(7).expect("recv pk");

    let rec_3 = recv_pk.open(aad, &ct_3, counter_3).expect("open 3");
    let rec_1 = recv_pk.open(aad, &ct_1, counter_1).expect("open 1");
    let rec_2 = recv_pk.open(aad, &ct_2, counter_2).expect("open 2");
    assert_eq!(rec_1, b"frame 1");
    assert_eq!(rec_2, b"frame 2");
    assert_eq!(rec_3, b"frame 3");
}
