// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end properties of the anonymous-token primitive: issue, finalize,
//! verify, and every way a token or an issuance must be refused.

use lakko_anon_token::{
    verify_token, AnonTokenError, IssuerKey, MasterSecret, Token, TokenContext, TokenRequest,
    DEFAULT_WINDOW_SECS, MAX_BATCH, OUTPUT_LEN, TOKEN_LEN,
};
use proptest::prelude::*;

const W: u64 = DEFAULT_WINDOW_SECS;

fn master() -> MasterSecret {
    MasterSecret::from_bytes([0x11; 32])
}

/// Run the whole client + issuer flow and return `count` tokens.
fn mint(master: &MasterSecret, ctx: TokenContext, epoch: u64, count: usize) -> Vec<Token> {
    let req = TokenRequest::new(ctx, epoch, count).expect("request");
    let key = IssuerKey::derive(master, ctx, epoch).expect("key");
    let issuance = key.issue(req.blinded_elements()).expect("issue");
    req.finalize(&issuance.evaluations, &issuance.proof, &key.public_key())
        .expect("finalize")
}

/// A clock inside `epoch`.
const fn now_in(epoch: u64) -> u64 {
    epoch * W + 123
}

/// A clock inside `epoch` of `ctx`'s window (`beta-place` has its own).
const fn now_in_ctx(ctx: TokenContext, epoch: u64) -> u64 {
    epoch * ctx.window_secs(W) + 123
}

#[test]
fn a_batch_round_trips_and_every_token_verifies_once_per_spend_id() {
    let m = master();
    let tokens = mint(&m, TokenContext::DiscoveryLookup, 500, 8);
    assert_eq!(tokens.len(), 8);
    let mut ids = std::collections::HashSet::new();
    for t in &tokens {
        let wire = t.encode();
        assert_eq!(wire.len(), TOKEN_LEN);
        let v = verify_token(&m, &wire, TokenContext::DiscoveryLookup, now_in(500), W)
            .expect("verifies");
        assert_eq!(v.epoch, 500);
        assert_eq!(v.context, TokenContext::DiscoveryLookup);
        assert!(ids.insert(v.spend_id), "spend ids must be distinct");
    }
}

#[test]
fn a_full_batch_of_max_size_round_trips() {
    let tokens = mint(&master(), TokenContext::PushRegister, 3, MAX_BATCH);
    assert_eq!(tokens.len(), MAX_BATCH);
}

#[test]
fn previous_epoch_is_accepted_older_and_future_are_not() {
    let m = master();
    let t = mint(&m, TokenContext::DsKnock, 100, 1).remove(0).encode();
    assert!(verify_token(&m, &t, TokenContext::DsKnock, now_in(100), W).is_ok());
    assert!(verify_token(&m, &t, TokenContext::DsKnock, now_in(101), W).is_ok());
    assert_eq!(
        verify_token(&m, &t, TokenContext::DsKnock, now_in(102), W),
        Err(AnonTokenError::EpochOutOfWindow)
    );
    assert_eq!(
        verify_token(&m, &t, TokenContext::DsKnock, now_in(99), W),
        Err(AnonTokenError::EpochOutOfWindow)
    );
}

#[test]
fn a_token_is_refused_at_another_context() {
    let m = master();
    let t = mint(&m, TokenContext::InviteConsume, 7, 1).remove(0);
    assert_eq!(
        verify_token(&m, &t.encode(), TokenContext::DsKnock, now_in(7), W),
        Err(AnonTokenError::WrongContext)
    );
    // Relabelling the context byte does not help: the key and the input both
    // bind the context.
    let mut relabelled = t;
    relabelled.context = TokenContext::DsKnock;
    assert_eq!(
        verify_token(
            &m,
            &relabelled.encode(),
            TokenContext::DsKnock,
            now_in(7),
            W
        ),
        Err(AnonTokenError::InvalidToken)
    );
}

#[test]
fn relabelling_the_epoch_is_refused() {
    let m = master();
    let mut t = mint(&m, TokenContext::DsKnock, 40, 1).remove(0);
    t.epoch = 41;
    assert_eq!(
        verify_token(&m, &t.encode(), TokenContext::DsKnock, now_in(41), W),
        Err(AnonTokenError::InvalidToken)
    );
}

#[test]
fn another_master_does_not_verify() {
    let t = mint(&master(), TokenContext::DsKnock, 5, 1)
        .remove(0)
        .encode();
    let other = MasterSecret::from_bytes([0x12; 32]);
    assert_eq!(
        verify_token(&other, &t, TokenContext::DsKnock, now_in(5), W),
        Err(AnonTokenError::InvalidToken)
    );
}

