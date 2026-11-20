// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

use super::*;
use proptest::prelude::*;

fn poll_header(poll_ref: i64) -> OpenItemHeader {
    let mut body = OPEN_ITEMS_POLL_BODY_PREFIX.to_vec();
    body.extend_from_slice(&[1, 2, 3]);
    OpenItemHeader {
        poll_ref,
        author_account_key: [7; 32],
        author_sig: [8; 64],
        body,
    }
}

fn checklist_header(poll_ref: i64) -> OpenItemHeader {
    let mut body = OPEN_ITEMS_CHECKLIST_BODY_PREFIX.to_vec();
    body.push(1);
    OpenItemHeader {
        poll_ref,
        author_account_key: [9; 32],
        author_sig: [10; 64],
        body,
    }
}

fn key(i: u32) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[..4].copy_from_slice(&i.to_be_bytes());
    k[31] = 1;
    k
}

fn vote(i: u32, poll_ref: i64) -> HandoverVote {
    HandoverVote {
        voter: key(i),
        message_id: poll_ref + 1 + i64::from(i),
        created_at_ms: 1_000 + i64::from(i),
        choices: vec![u8::try_from(i % 10).unwrap_or(0)],
    }
}

fn sample() -> OpenItems {
    OpenItems {
        items: vec![
            OpenItemEntry {
                header: poll_header(10),
                part: OpenItemPart::AnonymousPoll {
                    ring_total: 3,
                    lengths: vec![2, 3],
                    key_offset: 0,
                    keys: vec![key(1), key(2), key(3)],
                },
            },
            OpenItemEntry {
                header: poll_header(20),
                part: OpenItemPart::Poll {
                    vote_total: 2,
                    vote_offset: 0,
                    votes: vec![vote(1, 20), vote(2, 20)],
                },
            },
            OpenItemEntry {
                header: checklist_header(30),
                part: OpenItemPart::Checklist {
                    added: vec![HandoverAdded {
                        message_id: 31,
                        adder: [3; 32],
                        text: "milk".to_owned(),
                    }],
                    ticks: vec![
                        HandoverTick {
                            item: HandoverItemRef::Author(5),
                            ticker: [4; 32],
                            message_id: 33,
                            done: true,
                        },
                        HandoverTick {
                            item: HandoverItemRef::Added(31),
                            ticker: [4; 32],
                            message_id: 34,
                            done: false,
                        },
                    ],
                },
            },
        ],
    }
}

#[test]
fn round_trips_and_dispatches() {
    let items = sample();
    let wire = compose_open_items(&items).unwrap();
    assert_eq!(decode_open_items(&wire).unwrap(), items);
    assert!(is_open_items_envelope(&wire));
    match crate::incoming_envelope::dispatch_incoming_envelope(&wire).unwrap() {
        crate::incoming_envelope::IncomingEnvelope::OpenItems(d) => assert_eq!(d, items),
        other => panic!("expected open items, got {other:?}"),
    }
    let empty = compose_open_items(&OpenItems::default()).unwrap();
    assert_eq!(empty.len(), OPEN_ITEMS_ENVELOPE_MIN_LEN);
    assert_eq!(decode_open_items(&empty).unwrap(), OpenItems::default());
}

#[test]
fn version_before_length_and_wrong_domain() {
    let mut wire = compose_open_items(&sample()).unwrap();
    wire[OPEN_ITEMS_ENVELOPE_DOMAIN_V1.len()] = 9;
    assert_eq!(
        decode_open_items(&wire),
        Err(OpenItemsError::UnknownVersion(9))
    );
    assert_eq!(
        decode_open_items(OPEN_ITEMS_ENVELOPE_DOMAIN_V1),
        Err(OpenItemsError::BadLength)
    );
    assert_eq!(
        decode_open_items(b"lakko/pin/v1\0"),
        Err(OpenItemsError::WrongDomain)
    );
}

#[test]
fn every_truncation_and_trailing_byte_is_refused() {
    let wire = compose_open_items(&sample()).unwrap();
    for n in 0..wire.len() {
        assert!(decode_open_items(&wire[..n]).is_err(), "prefix {n}");
    }
    let mut longer = wire;
    longer.push(0);
    assert_eq!(decode_open_items(&longer), Err(OpenItemsError::BadLength));
}

