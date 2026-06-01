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
use lakko_transport::{
    Endpoint, MasqueConfig, MasqueTransport, RealityConfig, RealityTransport, ShadowsocksConfig,
    ShadowsocksTransport, Transport, WebTransportConfig, WebTransportPt,
};

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

// ===========================================================
// G5-7-transport-matrix iter2 — REALITY cover-SNI assertion
// ===========================================================
//
// REALITY's anti-censorship pitch: from a passive on-path
// observer's viewpoint, a REALITY connection looks
// indistinguishable from a real TLS connection to the cover
// host (`config.sni`). Internally REALITY then unwraps to
// Lakko's real inner endpoint — that inner step happens
// only after the cover-TLS handshake is accepted by the
// REALITY server, and the inner target name is NEVER on the
// wire in cleartext.
//
// This test asserts the observable property:
//   - The client sends a ClientHello whose `server_name`
//     extension carries the configured cover SNI.
//   - The observer logs that cover SNI (not Lakko's real
//     target, not nothing).
//   - The wire bytes are valid TLS (first byte 0x16) and
//     produce a JA3 + JA4 fingerprint — the goal of REALITY
//     is to look like normal TLS to a real host, so JA3/JA4
//     MUST be populated, not absent.
//
// What this gate catches that the existing in-crate REALITY
// tests cannot:
//   - A future REALITY redesign that accidentally emits the
//     real inner target's name in some auxiliary extension
//     (ALPN, ESNI, etc.) before the cover-TLS handshake
//     completes. The observer's parser walks every
//     extension; a name leak anywhere surfaces here.
//   - A future obfs layer that wraps REALITY in something
//     that strips the TLS record header — would surface as
//     `first_bytes_hex` not starting with `16`.
//
// Why `require_server_response: false`: the observer drops
// the connection after reading the ClientHello, so it never
// emits a ServerHello with the mutual-auth HMAC. REALITY's
// require-server-response gate would refuse the handshake
// before we got a chance to observe. The observation
// happens at ClientHello-send time, which is BEFORE the
// server-response check on the client side; flipping the
// flag just lets the client error cleanly after the
// observation rather than panicking the test.
//
// Test against the security claim in
// `docs/security/transport.md` T-1..T-15 + ADR-0004
// §"REALITY".

