// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fork-delta guard for the vendored `webauthn-rs-core` (ADR-0014, G-13,
//! test-plan item 5).
//!
//! Lakko vendors a fork of `webauthn-rs-core` 0.5.5 under
//! `crates/vendor/webauthn-rs-core` and consumes it via `[patch.crates-io]`.
//! The fork contract is intentionally tiny: the vendored `core.rs` is
//! **byte-identical to upstream 0.5.5 except for a single additive method**
//! (`generate_challenge_authenticate_with_challenge`) that lets the auth
//! service inject an action-bound challenge. Crucially, the dangerous
//! WebAuthn assertion-verification path (`authenticate_credential` /
//! `verify_credential_internal`) is upstream-**verbatim** — we do not
//! reimplement or touch it.
//!
//! This guard freezes that reviewed state. Any edit to the vendored
//! `core.rs` — including an accidental change to the verification path —
//! flips the pinned hash and fails CI, forcing a conscious re-review.
//!
//! When the failure is intentional (an upstream re-base, or a deliberate,
//! reviewed change to the fork):
//!   1. `diff` the vendored `core.rs` against the pinned upstream tag and
//!      confirm the ONLY delta is the `generate_challenge_authenticate_with_challenge`
//!      method (zero changes to `authenticate_credential` /
//!      `verify_credential_internal`);
//!   2. update [`PINNED_CORE_RS_SHA256`] below to the new value the test
//!      prints.
//!
//! The pinned value was last confirmed verbatim on 2026-06-04 by diffing
//! against the registry-cached pristine `webauthn-rs-core` 0.5.5: the diff
//! was a single additive hunk (the fork method), zero deletions.

use sha2::{Digest, Sha256};

/// Path to the vendored fork's `core.rs`, relative to this crate's
/// manifest dir (`services/auth`).
const VENDORED_CORE_RS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/vendor/webauthn-rs-core/src/core.rs"
);

/// SHA-256 of the LF-normalized vendored `core.rs`, pinned at the reviewed
/// fork state (upstream 0.5.5 + the one additive method). See the module
/// doc for the re-pin procedure when this legitimately changes.
const PINNED_CORE_RS_SHA256: &str =
    "64fd80f6d7e0bd19efeba854e3938bc3ef754694bc3008163bbc250ed9bd9fd2";

/// Read the vendored `core.rs` and normalize line endings to LF so the
/// hash does not depend on the checkout's autocrlf setting.
fn read_normalized() -> String {
    let raw = std::fs::read_to_string(VENDORED_CORE_RS).unwrap_or_else(|e| {
        panic!("cannot read vendored webauthn-rs-core core.rs at {VENDORED_CORE_RS}: {e}")
    });
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[test]
fn vendored_core_rs_matches_pinned_fork_state() {
    let normalized = read_normalized();
    let actual = sha256_hex(&normalized);
    assert_eq!(
        actual, PINNED_CORE_RS_SHA256,
        "\n\nThe vendored webauthn-rs-core core.rs changed (ADR-0014 fork-delta guard).\n\
         The fork contract is: byte-identical to upstream 0.5.5 EXCEPT the single\n\
         additive `generate_challenge_authenticate_with_challenge` method. The\n\
         verification path (`authenticate_credential` / `verify_credential_internal`)\n\
         must stay upstream-verbatim.\n\
         If this change is intentional, re-diff against the pinned upstream 0.5.5\n\
         tag, confirm the verification path is unchanged, then set\n\
         PINNED_CORE_RS_SHA256 to:\n    {actual}\n"
    );
}

#[test]
fn fork_delta_is_exactly_one_additive_method() {
    let src = read_normalized();

    // Exactly one fork-delta marker block and one injected method — the
    // whole and only delta over upstream.
    assert_eq!(
        src.matches("LAKKO FORK DELTA").count(),
        1,
        "expected exactly one LAKKO FORK DELTA marker in the vendored core.rs"
    );
    assert_eq!(
        src.matches("fn generate_challenge_authenticate_with_challenge")
            .count(),
        1,
        "expected exactly one definition of the injected challenge method"
    );

    // The upstream verification surface must still be present and named as
    // upstream named it — a rename / removal would slip past a reviewer who
    // only eyeballs the diff for additions.
    assert_eq!(
        src.matches("fn authenticate_credential").count(),
        1,
        "upstream `authenticate_credential` must be present exactly once"
    );
    assert_eq!(
        src.matches("fn verify_credential_internal").count(),
        1,
        "upstream `verify_credential_internal` must be present exactly once"
    );
}
