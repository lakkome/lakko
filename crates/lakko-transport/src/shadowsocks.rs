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
//! ## What's NOT here
//!
//! The [`Transport`] / [`Connection`] impl over TCP **is**
//! here: `connect()` opens a `TcpStream`, writes the salt +
//! sealed prelude, reads and verifies the response prelude,
//! and returns a working [`ShadowsocksConnection`] over the
//! chunked-AEAD body (T-9). [`ShadowsocksListener`] and
//! [`accept_inbound`] are the accept half, wired into
//! `services/obfs-proxy`. What remains absent:
//!
//! - Variable header parsing — Lakko uses SS-2022 as a
//!   raw obfs layer, not as a SOCKS5 proxy, so the
//!   variable header is opaque bytes that the caller
//!   threads through verbatim. A future proxy-mode helper
//!   can layer SOCKS5 destination encoding on top.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64_STANDARD;
use base64::Engine as _;
use bytes::Bytes;
use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, KEY_LEN, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::random;
use lakko_crypto::secret::SecretBytes;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tracing::debug;

use crate::aead_stream::{
    AeadStreamError, AeadStreamReader, AeadStreamWriter, MAX_CHUNK as AEAD_MAX_CHUNK,
};
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

/// Number of AEAD nonces the SS-2022 handshake prelude
/// consumes on each direction's subkey before the chunked
/// body stream begins.
///
/// The prelude seals the fixed header at nonce 0 and the
/// variable header at nonce 1 — identical in both
/// directions ([`encode_request`] and [`encode_response`]
/// each use `nonce_from_counter(0)` then
/// `nonce_from_counter(1)`) — so the body's chunked-AEAD
/// stream MUST resume its nonce counter at this value
/// under the very same per-session subkey. Restarting the
/// body at 0 would reuse `(subkey, nonce 0)` and
/// `(subkey, nonce 1)` on fresh plaintext, a catastrophic
/// ChaCha20-Poly1305 `(key, nonce)` reuse (keystream-XOR
/// leak + Poly1305 forgery on the first body chunk). This
/// matches the SS-2022 spec, where one continuous nonce
/// counter spans the prelude and the body.
pub const SS_PRELUDE_NONCE_COUNT: u64 = 2;

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
pub fn derive_session_subkey(psk: &[u8; KEY_LEN], salt: &[u8; SS_SALT_LEN]) -> AeadKey {
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
) -> std::result::Result<(Vec<u8>, [u8; SS_SALT_LEN], AeadKey), ShadowsocksHeaderError> {
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
    let var_len_u16 = u16::try_from(header.variable.len())
        .map_err(|_| ShadowsocksHeaderError::VarHeaderTooLarge(header.variable.len()))?;
    fixed_pt[9..11].copy_from_slice(&var_len_u16.to_be_bytes());

    let fixed_nonce = nonce_from_counter(0);
    let sealed_fixed = aead::seal(ALG, &subkey, &fixed_nonce, &[], &fixed_pt)
        .map_err(|_| ShadowsocksHeaderError::AeadSeal)?;
    let var_nonce = nonce_from_counter(1);
    let sealed_var = aead::seal(ALG, &subkey, &var_nonce, &[], &header.variable)
        .map_err(|_| ShadowsocksHeaderError::AeadSeal)?;

    let mut out = Vec::with_capacity(SS_SALT_LEN + sealed_fixed.len() + sealed_var.len());
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
) -> std::result::Result<(RequestHeader, [u8; SS_SALT_LEN], AeadKey), ShadowsocksHeaderError> {
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
    let var_len = u16::from_be_bytes(fixed_pt[9..11].try_into().unwrap()) as usize;

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
) -> std::result::Result<(Vec<u8>, [u8; SS_SALT_LEN], AeadKey), ShadowsocksHeaderError> {
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
    let var_len_u16 =
        u16::try_from(var_len).map_err(|_| ShadowsocksHeaderError::VarHeaderTooLarge(var_len))?;
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

    let mut out = Vec::with_capacity(SS_SALT_LEN + sealed_fixed.len() + sealed_var.len());
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
    let var_len = u16::from_be_bytes(fixed_pt[9..11].try_into().unwrap()) as usize;
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

fn check_timestamp_window(ts: u64, now: u64) -> std::result::Result<(), ShadowsocksHeaderError> {
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
// PSK parsing
// =========================================================

/// Parse the base64-encoded 32-byte SS-2022 master PSK.
///
/// SS-2022 spec mandates standard base64 (with padding).
/// We accept both padded and unpadded forms for caller
/// convenience — every public bootstrap generator we've
/// seen uses one or the other, never both, so the
/// `STANDARD_NO_PAD` fallback is defensive.
///
/// # Errors
///
/// [`Error::Config`] if the input is not valid base64 or
/// the decoded length is not exactly 32 bytes.
pub fn psk_from_b64(s: &str) -> Result<[u8; KEY_LEN]> {
    let decoded = B64_STANDARD
        .decode(s.trim())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s.trim()))
        .map_err(|e| Error::Config(format!("SS-2022 PSK base64 decode failed: {e}")))?;
    if decoded.len() != KEY_LEN {
        return Err(Error::Config(format!(
            "SS-2022 PSK must decode to exactly {KEY_LEN} bytes (got {})",
            decoded.len()
        )));
    }
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(&decoded);
    Ok(out)
}

