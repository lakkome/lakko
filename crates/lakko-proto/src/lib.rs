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
}

/// MLS application message payloads (`schemas/mls-app/v1.proto`).
pub mod mls_app {
    /// Version 1 of the MLS application payload schema.
    #[allow(clippy::pedantic, missing_docs)]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/lakko.mls_app.v1.rs"));
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
}
