// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`GroupInviteCapability`] — link-based group join-ticket (ADR-0028, G-3).
//!
//! The inverse of the admin-adds-by-username flow: an owner/admin mints a
//! signed link that conveys a group's `group_id` to a **non-member**, who then
//! crafts an MLS external `Add` proposal
//! ([`lakko_mls::Identity::create_external_join_proposal`], ADR-0027) and posts
//! it to the group journal for the admin to approve.
//!
//! Sibling of [`crate::call_invite::CallInviteCapability`] (ADR-0009) and
//! [`crate::invite::InviteCapability`] (ADR-0005): the same single-file manual
//! codec + Ed25519-over-the-prefix signature, but a leaner payload — no
//! embedded KeyPackage, no media flags, no account id. The joiner mints its
//! **own** KeyPackage inside `create_external_join_proposal`, and learns the
//! group's current epoch from the journal's public MLS framing (so the ticket
//! carries no epoch — see ADR-0028 §3).
//!
//! ## What the link carries (and does NOT carry)
//!
//! - `group_id` — the target group handle the joiner passes to
//!   `create_external_join_proposal`. Variable-length opaque MLS `GroupId`
//!   bytes (Lakko's `Group::group_id()` is a `Vec<u8>`), length-prefixed here.
//! - `issuer_signature_pubkey` — the owner/admin Ed25519 key that minted the
//!   link. Anti-tamper (the signature binds the `group_id`) + lets the admin
//!   UI surface *which* member's link a pending request arrived on, and lets
//!   the joiner render an anti-phishing fingerprint.
//! - `expiry_ms` — UNIX millis; [`NEVER_EXPIRES`] (= 0) = no expiry.
//!
//! **No epoch** (read from the journal at request time) and **no
//! authorization secret** — the security boundary is the admin's approval gate
//! (ADR-0027 role-gated, peer-verified), not the ticket. A leaked link lets a
//! stranger *ask* to join; it never lets them *in*.
//!
//! ## The preview (v2, 2026-09-26)
//!
//! A person opening a link used to see the issuer's key fingerprint and
//! nothing else, which tells them nothing about what they are joining. The
//! capability now carries an optional **sealed preview** - the group name, the
//! inviter's display name and the member count at mint time - encrypted under
//! a key derived from a random 12-byte **link secret** that travels in the URL
//! **fragment** (`https://lakko.me/g/<slug>#<secret>`, 16 base64url characters;
//! see [`preview_key`] for the derivation). A fragment never leaves the browser or
//! the app, so neither the discovery ledger (which stores pointer-link
//! capabilities) nor an access log ever sees the names; the server side stays
//! ciphertext-only (invariants #1, #2, #5). Whoever holds the whole link can
//! read the preview - that is the point of a link one shares. The preview is
//! inside the signature, so it cannot be swapped under a genuine capability;
//! it is still the ISSUER's claim (the inviter name is self-asserted like a
//! profile name), which is why the fingerprint stays available as the
//! out-of-band check.
//!
//! ## Wire format (manual codec, v2)
//!
//! ```text
//! u8           schema version (= 2)
//! u16be        group_id_len                       — bounded by MAX_GROUP_ID_LEN
//! [gid_len]    group_id                            — opaque MLS GroupId bytes
//! [32 bytes]   issuer_signature_pubkey             — Ed25519 verifying key
//! u64be        expiry_ms                           — UNIX millis; 0 = no expiry
//! u16be        preview_len                         — 0 = no preview; else <= MAX_SEALED_PREVIEW_LEN
//! [preview]    sealed preview                      — nonce(12) ‖ ChaCha20-Poly1305 over the plaintext below,
//!                                                    under preview_key(link secret, group_id, issuer)
//! [64 bytes]   signature                           — Ed25519 over the above
//! ```
//!
//! Preview plaintext (inside the AEAD):
//!
//! ```text
//! u8   preview version (= 1)
//! u8   group_name_len  ‖ group_name    — UTF-8, the display-name rule, 0 = none
//! u8   inviter_len     ‖ inviter_name  — UTF-8, the display-name rule, 0 = none
//! u16be member_count
//! ```
//!
//! Multi-byte integers are big-endian. The signature covers everything from
//! the schema-version byte through the sealed preview inclusive. v1 (no
//! preview field) is refused: there is no deployment to stay compatible with.

use lakko_crypto::aead::{
    open_with_key, seal_with_key, KEY_LEN as AEAD_KEY_LEN, NONCE_LEN, TAG_LEN,
};
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::random;
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::{
    Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN as ED25519_PUB_LEN, SIGNATURE_LEN,
};

use crate::error::{ContactError, Result};
use crate::invite::{BLOB_ID_LEN, NEVER_EXPIRES};
use crate::profile::check_display_name;

/// Current schema version on the wire.
const SCHEMA_VERSION: u8 = 2;

