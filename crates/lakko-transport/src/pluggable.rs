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

use futures::stream::{FuturesUnordered, StreamExt as _};

use async_trait::async_trait;
use thiserror::Error;

use crate::endpoint::Endpoint;
use crate::transport::{Connection, Transport};

/// How long the race waits before starting the next candidate in config
/// order (RFC 8305 section 5 recommends 250 ms). Chosen, not measured.
pub const CONNECTION_ATTEMPT_DELAY: Duration = Duration::from_millis(250);

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
/// Semantic: start the candidates one [`CONNECTION_ATTEMPT_DELAY`]
/// apart in config order, all running at once; the FIRST to return
/// `Ok` wins and the rest are dropped (cancelled), returning the
/// winning kind + connection. If every task returns `Err` (or `deadline`
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
    let deadline_ms = u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX);

    // Concurrent race (RFC 8305 shape): candidate `i` starts `i` connection
    // attempt delays after the first, in config order, and the first `Ok`
    // wins; the others are dropped, which cancels them. A candidate that
    // hangs (a blocked port answers nothing) therefore no longer holds the
    // ones after it for the whole deadline, as the sequential race did.
    let mut attempts: FuturesUnordered<_> = candidates
        .iter()
        .enumerate()
        .map(|(index, c)| async move {
            let delay =
                CONNECTION_ATTEMPT_DELAY.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX));
            tokio::time::sleep(delay).await;
            (index, c.kind, c.transport.connect(endpoint).await)
        })
        .collect();
    let mut failed: Vec<(usize, PluggableKind, crate::error::Error)> =
        Vec::with_capacity(candidates.len());
    let expiry = tokio::time::sleep(deadline);
    tokio::pin!(expiry);
    loop {
        tokio::select! {
            next = attempts.next() => match next {
                Some((_, kind, Ok(conn))) => return Ok((kind, conn)),
                Some((index, kind, Err(e))) => failed.push((index, kind, e)),
                None => break,
            },
            () = &mut expiry => break,
        }
    }
    failed.sort_by_key(|(index, _, _)| *index);
    Err(PluggableError::AllFailed {
        attempted: candidates.len(),
        deadline_ms,
        per_candidate: failed.into_iter().map(|(_, kind, e)| (kind, e)).collect(),
    })
}

