// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0090 at the envelope: every new content kind and op through
//! `compose_*` and `parse_body`, the strict refusals, and hostile input.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_attachments::article::{Block, Marks, Run};
use lakko_attachments::checklist::{ChecklistBody, ChecklistItem};
use lakko_attachments::fuzz_api::{
    article_decode_round_trip, content_bundle_round_trip, content_within_bounds,
    media_metadata_within_bounds, parse_message_body,
};
use lakko_attachments::poll::{PollBody, PollQuiz};
use lakko_attachments::{
    compose, compose_article_edit, compose_checklist_add, compose_checklist_edit,
    compose_checklist_tick, compose_content, compose_poll_close, compose_poll_vote,
    encrypt_attachment, parse, parse_body, Article, AttachmentError, AttachmentRef,
    ChecklistItemRef, ContactCard, Content, Forward, Location, MediaKind, MessageBody, Reply,
    SignedChecklist, SignedPoll, MAX_AUDIO_DURATION_MS, MEDIA_KIND_AUDIO, WIRE_VERSION,
};
use proptest::prelude::*;

const GROUP: [u8; 32] = [8; 32];
const SEED: [u8; 32] = [4; 32];

fn poll() -> SignedPoll {
    SignedPoll::sign(
        PollBody {
            id: [1; 16],
            question: "Where?".into(),
            options: vec!["Here".into(), "There".into()],
            multiple: false,
            quiz: Some(PollQuiz {
                correct: 1,
                explanation: "Because".into(),
            }),
            close_at_ms: Some(1_900_000_000_000),
            anonymous: None,
        },
        &GROUP,
        &SEED,
    )
    .unwrap()
}

fn checklist() -> SignedChecklist {
    SignedChecklist::sign(
        ChecklistBody {
            title: "Pack".into(),
            others_add: true,
            others_mark: true,
            items: vec![ChecklistItem {
                id: 7,
                text: "Socks".into(),
            }],
        },
        &GROUP,
        &SEED,
    )
    .unwrap()
}

fn image_ref() -> AttachmentRef {
    let enc = encrypt_attachment(b"jpeg").unwrap();
    AttachmentRef::from_encrypted(&enc, 4, Some("image/jpeg".into()), None)
}

fn article() -> Article {
    Article {
        title: "Story".into(),
        blocks: vec![
            Block::Paragraph(vec![Run::Text {
                marks: Marks::default(),
                text: "Once".into(),
            }]),
            Block::Image {
                attachment_index: 0,
                caption: vec![],
            },
        ],
    }
}

fn location() -> Location {
    Location {
        lat_e7: 523_702_200,
        lon_e7: 48_951_700,
        accuracy_m: 12,
        fix_at_ms: 1_800_000_000_000,
        name: Some("Corner".into()),
    }
}

fn card() -> ContactCard {
    ContactCard {
        name: "Ann".into(),
        username: "ann".into(),
        account_key: [3; 32],
    }
}

#[test]
fn every_content_kind_round_trips() {
    let reply = Reply {
        target: 5,
        author: Some("Bo".into()),
        excerpt: Some("hi".into()),
    };
    let fwd = Forward {
        author: Some("Cy".into()),
        origin: None,
    };

    let wire = compose_content(&Content::Poll(poll()), vec![], Some(&reply), None).unwrap();
    let MessageBody::Poll {
        poll: p,
        reply: r,
        forward: None,
    } = parse_body(&wire)
    else {
        panic!("poll");
    };
    assert_eq!(p, poll());
    assert_eq!(r, Some(reply));
    let key = lakko_crypto::signing::SigningKey::from_seed(&SEED)
        .verifying_key()
        .to_bytes();
    assert!(p.verify(&GROUP, &key).is_ok());

    let wire = compose_content(&Content::Checklist(checklist()), vec![], None, Some(&fwd)).unwrap();
    assert!(
        matches!(parse_body(&wire), MessageBody::Checklist { checklist: c, forward: Some(f), .. } if c == checklist() && f == fwd)
    );

    let wire = compose_content(&Content::Location(location()), vec![], None, None).unwrap();
    assert!(
        matches!(parse_body(&wire), MessageBody::Location { location: l, .. } if l == location())
    );

    let wire = compose_content(&Content::ContactCard(card()), vec![], None, None).unwrap();
    assert!(matches!(parse_body(&wire), MessageBody::ContactCard { card: c, .. } if c == card()));

    let wire =
        compose_content(&Content::Article(article()), vec![image_ref()], None, None).unwrap();
    let MessageBody::Article {
        article: a,
        attachments,
        ..
    } = parse_body(&wire)
    else {
        panic!("article");
    };
    assert_eq!(a, article());
    assert_eq!(attachments.len(), 1);
    assert!(content_within_bounds(&wire));
}

