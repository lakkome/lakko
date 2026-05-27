// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Encrypted Client Hello (ECH) — `ECHConfigList` wire-
//! format parser per draft-ietf-tls-esni-18 §4.
//!
//! ECH hides the SNI in the TLS handshake by wrapping the
//! real `ClientHello` (inner) inside an HPKE-sealed
//! envelope, with an outer `ClientHello` that exposes
//! only the provider's public name (e.g.
//! `cloudflare-ech.com`). A censor on the wire sees the
//! public name; the real destination stays hidden.
//!
//! ## Why a dedicated parser
//!
//! The DNS HTTPS resource record (RR type 65) carries an
//! `ech=...` parameter whose value is an
//! [`ECHConfigList`](https://datatracker.ietf.org/doc/html/draft-ietf-tls-esni-18#section-4)
//! — opaque bytes the client MUST parse before it can
//! configure rustls. Any attacker that can spoof / poison
//! the DNS answer (or any cache between the recursive
//! resolver and the client) can deliver arbitrary bytes
//! through this parser. The parser is the gate that
//! separates attacker bytes from rustls — any panic /
//! overflow / stack abuse here is a vulnerability on the
//! anti-censorship bootstrap path.
//!
//! ## What this module ships
//!
//! - [`parse_ech_config_list`] — outer parser, returns a
//!   typed [`EchConfigList`] or a typed [`EchError`].
//! - Strict version pinning ([`ECH_VERSION_DRAFT_13`] /
//!   `0xfe0d`); the parser refuses any other version
//!   rather than silently skipping it. Forward-compat
//!   path: add the new version constant and a parsing
//!   branch.
//! - Strict bounds on every length field — the parser
//!   gives up on the first malformed envelope rather than
//!   trying to recover and risk integer overflow.
//!
//! ## What this module does NOT ship
//!
//! - **rustls integration.** Once Lakko has a working
//!   ECH-supporting test endpoint (Cloudflare publishes
//!   one), a follow-up sprint wires the parsed
//!   [`EchConfigList`] into `rustls::client::ClientConfig`
//!   via `with_ech`. Until then, the parser stands alone
//!   so the audit-pack invariant (T-12) can be pinned
//!   independently of the network-side integration.
//! - **DNS HTTPS RR fetch.** The caller pulls the RR
//!   bytes from its preferred resolver (currently DoH via
//!   [`crate::DohFetcher`]) and hands the `ech=...`
//!   parameter bytes to [`parse_ech_config_list`].
//! - **Outer / inner `ClientHello` construction.** That
//!   is rustls's job; the parser only extracts the
//!   keying material rustls needs to build them.
//!
//! ## Pinned audit invariants
//!
//! - **T-12** — ECH config list parser is version-pinned
//!   (0xfe0d only) and length-bounded (max
//!   [`MAX_ECH_CONFIG_LIST_LEN`] bytes, max
//!   [`MAX_INNER_CONFIGS`] inner configs); no allocation
//!   exceeds a hard cap from arbitrary attacker input.

use thiserror::Error;

/// ECH version code published by IANA for
/// draft-ietf-tls-esni-13 onwards. Cloudflare, Mozilla,
/// and the rustls reference impl all use this value in
/// production today.
pub const ECH_VERSION_DRAFT_13: u16 = 0xfe0d;

/// Hard cap on the outer `ECHConfigList` length — 16 KiB.
/// In practice an `ECHConfigList` is < 1 KiB; the cap
/// protects against an attacker who spoofs a DNS answer
/// announcing a multi-megabyte list.
pub const MAX_ECH_CONFIG_LIST_LEN: usize = 16 * 1024;

/// Hard cap on the number of inner `ECHConfig` entries
/// per list. The spec does not pin a number; 16 is well
/// above any production publisher (typically 1-2) and
/// well below the point at which inner parsing becomes a
/// quadratic-input DoS.
pub const MAX_INNER_CONFIGS: usize = 16;

/// Hard cap on a single inner `ECHConfig` length. Each
/// config carries an HPKE public key (≤ 2 KiB worst case
/// for X-Wing-Kyber) plus extensions; 4 KiB is plenty.
pub const MAX_INNER_CONFIG_LEN: usize = 4 * 1024;

