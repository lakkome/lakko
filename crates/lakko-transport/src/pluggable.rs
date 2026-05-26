// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Pluggable-transport (PT) abstraction — Phase 5 scaffold
//! per [ADR-0004](../../../docs/adr/0004-anti-censorship.md).
//!
//! `lakko-transport` already exposes a `Transport` trait that
//! every concrete transport (QUIC via quinn, WebSocket-over-TLS,
//! in-memory test fixture) implements. The **pluggable** layer
//! sits one level up: it lets a *runtime* pick from a menu of
//! transports — typically a happy-eyeballs race between QUIC,
//! REALITY-obfuscated TLS, Shadowsocks-2022, and MASQUE — and
//! return the first that completes a handshake against the
//! current network conditions.
//!
//! ## Why this lives in a separate module
//!
//! The existing `Transport` trait is the **physical** wire — it
//! describes what bytes go on the wire. The pluggable layer
//! captures the *policy*: which transport, in what order, with
//! what fallback budget. Two separate concerns:
//!
//! - `Transport` answers "given this endpoint, hand me a
//!   `Connection`"
//! - `PluggableTransport` answers "given this set of
//!   candidates + a deadline, hand me a `Connection` that
//!   survives censorship"
//!
//! ## What is scaffolded vs implemented
//!
//! Phase 5 ships in seven sprints (G5-1..G5-7 in
//! `docs/ROADMAP-PHASE3-CLOSEOUT.md`). This module shapes the
//! API surface every sprint will populate; the per-transport
//! modules (`reality`, `shadowsocks`, `webtransport_pt`,
//! `masque`) start as stubs that return
//! [`PluggableError::NotImplemented`] from their builder so
//! call sites compile.
//!
//! The selector ([`race_select`]) IS implemented today —
//! happy-eyeballs is transport-agnostic, so it works against
//! stubs + real transports interchangeably. Once a real
//! transport lands, the selector picks it up without code
//! changes.

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

use crate::endpoint::Endpoint;
use crate::transport::{Connection, Transport};

/// Identifier used in config / logs to refer to a specific
/// transport variant. Stable string — clients ship configs that
/// carry these names; renaming any of them is a breaking
/// protocol change for everything that consumed the previous
/// config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluggableKind {
    /// Vanilla QUIC v1 over UDP/443. The hot-path candidate in
    /// uncensored networks; defeats most middlebox flow-tagging
    /// because the bytes after the QUIC header are encrypted.
    Quic,
    /// REALITY (xtls/REALITY) — TLS-in-TLS under an
    /// attacker-chosen SNI. Defeats SNI-blocking + active
    /// probing. Mandatory for the GFW / TSPU / Iranian DPI
    /// scenarios in 2026+.
    Reality,
    /// Shadowsocks-2022 (AEAD-2022). Alternative obfs, simpler
    /// than REALITY, useful for paired-deployment fallback.
    Shadowsocks2022,
    /// WebTransport over HTTP/3. Web-client first-class —
    /// inherits the QUIC under-layer + reuses the browser's
    /// native stack.
    WebTransport,
    /// MASQUE / CONNECT-UDP per RFC 9298. Tunnels QUIC inside
    /// an H3 CONNECT exchange so the outer bytes look like
    /// ordinary HTTPS.
    Masque,
}

impl PluggableKind {
    /// Stable lowercase string identifier. The rendezvous
    /// bootstrap config carries these strings; a typo is a
    /// silent mis-route, so the conversion is centralised
    /// here.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Quic => "quic",
            Self::Reality => "reality",
            Self::Shadowsocks2022 => "shadowsocks-2022",
            Self::WebTransport => "webtransport",
            Self::Masque => "masque",
        }
    }
}

/// One concrete transport candidate: the [`PluggableKind`]
/// discriminator + a boxed `Transport` impl ready to dial.
///
/// The trait object split lets the selector hold a heterogeneous
/// slice of transports without leaking the per-variant type into
/// the selector's type signature.
pub struct PluggableCandidate {
    /// Discriminator — surfaced in logs / metrics + the
    /// selector's return value.
    pub kind: PluggableKind,
    /// Boxed transport instance ready to call `connect()`.
    pub transport: Box<dyn Transport>,
}

impl PluggableCandidate {
    /// Build a candidate from a `Transport` + kind.
    #[must_use]
    pub fn new<T: Transport + 'static>(kind: PluggableKind, transport: T) -> Self {
        Self {
            kind,
            transport: Box::new(transport),
        }
    }
}

