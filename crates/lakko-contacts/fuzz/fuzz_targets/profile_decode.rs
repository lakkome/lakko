// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_profile` (ADR-0048).
//!
//! A profile envelope arrives two ways, both attacker-shaped: as an MLS
//! application message on a group journal (any member, or the zero-trust
//! DS replaying a corrupt row), and inside a contact request's sealed
//! envelope (any stranger who knows the recipient's public key). In both
//! cases the bytes reach this bounded, fail-closed decoder before any
//! state is consulted. Unlike its fixed-length siblings the payload is
//! variable-length — two one-byte length prefixes, UTF-8 validation, a
//! Unicode format-character table and the canonical-username rule — so
//! there is real parser surface here.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder;
//! * **round-trip stability** — anything that decodes re-composes (the
//!   composer must accept whatever the decoder accepted, since both run
//!   one validator) to bytes that decode to the identical profile, and
//!   those bytes are exactly the input: the encoding is canonical, so an
//!   accepted envelope has one spelling;
//! * a decoded envelope is within the advertised length bounds;
//! * the v3 bounds (ADR-0060): a certificate is exactly its length and
//!   never without a username, at most two grants, one per class, of a
//!   class a profile may carry;
//! * the order is total and panic-free, and a profile is never newer than
//!   itself.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::profile::{
    compose_profile, decode_profile, is_profile_grant_class, profile_is_newer, MAX_PROFILE_GRANTS,
    PROFILE_ENVELOPE_MAX_LEN, PROFILE_ENVELOPE_MIN_LEN, USERNAME_CERT_LEN,
};

fuzz_target!(|data: &[u8]| {
    let Ok(profile) = decode_profile(data) else {
        return;
    };

    assert!((PROFILE_ENVELOPE_MIN_LEN..=PROFILE_ENVELOPE_MAX_LEN).contains(&data.len()));
    // v3 (ADR-0060): the certificate and grant bounds hold on anything
    // decoded.
    if let Some(cert) = &profile.username_certificate {
        assert_eq!(cert.len(), USERNAME_CERT_LEN);
        assert!(profile.username.is_some(), "a certificate never without a username");
    }
    assert!(profile.grants.len() <= MAX_PROFILE_GRANTS);
    for (i, grant) in profile.grants.iter().enumerate() {
        assert!(is_profile_grant_class(grant.class));
        assert!(profile.grants[..i].iter().all(|g| g.class != grant.class));
    }

    let recomposed = compose_profile(&profile).expect("compose must accept what decode accepted");
    assert_eq!(recomposed.as_slice(), data, "the encoding must be canonical");
    let again = decode_profile(&recomposed).expect("a composed profile must decode");
    assert_eq!(again, profile);

    assert!(!profile_is_newer(profile.updated_at_ms, profile.updated_at_ms));
    let _ = profile_is_newer(profile.updated_at_ms, u64::MAX);
});
