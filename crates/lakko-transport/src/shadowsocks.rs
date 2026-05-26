// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Shadowsocks-2022 (AEAD-2022) — Phase 5 G5-3 outer
//! header layer per [ADR-0004][adr04] and the
//! [SS-2022 spec][spec].
//!
//! [adr04]: ../../../docs/adr/0004-anti-censorship.md
//! [spec]: https://github.com/Shadowsocks-NET/shadowsocks-specs/blob/main/2022-1-shadowsocks-2022-edition.md
//!
//! ## What's here (G5-3-header sprint)
//!
//! - [`derive_session_subkey`] — BLAKE3 `derive_key` from
//!   `psk || salt` under the pinned context string
//!   [`SS_SUBKEY_CONTEXT`]. The resulting 32-byte subkey
//!   is what feeds [`crate::aead_stream::AeadStreamWriter`]
//!   / [`crate::aead_stream::AeadStreamReader`] for the
//!   body chunks.
//! - [`RequestHeader`] / [`ResponseHeader`] — the SS-2022
//!   stream prelude that precedes the chunked AEAD body.
//!   Includes timestamp + variable header (free-form
//!   bytes; SOCKS5 destination encoding is the caller's
//!   problem for a generic SS proxy, but Lakko clients
//!   talk to Lakko relays where the field is degenerate).
//! - [`encode_request`] / [`decode_request`] — wire-level
//!   prelude codec for the client-to-server direction.
//! - [`encode_response`] / [`decode_response`] — same for
//!   the server-to-client direction. The response binds
//!   the **request salt** into its variable header so a
//!   replayed response from a different session fails
//!   verification.
//! - [`ReplayWindow`] — bounded LRU of recent request
//!   salts that pairs with a `±SS_TIMESTAMP_TOLERANCE_SECS`
//!   timestamp check on `decode_request` to block
//!   wholesale replay of a previously-observed prelude.
//!
//! ## What's NOT here yet
//!
//! - Full [`Transport`] / [`Connection`] impl over TCP.
//!   `connect()` still returns `Error::NotImplemented` —
//!   the wire-format primitives here are the bulk of the
//!   SS-2022 work, but stitching them to a `TcpStream` +
//!   [`crate::aead_stream`] body is its own sub-sprint
//!   (`S-5-shadowsocks-tcp`).
//! - Variable header parsing — Lakko uses SS-2022 as a
//!   raw obfs layer, not as a SOCKS5 proxy, so the
//!   variable header is opaque bytes that the caller
//!   threads through verbatim. A future proxy-mode helper
//!   can layer SOCKS5 destination encoding on top.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, KEY_LEN, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::random;
use lakko_crypto::secret::SecretBytes;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::transport::{Connection, Transport};

// =========================================================
// Pinned constants — wire contract for SS-2022 G5-3.
// =========================================================

/// BLAKE3 `derive_key` context for per-session subkeys.
/// Pinned exactly as the SS-2022 spec defines it; a
/// change here breaks wire compatibility with every
/// previously-shipped client.
pub const SS_SUBKEY_CONTEXT: &str = "shadowsocks 2022 session subkey";

/// AEAD ciphersuite for SS-2022 — ChaCha20-Poly1305.
/// AES-256-GCM is a separate spec variant; not exposed
/// at this layer (G5-3 only ships ChaCha20).
const ALG: Algorithm = Algorithm::ChaCha20Poly1305;

/// Per-session salt length — 32 bytes random, sent in
/// clear at the head of the stream.
pub const SS_SALT_LEN: usize = 32;

/// SS-2022 fixed-header plaintext length — 1 byte type +
/// 8 bytes timestamp + 2 bytes variable-header length.
pub const SS_FIXED_HEADER_LEN: usize = 1 + 8 + 2;

/// Sealed fixed-header wire length — plaintext + tag.
pub const SS_FIXED_HEADER_SEALED_LEN: usize = SS_FIXED_HEADER_LEN + TAG_LEN;

/// Header type byte for client-to-server prelude.
pub const SS_HEADER_TYPE_CLIENT: u8 = 0;
/// Header type byte for server-to-client prelude.
pub const SS_HEADER_TYPE_SERVER: u8 = 1;

