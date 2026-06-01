// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Passive TLS-SNI + JA3/JA4 fingerprint observer for the
//! G5-7 censorship lab.
//!
//! What this is
//! ============
//!
//! A small TCP listener that mimics what a state-level
//! adversary sees on the first packet of a TLS connection:
//! the unencrypted ClientHello. For each inbound TCP
//! connection it
//!
//! 1. Reads up to 16 KiB of the first inbound payload.
//! 2. Parses out the TLS record + handshake + ClientHello
//!    layers (no `rustls` — we want the raw wire view, not
//!    rustls's already-validated picture).
//! 3. Walks ClientHello to extract
//!    - `server_name` extension (RFC 6066 §3) → SNI
//!    - JA3 fingerprint (Salesforce 2017 spec) — MD5 over
//!      `version,ciphers,extensions,groups,ec_point_formats`
//!    - JA4 fingerprint (FoxIO 2023 spec, compact form) —
//!      `t13d{nc}{ne}{alpn}_{hash_a}_{hash_b}`
//! 4. Appends one JSON line to `$LAKKO_SNI_LOG`
//!    (default `/var/log/sni-observer.jsonl`) of the shape
//!    `{"ts_ms":..,"observed_sni":"..","ja3":"..",
//!      "ja3_string":"..","ja4":"..","peer":".."}`.
//! 5. Closes the TCP connection.
//!
//! The shape is intentionally minimal so the matching
//! integration test
//! (`crates/lakko-transport/tests/sni_observer_e2e.rs`)
//! can tail the file and assert on the property
//! "transport X leaks SNI / fingerprint Y to a passive on-
//! path observer". Used as a CI gate: REALITY MUST leak the
//! cover SNI + match a popular browser's JA3/JA4;
//! Shadowsocks-2022 MUST leak nothing parseable as TLS;
//! HTTP transport leaks the real SNI and is the negative
//! control.
//!
//! What this is **not**
//! ====================
//!
//! Not a proxy — the connection is dropped after the
//! observation, the bytes are never forwarded. This is
//! the simplest auditable shape: an adversary who poisoned
//! the observer cannot induce a false-pass by changing
//! what the upstream returns because there is no
//! upstream.
//!
//! Not a real GFW / TSPU / IR emulator — those land as
//! follow-up sub-sprints documented in
//! `ci/censorship-lab/README.md` ("Capability matrix").
//! What this one observer demonstrates is the lab
//! **infrastructure pattern** that the follow-ups reuse.

#![forbid(unsafe_code)]

mod quic_initial;

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use md5::{Digest as Md5Digest, Md5};
use sha2::{Digest as Sha2Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Max bytes we read from the first inbound TCP packet
/// before declaring "no TLS ClientHello fits in this".
/// 16 KiB matches the largest TLS record size — anything
/// past that is either a fragmented handshake we don't
/// care to reassemble or non-TLS noise.
const MAX_CLIENT_HELLO_BYTES: usize = 16 * 1024;

/// How long to wait for the client to send its first
/// bytes before giving up. Real WebRTC / HTTPS clients
/// send within milliseconds; 5 s is generous enough for
/// CI runners under load.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let bind: SocketAddr = std::env::var("LAKKO_SNI_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8443".to_owned())
        .parse()
        .context("LAKKO_SNI_BIND must be a valid SocketAddr")?;
    let log_path: PathBuf = std::env::var("LAKKO_SNI_LOG")
        .unwrap_or_else(|_| "/var/log/sni-observer.jsonl".to_owned())
        .into();

    info!(%bind, log = %log_path.display(), "sni-observer starting");

    // Open the log file once at boot so we fail loud if
    // the mounted volume is read-only or missing.
    let log_file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await
        .with_context(|| format!("open log {}", log_path.display()))?;
    let log = Arc::new(Mutex::new(log_file));

    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;

    // G5-7-transport-matrix-iter3 — UDP listener on the
    // same port for QUIC Initial observations. Independent
    // of the TCP path; a single concurrent loop is enough
    // (Initial packets are not high-rate in the CI lab).
    // Bind defaults to the same port as TCP so an operator
    // who only thinks in "the observer port" gets both
    // halves without further config.
    let udp_bind: SocketAddr = std::env::var("LAKKO_SNI_UDP_BIND")
        .unwrap_or_else(|_| bind.to_string())
        .parse()
        .context("LAKKO_SNI_UDP_BIND must be a valid SocketAddr")?;
    let udp_socket = tokio::net::UdpSocket::bind(udp_bind)
        .await
        .with_context(|| format!("udp bind {udp_bind}"))?;
    info!(%udp_bind, "sni-observer udp listener up");

    let udp_log = log.clone();
    tokio::spawn(async move {
        if let Err(e) = run_udp_loop(udp_socket, udp_log).await {
            warn!(error = %e, "udp loop exited");
        }
    });

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let log = log.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_one(stream, peer, log).await {
                        warn!(%peer, error = %e, "observation failed");
                    }
                });
            },
            Err(e) => {
                warn!(error = %e, "accept failed");
            },
        }
    }
}

