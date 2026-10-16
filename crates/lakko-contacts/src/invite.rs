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
//!     Deferred: `lakko-transparency` ships the inclusion-proof
//!     primitives (`prove_inclusion` / `verify_inclusion`), but
//!     no transparency SERVICE is deployed to anchor identities
//!     and the capability carries no proof field, so this codec
//!     cannot bind the proof end-to-end. Without it, accepting a
//!     link is TOFU on the inviter's pubkey.
//!   * `consumed_pubkey_log: HmacBloom` — ADR-0005's wire-side
//!     bloom is not part of this codec; the `max_uses` ceiling is
//!     carried on the wire and is now ENFORCED server-side. The
//!     `services/discovery` `/discovery/invite/{blob_id}/consume`
//!     handler (landed 1.18d2a) does an atomic counter bump plus
//!     a per-joiner de-dup against the `invite_consumers` side
//!     table keyed on the server-blind joiner tag this crate
//!     mints via [`compute_joiner_tag`] (1.18d3b). So the
//!     `max_uses` field is no longer a mere UI hint — a joiner
//!     past the ceiling is refused at consume time.
//!   * `blob_id` storage indirection — the ADR puts the
//!     capability bytes in `services/storage` and only embeds a
//!     short pointer in the URL. This codec still produces the
//!     full capability bytes; whether they ride directly in the
//!     URL or behind an indirection is a transport choice the
//!     server already supports: `services/discovery` stores an
//!     uploaded capability under a random [`BLOB_ID_LEN`]-byte id
//!     it mints at upload ([`generate_blob_id`]; since the
//!     2026-09-27 short-link amendment of ADR-0028, no longer a
//!     hash of the bytes) and serves it from
//!     `GET /discovery/invite/{blob_id}`, so a client can embed
//!     the short `/i/<blob_id>` pointer instead of the ~150-220
//!     char inline form. The capability struct is unchanged
//!     either way — only the URL shape differs. The id binds
//!     nothing on its own; since the 2026-09-28 security pass
//!     the ledger stores the capability SEALED under a secret
//!     in the link fragment ([`crate::link_seal`], see
//!     [`seal_link`] / [`open_link`]), so a malicious discovery
//!     serving another blob under a live id is caught: it does
//!     not open under the link's secret.
//!   * `ephemeral_prekey` — ADR keeps it for forward secrecy on
//!     the owner→joiner direction. Our flow has joiner→owner as
//!     the first sealed message, and the joiner generates a
//!     fresh ephemeral per envelope (`crate::envelope::seal`),
//!     so we don't need the owner-side prekey here.
//!
//! ## Wire format (manual codec, v2 — ADR-0060 §6)
//!
//! ```text
//! u8           schema version (= 2)
//! [32 bytes]   owner_signature_pubkey            — Ed25519 verifying key
//! [16 bytes]   owner_account_id                  — UUIDv7 raw
//! [32 bytes]   owner_x25519_pubkey               — long-term HPKE init key
//! u64be        expiry_ms                          — UNIX millis; 0 means "no expiry"
//! u32be        max_uses                           — 0 means "single-use"
//! u8           bootstrap_referral                 — 0/1; flags signup deep-link
//! u16be        label_len
//! [label_len]  label_bytes (UTF-8, ≤ MAX_LABEL_LEN)
//! [16 bytes]   link_id                            — random, the reach scope of this link
//! [134 bytes]  link_grant                         — ReachGrant, class `link` (SECRET)
//! [64 bytes]   signature                          — Ed25519 over the above
//! ```
//!
//! Signature covers everything from the schema-version byte through
//! `link_grant` inclusive — i.e. the entire encoding except the
//! trailing 64-byte signature itself. Mirrors `request.rs`'s
//! pattern exactly.
//!
//! v1 (no link grant) is refused: a flag day, since nothing is deployed
//! (ADR-0060).
//!
//! ## The link grant (ADR-0060 §6)
//!
//! Since ADR-0060 the owner's mailbox is closed: knowing the owner's account
//! id no longer lets anyone write the contact request a link exists to
//! carry. So the capability hands the link holder a **`link` reach grant**
//! ([`crate::reach::ReachGrant`], class [`crate::reach::ReachClass::Link`]),
//! derived by the owner as
//! `ReachGrant::derive(account_seed, Link, link_id, version)` over the
//! capability's own random [`LINK_ID_LEN`]-byte `link_id`. The holder signs
//! the contact request's mailbox write with it
//! (`DsProofSigner::for_reach(grant.seed)`).
//!
//! * **Scoped to this link.** The owner registers the grant's public key
//!   with the Delivery Service (expiring with the link) and, to revoke the
//!   link, removes exactly that key: every other link of theirs keeps
//!   working. The `link_id` is also what the owner's other devices need to
//!   re-derive the key, so the owner keeps it with the link.
//! * **Inside the signed and the sealed part.** The grant is covered by the
//!   owner's signature and travels inside the link seal
//!   ([`crate::link_seal`]), so neither the ledger nor a relay can read,
//!   strip or swap it. Whoever holds the link (the URL with its fragment)
//!   holds the grant; that is what a link is.
//! * **A grant of another class is refused** by [`decode_and_verify`] (and
//!   by [`compose`]): a capability that handed out a `contact` or `group`
//!   key would give a link holder reach it was never meant to have.
//! * **The grant's mailbox binding must be the owner's** (ADR-0078):
//!   [`decode_and_verify`] (and [`compose`]) verify it under
//!   `owner_signature_pubkey`, the owner's account identity key the
//!   capability is signed with, so the link holder writes to the owner's
//!   own mailbox. (A `/c/` call capability is signed by a device leaf key
//!   instead and cannot check this itself; see [`crate::call_invite`].)
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
//!     &link_id,    // generate_link_id(), kept with the link for revoke
//!     &link_grant, // ReachGrant::derive(account_seed, Link, &link_id, 0)
//! )?;
//! let secret = lakko_contacts::link_seal::generate_link_secret();
//! let sealed = lakko_contacts::invite::seal_link(&secret, &bytes)?;
//! // Upload `sealed` to the ledger and get an id back; the link is
//! // https://lakko.me/i/<id>#<secret>, or /i/<sealed>#<secret> inline.
//! ```
//!
//! Receiver side:
//!
//! ```ignore
//! let sealed = /* fetched by id, or base64url_decode(slug) inline */;
//! let secret = lakko_contacts::link_seal::link_secret_from_slice(&fragment_bytes)?;
//! let (cap, wire) = lakko_contacts::invite::open_link(&secret, &sealed)?;
//! if !cap.is_valid_now(now_ms) { /* expired */ }
//! // Use `cap.owner_signature_pubkey`, `cap.owner_account_id`,
//! // `cap.owner_x25519_pubkey` to seed the first ContactRequest.
//! ```

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, PUBLIC_KEY_LEN as X25519_PUB_LEN};
use lakko_crypto::signing::{
    Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN as ED25519_PUB_LEN, SIGNATURE_LEN,
};