/// Timestamp tolerance — SS-2022 spec pins a ±30s window
/// around the receiver's wall clock. A request whose
/// timestamp lies outside this window is rejected even
/// before the AEAD authenticator runs.
pub const SS_TIMESTAMP_TOLERANCE_SECS: u64 = 30;

/// Maximum variable-header length in bytes (matches the
/// 2-byte length-prefix ceiling). Same primitive cap as
/// `aead_stream::MAX_CHUNK` would impose on the body, but
/// SS-2022 spec independently caps the header field at
/// u16::MAX. Headers larger than this don't fit in the
/// wire field.
pub const SS_MAX_VAR_HEADER_LEN: usize = u16::MAX as usize;

/// Default replay-window capacity — 4096 distinct salts.
/// Each entry is a 32-byte salt + an 8-byte arrival
/// timestamp, so ~160 KiB per window — fits comfortably
/// in the per-connection state budget.
pub const SS_REPLAY_WINDOW_CAPACITY: usize = 4096;

// =========================================================
// Public types
// =========================================================

/// SS-2022 bootstrap-config descriptor — what a signed
/// rendezvous bootstrap delivers to the client.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowsocksConfig {
    /// Host:port of the SS-2022 server.
    pub remote: String,
    /// AEAD-2022 cipher suite ID. Stable strings:
    /// `"2022-blake3-chacha20-poly1305"`,
    /// `"2022-blake3-aes-256-gcm"`. Only the ChaCha20
    /// variant is implemented at the header layer in G5-3;
    /// AES-256-GCM is a separate sub-sprint.
    pub method: String,
    /// 32-byte master pre-shared key, base64. AEAD-2022
    /// derives per-session subkeys via BLAKE3 from
    /// `psk || salt` under the pinned context
    /// [`SS_SUBKEY_CONTEXT`].
    pub psk_b64: String,
}

/// SS-2022 outer-header errors. Branch on the variant,
/// not the message string.
#[derive(Debug, Error)]
pub enum ShadowsocksHeaderError {
    /// Wire buffer shorter than the minimum prelude
    /// (salt + sealed fixed header).
    #[error("wire buffer shorter than SS-2022 prelude minimum")]
    TruncatedWire,
    /// AEAD authentication failed — wrong PSK, tampered
    /// ciphertext, or unexpected counter.
    #[error("AEAD authenticator rejected the prelude")]
    AeadOpen,
    /// Header type byte did not match the expected
    /// direction (`SS_HEADER_TYPE_CLIENT` for requests,
    /// `SS_HEADER_TYPE_SERVER` for responses).
    #[error("wrong header type byte: expected {expected}, got {got}")]
    WrongHeaderType {
        /// Expected type byte for the decoded direction.
        expected: u8,
        /// Type byte actually observed on the wire.
        got: u8,
    },
    /// Timestamp lies outside the ±[`SS_TIMESTAMP_TOLERANCE_SECS`]
    /// window around `now_unix`. Surfaces an explicit
    /// drift value so a sysadmin debugging clock skew
    /// can quickly see which side is wrong.
    #[error("timestamp drift {drift_secs}s exceeds tolerance {tolerance_secs}s")]
    TimestampSkew {
        /// Absolute drift between the decoded timestamp
        /// and `now_unix`, in seconds.
        drift_secs: u64,
        /// The window the decoder was configured with —
        /// usually `SS_TIMESTAMP_TOLERANCE_SECS`.
        tolerance_secs: u64,
    },
    /// Salt was already seen in the replay window. The
    /// decoder considers the prelude valid in isolation
    /// but the connection-level state machine rejects it.
    #[error("replay detected: salt already observed within the window")]
    ReplayedSalt,
    /// Variable-header length exceeds the SS-2022 wire
    /// cap or its sealed size doesn't match the AEAD
    /// authenticator's reading.
    #[error("variable header length {0} exceeds the wire cap")]
    VarHeaderTooLarge(usize),
    /// AEAD seal failed — essentially impossible under
    /// ChaCha20-Poly1305 with valid lengths; surfaced for
    /// forward compatibility.
    #[error("AEAD seal failed")]
    AeadSeal,
    /// Response binds the request-salt into its variable
    /// header per SS-2022 spec; a response with a wrong
    /// request-salt is rejected at the application layer.
    #[error("response request-salt does not match the originating request")]
    WrongResponseSalt,
}