/// Single-task UDP loop. For each inbound datagram, attempt
/// to decrypt as a QUIC v1 Initial packet, then feed the
/// embedded handshake bytes to the same `parse_client_hello
/// _from_handshake` the TCP path uses. Emits one JSONL line
/// per datagram regardless of decrypt success — on failure
/// `observed_sni` is `null` and `first_bytes_hex` carries
/// the leading bytes for diagnostics.
async fn run_udp_loop(
    socket: tokio::net::UdpSocket,
    log: Arc<Mutex<tokio::fs::File>>,
) -> Result<()> {
    // UDP datagrams in QUIC v1 are capped at the path MTU
    // (~1500 typical, ~9000 jumbo). 64 KiB is the wire
    // ceiling and keeps the buffer one allocation.
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let (n, peer) = match socket.recv_from(&mut buf).await {
            Ok(pair) => pair,
            Err(e) => {
                warn!(error = %e, "udp recv_from failed");
                continue;
            },
        };
        let datagram = &buf[..n];

        let observed = quic_initial::decrypt_initial(datagram)
            .ok()
            .and_then(|d| parse_client_hello_from_handshake(&d.handshake_bytes));
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let line = match &observed {
            Some(ch) => format!(
                "{{\"ts_ms\":{},\"transport\":\"udp\",\"observed_sni\":{},\"ja3\":\"{}\",\"ja3_string\":\"{}\",\"ja4\":\"{}\",\"peer\":\"{}\",\"first_bytes_hex\":\"{}\"}}\n",
                ts_ms,
                match &ch.sni {
                    Some(name) => format!("\"{}\"", escape_json(name)),
                    None => "null".to_owned(),
                },
                ch.ja3,
                escape_json(&ch.ja3_string),
                ch.ja4,
                peer,
                hex_head(datagram, 16),
            ),
            None => format!(
                // QUIC Initial decryption failed (or the
                // datagram is not QUIC v1 at all). Log the
                // negative observation; first_bytes_hex
                // disambiguates "non-QUIC noise" from "QUIC
                // we failed to decrypt".
                "{{\"ts_ms\":{},\"transport\":\"udp\",\"observed_sni\":null,\"ja3\":null,\"ja3_string\":null,\"ja4\":null,\"peer\":\"{}\",\"first_bytes_hex\":\"{}\"}}\n",
                ts_ms,
                peer,
                hex_head(datagram, 16),
            ),
        };

        {
            let mut log = log.lock().await;
            log.write_all(line.as_bytes()).await?;
            log.flush().await?;
        }

        info!(
            %peer,
            transport = "udp",
            observed_sni = ?observed.as_ref().and_then(|c| c.sni.clone()),
            ja3 = ?observed.as_ref().map(|c| &c.ja3),
            ja4 = ?observed.as_ref().map(|c| &c.ja4),
            "observed",
        );
    }
}

/// Drain the first inbound packet of one TCP connection,
/// extract the SNI + JA3 + JA4, log them, drop the
/// connection.
async fn handle_one(
    mut stream: TcpStream,
    peer: SocketAddr,
    log: Arc<Mutex<tokio::fs::File>>,
) -> Result<()> {
    let mut buf = vec![0u8; MAX_CLIENT_HELLO_BYTES];
    // tokio's `timeout` wraps the entire read; an idle
    // client just sits there past the budget and we drop.
    let n = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut buf))
        .await
        .map_err(|_| anyhow!("read timed out after {READ_TIMEOUT:?}"))?
        .context("read first packet")?;
    if n == 0 {
        return Err(anyhow!("client closed before sending data"));
    }
    buf.truncate(n);

    let observed = parse_client_hello(&buf);
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let line = match &observed {
        Some(ch) => format!(
            "{{\"ts_ms\":{},\"transport\":\"tcp\",\"observed_sni\":{},\"ja3\":\"{}\",\"ja3_string\":\"{}\",\"ja4\":\"{}\",\"peer\":\"{}\",\"first_bytes_hex\":\"{}\"}}\n",
            ts_ms,
            match &ch.sni {
                Some(name) => format!("\"{}\"", escape_json(name)),
                None => "null".to_owned(),
            },
            ch.ja3,
            escape_json(&ch.ja3_string),
            ch.ja4,
            peer,
            hex_head(&buf, 16),
        ),
        None => format!(
            // No parseable ClientHello — log the negative
            // observation so the test harness can
            // distinguish "non-TLS" from "TLS we failed to
            // parse". `first_bytes_hex` is the disambiguator.
            "{{\"ts_ms\":{},\"transport\":\"tcp\",\"observed_sni\":null,\"ja3\":null,\"ja3_string\":null,\"ja4\":null,\"peer\":\"{}\",\"first_bytes_hex\":\"{}\"}}\n",
            ts_ms,
            peer,
            hex_head(&buf, 16),
        ),
    };

    {
        let mut log = log.lock().await;
        log.write_all(line.as_bytes()).await?;
        log.flush().await?;
    }

    info!(
        %peer,
        observed_sni = ?observed.as_ref().and_then(|c| c.sni.clone()),
        ja3 = ?observed.as_ref().map(|c| &c.ja3),
        ja4 = ?observed.as_ref().map(|c| &c.ja4),
        "observed",
    );

    // Best-effort half-close so the client sees a clean
    // FIN rather than an RST. Errors here are not
    // actionable — the connection is being dropped anyway.
    let _ = stream.shutdown().await;
    Ok(())
}

