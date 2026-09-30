// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`CallInviteCapability`] — link-based group-call invite
//! per ROADMAP 3c3.
//!
//! Sibling of [`crate::invite::InviteCapability`] (the
//! contact-invite capability from ADR-0005 §13.3). Same
//! single-file manual codec + Ed25519-over-the-prefix
//! signature, different payload:
//!
//! - The contact invite seeds a `ContactRequest` back to
//!   the owner so the joiner becomes a contact.
//! - The call invite seeds **a group call**: the joiner
//!   bootstraps a fresh MLS group using the embedded
//!   one-time `KeyPackage` (per the Phase 3c1
//!   [`crate::group_call::GroupCallSession`] design with
//!   `GroupCallScope::LinkBased`) and immediately becomes
//!   a member.
//!
//! Wire flow:
//!
//! ```text
//!  owner Alice                        joiner Bob
//!     │                                   │
//!     │── mint call_id (16 random bytes)  │
//!     │── mint one-time KeyPackage         │
//!     │── compose CallInviteCapability    │
//!     │       (signed by Alice's Ed25519) │
//!     │── seal under a fresh link secret (crate::link_seal)
//!     │── publish via https://lakko.me/c/<id>#<secret>
//!     │
//!     │           link out-of-band        │
//!     │ ────────────────────────────────► │
//!     │                                   │
//!     │                                   │ ── open_link (secret) + verify
//!     │                                   │ ── check is_valid_now
//!     │                                   │ ── extract one_time_key_package
//!     │                                   │ ── join_from_welcome(...)
//!     │                                   │    (server-side flow lands in
//!     │                                   │     a later sprint; this
//!     │                                   │     module ships the codec)
//! ```
//!
//! ## What ships in this V1
//!
//! - Wire codec (compose / decode_and_verify)
//! - Signature verification + expiry check
//! - Embedded one-time `KeyPackage` (opaque bytes from
//!   `lakko-mls`; this crate doesn't know the MLS shape)
//! - Media-flags bitmap (audio / video)
//!
//! What lives outside this codec (now wired elsewhere):
//!
//! - The `services/discovery /discovery/call-invite/{blob_id}`
//!   server endpoint holds the capability blob and enforces
//!   `max_uses` (atomic `consume` handler + per-joiner dedup
//!   via the `call_invite_consumers` side table). Since the
//!   2026-09-28 security pass the ledger holds the capability
//!   SEALED under a secret carried only in the link fragment
//!   ([`seal_link`] / [`open_link`]): a link is
//!   `/c/<id>#<secret>` or, when the upload failed,
//!   `/c/<sealed blob>#<secret>`. A 48-bit id alone opens nothing.
//! - Joiner-side orchestration that turns the embedded KP into
//!   a live `GroupCallSession` lives in `lakko-calls`
//!   (`link_invite::accept_call_invite`).
//!
//! Still deferred:
//!
//! - KT-log inclusion proof for `owner_signature_pubkey`
//!   (TOFU until then, same as `invite.rs`).
//!
//! ## Wire format (manual codec, v2 — ADR-0060 §6)
//!
//! ```text
//! u8           schema version (= 2)
//! [32 bytes]   owner_signature_pubkey            — Ed25519 verifying key
//! [16 bytes]   owner_account_id                  — UUIDv7 raw
//! [16 bytes]   call_id                           — 16 random bytes
//! u64be        expiry_ms                          — UNIX millis; 0 = no expiry
//! u32be        max_uses                           — 0 = single-use
//! u8           media_flags                        — bit 0 = audio, bit 1 = video
//! u32be        key_package_len                    — bounded by MAX_KEY_PACKAGE_LEN
//! [kp_len]     key_package_bytes                  — opaque MLS KeyPackage wire bytes
//! u16be        label_len
//! [label_len]  label_bytes (UTF-8, ≤ MAX_LABEL_LEN)
//! [16 bytes]   link_id                            — random, the reach scope of this link
//! [37 bytes]   link_grant                         — ReachGrant, class `link` (SECRET)
//! [64 bytes]   signature                          — Ed25519 over the above
//! ```
//!
//! Multi-byte integers are big-endian. The signature
//! covers everything from the schema-version byte through
//! `link_grant` inclusive. v1 (no link grant) is refused — a
//! flag day, nothing is deployed.
//!
//! ## The link grant (ADR-0060 §6)
//!
//! The owner's mailbox is closed since ADR-0060, so the joiner's
//! cold-start message to the owner (the call Welcome of the first
//! joiner, or the KeyPackage forward of a later one, both riding a
//! [`crate::request::ContactRequest`] into the owner's mailbox) needs a
//! key. The capability carries the owner's `link` reach grant, scoped to
//! a random per-link `link_id` rather than to the `call_id`: several links
//! can be minted for one call, and revoking one must remove exactly its
//! key. Same rules as [`crate::invite`]: inside the signature and the link
//! seal, class `link` or refused, the owner registers the public key
//! (expiring with the link) before sharing it.
//!
//! The way back is the joiner's: its `ContactRequest` carries its own
//! `reply` grant ([`crate::request`]), with which the owner (or the call's
//! committer) delivers the Welcome to the joiner's closed mailbox.

use lakko_crypto::signing::{
    Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN as ED25519_PUB_LEN, SIGNATURE_LEN,
};

use crate::error::{ContactError, Result};
use crate::invite::{
    check_link_grant, decode_link_grant, BLOB_ID_LEN, LINK_ID_LEN, MAX_LABEL_LEN, NEVER_EXPIRES,
};
use crate::reach::{ReachGrant, REACH_GRANT_LEN};
use crate::request::ACCOUNT_ID_LEN;

// ============================================================================
// Owner-side revocation — protocol-shared helpers (3c3-revoke)
// ============================================================================