/// Decoded SS-2022 request prelude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHeader {
    /// u64 Unix-seconds timestamp the sender wrote.
    pub timestamp: u64,
    /// Opaque variable-header bytes. Lakko keeps these
    /// free-form; a proxy-mode shim layer would parse a
    /// SOCKS5 destination + padding out of them.
    pub variable: Vec<u8>,
}

/// Decoded SS-2022 response prelude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHeader {
    /// u64 Unix-seconds timestamp the server wrote.
    pub timestamp: u64,
    /// 32-byte request-salt the server echoes back —
    /// proof that this response was minted by a party
    /// that observed the matching request.
    pub request_salt: [u8; SS_SALT_LEN],
    /// Opaque variable-header bytes. Same shape as
    /// request side.
    pub variable: Vec<u8>,
}

/// Bounded LRU replay window — tracks recently-observed
/// salts so a wholesale-replayed request is rejected even
/// when its timestamp is still within tolerance.
///
/// Eviction is FIFO: when capacity is hit, the oldest
/// entry is dropped to make room for the newest. The
/// `±SS_TIMESTAMP_TOLERANCE_SECS` timestamp window means
/// a 4096-entry queue covers ~136 requests/second average
/// before the window can leak — well above any expected
/// per-server traffic for a Lakko relay.
#[derive(Debug)]
pub struct ReplayWindow {
    seen: VecDeque<[u8; SS_SALT_LEN]>,
    capacity: usize,
}

impl ReplayWindow {
    /// Construct with the default capacity
    /// ([`SS_REPLAY_WINDOW_CAPACITY`]).
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(SS_REPLAY_WINDOW_CAPACITY)
    }

    /// Construct with an explicit capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            seen: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// `true` if `salt` has been admitted to the window
    /// before; salts are never re-inserted.
    #[must_use]
    pub fn contains(&self, salt: &[u8; SS_SALT_LEN]) -> bool {
        self.seen.iter().any(|s| s == salt)
    }

    /// Admit `salt` to the window. Returns `false` if
    /// the salt was already present (caller treats that
    /// as a replay-detected error); returns `true` on a
    /// fresh admission.
    pub fn admit(&mut self, salt: [u8; SS_SALT_LEN]) -> bool {
        if self.contains(&salt) {
            return false;
        }
        if self.seen.len() >= self.capacity {
            self.seen.pop_front();
        }
        self.seen.push_back(salt);
        true
    }

    /// Current number of entries in the window.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// `true` if the window has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================
// Subkey derivation
// =========================================================

/// Derive the per-session AEAD subkey from the master PSK
/// and the per-connection salt.
///
/// Computes:
/// ```text
/// subkey = BLAKE3::derive_key(
///     "shadowsocks 2022 session subkey",
///     PSK || salt,
/// )
/// ```
///
/// Matches the SS-2022 spec verbatim. The returned key is
/// suitable as input to
/// [`crate::aead_stream::AeadStreamWriter::new`] /
/// [`crate::aead_stream::AeadStreamReader::new`].
#[must_use]
pub fn derive_session_subkey(
    psk: &[u8; KEY_LEN],
    salt: &[u8; SS_SALT_LEN],
) -> AeadKey {
    let mut ikm = [0u8; KEY_LEN + SS_SALT_LEN];
    ikm[..KEY_LEN].copy_from_slice(psk);
    ikm[KEY_LEN..].copy_from_slice(salt);
    let raw = blake3_derive_key_from_material(SS_SUBKEY_CONTEXT, &ikm);
    SecretBytes::new(raw)
}

// =========================================================
// Encode / decode
// =========================================================

/// Build the SS-2022 client-to-server prelude in front of
/// the AEAD body. Returns:
///
/// - the raw wire bytes (`salt || sealed_fixed || sealed_variable`),
/// - the random salt (the caller stores it so it can verify
///   the response's `request_salt` field later),
/// - the per-session subkey (the caller hands this to
///   [`crate::aead_stream::AeadStreamWriter`] for body
///   chunks).
///
/// # Errors
///
/// - [`ShadowsocksHeaderError::VarHeaderTooLarge`] if
///   `header.variable.len() > SS_MAX_VAR_HEADER_LEN`.
/// - [`ShadowsocksHeaderError::AeadSeal`] on AEAD failure
///   (effectively impossible under ChaCha20-Poly1305).
pub fn encode_request(
    psk: &[u8; KEY_LEN],
    header: &RequestHeader,
) -> std::result::Result<
    (Vec<u8>, [u8; SS_SALT_LEN], AeadKey),
    ShadowsocksHeaderError,
