// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Censorship-lab E2E — verifies the property a
//! state-level adversary sees on the first packet of a
//! TLS handshake (G5-7).
//!
//! The test opens a `tokio-rustls` client connection to
//! the `sni-observer` running on
//! `127.0.0.1:8443` (the address the
//! `ci/censorship-lab/docker-compose.yml` stack
//! publishes), asks rustls to send `lakko.me` in the
//! server_name extension, then reads
//! `ci/censorship-lab/logs/observed.jsonl` from the
//! host bind-mount and asserts the observer wrote a
//! matching line.
//!
//! ## What this gate catches
//!
//! - A future rustls change that strips SNI by default
//!   (would be a transport-layer privacy regression for
//!   the *production* paths but the test would catch it
//!   here as a false-negative).
//! - A future REALITY / Shadowsocks bump that
//!   accidentally leaks the inner SNI to the cover
//!   channel — once the follow-up sub-sprints
//!   (`G5-7-reality`, `G5-7-shadowsocks`) wire those
//!   transports through the same observer harness, the
//!   leak surfaces as the wrong `observed_sni` in the
//!   log.
//!
//! ## Why `#[ignore]`?
//!
//! Standing up the compose stack costs ~10 s on a warm
//! cache. The CI workflow
//! (`.github/workflows/censorship-lab.yml`) runs this
//! test with `--include-ignored` after bringing the
//! stack up; developers run it locally via the
//! `ci/censorship-lab/README.md` recipe.

#![cfg(test)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Address of the running observer. Overridable so a
/// CI matrix can target a different host:port without
/// recompiling.
fn observer_addr() -> String {
    std::env::var("LAKKO_CENSORSHIP_OBSERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8443".to_owned())
}

/// Host-side path where the compose volume surfaces the
/// observer's `observed.jsonl` log. Defaults to the
/// committed compose layout; the CI workflow overrides
/// when matrix-paralleled runs publish to a temp dir.
fn observer_log_path() -> PathBuf {
    if let Ok(p) = std::env::var("LAKKO_CENSORSHIP_OBSERVER_LOG") {
        PathBuf::from(p)
    } else {
        // Resolve relative to the workspace root.
        // CARGO_MANIFEST_DIR is `crates/lakko-transport/`,
        // and the compose stack lives at
        // `<workspace>/ci/censorship-lab/`.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest
            .parent()
            .and_then(|p| p.parent())
            .expect("manifest must live two levels under workspace root");
        workspace.join("ci/censorship-lab/logs/observed.jsonl")
    }
}

/// Insecure server-cert verifier. The observer drops
/// the connection after the ClientHello — there is no
/// real handshake to validate. The Lakko production
/// paths NEVER use this verifier; it lives only here as
/// a censorship-lab fixture.
#[derive(Debug)]
struct AcceptAnyCert;

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ED25519,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

