// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The fuzz harness's properties, driven by proptest so they run in every
//! `cargo test` and not only where cargo-fuzz is installed.
//!
//! Random bytes rarely get far into a record, so most cases start from a
//! valid encoding and damage it: one byte replaced, or the input cut short
//! and extended. Generated records round-trip as well.

use lakko_vault::keys::{ConversationRef, VaultRef, VaultRoot};
use lakko_vault::record::{
    ChunkEntry, ConversationHead, ConversationKind, DevicesHead, Direction, HistoryChunk,
    IndexEntry, Membership, PersonalPin, ReentryEpoch, RegistrarSeed, Role, SharedPin,
    VaultAttachment, VaultCall, VaultForward, VaultForwardOrigin, VaultIndex, VaultMessage,
    VaultPerson, VaultReaction, VaultReply, VaultThumb,
};
use lakko_vault::seal::{open_object, seal_object, ObjectKind};
use lakko_vault::{fuzz_api, proof, SecretBytes};
use proptest::prelude::*;

fn text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..max).prop_map(|chars| chars.into_iter().collect())
}

fn thumb() -> impl Strategy<Value = VaultThumb> {
    (
        any::<[u8; 32]>(),
        any::<[u8; 32]>(),
        any::<[u8; 12]>(),
        text(8),
        any::<u32>(),
        any::<Option<u16>>(),
        any::<Option<u16>>(),
    )
        .prop_map(
            |(blob_id, key, nonce, mime, size, width, height)| VaultThumb {
                blob_id,
                key,
                nonce,
                mime,
                size,
                width,
                height,
            },
        )
}

fn attachment() -> impl Strategy<Value = VaultAttachment> {
    (
        (any::<[u8; 32]>(), any::<[u8; 32]>(), any::<[u8; 12]>()),
        any::<u64>(),
        (text(8), text(8), text(4)),
        any::<Option<u32>>(),
        prop::collection::vec(any::<u8>(), 0..8),
        (any::<Option<u16>>(), any::<Option<u16>>()),
        prop::option::of(thumb()),
    )
        .prop_map(
            |(
                (blob_id, key, nonce),
                size,
                (mime, name, kind),
                duration_ms,
                waveform,
                (width, height),
                thumb,
            )| {
                VaultAttachment {
                    blob_id,
                    key,
                    nonce,
                    size,
                    mime,
                    name,
                    kind,
                    duration_ms,
                    waveform,
                    width,
                    height,
                    thumb,
                }
            },
        )
}

fn reply() -> impl Strategy<Value = VaultReply> {
    (any::<u64>(), text(6), text(12)).prop_map(|(target, author, excerpt)| VaultReply {
        target,
        author,
        excerpt,
    })
}

fn forward() -> impl Strategy<Value = VaultForward> {
    (
        text(6),
        prop::option::of((prop::collection::vec(any::<u8>(), 1..12), any::<u64>())),
    )
        .prop_map(|(author, origin)| VaultForward {
            author,
            origin: origin.map(|(group_id, message_id)| VaultForwardOrigin {
                group_id,
                message_id,
            }),
        })
}

fn reaction() -> impl Strategy<Value = VaultReaction> {
    (
        any::<[u8; 32]>(),
        prop::sample::select(vec!["+", "ok", "\u{1F44D}"]),
    )
        .prop_map(|(reactor, emoji)| VaultReaction {
            reactor,
            emoji: emoji.to_owned(),
        })
}

fn direction() -> impl Strategy<Value = Direction> {
    prop::sample::select(vec![
        Direction::Outgoing,
        Direction::Incoming,
        Direction::System,
    ])
}

fn membership() -> impl Strategy<Value = Membership> {
    prop::sample::select(vec![Membership::In, Membership::Left, Membership::Removed])
}

fn message() -> impl Strategy<Value = VaultMessage> {
    (
        (any::<u64>(), direction(), any::<Option<[u8; 32]>>()),
        (any::<i64>(), any::<u64>(), any::<bool>(), any::<bool>()),
        text(40),
        prop::collection::vec(attachment(), 0..3),
        (prop::option::of(reply()), prop::option::of(forward())),
        prop::collection::vec(reaction(), 0..4),
        prop::option::of((any::<u8>(), any::<bool>(), any::<u32>())),
        any::<Option<u32>>(),
    )
        .prop_map(
            |(
                (message_id, direction, author),
                (created_at_ms, last_op_id, edited, deleted),
                text,
                attachments,
                (reply, forward),
                reactions,
                call,
                unsupported_version,
            )| VaultMessage {
                message_id,
                direction,
                author,
                created_at_ms,
                last_op_id,
                edited,
                deleted,
                text,
                attachments,
                reply,
                forward,
                reactions,
                call: call.map(|(outcome, video, duration_secs)| VaultCall {
                    outcome,
                    video,
                    duration_secs,
                }),
                unsupported_version,
            },
        )
}