#[test]
fn every_op_round_trips() {
    let cases = vec![
        (
            compose_poll_vote(9, &[0, 2]).unwrap(),
            MessageBody::PollVote {
                target: 9,
                choices: vec![0, 2],
            },
        ),
        (
            compose_poll_vote(9, &[]).unwrap(),
            MessageBody::PollVote {
                target: 9,
                choices: vec![],
            },
        ),
        (
            compose_poll_close(9, None).unwrap(),
            MessageBody::PollClose {
                target: 9,
                tally: None,
            },
        ),
        (
            compose_checklist_tick(9, ChecklistItemRef::Author(7), true).unwrap(),
            MessageBody::ChecklistTick {
                target: 9,
                item: ChecklistItemRef::Author(7),
                done: true,
            },
        ),
        (
            compose_checklist_tick(9, ChecklistItemRef::Added(7), false).unwrap(),
            MessageBody::ChecklistTick {
                target: 9,
                item: ChecklistItemRef::Added(7),
                done: false,
            },
        ),
        (
            compose_checklist_add(9, "Hat").unwrap(),
            MessageBody::ChecklistAdd {
                target: 9,
                text: "Hat".into(),
            },
        ),
        (
            compose_checklist_edit(9, &checklist()).unwrap(),
            MessageBody::EditChecklist {
                target: 9,
                checklist: checklist(),
            },
        ),
    ];
    for (wire, expected) in cases {
        assert_eq!(parse_body(&wire), expected);
        assert!(content_within_bounds(&wire));
    }
    let wire = compose_article_edit(9, &article(), vec![image_ref()]).unwrap();
    assert!(
        matches!(parse_body(&wire), MessageBody::EditArticle { target: 9, article: a, .. } if a == article())
    );
}

#[test]
fn compose_refuses_what_parse_refuses() {
    assert!(matches!(
        compose_poll_vote(1, &[2, 1]),
        Err(AttachmentError::Content(_))
    ));
    assert!(compose_poll_vote(1, &[10]).is_err());
    assert!(compose_checklist_add(1, "").is_err());
    assert!(compose_checklist_add(1, "a\nb").is_err());
    assert!(
        compose_content(&Content::Poll(poll()), vec![image_ref()], None, None).is_err(),
        "attachments beside a poll"
    );
    // An article image that is not an image attachment.
    let enc = encrypt_attachment(b"pdf").unwrap();
    let pdf = AttachmentRef::from_encrypted(&enc, 3, Some("application/pdf".into()), None);
    assert!(compose_content(&Content::Article(article()), vec![pdf], None, None).is_err());
    assert!(compose_content(&Content::Article(article()), vec![], None, None).is_err());
    let mut bad = location();
    bad.accuracy_m = 0;
    assert!(compose_content(&Content::Location(bad), vec![], None, None).is_err());
}

