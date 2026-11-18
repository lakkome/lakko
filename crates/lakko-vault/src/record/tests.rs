// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tests of the saved-record codec.
//!
//! In the crate rather than under `tests/` so they can reach the private
//! `write`, the layout without the bounds: that is how the decoder is handed
//! an encoding exactly one past a limit.

use super::*;
use crate::keys::RefKind;

fn minimal(id: u64) -> VaultMessage {
    VaultMessage {
        message_id: id,
        direction: Direction::Outgoing,
        author: None,
        created_at_ms: 0,
        last_op_id: id,
        edited: false,
        deleted: false,
        text: String::new(),
        attachments: Vec::new(),
        reply: None,
        forward: None,
        reactions: Vec::new(),
        call: None,
        unsupported_version: None,
        content: None,
    }
}

fn thumb() -> VaultThumb {
    VaultThumb {
        blob_id: [0x21; 32],
        key: [0x22; 32],
        nonce: [0x23; 12],
        mime: "t".into(),
        size: 9,
        width: None,
        height: Some(8),
    }
}

fn attachment() -> VaultAttachment {
    VaultAttachment {
        blob_id: [0x11; 32],
        key: [0x12; 32],
        nonce: [0x13; 12],
        size: 7,
        mime: "a/b".into(),
        name: "n".into(),
        kind: "gif".into(),
        duration_ms: Some(5),
        waveform: vec![1, 2],
        width: Some(3),
        height: Some(4),
        thumb: Some(thumb()),
        title: "Song".into(),
        artist: "Band".into(),
    }
}

fn full(id: u64) -> VaultMessage {
    VaultMessage {
        message_id: id,
        direction: Direction::Incoming,
        author: Some([0x44; 32]),
        created_at_ms: -5,
        last_op_id: id + 1,
        edited: true,
        deleted: false,
        text: "hi".into(),
        attachments: vec![attachment()],
        reply: Some(VaultReply {
            target: 2,
            author: "A".into(),
            excerpt: "E".into(),
        }),
        forward: Some(VaultForward {
            author: "F".into(),
            origin: Some(VaultForwardOrigin {
                group_id: vec![0xAA],
                message_id: 6,
            }),
        }),
        reactions: vec![VaultReaction {
            reactor: [0x33; 32],
            emoji: "+".into(),
        }],
        call: Some(VaultCall {
            outcome: 2,
            video: true,
            duration_secs: 60,
        }),
        unsupported_version: Some(9),
        content: Some(VaultContent {
            kind: 1,
            payload: b"{}".to_vec(),
            state: vec![1, 2],
        }),
    }
}

fn group_ref(byte: u8) -> ConversationRef {
    ConversationRef::new(RefKind::Group, &[byte, byte]).unwrap()
}

fn head() -> ConversationHead {
    ConversationHead {
        conversation: group_ref(0xC1),
        kind: ConversationKind::Group,
        title: "T".into(),
        people: vec![VaultPerson {
            account_key: [0x51; 32],
            display_name: "D".into(),
            username: "u".into(),
            role: Role::Owner,
        }],
        membership: Membership::In,
        shared_pins: vec![SharedPin {
            message_id: 3,
            pinned: true,
            updated_at_ms: 4,
        }],
        personal_pins: vec![PersonalPin {
            message_id: 5,
            pinned_at_ms: 6,
        }],
        last_activity_ms: 7,
        chunks: vec![ChunkEntry {
            count: 8,
            first_id: 9,
            last_id: 10,
            version: 11,
        }],
        tail: vec![minimal(12)],
        reentry: vec![ReentryEpoch {
            epoch: 13,
            group_info: vec![0xEE, 0xEF],
        }],
        registrar_seed: Some(RegistrarSeed {
            rank: 2,
            seed: SecretBytes::new([0x61; 32]),
        }),
    }
}

fn empty_head() -> ConversationHead {
    ConversationHead {
        conversation: group_ref(0xC2),
        kind: ConversationKind::Unknown,
        title: String::new(),
        people: Vec::new(),
        membership: Membership::Left,
        shared_pins: Vec::new(),
        personal_pins: Vec::new(),
        last_activity_ms: 0,
        chunks: Vec::new(),
        tail: Vec::new(),
        reentry: Vec::new(),
        registrar_seed: None,
    }
}

fn index() -> VaultIndex {
    VaultIndex {
        entries: vec![
            IndexEntry {
                conversation: group_ref(0xC1),
                last_activity_ms: 7,
                membership: Membership::In,
            },
            IndexEntry {
                conversation: ConversationRef::new(RefKind::OneToOne, &[0xC1, 0xC1]).unwrap(),
                last_activity_ms: 8,
                membership: Membership::Removed,
            },
        ],
    }
}

fn chunk_of(message: VaultMessage) -> HistoryChunk {
    HistoryChunk {
        messages: vec![message],
    }
}

// --- the wire, written out by hand ------------------------------------

fn minimal_message_bytes(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&id.to_be_bytes()); // message_id
    b.push(0); // direction: outgoing
    b.push(0); // no author
    b.extend_from_slice(&0i64.to_be_bytes()); // created_at_ms
    b.extend_from_slice(&id.to_be_bytes()); // last_op_id
    b.push(0); // flags
    b.extend_from_slice(&0u32.to_be_bytes()); // text_len
    b.push(0); // attachment_count
    b.push(0); // no reply
    b.push(0); // no forward
    b.extend_from_slice(&0u16.to_be_bytes()); // reaction_count
    b.push(0); // no call
    b.push(0); // no unsupported_version
    b.push(0); // no content
    b
}

#[test]
fn a_minimal_chunk_is_pinned() {
    let mut expected = vec![0x02, 0x00, 0x01];
    expected.extend_from_slice(&minimal_message_bytes(12));
    assert_eq!(minimal_message_bytes(12).len(), MIN_MESSAGE_LEN);
    let chunk = chunk_of(minimal(12));
    assert_eq!(chunk.encode().unwrap(), expected);
    assert_eq!(HistoryChunk::decode(&expected).unwrap(), chunk);

    let empty = HistoryChunk {
        messages: Vec::new(),
    };
    assert_eq!(empty.encode().unwrap(), [0x02, 0x00, 0x00]);
    assert_eq!(HistoryChunk::decode(&[0x02, 0x00, 0x00]).unwrap(), empty);
}

