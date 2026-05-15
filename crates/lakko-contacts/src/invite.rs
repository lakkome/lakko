// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`InviteCapability`] — capability-based contact invites per
//! ADR-0005 §13.3 / §13.4 (Phase 1.18a).
//!
//! Owner Alice generates a capability, embeds the wire bytes
//! (base64url) into a `https://lakko.me/i/<...>` link, shares the
//! link out-of-band. Joiner Bob opens the link, the web client
//! decodes + verifies the capability, and uses it to seed the
//! first ContactRequest back to Alice — short-circuiting the
//! `@username` lookup step from scenario A.
//!
//! ## What this V1 ships and what it doesn't
//!
//! ADR-0005 §13.3 specifies a richer `InviteCapability` than what
//! lands here:
//!
//!   * `owner_transparency_proof` (KT-log inclusion) — anchors
//!     `owner_signature_pubkey` to a verifiable identity record.
//!     Deferred: `lakko-transparency` does not yet expose
//!     inclusion proofs end-to-end. Without it, accepting a link
//!     is TOFU on the inviter's pubkey.
//!   * `consumed_pubkey_log: HmacBloom` — server-side bloom over
//!     joiners that have already consumed the link, paired with a
//!     `max_uses` enforcer. Deferred: needs a `services/discovery`
//!     `/invite/{blob_id}` endpoint that V1 does not have. The
//!     `max_uses` field is preserved on the wire as a **hint**
//!     for the joiner UI ("link can be used N more times") but
//!     no enforcement happens until 1.18d.
//!   * `blob_id` storage indirection — the ADR puts the
//!     capability bytes in `services/storage` and only embeds a
//!     short pointer in the URL. V1 puts the bytes **directly**
//!     in the URL. Trade-off: the URL is ~150-220 chars long
//!     instead of ~30, but no server dependency and no extra
//!     round-trip. We can swap to the indirection without a
//!     wire-format break — the capability struct stays the same;
//!     only the URL shape changes.
//!   * `ephemeral_prekey` — ADR keeps it for forward secrecy on
//!     the owner→joiner direction. Our flow has joiner→owner as
//!     the first sealed message, and the joiner generates a
//!     fresh ephemeral per envelope (`crate::envelope::seal`),
//!     so we don't need the owner-side prekey here.
//!
//! ## Wire format (manual codec, frozen at v1)
//!
//! ```text
//! u8           schema version (= 1)
//! [32 bytes]   owner_signature_pubkey            — Ed25519 verifying key
//! [16 bytes]   owner_account_id                  — UUIDv7 raw
//! [32 bytes]   owner_x25519_pubkey               — long-term HPKE init key
//! u64be        expiry_ms                          — UNIX millis; 0 means "no expiry"
//! u32be        max_uses                           — 0 means "single-use"
//! u8           bootstrap_referral                 — 0/1; flags signup deep-link
//! u16be        label_len
//! [label_len]  label_bytes (UTF-8, ≤ MAX_LABEL_LEN)
//! [64 bytes]   signature                          — Ed25519 over the above
//! ```
//!
//! Signature covers everything from the schema-version byte through
//! `label_bytes` inclusive — i.e. the entire encoding except the
//! trailing 64-byte signature itself. Mirrors `request.rs`'s
//! pattern exactly.
//!
//! All multi-byte integers are big-endian to match the rest of
//! Lakko's wire formats.
//!
//! ## Canonical use
//!
//! Sender side:
//!
//! ```ignore
//! let bytes = lakko_contacts::invite::compose(
//!     &owner_signing_key,
//!     &owner_account_id,
//!     &owner_x25519_pub,
//!     expiry_ms,
//!     max_uses,
//!     bootstrap_referral,
//!     "Alice's home",
//! )?;
//! let url = format!("https://lakko.me/i/{}", base64_url(&bytes));
//! ```
//!
//! Receiver side:
//!
//! ```ignore
//! let bytes = base64_url_decode(slug)?;
//! let cap = lakko_contacts::invite::decode_and_verify(&bytes)?;
//! if !cap.is_valid_now(now_ms) { /* expired */ }
//! // Use `cap.owner_signature_pubkey`, `cap.owner_account_id`,
//! // `cap.owner_x25519_pubkey` to seed the first ContactRequest.
//! ```

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, PUBLIC_KEY_LEN as X25519_PUB_LEN};
use lakko_crypto::signing::{
    Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN as ED25519_PUB_LEN, SIGNATURE_LEN,
};

use crate::error::{ContactError, Result};
use crate::request::ACCOUNT_ID_LEN;

/// Current schema version on the wire.
const SCHEMA_VERSION: u8 = 1;

/// Hard cap on the human-visible label. 64 bytes UTF-8 is enough
/// for "Alice — work account" without bloating the URL beyond a
/// few hundred chars.
pub const MAX_LABEL_LEN: usize = 64;

/// Sentinel meaning "never expires". Honest value here is
/// problematic in a security-critical surface (a forever invite
/// is a stalking risk per ADR-0005 §13.3 — discouraged in UI),
/// but we accept it on the wire so an enterprise / pinned-bio
/// UX can opt into it explicitly.
pub const NEVER_EXPIRES: u64 = 0;

