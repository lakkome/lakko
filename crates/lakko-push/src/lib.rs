// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Push wake-up payload framing for Lakko.
//!
//! ## What ships in a push notification
//!
//! Strictly metadata pointers — no plaintext, no preview, no sender.
//! Apple, Google and Mozilla see only:
//!
//!   * a 32-byte pseudonym (random; not linkable to a real identity)
//!   * an integer journal cursor
//!   * a payload kind tag
//!
//! On receipt the client wakes up, GETs the matching cursor from
//! `services/ds`, decrypts via MLS, and renders the OS-level
//! notification on-device. None of the user-visible string
//! ("Alice: hi") ever crosses the device boundary in clear.
//!
//! ## Wire format (v1)
//!
//! Compact JSON, since both APNs and FCM speak JSON natively and
//! the bytes-per-push budget (4 KiB) is far above what we need.
//! Three discriminated kinds:
//!
//! ```json
//! {"v":1,"k":"g","gp":"<base64 32B>","ep":1234}     // GroupMessage
//! {"v":1,"k":"m","ep":5678}                          // MailboxMessage
//! {"v":1,"k":"w"}                                    // Wakeup
//! ```
//!
//!   * `v` — schema version (currently 1; bumping is a hard fork —
//!     legacy clients receive v2 → drop the push silently and rely
//!     on the next long-poll round to catch up).
//!   * `k` — kind tag (1 char to keep payload tight): `g` group,
//!     `m` mailbox, `w` generic wake.
//!   * `gp` — group pseudonym, standard base64 (no URL-safe variant
//!     because APNs / FCM both pass JSON values verbatim and the `+`
//!     and `/` are JSON-safe).
//!   * `ep` — envelope pointer; the `id` returned by `services/ds`
//!     when the message was published. Client uses it as
//!     `?after=<ep - 1>` to fetch exactly the message that triggered
//!     the wake.
//!
//! `MailboxMessage` deliberately omits a device pseudonym — the
//! client already knows its own. Including it would just inflate
//! every push by 44 bytes.
//!
//! ## What's NOT here
//!
//! * **Server-side payload encryption** (Apple/Google never see
//!   pseudonym/pointer either). Apple+Google already see only
//!   uniformly-distributed random pseudonyms with no identity
//!   linkage; an extra encryption layer is marginal hardening, not
//!   a Phase-1 requirement. Phase 2 hardening or dedicated sub-
//!   sprint.
//! * **Web Push RFC 8291 framing** — Web Push natively encrypts
//!   to the subscription's auth/p256dh keys; that lives in
//!   `services/push-gw` when the real `WebPushProvider` lands in
//!   1.6b.
//! * **APNs / FCM client-side wire decoding** — that's per-platform
//!   FFI work in 1.14 (iOS) and 1.15 (Android).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Pseudonym length: 32 raw bytes. Same as everywhere else in the
/// codebase (`services/ds`, `services/push-gw`, `lakko-contacts`).
pub const PSEUDONYM_LEN: usize = 32;

/// Schema version embedded in every payload.
const SCHEMA_VERSION: u8 = 1;

/// Errors surfaced by [`PushPayload::decode_json`].
#[derive(Error, Debug)]
pub enum PushError {
    /// Underlying JSON parser rejected the input.
    #[error("invalid push payload JSON: {0}")]
    InvalidJson(String),
    /// Schema version field is missing or not the expected value.
    #[error("unsupported push schema version (got {got}, want {want})")]
    UnsupportedVersion {
        /// Schema version found in the payload.
        got: u8,
        /// Schema version this build understands.
        want: u8,
    },
    /// Kind tag is missing, unknown, or doesn't match its required
    /// fields.
    #[error("invalid push kind: {0}")]
    InvalidKind(String),
    /// A pseudonym field is malformed (bad base64 / wrong length).
    #[error("invalid pseudonym: {0}")]
    InvalidPseudonym(String),
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, PushError>;

/// Discriminated payload variants. The kind tells the client which
/// `services/ds` queue to poll on wake-up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PushPayload {
    /// A new message has landed in a group's fan-out journal. The
    /// client polls `GET /ds/group/{group_pseudonym}/messages?after=`
    /// `envelope_pointer - 1` to fetch exactly the new row.
    GroupMessage {
        /// 32-byte group pseudonym matching what's stored in
        /// `services/ds.ds_group_messages`.
        group_pseudonym: [u8; PSEUDONYM_LEN],
        /// Server-assigned `id` from `services/ds`. Use as
        /// `?after = envelope_pointer - 1`.
        envelope_pointer: u64,
    },
    /// A new direct row has landed in this device's mailbox
    /// (typically an MLS Welcome). The client polls
    /// `GET /ds/mailbox/{my_device_pseudonym}/messages?after=` —
    /// no need to ship the device pseudonym in the push since the
    /// client already knows its own.
    MailboxMessage {
        /// Server-assigned `id` from `services/ds`.
        envelope_pointer: u64,
    },
    /// Generic wake-up. Client doesn't know which queue advanced
    /// and polls every queue it subscribes to. Used as a fallback
    /// when the publisher could not (or did not want to) name the
    /// specific cursor.
    Wakeup,
}