fn with_part(part: OpenItemPart) -> OpenItems {
    let header = match part {
        OpenItemPart::Checklist { .. } => checklist_header(10),
        _ => poll_header(10),
    };
    OpenItems {
        items: vec![OpenItemEntry { header, part }],
    }
}

#[test]
fn compose_refuses_broken_parts() {
    // Lengths not ending at the total.
    let bad_ring = with_part(OpenItemPart::AnonymousPoll {
        ring_total: 3,
        lengths: vec![2],
        key_offset: 0,
        keys: vec![key(1)],
    });
    assert_eq!(compose_open_items(&bad_ring), Err(OpenItemsError::Ring));
    // Keys past the total.
    let past = with_part(OpenItemPart::AnonymousPoll {
        ring_total: 2,
        lengths: vec![2],
        key_offset: 1,
        keys: vec![key(1), key(2)],
    });
    assert_eq!(compose_open_items(&past), Err(OpenItemsError::Ring));
    // Votes out of order.
    let order = with_part(OpenItemPart::Poll {
        vote_total: 2,
        vote_offset: 0,
        votes: vec![vote(2, 10), vote(1, 10)],
    });
    assert_eq!(compose_open_items(&order), Err(OpenItemsError::Votes));
    // An empty vote (a retract is never handed over).
    let mut v = vote(1, 10);
    v.choices.clear();
    let retract = with_part(OpenItemPart::Poll {
        vote_total: 1,
        vote_offset: 0,
        votes: vec![v],
    });
    assert_eq!(compose_open_items(&retract), Err(OpenItemsError::Votes));
    // A vote before its poll.
    let mut early = vote(1, 10);
    early.message_id = 10;
    let early = with_part(OpenItemPart::Poll {
        vote_total: 1,
        vote_offset: 0,
        votes: vec![early],
    });
    assert_eq!(compose_open_items(&early), Err(OpenItemsError::OpOrder));
    // A tick naming an added item the part does not carry.
    let orphan = with_part(OpenItemPart::Checklist {
        added: vec![],
        ticks: vec![HandoverTick {
            item: HandoverItemRef::Added(40),
            ticker: [1; 32],
            message_id: 41,
            done: true,
        }],
    });
    assert_eq!(compose_open_items(&orphan), Err(OpenItemsError::Checklist));
    // Text with a format character.
    let fmt = with_part(OpenItemPart::Checklist {
        added: vec![HandoverAdded {
            message_id: 11,
            adder: [1; 32],
            text: "a\u{202e}b".to_owned(),
        }],
        ticks: vec![],
    });
    assert_eq!(compose_open_items(&fmt), Err(OpenItemsError::Checklist));
    // A poll part with a checklist body.
    let wrong_body = OpenItems {
        items: vec![OpenItemEntry {
            header: checklist_header(10),
            part: OpenItemPart::Poll {
                vote_total: 0,
                vote_offset: 0,
                votes: vec![],
            },
        }],
    };
    assert_eq!(compose_open_items(&wrong_body), Err(OpenItemsError::Body));
    // Items out of order.
    let mut swapped = sample();
    swapped.items.swap(0, 1);
    assert_eq!(compose_open_items(&swapped), Err(OpenItemsError::ItemOrder));
}

#[test]
fn trust_rule() {
    let a = [1u8; 32];
    let b = [2u8; 32];
    assert!(open_items_sender_trusted(&a, Some(&a), false));
    assert!(open_items_sender_trusted(&a, Some(&b), true));
    assert!(open_items_sender_trusted(&a, None, true));
    assert!(!open_items_sender_trusted(&a, Some(&b), false));
    assert!(!open_items_sender_trusted(&a, None, false));
}

#[test]
fn handover_due_rule() {
    assert!(open_items_handover_due(1, 1));
    assert!(!open_items_handover_due(0, 3));
    assert!(!open_items_handover_due(2, 0));
}

fn decode_all(rows: &[Vec<u8>]) -> Vec<OpenItems> {
    rows.iter()
        .map(|r| {
            assert!(r.len() <= OPEN_ITEMS_ENVELOPE_MAX_LEN);
            decode_open_items(r).unwrap()
        })
        .collect()
}