/// Sentinel meaning "single-use".
pub const SINGLE_USE: u32 = 0;

// ============================================================================
// Owner-side revocation (Phase 1.18d2b) — protocol-shared helpers
// ============================================================================

/// Domain-separator prefix for the canonical bytes the inviter
/// signs to prove a revocation request (Phase 1.18d2b). The
/// trailing NUL pin keeps the prefix's byte length distinct
/// from any incidental ASCII suffix the `lakko-invite-…/v1`
/// namespace might grow. The version is part of the domain so
/// a future revocation scheme (e.g. delegated-revoke after
/// KT-log identity rotation) gets a fresh prefix without
/// breaking existing signatures.
pub const REVOKE_DOMAIN_V1: &[u8] = b"lakko-invite-revoke/v1\x00";

/// Replay window for the signed `signed_at_ms` field. Symmetric
/// (±10 min) so client / server clock skew tolerance is the
/// same as the freshness ceiling. A signed body older than this
/// is rejected; a body claiming to be from the future by more
/// than this is also rejected.
pub const REVOKE_REPLAY_WINDOW_MS: u64 = 10 * 60 * 1_000;

/// Length in bytes of a `blob_id` — SHA-256 of the
/// capability bytes, mirrors `services/discovery`'s
/// `BLOB_ID_LEN`. Pinned in the codec layer because the
/// canonical-bytes layout binds to it.
pub const BLOB_ID_LEN: usize = 32;

/// Build the canonical bytes the inviter signs to prove a
/// revocation request.
///
/// Layout (63 bytes total):
///
/// ```text
/// REVOKE_DOMAIN_V1 (23 bytes)  ‖ blob_id (32 bytes) ‖ signed_at_ms (8 bytes BE)
/// ```
///
/// Domain separator binds the signature to the revocation-v1
/// use case; `blob_id` binds it to the specific row;
/// `signed_at_ms` binds it to a freshness window so a captured
/// signature cannot be replayed past
/// [`REVOKE_REPLAY_WINDOW_MS`].
///
/// Pure deterministic helper — same inputs always produce the
/// same bytes. Both the server (`services/discovery`) and the
/// web client drive verify / sign against the value this
/// function returns; consolidating the layout here is what
/// keeps the two sides in sync.
#[must_use]
pub fn revoke_canonical_bytes(blob_id: &[u8; BLOB_ID_LEN], signed_at_ms: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(REVOKE_DOMAIN_V1.len() + BLOB_ID_LEN + 8);
    out.extend_from_slice(REVOKE_DOMAIN_V1);
    out.extend_from_slice(blob_id);
    out.extend_from_slice(&signed_at_ms.to_be_bytes());
    out
}

/// Sign a revocation request — convenience over
/// [`revoke_canonical_bytes`] + the Ed25519 sign primitive.
/// Caller supplies the inviter's signing key (the same one
/// they used to compose the capability) plus the request
/// timestamp; the helper builds the canonical bytes, signs,
/// and returns the 64-byte signature.
///
/// The web client drives this through wasm-bindgen
/// (`wasm.signInviteRevocation` in `lakko-ffi-wasm` 1.18d2b);
/// the server-side `services/discovery::invite::revoke` path
/// verifies the same bytes against the embedded
/// `owner_signature_pubkey`.
#[must_use]
pub fn sign_revocation(
    signing: &SigningKey,
    blob_id: &[u8; BLOB_ID_LEN],
    signed_at_ms: u64,
) -> [u8; SIGNATURE_LEN] {
    let canonical = revoke_canonical_bytes(blob_id, signed_at_ms);
    signing.sign(&canonical).to_bytes()
}

// ============================================================================
// Owner-side invite-list query (Phase 1.18d2b-list)
// ============================================================================

/// Domain-separator prefix for the canonical bytes an owner
/// signs to authenticate a "give me my live invites" query
/// (Phase 1.18d2b-list). Distinct from
/// [`REVOKE_DOMAIN_V1`] so a captured revoke-signature cannot
/// be replayed as an owner-list signature (or vice versa).
/// Trailing NUL keeps the prefix's byte length distinct from any
/// incidental ASCII suffix the `lakko-invite-…/v1` namespace
/// might grow.
pub const OWNER_LIST_DOMAIN_V1: &[u8] = b"lakko-invite-owner-list/v1\x00";