impl PushPayload {
    /// Encode as compact JSON ready for an APNs / FCM `data` payload.
    /// Never fails — every variant has a finite, valid JSON shape.
    #[must_use]
    pub fn encode_json(&self) -> Vec<u8> {
        let wire: WireV1 = self.into();
        // `to_vec` cannot fail for our owned types; if it ever did
        // it would mean serde_json itself is broken. Fall back to
        // an empty vec rather than panicking — the receiver will
        // surface `InvalidJson` cleanly.
        serde_json::to_vec(&wire).unwrap_or_default()
    }

    /// Decode a JSON payload received from the OS push pipeline.
    ///
    /// # Errors
    ///
    /// See [`PushError`] for the failure modes (malformed JSON,
    /// unsupported version, bad kind, bad pseudonym).
    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let wire: WireV1 =
            serde_json::from_slice(bytes).map_err(|e| PushError::InvalidJson(e.to_string()))?;
        Self::try_from(wire)
    }
}

// ============================================================================
// Wire shape — separated from the rich enum to keep serde out of the
// public API surface (so future format changes don't break callers).
// ============================================================================

#[derive(Serialize, Deserialize)]
struct WireV1 {
    v: u8,
    k: String,
    /// Group pseudonym, base64-encoded. Present only when `k = "g"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gp: Option<String>,
    /// Envelope pointer. Present for `g` and `m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ep: Option<u64>,
}

impl From<&PushPayload> for WireV1 {
    fn from(p: &PushPayload) -> Self {
        match p {
            PushPayload::GroupMessage {
                group_pseudonym,
                envelope_pointer,
            } => Self {
                v: SCHEMA_VERSION,
                k: "g".to_string(),
                gp: Some(B64.encode(group_pseudonym)),
                ep: Some(*envelope_pointer),
            },
            PushPayload::MailboxMessage { envelope_pointer } => Self {
                v: SCHEMA_VERSION,
                k: "m".to_string(),
                gp: None,
                ep: Some(*envelope_pointer),
            },
            PushPayload::Wakeup => Self {
                v: SCHEMA_VERSION,
                k: "w".to_string(),
                gp: None,
                ep: None,
            },
        }
    }
}

impl TryFrom<WireV1> for PushPayload {
    type Error = PushError;

    fn try_from(wire: WireV1) -> Result<Self> {
        if wire.v != SCHEMA_VERSION {
            return Err(PushError::UnsupportedVersion {
                got: wire.v,
                want: SCHEMA_VERSION,
            });
        }
        match wire.k.as_str() {
            "g" => {
                let gp_b64 = wire
                    .gp
                    .ok_or_else(|| PushError::InvalidKind("k=g requires gp field".to_string()))?;
                let ep = wire
                    .ep
                    .ok_or_else(|| PushError::InvalidKind("k=g requires ep field".to_string()))?;
                let group_pseudonym = decode_pseudonym_b64(&gp_b64)?;
                Ok(PushPayload::GroupMessage {
                    group_pseudonym,
                    envelope_pointer: ep,
                })
            },
            "m" => {
                let ep = wire
                    .ep
                    .ok_or_else(|| PushError::InvalidKind("k=m requires ep field".to_string()))?;
                if wire.gp.is_some() {
                    return Err(PushError::InvalidKind(
                        "k=m must not carry gp field".to_string(),
                    ));
                }
                Ok(PushPayload::MailboxMessage {
                    envelope_pointer: ep,
                })
            },
            "w" => {
                if wire.gp.is_some() || wire.ep.is_some() {
                    return Err(PushError::InvalidKind(
                        "k=w must not carry gp or ep fields".to_string(),
                    ));
                }
                Ok(PushPayload::Wakeup)
            },
            other => Err(PushError::InvalidKind(format!("unknown kind {other:?}"))),
        }
    }
}