// =========================================================
// Connection — bidirectional AEAD-2022 stream over TCP
// =========================================================

/// SS-2022 connection — wraps a TCP socket in the two-
/// direction chunked AEAD body. After the handshake
/// completes, each `send` is one AEAD chunk on the
/// client→server subkey, each `recv` is one AEAD chunk
/// on the server→client subkey.
///
/// Frame size cap is [`aead_stream::MAX_CHUNK`] (16 KiB)
/// because each frame becomes one AEAD chunk on the wire.
/// Callers needing larger frames must chunk at a higher
/// layer. This is a tighter cap than the generic
/// `transport::MAX_FRAME_BYTES` (16 MiB); the SS-2022 wire
/// invariant takes precedence.
pub struct ShadowsocksConnection {
    /// Wrapped in a Mutex because the async-trait `send`
    /// / `recv` signature is `&mut self` but `close()`
    /// needs to coordinate writer shutdown after a recv
    /// loop has borrowed the reader. The lock is
    /// uncontended in normal use — one task per direction.
    tx: Mutex<Option<AeadStreamWriter<OwnedWriteHalf>>>,
    rx: Mutex<Option<AeadStreamReader<OwnedReadHalf>>>,
}

impl ShadowsocksConnection {
    /// Construct from already-handshaked halves. The
    /// `tx_subkey` is the client-direction subkey, the
    /// `rx_subkey` is the server-direction subkey.
    ///
    /// Both body streams **resume** their AEAD nonce counter
    /// at [`SS_PRELUDE_NONCE_COUNT`] rather than 0: on each
    /// end, the handshake prelude has already sealed a fixed
    /// header (nonce 0) + variable header (nonce 1) under the
    /// same per-direction subkey. Both callers reach here with
    /// that invariant — the client (`connect`) sealed its
    /// request prelude on `tx_subkey` and opened the response
    /// prelude on `rx_subkey`; the server (`accept_inbound`)
    /// did the mirror — so continuing the body counter past
    /// the prelude keeps every `(subkey, nonce)` pair unique
    /// and matches the SS-2022 single-continuous-counter wire.
    #[must_use]
    pub fn new(
        write_half: OwnedWriteHalf,
        read_half: OwnedReadHalf,
        tx_subkey: AeadKey,
        rx_subkey: AeadKey,
    ) -> Self {
        Self {
            tx: Mutex::new(Some(AeadStreamWriter::new_with_counter(
                write_half,
                tx_subkey,
                SS_PRELUDE_NONCE_COUNT,
            ))),
            rx: Mutex::new(Some(AeadStreamReader::new_with_counter(
                read_half,
                rx_subkey,
                SS_PRELUDE_NONCE_COUNT,
            ))),
        }
    }
}

#[async_trait]
impl Connection for ShadowsocksConnection {
    async fn send(&mut self, frame: Bytes) -> Result<()> {
        if frame.len() > AEAD_MAX_CHUNK {
            return Err(Error::FrameTooLarge {
                actual: frame.len(),
                limit: AEAD_MAX_CHUNK,
            });
        }
        // Take the writer out of the Mutex for the await
        // — Tokio's Mutex would be cleaner here but we
        // already use std::sync::Mutex for non-async
        // close coordination. The take/put pattern is
        // safe because no other task holds a reference
        // (the trait owns `&mut self`).
        let mut writer = self
            .tx
            .lock()
            .map_err(|_| Error::Io("tx lock poisoned".into()))?
            .take()
            .ok_or(Error::LocalClosed)?;
        let result = async {
            writer.write_chunk(&frame).await?;
            writer.flush().await?;
            Ok::<_, AeadStreamError>(())
        }
        .await;
        // Put the writer back on success; drop it on error.
        match result {
            Ok(()) => {
                self.tx
                    .lock()
                    .map_err(|_| Error::Io("tx lock poisoned".into()))?
                    .replace(writer);
                Ok(())
            },
            Err(e) => Err(aead_err_to_transport(e)),
        }
    }

    async fn recv(&mut self) -> Result<Bytes> {
        let mut reader = self
            .rx
            .lock()
            .map_err(|_| Error::Io("rx lock poisoned".into()))?
            .take()
            .ok_or(Error::PeerClosed)?;
        let result = reader.read_chunk().await;
        match result {
            Ok(Some(plaintext)) => {
                self.rx
                    .lock()
                    .map_err(|_| Error::Io("rx lock poisoned".into()))?
                    .replace(reader);
                Ok(Bytes::from(plaintext))
            },
            Ok(None) => Err(Error::PeerClosed),
            Err(e) => Err(aead_err_to_transport(e)),
        }
    }

    async fn close(&mut self) -> Result<()> {
        // Idempotent: take + drop both halves. If the
        // writer is still present, send a shutdown so
        // the peer sees a clean EOF at frame boundary.
        let writer_opt = self
            .tx
            .lock()
            .map_err(|_| Error::Io("tx lock poisoned".into()))?
            .take();
        if let Some(mut writer) = writer_opt {
            let _ = writer.shutdown().await;
        }
        let _ = self
            .rx
            .lock()
            .map_err(|_| Error::Io("rx lock poisoned".into()))?
            .take();
        Ok(())
    }
}