/// Build the canonical bytes the owner signs to authenticate a
/// "list my invites" query. The signature is verified server-
/// side against the owner's published Ed25519 verifying key
/// (from `identity_bundles`), proving ownership of the
/// `account_id` without requiring a session token to carry that
/// identity.
///
/// Layout (51 bytes total):
///
/// ```text
/// OWNER_LIST_DOMAIN_V1 (27 bytes) ‖ owner_account_id (16 bytes) ‖ signed_at_ms (8 bytes BE)
/// ```
///
/// Domain separator binds the signature to the owner-list-v1
/// use case; `owner_account_id` binds it to the specific owner;
/// `signed_at_ms` binds it to a freshness window so a captured
/// signature cannot be replayed past
/// [`REVOKE_REPLAY_WINDOW_MS`] (re-used — both endpoints share
/// the same ±10 min skew tolerance to keep the operational
/// story tight).
///
/// Pure deterministic helper — same inputs always produce the
/// same bytes. Both the server (`services/discovery`) and every
/// client drive verify / sign against the value this function
/// returns; consolidating the layout here is what keeps the
/// surfaces in sync.
///
/// **No nonce.** A list query is idempotent — server-side
/// replay within the window just produces another SELECT
/// returning the same rows. Anti-replay across the window
/// boundary is covered by `signed_at_ms`.
#[must_use]
pub fn owner_list_canonical_bytes(
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    signed_at_ms: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(OWNER_LIST_DOMAIN_V1.len() + ACCOUNT_ID_LEN + 8);
    out.extend_from_slice(OWNER_LIST_DOMAIN_V1);
    out.extend_from_slice(owner_account_id);
    out.extend_from_slice(&signed_at_ms.to_be_bytes());
    out
}

/// Sign an owner-list query — convenience over
/// [`owner_list_canonical_bytes`] + the Ed25519 sign primitive.
/// Caller supplies the owner's identity signing key (same one
/// they used to sign every `InviteCapability` they own) plus the
/// request timestamp; the helper builds the canonical bytes,
/// signs, and returns the 64-byte signature.
///
/// Each client drives this through its FFI surface
/// (`signOwnerListQuery` on UniFFI + wasm-bindgen, Phase
/// 1.18d2b-list); the server-side
/// `services/discovery::invite::list_by_owner` path verifies the
/// same bytes against `identity_bundles.signature_pubkey` for
/// the supplied `owner_account_id`.
#[must_use]
pub fn sign_owner_list(
    signing: &SigningKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    signed_at_ms: u64,
) -> [u8; SIGNATURE_LEN] {
    let canonical = owner_list_canonical_bytes(owner_account_id, signed_at_ms);
    signing.sign(&canonical).to_bytes()
}

// ============================================================================
// Per-joiner consume tag (Phase 1.18d3a) — server-blind dedup
// ============================================================================

/// Domain-separator prefix for the salt the joiner-tag HMAC
/// uses (Phase 1.18d3). Versioned so a future scheme bump
/// (e.g. switching from HMAC-SHA-256 to HMAC-BLAKE3) gets a
/// fresh prefix without colliding with old tags.
pub const JOINER_TAG_SALT_DOMAIN_V1: &[u8] = b"lakko-invite-bloom-salt/v1\x00";

/// Length of a joiner-tag — HMAC-SHA-256 output, 32 bytes.
pub const JOINER_TAG_LEN: usize = 32;

/// Compute the per-joiner consume-dedup tag for a given
/// `(capability_bytes, joiner_account_id)` pair.
///
/// Layout:
///
/// ```text
/// salt = SHA-256(JOINER_TAG_SALT_DOMAIN_V1 || capability_bytes)   // 32 bytes
/// tag  = HMAC-SHA-256(salt, joiner_account_id)                    // 32 bytes
/// ```
///
/// The double-stage construction (SHA-256 → HMAC) gives us:
///
///   * **Per-blob unique salt** — same joiner against two
///     different invites yields two different tags; the
///     server cannot link the two consume events.
///   * **Server-blindness** — server stores only the 32-byte
///     `tag` per `(blob_id, tag)` row; it never sees the raw
///     `joiner_account_id` and cannot reverse the HMAC to
///     recover it (one-way).
///   * **Determinism** — same `(capability, account_id)` pair
///     always produces the same tag, so a re-consume from the
///     same joiner conflicts on the row's PK and the server
///     idempotently returns "already consumed" without
///     bumping the counter.
///   * **No wire-format bump** — salt is derived from
///     `capability_bytes` (which the joiner already has after
///     decoding the URL slug), so 1.18a's wire format stays
///     unchanged.
///
/// Privacy ceiling: an attacker holding the capability bytes
/// AND a candidate `joiner_account_id` can recompute the tag
/// and submit it as a consume. The server's PK conflict
/// disambiguates "joiner X has consumed" from "joiner X has
/// not consumed", which is a probing oracle. PoW + rate
/// limits on consume attempts mitigate at scale; full
/// privacy against a determined attacker requires an OPRF
/// step the server runs blind, which is the deferred
/// alternative noted in the ROADMAP.
///
/// Pure deterministic helper. Both web client (via wasm-bindgen)
/// and server (verifying tag uniqueness) drive the same
/// function — no JS / SQL mirror to drift.
#[must_use]
pub fn compute_joiner_tag(
    capability_bytes: &[u8],
    joiner_account_id: &[u8],
) -> [u8; JOINER_TAG_LEN] {
    use lakko_crypto::hash::{hmac_sha256, sha256};

    // Stage 1: derive a per-blob salt from the capability
    // bytes. Using `sha256` (not BLAKE3) here keeps the
    // protocol surface in lock-step with the rest of the
    // invite codec — `blob_id` is also SHA-256-of-bytes
    // (server-side, see services/discovery), so the salt and
    // the id share the same hash family.
    let mut salt_input =
        Vec::with_capacity(JOINER_TAG_SALT_DOMAIN_V1.len() + capability_bytes.len());
    salt_input.extend_from_slice(JOINER_TAG_SALT_DOMAIN_V1);
    salt_input.extend_from_slice(capability_bytes);
    let salt = sha256(&salt_input);

    // Stage 2: HMAC the joiner's account_id under the salt.
    // The `joiner_account_id` is opaque to this helper — UUIDv7
    // is the documented shape but the function works for any
    // byte slice.
    hmac_sha256(&salt, joiner_account_id)
}