/// Drive a REALITY client at the observer. Assert the
/// observer's log line records the cover SNI verbatim AND
/// the bytes parse as a valid TLS ClientHello producing a
/// JA3 + JA4 fingerprint.
#[tokio::test]
#[ignore = "requires ci/censorship-lab docker compose stack — see ci/censorship-lab/README.md"]
async fn observer_records_reality_cover_sni() {
    // Install rustls's ring crypto provider exactly once
    // in the test process. Idempotent — the second call
    // returns Err which we deliberately ignore.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cover_sni = "www.microsoft.com";
    let real_target = "lakko.me"; // What the SFU / chat plane
                                  // would have used in a non-REALITY
                                  // transport. We want to assert this
                                  // is NEVER on the wire.

    // Parse observer_addr into host:port so we can build
    // the REALITY config's `remote` field + the Endpoint
    // the connect call points at.
    let addr_str = observer_addr();
    let (host, port_str) = addr_str.rsplit_once(':').expect("observer addr has port");
    let port: u16 = port_str.parse().expect("observer port is u16");

    let cfg = RealityConfig {
        sni: cover_sni.into(),
        // Arbitrary tag secret — the observer is not a real
        // REALITY server, so the secret is never verified;
        // we just need a well-formed config so
        // `RealityTransport::new` accepts it.
        tag_secret_b64: B64_STANDARD.encode([0xCA_u8; 32]),
        short_id_hex: "abababababababab".into(), // 8 bytes hex
        allowed_inner_servers: vec![real_target.into()],
        remote: addr_str.clone(),
        // Disable the mutual-auth gate — see test docstring
        // above for the rationale.
        require_server_response: false,
    };
    let transport = RealityTransport::new(cfg).expect("RealityConfig must validate");

    let endpoint = Endpoint::Hostname {
        host: host.into(),
        port,
    };
    let _ = tokio::time::timeout(Duration::from_secs(5), transport.connect(&endpoint)).await;
    // We don't assert on the connect result — the handshake
    // will fail (observer drops the connection mid-TLS),
    // and that is fine. The observation already happened
    // before the failure surfaces.

    tokio::time::sleep(Duration::from_millis(300)).await;

    let log_path = observer_log_path();
    let mut file = tokio::fs::File::open(&log_path)
        .await
        .unwrap_or_else(|e| panic!("open observer log {}: {e}", log_path.display()));
    let mut body = String::new();
    file.read_to_string(&mut body).await.expect("read log");

    // REALITY's cover SNI MUST be the observed_sni. Find
    // the most recent line carrying our cover SNI literal.
    let line = body
        .lines()
        .rev()
        .find(|line| line.contains(&format!(r#""observed_sni":"{cover_sni}""#)))
        .unwrap_or_else(|| {
            panic!(
                "observer log must carry an observed_sni == {cover_sni:?} line.\n\
                 Tail of {}:\n{}",
                log_path.display(),
                body.lines().rev().take(5).collect::<Vec<_>>().join("\n"),
            )
        });

    // The real inner target MUST NOT appear anywhere in the
    // line — not as observed_sni, not in first_bytes_hex
    // ASCII, not anywhere. This is the negative-space
    // assertion that proves REALITY does NOT leak the real
    // destination via the cover-TLS observation channel.
    assert!(
        !line.contains(real_target),
        "REALITY connection MUST NOT leak the real inner target {real_target:?} in the \
         observed line. Full line: {line}",
    );

    // ja3 + ja4 MUST be populated — REALITY's design is
    // to LOOK like normal TLS, so the fingerprints
    // exist (the security promise is "ja3 matches a
    // popular browser", not "no ja3"). We assert the
    // SHAPE here; whether the fingerprint matches
    // Chrome / Firefox specifically is a separate
    // sub-sprint (G5-7-fingerprint-match).
    let ja3 =
        extract_string_field(line, "ja3").unwrap_or_else(|| panic!("line missing ja3: {line}"));
    assert!(
        ja3 != "null" && ja3.len() == 32,
        "REALITY ClientHello must yield a non-null 32-char JA3. ja3: {ja3:?}",
    );
    let ja4 =
        extract_string_field(line, "ja4").unwrap_or_else(|| panic!("line missing ja4: {line}"));
    assert!(
        ja4 != "null",
        "REALITY ClientHello must yield a non-null JA4. ja4: {ja4:?}",
    );
    let ja4_parts: Vec<&str> = ja4.split('_').collect();
    assert_eq!(
        ja4_parts.len(),
        3,
        "JA4 must have 3 underscore-separated parts: {ja4}",
    );
    assert!(
        ja4_parts[0].starts_with('t'),
        "JA4 prefix must begin with `t` (TCP): {ja4}",
    );

    // first_bytes_hex MUST start with `16` — that's the
    // TLS handshake record type. REALITY by design carries
    // a real TLS record on the wire; if this byte ever
    // drifts, the cover-TLS premise is broken and any
    // DPI box looking for "starts with 16 03" would flag
    // the connection.
    let first = extract_string_field(line, "first_bytes_hex")
        .unwrap_or_else(|| panic!("line missing first_bytes_hex: {line}"));
    assert!(
        first.starts_with("16"),
        "REALITY ClientHello must begin with TLS handshake record type 0x16. \
         first_bytes_hex: {first}",
    );
    // The two bytes after the record-type are the legacy
    // TLS version (`03 01` for TLS 1.0 wire-version which
    // every modern client still sends for compat). Assert
    // the prefix so any future REALITY change that drops
    // the legacy-version envelope surfaces here.
    assert!(
        first.starts_with("160301") || first.starts_with("160303"),
        "REALITY ClientHello must carry a TLS 1.x record header (16 03 01 or 16 03 03). \
         first_bytes_hex: {first}",
    );
}

// ===========================================================
// G5-7-transport-matrix-iter3 — MASQUE + WebTransport via the
// UDP/QUIC half of the observer
// ===========================================================
//
// The observer's TCP listener catches Shadowsocks + REALITY +
// HTTP; MASQUE and WebTransport ride over QUIC v1, so their
// first packets land on the UDP listener that decodes
// RFC 9001 §5.2 Initial packets to extract the TLS
// ClientHello carried in a CRYPTO frame.
//
// What these two tests guard:
//   - The QUIC Initial we send carries the proxy / WT
//     server hostname as the SNI, NOT any "inner" target
//     (MASQUE inner_target / WebTransport URL path).
//   - The observer's `transport: "udp"` field is non-null
//     on a successful decrypt — proving the QUIC Initial
//     was parseable, not silently malformed.
//   - The negative-space invariant: the inner target name
//     must NEVER appear anywhere on the observed line.

/// MASQUE — proxy URL host is the SNI in the QUIC Initial;
/// the configured `inner_target` is wrapped in the
/// CONNECT-UDP request payload, which would only be sent
/// AFTER the QUIC + h3 handshake completes (the observer
/// drops well before that).
#[tokio::test]
#[ignore = "requires ci/censorship-lab docker compose stack — see ci/censorship-lab/README.md"]
async fn observer_records_masque_outer_proxy_sni() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 127.0.0.1 directly — `localhost` resolves to BOTH
    // ::1 and 127.0.0.1, and `to_socket_addrs().next()`
    // picks one platform-dependently; the observer's UDP
    // listener publishes on IPv4 only via Docker's port
    // mapping, so an IPv6-first pick would land in a
    // black hole. Using `127.0.0.1` ALSO means rustls
    // correctly OMITS the SNI extension (RFC 6066 §3
    // forbids IP literals in SNI) — which is fine: this
    // test asserts on the negative-space invariant
    // (inner target never on the wire) + the structural
    // shape (Initial successfully decrypted → JA3
    // populated), not on the SNI value itself.
    let inner_target_name = "lakko-masque-inner-target.example";

    let cfg = MasqueConfig {
        proxy_url: "https://127.0.0.1:8443/".to_owned(),
        inner_target: format!("{inner_target_name}:443"),
        bearer: None,
    };
    let transport = MasqueTransport::new(cfg).expect("MasqueConfig must validate");

    // The connect will time out / error — observer is not
    // a real h3 server, so the QUIC handshake never
    // completes. We just need the Initial bytes on the
    // wire, which happens IMMEDIATELY after connect()
    // calls quinn's `endpoint.connect(...)`.
    let _ = tokio::time::timeout(
        Duration::from_secs(8),
        transport.connect(&Endpoint::Hostname {
            host: "127.0.0.1".into(),
            port: 8443,
        }),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let log_path = observer_log_path();
    let body = read_log(&log_path).await;

    // Walk lines tail-first, find the first udp line where
    // the QUIC Initial was successfully decrypted (JA3
    // present). A null-ja3 udp line means the observer saw
    // the datagram but failed to decrypt as QUIC v1 — we
    // need a positive decrypt to assert the negative-space
    // claim against the cleartext frames.
    let line = body
        .lines()
        .rev()
        .find(|line| line.contains(r#""transport":"udp""#) && !line.contains(r#""ja3":null"#))
        .unwrap_or_else(|| {
            panic!(
                "observer log must carry a udp line with non-null ja3 (successful QUIC Initial \
                 decrypt).\nTail of {}:\n{}",
                log_path.display(),
                body.lines().rev().take(8).collect::<Vec<_>>().join("\n"),
            )
        });

    // The inner target name MUST NOT appear anywhere on
    // the observed line. MASQUE wraps it inside a
    // CONNECT-UDP request payload that the proxy unseals
    // AFTER the QUIC + h3 handshake completes — the inner
    // never rides in cleartext on the QUIC Initial.
    assert!(
        !line.contains(inner_target_name),
        "MASQUE connection MUST NOT leak the inner target {inner_target_name:?} in the udp \
         observed line. Full line: {line}",
    );

    // Shape: ja3 + ja4 populated (it IS valid TLS-over-QUIC
    // — the same browser-class fingerprint quinn emits for
    // any HTTPS site over h3).
    let ja3 =
        extract_string_field(line, "ja3").unwrap_or_else(|| panic!("line missing ja3: {line}"));
    assert!(
        ja3 != "null" && ja3.len() == 32,
        "MASQUE QUIC Initial must yield a non-null 32-char JA3. ja3: {ja3:?}",
    );
    let ja4 =
        extract_string_field(line, "ja4").unwrap_or_else(|| panic!("line missing ja4: {line}"));
    let ja4_parts: Vec<&str> = ja4.split('_').collect();
    assert_eq!(
        ja4_parts.len(),
        3,
        "JA4 must have 3 underscore-separated parts: {ja4}",
    );
    // Note: ja4 prefix starts with `t` for TCP — but the
    // observer's parser doesn't currently distinguish QUIC
    // from TCP at the JA4 prefix level (would need
    // protocol-aware fingerprinting per FoxIO JA4Q spec).
    // We assert only the structural shape here; classifying
    // as `q13d...` vs `t13d...` is a deferred fidelity
    // improvement.
}

/// WebTransport — same posture as MASQUE: the URL host is
/// the SNI, the URL path is sent only after handshake. No
/// path / inner-target leaks here either.
#[tokio::test]
#[ignore = "requires ci/censorship-lab docker compose stack — see ci/censorship-lab/README.md"]
async fn observer_records_webtransport_outer_sni() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 127.0.0.1 directly — same IPv4-only-Docker-publish
    // rationale as the MASQUE test above.
    let inner_path_name = "lakko-wt-inner-path-marker";

    let cfg = WebTransportConfig {
        url: format!("https://127.0.0.1:8443/{inner_path_name}"),
        cert_hashes_b64: vec![],
        ech_config_list_b64: None,
    };
    let transport = WebTransportPt::new(cfg).expect("WebTransportConfig must validate");

    let _ = tokio::time::timeout(
        Duration::from_secs(8),
        transport.connect(&Endpoint::Hostname {
            host: "127.0.0.1".into(),
            port: 8443,
        }),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let log_path = observer_log_path();
    let body = read_log(&log_path).await;

    let line = body
        .lines()
        .rev()
        .find(|line| line.contains(r#""transport":"udp""#) && !line.contains(r#""ja3":null"#))
        .unwrap_or_else(|| {
            panic!(
                "observer log must carry a udp line with non-null ja3 (successful QUIC Initial \
                 decrypt).\nTail of {}:\n{}",
                log_path.display(),
                body.lines().rev().take(8).collect::<Vec<_>>().join("\n"),
            )
        });

    // URL path MUST NOT be on the wire in cleartext.
    assert!(
        !line.contains(inner_path_name),
        "WebTransport connection MUST NOT leak the URL path marker {inner_path_name:?} in \
         the udp observed line. Full line: {line}",
    );

    let ja3 =
        extract_string_field(line, "ja3").unwrap_or_else(|| panic!("line missing ja3: {line}"));
    assert!(
        ja3 != "null" && ja3.len() == 32,
        "WebTransport QUIC Initial must yield a non-null 32-char JA3. ja3: {ja3:?}",
    );
}

/// Helper — read the whole observer log file. Used by the
/// MASQUE + WebTransport tests above; the older tests have
/// their own inline `tokio::fs::File::open` + `read_to_string`
/// pair, retained for readability.
async fn read_log(path: &std::path::Path) -> String {
    let mut file = tokio::fs::File::open(path)
        .await
        .unwrap_or_else(|e| panic!("open observer log {}: {e}", path.display()));
    let mut body = String::new();
    file.read_to_string(&mut body).await.expect("read log");
    body
}