fn conversation() -> impl Strategy<Value = ConversationRef> {
    (1u8..=3, prop::collection::vec(any::<u8>(), 1..=64))
        .prop_filter_map("a reference within its bounds", |(kind, id)| {
            ConversationRef::from_wire(kind, &id).ok()
        })
}

fn reentry() -> impl Strategy<Value = Vec<ReentryEpoch>> {
    prop::collection::vec(
        (
            any::<u64>(),
            any::<[u8; 32]>(),
            prop::collection::vec(any::<u8>(), 1..40),
        )
            .prop_map(|(epoch, seed, group_info)| ReentryEpoch {
                epoch,
                ds_access_seed: SecretBytes::new(seed),
                group_info,
            }),
        0..=2,
    )
}

fn person() -> impl Strategy<Value = VaultPerson> {
    (
        any::<[u8; 32]>(),
        text(10),
        "[a-z0-9_]{0,32}",
        prop::sample::select(vec![Role::Member, Role::Admin, Role::Owner]),
    )
        .prop_map(|(account_key, display_name, username, role)| VaultPerson {
            account_key,
            display_name,
            username,
            role,
        })
}

fn head() -> impl Strategy<Value = ConversationHead> {
    (
        (
            conversation(),
            prop::sample::select(vec![
                ConversationKind::Unknown,
                ConversationKind::OneToOne,
                ConversationKind::Group,
            ]),
            text(20),
        ),
        prop::collection::vec(person(), 0..4),
        (membership(), any::<u64>()),
        prop::collection::vec(
            (any::<u64>(), any::<bool>(), any::<u64>()).prop_map(
                |(message_id, pinned, updated_at_ms)| SharedPin {
                    message_id,
                    pinned,
                    updated_at_ms,
                },
            ),
            0..4,
        ),
        prop::collection::vec(
            (any::<u64>(), any::<u64>()).prop_map(|(message_id, pinned_at_ms)| PersonalPin {
                message_id,
                pinned_at_ms,
            }),
            0..4,
        ),
        prop::collection::vec(
            (any::<u16>(), any::<u64>(), any::<u64>(), any::<u64>()).prop_map(
                |(count, first_id, last_id, version)| ChunkEntry {
                    count,
                    first_id,
                    last_id,
                    version,
                },
            ),
            0..4,
        ),
        prop::collection::vec(message(), 0..3),
        reentry(),
        prop::option::of((any::<u8>(), any::<[u8; 32]>())),
    )
        .prop_map(
            |(
                (conversation, kind, title),
                people,
                (membership, last_activity_ms),
                shared_pins,
                personal_pins,
                chunks,
                tail,
                reentry,
                registrar,
            )| ConversationHead {
                conversation,
                kind,
                title,
                people,
                membership,
                shared_pins,
                personal_pins,
                last_activity_ms,
                chunks,
                tail,
                reentry,
                registrar_seed: registrar.map(|(rank, seed)| RegistrarSeed {
                    rank,
                    seed: SecretBytes::new(seed),
                }),
            },
        )
}

fn index() -> impl Strategy<Value = VaultIndex> {
    prop::collection::btree_map(conversation(), (any::<u64>(), membership()), 0..6).prop_map(
        |entries| VaultIndex {
            entries: entries
                .into_iter()
                .map(
                    |(conversation, (last_activity_ms, membership))| IndexEntry {
                        conversation,
                        last_activity_ms,
                        membership,
                    },
                )
                .collect(),
        },
    )
}

/// One byte replaced, or the input cut short and extended.
fn damaged(mut data: Vec<u8>, pos: usize, byte: u8, cut: Option<usize>, tail: &[u8]) -> Vec<u8> {
    if !data.is_empty() {
        let i = pos % data.len();
        data[i] = byte;
    }
    if let Some(cut) = cut {
        data.truncate(cut % (data.len() + 1));
        data.extend_from_slice(tail);
    }
    data
}