/// A composed invite capability, ready to wire-encode into a URL.
///
/// Sender side: produced by [`compose`].
/// Recipient side: produced by [`decode_and_verify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteCapability {
    /// Inviter's long-term Ed25519 verifying key. The wire
    /// signature is verified against this; the joiner can
    /// independently look up the same pubkey via
    /// `/discovery/identity/{owner_account_id}` and confirm
    /// they match (TOFU check until KT-log inclusion proofs
    /// land).
    pub owner_signature_pubkey: VerifyingKey,
    /// Inviter's UUIDv7 raw bytes. Used by the joiner to
    /// fetch the inviter's KeyPackage from `services/discovery`
    /// when bootstrapping the MLS group.
    pub owner_account_id: [u8; ACCOUNT_ID_LEN],
    /// Inviter's long-term X25519 public key. The joiner seals
    /// their first ContactRequest envelope under this. Embedding
    /// it in the capability lets the joiner act on the link
    /// without first hitting `/discovery/identity` — useful when
    /// the discovery service is censored or temporarily down.
    pub owner_x25519_pubkey: X25519PublicKey,
    /// UNIX milliseconds when the invite stops being valid.
    /// `NEVER_EXPIRES` (= 0) means no expiry — UI MUST surface
    /// this as a stalking-risk warning (ADR-0005 §13.3).
    pub expiry_ms: u64,
    /// Maximum number of times the invite can be consumed.
    /// `SINGLE_USE` (= 0) is the typical case. Best-effort hint
    /// in V1 — server-side enforcement arrives in 1.18d.
    pub max_uses: u32,
    /// `true` for "this link is a referral for someone who
    /// doesn't have a Lakko account yet"; the signup flow uses
    /// it to auto-send a ContactRequest back to the owner once
    /// the new account is provisioned (ADR-0005 §13.4).
    pub bootstrap_referral: bool,
    /// Optional human-visible label, UTF-8, capped at
    /// [`MAX_LABEL_LEN`] bytes. Empty string is allowed — the
    /// UI fills in a default from the inviter's display name.
    pub label: String,
}

impl InviteCapability {
    /// `true` iff the capability has not expired at `now_ms`.
    /// `NEVER_EXPIRES` capabilities always return `true` —
    /// the stalking risk is the UI's responsibility to surface,
    /// not the codec's to enforce.
    #[must_use]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.expiry_ms == NEVER_EXPIRES || now_ms < self.expiry_ms
    }
}

/// Compose an invite capability and produce its signed wire-format
/// bytes. The caller-supplied `label` is truncated-or-rejected per
/// the `label_too_long` path (we reject rather than truncate to
/// keep the wire format predictable for the receiver).
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when `label.len() > MAX_LABEL_LEN`.
pub fn compose(
    owner_signing: &SigningKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    owner_x25519_pubkey: &X25519PublicKey,
    expiry_ms: u64,
    max_uses: u32,
    bootstrap_referral: bool,
    label: &str,
) -> Result<Vec<u8>> {
    if label.len() > MAX_LABEL_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "invite label is {} bytes, max is {}",
            label.len(),
            MAX_LABEL_LEN,
        )));
    }
    let owner_pubkey = owner_signing.verifying_key();
    let mut wire = encode_signing_prefix(
        &owner_pubkey,
        owner_account_id,
        owner_x25519_pubkey,
        expiry_ms,
        max_uses,
        bootstrap_referral,
        label,
    );
    let signature = owner_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Decode the wire bytes, verify the Ed25519 signature against
