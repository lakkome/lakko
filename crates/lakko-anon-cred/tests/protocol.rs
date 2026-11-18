// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end and property tests of the credential (ADR-0090 4.7).

use lakko_anon_cred::{
    verify_registration, AnonCredError, Commitment, CommitmentRegistration, Credential,
    CredentialSecret, CredentialVerifier, IssuanceRequest, IssuanceResponse, IssuerPublicKey,
    IssuerSecretKey, MasterSecret, PollScope, Presentation, CREDENTIAL_LEN, ISSUANCE_REQUEST_LEN,
    ISSUANCE_RESPONSE_LEN, MAX_MESSAGE_LEN, PRESENTATION_LEN, REGISTRATION_LEN,
};
use proptest::prelude::*;

const MASTER: [u8; 32] = [0x5a; 32];
const PERIOD: u64 = 680;

fn master() -> MasterSecret {
    MasterSecret::from_bytes(MASTER)
}

fn issuer(period: u64) -> IssuerSecretKey {
    IssuerSecretKey::derive(&master(), period).unwrap()
}

/// Run registration and issuance over the wire forms; return the secret, the
/// public key and the credential.
fn credential_for(seed: [u8; 32], period: u64) -> (CredentialSecret, IssuerPublicKey, Credential) {
    let secret = CredentialSecret::from_lineage_seed(&seed).unwrap();
    let registration =
        CommitmentRegistration::decode(&secret.register().unwrap().to_bytes()).unwrap();
    let stored = verify_registration(&registration).unwrap();
    let key = issuer(period);
    let public = IssuerPublicKey::decode(period, &key.public_key().to_bytes()).unwrap();
    let request =
        IssuanceRequest::decode(&secret.issuance_request(&public).unwrap().to_bytes()).unwrap();
    let response =
        IssuanceResponse::decode(&key.issue(&request, &stored).unwrap().to_bytes()).unwrap();
    let credential = secret.finalize_issuance(&public, &response).unwrap();
    (secret, public, credential)
}

fn verifier() -> CredentialVerifier {
    CredentialVerifier::new(master())
}

fn chunks(bytes: &[u8]) -> Vec<[u8; 32]> {
    // The 32-byte fields of a presentation after `version || period`.
    bytes[9..]
        .chunks_exact(32)
        .map(|c| c.try_into().unwrap())
        .collect()
}

#[test]
fn wire_lengths() {
    assert_eq!(REGISTRATION_LEN, 97);
    assert_eq!(ISSUANCE_REQUEST_LEN, 73);
    assert_eq!(ISSUANCE_RESPONSE_LEN, 233);
    assert_eq!(CREDENTIAL_LEN, 73);
    assert_eq!(PRESENTATION_LEN, 265);
}

#[test]
fn full_round_trip_verifies_and_reports_the_tag() {
    let (secret, public, credential) = credential_for([1; 32], PERIOD);
    let stored = Credential::decode(&credential.to_bytes()).unwrap();
    assert_eq!(stored, credential);
    let scope = PollScope::channel_post([9; 16], 77);
    let presentation = secret.present(&stored, &public, &scope, b"labels").unwrap();
    let bytes = presentation.to_bytes();
    assert_eq!(bytes.len(), PRESENTATION_LEN);
    let verified = verifier()
        .verify(&bytes, &scope, b"labels", PERIOD)
        .unwrap();
    assert_eq!(verified.tag, presentation.tag());
    assert_eq!(verified.period, PERIOD);
}

#[test]
fn presentation_verifies_only_for_its_post_and_message() {
    let (secret, public, credential) = credential_for([2; 32], PERIOD);
    let scope = PollScope::channel_post([9; 16], 77);
    let bytes = secret
        .present(&credential, &public, &scope, b"m")
        .unwrap()
        .to_bytes();
    let v = verifier();
    assert!(v.verify(&bytes, &scope, b"m", PERIOD).is_ok());
    assert_eq!(
        v.verify(&bytes, &PollScope::channel_post([9; 16], 78), b"m", PERIOD),
        Err(AnonCredError::BadProof)
    );
    assert_eq!(
        v.verify(&bytes, &PollScope::channel_post([8; 16], 77), b"m", PERIOD),
        Err(AnonCredError::BadProof)
    );
    assert_eq!(
        v.verify(&bytes, &scope, b"n", PERIOD),
        Err(AnonCredError::BadProof)
    );
}