fn damage() -> impl Strategy<Value = (usize, u8, Option<usize>, Vec<u8>)> {
    (
        any::<usize>(),
        any::<u8>(),
        any::<Option<usize>>(),
        prop::collection::vec(any::<u8>(), 0..6),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    #[test]
    fn random_bytes(data in prop::collection::vec(any::<u8>(), 0..512)) {
        fuzz_api::head_decode(&data);
        fuzz_api::chunk_decode(&data);
        fuzz_api::index_decode(&data);
        fuzz_api::object_open(&data);
        fuzz_api::proof_parse(&data);
    }

    #[test]
    fn a_chunk_round_trips_and_survives_damage(
        messages in prop::collection::vec(message(), 0..4),
        (pos, byte, cut, tail) in damage(),
    ) {
        let chunk = HistoryChunk { messages };
        let bytes = chunk.encode().unwrap();
        prop_assert_eq!(&HistoryChunk::decode(&bytes).unwrap(), &chunk);
        fuzz_api::chunk_decode(&bytes);
        fuzz_api::chunk_decode(&damaged(bytes, pos, byte, cut, &tail));
    }

    #[test]
    fn a_head_round_trips_and_survives_damage(
        head in head(),
        (pos, byte, cut, tail) in damage(),
    ) {
        let bytes = head.encode().unwrap();
        prop_assert_eq!(&ConversationHead::decode(&bytes).unwrap(), &head);
        fuzz_api::head_decode(&bytes);
        fuzz_api::head_decode(&damaged(bytes, pos, byte, cut, &tail));
    }

    #[test]
    fn a_devices_head_round_trips_and_survives_damage(
        reentry in reentry(),
        (pos, byte, cut, tail) in damage(),
    ) {
        let head = DevicesHead { reentry };
        let bytes = head.encode().unwrap();
        prop_assert_eq!(&DevicesHead::decode(&bytes).unwrap(), &head);
        fuzz_api::head_decode(&bytes);
        fuzz_api::head_decode(&damaged(bytes, pos, byte, cut, &tail));
    }

    #[test]
    fn an_index_round_trips_and_survives_damage(
        index in index(),
        (pos, byte, cut, tail) in damage(),
    ) {
        let bytes = index.encode().unwrap();
        prop_assert_eq!(&VaultIndex::decode(&bytes).unwrap(), &index);
        fuzz_api::index_decode(&bytes);
        fuzz_api::index_decode(&damaged(bytes, pos, byte, cut, &tail));
    }

    #[test]
    fn a_proof_round_trips_and_survives_damage(
        seed in any::<[u8; 32]>(),
        now in any::<u64>(),
        body in prop::collection::vec(any::<u8>(), 0..64),
        (pos, byte, cut, tail) in damage(),
    ) {
        let header = proof::sign(&seed, "PUT", "/vault/ab/head", &body, now).unwrap();
        prop_assert!(header.len() <= proof::MAX_HEADER_LEN);
        prop_assert_eq!(
            proof::verify(&proof::public_key(&seed), "PUT", "/vault/ab/head", &body, &header, now),
            Ok(())
        );
        fuzz_api::proof_parse(header.as_bytes());
        fuzz_api::proof_parse(&damaged(header.into_bytes(), pos, byte, cut, &tail));
    }
}

proptest! {
    // Each case seals and opens up to a few hundred kilobytes.
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn an_object_round_trips_and_survives_damage(
        account in any::<[u8; 32]>(),
        conversation in conversation(),
        plaintext in prop::collection::vec(any::<u8>(), 0..3000),
        long in any::<bool>(),
        (chunk_no, version) in (any::<u32>(), any::<u64>()),
        nonce in any::<[u8; 12]>(),
        (pos, byte, cut, tail) in damage(),
    ) {
        let root = VaultRoot::from_account_seed(&account);
        let vault = VaultRef::Conversation(conversation);
        let key = root.vault_key(&vault);
        let id = root.vault_id(&vault);
        // Reach past the first buckets now and then.
        let plaintext = if long { plaintext.repeat(40) } else { plaintext };

        let sealed = seal_object(&key, &id, ObjectKind::Chunk, chunk_no, version, &nonce, &plaintext)
            .unwrap();
        prop_assert_eq!(
            open_object(&key, &id, ObjectKind::Chunk, chunk_no, version, &sealed),
            Ok(plaintext)
        );
        // Any other address fails.
        prop_assert!(
            open_object(&key, &id, ObjectKind::Chunk, chunk_no, version.wrapping_add(1), &sealed)
                .is_err()
        );
        prop_assert!(
            open_object(&key, &id, ObjectKind::Chunk, chunk_no.wrapping_add(1), version, &sealed)
                .is_err()
        );
        // Any damage fails, and nothing panics.
        let broken = damaged(sealed.clone(), pos, byte, cut, &tail);
        if broken != sealed {
            prop_assert!(
                open_object(&key, &id, ObjectKind::Chunk, chunk_no, version, &broken).is_err()
            );
        }
        fuzz_api::object_open(&broken);
    }
}
