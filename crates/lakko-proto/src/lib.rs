// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Generated protobuf types for the Lakko wire format.
//!
//! All types in this crate are generated from `schemas/*.proto` by `build.rs`
//! at compile time. Hand-written code is limited to thin convenience helpers
//! between domain types and proto types (added as needed).
//!
//! The build uses [`protox`](https://crates.io/crates/protox) (pure-Rust)
//! so consumers do not need `protoc` on their PATH.

#![forbid(unsafe_code)]

/// Outer transport envelope (`schemas/wire/v1.proto`).
pub mod wire {
    /// Version 1 of the wire envelope.
    #[allow(clippy::pedantic, missing_docs)]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/lakko.wire.v1.rs"));
    }

    /// Device-pairing QR payload
    /// (`schemas/wire/device_pairing_v1.proto`) — Phase 2a3b1.
    pub mod device_pairing {
        /// The device-pairing payload package (payload `version` 2 since
        /// ADR-0056, which replaced the account id with the login handle).
        #[allow(clippy::pedantic, missing_docs)]
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/lakko.wire.device_pairing.v1.rs"));
        }
    }
}

/// MLS application message payloads (`schemas/mls-app/v1.proto`).
pub mod mls_app {
    /// Version 1 of the MLS application payload schema.
    #[allow(clippy::pedantic, missing_docs)]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/lakko.mls_app.v1.rs"));
    }

    /// Cross-device application payloads carried on a user's
    /// DeviceGroup (`schemas/mls-app/devicegroup_v1.proto`) —
    /// Phase 2a3e. Read cursors, draft updates, per-thread
    /// flags, and explicit conversation ordering.
    pub mod devicegroup {
        /// Version 1 of the DeviceGroup payload schema.
        #[allow(clippy::pedantic, missing_docs)]
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/lakko.mls_app.devicegroup.v1.rs"));
        }
    }

    /// History catch-up bundle
    /// (`schemas/mls-app/history_bundle_v1.proto`) — Phase 2a3d8.
    /// HPKE-style sealed payload the scanner-side device hands to
    /// the joining device at add-time so the latter can backfill
    /// the application-level message log it would otherwise be
    /// unable to recover from the `whole_storage_snapshot` alone.
    pub mod history_bundle {
        /// Version 1 of the history-bundle schema.
        #[allow(clippy::pedantic, missing_docs)]
        pub mod v1 {
            include!(concat!(
                env!("OUT_DIR"),
                "/lakko.mls_app.history_bundle.v1.rs"
            ));
        }
    }

    /// Call signaling payloads
    /// (`schemas/mls-app/call_signaling_v1.proto`) — Phase 3a3.
    /// Offer / answer / trickled ICE candidates / end. Ride
    /// the existing conversation MLS group as application
    /// messages, reusing the Phase 1 DS journal — no new
    /// transport.
    pub mod call_signaling {
        /// Version 1 of the call-signaling schema.
        #[allow(clippy::pedantic, missing_docs)]
        pub mod v1 {
            include!(concat!(
                env!("OUT_DIR"),
                "/lakko.mls_app.call_signaling.v1.rs"
            ));
        }
    }
}