fn aead_err_to_transport(e: AeadStreamError) -> Error {
    match e {
        AeadStreamError::Io(io_err) => {
            // EOF mid-frame on read surfaces as
            // PeerClosed at the transport layer.
            if io_err.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::PeerClosed
            } else {
                Error::Io(io_err.to_string())
            }
        },
        AeadStreamError::AeadOpen | AeadStreamError::AeadSeal => {
            Error::Io(format!("AEAD-2022 wire error: {e}"))
        },
        AeadStreamError::ChunkTooLarge(actual, limit) => Error::FrameTooLarge { actual, limit },
        AeadStreamError::DecodedLengthTooLarge(actual) => Error::FrameTooLarge {
            actual,
            limit: AEAD_MAX_CHUNK,
        },
        AeadStreamError::NonceWrap => Error::Io("AEAD-2022 nonce counter wrapped".into()),
    }
}

// =========================================================
// Transport — client-side connect
// =========================================================

/// SS-2022 client transport. `connect(endpoint)` opens a
/// TCP socket, runs the SS-2022 handshake (encode + send
/// request prelude, read + decode server response prelude),
/// and returns a `ShadowsocksConnection` ready for chunked
/// AEAD body traffic.
pub struct ShadowsocksTransport {
    cfg: ShadowsocksConfig,
    psk: [u8; KEY_LEN],
}

impl ShadowsocksTransport {
    /// Build from a parsed [`ShadowsocksConfig`]. The
    /// `psk_b64` field is decoded eagerly so a bad PSK
    /// surfaces at construct time, not on first connect.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] if `cfg.psk_b64` is not 32 bytes
    /// of valid base64.
    pub fn new(cfg: ShadowsocksConfig) -> Result<Self> {
        let psk = psk_from_b64(&cfg.psk_b64)?;
        Ok(Self { cfg, psk })
    }

    /// Borrow the underlying config — useful for tests
    /// and observability.
    #[must_use]
    pub fn config(&self) -> &ShadowsocksConfig {
        &self.cfg
    }
}

#[async_trait]
impl Transport for ShadowsocksTransport {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        // Resolve + TCP-connect.
        let stream =
            match endpoint {
                Endpoint::Direct(addr) => TcpStream::connect(addr)
                    .await
                    .map_err(|e| Error::Setup(format!("TCP connect: {e}")))?,
                Endpoint::Hostname { host, port } => TcpStream::connect((host.as_str(), *port))
                    .await
                    .map_err(|e| Error::Setup(format!("TCP connect: {e}")))?,
            };
        // Disable Nagle: SS-2022 is message-oriented and
        // 2-write coalescing already happens inside
        // `AeadStreamWriter::write_chunk`.
        if let Err(e) = stream.set_nodelay(true) {
            debug!(error = %e, "SS-2022 set_nodelay failed (non-fatal)");
        }
        let (mut read_half, mut write_half) = stream.into_split();

        // Build + send the request prelude.
        let header = RequestHeader {
            timestamp: now_unix(),
            variable: Vec::new(),
        };
        let (wire_prelude, request_salt, tx_subkey) =
            encode_request(&self.psk, &header).map_err(header_err_to_transport)?;
        write_half
            .write_all(&wire_prelude)
            .await
            .map_err(|e| Error::Io(format!("SS-2022 prelude write: {e}")))?;
        write_half
            .flush()
            .await
            .map_err(|e| Error::Io(format!("SS-2022 prelude flush: {e}")))?;

        // Read the server's response prelude.
        let rx_subkey =
            read_and_decode_response_prelude(&self.psk, &request_salt, &mut read_half).await?;

        Ok(Box::new(ShadowsocksConnection::new(
            write_half, read_half, tx_subkey, rx_subkey,
        )))
    }
}

/// Read a server-direction prelude from the borrowed
/// read half and decode it. Uses `AsyncReadExt::read_exact`
/// directly against `OwnedReadHalf` (which is AsyncRead +
/// Unpin) so no adapter is needed.
async fn read_and_decode_response_prelude(
    psk: &[u8; KEY_LEN],
    expected_request_salt: &[u8; SS_SALT_LEN],
    reader: &mut OwnedReadHalf,
) -> Result<AeadKey> {
    // Read salt + sealed fixed header.
    let mut head = vec![0u8; SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN];
    reader
        .read_exact(&mut head)
        .await
        .map_err(|e| Error::Io(format!("SS-2022 response prelude head read: {e}")))?;
    // Peek the variable-header length by decoding the
    // fixed header in isolation.
    let mut salt = [0u8; SS_SALT_LEN];
    salt.copy_from_slice(&head[..SS_SALT_LEN]);
    let subkey = derive_session_subkey(psk, &salt);
    let sealed_fixed = &head[SS_SALT_LEN..];
    let fixed_nonce = nonce_from_counter(0);
    let fixed_pt = aead::open(ALG, &subkey, &fixed_nonce, &[], sealed_fixed)
        .map_err(|_| Error::Setup("SS-2022 response prelude AEAD open failed".into()))?;
    if fixed_pt.len() != SS_FIXED_HEADER_LEN {
        return Err(Error::Setup(
            "SS-2022 response prelude length mismatch".into(),
        ));
    }
    if fixed_pt[0] != SS_HEADER_TYPE_SERVER {
        return Err(Error::Setup(format!(
            "SS-2022 response prelude wrong header type: {}",
            fixed_pt[0]
        )));
    }
    let var_len = u16::from_be_bytes(fixed_pt[9..11].try_into().unwrap()) as usize;
    // Read the sealed variable header.
    let mut sealed_var = vec![0u8; var_len + TAG_LEN];
    reader
        .read_exact(&mut sealed_var)
        .await
        .map_err(|e| Error::Io(format!("SS-2022 response variable read: {e}")))?;
    // Combine + decode through the canonical helper.
    let mut full_wire =
        Vec::with_capacity(SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN + sealed_var.len());
    full_wire.extend_from_slice(&head);
    full_wire.extend_from_slice(&sealed_var);
    let (_header, sub) = decode_response(psk, &full_wire, now_unix(), expected_request_salt)
        .map_err(header_err_to_transport)?;
    Ok(sub)
}