/// Length of the link secret that opens the sealed preview. It rides in the
/// URL fragment and never reaches a server. 96 bits of CSPRNG output, the
/// maintainer's length choice for a link of about 45 characters (the
/// short-link amendment of ADR-0028, 2026-09-27): the preview is a name and a
/// count behind a link its holder shares, and a 2^96 offline search per link
/// (per-link, see [`preview_key`]) is out of reach.
pub const LINK_SECRET_LEN: usize = 12;

/// Length of the link secret in its URL-fragment form: base64url without
/// padding of [`LINK_SECRET_LEN`] bytes.
pub const LINK_SECRET_B64URL_LEN: usize = 16;

/// HKDF-SHA256 `info` domain for the preview key (see [`preview_key`]).
pub const PREVIEW_KEY_DOMAIN_V1: &[u8] = b"lakko/group-invite/preview-key/v1\x00";

/// Preview plaintext version byte.
const PREVIEW_VERSION: u8 = 1;

/// Longest name the preview carries, in bytes (the one-byte length prefix;
/// the display-name rule caps a name at 255 bytes anyway).
pub const MAX_PREVIEW_NAME_BYTES: usize = 255;

/// Longest preview plaintext: version, two length-prefixed names, the count.
const MAX_PREVIEW_PLAINTEXT_LEN: usize =
    1 + 1 + MAX_PREVIEW_NAME_BYTES + 1 + MAX_PREVIEW_NAME_BYTES + 2;

/// Longest sealed preview on the wire (nonce + ciphertext + tag). A link that
/// claims more is refused before any allocation.
pub const MAX_SEALED_PREVIEW_LEN: usize = NONCE_LEN + MAX_PREVIEW_PLAINTEXT_LEN + TAG_LEN;

/// Hard cap on the embedded `group_id` bytes. An MLS `GroupId` is opaque
/// variable-length; Lakko's are short (16–32 bytes in practice), but the codec
/// bounds at 256 to keep URL lengths sane and parse-time work bounded without
/// pinning an exact upstream upper bound.
pub const MAX_GROUP_ID_LEN: usize = 256;

/// Re-export so callers that only `use crate::group_invite::*` get a coherent
/// surface. `GROUP_INVITE_NEVER_EXPIRES` (= 0) means no expiry.
pub use crate::invite::NEVER_EXPIRES as GROUP_INVITE_NEVER_EXPIRES;

// ============================================================================
// GroupInviteCapability — wire shape
// ============================================================================

/// Capability granted by a group owner/admin to a prospective joiner. Decoded
/// and verified from the base64url-encoded slug at the end of the
/// `https://lakko.me/g/<slug>` URL (ADR-0028 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInviteCapability {
    /// The target group's opaque MLS `GroupId` bytes — what the joiner passes
    /// to `Identity::create_external_join_proposal`.
    pub group_id: Vec<u8>,
    /// The issuer's long-term Ed25519 verifying key. The signature over the
    /// canonical bytes is verified against this; the admin UI can additionally
    /// check it is a current group member before approving (a forged link
    /// names a non-member issuer).
    pub issuer_signature_pubkey: VerifyingKey,
    /// UNIX milliseconds when the link stops being valid. [`NEVER_EXPIRES`]
    /// (= 0) means no expiry — UI SHOULD surface a non-expiring link as a
    /// standing-exposure note.
    pub expiry_ms: u64,
    /// The sealed preview (empty = the link carries none). Opaque until
    /// [`GroupInviteCapability::open_preview`] is given the link key.
    pub sealed_preview: Vec<u8>,
}

/// What a link shows the person before they ask to join: the issuer's claim
/// about the group at mint time, sealed under the link key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInvitePreview {
    /// The group's name, or `None` when it had none.
    pub group_name: Option<String>,
    /// The inviter's display name, or `None` when they have none.
    pub inviter_name: Option<String>,
    /// How many people were in the group when the link was minted.
    pub member_count: u16,
}