/// Domain-separator prefix for the canonical bytes the inviter
/// signs to prove a call-invite revocation request. Distinct
/// from [`crate::invite::REVOKE_DOMAIN_V1`] (`lakko-invite-
/// revoke/v1\0`) so a captured contact-invite revoke signature
/// cannot be replayed against a call-invite of the same
/// `blob_id` (or vice versa). Trailing NUL keeps the prefix's
/// byte length distinct from any incidental ASCII suffix.
///
/// The version is part of the domain so a future revocation
/// scheme (e.g. delegated revoke after KT-log rotation) gets a
/// fresh prefix without breaking existing signatures.
pub const CALL_INVITE_REVOKE_DOMAIN_V1: &[u8] = b"lakko-call-invite-revoke/v1\x00";

/// Build the canonical bytes the inviter signs to prove a
/// call-invite revocation request. Mirror of
/// [`crate::invite::revoke_canonical_bytes`] but with the
/// call-invite-specific domain prefix so the two revocation
/// surfaces stay cross-kind-isolated.
///
/// Layout:
///
/// ```text
/// CALL_INVITE_REVOKE_DOMAIN_V1 (28 bytes) ‖ blob_id (6 bytes) ‖ signed_at_ms (8 bytes BE)
/// ```
///
/// Pure deterministic helper. Both the discovery service
/// (verify path) and the platform clients (sign path) drive
/// against the bytes this function returns; consolidating the
/// layout here is what keeps the two sides in sync.
///
/// Replay protection is provided by `signed_at_ms` —
/// [`CALL_REVOKE_REPLAY_WINDOW_MS`] (10 minutes, shared with
/// the contact-invite revocation) bounds how stale a
/// signature can be before the server rejects it.
#[must_use]
pub fn call_revoke_canonical_bytes(blob_id: &[u8; BLOB_ID_LEN], signed_at_ms: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(CALL_INVITE_REVOKE_DOMAIN_V1.len() + BLOB_ID_LEN + 8);
    out.extend_from_slice(CALL_INVITE_REVOKE_DOMAIN_V1);
    out.extend_from_slice(blob_id);
    out.extend_from_slice(&signed_at_ms.to_be_bytes());
    out
}

/// Sign a call-invite revocation. Mirror of
/// [`crate::invite::sign_revocation`] using the call-invite
/// revoke domain. Caller supplies the same Ed25519 signing
/// key that signed the original [`CallInviteCapability`] —
/// the server-side verify path checks the signature against
/// the embedded `owner_signature_pubkey` it stored when the
/// blob was uploaded.
#[must_use]
pub fn call_sign_revocation(
    signing: &SigningKey,
    blob_id: &[u8; BLOB_ID_LEN],
    signed_at_ms: u64,
) -> [u8; SIGNATURE_LEN] {
    let canonical = call_revoke_canonical_bytes(blob_id, signed_at_ms);
    signing.sign(&canonical).to_bytes()
}

/// Re-export the replay window so call-invite callers
/// don't have to reach into `crate::invite` for it. Same
/// 10-minute symmetric window applies to both revocation
/// surfaces — different domain separators, same freshness
/// ceiling.
pub use crate::invite::REVOKE_REPLAY_WINDOW_MS as CALL_REVOKE_REPLAY_WINDOW_MS;

/// Current schema version on the wire. v2 (ADR-0060) added the link id and
/// the link reach grant; v1 is refused.
const SCHEMA_VERSION: u8 = 2;

/// Length of `call_id` in bytes. Mirror of
/// `lakko_calls::signaling::CallId`'s 16-byte shape.
pub const CALL_ID_LEN: usize = 16;

/// Hard cap on the embedded `KeyPackage` wire bytes. A
/// real OpenMLS `KeyPackage` for the production ciphersuite
/// (`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`) lands
/// around 250–400 bytes; we cap at 4 KiB to keep URL
/// lengths sane and to bound parse-time work without
/// having an exact upstream upper bound to point at.
pub const MAX_KEY_PACKAGE_LEN: usize = 4 * 1024;

/// Re-export the contact-invite constants so callers that
/// only `use crate::call_invite::*` get a coherent surface.
/// `NEVER_EXPIRES` (= 0) means no expiry; `SINGLE_USE` (= 0)
/// means the link can be consumed exactly once.
pub use crate::invite::{NEVER_EXPIRES as CALL_NEVER_EXPIRES, SINGLE_USE as CALL_SINGLE_USE};

/// How long a link to a live call stays usable, in milliseconds.
///
/// `docs/ux/call-links.md` section 4. The person is no longer asked, so the
/// value is decided once — here, rather than three times in three clients.
/// It is here and not in `lakko-ui-rules` because it is not presentation: it
/// is a field of a signed capability, and the wire is what it has to agree
/// with.
///
/// **Twelve hours**, between two bounds the spec states rather than a number
/// it picks. The floor is a call's own length, because a link handed out at
/// the start must still work for someone arriving near the end of a meeting
/// that overran. The ceiling is exposure: the link is a standing invitation
/// to *ask* into a room, and one pasted this morning should be dead tonight
/// rather than sitting in a chat history someone scrolls back to months
/// later.
///
/// The ceiling is soft for the reason the spec gives itself — the host
/// admits every arrival ([ADR-0032], [ADR-0027]), so the expiry bounds how
/// long a stale link can be offered at a call that has almost certainly
/// ended, and never who gets in. That is why a wider window would not be
/// unsafe and a narrower one buys nothing.
///
/// [ADR-0032]: https://github.com/lakkome/lakko/blob/main/docs/adr/0032-multi-joiner-call-links.md
/// [ADR-0027]: https://github.com/lakkome/lakko/blob/main/docs/adr/0027-external-join-approval.md
pub const CALL_INVITE_LIFETIME_MS: u64 = 12 * 60 * 60 * 1000;

// The two bounds the spec states, checked when this compiles rather than
// when a test runs: a value outside them is not a failing assertion to read
// later, it is a build that does not happen.
//
// The floor is a call's own length - a link handed out at the start must
// still work for someone arriving near the end of a meeting that overran.
// The ceiling is the next day - one pasted this morning should not still be
// an invitation next week.
const _: () = assert!(CALL_INVITE_LIFETIME_MS >= 8 * 60 * 60 * 1000);
const _: () = assert!(CALL_INVITE_LIFETIME_MS <= 24 * 60 * 60 * 1000);

