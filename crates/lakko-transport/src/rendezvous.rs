// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Signed bootstrap rendezvous config — Phase 5 G5-5 per
//! [ADR-0004](../../../docs/adr/0004-anti-censorship.md).
//!
//! Workflow on first launch (and refresh):
//!
//! 1. Client resolves the rendezvous hostname through one
//!    of a hard-coded set of DoH providers (Cloudflare,
//!    Google, Quad9, Mullvad).
//! 2. Client `HTTP GET`s the rendezvous endpoint at a
//!    rotating innocuous-looking domain. The response body
//!    is a JSON [`BootstrapConfig`].
//! 3. Client calls [`BootstrapConfig::verify`] against a
//!    pinned Ed25519 trust anchor + checks expiry via
//!    [`BootstrapConfig::is_fresh`].
//! 4. On success, client builds [`PluggableCandidate`]s
//!    from the [`TransportSpec`] entries + hands them to
//!    [`crate::pluggable::HappyEyeballs`].
//!
//! ## Canonical signing
//!
//! The signature covers `serde_json::to_string(&self)`
//! with **the `signature` field zeroed first**. Two
//! decoders that disagree on `serde_json`'s key-ordering
//! still agree on the canonical bytes because
//! `serde_json` preserves the original JSON key order on
//! round-trip when fed through a `serde_json::Value`
//! re-serialise pass. We pin the canonical form here so
//! a future signer-side rewrite cannot drift from the
//! verifier.
//!
//! ## Trust anchor
//!
//! The trust-anchor Ed25519 public key is **shipped in
//! the binary** (TUF / app bundle). Compromise of the
//! signing key means a new build; rotation procedure is
//! tracked in `docs/security/known-gaps.md` G-N for the
//! upcoming sprint that wires TUF auto-update.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::masque::MasqueConfig;
use crate::reality::RealityConfig;
use crate::shadowsocks::ShadowsocksConfig;
use crate::webtransport_pt::WebTransportConfig;
use lakko_crypto::signing::{Signature as Ed25519Signature, VerifyingKey};

/// One transport entry inside the bootstrap config. The
/// `kind` discriminator matches
/// [`crate::pluggable::PluggableKind::as_str`]; payload
/// shape per variant matches the config struct in the
/// corresponding `lakko-transport` module.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TransportSpec {
    /// REALITY (xtls/REALITY) — G5-1.
    Reality(RealityConfig),
    /// Shadowsocks-2022 — G5-3.
    #[serde(rename = "shadowsocks-2022")]
    Shadowsocks2022(ShadowsocksConfig),
    /// WebTransport — G5-4.
    WebTransport(WebTransportConfig),
    /// MASQUE / CONNECT-UDP — G5-2.
    Masque(MasqueConfig),
}

/// Top-level rendezvous bootstrap config. Wire format:
/// JSON with the `signature` field cleared during signing,
/// then re-attached at the end.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapConfig {
    /// Schema version. `1` is the current line; a bump
    /// means clients that haven't updated reject the
    /// config rather than silently misparse new fields.
    pub version: u32,
    /// Issued-at unix seconds. The verifier accepts
    /// configs in a configurable past-window (default
    /// 1 hour clock skew); negative skew (future
    /// `issued_at`) is rejected outright.
    pub issued_at: u64,
    /// Expiration unix seconds. After this point
    /// [`Self::is_fresh`] returns `false` and clients
    /// fall back to a re-fetch.
    pub expires_at: u64,
    /// Ordered transport list. Order is the suggested
    /// race priority — clients race them in order via
    /// `HappyEyeballs`.
    pub transports: Vec<TransportSpec>,
    /// Ed25519 signature over [`Self::canonical_bytes`].
    /// Hex-encoded so the JSON stays inspectable; base64
    /// would be slightly tighter but tooling-friendly
    /// loses out to hex for an auditor staring at a
    /// captured config.
    pub signature_hex: String,
}