impl GroupInvitePreview {
    /// Refuse a preview that the decoder would refuse, before sealing it.
    fn validate(&self) -> Result<()> {
        for (what, name) in [
            ("group name", &self.group_name),
            ("inviter name", &self.inviter_name),
        ] {
            if let Some(n) = name {
                if n.is_empty() || n.len() > MAX_PREVIEW_NAME_BYTES {
                    return Err(ContactError::InvalidArgument(format!(
                        "group-invite preview {what} must be 1..={MAX_PREVIEW_NAME_BYTES} bytes",
                    )));
                }
                check_display_name(n).map_err(|e| {
                    ContactError::InvalidArgument(format!("group-invite preview {what}: {e}"))
                })?;
            }
        }
        Ok(())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAX_PREVIEW_PLAINTEXT_LEN);
        out.push(PREVIEW_VERSION);
        for name in [&self.group_name, &self.inviter_name] {
            let bytes = name.as_deref().map_or(&[][..], str::as_bytes);
            // `validate` bounded it; a wrong caller still cannot overflow the byte.
            let len = u8::try_from(bytes.len()).unwrap_or(u8::MAX);
            out.push(len);
            out.extend_from_slice(&bytes[..usize::from(len)]);
        }
        out.extend_from_slice(&self.member_count.to_be_bytes());
        out
    }

    fn decode(plain: &[u8]) -> Result<Self> {
        let mut cur = plain;
        let version = take_u8(&mut cur)?;
        if version != PREVIEW_VERSION {
            return Err(ContactError::InvalidEncoding(format!(
                "unsupported group-invite preview version {version}",
            )));
        }
        let mut names: [Option<String>; 2] = [None, None];
        for slot in &mut names {
            let len = usize::from(take_u8(&mut cur)?);
            if len == 0 {
                continue;
            }
            let raw = take_n(&mut cur, len)?;
            let text = core::str::from_utf8(raw).map_err(|_| {
                ContactError::InvalidEncoding("group-invite preview name is not UTF-8".into())
            })?;
            check_display_name(text).map_err(|e| {
                ContactError::InvalidEncoding(format!("group-invite preview name refused: {e}"))
            })?;
            *slot = Some(text.to_string());
        }
        let count = take_n(&mut cur, 2)?;
        if !cur.is_empty() {
            return Err(ContactError::InvalidEncoding(format!(
                "trailing {} bytes after group-invite preview",
                cur.len(),
            )));
        }
        let [group_name, inviter_name] = names;
        Ok(Self {
            group_name,
            inviter_name,
            member_count: u16::from_be_bytes([count[0], count[1]]),
        })
    }
}

/// Mint a fresh link secret (the URL-fragment value). One per link: it is what
/// makes the preview safe to seal under a random nonce, and it is never reused.
#[must_use]
pub fn generate_link_key() -> [u8; LINK_SECRET_LEN] {
    let mut secret = [0u8; LINK_SECRET_LEN];
    random::fill(&mut secret);
    secret
}

/// Derive the 32-byte preview AEAD key from the URL-carried link secret:
///
/// ```text
/// HKDF-SHA256(ikm  = link_secret,
///             salt = empty,
///             info = PREVIEW_KEY_DOMAIN_V1 ‖ u16be gid_len ‖ group_id ‖ issuer_pubkey(32))
/// ```
///
/// The secret is uniform random, so HKDF only has to expand it to the AEAD key
/// width; the key's strength is the secret's 96 bits. Binding the capability's
/// own `group_id` and issuer key into `info` is free (both are in the signed
/// capability the opener already holds) and makes every link's key derivation
/// distinct, so a search over secrets cannot be shared across many links: a
/// guessed secret has to be run through HKDF once per target link rather than
/// tested against all of them with one derivation. It also means a secret moved
/// onto a different capability opens nothing.
fn preview_key(
    link_secret: &[u8; LINK_SECRET_LEN],
    group_id: &[u8],
    issuer_pubkey: &VerifyingKey,
) -> Result<SecretBytes<AEAD_KEY_LEN>> {
    // Callers bound `group_id` to MAX_GROUP_ID_LEN; a wrong caller saturates.
    let gid_len = u16::try_from(group_id.len()).unwrap_or(u16::MAX);
    let mut info = Vec::with_capacity(PREVIEW_KEY_DOMAIN_V1.len() + 2 + group_id.len() + 32);
    info.extend_from_slice(PREVIEW_KEY_DOMAIN_V1);
    info.extend_from_slice(&gid_len.to_be_bytes());
    info.extend_from_slice(group_id);
    info.extend_from_slice(&issuer_pubkey.to_bytes());
    hkdf_sha256::<AEAD_KEY_LEN>(link_secret, &[], &info).map_err(|e| {
        ContactError::InvalidArgument(format!("group-invite preview key derivation: {e}"))
    })
}

impl GroupInviteCapability {
    /// Open the sealed preview with the link secret from the URL fragment.
    ///
    /// `Ok(None)` when the link carries no preview. A caller that has no key,
    /// or gets an `Err` (wrong key, tampered or malformed preview), shows the
    /// page without a preview - the fingerprint - and never a guess.
    ///
    /// # Errors
    ///
    /// - [`ContactError::InvalidEncoding`] when the preview does not open under
    ///   `link_secret` or its plaintext is malformed.
    pub fn open_preview(
        &self,
        link_secret: &[u8; LINK_SECRET_LEN],
    ) -> Result<Option<GroupInvitePreview>> {
        if self.sealed_preview.is_empty() {
            return Ok(None);
        }
        let key = preview_key(link_secret, &self.group_id, &self.issuer_signature_pubkey)?;
        let plain = open_with_key(key.as_bytes(), &self.sealed_preview).map_err(|_| {
            ContactError::InvalidEncoding(
                "group-invite preview does not open under this key".into(),
            )
        })?;
        GroupInvitePreview::decode(&plain).map(Some)
    }

