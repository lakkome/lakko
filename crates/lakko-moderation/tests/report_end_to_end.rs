// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0099 end to end: a bound device sends an attested message in a real
//! MLS group, the recipient reports it, a moderator opens the report and
//! checks that the reported account wrote the message.

use lakko_mls::{group, key_package, Group, GroupKind, Identity, Incoming};
use lakko_moderation::evidence::{verify_message_evidence, EvidenceVerdict};
use lakko_moderation::report::{
    open_report, seal_report, ConversationKind, EvidenceItem, ReportContent,
};
use lakko_moderation::seal::{moderator_report_secret, Recipient};
use lakko_moderation::{Reason, Target};

fn wire(msg: openmls::framing::MlsMessageOut) -> Vec<u8> {
    group::encode_message(&msg).expect("encode")
}

#[test]
#[allow(clippy::too_many_lines)]
fn a_reported_message_verifies_to_its_authors_account() {
    let alice = Identity::from_seed(&[0xA0; 64]).expect("alice");
    let alice_phone =
        Identity::from_seed_bound(&[0xA1; 32], &alice.ed25519_seed_bytes(), 1_700_000_000)
            .expect("alice's phone");
    let bob = Identity::from_seed(&[0xB0; 64]).expect("bob");

    let mut a_g = Group::create(&alice_phone, GroupKind::OneToOne).expect("create");
    let kp = key_package::serialize(&bob.create_key_package().expect("kp")).expect("kp wire");
    let add = a_g
        .add_member_from_wire(&alice_phone, &kp)
        .expect("add bob");
    let mut b_g = Group::join_from_welcome(&wire(add.welcome), &bob).expect("bob joins");
    let group_id = a_g.group_id();

    // Alice's phone sends an attested chat message.
    let envelope = lakko_attachments::compose("you will regret this", Vec::new()).expect("env");
    let attested = lakko_attachments::attested::attest(
        &alice_phone.ed25519_seed_bytes(),
        Some(alice_phone.signature_public_key()),
        &group_id,
        &envelope,
    )
    .expect("attest");
    let msg = a_g
        .create_application_message(&alice_phone, &attested)
        .expect("send");

    // Bob receives it and checks the signature against the sender leaf.
    let Incoming::Application {
        bytes, sender_leaf, ..
    } = b_g
        .process_incoming_verifying_roles(&bob, &wire(msg))
        .expect("receive")
    else {
        panic!("expected an application message");
    };
    let leaf = sender_leaf.expect("member sender");
    let leaf_key: [u8; 32] = b_g
        .members()
        .iter()
        .find(|m| m.leaf_index == leaf)
        .expect("sender in roster")
        .signature_public_key
        .as_slice()
        .try_into()
        .expect("32 bytes");
    let (_, att) = lakko_attachments::parse_attested_body(&bytes, &group_id, &leaf_key);
    assert_eq!(att, lakko_attachments::Attestation::Verified);

    // Bob reports it with the sender leaf's binding.
    let binding = b_g.device_binding_at(leaf).expect("binding");
    assert!(!binding.is_empty(), "a bound device carries a binding");
    let secret = moderator_report_secret(&[0x11; 32]).expect("moderator key");
    let moderator = Recipient {
        moderator_id: [0x11; 16],
        x25519_public: secret.public_key().to_bytes(),
    };
    let alice_account = alice.account_public_key().expect("account key");
    let target = Target::account(&alice_account).expect("target");
    let report = seal_report(
        target.clone(),
        Reason::Harassment,
        ConversationKind::OneToOne,
        &ReportContent {
            comment: String::new(),
            evidence: vec![EvidenceItem::Message {
                group_id: group_id.clone(),
                plaintext: bytes,
                leaf_key,
                binding,
            }],
        },
        1,
        &[moderator],
    )
    .expect("seal");

    // The moderator opens it and checks authorship.
    let (clear, content) = open_report(&report, &[0x11; 16], &secret).expect("open");
    assert_eq!(clear.target, target);
    let verdict = verify_message_evidence(&content.evidence[0], |b, k| {
        lakko_mls::account_key_of_credential(b, k)
    });
    assert_eq!(
        verdict,
        EvidenceVerdict::Verified {
            account_key: alice_account
        }
    );
    assert!(verdict.is_by(&target));

    // The same evidence does not verify as bob's.
    assert!(!verdict.is_by(&Target::account(bob.signature_public_key()).expect("t")));

    // A binding from another leaf does not resolve.
    let EvidenceItem::Message {
        group_id,
        plaintext,
        leaf_key,
        ..
    } = content.evidence[0].clone()
    else {
        unreachable!()
    };
    let forged = EvidenceItem::Message {
        group_id,
        plaintext,
        leaf_key,
        binding: Vec::new(),
    };
    assert_eq!(
        verify_message_evidence(&forged, lakko_mls::account_key_of_credential),
        EvidenceVerdict::Verified {
            account_key: leaf_key
        },
        "an empty binding resolves to the leaf itself, which is not alice's account"
    );
    assert!(!verify_message_evidence(&forged, lakko_mls::account_key_of_credential).is_by(&target));
}