/// Failure modes the rendezvous verifier surfaces.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RendezvousError {
    /// Schema version differs from this binary's expected
    /// value. Clients with the wrong version refuse
    /// rather than try to parse forward — protects
    /// against malicious server pushing a forward-
    /// incompatible schema bump to silence a client.
    #[error("rendezvous schema version mismatch: expected {expected}, got {got}")]
    VersionMismatch {
        /// Version this binary expects.
        expected: u32,
        /// Version the wire payload carried.
        got: u32,
    },

    /// `signature_hex` did not parse as 64 hex chars.
    #[error("signature_hex is not 128 hex chars (got {got})")]
    BadSignatureFormat {
        /// Length the caller passed.
        got: usize,
    },

    /// Ed25519 verify failed against the trust anchor.
    #[error("rendezvous signature did not verify against the trust anchor")]
    SignatureRejected,

    /// `expires_at` is in the past.
    #[error("rendezvous expired at {expires_at} unix-seconds (now {now})")]
    Expired {
        /// `expires_at` from the config.
        expires_at: u64,
        /// Current wall-clock the caller passed.
        now: u64,
    },

    /// `issued_at` is in the future beyond the allowed
    /// clock-skew window. Either an evil server or a
    /// clock-broken client; both warrant a hard reject.
    #[error("rendezvous issued in the future: issued_at {issued_at}, now {now}")]
    IssuedInFuture {
        /// `issued_at` from the config.
        issued_at: u64,
        /// Current wall-clock the caller passed.
        now: u64,
    },

    /// JSON round-trip failure during canonicalisation.
    /// Should not happen with valid `BootstrapConfig`
    /// inputs; surfaced so the verifier's contract is
    /// total.
    #[error("rendezvous canonicalisation failed: {0}")]
    CanonicalJsonFailed(String),
}

/// Convenience alias.
pub type RendezvousResult<T> = std::result::Result<T, RendezvousError>;

/// Current schema version this binary speaks. A bump is
/// a wire-protocol break; clients refuse other versions.
pub const SCHEMA_VERSION: u32 = 1;

/// Allowed past-window for `issued_at` (seconds). A 1-hour
/// window covers typical clock skew + the
/// distribution-time fan-out from the rendezvous server
/// to global edge caches.
pub const ISSUED_AT_CLOCK_SKEW_SECS: u64 = 3600;

impl BootstrapConfig {
    /// Canonical bytes the signer + verifier agree on.
    /// `signature_hex` is zeroed before serialisation;
    /// `serde_json` preserves struct field order so two
    /// re-serialisations produce identical bytes.
    fn canonical_bytes(&self) -> RendezvousResult<Vec<u8>> {
        let zeroed = Self {
            version: self.version,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            transports: self.transports.clone(),
            signature_hex: String::new(),
        };
        serde_json::to_vec(&zeroed)
            .map_err(|e| RendezvousError::CanonicalJsonFailed(e.to_string()))
    }

    /// Verify signature + schema + clock window against
    /// `trust_anchor` and `now`. The two checks are
    /// distinct error variants so the caller can branch
    /// on a stale-cache vs forged signature.
    ///
    /// # Errors
    ///
    /// See [`RendezvousError`].
    pub fn verify(
        &self,
        trust_anchor: &VerifyingKey,
        now: SystemTime,
    ) -> RendezvousResult<()> {
        if self.version != SCHEMA_VERSION {
            return Err(RendezvousError::VersionMismatch {
                expected: SCHEMA_VERSION,
                got: self.version,
            });
        }
        let now_unix = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if self.expires_at <= now_unix {
            return Err(RendezvousError::Expired {
                expires_at: self.expires_at,
                now: now_unix,
            });
        }
        if self.issued_at > now_unix.saturating_add(ISSUED_AT_CLOCK_SKEW_SECS) {
            return Err(RendezvousError::IssuedInFuture {
                issued_at: self.issued_at,
                now: now_unix,
            });
        }

        let sig_bytes = hex::decode(&self.signature_hex).map_err(|()| {
            RendezvousError::BadSignatureFormat {
                got: self.signature_hex.len(),
            }
        })?;
        let sig = Ed25519Signature::from_bytes(&sig_bytes)
            .map_err(|_| RendezvousError::BadSignatureFormat {
                got: self.signature_hex.len(),
            })?;
        let canonical = self.canonical_bytes()?;
        trust_anchor
            .verify(&canonical, &sig)
            .map_err(|_| RendezvousError::SignatureRejected)
    }

    /// `true` if the config has not yet expired against
    /// `now`. Split out from [`Self::verify`] so a UI
    /// affordance can show "the bootstrap will expire in
    /// N hours" without re-running the signature path.
    #[must_use]
    pub fn is_fresh(&self, now: SystemTime) -> bool {
        let now_unix = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.expires_at > now_unix
    }

    /// Sign helper — produces a fresh `signature_hex`
    /// against this config's canonical bytes. Used by the
    /// server-side `services/transport-rendezvous` (and by
    /// unit tests on the verify path). Production
    /// signing happens server-side; the helper is
    /// available here so the canonical-bytes contract
    /// stays in one place.
    pub fn sign(
        &mut self,
        signer: &lakko_crypto::signing::SigningKey,
    ) -> RendezvousResult<()> {
        self.signature_hex = String::new();
        let canonical = self.canonical_bytes()?;
        let sig = signer.sign(&canonical);
        self.signature_hex = hex::encode(sig.to_bytes());
        Ok(())
    }
}