    /// `true` iff the link has not expired at `now_ms`. A [`NEVER_EXPIRES`]
    /// link always returns `true`.
    #[must_use]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.expiry_ms == NEVER_EXPIRES || now_ms < self.expiry_ms
    }

    /// Signal-style safety number derived from the issuer's Ed25519 key — the
    /// same anti-phishing fingerprint format as
    /// [`crate::call_invite::CallInviteCapability::owner_fingerprint`]. The
    /// joiner UI shows it so the human can compare it against what the inviter
    /// shared out-of-band before clicking "request to join".
    #[must_use]
    pub fn issuer_fingerprint(&self) -> String {
        crate::safety::safety_number(&self.issuer_signature_pubkey, &self.issuer_signature_pubkey)
    }
}

/// Compose + sign a group join-ticket capability.
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when `group_id` is empty or longer than
///   [`MAX_GROUP_ID_LEN`].
pub fn compose(issuer_signing: &SigningKey, group_id: &[u8], expiry_ms: u64) -> Result<Vec<u8>> {
    if group_id.is_empty() {
        return Err(ContactError::InvalidArgument(
            "group-invite group_id must be non-empty".into(),
        ));
    }
    if group_id.len() > MAX_GROUP_ID_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "group-invite group_id is {} bytes, max is {MAX_GROUP_ID_LEN}",
            group_id.len(),
        )));
    }
    compose_sealed(issuer_signing, group_id, expiry_ms, &[])
}

/// Compose + sign a group join-ticket capability that carries a preview,
/// sealed under the key [`preview_key`] derives from `link_secret` (see
/// [`generate_link_key`]). The caller puts the secret in the link's URL
/// fragment and never anywhere a server reads.
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when `group_id` is out of bounds or a
///   preview name fails the display-name rule.
pub fn compose_with_preview(
    issuer_signing: &SigningKey,
    group_id: &[u8],
    expiry_ms: u64,
    preview: &GroupInvitePreview,
    link_secret: &[u8; LINK_SECRET_LEN],
) -> Result<Vec<u8>> {
    preview.validate()?;
    let key = preview_key(link_secret, group_id, &issuer_signing.verifying_key())?;
    let sealed = seal_with_key(key.as_bytes(), &preview.encode()).map_err(|e| {
        ContactError::InvalidArgument(format!("group-invite preview could not be sealed: {e}"))
    })?;
    compose_sealed(issuer_signing, group_id, expiry_ms, &sealed)
}

fn compose_sealed(
    issuer_signing: &SigningKey,
    group_id: &[u8],
    expiry_ms: u64,
    sealed_preview: &[u8],
) -> Result<Vec<u8>> {
    if group_id.is_empty() {
        return Err(ContactError::InvalidArgument(
            "group-invite group_id must be non-empty".into(),
        ));
    }
    if group_id.len() > MAX_GROUP_ID_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "group-invite group_id is {} bytes, max is {MAX_GROUP_ID_LEN}",
            group_id.len(),
        )));
    }
    if sealed_preview.len() > MAX_SEALED_PREVIEW_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "group-invite sealed preview is {} bytes, max is {MAX_SEALED_PREVIEW_LEN}",
            sealed_preview.len(),
        )));
    }
    let issuer_pubkey = issuer_signing.verifying_key();
    let mut wire = encode_signing_prefix(&issuer_pubkey, group_id, expiry_ms, sealed_preview);
    let signature = issuer_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Decode + verify a group join-ticket capability.