fn full_attachment_bytes() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&[0x11; 32]); // blob_id
    b.extend_from_slice(&[0x12; 32]); // key
    b.extend_from_slice(&[0x13; 12]); // nonce
    b.extend_from_slice(&7u64.to_be_bytes()); // size
    b.extend_from_slice(&[0, 3]); // mime_len
    b.extend_from_slice(b"a/b");
    b.extend_from_slice(&[0, 1]); // name_len
    b.extend_from_slice(b"n");
    b.push(3); // kind_len
    b.extend_from_slice(b"gif");
    b.push(1); // duration present
    b.extend_from_slice(&5u32.to_be_bytes());
    b.push(2); // waveform_len
    b.extend_from_slice(&[1, 2]);
    b.push(1); // width present
    b.extend_from_slice(&3u16.to_be_bytes());
    b.push(1); // height present
    b.extend_from_slice(&4u16.to_be_bytes());
    b.push(1); // thumb present
    b.extend_from_slice(&[0x21; 32]); // thumb blob_id
    b.extend_from_slice(&[0x22; 32]); // thumb key
    b.extend_from_slice(&[0x23; 12]); // thumb nonce
    b.extend_from_slice(&[0, 1]); // thumb mime_len
    b.extend_from_slice(b"t");
    b.extend_from_slice(&9u32.to_be_bytes()); // thumb size
    b.push(0); // no thumb width
    b.push(1); // thumb height present
    b.extend_from_slice(&8u16.to_be_bytes());
    b.push(4); // title_len
    b.extend_from_slice(b"Song");
    b.push(4); // artist_len
    b.extend_from_slice(b"Band");
    b
}

fn full_message_bytes(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&id.to_be_bytes()); // message_id
    b.push(1); // direction: incoming
    b.push(1); // author present
    b.extend_from_slice(&[0x44; 32]);
    b.extend_from_slice(&(-5i64).to_be_bytes()); // created_at_ms
    b.extend_from_slice(&(id + 1).to_be_bytes()); // last_op_id
    b.push(0b01); // flags: edited
    b.extend_from_slice(&2u32.to_be_bytes()); // text_len
    b.extend_from_slice(b"hi");
    b.push(1); // attachment_count
    b.extend_from_slice(&full_attachment_bytes());
    b.push(1); // reply present
    b.extend_from_slice(&2u64.to_be_bytes()); // target
    b.extend_from_slice(&[0, 1]);
    b.extend_from_slice(b"A");
    b.extend_from_slice(&[0, 1]);
    b.extend_from_slice(b"E");
    b.push(1); // forward present
    b.extend_from_slice(&[0, 1]);
    b.extend_from_slice(b"F");
    b.push(1); // origin present
    b.push(1); // group_id_len
    b.push(0xAA);
    b.extend_from_slice(&6u64.to_be_bytes()); // origin message_id
    b.extend_from_slice(&1u16.to_be_bytes()); // reaction_count
    b.extend_from_slice(&[0x33; 32]); // reactor
    b.push(1); // emoji_len
    b.extend_from_slice(b"+");
    b.push(1); // call present
    b.push(2); // outcome
    b.push(1); // video
    b.extend_from_slice(&60u32.to_be_bytes()); // duration_secs
    b.push(1); // unsupported_version present
    b.extend_from_slice(&9u32.to_be_bytes());
    b.push(1); // content present
    b.push(1); // kind: poll
    b.extend_from_slice(&2u32.to_be_bytes()); // payload_len
    b.extend_from_slice(b"{}");
    b.extend_from_slice(&2u16.to_be_bytes()); // state_len
    b.extend_from_slice(&[1, 2]);
    b
}

#[test]
fn a_full_message_is_pinned() {
    let mut expected = vec![0x02, 0x00, 0x01];
    expected.extend_from_slice(&full_message_bytes(40));
    let chunk = chunk_of(full(40));
    assert_eq!(chunk.encode().unwrap(), expected);
    assert_eq!(HistoryChunk::decode(&expected).unwrap(), chunk);
    assert_eq!(full(40).wire_bytes(), full_message_bytes(40));
}

#[test]
fn a_head_is_pinned() {
    let mut e = vec![0x02]; // version
    e.extend_from_slice(&[0x01, 0x02, 0x02, 0xC1, 0xC1]); // ref: group, 2-byte id
    e.push(2); // kind: group
    e.extend_from_slice(&[0, 1]); // title_len
    e.extend_from_slice(b"T");
    e.extend_from_slice(&1u16.to_be_bytes()); // people_count
    e.extend_from_slice(&[0x51; 32]); // account_key
    e.push(1);
    e.extend_from_slice(b"D"); // display_name
    e.push(1);
    e.extend_from_slice(b"u"); // username
    e.push(2); // role: owner
    e.push(0); // membership: in
    e.extend_from_slice(&1u16.to_be_bytes()); // shared_pin_count
    e.extend_from_slice(&3u64.to_be_bytes());
    e.push(1); // pinned
    e.extend_from_slice(&4u64.to_be_bytes());
    e.extend_from_slice(&1u16.to_be_bytes()); // personal_pin_count
    e.extend_from_slice(&5u64.to_be_bytes());
    e.extend_from_slice(&6u64.to_be_bytes());
    e.extend_from_slice(&7u64.to_be_bytes()); // last_activity_ms
    e.extend_from_slice(&1u32.to_be_bytes()); // chunk_count
    e.extend_from_slice(&8u16.to_be_bytes());
    e.extend_from_slice(&9u64.to_be_bytes());
    e.extend_from_slice(&10u64.to_be_bytes());
    e.extend_from_slice(&11u64.to_be_bytes());
    e.extend_from_slice(&1u16.to_be_bytes()); // tail_count
    e.extend_from_slice(&minimal_message_bytes(12));
    e.push(1); // reentry_count
    e.extend_from_slice(&13u64.to_be_bytes()); // epoch
    e.extend_from_slice(&2u32.to_be_bytes()); // group_info_len
    e.extend_from_slice(&[0xEE, 0xEF]);
    e.push(1); // registrar present
    e.push(2); // rank
    e.extend_from_slice(&[0x61; 32]); // seed

    assert_eq!(head().encode().unwrap(), e);
    assert_eq!(ConversationHead::decode(&e).unwrap(), head());
}