/// Everything a passive observer can pull out of one TLS
/// ClientHello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHelloFingerprint {
    /// SNI host_name (RFC 6066 §3), or `None` if absent.
    pub sni: Option<String>,
    /// JA3 raw string — `version,ciphers,extensions,groups,points`
    /// (decimal CSV per field). Useful for human
    /// inspection + corpus matching against published
    /// JA3 string libraries.
    pub ja3_string: String,
    /// JA3 fingerprint — lowercase hex of MD5(`ja3_string`).
    /// 32 chars.
    pub ja3: String,
    /// JA4 fingerprint (FoxIO 2023 compact form).
    /// Shape: `<prefix>_<hash_a>_<hash_b>`, e.g.
    /// `t13d1516h2_8daaf6152771_e5627efa2ab1`.
    pub ja4: String,
}

/// Parse the first SNI host_name + JA3/JA4 fingerprints
/// out of a TLS ClientHello packet. Returns `None` if the
/// bytes don't look like TLS.
///
/// Wire layout we walk (no rustls here on purpose — we
/// want the unvalidated view a passive observer sees):
///
/// ```text
/// TLSPlaintext         RFC 8446 §5.1
///   type          u8   (0x16 = handshake)
///   version       u16  (0x0301 .. 0x0304)
///   length        u16
///   fragment      [u8; length]
/// Handshake            RFC 8446 §4
///   msg_type      u8   (0x01 = ClientHello)
///   length        u24
///   body          [u8]
/// ClientHello          RFC 8446 §4.1.2
///   legacy_version           u16
///   random                   [u8; 32]
///   legacy_session_id_len    u8   + bytes
///   cipher_suites_len        u16  + bytes
///   legacy_compression_len   u8   + bytes
///   extensions_len           u16
///   extensions               [Extension]
/// ```
pub fn parse_client_hello(buf: &[u8]) -> Option<ClientHelloFingerprint> {
    let mut p = Parser::new(buf);

    // TLSPlaintext header
    let record_type = p.u8()?;
    if record_type != 0x16 {
        return None; // not a handshake record
    }
    let _legacy_record_version = p.u16()?;
    let _record_len = p.u16()?;

    // Handshake header + ClientHello body — delegate to the
    // record-less variant so QUIC Initial's CRYPTO frame
    // payload (which carries the handshake message WITHOUT
    // the TLS record wrapper) can share the same parser.
    parse_client_hello_from_handshake(&buf[5..])
}