#[tokio::test]
#[ignore = "requires ci/censorship-lab docker compose stack — see ci/censorship-lab/README.md"]
async fn observer_records_clienthello_sni() {
    // Use the ring crypto provider (installed in process
    // by the first test that touches it). Idempotent:
    // calling `install_default` twice is a no-op error
    // we silently accept.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(config));
    let sni_under_test = "lakko-censorship-test.example";
    let server_name =
        ServerName::try_from(sni_under_test.to_owned()).expect("test SNI is a valid DNS name");

    let stream = TcpStream::connect(observer_addr())
        .await
        .expect("TCP connect to observer must succeed — is the compose stack up?");

    // The handshake will not complete — the observer
    // drops the connection after reading the
    // ClientHello. We intentionally do not assert
    // success; the side effect (the SNI being written
    // to the observer's log) is what we care about.
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        connector.connect(server_name, stream),
    )
    .await;

    // Brief settle so the observer's async log flush
    // reaches disk. The observer calls `flush()`
    // synchronously after each write, but on tmpfs /
    // tmpfs-backed bind mounts the host view can lag
    // by a few ms.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let log_path = observer_log_path();
    let mut file = tokio::fs::File::open(&log_path)
        .await
        .unwrap_or_else(|e| panic!("open observer log {}: {e}", log_path.display()));
    let mut body = String::new();
    file.read_to_string(&mut body).await.expect("read log");

    // The observer appends — older lines from previous
    // test runs may sit ahead of ours. Walk from the
    // tail and find the first line that carries our SNI.
    let found = body
        .lines()
        .rev()
        .find(|line| line.contains(&format!(r#""observed_sni":"{sni_under_test}""#)));

    assert!(
        found.is_some(),
        "observer log must contain a line with observed_sni={sni_under_test:?}.\n\
         Tail of {}:\n{}",
        log_path.display(),
        body.lines().rev().take(5).collect::<Vec<_>>().join("\n"),
    );
}

/// G5-7-fingerprint — verify the observer records JA3 +
/// JA4 fingerprints alongside the SNI. The matching log
/// line MUST carry non-null `ja3`, `ja3_string`, and `ja4`
/// fields, and each MUST match the canonical shape (JA3
/// = 32 lowercase hex chars; JA4 = `<prefix>_<hash_a>_<hash_b>`
/// with two 12-char hex hashes).
#[tokio::test]
#[ignore = "requires ci/censorship-lab docker compose stack — see ci/censorship-lab/README.md"]
async fn observer_records_ja3_and_ja4_fingerprints() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));

    // A distinct SNI so this test's log line cannot be
    // confused with the SNI-only test's line in a shared
    // observed.jsonl tail.
    let sni_under_test = "lakko-fingerprint-test.example";
    let server_name =
        ServerName::try_from(sni_under_test.to_owned()).expect("test SNI is a valid DNS name");

    let stream = TcpStream::connect(observer_addr())
        .await
        .expect("TCP connect to observer must succeed — is the compose stack up?");

    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        connector.connect(server_name, stream),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let log_path = observer_log_path();
    let mut file = tokio::fs::File::open(&log_path)
        .await
        .unwrap_or_else(|e| panic!("open observer log {}: {e}", log_path.display()));
    let mut body = String::new();
    file.read_to_string(&mut body).await.expect("read log");

    let line = body
        .lines()
        .rev()
        .find(|line| line.contains(&format!(r#""observed_sni":"{sni_under_test}""#)))
        .unwrap_or_else(|| {
            panic!(
                "observer log must contain a fingerprint line for {sni_under_test:?}.\n\
                 Tail of {}:\n{}",
                log_path.display(),
                body.lines().rev().take(5).collect::<Vec<_>>().join("\n"),
            )
        });

    // JA3 — 32 lowercase hex chars between the literal
    // `"ja3":"` and the next `"`.
    let ja3 = extract_string_field(line, "ja3")
        .unwrap_or_else(|| panic!("line missing `ja3` field: {line}"));
    assert!(
        ja3 != "null"
            && ja3.len() == 32
            && ja3
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "ja3 must be 32 lowercase hex chars, got: {ja3:?}",
    );

    // JA3 raw string — non-empty, contains commas.
    let ja3_string = extract_string_field(line, "ja3_string")
        .unwrap_or_else(|| panic!("line missing `ja3_string` field: {line}"));
    assert!(
        ja3_string != "null" && ja3_string.matches(',').count() == 4,
        "ja3_string must carry 4 commas (5 fields), got: {ja3_string:?}",
    );

    // JA4 — `<prefix>_<hash_a>_<hash_b>`, each hash 12 hex chars.
    let ja4 = extract_string_field(line, "ja4")
        .unwrap_or_else(|| panic!("line missing `ja4` field: {line}"));
    let parts: Vec<&str> = ja4.split('_').collect();
    assert_eq!(
        parts.len(),
        3,
        "JA4 must have 3 underscore-separated parts: {ja4:?}"
    );
    assert!(
        parts[0].starts_with('t'),
        "JA4 prefix must begin with `t` (TCP): {ja4:?}"
    );
    assert_eq!(parts[1].len(), 12, "JA4 hash_a must be 12 chars: {ja4:?}");
    assert_eq!(parts[2].len(), 12, "JA4 hash_b must be 12 chars: {ja4:?}");
    assert!(
        parts[1].chars().all(|c| c.is_ascii_hexdigit()),
        "JA4 hash_a must be hex: {ja4:?}",
    );
    assert!(
        parts[2].chars().all(|c| c.is_ascii_hexdigit()),
        "JA4 hash_b must be hex: {ja4:?}",
    );
}

/// Minimal JSON-field extractor — finds `"name":"value"`
/// in a flat single-line JSON object. Returns the value
/// without the surrounding quotes, or `None` if the field
/// is missing or appears as `"name":null`. Avoids a full
/// `serde_json` dep just for the test harness — the
/// observer output is strictly known-shape one-line
/// JSON.
fn extract_string_field<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let needle = format!("\"{field}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if rest.starts_with("null") {
        // Strictly speaking returning `Some("null")` would
        // let the caller treat that as the JSON null
        // sentinel; the assertions above use `!= "null"`
        // for that case, so we keep the shape consistent.
        return Some("null");
    }
    let after_quote = rest.strip_prefix('"')?;
    let end = after_quote.find('"')?;
    Some(&after_quote[..end])
}

// ===========================================================
// G5-7-transport-matrix iter1 — Shadowsocks-2022 leaks-nothing
// ===========================================================
//
// The first transport-matrix data point: drive
// `ShadowsocksTransport::connect` at the SNI observer and
// assert the observer logs the connection as **non-TLS**.
//
// What the observer should see:
//   - First inbound bytes are a SS-2022 outer header:
//     16-byte salt (or 32 for chacha20-poly1305) +
//     sealed prelude. Random bytes; the first byte is
//     almost never `0x16` (TLS handshake record type),
//     and even if it were, the next bytes do not form
//     a valid TLS record header.
//   - `parse_client_hello` walks the wire format, fails
//     at the first sanity check, returns `None`.
//   - JSONL line: `observed_sni: null`, `ja3: null`,
//     `ja4: null`. `first_bytes_hex` carries the random
//     prefix.
//
// Why this matters for the security claim:
//   Shadowsocks-2022 is Lakko's "no metadata" transport
//   for environments where REALITY's cover-TLS heuristic
//   is itself a fingerprint. Any future change that
//   accidentally surfaces a parseable TLS ClientHello
//   (e.g. an obfs layer wrapping outbound bytes in a TLS
//   record) would defeat the design. The observer
//   doubles as the regression detector.
//
// Companion claims this gate validates:
//   T-10 (SS-2022 outer header layer) — bytes are
//   indistinguishable from random.
//   T-11 (SS-2022 TCP wiring) — does not accidentally
//   emit pre-header TLS-looking bytes.

use base64::engine::general_purpose::STANDARD as B64_STANDARD;
use base64::Engine as _;
use lakko_transport::{Endpoint, ShadowsocksConfig, ShadowsocksTransport, Transport};

/// Drive a SS-2022 client at the observer; assert the
/// observer's log line records `observed_sni: null`
/// AND the connection was logged at all (i.e. the
/// observer parsed the inbound bytes through to the
/// JSON-emit step, not silently dropped).
#[tokio::test]
#[ignore = "requires ci/censorship-lab docker compose stack — see ci/censorship-lab/README.md"]
async fn observer_records_shadowsocks_as_non_tls() {
    // PSK is arbitrary — the observer has no server
    // partner, the connection will fail mid-handshake.
    // What matters: SS-2022 emits its outer-header bytes
    // before failing, and those bytes hit the observer.
    let psk_b64 = B64_STANDARD.encode([0x99u8; 32]);
    let cfg = ShadowsocksConfig {
        remote: observer_addr(),
        method: "2022-blake3-chacha20-poly1305".into(),
        psk_b64,
    };
    let transport =
        ShadowsocksTransport::new(cfg).expect("ShadowsocksConfig must build a valid transport");

    // Connect must fail — the observer has no SS server
    // brain and drops the TCP after reading the prelude.
    // What we care about is that the prelude was emitted
    // and logged.
    let endpoint = Endpoint::Hostname {
        host: "127.0.0.1".into(),
        port: observer_addr()
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .expect("observer_addr should carry an explicit port"),
    };
    let connect_result =
        tokio::time::timeout(Duration::from_secs(3), transport.connect(&endpoint)).await;
    // We don't assert on the connect result — it may
    // surface as Err(timeout) (observer drops without
    // reply) or Err(transport-level error) depending on
    // how far SS-2022 got into its read-prelude state
    // machine. Both are fine; the observation already
    // happened before any of those terminal states.
    let _ = connect_result;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let log_path = observer_log_path();
    let mut file = tokio::fs::File::open(&log_path)
        .await
        .unwrap_or_else(|e| panic!("open observer log {}: {e}", log_path.display()));
    let mut body = String::new();
    file.read_to_string(&mut body).await.expect("read log");

    // The most recent line should be ours. SS-2022 does
    // not embed a SNI we could grep for, so we identify
    // our line by the timestamp ordering: tail of file =
    // most recent. We then assert the shape.
    let last = body
        .lines()
        .rev()
        .next()
        .unwrap_or_else(|| panic!("observer log is empty: {}", log_path.display()));

    // `observed_sni: null` is what an unparseable / non-
    // TLS prelude looks like in the JSONL. The extractor
    // returns "null" (the literal four-char sentinel) for
    // a `null` JSON value.
    let observed = extract_string_field(last, "observed_sni")
        .unwrap_or_else(|| panic!("line missing observed_sni: {last}"));
    assert_eq!(
        observed, "null",
        "shadowsocks outer header must NOT parse as a TLS ClientHello — observed_sni should be null.\n\
         line: {last}",
    );

    // JA3 + JA4 must also be null — these are derived
    // from the same parser, so any leak would surface here
    // too.
    let ja3 =
        extract_string_field(last, "ja3").unwrap_or_else(|| panic!("line missing ja3: {last}"));
    assert_eq!(
        ja3, "null",
        "shadowsocks bytes must not yield a JA3 fingerprint. line: {last}",
    );
    let ja4 =
        extract_string_field(last, "ja4").unwrap_or_else(|| panic!("line missing ja4: {last}"));
    assert_eq!(
        ja4, "null",
        "shadowsocks bytes must not yield a JA4 fingerprint. line: {last}",
    );

    // first_bytes_hex must be present + non-empty (the
    // observer read SOMETHING from the connection).
    let first = extract_string_field(last, "first_bytes_hex")
        .unwrap_or_else(|| panic!("line missing first_bytes_hex: {last}"));
    assert_ne!(
        first, "null",
        "observer must have read at least one byte from shadowsocks. line: {last}",
    );
    assert!(
        !first.is_empty(),
        "first_bytes_hex must record the observed prelude. line: {last}",
    );
    // The first hex pair MUST NOT be the TLS handshake
    // record type `16` — that would mean SS-2022's outer
    // header started with what looks like a TLS record,
    // which would defeat the no-TLS posture even if the
    // rest fails to parse. This is the precise
    // regression guard.
    assert!(
        !first.starts_with("16"),
        "shadowsocks outer header must NOT begin with 0x16 (TLS handshake type). \
         first_bytes_hex: {first}",
    );
}