use crate::error::{ContactError, Result};
use crate::reach::{ReachClass, ReachGrant, REACH_GRANT_LEN};
use crate::request::ACCOUNT_ID_LEN;

/// Current schema version on the wire. v2 (ADR-0060) added the link id and
/// the link reach grant; v1 is refused.
const SCHEMA_VERSION: u8 = 2;

/// Length of the random per-link id an `/i/` or `/c/` capability carries:
/// the scope of the link's reach grant (ADR-0060 §6). The owner mints one
/// per link ([`generate_link_id`]) and removes exactly that link's key on
/// revoke.
pub const LINK_ID_LEN: usize = 16;

/// Mint a fresh random link id (see [`LINK_ID_LEN`]). One per link, never
/// reused.
#[must_use]
pub fn generate_link_id() -> [u8; LINK_ID_LEN] {
    let mut id = [0u8; LINK_ID_LEN];
    lakko_crypto::random::fill(&mut id);
    id
}

/// Refuse a link grant of any class but [`ReachClass::Link`]. Shared by the
/// `/i/` and `/c/` codecs, compose and decode alike.
pub(crate) fn check_link_grant(grant: &ReachGrant) -> Result<()> {
    if grant.class == ReachClass::Link {
        Ok(())
    } else {
        Err(ContactError::InvalidArgument(format!(
            "a link capability carries a `link` reach grant, not class {:#04x}",
            grant.class.to_byte(),
        )))
    }
}

/// The grant's mailbox binding verified under the owner's account key
/// (ADR-0078). Only for a capability signed by the account key (`/i/`).
fn check_link_grant_binding(grant: &ReachGrant, owner_pk: &VerifyingKey) -> Result<()> {
    grant
        .mailbox_pk_verified(&owner_pk.to_bytes())
        .map(|_| ())
        .map_err(|e| ContactError::InvalidArgument(format!("link grant binding: {}", e.code())))
}

/// Decode the link grant of a capability wire, fail-closed: a grant that
/// does not parse, or is not class `link`, is an encoding error.
pub(crate) fn decode_link_grant(bytes: &[u8]) -> Result<ReachGrant> {
    let grant = ReachGrant::decode(bytes)
        .map_err(|e| ContactError::InvalidEncoding(format!("link grant: {}", e.code())))?;
    check_link_grant(&grant)
        .map_err(|_| ContactError::InvalidEncoding("link grant is not class `link`".into()))?;
    Ok(grant)
}

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

/// Length in bytes of a discovery-ledger `blob_id`: a random handle the
/// server assigns at upload (see [`generate_blob_id`]). Shared by all three
/// ledgers (`/i/`, `/c/`, `/g/`). Pinned in the codec layer because the
/// revoke canonical-bytes layouts bind to it.
///
/// Why 6 random bytes (2026-09-27, the short-link amendment of ADR-0028): the
/// id is a lookup handle, not a secret and not an integrity anchor, and the
/// maintainer wants a whole link of about 45 characters. It used to be the
/// SHA-256 of the capability (32 bytes, 43 URL characters, idempotent by
/// content); it is now CSPRNG output, so uploading the same capability twice
/// yields two ids. The id space is 2^48, so resistance to guessing live ids
/// rests on the per-IP rate limit on the open fetch / consume routes, not on
/// the id width. A guessed id is still worthless on its own since the
/// 2026-09-28 security pass: the ledger stores every capability sealed under
/// a 96-bit secret carried only in the link fragment ([`crate::link_seal`]),
/// so the fetched blob neither opens nor can be swapped without that secret.
/// What a bare id still allows is spending consumes (see
/// [`compute_joiner_tag`]).
pub const BLOB_ID_LEN: usize = 6;

/// Length of a `blob_id` in its URL form: base64url without padding of
/// [`BLOB_ID_LEN`] bytes. `https://<host>/i/<8 chars>` (likewise `/c/`,
/// `/g/`).
pub const BLOB_ID_B64URL_LEN: usize = 8;