fn json(v: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

fn unsupported() -> MessageBody {
    MessageBody::Unsupported {
        version: WIRE_VERSION,
    }
}

#[test]
fn strict_refusals() {
    let poll_v = serde_json::to_value(serde_json::json!({
        "body": B64.encode(poll().body.encode().unwrap()),
    }))
    .unwrap();
    let loc_v = serde_json::to_value(location()).unwrap();

    // Two content fields.
    let two = serde_json::json!({"v": 1, "poll": poll_v, "location": loc_v});
    assert_eq!(parse_body(&json(&two)), unsupported());

    // A broken payload with nothing else to show.
    let broken = serde_json::json!({"v": 1, "location": {"lat_e7": 1}});
    assert_eq!(parse_body(&json(&broken)), unsupported());

    // A broken payload beside text shows the text.
    let with_text = serde_json::json!({"v": 1, "text": "hi", "location": {"lat_e7": 1}});
    assert!(
        matches!(parse_body(&json(&with_text)), MessageBody::Chat { text, .. } if text == "hi")
    );

    // A valid payload beside text is refused as content and shows the text.
    let mixed = serde_json::json!({"v": 1, "text": "hi", "location": loc_v});
    assert!(matches!(parse_body(&json(&mixed)), MessageBody::Chat { text, .. } if text == "hi"));

    // An op that carries content.
    let op = serde_json::json!({"v": 1, "delete_of": 3, "poll": poll_v});
    assert_eq!(parse_body(&json(&op)), unsupported());

    // An edit of a poll.
    let edit_poll = serde_json::json!({"v": 1, "edit_of": 3, "poll": poll_v});
    assert_eq!(parse_body(&json(&edit_poll)), unsupported());

    // Unknown field inside the signed-body wrapper.
    let extra = serde_json::json!({"v": 1, "poll": {"body": B64.encode(poll().body.encode().unwrap()), "x": 1}});
    assert_eq!(parse_body(&json(&extra)), unsupported());

    // A signature of the wrong length.
    let short_sig = serde_json::json!({"v": 1, "poll": {"body": B64.encode(poll().body.encode().unwrap()), "author_sig": B64.encode([0u8; 63])}});
    assert_eq!(parse_body(&json(&short_sig)), unsupported());

    // Malformed ops.
    for bad in [
        serde_json::json!({"v": 1, "poll_vote_of": 3, "choices": [1, 0]}),
        serde_json::json!({"v": 1, "poll_vote_of": 3}),
        serde_json::json!({"v": 1, "poll_vote_of": "3", "choices": []}),
        serde_json::json!({"v": 1, "poll_vote_of": 3, "choices": [300]}),
        serde_json::json!({"v": 1, "check_of": 3, "item": -1, "done": true}),
        serde_json::json!({"v": 1, "check_of": 3, "item": 1}),
        serde_json::json!({"v": 1, "check_of": 3, "item": 1, "done": true, "item_added": 1}),
        serde_json::json!({"v": 1, "check_add_of": 3, "item_text": ""}),
        serde_json::json!({"v": 1, "poll_close_of": 3.5}),
    ] {
        assert_eq!(parse_body(&json(&bad)), unsupported(), "{bad}");
    }

    // Two ops keep the ADR-0023 degrade.
    let two_ops = serde_json::json!({"v": 1, "text": "t", "poll_close_of": 3, "delete_of": 4});
    assert!(matches!(parse_body(&json(&two_ops)), MessageBody::Chat { text, .. } if text == "t"));

    // A plain message is unchanged.
    assert!(
        matches!(parse_body(&compose("hello", vec![]).unwrap()), MessageBody::Chat { text, .. } if text == "hello")
    );
}

#[test]
fn audio_tags_are_kept_for_audio_only() {
    let enc = encrypt_attachment(b"mp3").unwrap();
    let audio = AttachmentRef::from_encrypted(&enc, 3, Some("audio/mpeg".into()), None)
        .with_kind(MEDIA_KIND_AUDIO)
        .with_duration_ms(MAX_AUDIO_DURATION_MS)
        .with_audio_tags(Some("Song".into()), Some("Band".into()));
    let (_, refs) = parse(&compose("", vec![audio.clone()]).unwrap());
    assert_eq!(refs[0].media_kind(), Some(MediaKind::Audio));
    assert_eq!(refs[0].title.as_deref(), Some("Song"));
    assert_eq!(refs[0].artist.as_deref(), Some("Band"));
    assert_eq!(refs[0].duration_ms, Some(MAX_AUDIO_DURATION_MS));

    let too_long = audio
        .clone()
        .with_duration_ms(MAX_AUDIO_DURATION_MS + 1)
        .with_audio_tags(Some("a\u{202E}b".into()), Some("x".repeat(256)));
    let (_, refs) = parse(&compose("", vec![too_long]).unwrap());
    assert_eq!(refs[0].duration_ms, None);
    assert_eq!(refs[0].title, None);
    assert_eq!(refs[0].artist, None);

    let not_audio = AttachmentRef::from_encrypted(&enc, 3, None, None)
        .with_audio_tags(Some("Song".into()), None);
    let (_, refs) = parse(&compose("", vec![not_audio]).unwrap());
    assert_eq!(refs[0].title, None);
}

fn fragment() -> impl Strategy<Value = (String, serde_json::Value)> {
    let poll_body = B64.encode(poll().body.encode().unwrap());
    let checklist_body = B64.encode(checklist().body.encode().unwrap());
    let article_b64 = B64.encode(article().encode().unwrap());
    let values = prop_oneof![
        any::<i64>().prop_map(serde_json::Value::from),
        any::<bool>().prop_map(serde_json::Value::from),
        ".{0,8}".prop_map(serde_json::Value::from),
        proptest::collection::vec(0u64..300, 0..4).prop_map(serde_json::Value::from),
        Just(serde_json::json!({"body": poll_body})),
        Just(serde_json::json!({"body": checklist_body})),
        Just(serde_json::Value::String(article_b64)),
        Just(serde_json::to_value(location()).unwrap()),
        Just(
            serde_json::json!({"name": "Ann", "username": "ann", "account_key": B64.encode([1u8; 32])})
        ),
        Just(serde_json::Value::Null),
    ];
    let keys = prop_oneof![
        Just("poll"),
        Just("checklist"),
        Just("location"),
        Just("contact_card"),
        Just("article"),
        Just("poll_vote_of"),
        Just("choices"),
        Just("poll_close_of"),
        Just("check_of"),
        Just("item"),
        Just("item_added"),
        Just("done"),
        Just("check_add_of"),
        Just("item_text"),
        Just("edit_of"),
        Just("text"),
        Just("reply_to"),
    ];
    (keys.prop_map(String::from), values)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn hostile_envelopes_never_panic_and_survivors_are_canonical(
        fields in proptest::collection::vec(fragment(), 0..5),
    ) {
        let mut obj = serde_json::Map::new();
        obj.insert("v".into(), 1.into());
        for (k, v) in fields {
            obj.insert(k, v);
        }
        let bytes = serde_json::to_vec(&serde_json::Value::Object(obj)).unwrap();
        let _ = parse_message_body(&bytes);
        prop_assert!(content_within_bounds(&bytes));
        prop_assert!(media_metadata_within_bounds(&bytes));
    }

    #[test]
    fn random_bytes_pass_every_oracle(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = parse_message_body(&bytes);
        prop_assert!(content_within_bounds(&bytes));
        prop_assert!(article_decode_round_trip(&bytes));
        prop_assert!(content_bundle_round_trip(&bytes));
    }
}

#[test]
fn every_content_kind_survives_the_bundle_payload() {
    use lakko_attachments::content_state::MAX_CONTENT_PAYLOAD_BYTES;
    let images = vec![image_ref()];
    for content in [
        Content::Poll(poll()),
        Content::Checklist(checklist()),
        Content::Location(location()),
        Content::ContactCard(card()),
        Content::Article(article()),
    ] {
        let payload = content.to_payload().unwrap();
        let back = Content::from_payload(content.kind_byte(), &payload, &images).unwrap();
        assert_eq!(back, content);
        // The bundle route and the envelope route read the same thing.
        let wire = compose_content(
            &content,
            if matches!(content, Content::Article(_)) {
                images.clone()
            } else {
                vec![]
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(parse_body(&wire).content(), Some(back));
    }
    // A wrong kind byte, bad JSON, a payload over the bound.
    let payload = Content::Location(location()).to_payload().unwrap();
    assert!(Content::from_payload(1, &payload, &[]).is_err());
    assert!(Content::from_payload(9, &payload, &[]).is_err());
    assert!(Content::from_payload(3, b"{", &[]).is_err());
    assert!(Content::from_payload(3, &vec![b' '; MAX_CONTENT_PAYLOAD_BYTES + 1], &[]).is_err());
    // An article's images must still be images on this route.
    let article_payload = Content::Article(article()).to_payload().unwrap();
    assert!(Content::from_payload(5, &article_payload, &[]).is_err());
}

#[test]
fn the_largest_article_fits_the_payload_bound() {
    use lakko_attachments::article::MAX_ARTICLE_BYTES;
    use lakko_attachments::content_state::MAX_CONTENT_PAYLOAD_BYTES;
    // A JSON string of base64 of the largest article, plus its two quotes.
    assert!(MAX_ARTICLE_BYTES.div_ceil(3) * 4 + 2 <= MAX_CONTENT_PAYLOAD_BYTES);
}

#[test]
fn state_snapshots_match_their_content() {
    use lakko_attachments::checklist::{self, ChecklistTickRecord};
    use lakko_attachments::content_state::{
        ChecklistSnapshot, ContentState, PollClosed, PollState,
    };
    use lakko_attachments::poll::{self, PollVoteRecord};

    let p = poll();
    let tally = poll::fold(
        &p.body,
        &[1; 32],
        1,
        &[PollVoteRecord {
            message_id: 2,
            created_at_ms: 2,
            voter: [2; 32],
            choices: vec![1],
        }],
        &[],
    );
    let state = ContentState::Poll(PollState::from_tally(&tally, 0));
    assert!(state.matches(&Content::Poll(p.clone())));
    assert!(!state.matches(&Content::Checklist(checklist())));
    let bytes = state.encode().unwrap();
    assert_eq!(ContentState::decode(&bytes).unwrap(), state);
    let short = ContentState::Poll(PollState {
        counts: vec![0, 0, 0],
        voters: 0,
        closed: PollClosed::Open,
    });
    assert!(
        !short.matches(&Content::Poll(p)),
        "three counts for a two-option poll"
    );

    let c = checklist();
    let folded = checklist::fold(
        &c.body,
        &[1; 32],
        1,
        &[],
        &[],
        &[ChecklistTickRecord {
            message_id: 2,
            ticker: [1; 32],
            privileged: false,
            item: ChecklistItemRef::Author(7),
            done: true,
        }],
        &[],
    );
    let state = ContentState::Checklist(ChecklistSnapshot::from_state(&folded));
    assert!(state.matches(&Content::Checklist(c)));
    let ContentState::Checklist(snap) = &state else {
        panic!("checklist");
    };
    assert_eq!(snap.done, vec![(ChecklistItemRef::Author(7), [1; 32])]);
    let dangling = ContentState::Checklist(ChecklistSnapshot {
        closed_by: None,
        added: vec![],
        done: vec![(ChecklistItemRef::Author(99), [1; 32])],
    });
    assert!(!dangling.matches(&Content::Checklist(checklist())));
}

#[test]
fn a_close_carries_a_channel_tally_and_drops_a_bad_one() {
    let tally = vec![7u8; 60];
    let wire = compose_poll_close(9, Some(&tally)).unwrap();
    assert_eq!(
        parse_body(&wire),
        MessageBody::PollClose {
            target: 9,
            tally: Some(tally),
        }
    );
    assert!(compose_poll_close(9, Some(&[])).is_err());
    assert!(compose_poll_close(9, Some(&[0; 119])).is_err());
    // Lenient on receipt: a tally that is not base64 or is too long is
    // dropped, the close stands.
    for bad in [
        r#"{"v":1,"text":"","poll_close_of":9,"tally":"not base64!"}"#.to_string(),
        format!(
            r#"{{"v":1,"text":"","poll_close_of":9,"tally":"{}"}}"#,
            B64.encode([0u8; 119])
        ),
        r#"{"v":1,"text":"","poll_close_of":9,"tally":5}"#.to_string(),
    ] {
        assert_eq!(
            parse_body(bad.as_bytes()),
            MessageBody::PollClose {
                target: 9,
                tally: None,
            }
        );
    }
}

proptest! {
    #[test]
    fn any_bounded_tally_round_trips_through_a_close(
        target in any::<i64>(),
        tally in proptest::collection::vec(any::<u8>(), 1..=118),
    ) {
        let wire = compose_poll_close(target, Some(&tally)).unwrap();
        prop_assert_eq!(
            parse_body(&wire),
            MessageBody::PollClose { target, tally: Some(tally) }
        );
        prop_assert!(content_within_bounds(&wire));
    }
}