/// The expiry stamp for a link minted at `now_ms`.
///
/// Saturating, and never `CALL_NEVER_EXPIRES`: a clock near the end of the
/// epoch would otherwise wrap to a small number, and `0` in that field means
/// *never expires* — the one value this must not produce by accident, since
/// its own documentation names it a stalking risk. A saturated stamp is a
/// link that outlives everyone, which is wrong but honest; a wrapped one
/// would be a link that silently never dies.
#[must_use]
pub const fn call_invite_expiry_ms(now_ms: u64) -> u64 {
    match now_ms.checked_add(CALL_INVITE_LIFETIME_MS) {
        Some(at) if at != CALL_NEVER_EXPIRES => at,
        _ => u64::MAX,
    }
}

/// How many people a link to a live call admits: no limit.
///
/// `docs/ux/call-links.md` section 4. A count here would cap a meeting for
/// no gain, because the host admits each arrival either way — the limit
/// would stop the tenth guest at the door of a room the host was holding
/// open.
pub const CALL_INVITE_MAX_USES: u32 = u32::MAX;

/// Longest capability wire [`compose`] can produce: the fixed fields, a
/// [`MAX_KEY_PACKAGE_LEN`] KeyPackage, a [`MAX_LABEL_LEN`] label, the link
/// id, the link grant and the signature (4361 bytes).
pub const MAX_WIRE_LEN: usize = 1
    + ED25519_PUB_LEN
    + ACCOUNT_ID_LEN
    + CALL_ID_LEN
    + 8
    + 4
    + 1
    + 4
    + MAX_KEY_PACKAGE_LEN
    + 2
    + MAX_LABEL_LEN
    + LINK_ID_LEN
    + REACH_GRANT_LEN
    + SIGNATURE_LEN;

/// Longest sealed `/c/` link blob (what the discovery ledger stores).
pub const MAX_SEALED_LEN: usize = crate::link_seal::sealed_len(MAX_WIRE_LEN);

/// Seal a composed capability wire for a `/c/` link under `link_secret` (the
/// 2026-09-28 security pass, see [`crate::link_seal`]). The secret goes in
/// the URL fragment only; without it a fetched `/c/<id>` blob is not a seat in
/// the call.
///
/// # Errors
///
/// [`ContactError::InvalidArgument`] for an empty wire; a crypto failure.
pub fn seal_link(
    link_secret: &[u8; crate::link_seal::LINK_SECRET_LEN],
    capability_wire: &[u8],
) -> Result<Vec<u8>> {
    crate::link_seal::seal(
        crate::link_seal::LinkKind::Call,
        link_secret,
        capability_wire,
    )
}

/// Open a sealed `/c/` link blob under the fragment's secret and verify the
/// capability inside. Returns the capability and its plaintext wire.
///
/// # Errors
///
/// [`ContactError::LinkDamaged`] on any failure.
pub fn open_link(
    link_secret: &[u8; crate::link_seal::LINK_SECRET_LEN],
    sealed: &[u8],
) -> Result<(CallInviteCapability, Vec<u8>)> {
    crate::link_seal::open_and_verify(
        crate::link_seal::LinkKind::Call,
        link_secret,
        sealed,
        decode_and_verify,
    )
}

/// Media kinds the invited call supports. Wire bitmap:
/// `bit 0 = audio`, `bit 1 = video`. Other bits are
/// reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallInviteMedia {
    /// `true` when audio is supported.
    pub audio: bool,
    /// `true` when video is supported.
    pub video: bool,
}

impl CallInviteMedia {
    /// `audio` + `video`.
    #[must_use]
    pub const fn audio_video() -> Self {
        Self {
            audio: true,
            video: true,
        }
    }

    /// `audio` only.
    #[must_use]
    pub const fn audio_only() -> Self {
        Self {
            audio: true,
            video: false,
        }
    }

    /// Encode to the wire byte. Bit 0 = audio, bit 1 = video.
    #[must_use]
    pub fn into_byte(self) -> u8 {
        let mut bits = 0_u8;
        if self.audio {
            bits |= 0b01;
        }
        if self.video {
            bits |= 0b10;
        }
        bits
    }

    /// Decode from the wire byte. Unknown bits are
    /// silently ignored (forward-compatible).
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        Self {
            audio: b & 0b01 != 0,
            video: b & 0b10 != 0,
        }
    }
}

// ============================================================================
// CallInviteCapability — wire shape
// ============================================================================

/// Capability granted by the link owner to a single joiner
/// (or `max_uses` joiners). Decoded + verified from the
/// base64url-encoded slug at the end of the
/// `https://lakko.me/c/<slug>` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallInviteCapability {
    /// Owner's long-term Ed25519 verifying key. Signature
    /// over the canonical bytes is verified against this.
    pub owner_signature_pubkey: VerifyingKey,
    /// Owner's UUIDv7 raw bytes. The joiner uses this to
    /// route signaling traffic back to the owner once the
    /// call group is bootstrapped.
    pub owner_account_id: [u8; ACCOUNT_ID_LEN],
    /// 16-byte random call id. The owner mints this when
    /// composing the capability; every joiner uses the
    /// **same** id so the group call ledger
    /// (`GroupCallSession`) shares its membership across
    /// all joiners.
    pub call_id: [u8; CALL_ID_LEN],
    /// UNIX milliseconds when the invite stops being valid.
    /// `CALL_NEVER_EXPIRES` (= 0) means no expiry — UI
    /// MUST surface this as a stalking-risk warning.
    pub expiry_ms: u64,
    /// Maximum number of joiners the invite supports.
    /// `CALL_SINGLE_USE` (= 0) is the typical case. V1
    /// keeps this as a hint; server-side enforcement
    /// lands when the discovery endpoint does.
    pub max_uses: u32,
    /// Media flags — audio / video toggles.
    pub media: CallInviteMedia,
    /// Owner's one-time `KeyPackage` wire bytes. The
    /// joiner uses this to bootstrap the call MLS group
    /// with the owner already on board (the joiner adds
    /// themselves as the first call participant after the
    /// owner's KP is consumed). Opaque from this crate's
    /// perspective — interop with `lakko-mls` happens at
    /// the caller.
    pub one_time_key_package: Vec<u8>,
    /// Optional human-visible label, UTF-8, capped at
    /// [`MAX_LABEL_LEN`]. Empty string is allowed.
    pub label: String,
    /// Random per-link id: the scope of [`Self::link_grant`] (ADR-0060 §6).
    pub link_id: [u8; LINK_ID_LEN],
    /// The owner's `link` reach grant for this link (ADR-0060 §6). SECRET:
    /// the joiner signs its cold-start message to the owner's mailbox with
    /// it. Always class `link`.
    pub link_grant: ReachGrant,
}

