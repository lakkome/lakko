// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The fuzz entry points run on stable over random inputs of the interesting
//! lengths, so their assertions are exercised without `cargo fuzz`.

use lakko_anon_cred::fuzz_api;
use lakko_anon_cred::{
    CredentialSecret, IssuerSecretKey, MasterSecret, PollScope, CREDENTIAL_LEN,
    ISSUANCE_REQUEST_LEN, ISSUANCE_RESPONSE_LEN, PRESENTATION_LEN, REGISTRATION_LEN,
};
use proptest::prelude::*;

fn lengths() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        proptest::collection::vec(any::<u8>(), CREDENTIAL_LEN),
        proptest::collection::vec(any::<u8>(), ISSUANCE_REQUEST_LEN),
        proptest::collection::vec(any::<u8>(), REGISTRATION_LEN),
        proptest::collection::vec(any::<u8>(), ISSUANCE_RESPONSE_LEN),
        proptest::collection::vec(any::<u8>(), PRESENTATION_LEN),
        proptest::collection::vec(any::<u8>(), 0..300),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]

    #[test]
    fn fuzz_entry_points_hold_on_random_input(mut data in lengths(), canonical_version in any::<bool>()) {
        if canonical_version && !data.is_empty() {
            data[0] = lakko_anon_cred::WIRE_VERSION;
        }
        fuzz_api::credential_decode(&data);
        fuzz_api::issuance_request_decode(&data);
        fuzz_api::presentation_verify(&data);
    }
}

/// Real wire forms, so the decode-and-round-trip branches run on inputs that
/// decode. A real presentation for another scope must still not verify in the
/// harness's fixed scope.
#[test]
fn fuzz_entry_points_hold_on_real_wire_forms() {
    let master = MasterSecret::from_bytes(fuzz_api::FUZZ_MASTER);
    let key = IssuerSecretKey::derive(&master, fuzz_api::FUZZ_PERIOD).unwrap();
    let secret = CredentialSecret::from_lineage_seed(&[1; 32]).unwrap();
    let registration = secret.register().unwrap().to_bytes();
    let request = secret.issuance_request(&key.public_key()).unwrap();
    let response = key.issue(&request, &secret.commitment()).unwrap();
    let credential = secret
        .finalize_issuance(&key.public_key(), &response)
        .unwrap();
    let presentation = secret
        .present(
            &credential,
            &key.public_key(),
            &PollScope::channel_post([4; 16], 42),
            b"fuzz",
        )
        .unwrap()
        .to_bytes();
    fuzz_api::credential_decode(&credential.to_bytes());
    fuzz_api::issuance_request_decode(&response.to_bytes());
    fuzz_api::presentation_verify(&presentation);
    // The registration and request are genuine, so the harness's "nothing
    // verifies" assertions must not see them: the registration's proof would
    // verify. Check only that they decode.
    assert!(lakko_anon_cred::CommitmentRegistration::decode(&registration).is_ok());
    assert!(lakko_anon_cred::IssuanceRequest::decode(&request.to_bytes()).is_ok());
}