> {
    if header.variable.len() > SS_MAX_VAR_HEADER_LEN {
        return Err(ShadowsocksHeaderError::VarHeaderTooLarge(
            header.variable.len(),
        ));
    }
    let mut salt = [0u8; SS_SALT_LEN];
    random::fill(&mut salt);
    let subkey = derive_session_subkey(psk, &salt);

    // Fixed-header plaintext: type(1) || timestamp(8) ||
    // var_header_len(2).
    let mut fixed_pt = [0u8; SS_FIXED_HEADER_LEN];
    fixed_pt[0] = SS_HEADER_TYPE_CLIENT;
    fixed_pt[1..9].copy_from_slice(&header.timestamp.to_be_bytes());
    let var_len_u16 = u16::try_from(header.variable.len()).map_err(|_| {
        ShadowsocksHeaderError::VarHeaderTooLarge(header.variable.len())
    })?;
    fixed_pt[9..11].copy_from_slice(&var_len_u16.to_be_bytes());

    let fixed_nonce = nonce_from_counter(0);
    let sealed_fixed = aead::seal(ALG, &subkey, &fixed_nonce, &[], &fixed_pt)
        .map_err(|_| ShadowsocksHeaderError::AeadSeal)?;
    let var_nonce = nonce_from_counter(1);
    let sealed_var = aead::seal(ALG, &subkey, &var_nonce, &[], &header.variable)
        .map_err(|_| ShadowsocksHeaderError::AeadSeal)?;

    let mut out =
        Vec::with_capacity(SS_SALT_LEN + sealed_fixed.len() + sealed_var.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&sealed_fixed);
    out.extend_from_slice(&sealed_var);
    Ok((out, salt, subkey))
}

/// Decode the SS-2022 client-to-server prelude, validate
/// timestamp + replay, and return the parsed header,
/// the request salt (for echoing back in the response),
/// and the per-session subkey.
///
/// Walks the wire as:
///
/// ```text
/// salt(32) || sealed_fixed_hdr(11+16) || sealed_var_hdr(var_len+16)
/// ```
///
/// `now_unix` is the receiver's current Unix-seconds clock
/// — passed in (rather than read from `SystemTime::now`)
/// so unit tests can drive deterministic time. Production
/// callers pass `SystemTime::now().duration_since(UNIX_EPOCH)`.
///
/// On success the salt is admitted to the `window` so a
/// replay of the same prelude bytes is rejected.
///
/// # Errors
///
/// - [`ShadowsocksHeaderError::TruncatedWire`] — buffer
///   shorter than salt + sealed fixed header.
/// - [`ShadowsocksHeaderError::AeadOpen`] — wrong PSK,
///   tampered ciphertext.
/// - [`ShadowsocksHeaderError::WrongHeaderType`] — type
///   byte was not `SS_HEADER_TYPE_CLIENT`.
/// - [`ShadowsocksHeaderError::TimestampSkew`] — clock
///   drift exceeded tolerance.
/// - [`ShadowsocksHeaderError::ReplayedSalt`] — salt
///   already in `window`.
pub fn decode_request(
    psk: &[u8; KEY_LEN],
    wire: &[u8],
    now_unix: u64,
    window: &mut ReplayWindow,
) -> std::result::Result<
    (RequestHeader, [u8; SS_SALT_LEN], AeadKey),
    ShadowsocksHeaderError,