impl CallInviteCapability {
    /// `true` iff the capability has not expired at
    /// `now_ms`. `CALL_NEVER_EXPIRES` capabilities always
    /// return `true` (stalking-risk surfacing is the UI's
    /// job).
    #[must_use]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.expiry_ms == NEVER_EXPIRES || now_ms < self.expiry_ms
    }

    /// Signal-style safety number derived from the owner's
    /// long-term Ed25519 identity key. 71-byte ASCII string
    /// (12 groups of 5 digits, single space between
    /// groups) — same format as
    /// [`crate::safety::safety_number`].
    ///
    /// **Purpose: anti-phishing.** Joiner UI shows this
    /// before letting the human click Accept. The human
    /// compares the displayed digits against what the
    /// owner shared out-of-band (in a Signal chat, on a
    /// business card, over the phone). If they don't
    /// match, the link is from someone else — typically a
    /// substitution attack where an attacker swapped a
    /// real `/c/<slug>` URL for one pointing at their own
    /// account.
    ///
    /// Order-independent on the inputs (the function reuses
    /// the two-party `safety_number(a, b)` with both args
    /// equal to the owner pubkey; deterministic from any
    /// 32-byte key). This is the same Signal-protocol
    /// fingerprint format already used for contact
    /// verification, so the human only learns one shape.
    #[must_use]
    pub fn owner_fingerprint(&self) -> String {
        crate::safety::safety_number(&self.owner_signature_pubkey, &self.owner_signature_pubkey)
    }
}

/// Compose + sign a call-invite capability. The
/// `one_time_key_package` bytes are stored opaque — the
/// caller (a `lakko-mls` consumer) is responsible for
/// minting them.
///
/// # Errors
///
/// `link_id` and `link_grant` are the link's reach scope and grant
/// (ADR-0060 §6, see the module docs).
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when:
///   - `label.len() > MAX_LABEL_LEN`
///   - `one_time_key_package.len() == 0`
///   - `one_time_key_package.len() > MAX_KEY_PACKAGE_LEN`
///   - `link_grant` is not class `link`
#[allow(clippy::too_many_arguments)]
pub fn compose(
    owner_signing: &SigningKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    call_id: &[u8; CALL_ID_LEN],
    expiry_ms: u64,
    max_uses: u32,
    media: CallInviteMedia,
    one_time_key_package: &[u8],
    label: &str,
    link_id: &[u8; LINK_ID_LEN],
    link_grant: &ReachGrant,
) -> Result<Vec<u8>> {
    if label.len() > MAX_LABEL_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "call-invite label is {} bytes, max is {MAX_LABEL_LEN}",
            label.len(),
        )));
    }
    if one_time_key_package.is_empty() {
        return Err(ContactError::InvalidArgument(
            "call-invite one_time_key_package must be non-empty".into(),
        ));
    }
    if one_time_key_package.len() > MAX_KEY_PACKAGE_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "call-invite one_time_key_package is {} bytes, max is {MAX_KEY_PACKAGE_LEN}",
            one_time_key_package.len(),
        )));
    }
    check_link_grant(link_grant)?;

    let owner_pubkey = owner_signing.verifying_key();
    let mut wire = encode_signing_prefix(
        &owner_pubkey,
        owner_account_id,
        call_id,
        expiry_ms,
        max_uses,
        media,
        one_time_key_package,
        label,
        link_id,
        link_grant,
    );
    let signature = owner_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Decode + verify a call-invite capability.
