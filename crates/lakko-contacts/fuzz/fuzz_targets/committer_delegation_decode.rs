// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `committer_delegation::decode_committer_delegation` (ADR-0058 §6): a
//! proposer's committer delegations ride an MLS application message on the
//! group journal, so these bytes reach the parser from any member (or the
//! zero-trust DS) before any state is consulted. Asserts no panic, that
//! anything decoded lies within the length bounds, round-trip stability,
//! and that the dispatcher classifies the same bytes the same way.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::committer_delegation::{
    compose_committer_delegation, decode_committer_delegation,
    COMMITTER_DELEGATION_ENVELOPE_MAX_LEN, COMMITTER_DELEGATION_ENVELOPE_MIN_LEN,
    MAX_COMMITTER_DELEGATIONS,
};
use lakko_contacts::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};

fuzz_target!(|data: &[u8]| {
    let dispatched = dispatch_incoming_envelope(data);
    let Ok(envelope) = decode_committer_delegation(data) else {
        assert!(!matches!(
            dispatched,
            Ok(IncomingEnvelope::CommitterDelegation(_))
        ));
        return;
    };
    assert!(
        (COMMITTER_DELEGATION_ENVELOPE_MIN_LEN..=COMMITTER_DELEGATION_ENVELOPE_MAX_LEN)
            .contains(&data.len())
    );
    assert!((1..=MAX_COMMITTER_DELEGATIONS).contains(&envelope.delegations.len()));
    let recomposed = compose_committer_delegation(&envelope).expect("a decoded envelope composes");
    assert_eq!(
        recomposed.as_slice(),
        data,
        "the encoding must be canonical"
    );
    assert_eq!(
        dispatched.expect("the dispatcher accepts what the decoder accepts"),
        IncomingEnvelope::CommitterDelegation(envelope)
    );
});