/// HPKE-symmetric cipher suite tuple — KDF + AEAD ids
/// per the HPKE registry (RFC 9180).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HpkeSymmetricCipherSuite {
    /// HPKE KDF id (e.g. `0x0001` for HKDF-SHA256).
    pub kdf_id: u16,
    /// HPKE AEAD id (e.g. `0x0001` for AES-128-GCM,
    /// `0x0003` for ChaCha20-Poly1305).
    pub aead_id: u16,
}

/// Single parsed `ECHConfig` body (draft-13 shape).
#[derive(Clone, Debug)]
pub struct EchConfig {
    /// Byte the server uses to select this config from
    /// the inner ClientHello. The publisher assigns this
    /// — typically a small ascending integer.
    pub config_id: u8,
    /// HPKE KEM id from the HPKE registry (e.g. `0x0020`
    /// for X25519-HKDF-SHA256).
    pub kem_id: u16,
    /// HPKE public key bytes. The cipher-suite KEM id
    /// determines length; the parser keeps the raw bytes
    /// for rustls to consume.
    pub public_key: Vec<u8>,
    /// Allowed (KDF, AEAD) combinations under this key.
    /// At least one entry; the parser refuses an empty
    /// list.
    pub cipher_suites: Vec<HpkeSymmetricCipherSuite>,
    /// Largest server name the outer ClientHello must be
    /// padded to look like (anti-fingerprinting). Range
    /// `1..=255` (draft §4); the parser enforces.
    pub maximum_name_length: u8,
    /// DNS name the outer ClientHello SNI advertises
    /// (e.g. `cloudflare-ech.com`). Length `1..=255` per
    /// the draft.
    pub public_name: String,
    /// Opaque extensions blob. The parser keeps it as a
    /// byte slice; rustls decodes any extensions it
    /// recognises. Forward-compat skip the rest.
    pub extensions: Vec<u8>,
}

/// Newtype around the inner `Vec<EchConfig>` so callers
/// can pattern-match on it directly.
#[derive(Clone, Debug)]
pub struct EchConfigList(Vec<EchConfig>);

impl EchConfigList {
    /// View the parsed entries.
    #[must_use]
    pub fn as_slice(&self) -> &[EchConfig] {
        &self.0
    }