#[test]
fn a_devices_head_and_an_index_are_pinned() {
    let devices = DevicesHead {
        reentry: vec![
            ReentryEpoch {
                epoch: 4,
                group_info: vec![0xA1],
            },
            ReentryEpoch {
                epoch: 5,
                group_info: vec![0xA2, 0xA3],
            },
        ],
    };
    let mut e = vec![0x02, 0x02];
    e.extend_from_slice(&4u64.to_be_bytes());
    e.extend_from_slice(&1u32.to_be_bytes());
    e.push(0xA1);
    e.extend_from_slice(&5u64.to_be_bytes());
    e.extend_from_slice(&2u32.to_be_bytes());
    e.extend_from_slice(&[0xA2, 0xA3]);
    assert_eq!(devices.encode().unwrap(), e);
    assert_eq!(DevicesHead::decode(&e).unwrap(), devices);
    let none = DevicesHead {
        reentry: Vec::new(),
    };
    assert_eq!(none.encode().unwrap(), [0x02, 0x00]);
    assert_eq!(DevicesHead::decode(&[0x02, 0x00]).unwrap(), none);

    let mut e = vec![0x02];
    e.extend_from_slice(&2u32.to_be_bytes()); // entry_count
    e.extend_from_slice(&[0x01, 0x02, 0x02, 0xC1, 0xC1]); // ref: group
    e.extend_from_slice(&7u64.to_be_bytes());
    e.push(0); // in
    e.extend_from_slice(&[0x01, 0x01, 0x02, 0xC1, 0xC1]); // ref: one-to-one
    e.extend_from_slice(&8u64.to_be_bytes());
    e.push(2); // removed
    assert_eq!(index().encode().unwrap(), e);
    assert_eq!(VaultIndex::decode(&e).unwrap(), index());
    let empty = VaultIndex {
        entries: Vec::new(),
    };
    assert_eq!(empty.encode().unwrap(), [0x02, 0, 0, 0, 0]);
}

// --- round trips ------------------------------------------------------

#[test]
fn records_round_trip() {
    let chunk = HistoryChunk {
        messages: vec![minimal(1), full(2), minimal(u64::MAX)],
    };
    let bytes = chunk.encode().unwrap();
    let back = HistoryChunk::decode(&bytes).unwrap();
    assert_eq!(back, chunk);
    assert_eq!(back.encode().unwrap(), bytes);

    let mut rich = head();
    rich.tail = vec![full(20), minimal(21)];
    rich.reentry.push(ReentryEpoch {
        epoch: 14,
        group_info: vec![1; 300],
    });
    for h in [rich, empty_head()] {
        let bytes = h.encode().unwrap();
        let back = ConversationHead::decode(&bytes).unwrap();
        assert_eq!(back, h);
        assert_eq!(back.encode().unwrap(), bytes);
    }

    let bytes = index().encode().unwrap();
    assert_eq!(VaultIndex::decode(&bytes).unwrap(), index());
}

#[test]
fn every_enum_value_round_trips_and_the_next_is_refused() {
    for byte in 0..=u8::MAX {
        assert_eq!(Direction::from_wire(byte).is_some(), byte <= 2);
        assert_eq!(ConversationKind::from_wire(byte).is_some(), byte <= 2);
        assert_eq!(Role::from_wire(byte).is_some(), byte <= 2);
        assert_eq!(Membership::from_wire(byte).is_some(), byte <= 2);
    }
    assert_eq!(Direction::System.to_wire(), 2);
    assert_eq!(ConversationKind::OneToOne.to_wire(), 1);
    assert_eq!(ConversationKind::Group.to_wire(), 2);
    assert_eq!(Role::Member.to_wire(), 0);
    assert_eq!(Role::Admin.to_wire(), 1);
    assert_eq!(Role::Owner.to_wire(), 2);
    assert_eq!(Membership::In.to_wire(), 0);
    assert_eq!(Membership::Left.to_wire(), 1);
    assert_eq!(Membership::Removed.to_wire(), 2);
}

// --- bounds: accepted at the limit, refused one past it ---------------

/// `at` is at a limit and must round-trip; `past` is one past it and must
/// be refused with `err` by the encoder and, handed the unchecked layout, by
/// the decoder.
fn message_bound(at: VaultMessage, past: VaultMessage, err: RecordError) {
    let ok = chunk_of(at);
    let bytes = ok.encode().unwrap();
    assert_eq!(HistoryChunk::decode(&bytes).unwrap(), ok);
    let bad = chunk_of(past);
    assert_eq!(bad.encode(), Err(err));
    assert_eq!(HistoryChunk::decode(&bad.write().unwrap()), Err(err));
}

fn head_bound(at: &ConversationHead, past: &ConversationHead, err: RecordError) {
    let bytes = at.encode().unwrap();
    assert_eq!(&ConversationHead::decode(&bytes).unwrap(), at);
    assert_eq!(past.encode(), Err(err));
    assert_eq!(ConversationHead::decode(&past.write().unwrap()), Err(err));
}

fn with(edit: impl FnOnce(&mut VaultMessage)) -> VaultMessage {
    let mut message = full(1);
    edit(&mut message);
    message
}

