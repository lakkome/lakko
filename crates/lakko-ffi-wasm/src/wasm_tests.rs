// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Browser-runtime smoke tests for the wasm-bindgen surface
//! (Phase 1.16f3).
//!
//! ## Why a separate test target
//!
//! The host-side `#[cfg(test)] mod tests` blocks scattered through
//! `mls.rs`, `auth.rs`, `mnemonic.rs`, … exercise the **Rust API**
//! of every wasm-bindgen wrapper. They run on the development
//! machine's native target via `cargo test -p lakko-ffi-wasm` and
//! catch ~95 % of regressions.
//!
//! What they cannot catch:
//!
//!   * The WASM module actually loads and instantiates inside a JS
//!     runtime. A botched feature flag (forgetting `getrandom = "js"`,
//!     missing `openmls = "js"`) would break wasm32 specifically;
//!     the host build still passes.
//!   * `getrandom` reaches `globalThis.crypto.getRandomValues`
//!     under the `js` feature and produces non-deterministic bytes.
//!     Host tests use the OS RNG path; wasm32 path is a different
//!     code branch.
//!   * `JsError` round-trips with a usable message. On the host,
//!     `Result::Err(JsError)` is just a value; in JS land it surfaces
//!     as a thrown `Error` whose `.message` matters for the UI.
//!
//! These three properties want a runtime that is at least
//! WASM-shaped — Node's V8 + the WASM/getrandom shim suffice.
//!
//! ## How to run
//!
//! ```text
//! just test-wasm                  # via Node (no browser driver needed)
//! ```
//!
//! Equivalent direct invocation:
//!
//! ```text
//! wasm-pack test --node crates/lakko-ffi-wasm
//! ```
//!
//! For browser-only paths (IndexedDB, structured-clone with
//! non-extractable `CryptoKey`) swap `--node` for `--headless --chrome`
//! / `--firefox` / `--safari`. Those are deferred to a follow-up
//! sub-sprint that wires the IDB-backed `Store` in 1.13c.
//!
//! ## What lives here
//!
//! Three smoke tests covering the three properties above. Adding a
//! new test should preserve the "smoke, not coverage" rule — full
//! coverage stays in the host-side `tests` modules where it is
//! cheap to run; this file only catches WASM-shaped regressions
//! that the host can't see.

#![cfg(all(test, target_arch = "wasm32"))]

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;

use crate::contacts::InviteCapability;
use crate::mls::{Group, Identity};

// `wasm-pack test --node` selects the Node V8 runner; no
// `wasm_bindgen_test_configure!` invocation is needed
// (wasm-bindgen-test 0.3.70+ removed `run_in_node` from the
// macro grammar — the runner picks the host from CLI flags).
// For browser-only paths (IndexedDB, structured-clone with a
// non-extractable `CryptoKey`), swap `--node` for
// `--headless --chrome` / `--firefox` / `--safari` and add a
// `wasm_bindgen_test_configure!(run_in_browser);` line above.

/// Smoke: `Identity::new(name)` must complete in a WASM runtime.
/// This calls `crypto.getRandomValues` under the hood (Ed25519
/// keypair generation goes through `lakko-crypto::SigningKey` ->
/// `getrandom`), so a missing `js` feature on `getrandom` /
/// `openmls` would surface as a panic on `unimplemented` rather
/// than a clean error. The host-side test target uses the OS
/// RNG and would never catch that.
#[wasm_bindgen_test]
fn identity_new_completes_in_wasm_runtime() {
    let id = Identity::new("smoke").expect("Identity::new must build under wasm32");
    assert_eq!(
        id.signature_public_key().len(),
        32,
        "Ed25519 pubkey is 32 bytes",
    );
}

/// `Identity::from_seed` -> `snapshot_storage` -> fresh
/// `Identity::from_seed(same seed)` -> `restore_storage` -> a
/// second snapshot must produce **byte-identical** bytes. This
/// exercises the full WASM bridge for the persistence path
/// (1.16e6a + 1.16e6c1) — the hot path for every reload of
/// `apps/web` after 1.16e6c3.
#[wasm_bindgen_test]
fn identity_snapshot_round_trips_through_wasm() {
    let seed = [0x42u8; 64];
    let id = Identity::from_seed(&seed).expect("from_seed");
    let snapshot = id.snapshot_storage().expect("snapshot");
    assert!(!snapshot.is_empty(), "snapshot must carry bytes");

    let restored = Identity::from_seed(&seed).expect("from_seed (restored)");
    restored
        .restore_storage(&snapshot)
        .expect("restore_storage on a same-seed identity must succeed");
    let snapshot2 = restored.snapshot_storage().expect("snapshot 2");

    assert_eq!(
        snapshot, snapshot2,
        "snapshot/restore/snapshot is a no-op (byte-identical)",
    );
}