> {
    if wire.len() < SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN {
        return Err(ShadowsocksHeaderError::TruncatedWire);
    }
    let mut salt = [0u8; SS_SALT_LEN];
    salt.copy_from_slice(&wire[..SS_SALT_LEN]);
    let subkey = derive_session_subkey(psk, &salt);

    // Sealed fixed header.
    let sealed_fixed_end = SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN;
    let sealed_fixed = &wire[SS_SALT_LEN..sealed_fixed_end];
    let fixed_nonce = nonce_from_counter(0);
    let fixed_pt = aead::open(ALG, &subkey, &fixed_nonce, &[], sealed_fixed)
        .map_err(|_| ShadowsocksHeaderError::AeadOpen)?;
    if fixed_pt.len() != SS_FIXED_HEADER_LEN {
        return Err(ShadowsocksHeaderError::AeadOpen);
    }
    if fixed_pt[0] != SS_HEADER_TYPE_CLIENT {
        return Err(ShadowsocksHeaderError::WrongHeaderType {
            expected: SS_HEADER_TYPE_CLIENT,
            got: fixed_pt[0],
        });
    }
    let timestamp = u64::from_be_bytes(fixed_pt[1..9].try_into().unwrap());
    let var_len =
        u16::from_be_bytes(fixed_pt[9..11].try_into().unwrap()) as usize;

    // Timestamp drift check.
    check_timestamp_window(timestamp, now_unix)?;

    // Sealed variable header.
    let sealed_var_end = sealed_fixed_end + var_len + TAG_LEN;
    if wire.len() < sealed_var_end {
        return Err(ShadowsocksHeaderError::TruncatedWire);
    }
    let sealed_var = &wire[sealed_fixed_end..sealed_var_end];
    let var_nonce = nonce_from_counter(1);
    let variable = aead::open(ALG, &subkey, &var_nonce, &[], sealed_var)
        .map_err(|_| ShadowsocksHeaderError::AeadOpen)?;

    // Replay check — admit only after every other
    // validation passes, so a partial-failure wire does
    // not poison the window.
    if !window.admit(salt) {
        return Err(ShadowsocksHeaderError::ReplayedSalt);
    }

    Ok((
        RequestHeader {
            timestamp,
            variable,
        },
        salt,
        subkey,
    ))
}

/// Build the SS-2022 server-to-client prelude. The variable
/// header carries the 32-byte request-salt followed by the
/// caller-supplied bytes — this binds the response to the
/// originating request and stops a replayed response from
/// a different session.
///
/// Returns the wire bytes + the response's own salt + the
/// response-direction subkey.
///
/// # Errors
///
/// Same envelope as [`encode_request`] plus the
/// inherited length cap on the combined
/// `request_salt || variable` payload.
pub fn encode_response(
    psk: &[u8; KEY_LEN],
    header: &ResponseHeader,
) -> std::result::Result<
    (Vec<u8>, [u8; SS_SALT_LEN], AeadKey),
    ShadowsocksHeaderError,
> {
    // The variable section is `request_salt || header.variable`.
    let var_len = SS_SALT_LEN + header.variable.len();
    if var_len > SS_MAX_VAR_HEADER_LEN {
        return Err(ShadowsocksHeaderError::VarHeaderTooLarge(var_len));
    }
    let mut salt = [0u8; SS_SALT_LEN];
    random::fill(&mut salt);
    let subkey = derive_session_subkey(psk, &salt);

    let mut fixed_pt = [0u8; SS_FIXED_HEADER_LEN];
    fixed_pt[0] = SS_HEADER_TYPE_SERVER;
    fixed_pt[1..9].copy_from_slice(&header.timestamp.to_be_bytes());
    let var_len_u16 = u16::try_from(var_len)
        .map_err(|_| ShadowsocksHeaderError::VarHeaderTooLarge(var_len))?;
    fixed_pt[9..11].copy_from_slice(&var_len_u16.to_be_bytes());

    let fixed_nonce = nonce_from_counter(0);
    let sealed_fixed = aead::seal(ALG, &subkey, &fixed_nonce, &[], &fixed_pt)
        .map_err(|_| ShadowsocksHeaderError::AeadSeal)?;

    // Variable section: request_salt || variable.
    let mut var_pt = Vec::with_capacity(var_len);
    var_pt.extend_from_slice(&header.request_salt);
    var_pt.extend_from_slice(&header.variable);
    let var_nonce = nonce_from_counter(1);
    let sealed_var = aead::seal(ALG, &subkey, &var_nonce, &[], &var_pt)
        .map_err(|_| ShadowsocksHeaderError::AeadSeal)?;

    let mut out =
        Vec::with_capacity(SS_SALT_LEN + sealed_fixed.len() + sealed_var.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&sealed_fixed);
    out.extend_from_slice(&sealed_var);
    Ok((out, salt, subkey))
}