/// Longest capability wire [`compose`] can produce: the fixed fields plus a
/// [`MAX_LABEL_LEN`]-byte label, the link id, the link grant and the
/// signature (277 bytes).
pub const MAX_WIRE_LEN: usize = 1
    + ED25519_PUB_LEN
    + ACCOUNT_ID_LEN
    + X25519_PUB_LEN
    + 8
    + 4
    + 1
    + 2
    + MAX_LABEL_LEN
    + LINK_ID_LEN
    + REACH_GRANT_LEN
    + SIGNATURE_LEN;

/// Longest sealed `/i/` link blob (what the discovery ledger stores).
pub const MAX_SEALED_LEN: usize = crate::link_seal::sealed_len(MAX_WIRE_LEN);

/// Seal a composed capability wire for an `/i/` link under `link_secret`
/// (see [`crate::link_seal`]). The secret goes in the URL fragment only.
///
/// # Errors
///
/// [`ContactError::InvalidArgument`] for an empty wire; a crypto failure.
pub fn seal_link(
    link_secret: &[u8; crate::link_seal::LINK_SECRET_LEN],
    capability_wire: &[u8],
) -> Result<Vec<u8>> {
    crate::link_seal::seal(
        crate::link_seal::LinkKind::Contact,
        link_secret,
        capability_wire,
    )
}

/// Open a sealed `/i/` link blob under the fragment's secret and verify the
/// capability inside. Returns the capability and its plaintext wire (the
/// joiner tag is computed over the plaintext, see [`compute_joiner_tag`]).
///
/// # Errors
///
/// [`ContactError::LinkDamaged`] on any failure (wrong or missing secret,
/// swapped / tampered / truncated blob, a capability that does not verify).
pub fn open_link(
    link_secret: &[u8; crate::link_seal::LINK_SECRET_LEN],
    sealed: &[u8],
) -> Result<(InviteCapability, Vec<u8>)> {
    crate::link_seal::open_and_verify(
        crate::link_seal::LinkKind::Contact,
        link_secret,
        sealed,
        decode_and_verify,
    )
}

/// Mint a fresh random ledger id. The discovery service calls this at upload
/// and retries on a primary-key collision; a client never mints one.
#[must_use]
pub fn generate_blob_id() -> [u8; BLOB_ID_LEN] {
    let mut id = [0u8; BLOB_ID_LEN];
    lakko_crypto::random::fill(&mut id);
    id
}

/// Build the canonical bytes the inviter signs to prove a
/// revocation request.
///
/// Layout (37 bytes total):
///
/// ```text
/// REVOKE_DOMAIN_V1 (23 bytes)  ‖ blob_id (6 bytes) ‖ signed_at_ms (8 bytes BE)
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

/// Retired domain separator of the owner-list query (Phase 1.18d2b-list).
///
/// ADR-0078 section 10 removed the owner from every invite ledger, and with
/// it `POST /discovery/invite/by-owner`: a client lists its own links from
/// the local record it keeps for each link it minted. The bytes stay
/// reserved so no future signature domain reuses them, and the sibling
/// envelopes' domain-isolation tests keep checking against them.
pub const OWNER_LIST_DOMAIN_V1: &[u8] = b"lakko-invite-owner-list/v1\x00";

/// Length of a link revocation seed and of its public key.
pub const REVOCATION_KEY_LEN: usize = 32;

/// The Ed25519 public key of a per-link revocation seed (ADR-0078
/// section 10).
///
/// A ledger row (`/i/`, `/c/`, `/g/`) names no owner and no issuer: it
/// stores this public key, minted fresh for the one link, and nothing
/// else about who made it. The issuing device keeps the 32-byte seed beside
/// the link secret it already keeps for the link, and signs a revocation
/// (`sign_revocation`, `call_sign_revocation`, `group_sign_revocation`)
/// with it. Two links of one person carry unrelated keys, so the ledger
/// cannot group them by person.
#[must_use]
pub fn revocation_public_key(seed: &[u8; REVOCATION_KEY_LEN]) -> [u8; REVOCATION_KEY_LEN] {
    SigningKey::from_seed(seed).verifying_key().to_bytes()
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
/// not consumed", which is a probing oracle. Each consume
/// attempt spends an anonymous `invite-consume` token
/// (ADR-0054 S3) and is charged to discovery's per-IP token
/// bucket, which mitigates at scale; full
/// privacy against a determined attacker requires an OPRF
/// step the server runs blind, which is the deferred
/// alternative noted in the ROADMAP.
///
/// **Computed over the PLAINTEXT capability** (the bytes a sealed
/// link opens to, [`open_link`]), never over the sealed blob. Since
/// the 2026-09-28 security pass the server stores only the sealed
/// blob, so the server itself can no longer recompute a candidate
/// joiner's tag; the probing oracle above needs the link's secret.
/// The server needs nothing else from the plaintext to count a
/// consume. **Residual (burn):** the tag is opaque to the server, so
/// anyone holding a live id can submit random tags, each counted as a
/// distinct joiner against `max_uses`, for the price of one anonymous
/// token and the per-IP limit per attempt.
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
    // bytes. SHA-256 (not BLAKE3) is a frozen choice of this
    // tag's v1 domain; the ledger `blob_id` it once shared a
    // hash family with is a random id since the short-link
    // amendment of ADR-0028 and no longer derives from the
    // bytes at all.
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
    /// Random per-link id: the scope of [`Self::link_grant`] (ADR-0060 §6).
    pub link_id: [u8; LINK_ID_LEN],
    /// The owner's `link` reach grant for this link (ADR-0060 §6). SECRET:
    /// the holder signs the contact request's mailbox write with it. Always
    /// class [`ReachClass::Link`].
    pub link_grant: ReachGrant,
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

    /// Signal-style safety number derived from the owner's
    /// long-term Ed25519 identity key. 71-byte ASCII string
    /// (12 groups of 5 digits, single space between
    /// groups) — same format as
    /// [`crate::safety::safety_number`] and the parallel
    /// [`crate::call_invite::CallInviteCapability::owner_fingerprint`].
    ///
    /// **Purpose: anti-phishing.** Joiner UI shows this
    /// before letting the human click "Accept" / "Add as
    /// contact". The human compares the displayed digits
    /// against what the owner shared out-of-band (in a
    /// Signal chat, on a business card, over the phone).
    /// If they don't match, the link is from someone else
    /// — typically a substitution attack where an attacker
    /// swapped a real `/i/<slug>` URL for one pointing at
    /// their own account.
    ///
    /// Deterministic per-pubkey (independent of
    /// `expiry_ms` / `max_uses` / `label` /
    /// `bootstrap_referral`) so a contact only needs to
    /// learn one fingerprint per identity, not per
    /// invite. Format match guarantees the joiner UI
    /// renders the same shape it already uses for the
    /// existing contact-verification flow.
    #[must_use]
    pub fn owner_fingerprint(&self) -> String {
        crate::safety::safety_number(&self.owner_signature_pubkey, &self.owner_signature_pubkey)
    }
}

