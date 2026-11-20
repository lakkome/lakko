// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

use std::collections::BTreeSet;

use lakko_attachments::checklist::{
    self, ChecklistAddRecord, ChecklistBody, ChecklistItem, ChecklistItemRef, ChecklistTickRecord,
};
use lakko_attachments::poll::{self, PollAnonymous, PollBody, PollVoteRecord};
use lakko_attachments::{SignedChecklist, SignedPoll};
use lakko_ballot::ring::{ring_hash, MAX_RING_LEN, MAX_VERSIONS};
use lakko_ballot::Ring;
use lakko_contacts::open_items::{
    assemble_open_items, decode_open_items, plan_open_items_rows, AssembledContent, AssembledItem,
    OpenItem, OpenItems, ReceivedOpenItems, OPEN_ITEMS_CHECKLIST_BODY_PREFIX,
    OPEN_ITEMS_MAX_BODY_LEN, OPEN_ITEMS_MAX_CHECKLIST_ITEMS, OPEN_ITEMS_MAX_CHOICES,
    OPEN_ITEMS_MAX_ITEM_TEXT_BYTES, OPEN_ITEMS_MAX_RING_LEN, OPEN_ITEMS_MAX_VERSIONS,
    OPEN_ITEMS_POLL_BODY_PREFIX,
};
use lakko_contacts::poll_ballots::{POLL_BALLOTS_MAX_ROW_LEN, POLL_BALLOTS_ROW_PREFIX};
use lakko_crypto::signing::SigningKey;

use crate::{adopt_open_item, checklist_open_item, poll_open_item, AdoptError, AdoptedItem};

const GROUP: [u8; 32] = [0x47; 32];
const SEED: [u8; 32] = [0x11; 32];

fn author() -> [u8; 32] {
    SigningKey::from_seed(&SEED).verifying_key().to_bytes()
}

fn key(i: u32) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[..4].copy_from_slice(&i.to_be_bytes());
    k
}

fn poll_body(anonymous: Option<PollAnonymous>) -> PollBody {
    PollBody {
        id: [5; 16],
        question: "Lunch?".to_owned(),
        options: vec!["Yes".to_owned(), "No".to_owned(), "Later".to_owned()],
        multiple: false,
        quiz: None,
        close_at_ms: None,
        anonymous,
    }
}

/// The hand-over end to end: build, plan, decode, assemble.
fn hand_over(items: &[OpenItem]) -> Vec<AssembledItem> {
    let rows: Vec<OpenItems> = plan_open_items_rows(items)
        .unwrap()
        .iter()
        .map(|r| decode_open_items(r).unwrap())
        .collect();
    let received: Vec<ReceivedOpenItems<'_>> = rows
        .iter()
        .map(|items| ReceivedOpenItems {
            sender_account_key: [9; 32],
            items,
        })
        .collect();
    assemble_open_items(&received, &BTreeSet::new())
}

#[test]
fn bounds_match_the_crates_they_mirror() {
    const { assert!(OPEN_ITEMS_MAX_BODY_LEN >= poll::MAX_POLL_BODY_LEN) };
    const { assert!(OPEN_ITEMS_MAX_BODY_LEN >= checklist::MAX_CHECKLIST_BODY_LEN) };
    assert_eq!(OPEN_ITEMS_POLL_BODY_PREFIX, poll::POLL_DOMAIN);
    assert_eq!(
        OPEN_ITEMS_CHECKLIST_BODY_PREFIX,
        checklist::CHECKLIST_DOMAIN
    );
    assert_eq!(OPEN_ITEMS_MAX_CHOICES, poll::MAX_POLL_OPTIONS);
    assert_eq!(
        OPEN_ITEMS_MAX_CHECKLIST_ITEMS,
        checklist::MAX_CHECKLIST_ITEMS
    );
    assert_eq!(
        OPEN_ITEMS_MAX_ITEM_TEXT_BYTES,
        checklist::MAX_CHECKLIST_ITEM_BYTES
    );
    assert_eq!(OPEN_ITEMS_MAX_RING_LEN, MAX_RING_LEN);
    assert_eq!(OPEN_ITEMS_MAX_VERSIONS, MAX_VERSIONS);
    assert_eq!(
        POLL_BALLOTS_ROW_PREFIX,
        lakko_ballot::ballot::BALLOT_ROW_DOMAIN
    );
    const { assert!(POLL_BALLOTS_MAX_ROW_LEN >= lakko_ballot::ballot::MAX_ROW_LEN) };
}