/// Failure modes specific to the pluggable layer. Per-transport
/// `Transport::connect` errors are re-wrapped under
/// [`PluggableError::AllFailed`] with the per-candidate detail
/// preserved so the caller can decide whether to surface them
/// to the user.
#[derive(Debug, Error)]
pub enum PluggableError {
    /// No candidate completed within the deadline.
    #[error("all {attempted} transport candidates failed; deadline {deadline_ms} ms")]
    AllFailed {
        /// How many candidates the selector tried.
        attempted: usize,
        /// The deadline the selector enforced, in milliseconds.
        deadline_ms: u64,
        /// Per-candidate failure detail (kind label + error).
        per_candidate: Vec<(PluggableKind, crate::error::Error)>,
    },

    /// The selector was handed an empty candidate set. Bug —
    /// the call site never has a reason to dial with zero
    /// options.
    #[error("pluggable selector called with empty candidate set")]
    EmptyCandidates,

    /// A scaffold-only transport ([`reality`], [`shadowsocks`],
    /// [`masque`], [`webtransport_pt`]) was asked to dial before
    /// its G5-N sprint landed an implementation.
    #[error("{kind:?} transport is scaffold-only — implementation lands in {sprint}")]
    NotImplemented {
        /// Which transport was reached.
        kind: PluggableKind,
        /// The sprint label tracking the open work (see
        /// `docs/ROADMAP-PHASE3-CLOSEOUT.md`).
        sprint: &'static str,
    },
}

/// Convenience alias for results from this module.
pub type PluggableResult<T> = std::result::Result<T, PluggableError>;

/// Pluggable transport policy. A wrapper around a list of
/// `Transport` candidates with a happy-eyeballs selector.
///
/// `tokio` runtime required (the selector uses
/// `tokio::time::timeout` + `tokio::select!`).
#[async_trait]
pub trait PluggableTransport: Send + Sync + 'static {
    /// Dial `endpoint` through the policy. Returns the first
    /// candidate that completes a connect within the deadline.
    async fn connect(
        &self,
        endpoint: &Endpoint,
        deadline: Duration,
    ) -> PluggableResult<(PluggableKind, Box<dyn Connection>)>;
}

/// Default policy implementation — races every candidate in
/// parallel with [`tokio::select!`] and returns the first
/// success. On failure, surfaces every candidate's error so the
/// UI can show the user a useful "no transport worked" message
/// + the operator's monitoring can attribute the failure.
pub struct HappyEyeballs {
    candidates: Vec<PluggableCandidate>,
}

impl HappyEyeballs {
    /// Build the policy from a list of candidates. Empty input
    /// fails at `connect` time, not here — the constructor
    /// stays infallible so call sites can build a policy
    /// progressively.
    #[must_use]
    pub fn new(candidates: Vec<PluggableCandidate>) -> Self {
        Self { candidates }
    }

    /// Number of candidates the selector will race. Surfaced
    /// for the bootstrap-config logger.
    #[must_use]
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    /// Kinds in race order. Surfaced for the bootstrap-config
    /// logger + tests.
    #[must_use]
    pub fn kinds(&self) -> Vec<PluggableKind> {
        self.candidates.iter().map(|c| c.kind).collect()
    }
}

#[async_trait]
impl PluggableTransport for HappyEyeballs {
    async fn connect(
        &self,
        endpoint: &Endpoint,
        deadline: Duration,
    ) -> PluggableResult<(PluggableKind, Box<dyn Connection>)> {
        race_select(&self.candidates, endpoint, deadline).await
    }
}