/// the embedded `owner_signature_pubkey`, return the validated
/// [`InviteCapability`].
///
/// Does **not** check `expiry_ms` — call
/// [`InviteCapability::is_valid_now`] separately if you want
/// freshness enforcement. Splitting the two lets a UI render
/// "this invite expired 3 days ago" rather than a generic
/// failure.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format malformed,
///   wrong schema version, oversized label, or trailing bytes.
/// - [`ContactError::BadSignature`] — Ed25519 verification failed.
/// - [`ContactError::Crypto`] — embedded pubkey bytes did not
///   parse as a valid Ed25519 / X25519 point.
pub fn decode_and_verify(wire: &[u8]) -> Result<InviteCapability> {
    let mut cur = wire;

    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported InviteCapability schema version {version}",
        )));
    }
    let owner_signature_pubkey = VerifyingKey::from_bytes(take_n(&mut cur, ED25519_PUB_LEN)?)
        .map_err(ContactError::Crypto)?;
    let mut owner_account_id = [0u8; ACCOUNT_ID_LEN];
    owner_account_id.copy_from_slice(take_n(&mut cur, ACCOUNT_ID_LEN)?);
    let owner_x25519_pubkey = X25519PublicKey::from_bytes(take_n(&mut cur, X25519_PUB_LEN)?)
        .map_err(ContactError::Crypto)?;
    let expiry_ms = u64::from_be_bytes(
        take_n(&mut cur, 8)?
            .try_into()
            .expect("take_n returned 8 bytes"),
    );
    let max_uses = u32::from_be_bytes(
        take_n(&mut cur, 4)?
            .try_into()
            .expect("take_n returned 4 bytes"),
    );
    let bootstrap_referral_byte = take_u8(&mut cur)?;
    let bootstrap_referral = match bootstrap_referral_byte {
        0 => false,
        1 => true,
        other => {
            return Err(ContactError::InvalidEncoding(format!(
                "bootstrap_referral byte must be 0 or 1, got {other}",
            )));
        },
    };
    let label_len = u16::from_be_bytes(
        take_n(&mut cur, 2)?
            .try_into()
            .expect("take_n returned 2 bytes"),
    ) as usize;
    if label_len > MAX_LABEL_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "label_len {label_len} exceeds MAX_LABEL_LEN {MAX_LABEL_LEN}",
        )));
    }
    let label_bytes = take_n(&mut cur, label_len)?;
    let label = std::str::from_utf8(label_bytes)
        .map_err(|e| ContactError::InvalidEncoding(format!("label is not valid UTF-8: {e}")))?
        .to_string();
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    let signature = Signature::from_bytes(signature_bytes).map_err(ContactError::Crypto)?;

    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "{} trailing bytes after signature",
            cur.len(),
        )));
    }

    // Re-encode the prefix exactly as the signer did and verify.
    let signed_prefix = encode_signing_prefix(
        &owner_signature_pubkey,
        &owner_account_id,
        &owner_x25519_pubkey,
        expiry_ms,
        max_uses,
        bootstrap_referral,
        &label,
    );
    owner_signature_pubkey
        .verify(&signed_prefix, &signature)
        .map_err(|_| ContactError::BadSignature)?;

    Ok(InviteCapability {
        owner_signature_pubkey,
        owner_account_id,
        owner_x25519_pubkey,
        expiry_ms,
        max_uses,
        bootstrap_referral,
        label,
    })
}

// ============================================================================
// Codec helpers
// ============================================================================