/// Parse a ClientHello from a buffer that begins with the
/// **handshake message header** (NOT the TLS record header).
/// QUIC Initial packets carry the ClientHello inside a
/// CRYPTO frame whose payload is `handshake_type (u8) +
/// length (u24) + body`; the outer TLS record layer is
/// absent because QUIC's own packet protection replaces it.
///
/// This is the inner half of [`parse_client_hello`]; the
/// outer half strips the record header and calls into here.
pub fn parse_client_hello_from_handshake(buf: &[u8]) -> Option<ClientHelloFingerprint> {
    let mut p = Parser::new(buf);

    // Handshake header
    let msg_type = p.u8()?;
    if msg_type != 0x01 {
        return None; // not ClientHello
    }
    let _hs_len = p.u24()?;

    // ClientHello body
    let legacy_client_version = p.u16()?;
    let _random = p.bytes(32)?;
    let session_id_len = p.u8()? as usize;
    let _session_id = p.bytes(session_id_len)?;

    let cipher_suites_len = p.u16()? as usize;
    let cipher_bytes = p.bytes(cipher_suites_len)?;
    if cipher_bytes.len() % 2 != 0 {
        return None;
    }
    let cipher_suites: Vec<u16> = cipher_bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();

    let compression_len = p.u8()? as usize;
    let _compression = p.bytes(compression_len)?;

    let extensions_len = p.u16()? as usize;
    let extensions_buf = p.bytes(extensions_len)?;

    // Walk extensions once to capture order + materialise
    // the fields JA3 / JA4 need.
    let mut sni: Option<String> = None;
    let mut ext_types: Vec<u16> = Vec::new();
    let mut supported_groups: Vec<u16> = Vec::new();
    let mut ec_point_formats: Vec<u8> = Vec::new();
    let mut supported_versions: Vec<u16> = Vec::new();
    let mut signature_algorithms: Vec<u16> = Vec::new();
    let mut first_alpn: Option<Vec<u8>> = None;

    let mut ep = Parser::new(extensions_buf);
    while !ep.is_empty() {
        let ext_type = ep.u16()?;
        let ext_len = ep.u16()? as usize;
        let ext_data = ep.bytes(ext_len)?;
        ext_types.push(ext_type);

        match ext_type {
            // server_name (RFC 6066 §3)
            0x0000 => {
                let mut sp = Parser::new(ext_data);
                if let Some(list_len) = sp.u16().map(|v| v as usize) {
                    if let Some(list) = sp.bytes(list_len) {
                        let mut lp = Parser::new(list);
                        while !lp.is_empty() {
                            let Some(name_type) = lp.u8() else { break };
                            let Some(name_len) = lp.u16().map(|v| v as usize) else { break };
                            let Some(name) = lp.bytes(name_len) else { break };
                            if name_type == 0x00 {
                                sni = std::str::from_utf8(name).ok().map(str::to_owned);
                                break;
                            }
                        }
                    }
                }
            },
            // supported_groups (RFC 8422 §5.1.1 / RFC 8446 §4.2.7)
            0x000a => {
                let mut sp = Parser::new(ext_data);
                if let Some(list_len) = sp.u16().map(|v| v as usize) {
                    if let Some(list) = sp.bytes(list_len) {
                        if list.len() % 2 == 0 {
                            supported_groups = list
                                .chunks_exact(2)
                                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                                .collect();
                        }
                    }
                }
            },
            // ec_point_formats (RFC 8422 §5.1.2) — deprecated
            // in TLS 1.3 but JA3 still includes it when present.
            0x000b => {
                let mut sp = Parser::new(ext_data);
                if let Some(list_len) = sp.u8().map(|v| v as usize) {
                    if let Some(list) = sp.bytes(list_len) {
                        ec_point_formats = list.to_vec();
                    }
                }
            },
            // signature_algorithms (RFC 8446 §4.2.3)
            0x000d => {
                let mut sp = Parser::new(ext_data);
                if let Some(list_len) = sp.u16().map(|v| v as usize) {
                    if let Some(list) = sp.bytes(list_len) {
                        if list.len() % 2 == 0 {
                            signature_algorithms = list
                                .chunks_exact(2)
                                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                                .collect();
                        }
                    }
                }
            },
            // alpn (RFC 7301)
            0x0010 => {
                let mut sp = Parser::new(ext_data);
                if let Some(list_len) = sp.u16().map(|v| v as usize) {
                    if let Some(list) = sp.bytes(list_len) {
                        let mut lp = Parser::new(list);
                        if let Some(alpn_len) = lp.u8().map(|v| v as usize) {
                            if let Some(alpn) = lp.bytes(alpn_len) {
                                first_alpn = Some(alpn.to_vec());
                            }
                        }
                    }
                }
            },
            // supported_versions (RFC 8446 §4.2.1) — TLS 1.3
            // negotiation marker; if 0x0304 is in here, JA4
            // version field is `13`.
            0x002b => {
                let mut sp = Parser::new(ext_data);
                if let Some(list_len) = sp.u8().map(|v| v as usize) {
                    if let Some(list) = sp.bytes(list_len) {
                        if list.len() % 2 == 0 {
                            supported_versions = list
                                .chunks_exact(2)
                                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                                .collect();
                        }
                    }
                }
            },
            _ => {},
        }
    }

    let (ja3_string, ja3) = compute_ja3(
        legacy_client_version,
        &cipher_suites,
        &ext_types,
        &supported_groups,
        &ec_point_formats,
    );

    let ja4 = compute_ja4(
        &supported_versions,
        legacy_client_version,
        sni.is_some(),
        &cipher_suites,
        &ext_types,
        &signature_algorithms,
        first_alpn.as_deref(),
    );

    Some(ClientHelloFingerprint {
        sni,
        ja3_string,
        ja3,
        ja4,
    })
}

/// Backward-compat wrapper — kept so the existing tests
/// that only care about the SNI field keep their shape.
pub fn parse_sni(buf: &[u8]) -> Option<String> {
    parse_client_hello(buf).and_then(|c| c.sni)
}

/// GREASE-value check (RFC 8701). Cipher suites,
/// extensions, named groups, signature algorithms, and ALPN
/// protocol IDs may carry GREASE values of the form
/// `0x{a}a{a}a` for `a` in `0..=15`. Passive observers
/// strip them before fingerprinting so the JA3 / JA4 is
/// stable across handshakes.
fn is_grease(v: u16) -> bool {
    let hi = (v >> 8) as u8;
    let lo = (v & 0xff) as u8;
    // Pattern: 0x{n}a{n}a where the two halves match AND
    // the low nibble of each half is `a` (0x0a / 0x1a /
    // 0x2a / ... / 0xfa).
    hi == lo && (lo & 0x0f) == 0x0a
}