fn decode_pseudonym_b64(b64: &str) -> Result<[u8; PSEUDONYM_LEN]> {
    let bytes = B64
        .decode(b64)
        .map_err(|e| PushError::InvalidPseudonym(format!("base64: {e}")))?;
    if bytes.len() != PSEUDONYM_LEN {
        return Err(PushError::InvalidPseudonym(format!(
            "expected {PSEUDONYM_LEN} bytes, got {}",
            bytes.len(),
        )));
    }
    let mut out = [0u8; PSEUDONYM_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Fuzz-only entry point. Marked `#[doc(hidden)]` — the sole harness
/// surface for `crates/lakko-push/fuzz/`, **not** part of the supported
/// public API.
#[doc(hidden)]
pub mod fuzz_api {
    use super::PushPayload;

    /// Drive [`PushPayload::decode_json`] on attacker-controlled bytes.
    ///
    /// A push payload is relayed verbatim by the OS push pipeline (APNs /
    /// FCM / UnifiedPush) and reaches this JSON parser on the device
    /// before any further processing, so the input is fully untrusted: it
    /// exercises `serde_json::from_slice` → the schema-version gate → the
    /// kind dispatch → the base64 pseudonym decode + 32-byte length check.
    /// Any panic / overflow / slice-out-of-bounds here is a vulnerability.
    #[must_use]
    pub fn decode_push_payload(bytes: &[u8]) -> bool {
        PushPayload::decode_json(bytes).is_ok()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn group_payload() -> PushPayload {
        PushPayload::GroupMessage {
            group_pseudonym: [0xAB; PSEUDONYM_LEN],
            envelope_pointer: 12345,
        }
    }

    #[test]
    fn group_message_roundtrip() {
        let original = group_payload();
        let bytes = original.encode_json();
        let decoded = PushPayload::decode_json(&bytes).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn mailbox_message_roundtrip() {
        let original = PushPayload::MailboxMessage {
            envelope_pointer: 42,
        };
        let bytes = original.encode_json();
        let decoded = PushPayload::decode_json(&bytes).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn wakeup_roundtrip() {
        let bytes = PushPayload::Wakeup.encode_json();
        let decoded = PushPayload::decode_json(&bytes).unwrap();
        assert_eq!(decoded, PushPayload::Wakeup);
    }

    #[test]
    fn group_payload_fits_in_apns_budget() {
        // APNs background-push silent payload is capped at 4 KiB.
        // A group payload with maximum-length pseudonym base64 +
        // u64::MAX pointer must fit comfortably.
        let p = PushPayload::GroupMessage {
            group_pseudonym: [0xFF; PSEUDONYM_LEN],
            envelope_pointer: u64::MAX,
        };
        let bytes = p.encode_json();
        assert!(
            bytes.len() <= 256,
            "group payload should be tight, got {} bytes",
            bytes.len(),
        );
    }

    #[test]
    fn unknown_version_rejected() {
        let json = br#"{"v":99,"k":"w"}"#;
        let err = PushPayload::decode_json(json).unwrap_err();
        assert!(matches!(
            err,
            PushError::UnsupportedVersion { got: 99, want: 1 },
        ));
    }

    #[test]
    fn unknown_kind_rejected() {
        let json = br#"{"v":1,"k":"q"}"#;
        let err = PushPayload::decode_json(json).unwrap_err();
        assert!(matches!(err, PushError::InvalidKind(_)));
    }

    #[test]
    fn group_kind_without_gp_rejected() {
        let json = br#"{"v":1,"k":"g","ep":1}"#;
        let err = PushPayload::decode_json(json).unwrap_err();
        assert!(matches!(err, PushError::InvalidKind(_)));
    }

    #[test]
    fn group_kind_without_ep_rejected() {
        // Use a valid base64 32-byte gp so the pseudonym decoder
        // doesn't fire first; we want to assert the ep check.
        let gp = B64.encode([0u8; PSEUDONYM_LEN]);
        let json = format!(r#"{{"v":1,"k":"g","gp":"{gp}"}}"#);
        let err = PushPayload::decode_json(json.as_bytes()).unwrap_err();
        assert!(matches!(err, PushError::InvalidKind(_)));
    }

    #[test]
    fn mailbox_kind_with_gp_rejected() {
        let gp = B64.encode([0u8; PSEUDONYM_LEN]);
        let json = format!(r#"{{"v":1,"k":"m","gp":"{gp}","ep":1}}"#);
        let err = PushPayload::decode_json(json.as_bytes()).unwrap_err();
        assert!(matches!(err, PushError::InvalidKind(_)));
    }

    #[test]
    fn wakeup_with_extra_fields_rejected() {
        let json = br#"{"v":1,"k":"w","ep":1}"#;
        let err = PushPayload::decode_json(json).unwrap_err();
        assert!(matches!(err, PushError::InvalidKind(_)));
    }

    #[test]
    fn malformed_pseudonym_rejected() {
        // gp is valid base64 but only 16 bytes after decode.
        let short = B64.encode([0u8; 16]);
        let json = format!(r#"{{"v":1,"k":"g","gp":"{short}","ep":1}}"#);
        let err = PushPayload::decode_json(json.as_bytes()).unwrap_err();
        assert!(matches!(err, PushError::InvalidPseudonym(_)));
    }

    #[test]
    fn malformed_json_rejected() {
        let err = PushPayload::decode_json(b"not json").unwrap_err();
        assert!(matches!(err, PushError::InvalidJson(_)));
    }

    #[test]
    fn extra_unknown_fields_are_tolerated() {
        // A future v1.x publisher might add a hint field; we should
        // still decode the known parts cleanly. (`serde` ignores
        // unknown JSON fields by default.)
        let gp = B64.encode([0u8; PSEUDONYM_LEN]);
        let json = format!(r#"{{"v":1,"k":"g","gp":"{gp}","ep":7,"hint":"future"}}"#);
        let decoded = PushPayload::decode_json(json.as_bytes()).unwrap();
        assert_eq!(
            decoded,
            PushPayload::GroupMessage {
                group_pseudonym: [0u8; PSEUDONYM_LEN],
                envelope_pointer: 7,
            },
        );
    }
}