///
/// Does **not** check `expiry_ms` — call [`GroupInviteCapability::is_valid_now`]
/// separately so the UI can render "this link expired" distinctly from a
/// malformed/forged link.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format malformed, unsupported
///   schema version, EOF, `group_id` length beyond [`MAX_GROUP_ID_LEN`] or
///   zero, trailing bytes, or signature verification failed.
pub fn decode_and_verify(wire: &[u8]) -> Result<GroupInviteCapability> {
    let mut cur = wire;
    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported GroupInviteCapability schema version {version}",
        )));
    }
    let gid_len_bytes = take_n(&mut cur, 2)?;
    let gid_len = usize::from(u16::from_be_bytes([gid_len_bytes[0], gid_len_bytes[1]]));
    if gid_len == 0 {
        return Err(ContactError::InvalidEncoding(
            "group-invite group_id_len is zero".into(),
        ));
    }
    if gid_len > MAX_GROUP_ID_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "group-invite group_id_len {gid_len} > MAX_GROUP_ID_LEN {MAX_GROUP_ID_LEN}"
        )));
    }
    let group_id = take_n(&mut cur, gid_len)?.to_vec();
    let issuer_pubkey_bytes = take_n(&mut cur, ED25519_PUB_LEN)?;
    let expiry_ms_bytes = take_n(&mut cur, 8)?;
    let preview_len_bytes = take_n(&mut cur, 2)?;
    let preview_len = usize::from(u16::from_be_bytes([
        preview_len_bytes[0],
        preview_len_bytes[1],
    ]));
    if preview_len > MAX_SEALED_PREVIEW_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "group-invite preview_len {preview_len} > MAX_SEALED_PREVIEW_LEN {MAX_SEALED_PREVIEW_LEN}"
        )));
    }
    if preview_len != 0 && preview_len < NONCE_LEN + TAG_LEN {
        return Err(ContactError::InvalidEncoding(
            "group-invite preview shorter than a nonce and a tag".into(),
        ));
    }
    let sealed_preview = take_n(&mut cur, preview_len)?.to_vec();
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "trailing {} bytes after group-invite signature",
            cur.len(),
        )));
    }

    // Verify the Ed25519 signature over everything before the signature.
    let signing_input = &wire[..wire.len() - SIGNATURE_LEN];
    let issuer_pubkey = VerifyingKey::from_bytes(issuer_pubkey_bytes).map_err(|_| {
        ContactError::InvalidEncoding("group-invite issuer_signature_pubkey malformed".into())
    })?;
    let sig = Signature::from_bytes(signature_bytes)
        .map_err(|_| ContactError::InvalidEncoding("group-invite signature length wrong".into()))?;
    issuer_pubkey
        .verify(signing_input, &sig)
        .map_err(|_| ContactError::InvalidEncoding("group-invite signature invalid".into()))?;

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

    Ok(GroupInviteCapability {
        group_id,
        issuer_signature_pubkey: issuer_pubkey,
        expiry_ms,
        sealed_preview,
    })
}

// ============================================================================
// Internal codec helpers
// ============================================================================

fn encode_signing_prefix(
    issuer_signature_pubkey: &VerifyingKey,
    group_id: &[u8],
    expiry_ms: u64,
    sealed_preview: &[u8],
) -> Vec<u8> {
    // Both lengths were bounded by the caller; a wrong caller still cannot
    // overflow the prefix, it saturates.
    let gid_len_u16 = u16::try_from(group_id.len()).unwrap_or(u16::MAX);
    let preview_len_u16 = u16::try_from(sealed_preview.len()).unwrap_or(u16::MAX);
    let mut out =
        Vec::with_capacity(1 + 2 + group_id.len() + ED25519_PUB_LEN + 8 + 2 + sealed_preview.len());
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&gid_len_u16.to_be_bytes());
    out.extend_from_slice(group_id);
    out.extend_from_slice(&issuer_signature_pubkey.to_bytes());
    out.extend_from_slice(&expiry_ms.to_be_bytes());
    out.extend_from_slice(&preview_len_u16.to_be_bytes());
    out.extend_from_slice(sealed_preview);
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
// Revocation (ADR-0028 OQ1 revoke) — owner-signed delete of a `/g/` blob
// ============================================================================

/// Domain separator for a group-invite revocation signature. Distinct from the
/// contact-invite ([`crate::invite::revoke_canonical_bytes`]) and call-invite
/// ([`crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1`]) revoke domains, so a
/// captured revoke signature cannot be replayed across invite kinds.
pub const GROUP_INVITE_REVOKE_DOMAIN_V1: &[u8] = b"lakko-group-invite-revoke/v1\x00";

/// Build the canonical bytes the issuer signs to revoke a group-invite blob.
///
/// Layout: `GROUP_INVITE_REVOKE_DOMAIN_V1 (29 bytes) ‖ blob_id (6 bytes) ‖
/// signed_at_ms (8 bytes BE)`. The discovery service recovers the issuer's
/// verifying key from the stored capability's `issuer_signature_pubkey` and
/// verifies this; `signed_at_ms` gives replay protection bounded by
/// [`GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS`].
#[must_use]
pub fn group_revoke_canonical_bytes(blob_id: &[u8; BLOB_ID_LEN], signed_at_ms: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(GROUP_INVITE_REVOKE_DOMAIN_V1.len() + BLOB_ID_LEN + 8);
    out.extend_from_slice(GROUP_INVITE_REVOKE_DOMAIN_V1);
    out.extend_from_slice(blob_id);
    out.extend_from_slice(&signed_at_ms.to_be_bytes());
    out
}

/// Sign a group-invite revocation with the issuer's Ed25519 key — the same key
/// that signed the [`GroupInviteCapability`]. The server verifies the signature
/// against the embedded `issuer_signature_pubkey` it stored at upload.
#[must_use]
pub fn group_sign_revocation(
    issuer_signing: &SigningKey,
    blob_id: &[u8; BLOB_ID_LEN],
    signed_at_ms: u64,
) -> [u8; SIGNATURE_LEN] {
    let canonical = group_revoke_canonical_bytes(blob_id, signed_at_ms);
    issuer_signing.sign(&canonical).to_bytes()
}