/// JA3 (Salesforce 2017). Raw string:
/// `version,ciphers,extensions,groups,ec_point_formats`.
/// Each field is decimal-comma-joined; GREASE values are
/// stripped from ciphers + extensions + groups (ec_point_
/// formats has no GREASE codepoints in practice). The
/// MD5 of the raw string, lowercase hex, is JA3.
fn compute_ja3(
    legacy_client_version: u16,
    cipher_suites: &[u16],
    ext_types: &[u16],
    supported_groups: &[u16],
    ec_point_formats: &[u8],
) -> (String, String) {
    let mut s = String::new();
    let _ = write!(s, "{legacy_client_version}");
    s.push(',');
    join_decimal_csv_u16(&mut s, cipher_suites.iter().copied().filter(|v| !is_grease(*v)));
    s.push(',');
    join_decimal_csv_u16(&mut s, ext_types.iter().copied().filter(|v| !is_grease(*v)));
    s.push(',');
    join_decimal_csv_u16(&mut s, supported_groups.iter().copied().filter(|v| !is_grease(*v)));
    s.push(',');
    join_decimal_csv_u8(&mut s, ec_point_formats.iter().copied());

    let mut hasher = Md5::new();
    hasher.update(s.as_bytes());
    let md5_bytes = hasher.finalize();
    let mut md5_hex = String::with_capacity(32);
    for b in md5_bytes {
        let _ = write!(md5_hex, "{b:02x}");
    }
    (s, md5_hex)
}

/// JA4 (FoxIO 2023, compact form). Shape:
/// `<prefix>_<hash_a>_<hash_b>` where:
///
/// * `prefix` = `t<version_2><sni_char><nc_2digit><ne_2digit><alpn_2char>`
///   - `t` — TCP (we are a TCP listener).
///   - `version_2` — `13` if supported_versions advertises
///     TLS 1.3, else last two digits of legacy_version.
///   - `sni_char` — `d` if SNI host_name present, else `i`.
///   - `nc_2` — count of non-GREASE ciphers, 2 digits,
///     capped at 99.
///   - `ne_2` — count of non-GREASE extensions, 2 digits,
///     capped at 99.
///   - `alpn_2` — first + last char of first ALPN
///     protocol, or `00` if no ALPN. For ASCII (the only
///     thing real ALPNs use) we lowercase.
///
/// * `hash_a` = SHA256(sorted unique non-GREASE ciphers in
///   `,`-joined lowercase 4-char hex), take first 12 hex
///   chars.
///
/// * `hash_b` = SHA256(
///     sorted unique non-GREASE extensions in `,`-joined
///     lowercase 4-char hex, EXCLUDING `0x0000` (SNI) and
///     `0x0010` (ALPN); then `_`; then signature_algorithms
///     in **observed order** joined with `,`
///   ), take first 12 hex chars.
fn compute_ja4(
    supported_versions: &[u16],
    legacy_client_version: u16,
    sni_present: bool,
    cipher_suites: &[u16],
    ext_types: &[u16],
    signature_algorithms: &[u16],
    first_alpn: Option<&[u8]>,
) -> String {
    // --- version_2 ---
    let version_2 = ja4_version_str(supported_versions, legacy_client_version);

    // --- sni_char ---
    let sni_char = if sni_present { 'd' } else { 'i' };

    // --- nc_2 / ne_2 ---
    let non_grease_ciphers: Vec<u16> = cipher_suites
        .iter()
        .copied()
        .filter(|v| !is_grease(*v))
        .collect();
    let non_grease_exts: Vec<u16> = ext_types
        .iter()
        .copied()
        .filter(|v| !is_grease(*v))
        .collect();
    let nc_2 = format!("{:02}", non_grease_ciphers.len().min(99));
    let ne_2 = format!("{:02}", non_grease_exts.len().min(99));

    // --- alpn_2 ---
    let alpn_2 = match first_alpn {
        Some(bytes) if !bytes.is_empty() => {
            // Take the first + last ASCII byte. If the
            // ALPN protocol has only one byte, repeat it.
            // Non-printable / non-ASCII bytes fall back
            // to '0' so the field is always 2 chars.
            let first = ascii_or_zero(bytes[0]);
            let last = ascii_or_zero(*bytes.last().unwrap());
            let mut s = String::with_capacity(2);
            s.push(first);
            s.push(last);
            s
        },
        _ => "00".to_owned(),
    };

    let prefix = format!("t{version_2}{sni_char}{nc_2}{ne_2}{alpn_2}");

    // --- hash_a ---
    let mut ciphers_sorted = non_grease_ciphers.clone();
    ciphers_sorted.sort_unstable();
    ciphers_sorted.dedup();
    let cipher_str = ciphers_sorted
        .iter()
        .map(|c| format!("{c:04x}"))
        .collect::<Vec<_>>()
        .join(",");
    let hash_a = sha256_first_12_hex(cipher_str.as_bytes());

    // --- hash_b ---
    // Sorted unique extensions excluding SNI (0x0000) and
    // ALPN (0x0010) — the JA4 spec explicitly removes
    // these two from the sort to keep the fingerprint
    // stable across servers that vary their SNI behaviour
    // or ALPN list.
    let mut ext_sorted: Vec<u16> = non_grease_exts
        .iter()
        .copied()
        .filter(|t| *t != 0x0000 && *t != 0x0010)
        .collect();
    ext_sorted.sort_unstable();
    ext_sorted.dedup();
    let ext_str = ext_sorted
        .iter()
        .map(|e| format!("{e:04x}"))
        .collect::<Vec<_>>()
        .join(",");
    let sig_str = signature_algorithms
        .iter()
        .map(|s| format!("{s:04x}"))
        .collect::<Vec<_>>()
        .join(",");
    let hash_b_input = format!("{ext_str}_{sig_str}");
    let hash_b = sha256_first_12_hex(hash_b_input.as_bytes());

    format!("{prefix}_{hash_a}_{hash_b}")
}