#[test]
fn tags_link_within_a_poll_and_not_across_polls_or_accounts() {
    let (a, public, cred_a) = credential_for([3; 32], PERIOD);
    let (b, _, cred_b) = credential_for([4; 32], PERIOD);
    let poll1 = PollScope::channel_post([1; 16], 1);
    let poll2 = PollScope::channel_post([1; 16], 2);
    let a1 = a.present(&cred_a, &public, &poll1, b"x").unwrap();
    let a1_again = a.present(&cred_a, &public, &poll1, b"y").unwrap();
    let a2 = a.present(&cred_a, &public, &poll2, b"x").unwrap();
    let b1 = b.present(&cred_b, &public, &poll1, b"x").unwrap();
    assert_eq!(a1.tag(), a1_again.tag());
    assert_ne!(a1.tag(), a2.tag());
    assert_ne!(a1.tag(), b1.tag());
}

#[test]
fn tag_is_the_same_across_periods() {
    let (secret, public_p, cred_p) = credential_for([5; 32], PERIOD);
    let (_, public_q, cred_q) = credential_for([5; 32], PERIOD + 1);
    let poll = PollScope::channel_post([2; 16], 9);
    let tp = secret.present(&cred_p, &public_p, &poll, b"").unwrap();
    let tq = secret.present(&cred_q, &public_q, &poll, b"").unwrap();
    assert_eq!(tp.tag(), tq.tag());
}

#[test]
fn presentations_repeat_no_value_but_the_tag() {
    let seed = [6; 32];
    let (secret, public, credential) = credential_for(seed, PERIOD);
    let poll = PollScope::channel_post([3; 16], 3);
    let one = secret
        .present(&credential, &public, &poll, b"m")
        .unwrap()
        .to_bytes();
    let two = secret
        .present(&credential, &public, &poll, b"m")
        .unwrap()
        .to_bytes();
    let c1 = chunks(&one);
    let c2 = chunks(&two);
    // Field order: U~, C_s, C_U', T, challenge, three responses.
    let tag_index = 3;
    let credential_bytes = credential.to_bytes();
    let issued: Vec<[u8; 32]> = vec![
        credential_bytes[9..41].try_into().unwrap(),
        credential_bytes[41..].try_into().unwrap(),
        secret.commitment().to_bytes(),
    ];
    for (i, field) in c1.iter().enumerate() {
        for (j, other) in c2.iter().enumerate() {
            if i == tag_index && j == tag_index {
                assert_eq!(field, other);
            } else {
                assert_ne!(field, other, "field {i} of one equals field {j} of two");
            }
        }
        for value in &issued {
            assert_ne!(field, value, "field {i} repeats an issuance value");
        }
    }
}

#[test]
fn request_for_another_secret_than_the_stored_commitment_is_refused() {
    let alice = CredentialSecret::from_lineage_seed(&[7; 32]).unwrap();
    let mallory = CredentialSecret::from_lineage_seed(&[8; 32]).unwrap();
    let key = issuer(PERIOD);
    let request = mallory.issuance_request(&key.public_key()).unwrap();
    assert_eq!(
        key.issue(&request, &alice.commitment()).unwrap_err(),
        AnonCredError::BadProof
    );
    // And a request for another period than the key's.
    let other = alice
        .issuance_request(&issuer(PERIOD + 1).public_key())
        .unwrap();
    assert_eq!(
        key.issue(&other, &alice.commitment()).unwrap_err(),
        AnonCredError::WrongPeriod
    );
}

#[test]
fn a_request_bound_to_another_key_is_refused() {
    let alice = CredentialSecret::from_lineage_seed(&[7; 32]).unwrap();
    let rogue = IssuerSecretKey::derive(&MasterSecret::from_bytes([1; 32]), PERIOD).unwrap();
    let request = alice.issuance_request(&rogue.public_key()).unwrap();
    assert_eq!(
        issuer(PERIOD)
            .issue(&request, &alice.commitment())
            .unwrap_err(),
        AnonCredError::BadProof
    );
}

#[test]
fn registration_without_knowledge_is_refused() {
    let alice = CredentialSecret::from_lineage_seed(&[9; 32]).unwrap();
    let mallory = CredentialSecret::from_lineage_seed(&[10; 32]).unwrap();
    let good = alice.register().unwrap().to_bytes();
    let theirs = mallory.register().unwrap().to_bytes();
    // Alice's commitment with Mallory's proof.
    let mut spliced = good.clone();
    spliced[33..].copy_from_slice(&theirs[33..]);
    let decoded = CommitmentRegistration::decode(&spliced).unwrap();
    assert_eq!(verify_registration(&decoded), Err(AnonCredError::BadProof));
    assert_eq!(
        verify_registration(&CommitmentRegistration::decode(&good).unwrap()).unwrap(),
        alice.commitment()
    );
}