/// Re-export the shared 10-minute symmetric replay window for group-invite
/// revoke callers (same freshness ceiling as the other revoke surfaces, a
/// distinct domain separator).
pub use crate::invite::REVOKE_REPLAY_WINDOW_MS as GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_signing() -> SigningKey {
        SigningKey::from_seed(&[0x5A; 32])
    }

    fn fixture_group_id() -> Vec<u8> {
        let mut gid = vec![0x11; 32];
        gid[0] = 0xCA;
        gid[31] = 0xFE;
        gid
    }

    #[test]
    fn compose_decode_round_trips() {
        let signing = fixture_signing();
        let gid = fixture_group_id();
        let wire = compose(&signing, &gid, 1_700_000_000_000).unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert_eq!(parsed.group_id, gid);
        assert_eq!(
            parsed.issuer_signature_pubkey.to_bytes(),
            signing.verifying_key().to_bytes()
        );
        assert_eq!(parsed.expiry_ms, 1_700_000_000_000);
    }

    #[test]
    fn variable_length_group_id_round_trips() {
        for len in [1usize, 16, 32, MAX_GROUP_ID_LEN] {
            let gid = vec![0x7Bu8; len];
            let wire = compose(&fixture_signing(), &gid, 0).unwrap();
            let parsed = decode_and_verify(&wire).unwrap();
            assert_eq!(parsed.group_id, gid, "len {len}");
        }
    }

    #[test]
    fn never_expires_is_valid_at_any_time() {
        let wire = compose(
            &fixture_signing(),
            &fixture_group_id(),
            GROUP_INVITE_NEVER_EXPIRES,
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(0));
        assert!(parsed.is_valid_now(u64::MAX));
    }

    #[test]
    fn expired_link_is_invalid_after_expiry() {
        let wire = compose(&fixture_signing(), &fixture_group_id(), 1_000).unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(999));
        assert!(!parsed.is_valid_now(1_000));
        assert!(!parsed.is_valid_now(2_000));
    }

    #[test]
    fn tampered_group_id_rejected() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0).unwrap();
        // Flip a byte inside the group_id region (offset 3 = first gid byte).
        wire[3] ^= 0xFF;
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("signature"), "{msg}"),
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn cross_signing_key_rejected() {
        let signing_a = SigningKey::from_seed(&[0xAA; 32]);
        let signing_b = SigningKey::from_seed(&[0xBB; 32]);
        let mut wire = compose(&signing_a, &fixture_group_id(), 0).unwrap();
        // Splice B's pubkey over A's (it sits right after version + len + gid).
        let off = 1 + 2 + fixture_group_id().len();
        wire[off..off + ED25519_PUB_LEN].copy_from_slice(&signing_b.verifying_key().to_bytes());
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("signature"), "{msg}"),
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn truncated_wire_rejected() {
        let wire = compose(&fixture_signing(), &fixture_group_id(), 0).unwrap();
        assert!(decode_and_verify(&wire[..wire.len() - SIGNATURE_LEN - 1]).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0).unwrap();
        wire.push(0x00);
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("trailing"), "{msg}"),
            other => panic!("expected trailing-bytes, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_schema_version_rejected() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0).unwrap();
        // v1 (the pre-preview layout) is refused too: there is no deployment
        // to stay compatible with, and a v1 wire has no preview field.
        wire[0] = 1;
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("schema version 1"), "{msg}");
            },
            other => panic!("expected unsupported-version, got {other:?}"),
        }
    }

    #[test]
    fn empty_group_id_rejected_at_compose() {
        let err = compose(&fixture_signing(), &[], 0).unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn oversize_group_id_rejected_at_compose() {
        let too_big = vec![0u8; MAX_GROUP_ID_LEN + 1];
        let err = compose(&fixture_signing(), &too_big, 0).unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn zero_group_id_len_rejected_at_decode() {
        // Hand-craft a wire with group_id_len = 0.
        let mut wire = vec![SCHEMA_VERSION, 0x00, 0x00];
        wire.extend_from_slice(&[0u8; ED25519_PUB_LEN]);
        wire.extend_from_slice(&0u64.to_be_bytes());
        wire.extend_from_slice(&[0u8; SIGNATURE_LEN]);
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("group_id_len"), "{msg}");
            },
            other => panic!("expected zero-len reject, got {other:?}"),
        }
    }

    #[test]
    fn issuer_fingerprint_is_deterministic_per_pubkey() {
        let cap_a =
            decode_and_verify(&compose(&fixture_signing(), &fixture_group_id(), 1_000).unwrap())
                .unwrap();
        let mut other_gid = fixture_group_id();
        other_gid[0] = 0x99;
        let cap_b =
            decode_and_verify(&compose(&fixture_signing(), &other_gid, 999_999).unwrap()).unwrap();
        assert_eq!(
            cap_a.issuer_fingerprint(),
            cap_b.issuer_fingerprint(),
            "fingerprint must depend ONLY on the issuer pubkey"
        );
    }

    #[test]
    fn different_issuers_have_different_fingerprints() {
        let cap_a = decode_and_verify(
            &compose(&SigningKey::from_seed(&[0xAA; 32]), &fixture_group_id(), 0).unwrap(),
        )
        .unwrap();
        let cap_b = decode_and_verify(
            &compose(&SigningKey::from_seed(&[0xBB; 32]), &fixture_group_id(), 0).unwrap(),
        )
        .unwrap();
        assert_ne!(cap_a.issuer_fingerprint(), cap_b.issuer_fingerprint());
    }

    #[test]
    fn revoke_canonical_bytes_layout_is_stable() {
        let blob_id = [0x33u8; BLOB_ID_LEN];
        let bytes = group_revoke_canonical_bytes(&blob_id, 0x0102_0304_0506_0708);
        assert_eq!(
            bytes.len(),
            GROUP_INVITE_REVOKE_DOMAIN_V1.len() + BLOB_ID_LEN + 8
        );
        assert!(bytes.starts_with(GROUP_INVITE_REVOKE_DOMAIN_V1));
        assert_eq!(
            &bytes[bytes.len() - 8..],
            &0x0102_0304_0506_0708u64.to_be_bytes()
        );
    }

    #[test]
    fn revoke_signature_verifies_under_issuer_key() {
        let issuer = fixture_signing();
        let blob_id = [0x44u8; BLOB_ID_LEN];
        let sig_bytes = group_sign_revocation(&issuer, &blob_id, 1_700_000_000_000);
        let canonical = group_revoke_canonical_bytes(&blob_id, 1_700_000_000_000);
        let sig = Signature::from_bytes(&sig_bytes).unwrap();
        assert!(issuer.verifying_key().verify(&canonical, &sig).is_ok());
        // A different signed_at_ms produces different canonical bytes → reject.
        let stale = group_revoke_canonical_bytes(&blob_id, 1_699_999_999_999);
        assert!(issuer.verifying_key().verify(&stale, &sig).is_err());
    }

    #[test]
    fn revoke_domain_isolates_from_call_invite() {
        // A group-invite revoke signature MUST NOT verify against the
        // call-invite revoke canonical bytes (distinct domain prefix).
        let issuer = fixture_signing();
        let blob_id = [0x55u8; BLOB_ID_LEN];
        let sig_bytes = group_sign_revocation(&issuer, &blob_id, 42);
        let sig = Signature::from_bytes(&sig_bytes).unwrap();
        let call_canonical = crate::call_invite::call_revoke_canonical_bytes(&blob_id, 42);
        assert!(issuer
            .verifying_key()
            .verify(&call_canonical, &sig)
            .is_err());
    }

    fn fixture_preview() -> GroupInvitePreview {
        GroupInvitePreview {
            group_name: Some("Weekend plans".to_string()),
            inviter_name: Some("Anna Weber".to_string()),
            member_count: 4,
        }
    }

    #[test]
    fn a_link_without_a_preview_opens_none() {
        let wire = compose(&fixture_signing(), &fixture_group_id(), NEVER_EXPIRES).unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.sealed_preview.is_empty());
        assert_eq!(cap.open_preview(&generate_link_key()).unwrap(), None);
    }

    #[test]
    fn preview_round_trips_under_the_link_key_and_not_under_another() {
        let key = generate_link_key();
        let wire = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            &fixture_preview(),
            &key,
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.group_id, fixture_group_id());
        assert_eq!(cap.open_preview(&key).unwrap(), Some(fixture_preview()));
        let other = generate_link_key();
        assert!(
            cap.open_preview(&other).is_err(),
            "a wrong key must not open the preview"
        );
    }

    #[test]
    fn link_secret_is_twelve_bytes_and_sixteen_url_characters() {
        assert_eq!(LINK_SECRET_LEN, 12);
        assert_eq!(generate_link_key().len(), LINK_SECRET_LEN);
        assert_eq!(LINK_SECRET_B64URL_LEN, (LINK_SECRET_LEN * 4).div_ceil(3));
        assert_eq!(LINK_SECRET_B64URL_LEN, 16);
        assert_ne!(generate_link_key(), generate_link_key());
    }

    #[test]
    fn preview_key_is_hkdf_over_the_secret_bound_to_group_and_issuer() {
        let secret = [0x07u8; LINK_SECRET_LEN];
        let gid = fixture_group_id();
        let issuer = fixture_signing().verifying_key();
        let key = preview_key(&secret, &gid, &issuer).unwrap();
        let mut info = PREVIEW_KEY_DOMAIN_V1.to_vec();
        info.extend_from_slice(&u16::try_from(gid.len()).unwrap().to_be_bytes());
        info.extend_from_slice(&gid);
        info.extend_from_slice(&issuer.to_bytes());
        let expected = hkdf_sha256::<AEAD_KEY_LEN>(&secret, &[], &info).unwrap();
        assert_eq!(key.as_bytes(), expected.as_bytes());
        // A different group or issuer gives a different key for the same secret.
        let mut other_gid = gid.clone();
        other_gid[1] ^= 0x01;
        assert_ne!(
            preview_key(&secret, &other_gid, &issuer)
                .unwrap()
                .as_bytes(),
            key.as_bytes()
        );
        let other_issuer = SigningKey::from_seed(&[0x6B; 32]).verifying_key();
        assert_ne!(
            preview_key(&secret, &gid, &other_issuer)
                .unwrap()
                .as_bytes(),
            key.as_bytes()
        );
    }

    #[test]
    fn a_link_secret_moved_onto_another_capability_opens_nothing() {
        // The key is bound to the capability's group id and issuer, so a
        // sealed preview transplanted onto a capability naming a different
        // group or issuer does not open under the same secret.
        let secret = generate_link_key();
        let wire_a = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            &fixture_preview(),
            &secret,
        )
        .unwrap();
        let cap_a = decode_and_verify(&wire_a).unwrap();
        let mut transplanted = cap_a.clone();
        transplanted.group_id[0] ^= 0x01;
        assert!(transplanted.open_preview(&secret).is_err());
        let mut other_issuer = cap_a.clone();
        other_issuer.issuer_signature_pubkey = SigningKey::from_seed(&[0x6B; 32]).verifying_key();
        assert!(other_issuer.open_preview(&secret).is_err());
        assert_eq!(
            cap_a.open_preview(&secret).unwrap(),
            Some(fixture_preview())
        );
    }

    #[test]
    fn preview_names_are_optional_and_the_count_survives() {
        let key = generate_link_key();
        let preview = GroupInvitePreview {
            group_name: None,
            inviter_name: None,
            member_count: u16::MAX,
        };
        let wire = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            &preview,
            &key,
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.open_preview(&key).unwrap(), Some(preview));
    }

    #[test]
    fn preview_names_follow_the_display_name_rule() {
        let key = generate_link_key();
        for bad in ["", "line\nbreak", "bidi\u{202E}", &"x".repeat(256)] {
            let preview = GroupInvitePreview {
                group_name: Some(bad.to_string()),
                inviter_name: None,
                member_count: 1,
            };
            assert!(
                compose_with_preview(
                    &fixture_signing(),
                    &fixture_group_id(),
                    NEVER_EXPIRES,
                    &preview,
                    &key
                )
                .is_err(),
                "{bad:?} must be refused",
            );
        }
        let preview = GroupInvitePreview {
            group_name: Some(
                "\u{1F389} \u{412}\u{435}\u{447}\u{435}\u{440}\u{438}\u{43D}\u{43A}\u{430}"
                    .to_string(),
            ),
            inviter_name: Some("\u{41C}\u{430}\u{440}\u{438}\u{44F}".to_string()),
            member_count: 3,
        };
        let wire = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            &preview,
            &key,
        )
        .unwrap();
        assert_eq!(
            decode_and_verify(&wire)
                .unwrap()
                .open_preview(&key)
                .unwrap(),
            Some(preview)
        );
    }

    #[test]
    fn a_tampered_preview_breaks_the_signature() {
        let key = generate_link_key();
        let mut wire = compose_with_preview(
            &fixture_signing(),
            &fixture_group_id(),
            NEVER_EXPIRES,
            &fixture_preview(),
            &key,
        )
        .unwrap();
        let idx = wire.len() - SIGNATURE_LEN - 5;
        wire[idx] ^= 0x01;
        assert!(decode_and_verify(&wire).is_err());
    }

    #[test]
    fn an_oversize_or_short_preview_length_is_refused_at_decode() {
        let signing = fixture_signing();
        let gid = fixture_group_id();
        let mut prefix = Vec::new();
        prefix.push(SCHEMA_VERSION);
        prefix.extend_from_slice(&u16::try_from(gid.len()).unwrap().to_be_bytes());
        prefix.extend_from_slice(&gid);
        prefix.extend_from_slice(&signing.verifying_key().to_bytes());
        prefix.extend_from_slice(&0u64.to_be_bytes());
        let mut over = prefix.clone();
        over.extend_from_slice(
            &u16::try_from(MAX_SEALED_PREVIEW_LEN + 1)
                .unwrap()
                .to_be_bytes(),
        );
        over.extend_from_slice(&vec![0u8; MAX_SEALED_PREVIEW_LEN + 1]);
        let sig = signing.sign(&over);
        over.extend_from_slice(&sig.to_bytes());
        assert!(decode_and_verify(&over).is_err());
        let mut short = prefix;
        short.extend_from_slice(&1u16.to_be_bytes());
        short.push(0);
        let sig = signing.sign(&short);
        short.extend_from_slice(&sig.to_bytes());
        assert!(decode_and_verify(&short).is_err());
    }
}
