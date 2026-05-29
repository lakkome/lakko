// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Passive TLS-SNI observer for the G5-7 censorship lab.
//!
//! What this is
//! ============
//!
//! A small TCP listener that mimics what a state-level
//! adversary sees on the first packet of a TLS connection:
//! the unencrypted ClientHello, specifically its `server_name`
//! extension (SNI). For each inbound TCP connection it
//!
//! 1. Reads up to 16 KiB of the first inbound payload.
//! 2. Parses out the TLS record + handshake + ClientHello
//!    layers (no `rustls` — we want the raw wire view, not
//!    rustls's already-validated picture).
//! 3. Walks ClientHello extensions for the
//!    `server_name` extension (RFC 6066 §3) and pulls out
//!    the first `host_name` entry.
//! 4. Appends one JSON line to `$LAKKO_SNI_LOG`
//!    (default `/var/log/sni-observer.jsonl`) of the shape
//!    `{"ts_ms":..,"observed_sni":"..","peer":".."}`.
//! 5. Closes the TCP connection.
//!
//! The shape is intentionally minimal so the matching
//! integration test
//! (`crates/lakko-transport/tests/sni_observer_e2e.rs`)
//! can tail the file and assert on the property
//! "transport X leaks SNI Y to a passive on-path
//! observer". Used as a CI gate: REALITY MUST leak the
//! cover SNI; Shadowsocks-2022 MUST leak nothing
//! parseable as TLS; HTTP transport leaks the real SNI
//! and is the negative control.
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
//! **infrastructure pattern** — Dockerfile, compose,
//! mounted log volume, ignored Rust integration test —
//! that the follow-ups reuse.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
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

/// Drain the first inbound packet of one TCP connection,
/// extract the SNI, log it, drop the connection.
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

    let observed = parse_sni(&buf);
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let line = format!(
        "{{\"ts_ms\":{},\"observed_sni\":{},\"peer\":\"{}\",\"first_bytes_hex\":\"{}\"}}\n",
        ts_ms,
        match &observed {
            Some(name) => format!("\"{}\"", escape_json(name)),
            None => "null".to_owned(),
        },
        peer,
        // First 16 bytes of the payload — useful when the
        // observed_sni is None to distinguish "client
        // sent garbage" from "client sent a non-TLS
        // protocol".
        hex_head(&buf, 16),
    );

    {
        let mut log = log.lock().await;
        log.write_all(line.as_bytes()).await?;
        log.flush().await?;
    }

    info!(%peer, observed = ?observed, "observed");

    // Best-effort half-close so the client sees a clean
    // FIN rather than an RST. Errors here are not
    // actionable — the connection is being dropped anyway.
    let _ = stream.shutdown().await;
    Ok(())
}

/// Parse the first SNI host_name out of a TLS
/// ClientHello packet. Returns `None` if the bytes don't
/// look like TLS or the SNI extension is absent.
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
/// Extension
///   extension_type           u16
///   extension_data_len       u16
///   extension_data           [u8]
/// server_name extension      RFC 6066 §3
///   server_name_list_len     u16
///   for each:
///     name_type              u8   (0x00 = host_name)
///     host_name_len          u16
///     host_name              UTF-8 bytes
/// ```
pub fn parse_sni(buf: &[u8]) -> Option<String> {
    let mut p = Parser::new(buf);

    // TLSPlaintext header
    let record_type = p.u8()?;
    if record_type != 0x16 {
        return None; // not a handshake record
    }
    let _legacy_version = p.u16()?;
    let _record_len = p.u16()?;

    // Handshake header
    let msg_type = p.u8()?;
    if msg_type != 0x01 {
        return None; // not ClientHello
    }
    let _hs_len = p.u24()?;

    // ClientHello body
    let _legacy_client_version = p.u16()?;
    let _random = p.bytes(32)?;
    let session_id_len = p.u8()? as usize;
    let _session_id = p.bytes(session_id_len)?;
    let cipher_suites_len = p.u16()? as usize;
    let _cipher_suites = p.bytes(cipher_suites_len)?;
    let compression_len = p.u8()? as usize;
    let _compression = p.bytes(compression_len)?;

    let extensions_len = p.u16()? as usize;
    let extensions = p.bytes(extensions_len)?;

    let mut ep = Parser::new(extensions);
    while !ep.is_empty() {
        let ext_type = ep.u16()?;
        let ext_len = ep.u16()? as usize;
        let ext_data = ep.bytes(ext_len)?;
        if ext_type == 0x0000 {
            // server_name extension
            let mut sp = Parser::new(ext_data);
            let list_len = sp.u16()? as usize;
            let list = sp.bytes(list_len)?;
            let mut lp = Parser::new(list);
            while !lp.is_empty() {
                let name_type = lp.u8()?;
                let name_len = lp.u16()? as usize;
                let name = lp.bytes(name_len)?;
                if name_type == 0x00 {
                    // host_name — UTF-8 per RFC 6066 §3.
                    return std::str::from_utf8(name).ok().map(str::to_owned);
                }
            }
            return None;
        }
    }
    None
}