// ---------------------------------------------------------
// Tests
// ---------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reality::{RealityConfig, RealityTransport};

    /// A test transport: hangs forever, fails at once, or connects after a delay.
    enum Fake {
        Hangs,
        Fails,
        ConnectsAfter(Duration),
    }

    struct FakeConnection;

    #[async_trait]
    impl Connection for FakeConnection {
        async fn send(&mut self, _frame: bytes::Bytes) -> crate::error::Result<()> {
            Ok(())
        }
        async fn recv(&mut self) -> crate::error::Result<bytes::Bytes> {
            Err(crate::error::Error::PeerClosed)
        }
        async fn close(&mut self) -> crate::error::Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Transport for Fake {
        async fn connect(&self, _endpoint: &Endpoint) -> crate::error::Result<Box<dyn Connection>> {
            match self {
                Self::Hangs => std::future::pending().await,
                Self::Fails => Err(crate::error::Error::Io("refused".into())),
                Self::ConnectsAfter(d) => {
                    tokio::time::sleep(*d).await;
                    Ok(Box::new(FakeConnection))
                },
            }
        }
    }

    fn endpoint() -> Endpoint {
        Endpoint::hostname("lakko.test", 443)
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_first_candidate_does_not_hold_the_second() {
        let candidates = vec![
            PluggableCandidate::new(PluggableKind::Reality, Fake::Hangs),
            PluggableCandidate::new(
                PluggableKind::Shadowsocks2022,
                Fake::ConnectsAfter(Duration::from_millis(100)),
            ),
        ];
        let started = tokio::time::Instant::now();
        let (kind, _) = race_select(&candidates, &endpoint(), Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(kind, PluggableKind::Shadowsocks2022);
        assert_eq!(
            started.elapsed(),
            CONNECTION_ATTEMPT_DELAY + Duration::from_millis(100)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_candidate_wins_when_it_answers_within_the_attempt_delay() {
        let candidates = vec![
            PluggableCandidate::new(
                PluggableKind::Reality,
                Fake::ConnectsAfter(Duration::from_millis(50)),
            ),
            PluggableCandidate::new(
                PluggableKind::Shadowsocks2022,
                Fake::ConnectsAfter(Duration::from_millis(1)),
            ),
        ];
        let (kind, _) = race_select(&candidates, &endpoint(), Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(kind, PluggableKind::Reality);
    }

    #[tokio::test(start_paused = true)]
    async fn every_candidate_failing_reports_them_in_config_order() {
        let candidates = vec![
            PluggableCandidate::new(PluggableKind::Reality, Fake::Fails),
            PluggableCandidate::new(PluggableKind::Shadowsocks2022, Fake::Fails),
        ];
        let Err(err) = race_select(&candidates, &endpoint(), Duration::from_secs(30)).await else {
            panic!("a race where every candidate fails must fail");
        };
        match err {
            PluggableError::AllFailed { per_candidate, .. } => {
                let kinds: Vec<_> = per_candidate.iter().map(|(k, _)| *k).collect();
                assert_eq!(
                    kinds,
                    [PluggableKind::Reality, PluggableKind::Shadowsocks2022]
                );
            },
            other => panic!("expected AllFailed, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_ends_a_race_nobody_answers() {
        let candidates = vec![
            PluggableCandidate::new(PluggableKind::Reality, Fake::Hangs),
            PluggableCandidate::new(PluggableKind::Shadowsocks2022, Fake::Hangs),
        ];
        let started = tokio::time::Instant::now();
        let Err(err) = race_select(&candidates, &endpoint(), Duration::from_secs(5)).await else {
            panic!("a race nobody answers must fail");
        };
        assert!(matches!(err, PluggableError::AllFailed { .. }));
        assert_eq!(started.elapsed(), Duration::from_secs(5));
    }

    #[test]
    fn pluggable_kind_string_table_pinned() {
        // Stability table — clients ship configs that carry these
        // strings. A rename is a breaking protocol change.
        assert_eq!(PluggableKind::Quic.as_str(), "quic");
        assert_eq!(PluggableKind::Reality.as_str(), "reality");
        assert_eq!(PluggableKind::Shadowsocks2022.as_str(), "shadowsocks-2022");
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
        // Valid-shape config (32-byte tag_secret, 8-byte
        // short_id) so `RealityTransport::new` accepts it.
        // Since S-5-reality-driver-v2 step 4/4, REALITY's
        // `connect` actually opens a TCP socket and drives
        // the handshake; pointing at `example.invalid:443`
        // makes the connect leg fail at DNS resolution and
        // surface `Error::Io`, which is enough to exercise
        // the happy-eyeballs `AllFailed` path.
        use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
        RealityTransport::new(RealityConfig {
            sni: "example.invalid".into(),
            tag_secret_b64: B64.encode([0u8; 32]),
            short_id_hex: hex::encode([0u8; 8]),
            allowed_inner_servers: vec![],
            remote: "example.invalid:443".into(),
            require_server_response: true,
        })
        .expect("dummy_reality builds a valid-shape config")
    }

    #[tokio::test]
    async fn all_candidates_failing_surfaces_all_failed() {
        // Two REALITY candidates pointed at an unresolvable
        // host — the connect leg fails at DNS, the selector
        // wraps both results as `AllFailed`. This test used
        // to assert each `inner` was `NotImplemented` (back
        // when REALITY was scaffold-only); after S-5-reality-
        // driver-v2 it asserts the variant-agnostic shape:
        // attempted count + per-candidate kinds.
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
                for (kind, _inner) in &per_candidate {
                    assert_eq!(*kind, PluggableKind::Reality);
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