///
/// Does **not** check `expiry_ms` — call
/// [`CallInviteCapability::is_valid_now`] separately so the
/// UI can render "this invite expired 3 days ago" rather
/// than a generic failure.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format
///   malformed, unsupported schema version, EOF, KP length
///   beyond [`MAX_KEY_PACKAGE_LEN`], label beyond
///   [`MAX_LABEL_LEN`], or signature verification failed.
pub fn decode_and_verify(wire: &[u8]) -> Result<CallInviteCapability> {
    let mut cur = wire;
    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported CallInviteCapability schema version {version}",
        )));
    }
    let owner_signature_pubkey_bytes = take_n(&mut cur, ED25519_PUB_LEN)?;
    let owner_account_id_bytes = take_n(&mut cur, ACCOUNT_ID_LEN)?;
    let call_id_bytes = take_n(&mut cur, CALL_ID_LEN)?;
    let expiry_ms_bytes = take_n(&mut cur, 8)?;
    let max_uses_bytes = take_n(&mut cur, 4)?;
    let media_byte = take_u8(&mut cur)?;
    let kp_len_bytes = take_n(&mut cur, 4)?;
    let kp_len = u32::from_be_bytes([
        kp_len_bytes[0],
        kp_len_bytes[1],
        kp_len_bytes[2],
        kp_len_bytes[3],
    ]);
    if kp_len == 0 {
        return Err(ContactError::InvalidEncoding(
            "call-invite key_package_len is zero".into(),
        ));
    }
    let kp_len_usize = usize::try_from(kp_len).map_err(|_| {
        ContactError::InvalidEncoding("call-invite key_package_len overflows usize".into())
    })?;
    if kp_len_usize > MAX_KEY_PACKAGE_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "call-invite key_package_len {kp_len_usize} > MAX_KEY_PACKAGE_LEN {MAX_KEY_PACKAGE_LEN}"
        )));
    }
    let key_package = take_n(&mut cur, kp_len_usize)?.to_vec();
    let label_len_bytes = take_n(&mut cur, 2)?;
    let label_len = u16::from_be_bytes([label_len_bytes[0], label_len_bytes[1]]);
    let label_len_usize = usize::from(label_len);
    if label_len_usize > MAX_LABEL_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "call-invite label_len {label_len_usize} > MAX_LABEL_LEN {MAX_LABEL_LEN}"
        )));
    }
    let label_bytes = take_n(&mut cur, label_len_usize)?.to_vec();
    let link_id_bytes = take_n(&mut cur, LINK_ID_LEN)?;
    let link_grant_bytes = take_n(&mut cur, REACH_GRANT_LEN)?;
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "trailing {} bytes after call-invite signature",
            cur.len(),
        )));
    }

    // Verify the Ed25519 signature against the embedded
    // public key. The signed bytes are everything before
    // the signature itself.
    let signing_input = &wire[..wire.len() - SIGNATURE_LEN];
    let owner_pubkey = VerifyingKey::from_bytes(owner_signature_pubkey_bytes).map_err(|_| {
        ContactError::InvalidEncoding("call-invite owner_signature_pubkey malformed".into())
    })?;
    let sig = Signature::from_bytes(signature_bytes)
        .map_err(|_| ContactError::InvalidEncoding("call-invite signature length wrong".into()))?;
    owner_pubkey
        .verify(signing_input, &sig)
        .map_err(|_| ContactError::InvalidEncoding("call-invite signature invalid".into()))?;

    let mut owner_account_id = [0_u8; ACCOUNT_ID_LEN];
    owner_account_id.copy_from_slice(owner_account_id_bytes);
    let mut call_id = [0_u8; CALL_ID_LEN];
    call_id.copy_from_slice(call_id_bytes);
    let expiry_ms = u64::from_be_bytes([
        expiry_ms_bytes[0],
        expiry_ms_bytes[1],
        expiry_ms_bytes[2],
        expiry_ms_bytes[3],
        expiry_ms_bytes[4],
        expiry_ms_bytes[5],
        expiry_ms_bytes[6],
        expiry_ms_bytes[7],
    ]);
    let max_uses = u32::from_be_bytes([
        max_uses_bytes[0],
        max_uses_bytes[1],
        max_uses_bytes[2],
        max_uses_bytes[3],
    ]);
    let media = CallInviteMedia::from_byte(media_byte);
    let label = String::from_utf8(label_bytes)
        .map_err(|e| ContactError::InvalidEncoding(format!("call-invite label not UTF-8: {e}")))?;
    let mut link_id = [0_u8; LINK_ID_LEN];
    link_id.copy_from_slice(link_id_bytes);
    let link_grant = decode_link_grant(link_grant_bytes)?;

    Ok(CallInviteCapability {
        owner_signature_pubkey: owner_pubkey,
        owner_account_id,
        call_id,
        expiry_ms,
        max_uses,
        media,
        one_time_key_package: key_package,
        label,
        link_id,
        link_grant,
    })
}

// ============================================================================
// Internal codec helpers
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn encode_signing_prefix(
    owner_signature_pubkey: &VerifyingKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    call_id: &[u8; CALL_ID_LEN],
    expiry_ms: u64,
    max_uses: u32,
    media: CallInviteMedia,
    one_time_key_package: &[u8],
    label: &str,
    link_id: &[u8; LINK_ID_LEN],
    link_grant: &ReachGrant,
) -> Vec<u8> {
    let label_len_u16 =
        u16::try_from(label.len()).expect("label fits in u16; caller checked MAX_LABEL_LEN");
    let kp_len_u32 = u32::try_from(one_time_key_package.len())
        .expect("kp fits in u32; caller checked MAX_KEY_PACKAGE_LEN");
    let mut out = Vec::with_capacity(
        1 + ED25519_PUB_LEN
            + ACCOUNT_ID_LEN
            + CALL_ID_LEN
            + 8
            + 4
            + 1
            + 4
            + one_time_key_package.len()
            + 2
            + label.len()
            + LINK_ID_LEN
            + REACH_GRANT_LEN,
    );
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&owner_signature_pubkey.to_bytes());
    out.extend_from_slice(owner_account_id);
    out.extend_from_slice(call_id);
    out.extend_from_slice(&expiry_ms.to_be_bytes());
    out.extend_from_slice(&max_uses.to_be_bytes());
    out.push(media.into_byte());
    out.extend_from_slice(&kp_len_u32.to_be_bytes());
    out.extend_from_slice(one_time_key_package);
    out.extend_from_slice(&label_len_u16.to_be_bytes());
    out.extend_from_slice(label.as_bytes());
    out.extend_from_slice(link_id);
    out.extend_from_slice(&link_grant.encode());
    out
}

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    if cur.is_empty() {
        return Err(ContactError::InvalidEncoding("unexpected EOF".to_string()));
    }
    let b = cur[0];
    *cur = &cur[1..];
    Ok(b)
}