#[test]
fn text_and_list_bounds() {
    message_bound(
        with(|m| m.text = "x".repeat(MAX_TEXT_BYTES)),
        with(|m| m.text = "x".repeat(MAX_TEXT_BYTES + 1)),
        RecordError::Text,
    );
    message_bound(
        with(|m| m.attachments = vec![attachment(); MAX_ATTACHMENTS]),
        with(|m| m.attachments = vec![attachment(); MAX_ATTACHMENTS + 1]),
        RecordError::Attachments,
    );
    let reaction = VaultReaction {
        reactor: [9; 32],
        emoji: "+".into(),
    };
    message_bound(
        with(|m| m.reactions = vec![reaction.clone(); MAX_REACTIONS]),
        with(|m| m.reactions = vec![reaction.clone(); MAX_REACTIONS + 1]),
        RecordError::Reactions,
    );
}

fn with_attachment(edit: impl FnOnce(&mut VaultAttachment)) -> VaultMessage {
    let mut a = attachment();
    edit(&mut a);
    with(|m| m.attachments = vec![a])
}

#[test]
fn attachment_field_bounds() {
    let err = RecordError::Attachment;
    message_bound(
        with_attachment(|a| a.mime = "m".repeat(MAX_MIME_BYTES)),
        with_attachment(|a| a.mime = "m".repeat(MAX_MIME_BYTES + 1)),
        err,
    );
    message_bound(
        with_attachment(|a| a.name = "n".repeat(MAX_NAME_BYTES)),
        with_attachment(|a| a.name = "n".repeat(MAX_NAME_BYTES + 1)),
        err,
    );
    message_bound(
        with_attachment(|a| a.kind = "k".repeat(MAX_KIND_BYTES)),
        with_attachment(|a| a.kind = "k".repeat(MAX_KIND_BYTES + 1)),
        err,
    );
    message_bound(
        with_attachment(|a| a.waveform = vec![7; MAX_WAVEFORM_BYTES]),
        with_attachment(|a| a.waveform = vec![7; MAX_WAVEFORM_BYTES + 1]),
        err,
    );
    let thumb_mime = |len: usize| {
        with_attachment(|a| {
            let mut t = thumb();
            t.mime = "t".repeat(len);
            a.thumb = Some(t);
        })
    };
    message_bound(
        thumb_mime(MAX_MIME_BYTES),
        thumb_mime(MAX_MIME_BYTES + 1),
        err,
    );
}

#[test]
fn reply_forward_and_reaction_bounds() {
    let reply = |author: usize, excerpt: usize| {
        with(|m| {
            m.reply = Some(VaultReply {
                target: 1,
                author: "a".repeat(author),
                excerpt: "e".repeat(excerpt),
            });
        })
    };
    message_bound(
        reply(MAX_REPLY_AUTHOR_BYTES, MAX_REPLY_EXCERPT_BYTES),
        reply(MAX_REPLY_AUTHOR_BYTES + 1, 0),
        RecordError::Reply,
    );
    message_bound(
        reply(0, MAX_REPLY_EXCERPT_BYTES),
        reply(0, MAX_REPLY_EXCERPT_BYTES + 1),
        RecordError::Reply,
    );

    let forward = |author: usize, group: usize| {
        with(|m| {
            m.forward = Some(VaultForward {
                author: "f".repeat(author),
                origin: Some(VaultForwardOrigin {
                    group_id: vec![0xAB; group],
                    message_id: 1,
                }),
            });
        })
    };
    message_bound(
        forward(MAX_FORWARD_AUTHOR_BYTES, MAX_FORWARD_ORIGIN_GROUP_BYTES),
        forward(MAX_FORWARD_AUTHOR_BYTES + 1, 1),
        RecordError::Forward,
    );
    message_bound(
        forward(0, MAX_FORWARD_ORIGIN_GROUP_BYTES),
        forward(0, MAX_FORWARD_ORIGIN_GROUP_BYTES + 1),
        RecordError::Forward,
    );
    // The origin's lower bound: one byte is accepted, none is refused.
    message_bound(forward(0, 1), forward(0, 0), RecordError::Forward);

    let emoji = |len: usize| {
        with(|m| {
            m.reactions = vec![VaultReaction {
                reactor: [9; 32],
                emoji: "e".repeat(len),
            }];
        })
    };
    message_bound(
        emoji(MAX_EMOJI_BYTES),
        emoji(MAX_EMOJI_BYTES + 1),
        RecordError::Reactions,
    );
    message_bound(emoji(1), emoji(0), RecordError::Reactions);
}

#[test]
fn chunk_and_tail_message_counts() {
    let messages = |n: usize| (0..n as u64).map(minimal).collect::<Vec<_>>();
    let at = HistoryChunk {
        messages: messages(MAX_CHUNK_MESSAGES),
    };
    let bytes = at.encode().unwrap();
    assert_eq!(HistoryChunk::decode(&bytes).unwrap(), at);
    let past = HistoryChunk {
        messages: messages(MAX_CHUNK_MESSAGES + 1),
    };
    assert_eq!(past.encode(), Err(RecordError::Messages));
    assert_eq!(
        HistoryChunk::decode(&past.write().unwrap()),
        Err(RecordError::Messages)
    );

    let mut at = empty_head();
    at.tail = messages(MAX_TAIL_MESSAGES);
    let mut past = empty_head();
    past.tail = messages(MAX_TAIL_MESSAGES + 1);
    head_bound(&at, &past, RecordError::Messages);
}

fn head_with(edit: impl FnOnce(&mut ConversationHead)) -> ConversationHead {
    let mut h = empty_head();
    edit(&mut h);
    h
}

fn person(display_name: usize, username: usize) -> VaultPerson {
    VaultPerson {
        account_key: [3; 32],
        display_name: "d".repeat(display_name),
        username: "u".repeat(username),
        role: Role::Member,
    }
}

