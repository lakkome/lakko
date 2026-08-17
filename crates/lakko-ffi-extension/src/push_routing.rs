// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` push-routing surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0042 / ADR-0044 envelope).
//!
//! ## Why the NSE needs this at all
//!
//! Service envelopes ride the SAME group journal as chat, as ordinary MLS
//! application messages. The foreground receive loop filters them out before
//! the chat parser (`ChatClient.toEvent`); the background NSE decrypt did not,
//! and [`crate::attachments::parse_body_envelope`] has a deliberate legacy
//! fallback that degrades non-JSON bytes to `Chat` via `from_utf8_lossy`
//! (pre-1.19 wire compatibility). A push-routing-secret envelope therefore
//! surfaced as a notification body containing its domain string plus raw
//! binary. The NSE needs the CODEC — not a hardcoded domain prefix in Swift,
//! which would silently drift on the next envelope version bump (exactly the
//! failure class ADR-0044 documents).
//!
//! ## Why adding `lakko-contacts` does not re-inflate the slim crate
//!
//! `lakko-contacts` depends only on `thiserror` (already present) and
//! `lakko-crypto` (already present transitively via `lakko-mls`), so a
//! `cargo tree` set difference against the `aarch64-apple-ios` target adds
//! exactly five crates: `lakko-contacts`, `tracing`, `tracing-core`,
//! `tracing-attributes`, `pin-project-lite`. No tokio, no TLS, no async
//! runtime, no transport — the properties that keep this crate inside the NSE's
//! ~30 MB budget are unchanged.
//!
//! ## Scope: DECODE only
//!
//! Mirror of the single read entry point the NSE needs from
//! `lakko-ffi-uniffi`'s `contacts` module — [`decode_push_routing_secret`] and
//! the [`FfiPushRoutingSecret`] record it returns. `compose_push_routing_secret`
//! / `derive_push_gid` / `push_routing_secret_is_newer` are DELIBERATELY absent:
//! the NSE only asks "is this a service envelope I must not render?", it never
//! mints, re-broadcasts, derives a routing id, or persists a secret. Keeping the
//! surface to the classification question is the same discipline the rest of
//! this crate follows — see the crate docs' read-only-throwaway note.

use lakko_contacts::push_routing::decode_push_routing_secret as rust_decode_push_routing_secret;

use crate::FfiError;

/// A decoded push-routing-secret distribution envelope (ADR-0042 v2, per
/// ADR-0044). Output of [`decode_push_routing_secret`].
///
/// Shape-identical to `lakko-ffi-uniffi`'s `FfiPushRoutingSecret` so the two
/// frameworks present the same Swift type shape. The NSE consumes only the
/// SUCCESS/FAILURE of the decode (a successful decode ⇒ "service envelope,
/// render nothing"); the fields are carried for parity and are never persisted
/// on this path.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiPushRoutingSecret {
    /// The minter's recency stamp — the primary key of the ADR-0044 canonical
    /// total order. Opaque; never reaches a routing-id derivation.
    pub mint_epoch: u64,
    /// The decoded 32-byte per-group routing secret.
    pub secret: Vec<u8>,
}

/// Strictly decode a push-routing-secret distribution envelope (ADR-0042 v2,
/// per ADR-0044). Fail-closed: rejects a plaintext that does not carry the
/// push-routing-secret domain separator or whose payload is malformed —
/// including a retired v1 envelope, which carries no `mint_epoch` and so cannot
/// be placed in the total order.
///
/// The NSE calls this as the AUTHORITATIVE half of its service-envelope filter,
/// behind a cheap length + first-byte peek (the same two-step the foreground
/// `PushGroupRouting.tryDecodeSecretEnvelope` uses). An `Err` means "not this
/// envelope" and the caller falls through to the normal chat parse.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid push-routing-secret
///   envelope (wrong domain / bad length / unknown version — a v1 envelope
///   lands here).
#[uniffi::export]
pub fn decode_push_routing_secret(plaintext: Vec<u8>) -> Result<FfiPushRoutingSecret, FfiError> {
    rust_decode_push_routing_secret(&plaintext)
        .map(|envelope| FfiPushRoutingSecret {
            mint_epoch: envelope.mint_epoch,
            secret: envelope.secret.to_vec(),
        })
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::push_routing::{
        compose_push_routing_secret, PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
    };

    /// A well-formed v2 envelope round-trips through the slim decoder with the
    /// mint epoch + secret intact — the surface the NSE filter calls.
    #[test]
    fn decodes_a_well_formed_envelope() {
        let secret = [0x42u8; 32];
        let wire = compose_push_routing_secret(7, &secret);
        let decoded = decode_push_routing_secret(wire).expect("v2 envelope decodes");
        assert_eq!(decoded.mint_epoch, 7);
        assert_eq!(decoded.secret, secret.to_vec());
    }

    /// The decode is what makes the NSE filter fail-closed the RIGHT way: a
    /// real chat envelope must NOT decode, or every chat message would be
    /// suppressed as a service envelope.
    #[test]
    fn refuses_a_chat_envelope() {
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(matches!(
            decode_push_routing_secret(chat),
            Err(FfiError::Contacts(_)),
        ));
    }

    /// Bytes carrying the right domain but a truncated payload are refused —
    /// the length gate, reachable from a corrupt / hostile journal row.
    #[test]
    fn refuses_a_truncated_payload() {
        let mut truncated = PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.to_vec();
        truncated.push(0x02);
        truncated.extend_from_slice(&[0u8; 8]);
        truncated.extend_from_slice(&[0u8; 31]); // one byte short of the secret
        assert!(matches!(
            decode_push_routing_secret(truncated),
            Err(FfiError::Contacts(_)),
        ));
    }

    /// ADR-0044 is a HARD flag day: a retired v1 (62-byte) envelope carries no
    /// `mint_epoch`, has no position in the total order, and is refused. Pinned
    /// here because the NSE's cheap pre-filter keys on the v2 length — if the
    /// codec ever silently dual-accepted, the pre-filter and the codec would
    /// disagree about what a service envelope is.
    #[test]
    fn refuses_a_retired_v1_envelope() {
        let mut v1 = PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.to_vec();
        v1.push(0x01);
        v1.extend_from_slice(&[0x42u8; 32]);
        assert_eq!(v1.len(), 62);
        assert!(matches!(
            decode_push_routing_secret(v1),
            Err(FfiError::Contacts(_)),
        ));
    }

    /// Empty input never panics — the parser is reached before any state is
    /// consulted, on bytes that arrive from the zero-trust DS.
    #[test]
    fn refuses_empty_input() {
        assert!(matches!(
            decode_push_routing_secret(Vec::new()),
            Err(FfiError::Contacts(_)),
        ));
    }

    /// The Swift-side cheap pre-filter keys on `count == 70 && first == 0x6c`.
    /// Pin both here so a wire change fails a Rust test rather than silently
    /// making the NSE peek reject every real envelope (which would restore the
    /// garbage-notification defect).
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        let wire = compose_push_routing_secret(1, &[0u8; 32]);
        assert_eq!(wire.len(), 70, "NSE pre-filter pins the envelope length");
        assert_eq!(wire[0], 0x6c, "NSE pre-filter pins the first domain byte");
    }
}