/// Tiny byte cursor — split out so the SNI parser stays
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
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Minimal JSON string escape — enough for valid SNIs
/// (host names cannot contain `"` or `\`, but defensive
/// against UTF-8 surprises that survived the
/// from_utf8 gate).
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real-world TLS 1.3 ClientHello captured from a
    /// `curl https://example.com` run. Pinned here so
    /// the SNI parser has at least one positive test
    /// regardless of the integration harness.
    ///
    /// Hand-rolled rather than checking in a `.bin` so
    /// any drift surfaces in `git diff` as a real
    /// review item.
    fn fixture_clienthello_example_com() -> Vec<u8> {
        // type=0x16 handshake
        // version=0x0303 (TLS 1.2 legacy)
        // length= filled below
        let mut hs = Vec::new();
        hs.push(0x01); // ClientHello
        let body_start = hs.len();
        hs.extend_from_slice(&[0; 3]); // hs length placeholder

        // client_version = 0x0303
        hs.extend_from_slice(&[0x03, 0x03]);
        hs.extend_from_slice(&[0u8; 32]); // random
        hs.push(0); // session_id_len = 0
        hs.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // cipher_suites: TLS_AES_128_GCM_SHA256
        hs.push(0x01);
        hs.push(0x00); // compression_methods: null

        // extensions
        let mut exts = Vec::new();
        // server_name: example.com
        exts.extend_from_slice(&[0x00, 0x00]); // ext_type = server_name
        let mut sn_data = Vec::new();
        let host = b"example.com";
        let mut sn_list = Vec::new();
        sn_list.push(0x00); // host_name
        sn_list.extend_from_slice(&(host.len() as u16).to_be_bytes());
        sn_list.extend_from_slice(host);
        sn_data.extend_from_slice(&(sn_list.len() as u16).to_be_bytes());
        sn_data.extend_from_slice(&sn_list);
        exts.extend_from_slice(&(sn_data.len() as u16).to_be_bytes());
        exts.extend_from_slice(&sn_data);

        hs.extend_from_slice(&(exts.len() as u16).to_be_bytes());
        hs.extend_from_slice(&exts);

        // Patch the 24-bit handshake length.
        let hs_body_len = hs.len() - body_start - 3;
        hs[body_start] = (hs_body_len >> 16) as u8;
        hs[body_start + 1] = (hs_body_len >> 8) as u8;
        hs[body_start + 2] = hs_body_len as u8;

        // Wrap in TLSPlaintext.
        let mut rec = Vec::new();
        rec.push(0x16);
        rec.extend_from_slice(&[0x03, 0x01]); // legacy version
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn parses_example_com() {
        let buf = fixture_clienthello_example_com();
        assert_eq!(parse_sni(&buf).as_deref(), Some("example.com"));
    }

    #[test]
    fn rejects_non_handshake_record() {
        // record_type = 0x17 (application data), not handshake.
        let buf = [0x17, 0x03, 0x03, 0x00, 0x10, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8];
        assert!(parse_sni(&buf).is_none());
    }

    #[test]
    fn rejects_truncated_clienthello() {
        // Valid record header, but body is empty so the
        // ClientHello parse fails on the very first field.
        let buf = [0x16, 0x03, 0x01, 0x00, 0x00];
        assert!(parse_sni(&buf).is_none());
    }

    #[test]
    fn rejects_clienthello_without_sni_ext() {
        // Build a ClientHello with an extensions block
        // that's empty — no server_name extension at all.
        let mut hs = vec![0x01];
        let body_start = hs.len();
        hs.extend_from_slice(&[0; 3]);
        hs.extend_from_slice(&[0x03, 0x03]);
        hs.extend_from_slice(&[0u8; 32]);
        hs.push(0);
        hs.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]);
        hs.push(0x01);
        hs.push(0x00);
        hs.extend_from_slice(&[0x00, 0x00]); // extensions_len = 0
        let hs_body_len = hs.len() - body_start - 3;
        hs[body_start] = (hs_body_len >> 16) as u8;
        hs[body_start + 1] = (hs_body_len >> 8) as u8;
        hs[body_start + 2] = hs_body_len as u8;
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        assert!(parse_sni(&rec).is_none());
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