#[test]
fn head_title_and_people_bounds() {
    head_bound(
        &head_with(|h| h.title = "t".repeat(MAX_TITLE_BYTES)),
        &head_with(|h| h.title = "t".repeat(MAX_TITLE_BYTES + 1)),
        RecordError::Title,
    );
    head_bound(
        &head_with(|h| h.people = vec![person(1, 1); MAX_PEOPLE]),
        &head_with(|h| h.people = vec![person(1, 1); MAX_PEOPLE + 1]),
        RecordError::People,
    );
    head_bound(
        &head_with(|h| h.people = vec![person(0, MAX_USERNAME_BYTES)]),
        &head_with(|h| h.people = vec![person(0, MAX_USERNAME_BYTES + 1)]),
        RecordError::People,
    );
    // A display name's bound is the largest length its one-byte prefix can
    // say, so one past it has no encoding to hand the decoder; the encoder
    // refuses it.
    let at = head_with(|h| h.people = vec![person(MAX_DISPLAY_NAME_BYTES, 0)]);
    assert_eq!(ConversationHead::decode(&at.encode().unwrap()).unwrap(), at);
    let past = head_with(|h| h.people = vec![person(MAX_DISPLAY_NAME_BYTES + 1, 0)]);
    assert_eq!(past.encode(), Err(RecordError::People));
    assert_eq!(MAX_DISPLAY_NAME_BYTES, usize::from(u8::MAX));
}

#[test]
fn head_pin_and_chunk_table_bounds() {
    let shared = SharedPin {
        message_id: 1,
        pinned: false,
        updated_at_ms: 2,
    };
    head_bound(
        &head_with(|h| h.shared_pins = vec![shared; MAX_PINS]),
        &head_with(|h| h.shared_pins = vec![shared; MAX_PINS + 1]),
        RecordError::Pins,
    );
    let personal = PersonalPin {
        message_id: 1,
        pinned_at_ms: 2,
    };
    head_bound(
        &head_with(|h| h.personal_pins = vec![personal; MAX_PINS]),
        &head_with(|h| h.personal_pins = vec![personal; MAX_PINS + 1]),
        RecordError::Pins,
    );
    let entry = ChunkEntry {
        count: 512,
        first_id: 1,
        last_id: 600,
        version: 3,
    };
    head_bound(
        &head_with(|h| h.chunks = vec![entry; MAX_CHUNKS]),
        &head_with(|h| h.chunks = vec![entry; MAX_CHUNKS + 1]),
        RecordError::Chunks,
    );
}

fn epochs(count: usize, info_len: usize) -> Vec<ReentryEpoch> {
    (0..count as u64)
        .map(|epoch| ReentryEpoch {
            epoch,
            group_info: vec![0x6A; info_len],
        })
        .collect()
}

#[test]
fn reentry_bounds() {
    let err = RecordError::Reentry;
    head_bound(
        &head_with(|h| h.reentry = epochs(MAX_REENTRY_EPOCHS, 1)),
        &head_with(|h| h.reentry = epochs(MAX_REENTRY_EPOCHS + 1, 1)),
        err,
    );
    head_bound(
        &head_with(|h| h.reentry = epochs(1, MAX_GROUP_INFO_BYTES)),
        &head_with(|h| h.reentry = epochs(1, MAX_GROUP_INFO_BYTES + 1)),
        err,
    );
    head_bound(
        &head_with(|h| h.reentry = epochs(1, 1)),
        &head_with(|h| h.reentry = epochs(1, 0)),
        err,
    );

    // The devices head runs the same rule.
    for (reentry, expected) in [
        (epochs(MAX_REENTRY_EPOCHS, MAX_GROUP_INFO_BYTES), None),
        (epochs(MAX_REENTRY_EPOCHS + 1, 1), Some(err)),
        (epochs(1, MAX_GROUP_INFO_BYTES + 1), Some(err)),
        (epochs(1, 0), Some(err)),
    ] {
        let devices = DevicesHead { reentry };
        match expected {
            None => {
                let bytes = devices.encode().unwrap();
                assert_eq!(DevicesHead::decode(&bytes).unwrap(), devices);
            },
            Some(err) => {
                assert_eq!(devices.encode(), Err(err));
                assert_eq!(DevicesHead::decode(&devices.write().unwrap()), Err(err));
            },
        }
    }
}

fn index_of(count: usize) -> VaultIndex {
    VaultIndex {
        entries: (0..count as u64)
            .map(|i| IndexEntry {
                conversation: ConversationRef::new(RefKind::Channel, &i.to_be_bytes()).unwrap(),
                last_activity_ms: i,
                membership: Membership::In,
            })
            .collect(),
    }
}

#[test]
fn index_bounds_and_repeats() {
    let at = index_of(MAX_INDEX_ENTRIES);
    let bytes = at.encode().unwrap();
    assert_eq!(VaultIndex::decode(&bytes).unwrap(), at);
    let past = index_of(MAX_INDEX_ENTRIES + 1);
    assert_eq!(past.encode(), Err(RecordError::Entries));
    assert_eq!(
        VaultIndex::decode(&past.write().unwrap()),
        Err(RecordError::Entries)
    );

    // The same conversation twice is refused; the same id under another
    // kind is another conversation.
    assert!(index().encode().is_ok());
    let mut repeated = index();
    repeated.entries.push(repeated.entries[0].clone());
    assert_eq!(repeated.encode(), Err(RecordError::RepeatedRef));
    assert_eq!(
        VaultIndex::decode(&repeated.write().unwrap()),
        Err(RecordError::RepeatedRef)
    );
}

#[test]
fn a_reference_that_is_not_admissible_is_refused() {
    let good = head().encode().unwrap();
    // Offsets: [0] version, [1] ref tag, [2] ref kind, [3] id length.
    let patched = |offset: usize, byte: u8| {
        let mut bytes = good.clone();
        bytes[offset] = byte;
        ConversationHead::decode(&bytes)
    };
    // The index and devices tags are not a conversation.
    for tag in [0x00, 0x02, 0x03, 0xFF] {
        assert_eq!(patched(1, tag), Err(RecordError::Ref));
    }
    for kind in [0, 4, 0xFF] {
        assert_eq!(patched(2, kind), Err(RecordError::Ref));
    }
    assert_eq!(patched(3, 0), Err(RecordError::Ref));
    assert_eq!(patched(3, 65), Err(RecordError::Ref));
    // The longest id is accepted.
    let mut longest = empty_head();
    longest.conversation =
        ConversationRef::new(RefKind::OneToOne, &[5; MAX_CONVERSATION_ID_LEN]).unwrap();
    let bytes = longest.encode().unwrap();
    assert_eq!(ConversationHead::decode(&bytes).unwrap(), longest);

    let good = index().encode().unwrap();
    let mut bytes = good;
    bytes[5] = 0x02;
    assert_eq!(VaultIndex::decode(&bytes), Err(RecordError::Ref));
}