fn anonymous_poll() -> (SignedPoll, Ring) {
    let v0: Vec<[u8; 32]> = (1..=5).map(key).collect();
    let mut ring = Ring::new(&v0).unwrap();
    ring.extend(&[key(9), key(7)]).unwrap();
    let body = poll_body(Some(PollAnonymous {
        secret: [3; 32],
        ring0_hash: ring_hash(&v0),
    }));
    (SignedPoll::sign(body, &GROUP, &SEED).unwrap(), ring)
}

#[test]
fn an_anonymous_poll_is_adopted_with_its_ring() {
    let (poll, ring) = anonymous_poll();
    let item = poll_open_item(100, &poll, &author(), Some(&ring), &[]).unwrap();
    let assembled = hand_over(&[item]);
    assert_eq!(assembled.len(), 1);
    match adopt_open_item(&GROUP, &assembled[0]).unwrap() {
        AdoptedItem::Poll {
            poll_ref,
            author: a,
            poll: p,
            ring: Some(r),
            votes,
        } => {
            assert_eq!(poll_ref, 100);
            assert_eq!(a, author());
            assert_eq!(p, poll);
            assert_eq!(r.encode(), ring.encode());
            assert!(votes.is_empty());
        },
        other => panic!("{other:?}"),
    }
    // In another group the signature does not verify.
    assert_eq!(
        adopt_open_item(&[0; 32], &assembled[0]).unwrap_err(),
        AdoptError::Signature
    );
}

#[test]
fn a_ring_whose_version_0_is_not_signed_is_refused() {
    let (poll, _) = anonymous_poll();
    // A committer adding its own key to version 0.
    let forged = Ring::new(&[key(1), key(2), key(3), key(4), key(5), key(6)]).unwrap();
    let item = poll_open_item(100, &poll, &author(), Some(&forged), &[]).unwrap();
    let assembled = hand_over(&[item]);
    assert_eq!(
        adopt_open_item(&GROUP, &assembled[0]).unwrap_err(),
        AdoptError::Ring
    );
}

#[test]
fn a_poll_in_someone_elses_name_is_refused() {
    let (poll, ring) = anonymous_poll();
    let item = poll_open_item(100, &poll, &[0x22; 32], Some(&ring), &[]).unwrap();
    let assembled = hand_over(&[item]);
    assert_eq!(
        adopt_open_item(&GROUP, &assembled[0]).unwrap_err(),
        AdoptError::Signature
    );
    // A missing ring for an anonymous poll is a build error.
    assert!(poll_open_item(100, &poll, &author(), None, &[]).is_err());
}

#[test]
fn a_public_poll_hands_over_each_voters_latest_vote() {
    let poll = SignedPoll::sign(poll_body(None), &GROUP, &SEED).unwrap();
    let vote = |voter: u8, message_id: i64, choices: Vec<u8>| PollVoteRecord {
        message_id,
        created_at_ms: message_id * 10,
        voter: [voter; 32],
        choices,
    };
    let votes = vec![
        vote(1, 101, vec![0]),
        vote(1, 105, vec![2]),
        vote(2, 102, vec![1]),
        vote(2, 106, vec![]),
        vote(3, 99, vec![0]),
        vote(4, 107, vec![7]),
        vote(5, 108, vec![1]),
    ];
    let item = poll_open_item(100, &poll, &author(), None, &votes).unwrap();
    let assembled = hand_over(&[item]);
    let AdoptedItem::Poll { votes: adopted, .. } = adopt_open_item(&GROUP, &assembled[0]).unwrap()
    else {
        panic!("not a poll")
    };
    let pairs: Vec<([u8; 32], Vec<u8>)> = adopted
        .iter()
        .map(|v| (v.voter, v.choices.clone()))
        .collect();
    assert_eq!(pairs, vec![([1; 32], vec![2]), ([5; 32], vec![1])]);
    // The newcomer's fold of the adopted votes is the committer's fold.
    let committer = poll::fold(&poll.body, &author(), 100, &votes, &[]);
    let newcomer = poll::fold(&poll.body, &author(), 100, &adopted, &[]);
    assert_eq!(committer, newcomer);
}