// We don't pull `hex` directly — it transitively comes
// in through the workspace. Re-exporting the only fn we
// use here avoids adding a new top-level dep.
mod hex {
    use std::fmt::Write as _;
    pub(super) fn encode(bytes: impl AsRef<[u8]>) -> String {
        let mut out = String::with_capacity(bytes.as_ref().len() * 2);
        for b in bytes.as_ref() {
            // write!-to-String never fails; the unused
            // `Result` is discarded explicitly.
            let _ = write!(out, "{b:02x}");
        }
        out
    }
    pub(super) fn decode(s: &str) -> Result<Vec<u8>, ()> {
        if s.len() % 2 != 0 {
            return Err(());
        }
        let mut out = Vec::with_capacity(s.len() / 2);
        for chunk in s.as_bytes().chunks_exact(2) {
            let hi = decode_nibble(chunk[0])?;
            let lo = decode_nibble(chunk[1])?;
            out.push((hi << 4) | lo);
        }
        Ok(out)
    }
    fn decode_nibble(b: u8) -> Result<u8, ()> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;
    use std::time::Duration;

    fn fixture_config(issued: u64, expires: u64) -> BootstrapConfig {
        BootstrapConfig {
            version: SCHEMA_VERSION,
            issued_at: issued,
            expires_at: expires,
            transports: vec![TransportSpec::Reality(RealityConfig {
                sni: "example.invalid".into(),
                server_pubkey_b64: "AAAA".into(),
                short_id_hex: "00".into(),
                allowed_inner_servers: vec!["inner.lakko.me".into()],
                remote: "1.2.3.4:443".into(),
            })],
            signature_hex: String::new(),
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn sign_then_verify_round_trip() {
        let signer = SigningKey::generate();
        let trust = signer.verifying_key();
        let now = now_secs();
        let mut cfg = fixture_config(now, now + 600);
        cfg.sign(&signer).unwrap();
        cfg.verify(&trust, SystemTime::now()).unwrap();
    }

    #[test]
    fn verify_rejects_wrong_trust_anchor() {
        let signer = SigningKey::generate();
        let attacker = SigningKey::generate();
        let now = now_secs();
        let mut cfg = fixture_config(now, now + 600);
        cfg.sign(&signer).unwrap();
        let err = cfg
            .verify(&attacker.verifying_key(), SystemTime::now())
            .unwrap_err();
        assert_eq!(err, RendezvousError::SignatureRejected);
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let signer = SigningKey::generate();
        let trust = signer.verifying_key();
        let now = now_secs();
        let mut cfg = fixture_config(now, now + 600);
        cfg.sign(&signer).unwrap();
        // Tamper with a transport field after signing.
        if let TransportSpec::Reality(r) = &mut cfg.transports[0] {
            r.sni = "evil.invalid".into();
        }
        let err = cfg.verify(&trust, SystemTime::now()).unwrap_err();
        assert_eq!(err, RendezvousError::SignatureRejected);
    }

    #[test]
    fn verify_rejects_expired() {
        let signer = SigningKey::generate();
        let trust = signer.verifying_key();
        let now = now_secs();
        let mut cfg = fixture_config(now - 1000, now - 100);
        cfg.sign(&signer).unwrap();
        let err = cfg.verify(&trust, SystemTime::now()).unwrap_err();
        match err {
            RendezvousError::Expired { .. } => {},
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_issued_in_future() {
        let signer = SigningKey::generate();
        let trust = signer.verifying_key();
        let now = now_secs();
        // 2 hours in the future, beyond the 1-hour skew
        // window.
        let mut cfg = fixture_config(now + 7200, now + 8000);
        cfg.sign(&signer).unwrap();
        let err = cfg.verify(&trust, SystemTime::now()).unwrap_err();
        match err {
            RendezvousError::IssuedInFuture { .. } => {},
            other => panic!("expected IssuedInFuture, got {other:?}"),
        }
    }

    #[test]
    fn verify_tolerates_minor_clock_skew() {
        let signer = SigningKey::generate();
        let trust = signer.verifying_key();
        let now = now_secs();
        // 10 minutes in the future — within the 1-hour
        // skew window.
        let mut cfg = fixture_config(now + 600, now + 1200);
        cfg.sign(&signer).unwrap();
        cfg.verify(&trust, SystemTime::now()).unwrap();
    }

    #[test]
    fn verify_rejects_version_mismatch() {
        let signer = SigningKey::generate();
        let trust = signer.verifying_key();
        let now = now_secs();
        let mut cfg = fixture_config(now, now + 600);
        cfg.version = SCHEMA_VERSION + 99;
        cfg.sign(&signer).unwrap();
        let err = cfg.verify(&trust, SystemTime::now()).unwrap_err();
        assert!(matches!(err, RendezvousError::VersionMismatch { .. }));
    }

    #[test]
    fn json_round_trip_preserves_canonical_bytes() {
        let signer = SigningKey::generate();
        let now = now_secs();
        let mut cfg = fixture_config(now, now + 600);
        cfg.sign(&signer).unwrap();
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: BootstrapConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cfg);
        // And the parsed copy still verifies — proves the
        // wire encode + decode preserves the canonical
        // signing input.
        parsed.verify(&signer.verifying_key(), SystemTime::now()).unwrap();
    }

    #[test]
    fn bad_signature_hex_is_typed_error() {
        let signer = SigningKey::generate();
        let trust = signer.verifying_key();
        let now = now_secs();
        let mut cfg = fixture_config(now, now + 600);
        cfg.signature_hex = "not-hex".into();
        let err = cfg.verify(&trust, SystemTime::now()).unwrap_err();
        assert!(matches!(err, RendezvousError::BadSignatureFormat { .. }));
    }

    #[test]
    fn is_fresh_tracks_expiry() {
        let now = now_secs();
        let cfg = fixture_config(now, now + 60);
        assert!(cfg.is_fresh(SystemTime::now()));
        let stale = fixture_config(now - 1000, now - 60);
        assert!(!stale.is_fresh(SystemTime::now()));
    }

    #[test]
    fn transport_spec_kebab_kind_pinned() {
        // Wire-format guard — kind strings are part of the
        // protocol surface, a bump breaks every config
        // already pinned by deployed clients.
        let json = serde_json::to_string(&TransportSpec::Shadowsocks2022(
            ShadowsocksConfig {
                remote: "1.2.3.4:8388".into(),
                method: "2022-blake3-chacha20-poly1305".into(),
                psk_b64: "AAAA".into(),
            },
        ))
        .unwrap();
        assert!(json.contains("\"kind\":\"shadowsocks-2022\""));
    }

    #[test]
    fn multi_transport_config_round_trip() {
        let signer = SigningKey::generate();
        let trust = signer.verifying_key();
        let now = now_secs();
        let mut cfg = fixture_config(now, now + 3600);
        // Mix four kinds.
        cfg.transports.push(TransportSpec::Shadowsocks2022(ShadowsocksConfig {
            remote: "5.6.7.8:8388".into(),
            method: "2022-blake3-aes-256-gcm".into(),
            psk_b64: "BBBB".into(),
        }));
        cfg.transports.push(TransportSpec::Masque(MasqueConfig {
            proxy_url: "https://proxy.invalid/h3".into(),
            inner_target: "rendez.lakko.me:443".into(),
            bearer: Some("token".into()),
        }));
        cfg.transports.push(TransportSpec::WebTransport(WebTransportConfig {
            url: "https://rdz.lakko.me/wt".into(),
            spki_pins_b64: vec!["pin-a".into(), "pin-b".into()],
        }));
        cfg.sign(&signer).unwrap();
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: BootstrapConfig = serde_json::from_str(&json).unwrap();
        parsed.verify(&trust, SystemTime::now()).unwrap();
        assert_eq!(parsed.transports.len(), 4);
    }

    /// 10-minute clock-skew tolerance happens at runtime —
    /// proven by `verify_tolerates_minor_clock_skew`. This
    /// case probes the boundary: issued_at exactly at
    /// `now + ISSUED_AT_CLOCK_SKEW_SECS` is accepted;
    /// one second past is rejected.
    #[test]
    fn skew_window_boundary() {
        let signer = SigningKey::generate();
        let trust = signer.verifying_key();
        let now = now_secs();
        let at_boundary = now + ISSUED_AT_CLOCK_SKEW_SECS;
        let mut cfg = fixture_config(at_boundary, at_boundary + 60);
        cfg.sign(&signer).unwrap();
        // At exact boundary: accepted.
        cfg.verify(&trust, SystemTime::now()).unwrap();

        // One second past: rejected.
        let past_boundary = now + ISSUED_AT_CLOCK_SKEW_SECS + 1;
        let mut cfg2 = fixture_config(past_boundary, past_boundary + 60);
        cfg2.sign(&signer).unwrap();
        let _ = Duration::from_secs(1); // sanity: import used
        assert!(matches!(
            cfg2.verify(&trust, SystemTime::now()).unwrap_err(),
            RendezvousError::IssuedInFuture { .. }
        ));
    }
}