fn empty_attachment() -> VaultAttachment {
    VaultAttachment {
        blob_id: [0; 32],
        key: [0; 32],
        nonce: [0; 12],
        size: 0,
        mime: String::new(),
        name: String::new(),
        kind: String::new(),
        duration_ms: None,
        waveform: Vec::new(),
        width: None,
        height: None,
        thumb: None,
        title: String::new(),
        artist: String::new(),
    }
}

#[test]
fn the_smallest_elements_have_the_sizes_the_count_check_assumes() {
    // A count is checked against the remaining input at these sizes. One
    // that is too large would refuse a valid record as truncated; one that
    // is too small only weakens the early check.
    let mut out = Vec::new();
    put_message(&mut out, &minimal(1));
    assert_eq!(out.len(), MIN_MESSAGE_LEN);

    let mut out = Vec::new();
    put_attachment(&mut out, &empty_attachment());
    assert_eq!(out.len(), MIN_ATTACHMENT_LEN);

    // A message whose only content is that smallest attachment, last in
    // the input, still decodes.
    let smallest = with(|m| {
        *m = minimal(1);
        m.attachments = vec![empty_attachment()];
    });
    let chunk = chunk_of(smallest);
    assert_eq!(
        HistoryChunk::decode(&chunk.encode().unwrap()).unwrap(),
        chunk
    );

    // A person with no names.
    let people = head_with(|h| h.people = vec![person(0, 0)]);
    let without = empty_head().encode().unwrap().len();
    assert_eq!(people.encode().unwrap().len() - without, MIN_PERSON_LEN);

    // The fixed-size rows.
    let pins = head_with(|h| {
        h.shared_pins = vec![SharedPin {
            message_id: 1,
            pinned: true,
            updated_at_ms: 2,
        }];
    });
    assert_eq!(pins.encode().unwrap().len() - without, SHARED_PIN_LEN);
    let pins = head_with(|h| {
        h.personal_pins = vec![PersonalPin {
            message_id: 1,
            pinned_at_ms: 2,
        }];
    });
    assert_eq!(pins.encode().unwrap().len() - without, PERSONAL_PIN_LEN);
    let table = head_with(|h| {
        h.chunks = vec![ChunkEntry {
            count: 1,
            first_id: 2,
            last_id: 3,
            version: 4,
        }];
    });
    assert_eq!(table.encode().unwrap().len() - without, CHUNK_ENTRY_LEN);
}

#[test]
fn an_element_that_must_carry_a_byte_is_sized_without_it() {
    // Where an element's own rule requires one byte, the size the count
    // check assumes is the element without it, so an empty one is refused by
    // that rule and not as a short input. The smallest valid element is one
    // more.
    let without = empty_head().encode().unwrap().len();
    let one_epoch = head_with(|h| h.reentry = epochs(1, 1));
    assert_eq!(
        one_epoch.encode().unwrap().len() - without,
        MIN_REENTRY_LEN + 1
    );
    let reacted = chunk_of(with(|m| {
        *m = minimal(1);
        m.reactions = vec![VaultReaction {
            reactor: [0; 32],
            emoji: "+".into(),
        }];
    }));
    assert_eq!(
        reacted.encode().unwrap().len() - 3 - MIN_MESSAGE_LEN,
        MIN_REACTION_LEN + 1
    );
    let one_entry = VaultIndex {
        entries: vec![IndexEntry {
            conversation: ConversationRef::new(RefKind::Group, &[1]).unwrap(),
            last_activity_ms: 0,
            membership: Membership::In,
        }],
    };
    assert_eq!(
        one_entry.encode().unwrap().len() - 5,
        MIN_INDEX_ENTRY_LEN + 1
    );
    // Each of those, alone at the end of its input, decodes.
    assert!(VaultIndex::decode(&one_entry.encode().unwrap()).is_ok());
    let devices = DevicesHead {
        reentry: epochs(1, 1),
    };
    assert!(DevicesHead::decode(&devices.encode().unwrap()).is_ok());
    assert!(HistoryChunk::decode(&reacted.encode().unwrap()).is_ok());
}

// --- what the encoder never writes ------------------------------------

#[test]
fn versions_trailing_bytes_and_truncation() {
    type Decode = fn(&[u8]) -> Result<(), RecordError>;
    let encodings = [
        chunk_of(full(1)).encode().unwrap(),
        head().encode().unwrap(),
        index().encode().unwrap(),
    ];
    let decoders: [Decode; 3] = [
        |b| HistoryChunk::decode(b).map(drop),
        |b| ConversationHead::decode(b).map(drop),
        |b| VaultIndex::decode(b).map(drop),
    ];
    for (good, decode) in encodings.iter().zip(decoders) {
        assert_eq!(decode(good), Ok(()));
        assert_eq!(decode(&[]), Err(RecordError::Truncated));

        for version in [0x00, 0x01, 0x03, 0xFF] {
            let mut bytes = good.clone();
            bytes[0] = version;
            assert_eq!(decode(&bytes), Err(RecordError::UnknownVersion));
        }

        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(decode(&trailing), Err(RecordError::TrailingBytes));

        // Cut anywhere, nothing decodes.
        for cut in 0..good.len() {
            assert!(decode(&good[..cut]).is_err(), "cut at {cut}");
        }
    }
    let devices = DevicesHead {
        reentry: epochs(2, 3),
    }
    .encode()
    .unwrap();
    let mut trailing = devices.clone();
    trailing.push(0);
    assert_eq!(
        DevicesHead::decode(&trailing),
        Err(RecordError::TrailingBytes)
    );
    for cut in 0..devices.len() {
        assert!(
            DevicesHead::decode(&devices[..cut]).is_err(),
            "cut at {cut}"
        );
    }
    assert_eq!(
        DevicesHead::decode(&[0x01, 0x00]),
        Err(RecordError::UnknownVersion)
    );
}