#[test]
fn client_refuses_a_response_under_another_key_or_for_another_secret() {
    let alice = CredentialSecret::from_lineage_seed(&[11; 32]).unwrap();
    let key = issuer(PERIOD);
    let request = alice.issuance_request(&key.public_key()).unwrap();
    let response = key.issue(&request, &alice.commitment()).unwrap();
    // The client checks against a key the issuer did not use (a tagging key).
    let other = IssuerSecretKey::derive(&MasterSecret::from_bytes([2; 32]), PERIOD).unwrap();
    assert_eq!(
        alice
            .finalize_issuance(&other.public_key(), &response)
            .unwrap_err(),
        AnonCredError::BadProof
    );
    // Another account cannot adopt Alice's MAC.
    let bob = CredentialSecret::from_lineage_seed(&[12; 32]).unwrap();
    assert_eq!(
        bob.finalize_issuance(&key.public_key(), &response)
            .unwrap_err(),
        AnonCredError::BadProof
    );
    assert_eq!(
        alice
            .finalize_issuance(&issuer(PERIOD + 1).public_key(), &response)
            .unwrap_err(),
        AnonCredError::WrongPeriod
    );
}

#[test]
fn period_window_is_own_period_and_the_next() {
    let (secret, public, credential) = credential_for([13; 32], PERIOD);
    let poll = PollScope::channel_post([4; 16], 4);
    let bytes = secret
        .present(&credential, &public, &poll, b"")
        .unwrap()
        .to_bytes();
    let v = verifier();
    assert!(v.verify(&bytes, &poll, b"", PERIOD).is_ok());
    assert!(v.verify(&bytes, &poll, b"", PERIOD + 1).is_ok());
    assert_eq!(
        v.verify(&bytes, &poll, b"", PERIOD + 2),
        Err(AnonCredError::PeriodNotAccepted)
    );
    assert_eq!(
        v.verify(&bytes, &poll, b"", PERIOD - 1),
        Err(AnonCredError::PeriodNotAccepted)
    );
}

#[test]
fn a_presentation_relabelled_to_another_period_is_refused() {
    let (secret, public, credential) = credential_for([14; 32], PERIOD);
    let poll = PollScope::channel_post([4; 16], 4);
    let mut bytes = secret
        .present(&credential, &public, &poll, b"")
        .unwrap()
        .to_bytes();
    bytes[1..9].copy_from_slice(&(PERIOD + 1).to_be_bytes());
    assert_eq!(
        verifier().verify(&bytes, &poll, b"", PERIOD + 1),
        Err(AnonCredError::BadProof)
    );
}

#[test]
fn a_credential_under_another_master_does_not_verify() {
    let secret = CredentialSecret::from_lineage_seed(&[15; 32]).unwrap();
    let rogue = IssuerSecretKey::derive(&MasterSecret::from_bytes([3; 32]), PERIOD).unwrap();
    let request = secret.issuance_request(&rogue.public_key()).unwrap();
    let response = rogue.issue(&request, &secret.commitment()).unwrap();
    let credential = secret
        .finalize_issuance(&rogue.public_key(), &response)
        .unwrap();
    let poll = PollScope::channel_post([5; 16], 5);
    let bytes = secret
        .present(&credential, &rogue.public_key(), &poll, b"")
        .unwrap()
        .to_bytes();
    assert_eq!(
        verifier().verify(&bytes, &poll, b"", PERIOD),
        Err(AnonCredError::BadProof)
    );
}

#[test]
fn identity_u_is_refused_at_decode() {
    let (secret, public, credential) = credential_for([16; 32], PERIOD);
    let poll = PollScope::channel_post([6; 16], 6);
    let mut bytes = secret
        .present(&credential, &public, &poll, b"")
        .unwrap()
        .to_bytes();
    bytes[9..41].fill(0);
    assert_eq!(
        Presentation::decode(&bytes).unwrap_err(),
        AnonCredError::IdentityPoint
    );
}