/// Truncated snapshot must produce a `JsError` whose JS-visible
/// message names the failure. `apps/web/src/chat/rehydrate.ts`
/// (1.16e6c3) catches and logs this string when a corrupt blob
/// hits a hydrate; if the message becomes empty or pure
/// "JsValue { ... }" debug noise, the user-facing log loses its
/// teeth.
#[wasm_bindgen_test]
fn restore_storage_returns_helpful_error_for_truncated_snapshot() {
    let id = Identity::from_seed(&[0x07u8; 64]).expect("from_seed");
    // 3 bytes — far below the smallest valid snapshot frame.
    let truncated = [0x00u8, 0x00, 0x00];

    let err = id
        .restore_storage(&truncated)
        .expect_err("3-byte buffer must be rejected");

    // JsError has no public accessor for the wrapped message in
    // 0.2.x; round-tripping it through Display via `format!` on the
    // host fails because JsError's Display is empty. Inside the WASM
    // runtime, however, the value surfaces as a JS `Error` with the
    // expected `.message`. We fish the message out via the JS-side
    // `js_sys::Error` cast, mirroring how the JS caller would read
    // it.
    let js_value: JsValue = err.into();
    let js_error: js_sys::Error = js_value
        .dyn_into()
        .expect("JsError must surface as a real JS Error");
    let message = js_error.message().as_string().unwrap_or_default();
    assert!(
        !message.is_empty(),
        "truncated-snapshot error must carry a non-empty .message; got {message:?}",
    );
}

/// `InviteCapability::compose` -> `decodeAndVerify` round-trip
/// through the WASM bridge (1.18b). The host-side test in
/// `contacts::tests::invite_capability_roundtrip_happy_path`
/// covers the same shape on the native target; this version
/// confirms the wasm-bindgen surface (Vec<u8> args / String
/// args / u64 args / bool args) survives the JS↔WASM boundary
/// in a real V8 runtime.
#[wasm_bindgen_test]
fn invite_capability_round_trips_through_wasm() {
    let owner_seed = [9u8; 32];
    let owner_account_id = [0xA1u8; 16];
    // Build the X25519 pubkey from a known secret to keep the
    // assertion deterministic.
    let x25519_secret = lakko_crypto::agreement::SecretKey::from_bytes([0xB2; 32]);
    let owner_x25519_pubkey = x25519_secret.public_key().to_bytes();

    let wire = InviteCapability::compose(
        &owner_seed,
        &owner_account_id,
        &owner_x25519_pubkey,
        1_700_000_000_000,
        0,
        true,
        "alice@home",
    )
    .expect("compose");

    let cap = InviteCapability::decode_and_verify(&wire).expect("decode");
    assert_eq!(cap.owner_account_id(), owner_account_id.to_vec());
    assert_eq!(cap.owner_x25519_pubkey(), owner_x25519_pubkey.to_vec());
    assert_eq!(cap.expiry_ms(), 1_700_000_000_000);
    assert_eq!(cap.max_uses(), 0);
    assert!(cap.bootstrap_referral());
    assert_eq!(cap.label(), "alice@home");
    assert!(cap.is_valid_now(0));
    assert!(!cap.is_valid_now(1_700_000_000_000));
}

/// `Group.members()` (Phase 1.18h) returns a flat array of
/// `GroupMember` instances at the current epoch. Verify
/// the array shape — length matches `memberCount`, JsValues
/// are non-null. The per-member field round-trip is tested
/// from the JS layer in `apps/web` (it can dyn_into through
/// the actual wasm-bindgen runtime); inside this test target
/// the `JsCast` impl isn't generated for the inner struct, so
/// we settle for the array-shape pin here.
#[wasm_bindgen_test]
fn group_members_returns_one_entry_for_a_fresh_group() {
    let alice = Identity::from_seed(&[0xC0u8; 64]).expect("alice");
    let group = Group::new(&alice).expect("group");

    let members = group.members().expect("members");
    assert_eq!(members.len(), 1, "one-of-one group has one leaf");
    assert!(!members[0].is_null());
    assert!(!members[0].is_undefined());

    assert_eq!(
        group.member_count().expect("member_count"),
        1,
        "members.len() must agree with memberCount",
    );
}