/// The offset of the first message in a chunk: version and count.
const MSG: usize = 3;

#[test]
fn unknown_flags_and_options_are_refused() {
    let good = chunk_of(minimal(1)).encode().unwrap();
    let patched = |offset: usize, byte: u8| {
        let mut bytes = good.clone();
        bytes[offset] = byte;
        HistoryChunk::decode(&bytes)
    };
    // Minimal message offsets: direction 8, author presence 9, flags 26,
    // reply presence 32, forward presence 33, call presence 36,
    // unsupported presence 37, content presence 38.
    assert_eq!(patched(MSG + 8, 3), Err(RecordError::UnknownOption));
    assert_eq!(patched(MSG + 26, 0b100), Err(RecordError::UnknownFlag));
    assert_eq!(patched(MSG + 26, 0x80), Err(RecordError::UnknownFlag));
    for presence in [9, 32, 33, 36, 37, 38] {
        assert_eq!(
            patched(MSG + presence, 2),
            Err(RecordError::UnknownOption),
            "{presence}"
        );
    }
    // The two defined flags, alone and together.
    for flags in 0..=0b11u8 {
        let chunk = patched(MSG + 26, flags).unwrap();
        assert_eq!(chunk.messages[0].edited, flags & 1 != 0);
        assert_eq!(chunk.messages[0].deleted, flags & 2 != 0);
    }

    // A call's `video` and a shared pin's `pinned` are booleans.
    let mut call = chunk_of(with(|m| {
        m.unsupported_version = None;
        m.content = None;
    }))
    .encode()
    .unwrap();
    let video = call.len() - 1 - 1 - 4 - 1;
    call[video] = 2;
    assert_eq!(HistoryChunk::decode(&call), Err(RecordError::UnknownOption));

    let good = head().encode().unwrap();
    let patched = |offset: usize, byte: u8| {
        let mut bytes = good.clone();
        bytes[offset] = byte;
        ConversationHead::decode(&bytes)
    };
    // Head offsets: kind 6, role 48, membership 49, pinned 60.
    assert_eq!(patched(6, 3), Err(RecordError::UnknownOption));
    assert_eq!(patched(48, 3), Err(RecordError::UnknownOption));
    assert_eq!(patched(49, 3), Err(RecordError::UnknownOption));
    assert_eq!(patched(60, 2), Err(RecordError::UnknownOption));
    let last = good.len() - 1 - 32 - 1;
    assert_eq!(patched(last, 2), Err(RecordError::UnknownOption));

    let mut bytes = index().encode().unwrap();
    let membership = bytes.len() - 1;
    bytes[membership] = 3;
    assert_eq!(VaultIndex::decode(&bytes), Err(RecordError::UnknownOption));
}

#[test]
fn text_that_is_not_utf8_is_refused() {
    // A message text.
    let mut bytes = chunk_of(with(|m| m.text = "ab".into())).encode().unwrap();
    let text = MSG + 8 + 1 + 1 + 32 + 8 + 8 + 1 + 4;
    assert_eq!(&bytes[text..text + 2], b"ab");
    bytes[text] = 0xFF;
    assert_eq!(HistoryChunk::decode(&bytes), Err(RecordError::NotUtf8));

    // An emoji: the byte before the call, unsupported and content presences.
    let mut bytes = chunk_of(with(|m| {
        m.call = None;
        m.unsupported_version = None;
        m.content = None;
    }))
    .encode()
    .unwrap();
    let emoji = bytes.len() - 4;
    assert_eq!(bytes[emoji], b'+');
    bytes[emoji] = 0xC0;
    assert_eq!(HistoryChunk::decode(&bytes), Err(RecordError::NotUtf8));

    // A head's title.
    let mut bytes = head().encode().unwrap();
    assert_eq!(bytes[9], b'T');
    bytes[9] = 0x80;
    assert_eq!(ConversationHead::decode(&bytes), Err(RecordError::NotUtf8));
}

#[test]
fn a_count_larger_than_the_input_is_refused_before_allocation() {
    // 512 messages declared, none present.
    let declared = u16::try_from(MAX_CHUNK_MESSAGES).unwrap().to_be_bytes();
    assert_eq!(
        HistoryChunk::decode(&[0x02, declared[0], declared[1]]),
        Err(RecordError::Truncated)
    );
    // One past the bound reports the bound, with or without the bytes.
    let declared = u16::try_from(MAX_CHUNK_MESSAGES + 1).unwrap().to_be_bytes();
    assert_eq!(
        HistoryChunk::decode(&[0x02, declared[0], declared[1]]),
        Err(RecordError::Messages)
    );
    assert_eq!(
        HistoryChunk::decode(&[0x02, 0xFF, 0xFF]),
        Err(RecordError::Messages)
    );

    // An index declaring 65536 entries in five bytes.
    let declared = u32::try_from(MAX_INDEX_ENTRIES).unwrap().to_be_bytes();
    let mut bytes = vec![0x02];
    bytes.extend_from_slice(&declared);
    assert_eq!(VaultIndex::decode(&bytes), Err(RecordError::Truncated));
    assert_eq!(
        VaultIndex::decode(&[0x02, 0xFF, 0xFF, 0xFF, 0xFF]),
        Err(RecordError::Entries)
    );

    // A text length that runs past the input.
    let mut bytes = chunk_of(minimal(1)).encode().unwrap();
    bytes[MSG + 27..MSG + 31].copy_from_slice(&1000u32.to_be_bytes());
    assert_eq!(HistoryChunk::decode(&bytes), Err(RecordError::Truncated));
    // One over the text bound is named as such even though the input is
    // short as well.
    let over = u32::try_from(MAX_TEXT_BYTES + 1).unwrap().to_be_bytes();
    bytes[MSG + 27..MSG + 31].copy_from_slice(&over);
    assert_eq!(HistoryChunk::decode(&bytes), Err(RecordError::Text));
}