// =========================================================
// Server-side helper — accept_inbound
// =========================================================

/// Server-side handshake helper. Given an accepted
/// `TcpStream`, the master PSK, and a (caller-owned)
/// replay window, reads + decodes the client's request
/// prelude, builds + sends the matching server response
/// prelude, and returns a [`ShadowsocksConnection`]
/// ready for body traffic.
///
/// A full `Listener` impl that loops on
/// `TcpListener::accept` and spawns one task per
/// connection is straight-line composition over this
/// helper; we ship the helper alone because the listener
/// side benefits from caller-supplied graceful-shutdown
/// wiring that's out of scope for the header sprint.
///
/// # Errors
///
/// Inherits the same variant envelope as `connect` —
/// [`Error::Io`] on TCP I/O failure, [`Error::Setup`] on
/// AEAD / header validation, [`Error::Config`] never
/// (PSK is supplied as raw bytes).
pub async fn accept_inbound(
    stream: TcpStream,
    psk: &[u8; KEY_LEN],
    window: &mut ReplayWindow,
) -> Result<Box<dyn Connection>> {
    if let Err(e) = stream.set_nodelay(true) {
        debug!(error = %e, "SS-2022 accept set_nodelay failed (non-fatal)");
    }
    let (mut read_half, mut write_half) = stream.into_split();

    // Read salt + sealed fixed header.
    let mut head = vec![0u8; SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN];
    read_half
        .read_exact(&mut head)
        .await
        .map_err(|e| Error::Io(format!("SS-2022 inbound prelude head: {e}")))?;
    // Peek var_len.
    let mut salt = [0u8; SS_SALT_LEN];
    salt.copy_from_slice(&head[..SS_SALT_LEN]);
    let subkey_peek = derive_session_subkey(psk, &salt);
    let fixed_nonce = nonce_from_counter(0);
    let fixed_pt = aead::open(ALG, &subkey_peek, &fixed_nonce, &[], &head[SS_SALT_LEN..])
        .map_err(|_| Error::Setup("SS-2022 inbound AEAD open failed".into()))?;
    if fixed_pt.len() != SS_FIXED_HEADER_LEN {
        return Err(Error::Setup("SS-2022 inbound length mismatch".into()));
    }
    let var_len = u16::from_be_bytes(fixed_pt[9..11].try_into().unwrap()) as usize;
    let mut sealed_var = vec![0u8; var_len + TAG_LEN];
    read_half
        .read_exact(&mut sealed_var)
        .await
        .map_err(|e| Error::Io(format!("SS-2022 inbound variable header: {e}")))?;
    let mut full_wire =
        Vec::with_capacity(SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN + sealed_var.len());
    full_wire.extend_from_slice(&head);
    full_wire.extend_from_slice(&sealed_var);
    let (req, request_salt, tx_subkey_dec) =
        decode_request(psk, &full_wire, now_unix(), window).map_err(header_err_to_transport)?;
    debug!(ts = req.timestamp, "SS-2022 inbound prelude accepted");

    // Build + send the server response prelude.
    let response_header = ResponseHeader {
        timestamp: now_unix(),
        request_salt,
        variable: Vec::new(),
    };
    let (resp_wire, _resp_salt, rx_subkey_send) =
        encode_response(psk, &response_header).map_err(header_err_to_transport)?;
    write_half
        .write_all(&resp_wire)
        .await
        .map_err(|e| Error::Io(format!("SS-2022 inbound response write: {e}")))?;
    write_half
        .flush()
        .await
        .map_err(|e| Error::Io(format!("SS-2022 inbound response flush: {e}")))?;

    // From the server's perspective:
    //   tx (server → client) uses the response-side
    //     subkey (we just sent the response prelude with
    //     it; body chunks continue under the same).
    //   rx (client → server) uses the request-side
    //     subkey (we just decoded the request prelude
    //     with it; body chunks continue under the same).
    Ok(Box::new(ShadowsocksConnection::new(
        write_half,
        read_half,
        rx_subkey_send,
        tx_subkey_dec,
    )))
}

// =========================================================
// Listener — accept loop wrapping accept_inbound
// =========================================================

use crate::transport::Listener as TransportListener;

