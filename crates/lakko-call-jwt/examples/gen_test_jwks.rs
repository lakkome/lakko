// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Generate the fixture JWKS used by `ci/sfu-e2e/` (G3-4).
//!
//! The CI E2E gate brings up a Dockerised `lakko-svc-sfu`
//! container that fetches its JWKS from a tiny nginx
//! sidecar serving a static JSON file. That file's
//! contents — the JWK shape produced by
//! `lakko-call-jwt::Jwk::from_signing_key` — must match
//! exactly what the test-side `CallSigningKey::from_seed`
//! mints, otherwise admission fails with `UnknownKid`.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p lakko-call-jwt --example gen_test_jwks \
//!   > ci/sfu-e2e/test-jwks/jwks.json
//! ```
//!
//! The seed is `0x42` repeated 32 times — pinned in the
//! sibling integration test
//! (`services/sfu/tests/e2e_dockerized.rs`) so the CI
//! workflow can mint matching tokens without sharing
//! state through any other channel.

use lakko_call_jwt::{CallSigningKey, Jwk, Jwks};

fn main() {
    let signing = CallSigningKey::from_seed(&[0x42_u8; 32]);
    let jwks = Jwks {
        keys: vec![Jwk::from_signing_key(&signing)],
    };
    let json = serde_json::to_string_pretty(&jwks).expect("Jwks serialises as JSON");
    println!("{json}");
}