#[test]
fn a_record_larger_than_one_object_is_refused() {
    assert_eq!(MAX_RECORD_BYTES, crate::seal::MAX_PLAINTEXT_BYTES);
    // Four messages of the longest text exceed what one object carries,
    // though each is within its own bound.
    let big = HistoryChunk {
        messages: (0..4)
            .map(|id| {
                let mut m = minimal(id);
                m.text = "x".repeat(MAX_TEXT_BYTES);
                m
            })
            .collect(),
    };
    assert!(big.validate().is_ok());
    assert_eq!(big.encode(), Err(RecordError::TooLarge));
    let mut head = empty_head();
    head.tail = big.messages;
    assert_eq!(head.encode(), Err(RecordError::TooLarge));

    let oversized = vec![0x01; MAX_RECORD_BYTES + 1];
    assert_eq!(HistoryChunk::decode(&oversized), Err(RecordError::TooLarge));
    assert_eq!(
        ConversationHead::decode(&oversized),
        Err(RecordError::TooLarge)
    );
    assert_eq!(DevicesHead::decode(&oversized), Err(RecordError::TooLarge));
    assert_eq!(VaultIndex::decode(&oversized), Err(RecordError::TooLarge));
}

#[test]
fn what_one_object_carries_can_be_sealed() {
    // The largest record the encoder accepts is the largest plaintext the
    // seal accepts.
    let key = lakko_crypto::aead::AeadKey::new([1; 32]);
    let mut chunk = HistoryChunk {
        messages: (0..3)
            .map(|id| {
                let mut m = minimal(id);
                m.text = "x".repeat(MAX_TEXT_BYTES);
                m
            })
            .collect(),
    };
    chunk.messages.push(minimal(3));
    let bytes = chunk.encode().unwrap();
    assert!(bytes.len() <= MAX_RECORD_BYTES);
    let sealed = crate::seal::seal_object(
        &key,
        &[2; 32],
        crate::seal::ObjectKind::Chunk,
        0,
        1,
        &[3; 12],
        &bytes,
    )
    .unwrap();
    let opened = crate::seal::open_object(
        &key,
        &[2; 32],
        crate::seal::ObjectKind::Chunk,
        0,
        1,
        &sealed,
    )
    .unwrap();
    assert_eq!(HistoryChunk::decode(&opened).unwrap(), chunk);
}

#[test]
fn debug_output_carries_no_key() {
    let shown = format!("{:?}", full(1));
    assert!(shown.contains("<redacted>"));
    // The attachment key is 0x12 x 32 and the thumb key 0x22 x 32; `Debug`
    // prints a byte array as decimal numbers.
    assert!(!shown.contains("18, 18, 18"));
    assert!(!shown.contains("34, 34, 34"));
    // The blob id, which is not secret, is still shown.
    assert!(shown.contains("17, 17, 17"));

    let shown = format!("{:?}", head());
    assert!(!shown.contains("97, 97, 97"));
    assert!(shown.contains("redacted"));
}

#[test]
fn reason_codes_are_distinct() {
    let all = [
        RecordError::TooLarge,
        RecordError::UnknownVersion,
        RecordError::Truncated,
        RecordError::TrailingBytes,
        RecordError::UnknownFlag,
        RecordError::UnknownOption,
        RecordError::NotUtf8,
        RecordError::Ref,
        RecordError::Text,
        RecordError::Attachments,
        RecordError::Attachment,
        RecordError::Reply,
        RecordError::Forward,
        RecordError::Reactions,
        RecordError::Messages,
        RecordError::Title,
        RecordError::People,
        RecordError::Pins,
        RecordError::Chunks,
        RecordError::Reentry,
        RecordError::Entries,
        RecordError::RepeatedRef,
    ];
    let codes: BTreeSet<&str> = all.iter().map(|e| e.code()).collect();
    assert_eq!(codes.len(), all.len());
    assert!(codes.iter().all(|c| !c.is_empty()));
}

#[test]
fn audio_tag_and_content_bounds() {
    // The tag bound is the largest a one-byte length can name, so an
    // over-long tag cannot be written at all; the encoder refuses it.
    for (at, past) in [
        (
            with_attachment(|a| a.title = "t".repeat(MAX_AUDIO_TAG_BYTES)),
            with_attachment(|a| a.title = "t".repeat(MAX_AUDIO_TAG_BYTES + 1)),
        ),
        (
            with_attachment(|a| a.artist = "a".repeat(MAX_AUDIO_TAG_BYTES)),
            with_attachment(|a| a.artist = "a".repeat(MAX_AUDIO_TAG_BYTES + 1)),
        ),
    ] {
        let ok = chunk_of(at);
        assert_eq!(HistoryChunk::decode(&ok.encode().unwrap()).unwrap(), ok);
        assert_eq!(chunk_of(past).encode(), Err(RecordError::Attachment));
    }
    let content = |kind: u8, payload: usize, state: usize| {
        with(|m| {
            m.content = Some(VaultContent {
                kind,
                payload: vec![b'x'; payload],
                state: vec![1; state],
            });
        })
    };
    message_bound(
        content(5, MAX_CONTENT_PAYLOAD_BYTES, 0),
        content(5, MAX_CONTENT_PAYLOAD_BYTES + 1, 0),
        RecordError::Content,
    );
    message_bound(
        content(2, 1, MAX_CONTENT_STATE_BYTES),
        content(2, 1, MAX_CONTENT_STATE_BYTES + 1),
        RecordError::Content,
    );
    for bad in [content(0, 1, 0), content(6, 1, 0), content(1, 0, 0)] {
        assert_eq!(chunk_of(bad).encode(), Err(RecordError::Content));
    }
    // An unknown kind byte written by hand is refused on decode.
    let mut bytes = chunk_of(full(1)).encode().unwrap();
    // From the end: state(2), state_len(2), payload(2), payload_len(4), kind.
    let kind = bytes.len() - (2 + 2 + 2 + 4 + 1);
    assert_eq!(bytes[kind], 1);
    bytes[kind] = 9;
    assert_eq!(HistoryChunk::decode(&bytes), Err(RecordError::Content));
}