/// Decode the SS-2022 server-to-client prelude. Validates
/// timestamp + (caller-supplied) expected request-salt;
/// returns the parsed header + the response-direction
/// subkey.
///
/// The response does **not** go through a replay window —
/// the binding to the originating request-salt is the
/// SS-2022-side anti-replay mechanism for this direction.
///
/// # Errors
///
/// Same as [`decode_request`], plus
/// [`ShadowsocksHeaderError::WrongResponseSalt`] when the
/// echoed request-salt does not match `expected_request_salt`.
pub fn decode_response(
    psk: &[u8; KEY_LEN],
    wire: &[u8],
    now_unix: u64,
    expected_request_salt: &[u8; SS_SALT_LEN],
) -> std::result::Result<(ResponseHeader, AeadKey), ShadowsocksHeaderError> {
    if wire.len() < SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN {
        return Err(ShadowsocksHeaderError::TruncatedWire);
    }
    let mut salt = [0u8; SS_SALT_LEN];
    salt.copy_from_slice(&wire[..SS_SALT_LEN]);
    let subkey = derive_session_subkey(psk, &salt);

    let sealed_fixed_end = SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN;
    let sealed_fixed = &wire[SS_SALT_LEN..sealed_fixed_end];
    let fixed_nonce = nonce_from_counter(0);
    let fixed_pt = aead::open(ALG, &subkey, &fixed_nonce, &[], sealed_fixed)
        .map_err(|_| ShadowsocksHeaderError::AeadOpen)?;
    if fixed_pt.len() != SS_FIXED_HEADER_LEN {
        return Err(ShadowsocksHeaderError::AeadOpen);
    }
    if fixed_pt[0] != SS_HEADER_TYPE_SERVER {
        return Err(ShadowsocksHeaderError::WrongHeaderType {
            expected: SS_HEADER_TYPE_SERVER,
            got: fixed_pt[0],
        });
    }
    let timestamp = u64::from_be_bytes(fixed_pt[1..9].try_into().unwrap());
    let var_len =
        u16::from_be_bytes(fixed_pt[9..11].try_into().unwrap()) as usize;
    check_timestamp_window(timestamp, now_unix)?;

    let sealed_var_end = sealed_fixed_end + var_len + TAG_LEN;
    if wire.len() < sealed_var_end {
        return Err(ShadowsocksHeaderError::TruncatedWire);
    }
    let sealed_var = &wire[sealed_fixed_end..sealed_var_end];
    let var_nonce = nonce_from_counter(1);
    let var_pt = aead::open(ALG, &subkey, &var_nonce, &[], sealed_var)
        .map_err(|_| ShadowsocksHeaderError::AeadOpen)?;
    if var_pt.len() < SS_SALT_LEN {
        return Err(ShadowsocksHeaderError::AeadOpen);
    }
    let mut req_salt = [0u8; SS_SALT_LEN];
    req_salt.copy_from_slice(&var_pt[..SS_SALT_LEN]);
    if &req_salt != expected_request_salt {
        return Err(ShadowsocksHeaderError::WrongResponseSalt);
    }
    let variable = var_pt[SS_SALT_LEN..].to_vec();
    Ok((
        ResponseHeader {
            timestamp,
            request_salt: req_salt,
            variable,
        },
        subkey,
    ))
}

// =========================================================
// Helpers — same nonce layout as aead_stream
// =========================================================

fn nonce_from_counter(counter: u64) -> Nonce {
    let mut n = [0u8; NONCE_LEN];
    n[NONCE_LEN - 8..].copy_from_slice(&counter.to_be_bytes());
    n
}

fn check_timestamp_window(
    ts: u64,
    now: u64,
) -> std::result::Result<(), ShadowsocksHeaderError> {
    let drift = ts.abs_diff(now);
    if drift > SS_TIMESTAMP_TOLERANCE_SECS {
        return Err(ShadowsocksHeaderError::TimestampSkew {
            drift_secs: drift,
            tolerance_secs: SS_TIMESTAMP_TOLERANCE_SECS,
        });
    }
    Ok(())
}

/// Convenience: read the receiver's Unix-seconds clock.
/// Production code passes the result of this into
/// [`decode_request`] / [`decode_response`]; tests pass
/// a deterministic value.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

// =========================================================
// Scaffold Transport — TCP wiring lives in S-5-shadowsocks-tcp
// =========================================================

/// Scaffold transport — fails at `connect` until the
/// `S-5-shadowsocks-tcp` sub-sprint wires the header
/// primitives above to a real TCP `Connection`.
pub struct ShadowsocksTransport {
    #[allow(dead_code)]
    cfg: ShadowsocksConfig,
}