fn assemble(rows: &[OpenItems], sender: [u8; 32]) -> Vec<AssembledItem> {
    let received: Vec<ReceivedOpenItems<'_>> = rows
        .iter()
        .map(|items| ReceivedOpenItems {
            sender_account_key: sender,
            items,
        })
        .collect();
    assemble_open_items(&received, &BTreeSet::new())
}

fn expected_ring(keys: &[[u8; 32]], lengths: &[u32]) -> Vec<u8> {
    let mut out = len_u32(keys.len()).to_be_bytes().to_vec();
    for k in keys {
        out.extend_from_slice(k);
    }
    out.extend_from_slice(&u16::try_from(lengths.len()).unwrap().to_be_bytes());
    for l in lengths {
        out.extend_from_slice(&l.to_be_bytes());
    }
    out
}

#[test]
fn a_ring_at_the_cap_splits_and_reassembles() {
    let keys: Vec<[u8; 32]> = (0..u32::try_from(OPEN_ITEMS_MAX_RING_LEN).unwrap())
        .map(key)
        .collect();
    let lengths = vec![100, 200_000, len_u32(keys.len())];
    let votes: Vec<HandoverVote> = (0..40_000).map(|i| vote(i, 5)).collect();
    let items = vec![
        OpenItem {
            header: poll_header(50),
            content: OpenItemContent::AnonymousPoll {
                keys: keys.clone(),
                lengths: lengths.clone(),
            },
        },
        OpenItem {
            header: poll_header(5),
            content: OpenItemContent::Poll {
                votes: votes.iter().rev().cloned().collect(),
            },
        },
        OpenItem {
            header: checklist_header(60),
            content: OpenItemContent::Checklist {
                added: vec![],
                ticks: vec![],
            },
        },
    ];
    let rows = plan_open_items_rows(&items).unwrap();
    assert!(rows.len() >= 10, "{} rows", rows.len());
    let decoded = decode_all(&rows);
    let out = assemble(&decoded, [1; 32]);
    assert_eq!(out.len(), 3);
    let by_ref: BTreeMap<i64, &AssembledItem> =
        out.iter().map(|a| (a.header.poll_ref, a)).collect();
    assert_eq!(
        by_ref[&50].content,
        AssembledContent::AnonymousPoll {
            ring: expected_ring(&keys, &lengths)
        }
    );
    assert_eq!(by_ref[&5].content, AssembledContent::Poll { votes });
    assert!(matches!(
        by_ref[&60].content,
        AssembledContent::Checklist { .. }
    ));
    // Reverse row order assembles to the same items.
    let mut reversed = decoded.clone();
    reversed.reverse();
    let mut again = assemble(&reversed, [1; 32]);
    again.sort_by_key(|a| a.header.poll_ref);
    let mut first = out;
    first.sort_by_key(|a| a.header.poll_ref);
    assert_eq!(again, first);
}

#[test]
fn a_missing_part_or_a_contradiction_gives_nothing() {
    let keys: Vec<[u8; 32]> = (0..60_000).map(key).collect();
    let items = vec![OpenItem {
        header: poll_header(5),
        content: OpenItemContent::AnonymousPoll {
            keys,
            lengths: vec![60_000],
        },
    }];
    let rows = decode_all(&plan_open_items_rows(&items).unwrap());
    assert!(rows.len() >= 2);
    // One part missing.
    assert!(assemble(&rows[1..], [1; 32]).is_empty());
    // A part repeated identically is harmless.
    let mut repeated = rows.clone();
    repeated.insert(1, rows[0].clone());
    assert_eq!(assemble(&repeated, [1; 32]).len(), 1);
    // A part that overlaps with other keys poisons the sender's group.
    let mut forged = rows[0].clone();
    if let OpenItemPart::AnonymousPoll { keys, .. } = &mut forged.items[0].part {
        keys[0] = key(999_999);
    }
    let mut contradicted = vec![forged];
    contradicted.extend(rows.iter().cloned());
    assert!(assemble(&contradicted, [1; 32]).is_empty());
    // Parts from two senders are never joined.
    let received = vec![
        ReceivedOpenItems {
            sender_account_key: [1; 32],
            items: &rows[0],
        },
        ReceivedOpenItems {
            sender_account_key: [2; 32],
            items: &rows[1],
        },
    ];
    let joined_rest: Vec<ReceivedOpenItems<'_>> = rows[2..]
        .iter()
        .map(|items| ReceivedOpenItems {
            sender_account_key: [2; 32],
            items,
        })
        .collect();
    let mut all = received;
    all.extend(joined_rest);
    assert!(assemble_open_items(&all, &BTreeSet::new()).is_empty());
}