/// Call media metadata (`schemas/media/v1.proto`).
pub mod media {
    /// Version 1 of the call media metadata schema.
    #[allow(clippy::pedantic, missing_docs)]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/lakko.media.v1.rs"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test — verifies generated types compile and round-trip via prost.
    #[test]
    fn envelope_roundtrip() {
        use prost::Message as _;

        let original = wire::v1::Envelope {
            version: 1,
            routing_pseudonym: vec![0xab; 32],
            ciphertext: vec![0xcd; 128],
        };

        let mut buf = Vec::new();
        original.encode(&mut buf).expect("encode");

        let decoded = wire::v1::Envelope::decode(&*buf).expect("decode");
        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.routing_pseudonym, vec![0xab; 32]);
        assert_eq!(decoded.ciphertext, vec![0xcd; 128]);
    }

    /// Smoke test for the device-pairing payload (v2, ADR-0056).
    #[test]
    fn pairing_qr_roundtrip() {
        use prost::Message as _;

        let original = wire::device_pairing::v1::PairingQr {
            version: 2,
            key_package: vec![0xfe; 768],
            rendezvous_pseudonym: vec![0xab; 32],
            login_handle: vec![0x42; 32],
        };

        let mut buf = Vec::new();
        original.encode(&mut buf).expect("encode");

        let decoded = wire::device_pairing::v1::PairingQr::decode(&*buf).expect("decode");
        assert_eq!(decoded.version, 2);
        assert_eq!(decoded.login_handle, vec![0x42; 32]);
        assert_eq!(decoded.key_package, vec![0xfe; 768]);
        assert_eq!(decoded.rendezvous_pseudonym, vec![0xab; 32]);
    }

    /// Phase 2a3e smoke tests — DeviceGroup cross-device
    /// payloads round-trip through prost for each oneof branch.
    #[test]
    fn devicegroup_read_cursor_roundtrip() {
        use mls_app::devicegroup::v1::{
            device_group_payload::Body, DeviceGroupPayload, ReadCursor,
        };
        use prost::Message as _;

        let original = DeviceGroupPayload {
            hlc: 1_700_000_000_000,
            sender_leaf: 3,
            body: Some(Body::ReadCursor(ReadCursor {
                group_id: vec![0xAB; 32],
                up_to_message_id: vec![0xCD; 8],
                read_at_ms: 1_700_000_001_000,
            })),
        };
        let mut buf = Vec::new();
        original.encode(&mut buf).expect("encode");
        let decoded = DeviceGroupPayload::decode(&*buf).expect("decode");
        assert_eq!(decoded, original);
    }

    /// Phase 2a3d8 smoke test — `HistoryBundle` + `SealedBundle`
    /// round-trip through prost.
    #[test]
    fn history_bundle_roundtrip() {
        use mls_app::history_bundle::v1::{
            HistoryBundle, HistoryGroup, HistoryMessage, SealedBundle,
        };
        use prost::Message as _;

        let inner = HistoryBundle {
            max_hlc: 1_700_000_001_234,
            groups: vec![
                HistoryGroup {
                    group_id: vec![0xAA; 32],
                    count_capped: false,
                    messages: vec![
                        HistoryMessage {
                            payload_bytes: vec![0x01, 0x02, 0x03],
                            sender_leaf: 0,
                            message_id: vec![0x10; 8],
                            received_at_ms: 1_700_000_000_000,
                        },
                        HistoryMessage {
                            payload_bytes: vec![0x04, 0x05, 0x06],
                            sender_leaf: 1,
                            message_id: vec![0x11; 8],
                            received_at_ms: 1_700_000_001_000,
                        },
                    ],
                },
                HistoryGroup {
                    group_id: vec![0xBB; 32],
                    count_capped: true,
                    messages: vec![HistoryMessage {
                        payload_bytes: vec![0x07],
                        sender_leaf: 2,
                        message_id: vec![0x12; 8],
                        received_at_ms: 1_700_000_001_234,
                    }],
                },
            ],
        };
        let mut buf = Vec::new();
        inner.encode(&mut buf).expect("encode bundle");
        let decoded = HistoryBundle::decode(&*buf).expect("decode bundle");
        assert_eq!(decoded, inner);

        let sealed = SealedBundle {
            algorithm: 1,
            nonce: vec![0xCD; 12],
            ciphertext: buf.clone(),
        };
        let mut sbuf = Vec::new();
        sealed.encode(&mut sbuf).expect("encode sealed");
        let sdecoded = SealedBundle::decode(&*sbuf).expect("decode sealed");
        assert_eq!(sdecoded, sealed);
    }

    /// Phase 3a3 smoke test — `CallSignalingPayload`
    /// round-trips through prost for each oneof branch.
    #[test]
    fn call_signaling_roundtrip() {
        use mls_app::call_signaling::v1::{
            call_signaling_payload::Body, CallAnswer, CallEnd, CallOffer, CallSignalingPayload,
            GroupCallJoin, GroupCallLeave, GroupCallStart, IceCandidate,
        };
        use prost::Message as _;

        for body in [
            Body::Offer(CallOffer {
                call_id: vec![0x10; 16],
                sdp: "v=0\no=- 1 2 IN IP4 0.0.0.0\nm=audio 9 UDP/TLS/RTP/SAVPF 111\n".into(),
                created_at_ms: 1_700_000_000_000,
                media_flags: 0b11,
            }),
            Body::Answer(CallAnswer {
                call_id: vec![0x10; 16],
                sdp: "v=0\no=- 3 4 IN IP4 0.0.0.0\nm=audio 9 UDP/TLS/RTP/SAVPF 111\n".into(),
            }),
            Body::Candidate(IceCandidate {
                call_id: vec![0x10; 16],
                candidate: "candidate:1 1 udp 1677729535 192.168.0.1 56789 typ srflx".into(),
                sdp_mid: "audio".into(),
                sdp_m_line_index: 0,
            }),
            Body::End(CallEnd {
                call_id: vec![0x10; 16],
                reason: 0,
            }),
            Body::GroupStart(GroupCallStart {
                call_id: vec![0x20; 16],
                initiator_leaf: 7,
                created_at_ms: 1_700_000_000_500,
                media_flags: 0b11,
            }),
            Body::GroupJoin(GroupCallJoin {
                call_id: vec![0x20; 16],
                leaf_index: 12,
                joined_at_ms: 1_700_000_001_000,
            }),
            Body::GroupLeave(GroupCallLeave {
                call_id: vec![0x20; 16],
                leaf_index: 12,
                reason: 3,
            }),
        ] {
            let original = CallSignalingPayload { body: Some(body) };
            let mut buf = Vec::new();
            original.encode(&mut buf).expect("encode");
            let decoded = CallSignalingPayload::decode(&*buf).expect("decode");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn devicegroup_draft_and_flags_and_order_roundtrip() {
        use mls_app::devicegroup::v1::{
            device_group_payload::Body, ConversationOrder, DeviceGroupPayload, DraftUpdate,
            ThreadFlags,
        };
        use prost::Message as _;

        for body in [
            Body::Draft(DraftUpdate {
                group_id: vec![0x01; 32],
                text: "hello, draft 👋".into(),
                reply_to_id: vec![0x99; 8],
            }),
            Body::Flags(ThreadFlags {
                group_id: vec![0x02; 32],
                pinned: true,
                muted: false,
                archived: false,
                unread_override: true,
            }),
            Body::Order(ConversationOrder {
                group_ids: vec![vec![0xAA; 32], vec![0xBB; 32], vec![0xCC; 32]],
            }),
        ] {
            let original = DeviceGroupPayload {
                hlc: 42,
                sender_leaf: 0,
                body: Some(body),
            };
            let mut buf = Vec::new();
            original.encode(&mut buf).expect("encode");
            let decoded = DeviceGroupPayload::decode(&*buf).expect("decode");
            assert_eq!(decoded, original);
        }
    }
}