#[test]
fn a_checklist_is_adopted_and_folds_to_the_committers_state() {
    let body = ChecklistBody {
        title: "Trip".to_owned(),
        others_add: true,
        others_mark: true,
        items: vec![
            ChecklistItem {
                id: 1,
                text: "Tent".to_owned(),
            },
            ChecklistItem {
                id: 2,
                text: "Stove".to_owned(),
            },
        ],
    };
    let signed = SignedChecklist::sign(body.clone(), &GROUP, &SEED).unwrap();
    let adds = vec![ChecklistAddRecord {
        message_id: 205,
        adder: [4; 32],
        privileged: false,
        text: "Maps".to_owned(),
    }];
    let ticks = vec![
        ChecklistTickRecord {
            message_id: 206,
            ticker: [4; 32],
            privileged: false,
            item: ChecklistItemRef::Author(2),
            done: true,
        },
        ChecklistTickRecord {
            message_id: 207,
            ticker: [6; 32],
            privileged: false,
            item: ChecklistItemRef::Added(205),
            done: true,
        },
        ChecklistTickRecord {
            message_id: 208,
            ticker: [6; 32],
            privileged: false,
            item: ChecklistItemRef::Author(1),
            done: true,
        },
        ChecklistTickRecord {
            message_id: 209,
            ticker: [6; 32],
            privileged: false,
            item: ChecklistItemRef::Author(1),
            done: false,
        },
    ];
    let state = checklist::fold(&body, &author(), 200, &[], &adds, &ticks, &[]);
    let item = checklist_open_item(200, &signed, &author(), &state, &ticks).unwrap();
    let assembled = hand_over(&[item]);
    let AdoptedItem::Checklist {
        checklist: c,
        adds: a,
        ticks: t,
        ..
    } = adopt_open_item(&GROUP, &assembled[0]).unwrap()
    else {
        panic!("not a checklist")
    };
    assert_eq!(c, signed);
    let newcomer = checklist::fold(&c.body, &author(), 200, &[], &a, &t, &[]);
    assert_eq!(newcomer, state);
    // Even after the author turns "others can mark" off in an edit the
    // newcomer never saw, the handed-over marks stay.
    let mut stricter = body.clone();
    stricter.others_mark = false;
    let signed_stricter = SignedChecklist::sign(stricter.clone(), &GROUP, &SEED).unwrap();
    let edit = lakko_attachments::checklist::ChecklistEditRecord {
        message_id: 210,
        editor: author(),
        body: stricter.clone(),
    };
    let state_stricter = checklist::fold(&body, &author(), 200, &[edit], &adds, &ticks, &[]);
    assert_eq!(state_stricter.done_count(), 2);
    let item =
        checklist_open_item(200, &signed_stricter, &author(), &state_stricter, &ticks).unwrap();
    let AdoptedItem::Checklist {
        checklist: c,
        adds: a,
        ticks: t,
        ..
    } = adopt_open_item(&GROUP, &hand_over(&[item])[0]).unwrap()
    else {
        panic!("not a checklist")
    };
    assert_eq!(
        checklist::fold(&c.body, &author(), 200, &[], &a, &t, &[]),
        state_stricter
    );
    // A state that is not this body's is refused at build.
    assert!(checklist_open_item(200, &signed_stricter, &author(), &state, &ticks).is_err());
}

#[test]
fn a_tick_on_an_item_the_body_lacks_is_refused() {
    let body = ChecklistBody {
        title: "T".to_owned(),
        others_add: false,
        others_mark: false,
        items: vec![ChecklistItem {
            id: 1,
            text: "a".to_owned(),
        }],
    };
    let signed = SignedChecklist::sign(body, &GROUP, &SEED).unwrap();
    let mut assembled = AssembledItem {
        header: lakko_contacts::open_items::OpenItemHeader {
            poll_ref: 1,
            author_account_key: author(),
            author_sig: signed.author_sig.unwrap(),
            body: signed.body.encode().unwrap(),
        },
        sender_account_key: [9; 32],
        content: AssembledContent::Checklist {
            added: vec![],
            ticks: vec![lakko_contacts::open_items::HandoverTick {
                item: lakko_contacts::open_items::HandoverItemRef::Author(2),
                ticker: [1; 32],
                message_id: 2,
                done: true,
            }],
        },
    };
    assert_eq!(
        adopt_open_item(&GROUP, &assembled).unwrap_err(),
        AdoptError::Tick
    );
    // A checklist body under a poll item is a kind mismatch in decoding.
    assembled.content = AssembledContent::Poll { votes: vec![] };
    assert!(matches!(
        adopt_open_item(&GROUP, &assembled),
        Err(AdoptError::Body(_))
    ));
}