/// SS-2022 server listener. Wraps a [`tokio::net::TcpListener`]
/// together with a per-listener [`ReplayWindow`] and the
/// master PSK. Calling [`Listener::accept`] in a loop
/// drives the server-side handshake for each inbound TCP
/// connection.
///
/// Cheap to construct via [`Self::bind`]; the listener
/// owns its replay window, so a single listener
/// rejects salt-replay across its own connections but
/// two distinct listeners on different ports have
/// independent windows. That matches the threat model:
/// a replay against listener A cannot be repeated
/// against listener B because the response prelude
/// binds the request salt to the specific session.
pub struct ShadowsocksListener {
    tcp: tokio::net::TcpListener,
    psk: [u8; KEY_LEN],
    window: ReplayWindow,
    local_addr: std::net::SocketAddr,
}

impl ShadowsocksListener {
    /// Bind a TCP listener and prepare it for SS-2022
    /// accepts. The `psk` is the 32-byte master PSK
    /// (already decoded from base64 — call
    /// [`psk_from_b64`] if the caller has the b64
    /// form from a `ShadowsocksConfig`).
    ///
    /// # Errors
    ///
    /// [`Error::Setup`] on TCP bind failure.
    pub async fn bind(addr: std::net::SocketAddr, psk: [u8; KEY_LEN]) -> Result<Self> {
        let tcp = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| Error::Setup(format!("SS-2022 listener bind: {e}")))?;
        let local_addr = tcp
            .local_addr()
            .map_err(|e| Error::Setup(format!("SS-2022 listener local_addr: {e}")))?;
        Ok(Self {
            tcp,
            psk,
            window: ReplayWindow::new(),
            local_addr,
        })
    }

    /// Build from an already-bound `TcpListener` and a
    /// caller-supplied replay window. Useful for tests
    /// that pin a known window state or for advanced
    /// callers that want to share a window across
    /// listeners (uncommon — the default
    /// [`Self::bind`] gives each listener its own).
    pub fn from_parts(
        tcp: tokio::net::TcpListener,
        psk: [u8; KEY_LEN],
        window: ReplayWindow,
    ) -> Result<Self> {
        let local_addr = tcp
            .local_addr()
            .map_err(|e| Error::Setup(format!("SS-2022 listener local_addr: {e}")))?;
        Ok(Self {
            tcp,
            psk,
            window,
            local_addr,
        })
    }
}

#[async_trait]
impl TransportListener for ShadowsocksListener {
    async fn accept(&mut self) -> Result<Box<dyn Connection>> {
        let (stream, peer) = self
            .tcp
            .accept()
            .await
            .map_err(|e| Error::Io(format!("SS-2022 listener accept: {e}")))?;
        debug!(%peer, "SS-2022 inbound TCP accepted");
        accept_inbound(stream, &self.psk, &mut self.window).await
    }

    fn local_endpoint(&self) -> Endpoint {
        Endpoint::Direct(self.local_addr)
    }
}

// =========================================================
// Error mapping helpers
// =========================================================