impl ShadowsocksTransport {
    /// Build from a parsed config.
    #[must_use]
    pub fn new(cfg: ShadowsocksConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Transport for ShadowsocksTransport {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        Err(Error::NotImplemented {
            feature: "shadowsocks-2022-tcp",
            sprint: "S-5-shadowsocks-tcp",
        })
    }
}

// =========================================================
// Tests
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_psk(byte: u8) -> [u8; KEY_LEN] {
        [byte; KEY_LEN]
    }

    #[test]
    fn pinned_constants() {
        assert_eq!(SS_SUBKEY_CONTEXT, "shadowsocks 2022 session subkey");
        assert_eq!(SS_SALT_LEN, 32);
        assert_eq!(SS_FIXED_HEADER_LEN, 11);
        assert_eq!(SS_FIXED_HEADER_SEALED_LEN, 11 + 16);
        assert_eq!(SS_HEADER_TYPE_CLIENT, 0);
        assert_eq!(SS_HEADER_TYPE_SERVER, 1);
        assert_eq!(SS_TIMESTAMP_TOLERANCE_SECS, 30);
    }

    #[test]
    fn derive_session_subkey_is_deterministic() {
        let psk = fresh_psk(0x11);
        let salt = [0xAAu8; SS_SALT_LEN];
        let a = derive_session_subkey(&psk, &salt);
        let b = derive_session_subkey(&psk, &salt);
        assert_eq!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn derive_session_subkey_psk_separates() {
        let psk_a = fresh_psk(0x11);
        let psk_b = fresh_psk(0x22);
        let salt = [0xAAu8; SS_SALT_LEN];
        let a = derive_session_subkey(&psk_a, &salt);
        let b = derive_session_subkey(&psk_b, &salt);
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn derive_session_subkey_salt_separates() {
        let psk = fresh_psk(0x11);
        let salt_a = [0xAAu8; SS_SALT_LEN];
        let salt_b = [0xBBu8; SS_SALT_LEN];
        let a = derive_session_subkey(&psk, &salt_a);
        let b = derive_session_subkey(&psk, &salt_b);
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn replay_window_admits_once() {
        let mut w = ReplayWindow::with_capacity(4);
        let salt = [0x77u8; SS_SALT_LEN];
        assert!(w.admit(salt));
        assert!(!w.admit(salt));
        assert!(w.contains(&salt));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn replay_window_evicts_at_capacity() {
        let mut w = ReplayWindow::with_capacity(3);
        for i in 0..3u8 {
            w.admit([i; SS_SALT_LEN]);
        }
        assert_eq!(w.len(), 3);
        // Fourth admission evicts the oldest (all-zeros).
        w.admit([99u8; SS_SALT_LEN]);
        assert_eq!(w.len(), 3);
        assert!(!w.contains(&[0u8; SS_SALT_LEN]));
        assert!(w.contains(&[99u8; SS_SALT_LEN]));
    }

    #[test]
    fn round_trip_request() {
        let psk = fresh_psk(0x42);
        let header = RequestHeader {
            timestamp: 1_700_000_000,
            variable: b"hello payload prefix".to_vec(),
        };
        let (wire, salt, _subkey) = encode_request(&psk, &header).unwrap();
        let mut window = ReplayWindow::new();
        let (decoded, decoded_salt, _subkey2) =
            decode_request(&psk, &wire, 1_700_000_000, &mut window).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(decoded_salt, salt);
        assert!(window.contains(&salt));
    }

    #[test]
    fn round_trip_request_with_empty_variable() {
        let psk = fresh_psk(0xCC);
        let header = RequestHeader {
            timestamp: 1_700_000_000,
            variable: Vec::new(),
        };
        let (wire, _salt, _subkey) = encode_request(&psk, &header).unwrap();
        let mut window = ReplayWindow::new();
        let (decoded, _, _) =
            decode_request(&psk, &wire, 1_700_000_000, &mut window).unwrap();
        assert_eq!(decoded, header);
    }

    #[test]
    fn round_trip_response_binds_request_salt() {
        let psk = fresh_psk(0x33);
        let req_salt = [0xABu8; SS_SALT_LEN];
        let header = ResponseHeader {
            timestamp: 1_700_000_005,
            request_salt: req_salt,
            variable: b"hello from server".to_vec(),
        };
        let (wire, _resp_salt, _subkey) = encode_response(&psk, &header).unwrap();
        let (decoded, _subkey2) =
            decode_response(&psk, &wire, 1_700_000_005, &req_salt).unwrap();
        assert_eq!(decoded, header);
    }

    #[test]
    fn decode_request_rejects_wrong_psk() {
        let psk = fresh_psk(0x10);
        let other = fresh_psk(0x20);
        let header = RequestHeader {
            timestamp: 1_700_000_000,
            variable: b"x".to_vec(),
        };
        let (wire, _, _) = encode_request(&psk, &header).unwrap();
        let mut window = ReplayWindow::new();
        let err =
            decode_request(&other, &wire, 1_700_000_000, &mut window).unwrap_err();
        assert!(matches!(err, ShadowsocksHeaderError::AeadOpen));
    }

    #[test]
    fn decode_request_rejects_timestamp_skew_past() {
        let psk = fresh_psk(0x55);
        let header = RequestHeader {
            timestamp: 1_700_000_000,
            variable: Vec::new(),
        };
        let (wire, _, _) = encode_request(&psk, &header).unwrap();
        let mut window = ReplayWindow::new();
        // 60 seconds in the future vs the message — drift
        // 60s, tolerance 30s → reject.
        let err = decode_request(&psk, &wire, 1_700_000_060, &mut window).unwrap_err();
        assert!(matches!(
            err,
            ShadowsocksHeaderError::TimestampSkew {
                drift_secs: 60,
                tolerance_secs: SS_TIMESTAMP_TOLERANCE_SECS,
            }
        ));
    }

    #[test]
    fn decode_request_accepts_timestamp_within_tolerance() {
        let psk = fresh_psk(0x66);
        let header = RequestHeader {
            timestamp: 1_700_000_000,
            variable: Vec::new(),
        };
        let (wire, _, _) = encode_request(&psk, &header).unwrap();
        let mut window = ReplayWindow::new();
        // 25 seconds drift — within ±30s tolerance.
        let res = decode_request(&psk, &wire, 1_700_000_025, &mut window);
        assert!(res.is_ok());
    }

    #[test]
    fn decode_request_rejects_replay() {
        let psk = fresh_psk(0x88);
        let header = RequestHeader {
            timestamp: 1_700_000_000,
            variable: Vec::new(),
        };
        let (wire, _, _) = encode_request(&psk, &header).unwrap();
        let mut window = ReplayWindow::new();
        // First admission succeeds.
        decode_request(&psk, &wire, 1_700_000_000, &mut window).unwrap();
        // Second admission of the same wire — same salt —
        // is rejected as a replay.
        let err =
            decode_request(&psk, &wire, 1_700_000_000, &mut window).unwrap_err();
        assert!(matches!(err, ShadowsocksHeaderError::ReplayedSalt));
    }

    #[test]
    fn decode_request_rejects_truncated_wire() {
        let mut window = ReplayWindow::new();
        // 32 (salt) + 27 (sealed fixed header) = 59 bytes
        // minimum; 40 bytes is below that.
        let err = decode_request(&fresh_psk(0x01), &[0u8; 40], 0, &mut window)
            .unwrap_err();
        assert!(matches!(err, ShadowsocksHeaderError::TruncatedWire));
    }

    #[test]
    fn decode_response_rejects_wrong_request_salt() {
        let psk = fresh_psk(0xDD);
        let header = ResponseHeader {
            timestamp: 1_700_000_000,
            request_salt: [0x11u8; SS_SALT_LEN],
            variable: b"server bytes".to_vec(),
        };
        let (wire, _, _) = encode_response(&psk, &header).unwrap();
        // Attacker tries to splice this response into a
        // session whose request-salt was 0x22.
        let err = decode_response(
            &psk,
            &wire,
            1_700_000_000,
            &[0x22u8; SS_SALT_LEN],
        )
        .unwrap_err();
        assert!(matches!(err, ShadowsocksHeaderError::WrongResponseSalt));
    }

    #[test]
    fn now_unix_returns_recent_value() {
        // Sanity that now_unix wraps SystemTime correctly.
        // Test isn't deterministic but bounds the result
        // to something post-2020 and pre-2200.
        let n = now_unix();
        assert!(n > 1_600_000_000);
        assert!(n < 7_000_000_000);
    }
}