#[test]
fn held_items_are_skipped_and_duplicates_refused() {
    let rows = decode_all(&[compose_open_items(&sample()).unwrap()]);
    let held: BTreeSet<i64> = [10, 30].into_iter().collect();
    let received = [ReceivedOpenItems {
        sender_account_key: [1; 32],
        items: &rows[0],
    }];
    let out = assemble_open_items(&received, &held);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].header.poll_ref, 20);
    let dup = vec![
        OpenItem {
            header: poll_header(1),
            content: OpenItemContent::Poll { votes: vec![] },
        },
        OpenItem {
            header: poll_header(1),
            content: OpenItemContent::Poll { votes: vec![] },
        },
    ];
    assert_eq!(
        plan_open_items_rows(&dup),
        Err(OpenItemsError::DuplicateItem)
    );
    assert!(plan_open_items_rows(&[]).unwrap().is_empty());
}

#[test]
fn many_small_items_respect_the_item_cap() {
    let items: Vec<OpenItem> = (1..=200)
        .map(|i| OpenItem {
            header: poll_header(i),
            content: OpenItemContent::Poll { votes: vec![] },
        })
        .collect();
    let rows = decode_all(&plan_open_items_rows(&items).unwrap());
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| r.items.len() <= OPEN_ITEMS_MAX_ITEMS));
    let out = assemble(&rows, [1; 32]);
    assert_eq!(out.len(), 200);
}

#[test]
fn separator_distinct_from_sibling_domains() {
    for d in [
        crate::poll_ballots::POLL_BALLOTS_ENVELOPE_DOMAIN_V1,
        crate::poll_ballots::POLL_BALLOTS_ROW_PREFIX,
        crate::pin::PIN_ENVELOPE_DOMAIN_V1,
        crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
        crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
        OPEN_ITEMS_POLL_BODY_PREFIX,
        OPEN_ITEMS_CHECKLIST_BODY_PREFIX,
    ] {
        assert!(
            !d.starts_with(OPEN_ITEMS_ENVELOPE_DOMAIN_V1)
                && !OPEN_ITEMS_ENVELOPE_DOMAIN_V1.starts_with(d),
            "{d:?}"
        );
    }
}

proptest! {
    #[test]
    fn decode_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
        let mut input = OPEN_ITEMS_ENVELOPE_DOMAIN_V1.to_vec();
        input.push(OPEN_ITEMS_VERSION_V1);
        input.extend_from_slice(&bytes);
        if let Ok(items) = decode_open_items(&input) {
            prop_assert_eq!(compose_open_items(&items).unwrap(), input);
        }
    }

    #[test]
    fn any_byte_flip_never_panics(i in 0usize..400, flip in 1u8..=255) {
        let mut wire = compose_open_items(&sample()).unwrap();
        let i = i % wire.len();
        wire[i] ^= flip;
        if let Ok(items) = decode_open_items(&wire) {
            prop_assert_eq!(compose_open_items(&items).unwrap(), wire);
        }
    }

    #[test]
    fn plan_then_assemble_is_identity_for_votes(n in 0u32..3000, order in any::<u64>()) {
        let mut votes: Vec<HandoverVote> = (0..n).map(|i| vote(i, 1)).collect();
        // A deterministic shuffle: the planner sorts.
        let len = votes.len();
        if len > 1 {
            for i in 0..len {
                let j = usize::try_from((order.wrapping_mul(i as u64 + 1)) % len as u64).unwrap_or(0);
                votes.swap(i, j);
            }
        }
        let rows = decode_all(&plan_open_items_rows(&[OpenItem {
            header: poll_header(1),
            content: OpenItemContent::Poll { votes: votes.clone() },
        }]).unwrap());
        let out = assemble(&rows, [3; 32]);
        votes.sort_by_key(|v| v.voter);
        prop_assert_eq!(out.len(), 1);
        prop_assert_eq!(&out[0].content, &AssembledContent::Poll { votes });
    }
}