fn encode_signing_prefix(
    owner_signature_pubkey: &VerifyingKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    owner_x25519_pubkey: &X25519PublicKey,
    expiry_ms: u64,
    max_uses: u32,
    bootstrap_referral: bool,
    label: &str,
) -> Vec<u8> {
    let label_len_u16 =
        u16::try_from(label.len()).expect("label fits in u16; caller checked MAX_LABEL_LEN");
    let mut out = Vec::with_capacity(
        1 + ED25519_PUB_LEN + ACCOUNT_ID_LEN + X25519_PUB_LEN + 8 + 4 + 1 + 2 + label.len(),
    );
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&owner_signature_pubkey.to_bytes());
    out.extend_from_slice(owner_account_id);
    out.extend_from_slice(&owner_x25519_pubkey.to_bytes());
    out.extend_from_slice(&expiry_ms.to_be_bytes());
    out.extend_from_slice(&max_uses.to_be_bytes());
    out.push(u8::from(bootstrap_referral));
    out.extend_from_slice(&label_len_u16.to_be_bytes());
    out.extend_from_slice(label.as_bytes());
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

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::agreement::SecretKey as X25519SecretKey;

    fn fixture() -> (SigningKey, [u8; ACCOUNT_ID_LEN], X25519PublicKey) {
        let signing = SigningKey::generate();
        let account_id = [0xA1u8; ACCOUNT_ID_LEN];
        let x25519_secret = X25519SecretKey::generate();
        let x25519_pub = x25519_secret.public_key();
        (signing, account_id, x25519_pub)
    }

    #[test]
    fn compose_and_decode_roundtrip() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(
            &signing,
            &account_id,
            &x25519_pub,
            1_700_000_000_000,
            5,
            false,
            "alice@home",
        )
        .unwrap();

        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(
            cap.owner_signature_pubkey.to_bytes(),
            signing.verifying_key().to_bytes()
        );
        assert_eq!(cap.owner_account_id, account_id);
        assert_eq!(cap.owner_x25519_pubkey.to_bytes(), x25519_pub.to_bytes());
        assert_eq!(cap.expiry_ms, 1_700_000_000_000);
        assert_eq!(cap.max_uses, 5);
        assert!(!cap.bootstrap_referral);
        assert_eq!(cap.label, "alice@home");
    }

    #[test]
    fn empty_label_round_trips() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.label, "");
    }

    #[test]
    fn never_expires_means_always_valid() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(
            &signing,
            &account_id,
            &x25519_pub,
            NEVER_EXPIRES,
            0,
            false,
            "",
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.is_valid_now(0));
        assert!(cap.is_valid_now(u64::MAX));
    }

    #[test]
    fn expiry_in_the_past_invalidates() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(
            &signing,
            &account_id,
            &x25519_pub,
            1_000_000_000,
            0,
            false,
            "",
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.is_valid_now(999_999_999));
        assert!(!cap.is_valid_now(1_000_000_000)); // boundary: equal = expired
        assert!(!cap.is_valid_now(2_000_000_000));
    }

    #[test]
    fn bootstrap_referral_round_trips() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, true, "").unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.bootstrap_referral);
    }

    #[test]
    fn label_at_max_len_round_trips() {
        let (signing, account_id, x25519_pub) = fixture();
        let label = "x".repeat(MAX_LABEL_LEN);
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, &label).unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.label.len(), MAX_LABEL_LEN);
    }

    #[test]
    fn label_over_max_len_rejected_at_compose() {
        let (signing, account_id, x25519_pub) = fixture();
        let label = "x".repeat(MAX_LABEL_LEN + 1);
        let err = compose(&signing, &account_id, &x25519_pub, 0, 0, false, &label).unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn tampered_signature_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        // Flip a bit in the trailing 64-byte signature.
        let len = wire.len();
        wire[len - 1] ^= 0x01;
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn tampered_label_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "alice").unwrap();
        // Find the label byte 'a' (after the fixed-position prefix) and flip
        // its high bit. The signature was over the original label, so this
        // must surface as BadSignature, not as a UTF-8 error.
        let label_offset = 1
            + ED25519_PUB_LEN
            + ACCOUNT_ID_LEN
            + X25519_PUB_LEN
            + 8 // expiry_ms
            + 4 // max_uses
            + 1 // bootstrap_referral
            + 2; // label_len
        wire[label_offset] = b'b';
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn truncated_wire_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        // Strip the trailing 64-byte signature.
        let truncated = &wire[..wire.len() - 1];
        let err = decode_and_verify(truncated).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn empty_wire_rejected() {
        let err = decode_and_verify(&[]).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn unknown_schema_version_rejected() {
        let mut wire = vec![0xFFu8]; // schema version 255
        wire.extend_from_slice(&[0u8; 200]); // some bytes after
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn invalid_bootstrap_referral_byte_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        // bootstrap_referral byte sits at:
        //   1 + 32 + 16 + 32 + 8 + 4 = 93
        wire[93] = 0x42;
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn trailing_bytes_after_signature_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        wire.push(0xDE);
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn wire_starts_with_schema_version_byte() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        assert_eq!(wire[0], SCHEMA_VERSION);
    }

    #[test]
    fn signing_with_a_different_key_does_not_verify() {
        let (signing_real, account_id, x25519_pub) = fixture();
        let signing_attacker = SigningKey::generate();
        // Hand-build a wire with the real owner's pubkey but the
        // attacker's signature — should fail signature check.
        let mut wire = encode_signing_prefix(
            &signing_real.verifying_key(),
            &account_id,
            &x25519_pub,
            0,
            0,
            false,
            "",
        );
        let bad_sig = signing_attacker.sign(&wire);
        wire.extend_from_slice(&bad_sig.to_bytes());

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn label_with_invalid_utf8_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        // Hand-build the wire by emitting raw bytes — `encode_signing_prefix`
        // takes `&str` so we cannot route invalid UTF-8 through it.
        // The signature is over THESE bytes (so signature verification
        // passes), and only the UTF-8 conversion step inside
        // `decode_and_verify` fails — which is what we want to assert.
        let label_bytes: [u8; 3] = [0xFF, 0xFE, 0xFD];
        let label_len_u16 = u16::try_from(label_bytes.len()).unwrap();

        let mut prefix = Vec::new();
        prefix.push(SCHEMA_VERSION);
        prefix.extend_from_slice(&signing.verifying_key().to_bytes());
        prefix.extend_from_slice(&account_id);
        prefix.extend_from_slice(&x25519_pub.to_bytes());
        prefix.extend_from_slice(&0u64.to_be_bytes()); // expiry_ms
        prefix.extend_from_slice(&0u32.to_be_bytes()); // max_uses
        prefix.push(0); // bootstrap_referral
        prefix.extend_from_slice(&label_len_u16.to_be_bytes());
        prefix.extend_from_slice(&label_bytes);

        let sig = signing.sign(&prefix);
        let mut wire = prefix;
        wire.extend_from_slice(&sig.to_bytes());

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    // ----------------------------------------------------------------
    // Revocation canonical-bytes + signing helper (Phase 1.18d2b)
    // ----------------------------------------------------------------

    #[test]
    fn revoke_canonical_bytes_layout() {
        // Domain prefix is 23 bytes; blob_id 32; u64 BE 8 → 63
        // total. Pin so a future refactor that drifts the
        // prefix surfaces here.
        assert_eq!(REVOKE_DOMAIN_V1.len(), 23);
        assert_eq!(REVOKE_DOMAIN_V1, b"lakko-invite-revoke/v1\x00");
        assert_eq!(BLOB_ID_LEN, 32);

        let blob_id = [0xCDu8; BLOB_ID_LEN];
        let signed_at_ms: u64 = 0x0123_4567_89AB_CDEF;
        let canonical = revoke_canonical_bytes(&blob_id, signed_at_ms);
        assert_eq!(canonical.len(), REVOKE_DOMAIN_V1.len() + BLOB_ID_LEN + 8);

        // Order: prefix, blob_id, signed_at_ms (big-endian).
        assert_eq!(&canonical[..REVOKE_DOMAIN_V1.len()], REVOKE_DOMAIN_V1);
        let blob_off = REVOKE_DOMAIN_V1.len();
        assert_eq!(&canonical[blob_off..blob_off + BLOB_ID_LEN], &blob_id[..]);
        let ts_off = blob_off + BLOB_ID_LEN;
        assert_eq!(&canonical[ts_off..ts_off + 8], &signed_at_ms.to_be_bytes());
    }

    #[test]
    fn revoke_replay_window_is_10_minutes() {
        // The web client builds `signed_at_ms` against
        // `Date.now()`; tightening or loosening this constant
        // without bumping the client surfaces as silent
        // revocation failures with no distinguishable error
        // code (server folds bad-window into 400). Pin.
        assert_eq!(REVOKE_REPLAY_WINDOW_MS, 600_000);
    }

    // ----------------------------------------------------------------
    // Owner-list canonical-bytes + signing helper (Phase 1.18d2b-list)
    // ----------------------------------------------------------------

    #[test]
    fn owner_list_canonical_bytes_layout() {
        // Domain prefix is 27 bytes (26 chars + NUL); account_id
        // 16; u64 BE 8 → 51 total. Pin so a future refactor that
        // drifts the prefix surfaces here.
        assert_eq!(OWNER_LIST_DOMAIN_V1.len(), 27);
        assert_eq!(OWNER_LIST_DOMAIN_V1, b"lakko-invite-owner-list/v1\x00");
        assert_eq!(ACCOUNT_ID_LEN, 16);

        let acct = [0xA5u8; ACCOUNT_ID_LEN];
        let signed_at_ms: u64 = 0x0123_4567_89AB_CDEF;
        let canonical = owner_list_canonical_bytes(&acct, signed_at_ms);
        assert_eq!(
            canonical.len(),
            OWNER_LIST_DOMAIN_V1.len() + ACCOUNT_ID_LEN + 8
        );

        // Order: prefix, account_id, signed_at_ms (big-endian).
        assert_eq!(
            &canonical[..OWNER_LIST_DOMAIN_V1.len()],
            OWNER_LIST_DOMAIN_V1
        );
        let acct_off = OWNER_LIST_DOMAIN_V1.len();
        assert_eq!(&canonical[acct_off..acct_off + ACCOUNT_ID_LEN], &acct[..]);
        let ts_off = acct_off + ACCOUNT_ID_LEN;
        assert_eq!(&canonical[ts_off..ts_off + 8], &signed_at_ms.to_be_bytes());
    }

    #[test]
    fn owner_list_domain_distinct_from_revoke() {
        // Distinct domain prefixes are the core anti-replay
        // invariant: a captured revoke-sig must not authenticate
        // a list-query, and vice versa. The two domains must
        // produce non-overlapping byte streams under any
        // common-suffix tail.
        assert_ne!(OWNER_LIST_DOMAIN_V1, REVOKE_DOMAIN_V1);
        // Lengths differ too — even a pathological caller
        // building a list-domain signed message against the
        // revoke-domain verify (or vice versa) lands in different
        // byte ranges, surfacing as a clean signature failure
        // rather than a partial-match anomaly.
        assert_ne!(OWNER_LIST_DOMAIN_V1.len(), REVOKE_DOMAIN_V1.len());
    }

    #[test]
    fn sign_owner_list_round_trips() {
        // Sign with one key, verify against the matching pubkey.
        // The signing key + signed_at_ms pair fully determines
        // the signature; this gates that `sign_owner_list` is a
        // thin wrapper over Ed25519::sign that the
        // canonical-bytes layer feeds.
        let signing = SigningKey::from_seed(&[0xAAu8; 32]);
        let verifying = signing.verifying_key();
        let acct = [0x42u8; ACCOUNT_ID_LEN];
        let signed_at_ms: u64 = 1_700_000_000_000;

        let sig_bytes = sign_owner_list(&signing, &acct, signed_at_ms);
        let sig =
            lakko_crypto::signing::Signature::from_bytes(&sig_bytes).expect("signature decodes");
        let canonical = owner_list_canonical_bytes(&acct, signed_at_ms);
        verifying
            .verify(&canonical, &sig)
            .expect("signature verifies against canonical bytes");
    }

    #[test]
    fn owner_list_signature_does_not_authenticate_revoke() {
        // The whole point of distinct domain prefixes: a sig
        // produced by `sign_owner_list` MUST NOT verify against
        // `revoke_canonical_bytes` even when the timestamp + (a
        // structurally-fitting fake) blob_id align.
        let signing = SigningKey::from_seed(&[0xCCu8; 32]);
        let verifying = signing.verifying_key();
        let acct = [0x01u8; ACCOUNT_ID_LEN];
        let signed_at_ms: u64 = 1_700_000_000_000;
        let sig_bytes = sign_owner_list(&signing, &acct, signed_at_ms);
        let sig = lakko_crypto::signing::Signature::from_bytes(&sig_bytes).unwrap();

        // Try to verify the owner-list signature against a
        // revoke-canonical-bytes message. Even if the attacker
        // can pick any blob_id, the domain separator mismatch
        // ensures the signature won't authenticate.
        let fake_blob_id = [0xFFu8; BLOB_ID_LEN];
        let revoke_canonical = revoke_canonical_bytes(&fake_blob_id, signed_at_ms);
        assert!(
            verifying.verify(&revoke_canonical, &sig).is_err(),
            "owner-list signature must not authenticate revoke-domain message",
        );
    }

    #[test]
    fn owner_list_signed_at_binds_into_signature() {
        // Two signatures over the same (key, account_id) but
        // different `signed_at_ms` MUST differ — the timestamp
        // is part of what gets hashed into the signature, not a
        // detached side-channel field. Without this, a captured
        // signature could be replayed past the freshness window
        // by mutating `signed_at_ms` in the request body.
        let signing = SigningKey::from_seed(&[0xBBu8; 32]);
        let acct = [0x11u8; ACCOUNT_ID_LEN];
        let sig_a = sign_owner_list(&signing, &acct, 1_000_000);
        let sig_b = sign_owner_list(&signing, &acct, 2_000_000);
        assert_ne!(sig_a, sig_b);
    }

    // ----------------------------------------------------------------
    // Per-joiner consume tag (Phase 1.18d3a)
    // ----------------------------------------------------------------

    #[test]
    fn joiner_tag_is_deterministic() {
        // Same inputs → same output. This is the load-bearing
        // contract: a re-consume from the same joiner against
        // the same capability MUST collide on the server's PK
        // for idempotent dedup to work.
        let cap = vec![0x01u8; 200];
        let acct = [0xA1u8; 16];
        let a = compute_joiner_tag(&cap, &acct);
        let b = compute_joiner_tag(&cap, &acct);
        assert_eq!(a, b);
        assert_eq!(a.len(), JOINER_TAG_LEN);
    }

    #[test]
    fn joiner_tag_changes_with_capability() {
        // Same joiner + different capability → different tag.
        // Per ADR-0005 §13.7's anti-correlation property: the
        // server cannot link two consume events from the same
        // joiner against different invites.
        let cap_a = vec![0x01u8; 200];
        let mut cap_b = cap_a.clone();
        cap_b[0] ^= 0x01;
        let acct = [0xA1u8; 16];
        let a = compute_joiner_tag(&cap_a, &acct);
        let b = compute_joiner_tag(&cap_b, &acct);
        assert_ne!(a, b);
    }

    #[test]
    fn joiner_tag_changes_with_account_id() {
        // Same capability + different joiners → different tags.
        // This is the basic dedup contract: server's PK on
        // (blob_id, tag) only catches repeats from the *same*
        // joiner.
        let cap = vec![0x01u8; 200];
        let a = compute_joiner_tag(&cap, &[0xA1u8; 16]);
        let b = compute_joiner_tag(&cap, &[0xA2u8; 16]);
        assert_ne!(a, b);
    }

    #[test]
    fn joiner_tag_layout_pin() {
        // Domain prefix 26 bytes + NUL = 27. Salt is SHA-256
        // (32 bytes); tag is HMAC-SHA-256 (32 bytes). Pin so a
        // future refactor that drifts the prefix surfaces here.
        assert_eq!(JOINER_TAG_SALT_DOMAIN_V1.len(), 27);
        assert_eq!(JOINER_TAG_SALT_DOMAIN_V1, b"lakko-invite-bloom-salt/v1\x00");
        assert_eq!(JOINER_TAG_LEN, 32);
    }

    #[test]
    fn sign_revocation_round_trip() {
        let signing = SigningKey::from_seed(&[0xAAu8; 32]);
        let pubkey = signing.verifying_key();
        let blob_id = [0xBBu8; BLOB_ID_LEN];
        let signed_at_ms = 1_700_000_000_000u64;

        let signature_bytes = sign_revocation(&signing, &blob_id, signed_at_ms);
        let signature = Signature::from_bytes(&signature_bytes).unwrap();

        let canonical = revoke_canonical_bytes(&blob_id, signed_at_ms);
        assert!(pubkey.verify(&canonical, &signature).is_ok());

        // Tamper signed_at_ms — must reject.
        let canonical_tampered = revoke_canonical_bytes(&blob_id, signed_at_ms + 1);
        assert!(pubkey.verify(&canonical_tampered, &signature).is_err());

        // Tamper blob_id — must reject.
        let mut blob_id_tampered = blob_id;
        blob_id_tampered[0] ^= 0x01;
        let canonical_tampered = revoke_canonical_bytes(&blob_id_tampered, signed_at_ms);
        assert!(pubkey.verify(&canonical_tampered, &signature).is_err());

        // Different signing key — must reject.
        let attacker = SigningKey::from_seed(&[0xCCu8; 32]);
        let attacker_sig_bytes = sign_revocation(&attacker, &blob_id, signed_at_ms);
        let attacker_sig = Signature::from_bytes(&attacker_sig_bytes).unwrap();
        assert!(pubkey.verify(&canonical, &attacker_sig).is_err());
    }
}