#[test]
fn bad_inputs_are_refused() {
    let (secret, public, credential) = credential_for([17; 32], PERIOD);
    let poll = PollScope::channel_post([7; 16], 7);
    let long = vec![0u8; MAX_MESSAGE_LEN + 1];
    assert_eq!(
        secret
            .present(&credential, &public, &poll, &long)
            .unwrap_err(),
        AnonCredError::MessageTooLong
    );
    let bytes = secret
        .present(&credential, &public, &poll, b"")
        .unwrap()
        .to_bytes();
    assert_eq!(
        verifier().verify(&bytes, &poll, &long, PERIOD),
        Err(AnonCredError::MessageTooLong)
    );
    assert_eq!(
        verifier().verify(&bytes[..PRESENTATION_LEN - 1], &poll, b"", PERIOD),
        Err(AnonCredError::Malformed)
    );
    let mut versioned = bytes;
    versioned[0] = 2;
    assert_eq!(
        verifier().verify(&versioned, &poll, b"", PERIOD),
        Err(AnonCredError::UnknownVersion)
    );
    assert_eq!(
        secret
            .present(&credential, &issuer(PERIOD + 1).public_key(), &poll, b"")
            .unwrap_err(),
        AnonCredError::WrongPeriod
    );
    assert_eq!(
        Commitment::decode(&[0; 32]),
        Err(AnonCredError::IdentityPoint)
    );
}

#[test]
fn secret_derivation_is_deterministic() {
    let a = CredentialSecret::from_lineage_seed(&[18; 32]).unwrap();
    let b = CredentialSecret::from_lineage_seed(&[18; 32]).unwrap();
    let c = CredentialSecret::from_lineage_seed(&[19; 32]).unwrap();
    assert_eq!(a.commitment(), b.commitment());
    assert_ne!(a.commitment(), c.commitment());
}

/// Golden values computed by this implementation: a change to any derivation
/// (secret, generators, issuer key) fails here before it reaches another
/// platform.
#[test]
fn golden_values_are_pinned() {
    let secret = CredentialSecret::from_lineage_seed(&[0x11; 32]).unwrap();
    let commitment = hex(&secret.commitment().to_bytes());
    let public = hex(&issuer(PERIOD).public_key().to_bytes());
    assert_eq!(commitment, GOLDEN_COMMITMENT, "commitment");
    assert_eq!(public, GOLDEN_PUBLIC_KEY, "public key");
}

const GOLDEN_COMMITMENT: &str = "a20de251d1516d790e0fb9fb9ef44f06f59b027982e09b05b02c302d4a67c62b";
const GOLDEN_PUBLIC_KEY: &str = "08c39b777913d607bfd376f80757a5d7ddf089a208194939bad50fec76aae71926b3e88c3c33febcc9f6239566ab73bd81ffb34945077796678c3ccdd838392dcc1525fa93ca2684122bf67d0b5a6fbe0884ae8b91264de6ea737b49e393187c";

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 16, .. ProptestConfig::default() })]

    /// Any single-byte change to a valid presentation makes it fail.
    #[test]
    fn any_tampered_byte_is_refused(index in 0usize..PRESENTATION_LEN, flip in 1u8..=255) {
        let (secret, public, credential) = credential_for([20; 32], PERIOD);
        let poll = PollScope::channel_post([8; 16], 8);
        let mut bytes = secret.present(&credential, &public, &poll, b"v").unwrap().to_bytes();
        bytes[index] ^= flip;
        prop_assert!(verifier().verify(&bytes, &poll, b"v", PERIOD).is_err());
    }

    /// Completeness and the tag rule over random accounts and polls.
    #[test]
    fn random_accounts_and_polls(
        seed in any::<[u8; 32]>(),
        channel in any::<[u8; 16]>(),
        cursor in any::<u64>(),
        other_cursor in any::<u64>(),
    ) {
        prop_assume!(cursor != other_cursor);
        let (secret, public, credential) = credential_for(seed, PERIOD);
        let poll = PollScope::channel_post(channel, cursor);
        let other = PollScope::channel_post(channel, other_cursor);
        let p1 = secret.present(&credential, &public, &poll, b"").unwrap();
        let p2 = secret.present(&credential, &public, &other, b"").unwrap();
        let v = verifier();
        prop_assert_eq!(v.verify(&p1.to_bytes(), &poll, b"", PERIOD).unwrap().tag, p1.tag());
        prop_assert!(v.verify(&p1.to_bytes(), &other, b"", PERIOD).is_err());
        prop_assert_ne!(p1.tag(), p2.tag());
    }
}