/// JA4 version slot — `13` when supported_versions
/// advertises TLS 1.3 (0x0304), else the last two digits
/// of the legacy_version (decimal). e.g. 0x0303 → `12`,
/// 0x0302 → `11`, 0x0301 → `10`.
fn ja4_version_str(supported_versions: &[u16], legacy_client_version: u16) -> &'static str {
    if supported_versions
        .iter()
        .any(|v| *v == 0x0304 && !is_grease(*v))
    {
        return "13";
    }
    match legacy_client_version {
        0x0304 => "13",
        0x0303 => "12",
        0x0302 => "11",
        0x0301 => "10",
        0x0300 => "s3",
        _ => "00",
    }
}

/// ASCII-printable chars only; lowercased; everything else
/// → '0'. Used by the JA4 prefix's `alpn_2` field.
fn ascii_or_zero(b: u8) -> char {
    let c = b as char;
    if c.is_ascii_alphanumeric() {
        c.to_ascii_lowercase()
    } else {
        '0'
    }
}

fn join_decimal_csv_u16(out: &mut String, mut iter: impl Iterator<Item = u16>) {
    if let Some(first) = iter.next() {
        let _ = write!(out, "{first}");
        for v in iter {
            out.push('-');
            let _ = write!(out, "{v}");
        }
    }
}

fn join_decimal_csv_u8(out: &mut String, mut iter: impl Iterator<Item = u8>) {
    if let Some(first) = iter.next() {
        let _ = write!(out, "{first}");
        for v in iter {
            out.push('-');
            let _ = write!(out, "{v}");
        }
    }
}

fn sha256_first_12_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(12);
    for b in &digest[..6] {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Tiny byte cursor — split out so the parser stays
/// linear and obviously correct.
struct Parser<'a> {
    buf: &'a [u8],
}

impl<'a> Parser<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn u8(&mut self) -> Option<u8> {
        let (head, rest) = self.buf.split_first()?;
        self.buf = rest;
        Some(*head)
    }

    fn u16(&mut self) -> Option<u16> {
        if self.buf.len() < 2 {
            return None;
        }
        let v = u16::from_be_bytes([self.buf[0], self.buf[1]]);
        self.buf = &self.buf[2..];
        Some(v)
    }

    fn u24(&mut self) -> Option<u32> {
        if self.buf.len() < 3 {
            return None;
        }
        let v = u32::from_be_bytes([0, self.buf[0], self.buf[1], self.buf[2]]);
        self.buf = &self.buf[3..];
        Some(v)
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.buf.len() < n {
            return None;
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Some(head)
    }
}