#[test]
fn a_forged_token_with_a_random_output_is_refused() {
    let forged = Token {
        context: TokenContext::DsKnock,
        epoch: 9,
        nonce: [7; 32],
        output: [9; OUTPUT_LEN],
    };
    assert_eq!(
        verify_token(
            &master(),
            &forged.encode(),
            TokenContext::DsKnock,
            now_in(9),
            W
        ),
        Err(AnonTokenError::InvalidToken)
    );
}

#[test]
fn a_zero_window_is_refused() {
    let t = mint(&master(), TokenContext::DsKnock, 0, 1)
        .remove(0)
        .encode();
    assert_eq!(
        verify_token(&master(), &t, TokenContext::DsKnock, 10, 0),
        Err(AnonTokenError::InvalidWindow)
    );
}

#[test]
fn an_evaluation_under_another_key_is_refused_by_the_client() {
    let m = master();
    let req = TokenRequest::new(TokenContext::DsKnock, 3, 4).unwrap();
    let published = IssuerKey::derive(&m, TokenContext::DsKnock, 3).unwrap();
    // The issuer evaluates this client under a key of its own (a tagging
    // attempt) and ships that key's valid proof.
    let tagging = IssuerKey::derive(
        &MasterSecret::from_bytes([0x99; 32]),
        TokenContext::DsKnock,
        3,
    )
    .unwrap();
    let issuance = tagging.issue(req.blinded_elements()).unwrap();
    assert_eq!(
        req.finalize(
            &issuance.evaluations,
            &issuance.proof,
            &published.public_key()
        )
        .unwrap_err(),
        AnonTokenError::ProofVerification
    );
}

#[test]
fn a_tampered_proof_or_evaluation_is_refused_by_the_client() {
    let m = master();
    let key = IssuerKey::derive(&m, TokenContext::DsKnock, 3).unwrap();

    let req = TokenRequest::new(TokenContext::DsKnock, 3, 2).unwrap();
    let mut issuance = key.issue(req.blinded_elements()).unwrap();
    issuance.proof[0] ^= 1;
    assert_eq!(
        req.finalize(&issuance.evaluations, &issuance.proof, &key.public_key())
            .unwrap_err(),
        AnonTokenError::ProofVerification
    );

    // Swap the two evaluations: each is a valid point but no longer matches
    // the proof's transcript.
    let req = TokenRequest::new(TokenContext::DsKnock, 3, 2).unwrap();
    let mut issuance = key.issue(req.blinded_elements()).unwrap();
    issuance.evaluations.swap(0, 1);
    assert_eq!(
        req.finalize(&issuance.evaluations, &issuance.proof, &key.public_key())
            .unwrap_err(),
        AnonTokenError::ProofVerification
    );

    // An evaluation from another request under the right key and its own
    // proof does not satisfy this request.
    let req_a = TokenRequest::new(TokenContext::DsKnock, 3, 1).unwrap();
    let req_b = TokenRequest::new(TokenContext::DsKnock, 3, 1).unwrap();
    let issuance_b = key.issue(req_b.blinded_elements()).unwrap();
    assert_eq!(
        req_a
            .finalize(
                &issuance_b.evaluations,
                &issuance_b.proof,
                &key.public_key()
            )
            .unwrap_err(),
        AnonTokenError::ProofVerification
    );
}