    /// Number of inner configs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` if the list is empty (the spec forbids this,
    /// so this method only returns `true` if a caller
    /// constructs a list bypassing the parser).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Failure modes the parser surfaces. Callers branch on
/// the variant, never on the message string — same
/// convention as the rest of the transport stack.
#[derive(Debug, Error)]
pub enum EchError {
    /// The byte slice is shorter than the next length
    /// prefix or fixed-size field expects.
    #[error("truncated wire: need {needed} bytes, have {have}")]
    TruncatedWire {
        /// Number of bytes the next field expected.
        needed: usize,
        /// Number of bytes actually remaining.
        have: usize,
    },
    /// Outer `ECHConfigList` length exceeds
    /// [`MAX_ECH_CONFIG_LIST_LEN`].
    #[error("ECHConfigList too large: {len} bytes > cap {cap}")]
    OuterTooLarge {
        /// Encoded outer length.
        len: usize,
        /// Hard cap.
        cap: usize,
    },
    /// Inner config count exceeds [`MAX_INNER_CONFIGS`].
    #[error("too many inner ECHConfig entries: {count} > cap {cap}")]
    TooManyInnerConfigs {
        /// Inner-configs count observed.
        count: usize,
        /// Hard cap.
        cap: usize,
    },
    /// Inner config length exceeds
    /// [`MAX_INNER_CONFIG_LEN`].
    #[error("inner ECHConfig too large: {len} bytes > cap {cap}")]
    InnerTooLarge {
        /// Inner length observed.
        len: usize,
        /// Hard cap.
        cap: usize,
    },
    /// `ECHConfig.version` field is not
    /// [`ECH_VERSION_DRAFT_13`]. Unknown versions are
    /// refused, not silently skipped — the audit-safe
    /// stance until a forward-compat strategy is wired.
    #[error("unsupported ECHConfig version: got {got:#x}, expected {expected:#x}")]
    UnsupportedVersion {
        /// Encoded version code.
        got: u16,
        /// Pinned version code this build understands.
        expected: u16,
    },
    /// Inner body declared one length but consumed
    /// another — usually a corrupted wire.
    #[error("inner ECHConfig body length mismatch: declared {declared}, consumed {consumed}")]
    InnerBodyLengthMismatch {
        /// Length the outer header declared.
        declared: usize,
        /// Length the inner parser actually consumed.
        consumed: usize,
    },
    /// `cipher_suites` array is empty. The spec
    /// (draft §4) requires at least one entry.
    #[error("ECHConfig cipher_suites is empty")]
    EmptyCipherSuites,
    /// `cipher_suites` blob length is not a multiple of 4
    /// (KDF + AEAD = 4 bytes). Malformed wire.
    #[error("cipher_suites length {len} is not a multiple of 4")]
    BadCipherSuitesLength {
        /// Encoded blob length.
        len: usize,
    },
    /// `public_name` length not in `1..=255`.
    #[error("public_name length out of bounds: {len} (want 1..=255)")]
    BadPublicNameLength {
        /// Encoded length.
        len: usize,
    },
    /// `public_name` bytes are not valid UTF-8 — required
    /// by the draft (it's a DNS name, ASCII subset).
    #[error("public_name is not valid UTF-8")]
    BadPublicNameUtf8,
    /// `maximum_name_length` is 0 (out of spec).
    #[error("maximum_name_length is 0")]
    ZeroMaxNameLength,
    /// `public_key` length is 0.
    #[error("public_key length is 0")]
    EmptyPublicKey,
    /// Trailing bytes past the declared outer length.
    /// Conservative refusal — TLS extensions are length-
    /// prefixed for a reason; trailing junk means
    /// something went wrong upstream.
    #[error("trailing bytes after ECHConfigList")]
    TrailingBytes,
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, EchError>;

/// Parse an `ECHConfigList` per
/// [draft-ietf-tls-esni-18 §4](https://datatracker.ietf.org/doc/html/draft-ietf-tls-esni-18#section-4).
///
/// Strict — refuses any unsupported version, any
/// out-of-bounds length, and any trailing junk past the
/// outer length prefix. Returns an [`EchConfigList`]
/// containing zero or more [`EchConfig`] entries.
///
/// The caller is responsible for the DNS HTTPS RR fetch
/// (currently [`crate::DohFetcher`]) and for handing the
/// parsed list to rustls.
///
/// # Errors
///
/// See [`EchError`].
pub fn parse_ech_config_list(bytes: &[u8]) -> Result<EchConfigList> {
    let mut cur = Cursor::new(bytes);
    let outer_len = cur.read_u16()? as usize;
    if outer_len > MAX_ECH_CONFIG_LIST_LEN {
        return Err(EchError::OuterTooLarge {
            len: outer_len,
            cap: MAX_ECH_CONFIG_LIST_LEN,
        });
    }
    let outer_body = cur.read_slice(outer_len)?;
    if cur.remaining() != 0 {
        return Err(EchError::TrailingBytes);
    }

    let mut inner_cur = Cursor::new(outer_body);
    let mut configs: Vec<EchConfig> = Vec::new();
    while inner_cur.remaining() > 0 {
        if configs.len() == MAX_INNER_CONFIGS {
            return Err(EchError::TooManyInnerConfigs {
                count: configs.len() + 1,
                cap: MAX_INNER_CONFIGS,
            });
        }
        configs.push(parse_one_config(&mut inner_cur)?);
    }
    Ok(EchConfigList(configs))
}

/// Parse a single `ECHConfig` (version + length + body)
/// from the cursor. Refuses any version other than the
/// pinned [`ECH_VERSION_DRAFT_13`].
fn parse_one_config(cur: &mut Cursor<'_>) -> Result<EchConfig> {
    let version = cur.read_u16()?;
    let body_len = cur.read_u16()? as usize;
    if body_len > MAX_INNER_CONFIG_LEN {
        return Err(EchError::InnerTooLarge {
            len: body_len,
            cap: MAX_INNER_CONFIG_LEN,
        });
    }
    let body = cur.read_slice(body_len)?;
    if version != ECH_VERSION_DRAFT_13 {
        return Err(EchError::UnsupportedVersion {
            got: version,
            expected: ECH_VERSION_DRAFT_13,
        });
    }

    let mut body_cur = Cursor::new(body);
    let config_id = body_cur.read_u8()?;
    let kem_id = body_cur.read_u16()?;
    let pubkey_len = body_cur.read_u16()? as usize;
    if pubkey_len == 0 {
        return Err(EchError::EmptyPublicKey);
    }
    let public_key = body_cur.read_slice(pubkey_len)?.to_vec();

    let suites_len = body_cur.read_u16()? as usize;
    if suites_len % 4 != 0 {
        return Err(EchError::BadCipherSuitesLength { len: suites_len });
    }
    if suites_len == 0 {
        return Err(EchError::EmptyCipherSuites);
    }
    let suites_bytes = body_cur.read_slice(suites_len)?;
    let mut cipher_suites = Vec::with_capacity(suites_len / 4);
    let mut suites_cur = Cursor::new(suites_bytes);
    while suites_cur.remaining() > 0 {
        let kdf_id = suites_cur.read_u16()?;
        let aead_id = suites_cur.read_u16()?;
        cipher_suites.push(HpkeSymmetricCipherSuite { kdf_id, aead_id });
    }

    let maximum_name_length = body_cur.read_u8()?;
    if maximum_name_length == 0 {
        return Err(EchError::ZeroMaxNameLength);
    }
    let pubname_len = body_cur.read_u8()? as usize;
    if !(1..=255).contains(&pubname_len) {
        return Err(EchError::BadPublicNameLength { len: pubname_len });
    }
    let pubname_bytes = body_cur.read_slice(pubname_len)?;
    let public_name = std::str::from_utf8(pubname_bytes)
        .map_err(|_| EchError::BadPublicNameUtf8)?
        .to_string();

    let ext_len = body_cur.read_u16()? as usize;
    let extensions = body_cur.read_slice(ext_len)?.to_vec();

    // Inner body MUST consume the declared length exactly.
    if body_cur.remaining() != 0 {
        return Err(EchError::InnerBodyLengthMismatch {
            declared: body_len,
            consumed: body_len - body_cur.remaining(),
        });
    }

    Ok(EchConfig {
        config_id,
        kem_id,
        public_key,
        cipher_suites,
        maximum_name_length,
        public_name,
        extensions,
    })
}

// =========================================================
// Cursor — minimal big-endian byte reader. The TLS wire
// format is fixed-width big-endian; pulling in `byteorder`
// or `bytes` for one parser is overkill.
// =========================================================

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn read_u8(&mut self) -> Result<u8> {
        if self.remaining() < 1 {
            return Err(EchError::TruncatedWire {
                needed: 1,
                have: self.remaining(),
            });
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_u16(&mut self) -> Result<u16> {
        if self.remaining() < 2 {
            return Err(EchError::TruncatedWire {
                needed: 2,
                have: self.remaining(),
            });
        }
        let v = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn read_slice(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.remaining() < len {
            return Err(EchError::TruncatedWire {
                needed: len,
                have: self.remaining(),
            });
        }
        let s = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(s)
    }
}

// =========================================================
// Tests
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid ECHConfigList with one
    /// config, returning the on-wire bytes. Used as the
    /// fixture across happy + negative tests.
    fn build_one_config_list() -> Vec<u8> {
        // Inner config body fields.
        let config_id: u8 = 7;
        let kem_id: u16 = 0x0020; // X25519-HKDF-SHA256
        let public_key = vec![0u8; 32]; // X25519 pubkey length
        let cipher_suites: Vec<u8> = vec![
            0x00, 0x01, // kdf: HKDF-SHA256
            0x00, 0x03, // aead: ChaCha20-Poly1305
        ];
        let max_name_len: u8 = 64;
        let public_name = b"cloudflare-ech.com";
        let extensions = Vec::<u8>::new();

        let mut body = Vec::new();
        body.push(config_id);
        body.extend_from_slice(&kem_id.to_be_bytes());
        body.extend_from_slice(&u16::try_from(public_key.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&public_key);
        body.extend_from_slice(&u16::try_from(cipher_suites.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&cipher_suites);
        body.push(max_name_len);
        body.push(u8::try_from(public_name.len()).unwrap());
        body.extend_from_slice(public_name);
        body.extend_from_slice(&u16::try_from(extensions.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&extensions);

        let mut config = Vec::new();
        config.extend_from_slice(&ECH_VERSION_DRAFT_13.to_be_bytes());
        config.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        config.extend_from_slice(&body);

        let mut wire = Vec::new();
        wire.extend_from_slice(&u16::try_from(config.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(&config);
        wire
    }

    #[test]
    fn parse_one_config_happy_path() {
        let wire = build_one_config_list();
        let list = parse_ech_config_list(&wire).expect("parses");
        assert_eq!(list.len(), 1);
        let cfg = &list.as_slice()[0];
        assert_eq!(cfg.config_id, 7);
        assert_eq!(cfg.kem_id, 0x0020);
        assert_eq!(cfg.public_key.len(), 32);
        assert_eq!(cfg.cipher_suites.len(), 1);
        assert_eq!(cfg.cipher_suites[0].kdf_id, 0x0001);
        assert_eq!(cfg.cipher_suites[0].aead_id, 0x0003);
        assert_eq!(cfg.maximum_name_length, 64);
        assert_eq!(cfg.public_name, "cloudflare-ech.com");
        assert!(cfg.extensions.is_empty());
    }

    #[test]
    fn parse_empty_list() {
        // Outer length 0 = no inner configs. The spec
        // permits a zero-length list (it means "no ECH
        // available"); the parser accepts.
        let wire = vec![0x00, 0x00];
        let list = parse_ech_config_list(&wire).expect("parses empty");
        assert!(list.is_empty());
    }

    #[test]
    fn rejects_truncated_outer_length() {
        let wire = vec![0x00]; // one byte short of the u16 length prefix
        match parse_ech_config_list(&wire) {
            Err(EchError::TruncatedWire { needed, have }) => {
                assert_eq!(needed, 2);
                assert_eq!(have, 1);
            },
            other => panic!("expected TruncatedWire, got {other:?}"),
        }
    }

    #[test]
    fn rejects_truncated_inner_body() {
        let mut wire = build_one_config_list();
        // Trim two bytes off the end — the inner body
        // promises more than is on the wire.
        wire.truncate(wire.len() - 2);
        // Patch the outer length to match the truncated
        // body so we exercise the inner-truncation branch
        // rather than tripping the outer length first.
        let new_outer = u16::try_from(wire.len() - 2).unwrap();
        wire[0..2].copy_from_slice(&new_outer.to_be_bytes());
        match parse_ech_config_list(&wire) {
            Err(EchError::TruncatedWire { .. }) => {},
            other => panic!("expected TruncatedWire on inner body, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut wire = build_one_config_list();
        // Flip the inner version bytes (at offset 2..=3
        // — right after the outer u16 length prefix) to
        // a fictional draft-99.
        wire[2] = 0xfe;
        wire[3] = 0xff;
        match parse_ech_config_list(&wire) {
            Err(EchError::UnsupportedVersion { got, expected }) => {
                assert_eq!(got, 0xfeff);
                assert_eq!(expected, ECH_VERSION_DRAFT_13);
            },
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn rejects_outer_too_large() {
        // u16::MAX would be ~64 KiB, well above
        // MAX_ECH_CONFIG_LIST_LEN. Build a wire whose
        // outer length claims 32 KiB and assert refusal.
        let mut wire = Vec::new();
        let claimed = u16::try_from(MAX_ECH_CONFIG_LIST_LEN + 1).unwrap();
        wire.extend_from_slice(&claimed.to_be_bytes());
        // We don't bother filling the body — the cap
        // trips before any body bytes are read.
        match parse_ech_config_list(&wire) {
            Err(EchError::OuterTooLarge { len, cap }) => {
                assert_eq!(len, MAX_ECH_CONFIG_LIST_LEN + 1);
                assert_eq!(cap, MAX_ECH_CONFIG_LIST_LEN);
            },
            other => panic!("expected OuterTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut wire = build_one_config_list();
        wire.push(0x00); // one junk byte past the outer length
        match parse_ech_config_list(&wire) {
            Err(EchError::TrailingBytes) => {},
            other => panic!("expected TrailingBytes, got {other:?}"),
        }
    }

    #[test]
    fn rejects_empty_cipher_suites() {
        // Hand-craft a minimal wire whose cipher_suites
        // length is 0. Easier to build from scratch than
        // patch the fixture.
        let body = {
            let mut b = Vec::new();
            b.push(0u8); // config_id
            b.extend_from_slice(&0x0020u16.to_be_bytes()); // kem_id
            b.extend_from_slice(&1u16.to_be_bytes()); // pk_len = 1
            b.push(0xAA); // public_key byte
            b.extend_from_slice(&0u16.to_be_bytes()); // cs_len = 0 — INVALID
            b.push(1u8); // max_name_length
            b.push(3u8); // public_name length
            b.extend_from_slice(b"abc");
            b.extend_from_slice(&0u16.to_be_bytes()); // extensions len = 0
            b
        };
        let mut config = Vec::new();
        config.extend_from_slice(&ECH_VERSION_DRAFT_13.to_be_bytes());
        config.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        config.extend_from_slice(&body);
        let mut wire = Vec::new();
        wire.extend_from_slice(&u16::try_from(config.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(&config);

        match parse_ech_config_list(&wire) {
            Err(EchError::EmptyCipherSuites) => {},
            other => panic!("expected EmptyCipherSuites, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_cipher_suites_length() {
        // cs_len = 3 (not a multiple of 4).
        let body = {
            let mut b = Vec::new();
            b.push(0u8);
            b.extend_from_slice(&0x0020u16.to_be_bytes());
            b.extend_from_slice(&1u16.to_be_bytes());
            b.push(0xAA);
            b.extend_from_slice(&3u16.to_be_bytes()); // cs_len = 3 — INVALID
            b.extend_from_slice(&[0x00, 0x01, 0x00]); // 3 bytes
            b.push(1u8);
            b.push(3u8);
            b.extend_from_slice(b"abc");
            b.extend_from_slice(&0u16.to_be_bytes());
            b
        };
        let mut config = Vec::new();
        config.extend_from_slice(&ECH_VERSION_DRAFT_13.to_be_bytes());
        config.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        config.extend_from_slice(&body);
        let mut wire = Vec::new();
        wire.extend_from_slice(&u16::try_from(config.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(&config);

        match parse_ech_config_list(&wire) {
            Err(EchError::BadCipherSuitesLength { len }) => assert_eq!(len, 3),
            other => panic!("expected BadCipherSuitesLength, got {other:?}"),
        }
    }

    #[test]
    fn rejects_zero_max_name_length() {
        let body = {
            let mut b = Vec::new();
            b.push(0u8);
            b.extend_from_slice(&0x0020u16.to_be_bytes());
            b.extend_from_slice(&1u16.to_be_bytes());
            b.push(0xAA);
            b.extend_from_slice(&4u16.to_be_bytes());
            b.extend_from_slice(&[0x00, 0x01, 0x00, 0x03]);
            b.push(0u8); // maximum_name_length = 0 — INVALID
            b.push(3u8);
            b.extend_from_slice(b"abc");
            b.extend_from_slice(&0u16.to_be_bytes());
            b
        };
        let mut config = Vec::new();
        config.extend_from_slice(&ECH_VERSION_DRAFT_13.to_be_bytes());
        config.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        config.extend_from_slice(&body);
        let mut wire = Vec::new();
        wire.extend_from_slice(&u16::try_from(config.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(&config);

        match parse_ech_config_list(&wire) {
            Err(EchError::ZeroMaxNameLength) => {},
            other => panic!("expected ZeroMaxNameLength, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_public_name_utf8() {
        let body = {
            let mut b = Vec::new();
            b.push(0u8);
            b.extend_from_slice(&0x0020u16.to_be_bytes());
            b.extend_from_slice(&1u16.to_be_bytes());
            b.push(0xAA);
            b.extend_from_slice(&4u16.to_be_bytes());
            b.extend_from_slice(&[0x00, 0x01, 0x00, 0x03]);
            b.push(1u8);
            b.push(3u8); // public_name length
            b.extend_from_slice(&[0xFF, 0xFE, 0xFD]); // invalid UTF-8
            b.extend_from_slice(&0u16.to_be_bytes());
            b
        };
        let mut config = Vec::new();
        config.extend_from_slice(&ECH_VERSION_DRAFT_13.to_be_bytes());
        config.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        config.extend_from_slice(&body);
        let mut wire = Vec::new();
        wire.extend_from_slice(&u16::try_from(config.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(&config);

        match parse_ech_config_list(&wire) {
            Err(EchError::BadPublicNameUtf8) => {},
            other => panic!("expected BadPublicNameUtf8, got {other:?}"),
        }
    }

    #[test]
    fn parses_two_configs_in_one_list() {
        // Concatenate two copies of the one-config fixture
        // body (rebuilding the outer length to cover
        // both).
        let single = build_one_config_list();
        // Strip the outer length prefix from the second
        // copy so we splice the inner configs together.
        let inner_each = &single[2..];
        let mut combined_inner = Vec::new();
        combined_inner.extend_from_slice(inner_each);
        combined_inner.extend_from_slice(inner_each);
        let mut wire = Vec::new();
        wire.extend_from_slice(&u16::try_from(combined_inner.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(&combined_inner);

        let list = parse_ech_config_list(&wire).expect("parses");
        assert_eq!(list.len(), 2);
        assert_eq!(list.as_slice()[0].public_name, "cloudflare-ech.com");
        assert_eq!(list.as_slice()[1].public_name, "cloudflare-ech.com");
    }
}