/// Hex-encode the first `n` bytes of `buf` as lowercase
/// pairs, no separators. Used only for the diagnostic
/// `first_bytes_hex` field in the log JSON.
fn hex_head(buf: &[u8], n: usize) -> String {
    let take = buf.len().min(n);
    let mut out = String::with_capacity(take * 2);
    for b in &buf[..take] {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Minimal JSON string escape — enough for valid SNIs +
/// JA3 raw strings (which only contain digits, commas, and
/// a few dashes, but defensive against UTF-8 surprises).
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            },
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-rolled ClientHello fixture so any drift
    /// surfaces in `git diff` as a real review item.
    /// Carries enough fields to make the JA3 / JA4
    /// computation non-trivial.
    fn fixture_clienthello_example_com() -> Vec<u8> {
        let mut hs = Vec::new();
        hs.push(0x01); // ClientHello
        let body_start = hs.len();
        hs.extend_from_slice(&[0; 3]); // hs length placeholder

        // client_version = 0x0303 (TLS 1.2 legacy)
        hs.extend_from_slice(&[0x03, 0x03]);
        hs.extend_from_slice(&[0u8; 32]); // random
        hs.push(0); // session_id_len = 0

        // cipher_suites: TLS_AES_128_GCM_SHA256 (0x1301)
        //              + TLS_CHACHA20_POLY1305_SHA256 (0x1303)
        //              + one GREASE value to verify the filter
        hs.extend_from_slice(&[0x00, 0x06]); // cipher_suites_len
        hs.extend_from_slice(&[0x1a, 0x1a]); // GREASE
        hs.extend_from_slice(&[0x13, 0x01]);
        hs.extend_from_slice(&[0x13, 0x03]);

        hs.push(0x01);
        hs.push(0x00); // compression_methods: null

        // extensions
        let mut exts = Vec::new();

        // server_name: example.com (ext_type 0x0000)
        exts.extend_from_slice(&[0x00, 0x00]);
        let host = b"example.com";
        let mut sn_list = Vec::new();
        sn_list.push(0x00);
        sn_list.extend_from_slice(&(host.len() as u16).to_be_bytes());
        sn_list.extend_from_slice(host);
        let mut sn_data = Vec::new();
        sn_data.extend_from_slice(&(sn_list.len() as u16).to_be_bytes());
        sn_data.extend_from_slice(&sn_list);
        exts.extend_from_slice(&(sn_data.len() as u16).to_be_bytes());
        exts.extend_from_slice(&sn_data);

        // supported_groups (0x000a): X25519 (0x001d) + P-256 (0x0017)
        exts.extend_from_slice(&[0x00, 0x0a]);
        let groups: [u8; 4] = [0x00, 0x1d, 0x00, 0x17];
        let mut sg_data = Vec::new();
        sg_data.extend_from_slice(&(groups.len() as u16).to_be_bytes());
        sg_data.extend_from_slice(&groups);
        exts.extend_from_slice(&(sg_data.len() as u16).to_be_bytes());
        exts.extend_from_slice(&sg_data);

        // ec_point_formats (0x000b): uncompressed only
        exts.extend_from_slice(&[0x00, 0x0b]);
        let ec_data: [u8; 2] = [0x01, 0x00]; // list_len=1, format=0 (uncompressed)
        exts.extend_from_slice(&(ec_data.len() as u16).to_be_bytes());
        exts.extend_from_slice(&ec_data);

        // signature_algorithms (0x000d): ecdsa_secp256r1_sha256 (0x0403)
        //                              + rsa_pss_rsae_sha256 (0x0804)
        exts.extend_from_slice(&[0x00, 0x0d]);
        let sigs: [u8; 4] = [0x04, 0x03, 0x08, 0x04];
        let mut sa_data = Vec::new();
        sa_data.extend_from_slice(&(sigs.len() as u16).to_be_bytes());
        sa_data.extend_from_slice(&sigs);
        exts.extend_from_slice(&(sa_data.len() as u16).to_be_bytes());
        exts.extend_from_slice(&sa_data);

        // alpn (0x0010): h2 + http/1.1
        exts.extend_from_slice(&[0x00, 0x10]);
        let mut alpn_inner = Vec::new();
        alpn_inner.push(0x02);
        alpn_inner.extend_from_slice(b"h2");
        alpn_inner.push(0x08);
        alpn_inner.extend_from_slice(b"http/1.1");
        let mut alpn_data = Vec::new();
        alpn_data.extend_from_slice(&(alpn_inner.len() as u16).to_be_bytes());
        alpn_data.extend_from_slice(&alpn_inner);
        exts.extend_from_slice(&(alpn_data.len() as u16).to_be_bytes());
        exts.extend_from_slice(&alpn_data);

        // supported_versions (0x002b): TLS 1.3 (0x0304)
        exts.extend_from_slice(&[0x00, 0x2b]);
        let sv: [u8; 3] = [0x02, 0x03, 0x04];
        exts.extend_from_slice(&(sv.len() as u16).to_be_bytes());
        exts.extend_from_slice(&sv);

        hs.extend_from_slice(&(exts.len() as u16).to_be_bytes());
        hs.extend_from_slice(&exts);

        let hs_body_len = hs.len() - body_start - 3;
        hs[body_start] = (hs_body_len >> 16) as u8;
        hs[body_start + 1] = (hs_body_len >> 8) as u8;
        hs[body_start + 2] = hs_body_len as u8;

        let mut rec = Vec::new();
        rec.push(0x16);
        rec.extend_from_slice(&[0x03, 0x01]);
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn parses_example_com_sni() {
        let buf = fixture_clienthello_example_com();
        assert_eq!(parse_sni(&buf).as_deref(), Some("example.com"));
    }

    #[test]
    fn rejects_non_handshake_record() {
        let buf = [0x17, 0x03, 0x03, 0x00, 0x10, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8];
        assert!(parse_client_hello(&buf).is_none());
    }

    #[test]
    fn rejects_truncated_clienthello() {
        let buf = [0x16, 0x03, 0x01, 0x00, 0x00];
        assert!(parse_client_hello(&buf).is_none());
    }

    #[test]
    fn parses_full_fingerprint() {
        let buf = fixture_clienthello_example_com();
        let ch = parse_client_hello(&buf).expect("parse");
        assert_eq!(ch.sni.as_deref(), Some("example.com"));

        // JA3 raw string — version 771 (0x0303), ciphers
        // 4865 (0x1301) + 4867 (0x1303) (GREASE stripped),
        // extensions 0-10-11-13-16-43 (no GREASE in our
        // fixture), groups 29-23, ec-point-formats 0.
        // Field separator is `,`; intra-field separator
        // is `-`.
        assert_eq!(
            ch.ja3_string,
            "771,4865-4867,0-10-11-13-16-43,29-23,0",
        );

        // JA3 hash — 32 lowercase hex chars.
        assert_eq!(ch.ja3.len(), 32);
        assert!(ch.ja3.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        // JA4 prefix — TCP TLS 1.3 with SNI domain, 2
        // non-GREASE ciphers (`02`), 6 extensions (`06`),
        // ALPN starts with `h` ends with `2` → `h2`.
        // Final form: `t13d0206h2_<hash_a>_<hash_b>`.
        assert!(
            ch.ja4.starts_with("t13d0206h2_"),
            "unexpected JA4 prefix: {}",
            ch.ja4,
        );
        let parts: Vec<&str> = ch.ja4.split('_').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1].len(), 12);
        assert_eq!(parts[2].len(), 12);
        assert!(parts[1].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(parts[2].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn is_grease_recognises_all_16_codepoints() {
        for n in 0u8..=15u8 {
            let nibble = n << 4 | 0x0a;
            let v = u16::from_be_bytes([nibble, nibble]);
            assert!(is_grease(v), "0x{v:04x} should be GREASE");
        }
        // Common non-GREASE values must not match.
        assert!(!is_grease(0x1301));
        assert!(!is_grease(0x0000));
        assert!(!is_grease(0x002b));
        // Note: 0xaaaa IS a GREASE value (n=10); see the
        // dedicated `is_grease_pattern_matches_0xaaaa` test
        // below for that codepoint.
    }

    #[test]
    fn is_grease_pattern_matches_0xaaaa() {
        // 0xaaaa is n=10 → matches GREASE.
        assert!(is_grease(0xaaaa));
        // 0xbbbb is NOT a GREASE pattern (low nibble is
        // `b`, not `a`).
        assert!(!is_grease(0xbbbb));
    }

    #[test]
    fn ja4_falls_back_to_legacy_when_no_supported_versions() {
        // Same fixture but supported_versions is absent.
        // We hand-build a minimal CH without that ext.
        let mut hs = vec![0x01];
        let body_start = hs.len();
        hs.extend_from_slice(&[0; 3]);
        hs.extend_from_slice(&[0x03, 0x03]); // legacy 0x0303 → "12"
        hs.extend_from_slice(&[0u8; 32]);
        hs.push(0);
        hs.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // one cipher
        hs.push(0x01);
        hs.push(0x00);
        // extensions: just SNI (so sni_present=true)
        let mut exts = Vec::new();
        exts.extend_from_slice(&[0x00, 0x00]);
        let host = b"example.com";
        let mut sn_list = Vec::new();
        sn_list.push(0x00);
        sn_list.extend_from_slice(&(host.len() as u16).to_be_bytes());
        sn_list.extend_from_slice(host);
        let mut sn_data = Vec::new();
        sn_data.extend_from_slice(&(sn_list.len() as u16).to_be_bytes());
        sn_data.extend_from_slice(&sn_list);
        exts.extend_from_slice(&(sn_data.len() as u16).to_be_bytes());
        exts.extend_from_slice(&sn_data);
        hs.extend_from_slice(&(exts.len() as u16).to_be_bytes());
        hs.extend_from_slice(&exts);
        let hs_body_len = hs.len() - body_start - 3;
        hs[body_start] = (hs_body_len >> 16) as u8;
        hs[body_start + 1] = (hs_body_len >> 8) as u8;
        hs[body_start + 2] = hs_body_len as u8;
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);

        let ch = parse_client_hello(&rec).expect("parse");
        // legacy 0x0303 → "12"; SNI present → "d"; 1
        // cipher (`01`); 1 extension (`01`); no ALPN
        // (`00`). Prefix: `t12d0101_00`.
        assert!(
            ch.ja4.starts_with("t12d010100_"),
            "unexpected JA4 prefix: {}",
            ch.ja4,
        );
    }

    #[test]
    fn hex_head_truncates_and_pads() {
        assert_eq!(hex_head(b"\x00\xff", 4), "00ff");
        assert_eq!(hex_head(&[0x12, 0x34, 0x56, 0x78, 0x9a], 3), "123456");
    }

    #[test]
    fn escape_json_handles_quotes_and_backslashes() {
        assert_eq!(escape_json(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