#[test]
fn the_client_refuses_bad_shapes() {
    let key = IssuerKey::derive(&master(), TokenContext::DsKnock, 3).unwrap();
    assert_eq!(
        TokenRequest::new(TokenContext::DsKnock, 3, 0).unwrap_err(),
        AnonTokenError::BadBatchSize
    );
    assert_eq!(
        TokenRequest::new(TokenContext::DsKnock, 3, MAX_BATCH + 1).unwrap_err(),
        AnonTokenError::BadBatchSize
    );

    let req = TokenRequest::new(TokenContext::DsKnock, 3, 2).unwrap();
    let issuance = key.issue(req.blinded_elements()).unwrap();
    assert_eq!(
        req.finalize(
            &issuance.evaluations[..1],
            &issuance.proof,
            &key.public_key()
        )
        .unwrap_err(),
        AnonTokenError::CountMismatch
    );

    let req = TokenRequest::new(TokenContext::DsKnock, 3, 1).unwrap();
    let issuance = key.issue(req.blinded_elements()).unwrap();
    assert_eq!(
        req.finalize(&issuance.evaluations, &issuance.proof, &[0u8; 32])
            .unwrap_err(),
        AnonTokenError::BadPublicKey
    );

    let req = TokenRequest::new(TokenContext::DsKnock, 3, 1).unwrap();
    let issuance = key.issue(req.blinded_elements()).unwrap();
    assert_eq!(
        req.finalize(&[[0u8; 32]], &issuance.proof, &key.public_key())
            .unwrap_err(),
        AnonTokenError::BadElement
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Any single-bit flip anywhere in a valid token makes it refuse (the
    /// exact error depends on which field the flip lands in).
    #[test]
    fn any_bit_flip_is_refused(ctx_idx in 0usize..TokenContext::ALL.len(), epoch in 1u64..1_000_000, bit in 0usize..(TOKEN_LEN * 8)) {
        let ctx = TokenContext::ALL[ctx_idx];
        let m = master();
        let t = mint(&m, ctx, epoch, 1).remove(0);
        let mut wire = t.encode();
        wire[bit / 8] ^= 1 << (bit % 8);
        prop_assert!(verify_token(&m, &wire, ctx, now_in_ctx(ctx, epoch), W).is_err());
    }

    /// A minted token verifies for its context in its epoch and the next.
    #[test]
    fn minted_tokens_verify(ctx_idx in 0usize..TokenContext::ALL.len(), epoch in 0u64..1_000_000, count in 1usize..6) {
        let ctx = TokenContext::ALL[ctx_idx];
        let m = master();
        for t in mint(&m, ctx, epoch, count) {
            let wire = t.encode();
            prop_assert!(verify_token(&m, &wire, ctx, now_in_ctx(ctx, epoch), W).is_ok());
            prop_assert!(verify_token(&m, &wire, ctx, now_in_ctx(ctx, epoch + 1), W).is_ok());
        }
    }
}

#[test]
fn the_base64url_exchange_round_trips_and_the_header_value_verifies() {
    use lakko_anon_token::{decode_header_value, encode_b64url};
    let m = master();
    let ctx = TokenContext::InviteConsume;
    let req = TokenRequest::new(ctx, 40, 3).expect("request");
    let blinded = req.blinded_elements_b64url();
    assert_eq!(blinded.len(), 3);
    // What the issuer does with the text: decode, evaluate, encode.
    let raw: Vec<[u8; 32]> = blinded
        .iter()
        .map(|b| lakko_anon_token::decode_b64url_fixed::<32>(b).expect("32 bytes"))
        .collect();
    let key = IssuerKey::derive(&m, ctx, 40).expect("key");
    let issuance = key.issue(&raw).expect("issue");
    let evaluations: Vec<String> = issuance
        .evaluations
        .iter()
        .map(|e| encode_b64url(e))
        .collect();
    let tokens = req
        .finalize_b64url(
            &evaluations,
            &encode_b64url(&issuance.proof),
            &encode_b64url(&key.public_key()),
        )
        .expect("finalize");
    for t in tokens {
        let bytes = decode_header_value(&t.to_header_value()).expect("header");
        assert!(verify_token(&m, &bytes, ctx, now_in(40), W).is_ok());
    }
}

#[test]
fn the_base64url_finalize_refuses_bad_text() {
    use lakko_anon_token::encode_b64url;
    let m = master();
    let ctx = TokenContext::DsKnock;
    let key = IssuerKey::derive(&m, ctx, 1).expect("key");
    let pk = encode_b64url(&key.public_key());
    let fresh = || {
        let req = TokenRequest::new(ctx, 1, 1).expect("request");
        let issuance = key.issue(req.blinded_elements()).expect("issue");
        (req, issuance)
    };
    let (req, iss) = fresh();
    assert_eq!(
        req.finalize_b64url(&["not base64!"], &encode_b64url(&iss.proof), &pk)
            .unwrap_err(),
        AnonTokenError::BadElement
    );
    let (req, iss) = fresh();
    assert_eq!(
        req.finalize_b64url(&[encode_b64url(&iss.evaluations[0])], "short", &pk)
            .unwrap_err(),
        AnonTokenError::ProofVerification
    );
    let (req, iss) = fresh();
    assert_eq!(
        req.finalize_b64url(
            &[encode_b64url(&iss.evaluations[0])],
            &encode_b64url(&iss.proof),
            "short"
        )
        .unwrap_err(),
        AnonTokenError::BadPublicKey
    );
}