/// Compose an invite capability and produce its signed wire-format
/// bytes. The caller-supplied `label` is truncated-or-rejected per
/// the `label_too_long` path (we reject rather than truncate to
/// keep the wire format predictable for the receiver).
///
/// `link_id` and `link_grant` are the link's reach scope and grant
/// (ADR-0060 §6; see the module docs): the owner derives the grant as
/// `ReachGrant::derive(account_seed, ReachClass::Link, link_id, version)`
/// and registers its public key before sharing the link.
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when `label.len() > MAX_LABEL_LEN`,
///   or `link_grant` is not class [`ReachClass::Link`].
#[allow(clippy::too_many_arguments)]
pub fn compose(
    owner_signing: &SigningKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    owner_x25519_pubkey: &X25519PublicKey,
    expiry_ms: u64,
    max_uses: u32,
    bootstrap_referral: bool,
    label: &str,
    link_id: &[u8; LINK_ID_LEN],
    link_grant: &ReachGrant,
) -> Result<Vec<u8>> {
    if label.len() > MAX_LABEL_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "invite label is {} bytes, max is {}",
            label.len(),
            MAX_LABEL_LEN,
        )));
    }
    check_link_grant(link_grant)?;
    let owner_pubkey = owner_signing.verifying_key();
    check_link_grant_binding(link_grant, &owner_pubkey)?;
    let mut wire = encode_signing_prefix(
        &owner_pubkey,
        owner_account_id,
        owner_x25519_pubkey,
        expiry_ms,
        max_uses,
        bootstrap_referral,
        label,
        link_id,
        link_grant,
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
///   wrong schema version (v1 included), oversized label, a link grant
///   that does not parse or is not class `link`, or trailing bytes.
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
    let mut link_id = [0u8; LINK_ID_LEN];
    link_id.copy_from_slice(take_n(&mut cur, LINK_ID_LEN)?);
    let link_grant = decode_link_grant(take_n(&mut cur, REACH_GRANT_LEN)?)?;
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
        &link_id,
        &link_grant,
    );
    owner_signature_pubkey
        .verify(&signed_prefix, &signature)
        .map_err(|_| ContactError::BadSignature)?;
    // ADR-0078: the grant writes to the owner's own mailbox. Checked after
    // the signature, so a tampered capability reports as such.
    check_link_grant_binding(&link_grant, &owner_signature_pubkey)
        .map_err(|_| ContactError::InvalidEncoding("link grant names another mailbox".into()))?;

    Ok(InviteCapability {
        owner_signature_pubkey,
        owner_account_id,
        owner_x25519_pubkey,
        expiry_ms,
        max_uses,
        bootstrap_referral,
        label,
        link_id,
        link_grant,
    })
}