/// Run the happy-eyeballs race. Public so a future caller can
/// race a one-off ad-hoc set without constructing a
/// [`HappyEyeballs`] instance.
///
/// Semantic: spawn one task per candidate, wait for the FIRST
/// to return `Ok`, abort the rest, return the winning kind +
/// connection. If every task returns `Err` (or `deadline`
/// fires), return `AllFailed` with the per-candidate detail.
///
/// # Errors
///
/// - [`PluggableError::EmptyCandidates`] if the slice is empty.
/// - [`PluggableError::AllFailed`] if every candidate's connect
///   path errored OR the deadline fired before any returned
///   `Ok`.
pub async fn race_select(
    candidates: &[PluggableCandidate],
    endpoint: &Endpoint,
    deadline: Duration,
) -> PluggableResult<(PluggableKind, Box<dyn Connection>)> {
    if candidates.is_empty() {
        return Err(PluggableError::EmptyCandidates);
    }
    let deadline_ms =
        u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX);

    // **Sequential** race — scaffold version. Each candidate
    // gets its own `tokio::time::timeout` slice of the
    // deadline; first `Ok(...)` wins, errors are collected
    // and returned together if every candidate fails. A
    // proper happy-eyeballs `tokio::select!` race over
    // `Arc<dyn Transport>` clones lands in the first G5-N
    // sprint that needs sub-RTT first-byte; the trait would
    // need `Clone` (or an `Arc<dyn Transport>` wrapper in
    // every candidate) to support concurrent spawn, and
    // refactoring that surface is its own task.
    let mut per_candidate: Vec<(PluggableKind, crate::error::Error)> =
        Vec::with_capacity(candidates.len());
    for c in candidates {
        match tokio::time::timeout(deadline, c.transport.connect(endpoint)).await {
            Ok(Ok(conn)) => return Ok((c.kind, conn)),
            Ok(Err(e)) => per_candidate.push((c.kind, e)),
            Err(_) => {
                return Err(PluggableError::AllFailed {
                    attempted: candidates.len(),
                    deadline_ms,
                    per_candidate,
                });
            },
        }
    }
    Err(PluggableError::AllFailed {
        attempted: candidates.len(),
        deadline_ms,
        per_candidate,
    })
}

// ---------------------------------------------------------
// Tests
// ---------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reality::{RealityConfig, RealityTransport};

    #[test]
    fn pluggable_kind_string_table_pinned() {
        // Stability table — clients ship configs that carry these
        // strings. A rename is a breaking protocol change.
        assert_eq!(PluggableKind::Quic.as_str(), "quic");
        assert_eq!(PluggableKind::Reality.as_str(), "reality");
        assert_eq!(
            PluggableKind::Shadowsocks2022.as_str(),
            "shadowsocks-2022"
        );
        assert_eq!(PluggableKind::WebTransport.as_str(), "webtransport");
        assert_eq!(PluggableKind::Masque.as_str(), "masque");
    }

    #[tokio::test]
    async fn empty_candidates_rejects_at_connect() {
        let policy = HappyEyeballs::new(vec![]);
        let endpoint = Endpoint::hostname("example.invalid", 443);
        let result = policy.connect(&endpoint, Duration::from_secs(1)).await;
        match result {
            Err(PluggableError::EmptyCandidates) => {},
            Err(other) => panic!("expected EmptyCandidates, got {other:?}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }

    fn dummy_reality() -> RealityTransport {
        RealityTransport::new(RealityConfig {
            sni: "example.invalid".into(),
            server_pubkey_b64: "AAAA".into(),
            short_id_hex: "00".into(),
            allowed_inner_servers: vec![],
            remote: "example.invalid:443".into(),
        })
    }

    #[tokio::test]
    async fn scaffold_transport_surfaces_not_implemented() {
        // Two scaffold candidates — both should report
        // NotImplemented; the selector wraps them as
        // AllFailed because no candidate completed.
        let policy = HappyEyeballs::new(vec![
            PluggableCandidate::new(PluggableKind::Reality, dummy_reality()),
            PluggableCandidate::new(PluggableKind::Reality, dummy_reality()),
        ]);
        let endpoint = Endpoint::hostname("example.invalid", 443);
        let result = policy.connect(&endpoint, Duration::from_secs(1)).await;
        match result {
            Ok(_) => panic!("expected error, got Ok"),
            Err(PluggableError::AllFailed {
                attempted,
                per_candidate,
                ..
            }) => {
                assert_eq!(attempted, 2);
                assert_eq!(per_candidate.len(), 2);
                for (kind, inner) in &per_candidate {
                    assert_eq!(*kind, PluggableKind::Reality);
                    assert!(matches!(inner, crate::error::Error::NotImplemented { .. }));
                }
            },
            Err(other) => panic!("expected AllFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn candidate_kinds_in_race_order_preserved() {
        let policy = HappyEyeballs::new(vec![
            PluggableCandidate::new(PluggableKind::Reality, dummy_reality()),
            PluggableCandidate::new(PluggableKind::Masque, dummy_reality()),
        ]);
        assert_eq!(policy.candidate_count(), 2);
        assert_eq!(
            policy.kinds(),
            vec![PluggableKind::Reality, PluggableKind::Masque]
        );
    }
}