fn take_n<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if cur.len() < n {
        return Err(ContactError::InvalidEncoding(format!(
            "need {n} bytes, have {}",
            cur.len(),
        )));
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reach::ReachClass;

    const TEST_LINK_ID: [u8; LINK_ID_LEN] = [0x2D; LINK_ID_LEN];

    fn test_link_grant() -> ReachGrant {
        ReachGrant::derive(&[0x6E; 32], ReachClass::Link, &TEST_LINK_ID, 0).unwrap()
    }

    /// The pre-ADR-0060 argument list, with the test link id and grant.
    #[allow(clippy::too_many_arguments)]
    fn compose_t(
        owner_signing: &SigningKey,
        owner_account_id: &[u8; ACCOUNT_ID_LEN],
        call_id: &[u8; CALL_ID_LEN],
        expiry_ms: u64,
        max_uses: u32,
        media: CallInviteMedia,
        one_time_key_package: &[u8],
        label: &str,
    ) -> Result<Vec<u8>> {
        compose(
            owner_signing,
            owner_account_id,
            call_id,
            expiry_ms,
            max_uses,
            media,
            one_time_key_package,
            label,
            &TEST_LINK_ID,
            &test_link_grant(),
        )
    }

    #[test]
    fn the_link_id_and_grant_round_trip_inside_the_signature() {
        let link_id = crate::invite::generate_link_id();
        let grant = ReachGrant::derive(&[7; 32], ReachClass::Link, &link_id, 2).unwrap();
        let wire = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
            &link_id,
            &grant,
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.link_id, link_id);
        assert_eq!(cap.link_grant, grant);
        let grant_at = wire.len() - SIGNATURE_LEN - REACH_GRANT_LEN;
        for at in [grant_at - 1, grant_at + 5, wire.len() - SIGNATURE_LEN - 1] {
            let mut bad = wire.clone();
            bad[at] ^= 0x01;
            assert!(decode_and_verify(&bad).is_err());
        }
    }

    #[test]
    fn a_grant_of_another_class_is_refused_by_compose_and_decode() {
        for class in ReachClass::ALL {
            if class == ReachClass::Link {
                continue;
            }
            let scope: &[u8] = if class.is_account_wide() { b"" } else { b"s" };
            let grant = ReachGrant::derive(&[7; 32], class, scope, 1).unwrap();
            assert!(matches!(
                compose(
                    &fixture_signing(),
                    &fixture_account_id(),
                    &fixture_call_id(),
                    0,
                    0,
                    CallInviteMedia::audio_only(),
                    &fixture_kp(),
                    "",
                    &TEST_LINK_ID,
                    &grant,
                ),
                Err(ContactError::InvalidArgument(_))
            ));
            // The same grant spliced into a validly re-signed wire.
            let good = compose_t(
                &fixture_signing(),
                &fixture_account_id(),
                &fixture_call_id(),
                0,
                0,
                CallInviteMedia::audio_only(),
                &fixture_kp(),
                "",
            )
            .unwrap();
            let body = good.len() - SIGNATURE_LEN;
            let mut prefix = good[..body].to_vec();
            prefix[body - REACH_GRANT_LEN..].copy_from_slice(&grant.encode());
            let sig = fixture_signing().sign(&prefix);
            prefix.extend_from_slice(&sig.to_bytes());
            assert!(matches!(
                decode_and_verify(&prefix),
                Err(ContactError::InvalidEncoding(_))
            ));
        }
    }

    #[test]
    fn a_v1_capability_is_refused() {
        let mut wire = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        wire[0] = 1;
        let body = wire.len() - SIGNATURE_LEN;
        let sig = fixture_signing().sign(&wire[..body]);
        wire[body..].copy_from_slice(&sig.to_bytes());
        assert!(matches!(
            decode_and_verify(&wire),
            Err(ContactError::InvalidEncoding(_))
        ));
    }

    fn fixture_signing() -> SigningKey {
        SigningKey::from_seed(&[0xAB; 32])
    }

    fn fixture_account_id() -> [u8; ACCOUNT_ID_LEN] {
        let mut id = [0_u8; ACCOUNT_ID_LEN];
        id[0] = 0x01;
        id[15] = 0xAB;
        id
    }

    fn fixture_call_id() -> [u8; CALL_ID_LEN] {
        let mut id = [0_u8; CALL_ID_LEN];
        id[0] = 0x10;
        id[15] = 0x10;
        id
    }

    fn fixture_kp() -> Vec<u8> {
        // Production OpenMLS KeyPackage is ~ 300–400 bytes
        // of TLS-codec'd wire; we use a fixture pattern
        // that's recognisable in hex dumps.
        let mut kp = vec![0x42; 256];
        kp[0] = 0xCA;
        kp[1] = 0xFE;
        kp
    }

    /// A maximal capability is exactly MAX_WIRE_LEN; the sealed `/c/` link
    /// opens under its secret only, and not behind another path.
    #[test]
    fn sealed_link_round_trips_and_refuses_a_wrong_secret() {
        let wire = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            CALL_INVITE_MAX_USES,
            CallInviteMedia::audio_video(),
            &vec![0x42; MAX_KEY_PACKAGE_LEN],
            &"y".repeat(MAX_LABEL_LEN),
        )
        .unwrap();
        assert_eq!(wire.len(), MAX_WIRE_LEN);
        let secret = crate::link_seal::generate_link_secret();
        let sealed = seal_link(&secret, &wire).unwrap();
        assert_eq!(sealed.len(), MAX_SEALED_LEN);
        let (cap, opened) = open_link(&secret, &sealed).unwrap();
        assert_eq!(opened, wire);
        assert_eq!(cap.call_id, fixture_call_id());
        assert!(matches!(
            open_link(&crate::link_seal::generate_link_secret(), &sealed),
            Err(ContactError::LinkDamaged)
        ));
        assert!(matches!(
            crate::group_invite::open_link(&secret, &sealed),
            Err(ContactError::LinkDamaged)
        ));
    }

    #[test]
    fn compose_decode_round_trips() {
        let signing = fixture_signing();
        let account_id = fixture_account_id();
        let call_id = fixture_call_id();
        let kp = fixture_kp();
        let label = "Alice's standup";

        let wire = compose_t(
            &signing,
            &account_id,
            &call_id,
            1_700_000_000_000,
            5,
            CallInviteMedia::audio_video(),
            &kp,
            label,
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();

        assert_eq!(
            parsed.owner_signature_pubkey.to_bytes(),
            signing.verifying_key().to_bytes()
        );
        assert_eq!(parsed.owner_account_id, account_id);
        assert_eq!(parsed.call_id, call_id);
        assert_eq!(parsed.expiry_ms, 1_700_000_000_000);
        assert_eq!(parsed.max_uses, 5);
        assert_eq!(parsed.media, CallInviteMedia::audio_video());
        assert_eq!(parsed.one_time_key_package, kp);
        assert_eq!(parsed.label, label);
    }

    #[test]
    fn audio_only_media_round_trips() {
        let wire = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            CALL_SINGLE_USE,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert_eq!(parsed.media, CallInviteMedia::audio_only());
    }

    #[test]
    fn never_expires_is_valid_at_any_time() {
        let wire = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            CALL_NEVER_EXPIRES,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(0));
        assert!(parsed.is_valid_now(u64::MAX));
    }

    #[test]
    fn expired_capability_is_invalid_after_expiry() {
        let wire = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            1_000,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(999));
        assert!(!parsed.is_valid_now(1_000));
        assert!(!parsed.is_valid_now(2_000));
    }

    #[test]
    fn tampered_payload_rejected() {
        let mut wire = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_video(),
            &fixture_kp(),
            "x",
        )
        .unwrap();
        // Flip a byte inside the KP region (~position 60).
        wire[60] ^= 0xFF;
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("signature"), "got: {msg}");
            },
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn truncated_wire_rejected() {
        let wire = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        // Drop the trailing signature.
        let truncated = &wire[..wire.len() - SIGNATURE_LEN - 1];
        assert!(decode_and_verify(truncated).is_err());
    }

    #[test]
    fn unsupported_schema_version_rejected() {
        let mut wire = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        wire[0] = 3; // bump version byte
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("schema version 3"), "got: {msg}");
            },
            other => panic!("expected unsupported-version, got {other:?}"),
        }
    }

    #[test]
    fn empty_key_package_rejected_at_compose() {
        let err = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &[],
            "",
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn oversize_key_package_rejected_at_compose() {
        let too_big = vec![0_u8; MAX_KEY_PACKAGE_LEN + 1];
        let err = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &too_big,
            "",
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn oversize_label_rejected_at_compose() {
        let too_long = "x".repeat(MAX_LABEL_LEN + 1);
        let err = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            &too_long,
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn cross_signing_key_rejected() {
        // Sign with one key, replace the embedded pubkey
        // with another's — signature verify must fail.
        let signing_a = SigningKey::from_seed(&[0xAA; 32]);
        let signing_b = SigningKey::from_seed(&[0xBB; 32]);
        let mut wire = compose_t(
            &signing_a,
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        // Splice B's pubkey at the schema-version + 1
        // offset. The signature was computed over A's
        // pubkey, so swapping in B's must invalidate it.
        let b_pubkey = signing_b.verifying_key().to_bytes();
        wire[1..=ED25519_PUB_LEN].copy_from_slice(&b_pubkey);
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("signature"), "got: {msg}");
            },
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn owner_fingerprint_is_deterministic_per_pubkey() {
        // Same pubkey → same fingerprint, regardless of
        // the capability's other fields.
        let cap_wire_a = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            1_000,
            5,
            CallInviteMedia::audio_video(),
            &fixture_kp(),
            "Alice",
        )
        .unwrap();
        let mut another_call_id = fixture_call_id();
        another_call_id[0] = 0xFF;
        let cap_wire_b = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &another_call_id,
            999_999,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "Different label",
        )
        .unwrap();
        let cap_a = decode_and_verify(&cap_wire_a).unwrap();
        let cap_b = decode_and_verify(&cap_wire_b).unwrap();
        assert_eq!(
            cap_a.owner_fingerprint(),
            cap_b.owner_fingerprint(),
            "fingerprint must depend ONLY on the owner pubkey"
        );
    }

    #[test]
    fn different_owners_have_different_fingerprints() {
        let signing_a = SigningKey::from_seed(&[0xAA; 32]);
        let signing_b = SigningKey::from_seed(&[0xBB; 32]);
        let cap_a = decode_and_verify(
            &compose_t(
                &signing_a,
                &fixture_account_id(),
                &fixture_call_id(),
                0,
                0,
                CallInviteMedia::audio_only(),
                &fixture_kp(),
                "",
            )
            .unwrap(),
        )
        .unwrap();
        let cap_b = decode_and_verify(
            &compose_t(
                &signing_b,
                &fixture_account_id(),
                &fixture_call_id(),
                0,
                0,
                CallInviteMedia::audio_only(),
                &fixture_kp(),
                "",
            )
            .unwrap(),
        )
        .unwrap();
        assert_ne!(cap_a.owner_fingerprint(), cap_b.owner_fingerprint());
    }

    #[test]
    fn owner_fingerprint_matches_safety_number_format() {
        let cap = decode_and_verify(
            &compose_t(
                &fixture_signing(),
                &fixture_account_id(),
                &fixture_call_id(),
                0,
                0,
                CallInviteMedia::audio_only(),
                &fixture_kp(),
                "",
            )
            .unwrap(),
        )
        .unwrap();
        let fingerprint = cap.owner_fingerprint();
        // 12 groups × 5 digits + 11 spaces = 71 chars.
        assert_eq!(fingerprint.len(), 71);
        // Each chunk must be ASCII digits.
        for chunk in fingerprint.split(' ') {
            assert_eq!(chunk.len(), 5);
            assert!(chunk.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn media_byte_codec_is_stable() {
        for case in [
            CallInviteMedia {
                audio: false,
                video: false,
            },
            CallInviteMedia::audio_only(),
            CallInviteMedia::audio_video(),
            CallInviteMedia {
                audio: false,
                video: true,
            },
        ] {
            assert_eq!(CallInviteMedia::from_byte(case.into_byte()), case);
        }
    }

    #[test]
    fn unknown_media_bits_are_silently_ignored() {
        // Forward-compatibility — a future schema bump
        // adding bit 2 must not break v1 decoders.
        let decoded = CallInviteMedia::from_byte(0b1111_1111);
        assert!(decoded.audio);
        assert!(decoded.video);
    }

    #[test]
    fn call_revoke_canonical_bytes_layout_is_stable() {
        let blob_id = [0xAB_u8; BLOB_ID_LEN];
        let bytes = call_revoke_canonical_bytes(&blob_id, 0x0102_0304_0506_0708);
        // Domain (28) + blob_id (6) + signed_at_ms (8) = 42 bytes.
        assert_eq!(
            bytes.len(),
            CALL_INVITE_REVOKE_DOMAIN_V1.len() + BLOB_ID_LEN + 8
        );
        assert_eq!(
            &bytes[..CALL_INVITE_REVOKE_DOMAIN_V1.len()],
            CALL_INVITE_REVOKE_DOMAIN_V1
        );
        assert_eq!(
            &bytes[CALL_INVITE_REVOKE_DOMAIN_V1.len()
                ..CALL_INVITE_REVOKE_DOMAIN_V1.len() + BLOB_ID_LEN],
            &blob_id[..]
        );
        // Last 8 bytes are big-endian signed_at_ms.
        let tail = &bytes[bytes.len() - 8..];
        assert_eq!(tail, &0x0102_0304_0506_0708_u64.to_be_bytes()[..]);
    }

    #[test]
    fn call_sign_revocation_is_deterministic_and_verifiable() {
        let signing = SigningKey::from_seed(&[0xAB; 32]);
        let blob_id = [0xCD_u8; BLOB_ID_LEN];
        let sig_a = call_sign_revocation(&signing, &blob_id, 1_700_000_000_000);
        let sig_b = call_sign_revocation(&signing, &blob_id, 1_700_000_000_000);
        assert_eq!(sig_a, sig_b);

        // Verify against the same canonical bytes.
        let canonical = call_revoke_canonical_bytes(&blob_id, 1_700_000_000_000);
        let signature = lakko_crypto::signing::Signature::from_bytes(&sig_a).unwrap();
        signing
            .verifying_key()
            .verify(&canonical, &signature)
            .expect("call-revoke signature must verify under owner's pubkey");
    }

    #[test]
    fn call_revoke_signature_does_not_validate_under_contact_invite_domain() {
        // The cross-kind isolation invariant: a call-invite
        // revoke signature must NOT verify when an attacker
        // tries to replay it through the contact-invite
        // `revoke_canonical_bytes` path. Different domain
        // prefixes (`lakko-call-invite-revoke/v1\0` vs
        // `lakko-invite-revoke/v1\0`) → different signed bytes
        // → signature check fails.
        let signing = SigningKey::from_seed(&[0xAB; 32]);
        let blob_id = [0xCD_u8; BLOB_ID_LEN];
        let signed_at_ms = 1_700_000_000_000_u64;

        let call_sig = call_sign_revocation(&signing, &blob_id, signed_at_ms);
        let signature = lakko_crypto::signing::Signature::from_bytes(&call_sig).unwrap();

        // Try to verify the call-invite signature against the
        // CONTACT-invite revoke domain. Must fail.
        let contact_canonical = crate::invite::revoke_canonical_bytes(&blob_id, signed_at_ms);
        assert!(
            signing
                .verifying_key()
                .verify(&contact_canonical, &signature)
                .is_err(),
            "call-invite revoke signature MUST NOT verify under contact-invite domain"
        );
    }

    #[test]
    fn call_revoke_signature_changes_with_signed_at_ms() {
        let signing = SigningKey::from_seed(&[0xAB; 32]);
        let blob_id = [0xCD_u8; BLOB_ID_LEN];
        let sig_a = call_sign_revocation(&signing, &blob_id, 1_700_000_000_000);
        let sig_b = call_sign_revocation(&signing, &blob_id, 1_700_000_001_000);
        assert_ne!(
            sig_a, sig_b,
            "different timestamps must produce different signatures"
        );
    }

    #[test]
    fn joiner_tag_works_on_call_invite_bytes() {
        // 3c3-multi-use server-side dedup reuses the
        // contact-invite `compute_joiner_tag` function:
        // it's generic over `capability_bytes` because
        // the salt is `SHA-256(domain || capability_bytes)`
        // — wholly different wire shapes produce wholly
        // different salts, so tags don't collide across
        // invite kinds even though one HMAC helper covers
        // both ecosystems.
        let cap_a = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        let mut other_call_id = fixture_call_id();
        other_call_id[0] = 0x99;
        let cap_b = compose_t(
            &fixture_signing(),
            &fixture_account_id(),
            &other_call_id,
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        let acct_a = [0xA1_u8; 16];
        let acct_b = [0xB2_u8; 16];

        let tag_cap_a_acct_a = crate::invite::compute_joiner_tag(&cap_a, &acct_a);
        let tag_cap_a_acct_a_repeat = crate::invite::compute_joiner_tag(&cap_a, &acct_a);
        let tag_cap_a_acct_other = crate::invite::compute_joiner_tag(&cap_a, &acct_b);
        let tag_cap_other_acct_a = crate::invite::compute_joiner_tag(&cap_b, &acct_a);

        // Determinism.
        assert_eq!(tag_cap_a_acct_a, tag_cap_a_acct_a_repeat);
        assert_eq!(tag_cap_a_acct_a.len(), crate::invite::JOINER_TAG_LEN);
        // Different account → different tag.
        assert_ne!(tag_cap_a_acct_a, tag_cap_a_acct_other);
        // Different capability (different call_id) → different
        // tag for the same joiner. Per-blob salt isolation
        // across distinct call invites.
        assert_ne!(tag_cap_a_acct_a, tag_cap_other_acct_a);
    }
}

#[cfg(test)]
mod live_call_link_values {
    use super::*;

    #[test]
    fn an_expiry_is_never_the_never_expires_value() {
        // `0` in this field means "never", which the field's own docs call a
        // stalking risk. No input may produce it.
        for now in [0, 1, CALL_NEVER_EXPIRES, u64::MAX, u64::MAX - 1] {
            assert_ne!(call_invite_expiry_ms(now), CALL_NEVER_EXPIRES);
        }
    }

    #[test]
    fn a_clock_at_the_end_of_time_saturates_rather_than_wrapping() {
        // Wrapping would produce a small stamp - or `0` - and a link that
        // silently never dies. Saturating is wrong in the safe direction.
        assert_eq!(call_invite_expiry_ms(u64::MAX), u64::MAX);
        assert_eq!(call_invite_expiry_ms(u64::MAX - 10), u64::MAX);
    }

    #[test]
    fn an_ordinary_clock_just_adds_the_window() {
        let now = 1_700_000_000_000;
        assert_eq!(call_invite_expiry_ms(now), now + CALL_INVITE_LIFETIME_MS);
    }

    #[test]
    fn the_link_admits_everyone_the_host_lets_in() {
        // Not `CALL_SINGLE_USE`, which is `0` and means exactly one.
        assert_ne!(CALL_INVITE_MAX_USES, CALL_SINGLE_USE);
        assert_eq!(CALL_INVITE_MAX_USES, u32::MAX);
    }
}