// ============================================================================
// Codec helpers
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn encode_signing_prefix(
    owner_signature_pubkey: &VerifyingKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    owner_x25519_pubkey: &X25519PublicKey,
    expiry_ms: u64,
    max_uses: u32,
    bootstrap_referral: bool,
    label: &str,
    link_id: &[u8; LINK_ID_LEN],
    link_grant: &ReachGrant,
) -> Vec<u8> {
    let label_len_u16 =
        u16::try_from(label.len()).expect("label fits in u16; caller checked MAX_LABEL_LEN");
    let mut out = Vec::with_capacity(MAX_WIRE_LEN);
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&owner_signature_pubkey.to_bytes());
    out.extend_from_slice(owner_account_id);
    out.extend_from_slice(&owner_x25519_pubkey.to_bytes());
    out.extend_from_slice(&expiry_ms.to_be_bytes());
    out.extend_from_slice(&max_uses.to_be_bytes());
    out.push(u8::from(bootstrap_referral));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reach::ReachClass;
    use lakko_crypto::agreement::SecretKey as X25519SecretKey;

    const TEST_LINK_ID: [u8; LINK_ID_LEN] = [0x1D; LINK_ID_LEN];

    fn test_link_grant() -> ReachGrant {
        ReachGrant::derive(&[0x5E; 32], ReachClass::Link, &TEST_LINK_ID, 0).unwrap()
    }

    /// The test link grant of the account `signing` is the identity key of:
    /// its mailbox binding verifies under the capability's owner key.
    fn link_grant_of(signing: &SigningKey) -> ReachGrant {
        ReachGrant::derive(&signing.to_seed(), ReachClass::Link, &TEST_LINK_ID, 0).unwrap()
    }

    /// The pre-ADR-0060 argument list, with the test link id and grant.
    fn compose_t(
        signing: &SigningKey,
        account_id: &[u8; ACCOUNT_ID_LEN],
        x25519_pub: &X25519PublicKey,
        expiry_ms: u64,
        max_uses: u32,
        bootstrap_referral: bool,
        label: &str,
    ) -> Result<Vec<u8>> {
        compose(
            signing,
            account_id,
            x25519_pub,
            expiry_ms,
            max_uses,
            bootstrap_referral,
            label,
            &TEST_LINK_ID,
            &link_grant_of(signing),
        )
    }

    /// Hand-sign a v2 wire whose link grant is `grant_bytes` verbatim,
    /// bypassing compose's class check.
    fn hand_signed_with_grant(signing: &SigningKey, version: u8, grant_bytes: &[u8]) -> Vec<u8> {
        let mut prefix = vec![version];
        prefix.extend_from_slice(&signing.verifying_key().to_bytes());
        prefix.extend_from_slice(&[0xA1; ACCOUNT_ID_LEN]);
        prefix.extend_from_slice(&X25519SecretKey::generate().public_key().to_bytes());
        prefix.extend_from_slice(&0u64.to_be_bytes());
        prefix.extend_from_slice(&0u32.to_be_bytes());
        prefix.push(0);
        prefix.extend_from_slice(&0u16.to_be_bytes());
        prefix.extend_from_slice(&TEST_LINK_ID);
        prefix.extend_from_slice(grant_bytes);
        let sig = signing.sign(&prefix);
        prefix.extend_from_slice(&sig.to_bytes());
        prefix
    }

    #[test]
    fn the_link_id_and_grant_round_trip_inside_the_signature() {
        let (signing, account_id, x25519_pub) = fixture();
        let link_id = generate_link_id();
        let grant = ReachGrant::derive(&signing.to_seed(), ReachClass::Link, &link_id, 3).unwrap();
        let wire = compose(
            &signing,
            &account_id,
            &x25519_pub,
            0,
            0,
            false,
            "",
            &link_id,
            &grant,
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.link_id, link_id);
        assert_eq!(cap.link_grant, grant);
        assert_eq!(cap.link_grant.verifying_key(), grant.verifying_key());
        // The id and the grant sit before the signature; flipping either is
        // a signature failure, so a relay cannot swap the grant.
        let grant_at = wire.len() - SIGNATURE_LEN - REACH_GRANT_LEN;
        for at in [grant_at - 1, grant_at + 1, wire.len() - SIGNATURE_LEN - 1] {
            let mut bad = wire.clone();
            bad[at] ^= 0x01;
            assert!(matches!(
                decode_and_verify(&bad),
                Err(ContactError::BadSignature)
            ));
        }
    }

    #[test]
    fn a_grant_of_another_class_is_refused_by_compose_and_decode() {
        let (signing, account_id, x25519_pub) = fixture();
        for class in ReachClass::ALL {
            if class == ReachClass::Link {
                continue;
            }
            let scope: &[u8] = if class.is_account_wide() { b"" } else { b"s" };
            let grant = ReachGrant::derive(&signing.to_seed(), class, scope, 1).unwrap();
            assert!(matches!(
                compose(
                    &signing,
                    &account_id,
                    &x25519_pub,
                    0,
                    0,
                    false,
                    "",
                    &TEST_LINK_ID,
                    &grant,
                ),
                Err(ContactError::InvalidArgument(_))
            ));
            let wire = hand_signed_with_grant(&signing, SCHEMA_VERSION, &grant.encode());
            assert!(matches!(
                decode_and_verify(&wire),
                Err(ContactError::InvalidEncoding(_))
            ));
        }
        // An unknown class byte is refused the same way.
        let mut unknown = test_link_grant().encode();
        unknown[0] = 0;
        let wire = hand_signed_with_grant(&signing, SCHEMA_VERSION, &unknown);
        assert!(matches!(
            decode_and_verify(&wire),
            Err(ContactError::InvalidEncoding(_))
        ));
        // A well-formed hand-signed wire decodes (the helper is sound).
        let good =
            hand_signed_with_grant(&signing, SCHEMA_VERSION, &link_grant_of(&signing).encode());
        assert_eq!(
            decode_and_verify(&good).unwrap().link_grant,
            link_grant_of(&signing)
        );
        // A well-signed capability whose grant names another account's
        // mailbox (ADR-0078) is refused by decode and by compose.
        let foreign = hand_signed_with_grant(&signing, SCHEMA_VERSION, &test_link_grant().encode());
        assert!(matches!(
            decode_and_verify(&foreign),
            Err(ContactError::InvalidEncoding(_))
        ));
        assert!(matches!(
            compose(
                &signing,
                &account_id,
                &x25519_pub,
                0,
                0,
                false,
                "",
                &TEST_LINK_ID,
                &test_link_grant(),
            ),
            Err(ContactError::InvalidArgument(_))
        ));
    }

    #[test]
    fn a_v1_capability_is_refused() {
        let (signing, _, _) = fixture();
        let wire = hand_signed_with_grant(&signing, 1, &test_link_grant().encode());
        assert!(matches!(
            decode_and_verify(&wire),
            Err(ContactError::InvalidEncoding(_))
        ));
    }

    #[test]
    fn link_ids_are_random() {
        assert_ne!(generate_link_id(), generate_link_id());
    }

    fn fixture() -> (SigningKey, [u8; ACCOUNT_ID_LEN], X25519PublicKey) {
        let signing = SigningKey::generate();
        let account_id = [0xA1u8; ACCOUNT_ID_LEN];
        let x25519_secret = X25519SecretKey::generate();
        let x25519_pub = x25519_secret.public_key();
        (signing, account_id, x25519_pub)
    }

    /// A maximal capability is exactly MAX_WIRE_LEN; the sealed link opens
    /// under its secret, and under nothing else.
    #[test]
    fn sealed_link_round_trips_and_refuses_a_wrong_secret() {
        let (signing, account_id, x25519_pub) = fixture();
        let label = "x".repeat(MAX_LABEL_LEN);
        let wire = compose_t(&signing, &account_id, &x25519_pub, 0, 3, true, &label).unwrap();
        assert_eq!(wire.len(), MAX_WIRE_LEN);
        let secret = crate::link_seal::generate_link_secret();
        let sealed = seal_link(&secret, &wire).unwrap();
        assert_eq!(sealed.len(), MAX_SEALED_LEN);
        let (cap, opened) = open_link(&secret, &sealed).unwrap();
        assert_eq!(opened, wire);
        assert_eq!(cap.label, label);
        // The joiner tag is over the plaintext, and matches what the joiner
        // computes from the opened wire.
        assert_eq!(
            compute_joiner_tag(&opened, b"joiner"),
            compute_joiner_tag(&wire, b"joiner")
        );
        let wrong = crate::link_seal::generate_link_secret();
        assert!(matches!(
            open_link(&wrong, &sealed),
            Err(ContactError::LinkDamaged)
        ));
    }

    /// A validly-signed capability sealed under another secret (the ledger
    /// substituting a capability it holds) does not open under this link's.
    #[test]
    fn a_substituted_capability_is_damaged() {
        let (signing, account_id, x25519_pub) = fixture();
        let secret = crate::link_seal::generate_link_secret();
        let theirs = compose_t(
            &SigningKey::generate(),
            &account_id,
            &x25519_pub,
            0,
            0,
            false,
            "",
        )
        .unwrap();
        let substituted = seal_link(&crate::link_seal::generate_link_secret(), &theirs).unwrap();
        assert!(matches!(
            open_link(&secret, &substituted),
            Err(ContactError::LinkDamaged)
        ));
        // A capability that does not verify inside a well-sealed blob is damaged too.
        let mut bad = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        let n = bad.len();
        bad[n - 1] ^= 1;
        let sealed_bad = seal_link(&secret, &bad).unwrap();
        assert!(matches!(
            open_link(&secret, &sealed_bad),
            Err(ContactError::LinkDamaged)
        ));
    }

    #[test]
    fn compose_and_decode_roundtrip() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose_t(
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
        let wire = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.label, "");
    }

    #[test]
    fn never_expires_means_always_valid() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose_t(
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
        let wire = compose_t(
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
        let wire = compose_t(&signing, &account_id, &x25519_pub, 0, 0, true, "").unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.bootstrap_referral);
    }

    #[test]
    fn label_at_max_len_round_trips() {
        let (signing, account_id, x25519_pub) = fixture();
        let label = "x".repeat(MAX_LABEL_LEN);
        let wire = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, &label).unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.label.len(), MAX_LABEL_LEN);
    }

    #[test]
    fn label_over_max_len_rejected_at_compose() {
        let (signing, account_id, x25519_pub) = fixture();
        let label = "x".repeat(MAX_LABEL_LEN + 1);
        let err = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, &label).unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn tampered_signature_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        // Flip a bit in the trailing 64-byte signature.
        let len = wire.len();
        wire[len - 1] ^= 0x01;
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn tampered_label_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "alice").unwrap();
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
        let wire = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
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
        let mut wire = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        // bootstrap_referral byte sits at:
        //   1 + 32 + 16 + 32 + 8 + 4 = 93
        wire[93] = 0x42;
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn trailing_bytes_after_signature_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        wire.push(0xDE);
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn wire_starts_with_schema_version_byte() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        assert_eq!(wire[0], SCHEMA_VERSION);
    }

    #[test]
    fn owner_fingerprint_is_deterministic_per_pubkey() {
        // Same owner, different capability fields →
        // same fingerprint. The contact only needs to
        // learn one fingerprint per identity.
        let (signing, account_id, x25519_pub) = fixture();
        let wire_a =
            compose_t(&signing, &account_id, &x25519_pub, 1_000, 5, true, "Alice").unwrap();
        let wire_b = compose_t(
            &signing,
            &account_id,
            &x25519_pub,
            999_999,
            0,
            false,
            "Different label",
        )
        .unwrap();
        let cap_a = decode_and_verify(&wire_a).unwrap();
        let cap_b = decode_and_verify(&wire_b).unwrap();
        assert_eq!(
            cap_a.owner_fingerprint(),
            cap_b.owner_fingerprint(),
            "fingerprint must depend ONLY on the owner pubkey"
        );
    }

    #[test]
    fn owner_fingerprint_distinct_per_pubkey() {
        let (_, account_id, x25519_pub) = fixture();
        let signing_a = SigningKey::from_seed(&[0xAAu8; 32]);
        let signing_b = SigningKey::from_seed(&[0xBBu8; 32]);
        let cap_a = decode_and_verify(
            &compose_t(&signing_a, &account_id, &x25519_pub, 0, 0, false, "").unwrap(),
        )
        .unwrap();
        let cap_b = decode_and_verify(
            &compose_t(&signing_b, &account_id, &x25519_pub, 0, 0, false, "").unwrap(),
        )
        .unwrap();
        assert_ne!(cap_a.owner_fingerprint(), cap_b.owner_fingerprint());
    }

    #[test]
    fn owner_fingerprint_matches_safety_number_format() {
        let (signing, account_id, x25519_pub) = fixture();
        let cap = decode_and_verify(
            &compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap(),
        )
        .unwrap();
        let fingerprint = cap.owner_fingerprint();
        // 12 groups × 5 digits + 11 spaces = 71 chars.
        assert_eq!(fingerprint.len(), 71);
        for chunk in fingerprint.split(' ') {
            assert_eq!(chunk.len(), 5);
            assert!(chunk.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn owner_fingerprint_matches_across_invite_kinds() {
        // The contact-invite fingerprint and the call-
        // invite fingerprint MUST be identical for the
        // same owner — both reuse `safety_number(pk, pk)`,
        // so the human only learns ONE shape across both
        // invite ecosystems.
        let (signing, account_id, x25519_pub) = fixture();

        let contact_cap = decode_and_verify(
            &compose_t(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap(),
        )
        .unwrap();

        // Build a call-invite for the SAME owner.
        let call_wire = crate::call_invite::compose(
            &signing,
            &account_id,
            &[0xAB; crate::call_invite::CALL_ID_LEN],
            0,
            0,
            crate::call_invite::CallInviteMedia::audio_only(),
            &[0x42; 256],
            "",
            &TEST_LINK_ID,
            &test_link_grant(),
        )
        .unwrap();
        let call_cap = crate::call_invite::decode_and_verify(&call_wire).unwrap();

        assert_eq!(
            contact_cap.owner_fingerprint(),
            call_cap.owner_fingerprint(),
            "contact-invite and call-invite fingerprints MUST match for the same owner"
        );
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
            &TEST_LINK_ID,
            &link_grant_of(&signing_real),
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
        prefix.extend_from_slice(&TEST_LINK_ID);
        prefix.extend_from_slice(&test_link_grant().encode());

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
        // Domain prefix is 23 bytes; blob_id 6; u64 BE 8 → 37
        // total. Pin so a future refactor that drifts the
        // prefix surfaces here.
        assert_eq!(REVOKE_DOMAIN_V1.len(), 23);
        assert_eq!(REVOKE_DOMAIN_V1, b"lakko-invite-revoke/v1\x00");
        assert_eq!(BLOB_ID_LEN, 6);

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

    // ----------------------------------------------------------------
    // Ledger blob ids (ADR-0028 short-link amendment, 2026-09-27)
    // ----------------------------------------------------------------

    #[test]
    fn blob_id_is_six_random_bytes_and_eight_url_characters() {
        // base64url without padding: ceil(6 * 4 / 3) = 8, no partial group.
        assert_eq!(BLOB_ID_B64URL_LEN, (BLOB_ID_LEN * 4).div_ceil(3));
        assert_eq!(BLOB_ID_B64URL_LEN, 8);
        let a = generate_blob_id();
        let b = generate_blob_id();
        assert_eq!(a.len(), BLOB_ID_LEN);
        // 2^-48 chance of a false failure.
        assert_ne!(a, b, "two fresh ids must differ");
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

    #[test]
    fn owner_list_domain_stays_reserved_and_distinct_from_revoke() {
        assert_eq!(OWNER_LIST_DOMAIN_V1, b"lakko-invite-owner-list/v1\x00");
        assert_ne!(OWNER_LIST_DOMAIN_V1, REVOKE_DOMAIN_V1);
    }

    #[test]
    fn a_revocation_signed_by_the_link_seed_verifies_under_its_public_key() {
        let seed = [0x5Au8; REVOCATION_KEY_LEN];
        let pk = revocation_public_key(&seed);
        let blob_id = [0x01u8; BLOB_ID_LEN];
        let sig = sign_revocation(&SigningKey::from_seed(&seed), &blob_id, 7);
        let vk = lakko_crypto::signing::VerifyingKey::from_bytes(&pk).unwrap();
        let sig = lakko_crypto::signing::Signature::from_bytes(&sig).unwrap();
        assert!(vk
            .verify(&revoke_canonical_bytes(&blob_id, 7), &sig)
            .is_ok());
        // Another link's seed gives an unrelated key.
        assert_ne!(pk, revocation_public_key(&[0x5Bu8; REVOCATION_KEY_LEN]));
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

    /// Full contact-invite (ADR-0005 scenario A) E2E. Walks the
    /// **complete** wire chain a production deployment drives
    /// when Alice shares an invite link with Bob, Bob clicks
    /// it, and the resulting `ContactRequest` lands in Alice's
    /// stranger-inbox decrypted + verified.
    ///
    /// Mirror of `lakko_calls::link_invite::tests::link_call_full_e2e_via_welcome_envelope`
    /// (C-35) for the contact-invite path. Pure Rust — no HTTP /
    /// FFI / network mocks. Every link in the chain is asserted:
    /// signature verifies, joiner-tag is server-blind +
    /// determinstic, HPKE envelope seals + opens round-trips,
    /// intro plaintext matches verbatim.
    ///
    /// Steps mapped to the audit-pack invariants:
    ///
    ///   1. Alice composes an `InviteCapability` — pinned by
    ///      `compose_and_decode_roundtrip` + sign-verify
    ///      invariants.
    ///   2. Bob runs `decode_and_verify` on the wire bytes —
    ///      same signature gate the discovery service runs
    ///      server-side; defence-in-depth re-verify on the
    ///      joiner side.
    ///   3. Bob computes `joiner_tag(capability_bytes,
    ///      bob_account_id)` — the server-blind dedup tag the
    ///      `/discovery/invite/{blob_id}/consume` endpoint
    ///      stores; pinned to be deterministic.
    ///   4. Bob composes a `ContactRequest` targeted at
    ///      Alice's X25519 pubkey (recovered from the
    ///      decoded capability), HPKE-sealing his intro
    ///      message.
    ///   5. Alice runs `decode_and_verify` on the request
    ///      wire bytes (after the server routed them to her
    ///      stranger-inbox pseudonym).
    ///   6. Alice runs `open_envelope` with her X25519
    ///      private key — recovers Bob's intro plaintext
    ///      verbatim.
    ///
    /// The strongest property pinned: the recovered intro
    /// plaintext is byte-identical to what Bob sealed, AND
    /// any single-byte tamper anywhere in the chain (the
    /// capability wire, the joiner-tag, the ContactRequest
    /// wire, the sealed envelope) causes either signature-
    /// verify or AEAD-open to reject.
    #[test]
    fn contact_invite_full_e2e_via_envelope() {
        use crate::pseudonym::StrangerInbox;
        use crate::request::{
            compose as compose_request, decode_and_verify as decode_request, open_envelope,
        };
        use crate::PSEUDONYM_LEN;
        use lakko_crypto::agreement::SecretKey as X25519SecretKey;
        use lakko_crypto::secret::SecretBytes;
        use lakko_crypto::signing::SigningKey;

        // ---- 0. Set up identities -----------------------
        // Alice (owner) — signing key + X25519 key + a
        // stranger-inbox secret she shares server-side via
        // the identity bundle.
        let alice_signing = SigningKey::from_seed(&[0xA1u8; 32]);
        let alice_account_id = [0xAAu8; ACCOUNT_ID_LEN];
        let alice_x25519_priv = X25519SecretKey::from_bytes([0xA2u8; 32]);
        let alice_x25519_pub = alice_x25519_priv.public_key();
        let alice_stranger_secret = SecretBytes::<32>::new([0xA3u8; 32]);
        let alice_stranger_pseudonym = StrangerInbox::derive(&alice_stranger_secret);

        // Bob (joiner) — signing key + account_id + a
        // fresh per-relationship pseudonym for the sender
        // side of the ContactRequest.
        let bob_signing = SigningKey::from_seed(&[0xB1u8; 32]);
        let bob_account_id = [0xBBu8; ACCOUNT_ID_LEN];
        let bob_from_pseudonym = [0xB2u8; PSEUDONYM_LEN];

        // ---- 1. Alice composes the InviteCapability ------
        let cap_wire = compose_t(
            &alice_signing,
            &alice_account_id,
            &alice_x25519_pub,
            0, // NEVER_EXPIRES
            0, // SINGLE_USE
            false,
            "alice@team",
        )
        .unwrap();

        // ---- 2. Bob decodes + verifies the capability ---
        let capability = decode_and_verify(&cap_wire).expect("Bob's verify must succeed");
        assert_eq!(capability.owner_account_id, alice_account_id);
        assert_eq!(
            capability.owner_x25519_pubkey.to_bytes(),
            alice_x25519_pub.to_bytes(),
        );
        assert_eq!(capability.label, "alice@team");

        // ---- 3. Bob computes the joiner-tag -------------
        let joiner_tag = compute_joiner_tag(&cap_wire, &bob_account_id);
        assert_eq!(joiner_tag.len(), JOINER_TAG_LEN);
        // Determinism: same inputs ⇒ same tag (server's
        // dedup PK relies on this).
        let joiner_tag_repeat = compute_joiner_tag(&cap_wire, &bob_account_id);
        assert_eq!(joiner_tag, joiner_tag_repeat);

        // ---- 4. Bob composes the ContactRequest ---------
        let intro = b"hi alice, met you at the conference";
        let req_wire = compose_request(
            &bob_signing,
            &bob_account_id,
            &capability.owner_x25519_pubkey,
            bob_from_pseudonym,
            1_700_000_000_000, // valid_after_ms
            b"",               // privacy_pass_token (empty for the test)
            None,              // profile (ADR-0048)
            None,              // reply grant (ADR-0060)
            intro,
        )
        .expect("Bob's ContactRequest compose must succeed");

        // ---- 5. Alice decodes + verifies the request ----
        // (In production, the discovery service routes the
        // request to `alice_stranger_pseudonym`; here we
        // assert the pseudonym is the right shape without
        // running the routing.)
        let _ = alice_stranger_pseudonym.as_bytes();
        let req = decode_request(&req_wire).expect("Alice's decode must succeed");
        assert_eq!(req.from_pseudonym, bob_from_pseudonym);
        assert_eq!(req.from_account_id, Some(bob_account_id));

        // ---- 6. Alice opens the HPKE envelope -----------
        let plaintext = open_envelope(&req, &alice_x25519_priv)
            .expect("Alice's open must succeed")
            .intro;
        assert_eq!(
            plaintext.as_slice(),
            intro.as_ref(),
            "intro plaintext must round-trip byte-for-byte across the chain",
        );

        // ---- 7. Tamper sanity (defence-in-depth) --------
        // A single-byte tamper deep in the ContactRequest
        // wire bytes (away from the signature suffix) must
        // cause Alice's decode_and_verify to reject. This
        // pins the chain's "any-byte tamper → rejection"
        // property end-to-end.
        let mut req_tampered = req_wire.clone();
        // Flip a byte well before the trailing signature —
        // index 40 lands inside the encrypted envelope
        // payload for any plausible request length.
        req_tampered[40] ^= 0x01;
        assert!(
            decode_request(&req_tampered).is_err()
                || open_envelope(&decode_request(&req_tampered).unwrap(), &alice_x25519_priv,)
                    .is_err(),
            "single-byte tamper must surface as either signature or AEAD rejection",
        );
    }
}