fn header_err_to_transport(e: ShadowsocksHeaderError) -> Error {
    match e {
        ShadowsocksHeaderError::AeadOpen
        | ShadowsocksHeaderError::AeadSeal
        | ShadowsocksHeaderError::WrongHeaderType { .. }
        | ShadowsocksHeaderError::TimestampSkew { .. }
        | ShadowsocksHeaderError::ReplayedSalt
        | ShadowsocksHeaderError::WrongResponseSalt
        | ShadowsocksHeaderError::TruncatedWire => Error::Setup(format!("SS-2022 handshake: {e}")),
        ShadowsocksHeaderError::VarHeaderTooLarge(n) => Error::FrameTooLarge {
            actual: n,
            limit: SS_MAX_VAR_HEADER_LEN,
        },
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
        let (decoded, _, _) = decode_request(&psk, &wire, 1_700_000_000, &mut window).unwrap();
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
        let (decoded, _subkey2) = decode_response(&psk, &wire, 1_700_000_005, &req_salt).unwrap();
        assert_eq!(decoded, header);
    }

    /// Assert a prelude occupies exactly the first two
    /// nonces on its subkey: the sealed fixed header opens at
    /// nonce 0, the sealed variable header opens at nonce 1
    /// (and NOT at nonce 0 — proof the two sections use
    /// distinct nonces).
    fn assert_prelude_occupies_two_nonces(wire: &[u8], subkey: &AeadKey) {
        let sealed_fixed = &wire[SS_SALT_LEN..SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN];
        assert!(aead::open(ALG, subkey, &nonce_from_counter(0), &[], sealed_fixed).is_ok());
        let sealed_var = &wire[SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN..];
        assert!(aead::open(ALG, subkey, &nonce_from_counter(1), &[], sealed_var).is_ok());
        assert!(aead::open(ALG, subkey, &nonce_from_counter(0), &[], sealed_var).is_err());
    }

    /// Seal a body first-chunk length frame at the body's
    /// resume counter ([`SS_PRELUDE_NONCE_COUNT`]) and prove
    /// it does not open at any prelude nonce (0 or 1) under
    /// the same subkey — only at its true nonce. This is the
    /// no-`(key, nonce)`-reuse property in isolation.
    fn assert_body_first_chunk_disjoint_from_prelude(subkey: &AeadKey) {
        let sealed = aead::seal(
            ALG,
            subkey,
            &nonce_from_counter(SS_PRELUDE_NONCE_COUNT),
            &[],
            &7u16.to_be_bytes(),
        )
        .unwrap();
        assert!(aead::open(ALG, subkey, &nonce_from_counter(0), &[], &sealed).is_err());
        assert!(aead::open(ALG, subkey, &nonce_from_counter(1), &[], &sealed).is_err());
        assert!(aead::open(
            ALG,
            subkey,
            &nonce_from_counter(SS_PRELUDE_NONCE_COUNT),
            &[],
            &sealed
        )
        .is_ok());
    }

    #[test]
    fn prelude_and_body_use_disjoint_nonces_same_subkey() {
        // Regression for the SS-2022 (key, nonce) reuse bug:
        // the prelude seals the fixed header at nonce 0 and
        // the variable header at nonce 1 under the SAME
        // per-direction subkey the chunked body then uses, so
        // the body MUST continue at SS_PRELUDE_NONCE_COUNT. If
        // it restarted at 0 the first body chunk would reseal
        // fresh plaintext under (subkey, nonce 0/1) —
        // catastrophic keystream reuse + Poly1305 forgery.
        // Verified in BOTH directions (per the task's caution
        // that each direction's prelude count be confirmed
        // independently — request and response both consume 2).
        assert_eq!(SS_PRELUDE_NONCE_COUNT, 2);
        let psk = fresh_psk(0x42);

        // client -> server direction (request prelude).
        let (req_wire, _rsalt, tx_subkey) = encode_request(
            &psk,
            &RequestHeader {
                timestamp: 1_700_000_000,
                variable: b"req-var".to_vec(),
            },
        )
        .unwrap();
        assert_prelude_occupies_two_nonces(&req_wire, &tx_subkey);
        assert_body_first_chunk_disjoint_from_prelude(&tx_subkey);

        // server -> client direction (response prelude).
        let (resp_wire, _ssalt, rx_subkey) = encode_response(
            &psk,
            &ResponseHeader {
                timestamp: 1_700_000_000,
                request_salt: [0x11u8; SS_SALT_LEN],
                variable: b"resp-var".to_vec(),
            },
        )
        .unwrap();
        assert_prelude_occupies_two_nonces(&resp_wire, &rx_subkey);
        assert_body_first_chunk_disjoint_from_prelude(&rx_subkey);
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
        let err = decode_request(&other, &wire, 1_700_000_000, &mut window).unwrap_err();
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
        let err = decode_request(&psk, &wire, 1_700_000_000, &mut window).unwrap_err();
        assert!(matches!(err, ShadowsocksHeaderError::ReplayedSalt));
    }

    #[test]
    fn decode_request_rejects_truncated_wire() {
        let mut window = ReplayWindow::new();
        // 32 (salt) + 27 (sealed fixed header) = 59 bytes
        // minimum; 40 bytes is below that.
        let err = decode_request(&fresh_psk(0x01), &[0u8; 40], 0, &mut window).unwrap_err();
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
        let err = decode_response(&psk, &wire, 1_700_000_000, &[0x22u8; SS_SALT_LEN]).unwrap_err();
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

    // -----------------------------------------------------
    // psk_from_b64 (S-5-shadowsocks-tcp)
    // -----------------------------------------------------

    #[test]
    fn psk_from_b64_round_trips_padded() {
        let raw = [0xABu8; KEY_LEN];
        let encoded = B64_STANDARD.encode(raw);
        let parsed = psk_from_b64(&encoded).unwrap();
        assert_eq!(parsed, raw);
    }

    #[test]
    fn psk_from_b64_round_trips_unpadded() {
        let raw = [0xCDu8; KEY_LEN];
        let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(raw);
        let parsed = psk_from_b64(&encoded).unwrap();
        assert_eq!(parsed, raw);
    }

    #[test]
    fn psk_from_b64_rejects_wrong_length() {
        // 16 bytes encoded → 24 chars (with padding).
        let short = B64_STANDARD.encode([0u8; 16]);
        let err = psk_from_b64(&short).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn psk_from_b64_rejects_invalid_base64() {
        let err = psk_from_b64("!!! not base64 at all !!!").unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    // -----------------------------------------------------
    // TCP round-trip (S-5-shadowsocks-tcp)
    // -----------------------------------------------------

    use tokio::net::TcpListener;

    /// End-to-end test: client connect → server accept →
    /// bidirectional frame round trip through real TCP +
    /// SS-2022 handshake + chunked-AEAD body.
    #[tokio::test]
    async fn tcp_round_trip_frame_traffic() {
        let psk = [0x55u8; KEY_LEN];
        let psk_b64 = B64_STANDARD.encode(psk);
        let cfg = ShadowsocksConfig {
            remote: "127.0.0.1:0".into(),
            method: "2022-blake3-chacha20-poly1305".into(),
            psk_b64,
        };
        let transport = ShadowsocksTransport::new(cfg).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound_addr = listener.local_addr().unwrap();

        // Spawn the server side first so the client's
        // connect can land.
        let psk_server = psk;
        let server = tokio::spawn(async move {
            let (sock, _peer) = listener.accept().await.unwrap();
            let mut window = ReplayWindow::new();
            let mut conn = accept_inbound(sock, &psk_server, &mut window)
                .await
                .unwrap();
            // Echo: receive one frame, send one frame.
            let received = conn.recv().await.unwrap();
            conn.send(Bytes::from(format!(
                "echo: {}",
                String::from_utf8_lossy(&received)
            )))
            .await
            .unwrap();
            // Receive a second frame to prove counters
            // ratchet correctly on both sides.
            let second = conn.recv().await.unwrap();
            assert_eq!(&second[..], b"second");
            conn.close().await.unwrap();
        });

        let mut conn = transport
            .connect(&Endpoint::Direct(bound_addr))
            .await
            .unwrap();
        conn.send(Bytes::from_static(b"hello SS-2022"))
            .await
            .unwrap();
        let resp = conn.recv().await.unwrap();
        assert_eq!(&resp[..], b"echo: hello SS-2022");
        conn.send(Bytes::from_static(b"second")).await.unwrap();
        conn.close().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn handshake_then_first_body_frame_does_not_reuse_prelude_nonce() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        // Drive the real handshake through the production
        // connect() path, capture the client's FIRST body
        // frame off the wire, and prove it is sealed at the
        // prelude continuation counter (SS_PRELUDE_NONCE_COUNT)
        // — NOT at nonce 0/1, which the request prelude already
        // used on the same client->server subkey. This proves
        // the fix flows through ShadowsocksConnection::new, not
        // just the primitive.
        const FIRST_FRAME: &[u8] = b"first body frame after the prelude";

        let psk = [0x42u8; KEY_LEN];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (mut rd, mut wr) = sock.into_split();

            // Read the client request prelude off the wire.
            let mut head = vec![0u8; SS_SALT_LEN + SS_FIXED_HEADER_SEALED_LEN];
            rd.read_exact(&mut head).await.unwrap();
            let mut salt = [0u8; SS_SALT_LEN];
            salt.copy_from_slice(&head[..SS_SALT_LEN]);
            let subkey_peek = derive_session_subkey(&psk, &salt);
            let fixed_pt = aead::open(
                ALG,
                &subkey_peek,
                &nonce_from_counter(0),
                &[],
                &head[SS_SALT_LEN..],
            )
            .unwrap();
            let var_len = u16::from_be_bytes(fixed_pt[9..11].try_into().unwrap()) as usize;
            let mut sealed_var = vec![0u8; var_len + TAG_LEN];
            rd.read_exact(&mut sealed_var).await.unwrap();
            let mut full = head.clone();
            full.extend_from_slice(&sealed_var);
            let mut window = ReplayWindow::new();
            let (_req, request_salt, tx_subkey) =
                decode_request(&psk, &full, now_unix(), &mut window).unwrap();

            // Send a valid response prelude so connect() returns
            // and the client sends its first body frame.
            let (resp_wire, _rs, _rx) = encode_response(
                &psk,
                &ResponseHeader {
                    timestamp: now_unix(),
                    request_salt,
                    variable: Vec::new(),
                },
            )
            .unwrap();
            wr.write_all(&resp_wire).await.unwrap();
            wr.flush().await.unwrap();

            // Read exactly the first body frame: sealed length
            // (LENGTH_PREFIX + TAG) + sealed payload (N + TAG).
            let total =
                (crate::aead_stream::LENGTH_PREFIX_LEN + TAG_LEN) + (FIRST_FRAME.len() + TAG_LEN);
            let mut body = vec![0u8; total];
            rd.read_exact(&mut body).await.unwrap();
            (tx_subkey, body)
        });

        let cfg = ShadowsocksConfig {
            remote: bound.to_string(),
            method: "2022-blake3-chacha20-poly1305".into(),
            psk_b64: B64_STANDARD.encode(psk),
        };
        let transport = ShadowsocksTransport::new(cfg).unwrap();
        let mut conn = transport.connect(&Endpoint::Direct(bound)).await.unwrap();
        conn.send(Bytes::from_static(FIRST_FRAME)).await.unwrap();

        let (tx_subkey, body) = server.await.unwrap();

        // A body reader that (buggily) restarts at 0 MUST fail
        // — proof the frame did not reuse a prelude nonce.
        {
            let (mut w, r) = tokio::io::duplex(4096);
            w.write_all(&body).await.unwrap();
            drop(w);
            let mut wrong = AeadStreamReader::new(r, tx_subkey.clone());
            assert!(
                matches!(wrong.read_chunk().await, Err(AeadStreamError::AeadOpen)),
                "first body frame opened at nonce 0 — prelude/body (key, nonce) reuse present"
            );
        }
        // A body reader resuming at SS_PRELUDE_NONCE_COUNT opens
        // it verbatim.
        {
            let (mut w, r) = tokio::io::duplex(4096);
            w.write_all(&body).await.unwrap();
            drop(w);
            let mut right =
                AeadStreamReader::new_with_counter(r, tx_subkey, SS_PRELUDE_NONCE_COUNT);
            let got = right.read_chunk().await.unwrap().unwrap();
            assert_eq!(&got[..], FIRST_FRAME);
        }
        let _ = conn.close().await;
    }

    #[tokio::test]
    async fn tcp_connect_rejects_wrong_psk() {
        let psk_server = [0x10u8; KEY_LEN];
        let psk_client = [0x20u8; KEY_LEN];
        let cfg = ShadowsocksConfig {
            remote: "127.0.0.1:0".into(),
            method: "2022-blake3-chacha20-poly1305".into(),
            psk_b64: B64_STANDARD.encode(psk_client),
        };
        let transport = ShadowsocksTransport::new(cfg).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut window = ReplayWindow::new();
            let result = accept_inbound(sock, &psk_server, &mut window).await;
            assert!(result.is_err());
        });

        // Client side may either error during handshake
        // (server hangs up the peer after rejecting) or
        // successfully send the prelude but find the
        // connection closed before the response prelude.
        // Either way, this connect attempt must NOT
        // succeed.
        let result = transport.connect(&Endpoint::Direct(bound)).await;
        assert!(result.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn listener_accept_pairs_with_transport_connect() {
        // Symmetric pair via the high-level Listener
        // API: ShadowsocksListener::bind on one side,
        // ShadowsocksTransport::connect on the other,
        // round-trip a frame both ways.
        let psk = [0x99u8; KEY_LEN];
        let mut listener = ShadowsocksListener::bind("127.0.0.1:0".parse().unwrap(), psk)
            .await
            .unwrap();
        let bound = match listener.local_endpoint() {
            Endpoint::Direct(addr) => addr,
            Endpoint::Hostname { .. } => panic!("bind expected to yield Direct endpoint"),
        };

        let psk_b64 = B64_STANDARD.encode(psk);
        let cfg = ShadowsocksConfig {
            remote: bound.to_string(),
            method: "2022-blake3-chacha20-poly1305".into(),
            psk_b64,
        };
        let transport = ShadowsocksTransport::new(cfg).unwrap();

        // Spawn the server-side accept loop for exactly
        // one connection, then echo the body.
        let server = tokio::spawn(async move {
            let mut conn = listener.accept().await.unwrap();
            let received = conn.recv().await.unwrap();
            conn.send(Bytes::from(format!(
                "listener: {}",
                String::from_utf8_lossy(&received)
            )))
            .await
            .unwrap();
            conn.close().await.unwrap();
        });

        let mut conn = transport.connect(&Endpoint::Direct(bound)).await.unwrap();
        conn.send(Bytes::from_static(b"via listener API"))
            .await
            .unwrap();
        let echo = conn.recv().await.unwrap();
        assert_eq!(&echo[..], b"listener: via listener API");
        conn.close().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn listener_accepts_two_distinct_clients_in_sequence() {
        // The replay window lives on the listener and
        // must NOT block legitimate second-client
        // handshakes — each fresh client picks a fresh
        // salt, so the window only fires on actual
        // replays. Verifies the window does not
        // accidentally reject distinct sessions.
        let psk = [0x77u8; KEY_LEN];
        let mut listener = ShadowsocksListener::bind("127.0.0.1:0".parse().unwrap(), psk)
            .await
            .unwrap();
        let bound = match listener.local_endpoint() {
            Endpoint::Direct(addr) => addr,
            Endpoint::Hostname { .. } => unreachable!(),
        };
        let psk_b64 = B64_STANDARD.encode(psk);
        let cfg = ShadowsocksConfig {
            remote: bound.to_string(),
            method: "2022-blake3-chacha20-poly1305".into(),
            psk_b64,
        };
        let transport = ShadowsocksTransport::new(cfg).unwrap();

        let server = tokio::spawn(async move {
            for expected in [&b"client-a"[..], &b"client-b"[..]] {
                let mut conn = listener.accept().await.unwrap();
                let got = conn.recv().await.unwrap();
                assert_eq!(&got[..], expected);
                conn.close().await.unwrap();
            }
        });

        for label in [&b"client-a"[..], &b"client-b"[..]] {
            let mut conn = transport.connect(&Endpoint::Direct(bound)).await.unwrap();
            conn.send(Bytes::copy_from_slice(label)).await.unwrap();
            conn.close().await.unwrap();
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_send_oversize_frame_rejected() {
        let psk = [0x77u8; KEY_LEN];
        let cfg = ShadowsocksConfig {
            remote: "127.0.0.1:0".into(),
            method: "2022-blake3-chacha20-poly1305".into(),
            psk_b64: B64_STANDARD.encode(psk),
        };
        let transport = ShadowsocksTransport::new(cfg).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();

        let psk_server = psk;
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut window = ReplayWindow::new();
            // Server accepts but never reads the body —
            // client's oversize-send fails locally before
            // any byte hits the wire.
            let _ = accept_inbound(sock, &psk_server, &mut window)
                .await
                .unwrap();
            // Hold the connection long enough for the
            // client's send to land.
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        let mut conn = transport.connect(&Endpoint::Direct(bound)).await.unwrap();
        let oversized = vec![0u8; AEAD_MAX_CHUNK + 1];
        let err = conn.send(Bytes::from(oversized)).await.unwrap_err();
        assert!(matches!(err, Error::FrameTooLarge { .. }));
        let _ = conn.close().await;
        let _ = server.await;
    }
}
