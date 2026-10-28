// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`ContactRequest`] — the cold-contact envelope per ADR-0005 §13.2.
//!
//! Sender Alice composes a request for recipient Bob:
//!
//! 1. Encrypts her optional profile (ADR-0048) and the optional intro
//!    message to Bob's identity public key via [`crate::envelope`]
//!    sealed-box (HPKE-equivalent over X25519 + ChaCha20-Poly1305).
//! 2. Wraps the result in a `ContactRequest` carrying her own
//!    identity public key, a per-recipient pseudonym, and a
//!    `valid_after_ms` freshness anchor.
//! 3. Signs the canonical encoding with her Ed25519 identity key.
//! 4. Sends the encoded bytes into Bob's stranger inbox via
//!    `services/ds`.
//!
//! On receipt Bob:
//!
//! 1. Decodes the wire format.
//! 2. Verifies the signature against `from_pubkey`.
//! 3. Opens the sealed envelope with his identity X25519 private key,
//!    recovering Alice's profile (if any) and the intro message.
//! 4. Surfaces the request to UI: Accept / Decline / Block / Ignore.
//!
//! ## Sealed plaintext (ADR-0048 §2, ADR-0060 §6, ADR-0079 §2)
//!
//! The bytes inside `sealed_envelope` are:
//!
//! ```text
//! u16be        profile_len          — 0 = no profile
//! [..]         profile envelope     — crate::profile wire, `profile_len` bytes
//! u8           reply_grant_len      — 0 = no reply grant, else exactly 134
//! [134]         reply_grant          — crate::reach::ReachGrant, class `reply` (SECRET)
//! u8           link_answer_len      — 0 = no link answer, else exactly 28
//! [16]         link_id              — the `/i/` link the sender opened
//! [12]         link_secret          — that link's secret (SECRET)
//! [..]         intro message        — everything that remains (may be empty)
//! ```
//!
//! There is no dual format: nothing is deployed, so a sealed plaintext
//! without the prefixes is simply malformed, and the outer schema moved to
//! v5 so a request framed the old way is refused before it is opened.
//!
//! ## The link answer (ADR-0079 §2)
//!
//! A request sent from an `/i/` link carries the link's id and secret, as
//! the sender received them, so the recipient's device can tell that the
//! request answers one of its own links and accept it without asking
//! (ADR-0079 §3). It is sealed to the recipient, so only the recipient reads
//! it; it proves the sender holds the link, whose secret is only ever in a
//! URL fragment. A `link_answer_len` other than 0 or 28 is a framing error.
//! Whether the answer names a live link is the recipient's decision
//! (`lakko_sync::SyncedState::link_answer_verdict`), never this codec's.
//!
//! **Who the profile belongs to.** The envelope carries no identity.
//! The recipient binds the profile to `from_pubkey`, which the request
//! signature authenticates, and treats every field as that sender's
//! claim — the username is shown only after discovery verification
//! (ADR-0048 §3).
//!
//! **A malformed profile does not drop the request.** When the framing
//! is intact but the profile bytes fail [`crate::profile::decode_profile`]
//! (an out-of-bounds name, a look-alike username, a future version),
//! [`open_envelope`] returns the intro with the profile `None`: the
//! request itself is still authentic and still wanted, and the recipient
//! falls back to the fingerprint exactly as it would for a sender with
//! no profile. A **framing** error is different — a `profile_len` that
//! overruns the plaintext, or a plaintext shorter than the prefix, leaves
//! no defined boundary for the intro, so the open fails with
//! [`ContactError::InvalidEncoding`]. Only a sender with the recipient's
//! public key and its own signing key can produce either, since the
//! plaintext is AEAD-authenticated inside a signed request.
//!
//! ## The reply grant (ADR-0060 §6)
//!
//! Since ADR-0060 the requester's own mailbox is closed, so the recipient
//! could not answer — neither the accept (the one-to-one Welcome) nor the
//! [`crate::ack`] — without a key. The requester therefore seals its
//! **`reply` reach grant** into the request: derived as
//! `ReachGrant::derive(account_seed, Reply, reply_id, 0)` over a fresh
//! random 16-byte `reply_id`, registered with the Delivery Service with a
//! 7-day expiry, and removed once the request is answered. The recipient
//! signs both writes back to the requester's mailbox with it
//! (`DsProofSigner::for_reach(grant.seed)`). Since ADR-0078 the grant
//! carries the requester's mailbox binding, and [`open_envelope`] verifies
//! it under `from_pubkey` (the key the request signature authenticates), so
//! the mailbox the recipient writes back to is the requester's own.
//! The same carrier serves the call links: a joiner's call Welcome or
//! KeyPackage forward rides a `ContactRequest` to the owner, and the reply
//! grant is how the owner (or the call's committer) delivers the Welcome
//! back.
//!
//! **A malformed reply grant fails the open, unlike a malformed profile.**
//! A `reply_grant_len` other than 0 or 134 is a framing error like any
//! other. A grant of 134 bytes that does not decode, decodes to a class
//! other than `reply`, or carries a mailbox binding that does not verify
//! under the request's `from_pubkey` (ADR-0078), is refused the same way, with
//! [`ContactError::InvalidEncoding`]: the profile is display data the
//! recipient can do without, while the reply grant is the only way to
//! answer. Opening the request without it would put an Accept button in
//! front of a person that silently cannot deliver, and a grant of another
//! class (`contact`, `group`) would hand the recipient reach the requester
//! never meant to give. Neither is a state worth rendering, and both come
//! only from a sender that is broken or hostile. An **absent** grant
//! (`reply_grant_len` 0) opens normally: the requester's mailbox may be
//! open (Everyone), and the recipient's client learns at write time.
//!
//! ## Wire format (manual codec)
//!
//! **v4** (current — ADR-0060, the reply grant in the sealed plaintext):
//!
//! ```text
//! u8           schema version (= 4)
//! [32 bytes]   from_pubkey                           — Ed25519 verifying key
//! [16 bytes]   from_account_id                       — sender's UUIDv7
//! [32 bytes]   from_pseudonym                        — fresh per-recipient pseudonym
//! u64be        valid_after_ms                        — UNIX millis; reject if too old
//! u16be        privacy_pass_token length             — 0 = no token (dev / legacy)
//! [..]         privacy_pass_token bytes              — opaque blob (RFC 9576)
//! u32be        sealed_envelope length
//! [..]         sealed_envelope bytes
//! [64 bytes]   signature                             — Ed25519 over the prefix above
//! ```
//!
//! The outer layout is the old v3's; only the sealed plaintext changed. v1
//! to v3 are refused: their sealed plaintexts have no reply-grant field,
//! and a flag day (nothing is deployed) is cheaper than a second splitter.
//! [`ContactRequest::from_account_id`] stays an `Option` for the FFI
//! surfaces' sake and is always `Some` since v4.
//!
//! The `privacy_pass_token` (Phase 1.9b2) carries a Privacy Pass /
//! App-Attest / Play-Integrity attestation blob through the envelope. The
//! token is opaque to `lakko-contacts` itself. In dev and on web builds the
//! token field is empty bytes.
//!
//! Signature covers everything from the schema-version byte through
//! the sealed-envelope bytes — i.e. the entire encoding except the
//! trailing 64-byte signature itself. Bound to the token so a
//! man-in-the-middle cannot strip / forge attestation.

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};

use crate::envelope;
use crate::error::{ContactError, Result};
use crate::invite::LINK_ID_LEN;
use crate::link_seal::{link_secrets_match, LINK_SECRET_LEN};
use crate::profile::{compose_profile, decode_profile, Profile};
use crate::pseudonym::{decode_pseudonym, PSEUDONYM_LEN};
use crate::reach::{ReachClass, ReachGrant, REACH_GRANT_LEN};

/// Current `compose`d schema version (ADR-0079 — the link answer in the
/// sealed plaintext). The only version accepted.
const SCHEMA_VERSION: u8 = 5;

/// Byte length of a link answer in the sealed plaintext: the link id and the
/// link secret (ADR-0079 §2).
pub const LINK_ANSWER_LEN: usize = LINK_ID_LEN + LINK_SECRET_LEN;

/// [`LINK_ANSWER_LEN`] as the one-byte `link_answer_len` value.
const LINK_ANSWER_WIRE_LEN: u8 = {
    assert!(LINK_ANSWER_LEN <= u8::MAX as usize);
    #[allow(clippy::cast_possible_truncation)]
    let len = LINK_ANSWER_LEN as u8;
    len
};

/// Which `/i/` link a request answers (ADR-0079 §2): the link's id and its
/// secret, as the sender opened them. SECRET: never logged (the `Debug`
/// output omits the secret).
#[derive(Clone, PartialEq, Eq)]
pub struct LinkAnswer {
    /// The link's 16-byte id ([`crate::invite::InviteCapability::link_id`]).
    pub link_id: [u8; LINK_ID_LEN],
    /// The link's 12-byte secret (the URL fragment).
    pub link_secret: [u8; LINK_SECRET_LEN],
}

impl LinkAnswer {
    /// `true` when this answer names `link_id` and carries `link_secret`.
    /// The secret is compared in constant time.
    #[must_use]
    pub fn matches(
        &self,
        link_id: &[u8; LINK_ID_LEN],
        link_secret: &[u8; LINK_SECRET_LEN],
    ) -> bool {
        // Both sides are evaluated so the time does not depend on which
        // differs.
        let id = self.link_id == *link_id;
        let secret = link_secrets_match(&self.link_secret, link_secret);
        id & secret
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.link_id);
        out.extend_from_slice(&self.link_secret);
    }
}

impl core::fmt::Debug for LinkAnswer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LinkAnswer").finish_non_exhaustive()
    }
}

/// `from_account_id` length in bytes (UUIDv7 raw, matches
/// `services/auth.accounts.id` and the rest of the Lakko stack).
pub const ACCOUNT_ID_LEN: usize = 16;

/// Hard upper bound on the Privacy Pass token blob (1.9b2). Real
/// RFC 9576 tokens land around 80–160 bytes; the cap is a sanity
/// gate against pathologically large attestation payloads
/// crashing parsers. The wire field is `u16` so any value up to
/// 65535 would otherwise decode — we cap earlier so callers see
/// a clean [`ContactError::InvalidEncoding`] instead.
pub const MAX_PRIVACY_PASS_TOKEN_LEN: usize = 2048;

/// Byte length of the `profile_len` prefix at the head of the sealed
/// plaintext (see the module docs).
pub const PROFILE_LEN_PREFIX_LEN: usize = 2;

/// Byte length of the `reply_grant_len` prefix that follows the profile in
/// the sealed plaintext (ADR-0060 §6). Its value is 0 or
/// [`REACH_GRANT_LEN`].
pub const REPLY_GRANT_LEN_PREFIX_LEN: usize = 1;

/// AAD bound to the sealed envelope. Domain-separates this envelope
/// from any future non-contact-request use of the same primitive.
const ENVELOPE_AAD: &[u8] = b"lakko-contacts/v1/contact-request";

/// A composed `ContactRequest`, ready to wire-encode.
///
/// Sender side: produced by [`compose`].
/// Recipient side: produced by [`decode_and_verify`].
#[derive(Clone, Debug)]
pub struct ContactRequest {
    /// Sender's long-term Ed25519 identity public key.
    pub from_pubkey: VerifyingKey,
    /// Sender's `account_id` (UUIDv7 raw bytes), so the receiver can
    /// drive Accept-side group creation (Phase 1.16e3b). Always `Some`
    /// since v4 (the older layouts that lacked it are refused); kept an
    /// `Option` so the FFI surfaces did not change shape.
    pub from_account_id: Option<[u8; ACCOUNT_ID_LEN]>,
    /// Fresh per-recipient pseudonym chosen by the sender. Server
    /// uses it for routing; recipient stores it as the canonical
    /// pseudonym for any reply they want to send before the contact
    /// is fully accepted.
    pub from_pseudonym: [u8; PSEUDONYM_LEN],
    /// Anti-replay anchor: UNIX milliseconds at which the request
    /// becomes valid. Recipients reject requests too far in the
    /// past (e.g. > 7 days) at the application layer.
    pub valid_after_ms: u64,
    /// Privacy Pass (RFC 9576) attestation token (Phase 1.9b2).
    /// Carried as an opaque blob — `lakko-contacts` does not
    /// interpret it. The DS ingest layer at
    /// `services/ds/src/handlers/mailbox.rs` lifts the per-
    /// stranger rate-limit ceiling for senders that supply a
    /// verifiable token.
    ///
    /// **Empty `Vec` means "no token"** — accepted on the receive
    /// path (clients without an attestation API send it empty in
    /// dev). Production builds populate this from App Attest
    /// (iOS) / Play Integrity (Android) / a server-issued token
    /// (web fallback).
    pub privacy_pass_token: Vec<u8>,
    /// Sealed envelope, opaque to anyone but the recipient.
    pub sealed_envelope: Vec<u8>,
}

/// What [`open_envelope`] recovers from a request's sealed plaintext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedRequest {
    /// The sender's profile (ADR-0048), or `None` when absent or invalid —
    /// a claim bound to the request's `from_pubkey` (module docs).
    pub profile: Option<Profile>,
    /// The sender's `reply` reach grant (ADR-0060 §6), or `None` when the
    /// request carried none. SECRET: the recipient signs the accept (the
    /// one-to-one Welcome, or a call Welcome) and the [`crate::ack`] back
    /// to the sender's mailbox with it. Always class
    /// [`ReachClass::Reply`] when present.
    pub reply_grant: Option<ReachGrant>,
    /// Which of the recipient's `/i/` links the request answers (ADR-0079
    /// §2), or `None`. SECRET: the recipient compares it with its own link
    /// records and never logs it.
    pub link_answer: Option<LinkAnswer>,
    /// The intro message, possibly empty — itself possibly a call
    /// envelope, classified with [`crate::incoming_envelope`].
    pub intro: Vec<u8>,
}

/// Compose a `ContactRequest` and produce its signed wire-format bytes.
///
/// The sender supplies their Ed25519 signing key (for authentication),
/// the recipient's X25519 public key (for sealed-envelope encryption),
/// an optional profile (ADR-0048), an optional `reply` reach grant
/// (ADR-0060 §6, see the module docs), an optional link answer (ADR-0079
/// §2: the `/i/` link the request is sent from), an optional intro message
/// body, and
/// an optional Privacy Pass
/// attestation token (Phase 1.9b2). `from_pseudonym` should be
/// freshly generated per-recipient (use
/// [`crate::pseudonym::PerRelationship::derive`] with a one-shot
/// seed, or just 32 random bytes — a pseudonym is just routing
/// metadata).
///
/// Pass an **empty slice** as `privacy_pass_token` to omit it
/// (dev environments + web fallback). Production mobile builds
/// supply the platform-attestation blob (App Attest on iOS, Play
/// Integrity on Android).
///
/// # Errors
///
/// * [`ContactError::Crypto`] on AEAD failure (essentially
///   impossible for valid inputs).
/// * [`ContactError::InvalidEncoding`] when
///   `privacy_pass_token.len() > MAX_PRIVACY_PASS_TOKEN_LEN`.
/// * [`ContactError::InvalidArgument`] when `profile` violates a bound —
///   the same bounds the recipient's decoder enforces, so a conforming
///   sender never seals a profile the recipient would drop — or when
///   `reply_grant` is not class [`ReachClass::Reply`] or its mailbox binding
///   does not verify under `sender_signing`'s public key (ADR-0078).
#[allow(clippy::too_many_arguments)]
pub fn compose(
    sender_signing: &SigningKey,
    sender_account_id: &[u8; ACCOUNT_ID_LEN],
    recipient_x25519_pub: &X25519PublicKey,
    from_pseudonym: [u8; PSEUDONYM_LEN],
    valid_after_ms: u64,
    privacy_pass_token: &[u8],
    profile: Option<&Profile>,
    reply_grant: Option<&ReachGrant>,
    link_answer: Option<&LinkAnswer>,
    intro_message: &[u8],
) -> Result<Vec<u8>> {
    if privacy_pass_token.len() > MAX_PRIVACY_PASS_TOKEN_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "privacy_pass_token must be <= {MAX_PRIVACY_PASS_TOKEN_LEN} bytes, got {}",
            privacy_pass_token.len(),
        )));
    }
    if let Some(grant) = reply_grant {
        grant
            .mailbox_pk_verified(&sender_signing.verifying_key().to_bytes())
            .map_err(|e| {
                ContactError::InvalidArgument(format!("reply grant binding: {}", e.code()))
            })?;
    }
    let sealed_plaintext =
        frame_sealed_plaintext(profile, reply_grant, link_answer, intro_message)?;
    let sealed_envelope = envelope::seal(recipient_x25519_pub, ENVELOPE_AAD, &sealed_plaintext)?;

    let from_pubkey = sender_signing.verifying_key();
    let mut wire = encode_signing_prefix(
        &from_pubkey,
        sender_account_id,
        &from_pseudonym,
        valid_after_ms,
        privacy_pass_token,
        &sealed_envelope,
    );
    let signature = sender_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Recipient-side: decode the wire bytes, verify the Ed25519
/// signature, return the validated [`ContactRequest`].
///
/// **Does not open the sealed envelope** — call [`open_envelope`]
/// after the recipient has decided to act on the request. Splitting
/// this lets the UI render "pending request from `<fingerprint>`"
/// before paying the X25519 + AEAD cost on accept.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format is malformed.
/// - [`ContactError::BadSignature`] — Ed25519 verification failed.
pub fn decode_and_verify(wire: &[u8]) -> Result<ContactRequest> {
    let mut cur = wire;

    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported ContactRequest schema version {version}",
        )));
    }
    let from_pubkey_bytes = take_n(&mut cur, PUBLIC_KEY_LEN)?;
    let from_pubkey = VerifyingKey::from_bytes(from_pubkey_bytes).map_err(ContactError::Crypto)?;

    let mut from_account_id = [0u8; ACCOUNT_ID_LEN];
    from_account_id.copy_from_slice(take_n(&mut cur, ACCOUNT_ID_LEN)?);

    let from_pseudonym = decode_pseudonym(take_n(&mut cur, PSEUDONYM_LEN)?)?;
    let valid_after_ms = u64::from_be_bytes(take_array::<8>(&mut cur)?);

    let token_len = usize::from(u16::from_be_bytes(take_array::<2>(&mut cur)?));
    if token_len > MAX_PRIVACY_PASS_TOKEN_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "privacy_pass_token over cap ({token_len} > {MAX_PRIVACY_PASS_TOKEN_LEN})",
        )));
    }
    let privacy_pass_token = take_n(&mut cur, token_len)?.to_vec();

    let sealed_len = usize::try_from(u32::from_be_bytes(take_array::<4>(&mut cur)?))
        .map_err(|_| ContactError::InvalidEncoding("sealed length overflows usize".into()))?;
    let sealed_envelope = take_n(&mut cur, sealed_len)?.to_vec();
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
        &from_pubkey,
        &from_account_id,
        &from_pseudonym,
        valid_after_ms,
        &privacy_pass_token,
        &sealed_envelope,
    );
    from_pubkey
        .verify(&signed_prefix, &signature)
        .map_err(|_| ContactError::BadSignature)?;

    Ok(ContactRequest {
        from_pubkey,
        from_account_id: Some(from_account_id),
        from_pseudonym,
        valid_after_ms,
        privacy_pass_token,
        sealed_envelope,
    })
}

/// Decrypt the sealed envelope on a previously-verified
/// [`ContactRequest`] using the recipient's X25519 private key.
///
/// Returns the profile, the reply grant and the intro message. The
/// profile, when present, is the sender's claim bound to `req.from_pubkey`
/// — see the module docs. A profile that fails its bounds yields `None`
/// with the rest intact; a reply grant that fails its bounds fails the
/// open (module docs).
///
/// # Errors
///
/// - [`ContactError::WrongRecipient`] — envelope was sealed for a
///   different recipient (or tampered with after sealing).
/// - [`ContactError::InvalidEncoding`] — envelope wire format is
///   malformed, the sealed plaintext's framing is, or the reply grant
///   does not decode, is not class `reply`, or names a mailbox
///   `req.from_pubkey` did not bind (ADR-0078).
pub fn open_envelope(
    req: &ContactRequest,
    recipient_priv: &X25519SecretKey,
) -> Result<OpenedRequest> {
    let plaintext = envelope::open(recipient_priv, ENVELOPE_AAD, &req.sealed_envelope)?;
    let (profile, reply_grant, link_answer, intro) = split_sealed_plaintext(&plaintext)?;
    if let Some(grant) = &reply_grant {
        grant
            .mailbox_pk_verified(&req.from_pubkey.to_bytes())
            .map_err(|e| {
                ContactError::InvalidEncoding(format!("reply grant binding: {}", e.code()))
            })?;
    }
    Ok(OpenedRequest {
        profile,
        reply_grant,
        link_answer,
        intro: intro.to_vec(),
    })
}

/// Build the sealed plaintext
/// `profile_len ‖ profile ‖ reply_grant_len ‖ reply_grant ‖ link_answer_len ‖
/// link_answer ‖ intro`.
fn frame_sealed_plaintext(
    profile: Option<&Profile>,
    reply_grant: Option<&ReachGrant>,
    link_answer: Option<&LinkAnswer>,
    intro_message: &[u8],
) -> Result<Vec<u8>> {
    let profile_bytes = match profile {
        Some(p) => compose_profile(p)
            .map_err(|e| ContactError::InvalidArgument(format!("profile: {e}")))?,
        None => Vec::new(),
    };
    if let Some(grant) = reply_grant {
        if grant.class != ReachClass::Reply {
            return Err(ContactError::InvalidArgument(format!(
                "a contact request carries a `reply` reach grant, not class {:#04x}",
                grant.class.to_byte(),
            )));
        }
    }
    // A composed profile is at most `PROFILE_ENVELOPE_MAX_LEN` bytes, far
    // below `u16::MAX`; checked rather than cast all the same.
    let profile_len = u16::try_from(profile_bytes.len())
        .map_err(|_| ContactError::InvalidArgument("profile envelope too large".to_string()))?;
    let grant_len = if reply_grant.is_some() {
        REACH_GRANT_LEN
    } else {
        0
    };
    let mut out = Vec::with_capacity(
        PROFILE_LEN_PREFIX_LEN
            + profile_bytes.len()
            + REPLY_GRANT_LEN_PREFIX_LEN
            + grant_len
            + 1
            + LINK_ANSWER_LEN
            + intro_message.len(),
    );
    out.extend_from_slice(&profile_len.to_be_bytes());
    out.extend_from_slice(&profile_bytes);
    match reply_grant {
        Some(grant) => {
            // REACH_GRANT_LEN is 134, a one-byte value by construction.
            out.push(REPLY_GRANT_WIRE_LEN);
            out.extend_from_slice(&grant.encode());
        },
        None => out.push(0),
    }
    match link_answer {
        Some(answer) => {
            out.push(LINK_ANSWER_WIRE_LEN);
            answer.encode_into(&mut out);
        },
        None => out.push(0),
    }
    out.extend_from_slice(intro_message);
    Ok(out)
}

/// [`REACH_GRANT_LEN`] as the one-byte `reply_grant_len` value.
const REPLY_GRANT_WIRE_LEN: u8 = {
    assert!(REACH_GRANT_LEN <= u8::MAX as usize);
    #[allow(clippy::cast_possible_truncation)]
    let len = REACH_GRANT_LEN as u8;
    len
};

/// The four parts of an opened sealed plaintext.
type SplitPlaintext<'a> = (
    Option<Profile>,
    Option<ReachGrant>,
    Option<LinkAnswer>,
    &'a [u8],
);

/// Split an opened sealed plaintext into `(profile, reply_grant,
/// link_answer, intro)`.
///
/// Framing errors are errors; a well-framed but invalid profile is
/// `None`; a present but invalid reply grant is an error (see the module
/// docs for why the three differ); a link answer is framing only.
pub(crate) fn split_sealed_plaintext(plaintext: &[u8]) -> Result<SplitPlaintext<'_>> {
    let mut cur = plaintext;
    let len_bytes: [u8; PROFILE_LEN_PREFIX_LEN] = take_n(&mut cur, PROFILE_LEN_PREFIX_LEN)?
        .try_into()
        .map_err(|_| ContactError::InvalidEncoding("profile length prefix".to_string()))?;
    let profile_len = usize::from(u16::from_be_bytes(len_bytes));
    let profile_bytes = take_n(&mut cur, profile_len)?;
    let profile = if profile_bytes.is_empty() {
        None
    } else {
        match decode_profile(profile_bytes) {
            Ok(p) => Some(p),
            Err(e) => {
                // The reason code only: the profile's content is the
                // sender's personal data and never reaches a log.
                tracing::warn!(reason = %e.code(), "contact request carried an invalid profile; opening without it");
                None
            },
        }
    };
    let reply_grant = match take_u8(&mut cur)? {
        0 => None,
        REPLY_GRANT_WIRE_LEN => {
            let grant = ReachGrant::decode(take_n(&mut cur, REACH_GRANT_LEN)?)
                .map_err(|e| ContactError::InvalidEncoding(format!("reply grant: {}", e.code())))?;
            if grant.class != ReachClass::Reply {
                return Err(ContactError::InvalidEncoding(
                    "reply grant is not class `reply`".to_string(),
                ));
            }
            Some(grant)
        },
        other => {
            return Err(ContactError::InvalidEncoding(format!(
                "reply grant length {other}, expected 0 or {REACH_GRANT_LEN}",
            )));
        },
    };
    let link_answer = match take_u8(&mut cur)? {
        0 => None,
        LINK_ANSWER_WIRE_LEN => {
            let link_id = take_array::<LINK_ID_LEN>(&mut cur)?;
            let link_secret = take_array::<LINK_SECRET_LEN>(&mut cur)?;
            Some(LinkAnswer {
                link_id,
                link_secret,
            })
        },
        other => {
            return Err(ContactError::InvalidEncoding(format!(
                "link answer length {other}, expected 0 or {LINK_ANSWER_LEN}",
            )));
        },
    };
    Ok((profile, reply_grant, link_answer, cur))
}

/// The fuzz oracle behind `fuzz_api::contact_request_plaintext`.
pub(crate) fn fuzz_split(plain: &[u8]) -> Option<bool> {
    let (profile, reply_grant, link_answer, intro) = split_sealed_plaintext(plain).ok()?;
    if reply_grant
        .as_ref()
        .is_some_and(|g| g.class != ReachClass::Reply)
    {
        return Some(false);
    }
    // Only a decoded profile re-frames byte-identically (an invalid one is
    // dropped to `None` on purpose), so re-frame and compare only then or
    // when the profile field was empty.
    let profile_len = usize::from(u16::from_be_bytes([plain[0], plain[1]]));
    if profile.is_none() && profile_len != 0 {
        return Some(true);
    }
    let reframed = frame_sealed_plaintext(
        profile.as_ref(),
        reply_grant.as_ref(),
        link_answer.as_ref(),
        intro,
    )
    .ok()?;
    Some(reframed == plain)
}

// ============================================================================
// Codec helpers
// ============================================================================

/// Encode the bytes that get signed for the current (v3) wire
/// format: everything except the trailing signature itself.
fn encode_signing_prefix(
    from_pubkey: &VerifyingKey,
    from_account_id: &[u8; ACCOUNT_ID_LEN],
    from_pseudonym: &[u8; PSEUDONYM_LEN],
    valid_after_ms: u64,
    privacy_pass_token: &[u8],
    sealed_envelope: &[u8],
) -> Vec<u8> {
    let sealed_len = u32::try_from(sealed_envelope.len())
        .expect("sealed envelope under u32::MAX bytes — caller invariant");
    let token_len = u16::try_from(privacy_pass_token.len())
        .expect("privacy_pass_token capped at MAX_PRIVACY_PASS_TOKEN_LEN (<= u16::MAX)");
    let mut out = Vec::with_capacity(
        1 + PUBLIC_KEY_LEN
            + ACCOUNT_ID_LEN
            + PSEUDONYM_LEN
            + 8
            + 2
            + privacy_pass_token.len()
            + 4
            + sealed_envelope.len(),
    );
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&from_pubkey.to_bytes());
    out.extend_from_slice(from_account_id);
    out.extend_from_slice(from_pseudonym);
    out.extend_from_slice(&valid_after_ms.to_be_bytes());
    out.extend_from_slice(&token_len.to_be_bytes());
    out.extend_from_slice(privacy_pass_token);
    out.extend_from_slice(&sealed_len.to_be_bytes());
    out.extend_from_slice(sealed_envelope);
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

fn take_array<const N: usize>(cur: &mut &[u8]) -> Result<[u8; N]> {
    let bytes = take_n(cur, N)?;
    let mut out = [0u8; N];
    out.copy_from_slice(bytes);
    Ok(out)
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
    use lakko_crypto::signing::SigningKey;

    fn sender() -> SigningKey {
        SigningKey::from_seed(&[7u8; 32])
    }

    /// Stable test `account_id`. Real callers pass a real
    /// UUIDv7; the wire format is byte-agnostic.
    fn sender_account_id() -> [u8; ACCOUNT_ID_LEN] {
        [0x77; ACCOUNT_ID_LEN]
    }

    fn recipient_x25519() -> X25519SecretKey {
        // X25519 keypair (separate from the recipient's Ed25519
        // identity key — in practice the client derives one from
        // the other or maintains both side by side).
        X25519SecretKey::from_bytes([0xCD; 32])
    }

    #[test]
    fn compose_and_decode_roundtrip() {
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0xAA; PSEUDONYM_LEN];

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            1_000_000,
            b"",
            None,
            None,
            None,
            b"hi bob",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();

        assert_eq!(req.from_pubkey.to_bytes(), alice.verifying_key().to_bytes());
        assert_eq!(req.from_account_id, Some(sender_account_id()));
        assert_eq!(req.from_pseudonym, pseudonym);
        assert_eq!(req.valid_after_ms, 1_000_000);
        assert!(req.privacy_pass_token.is_empty());

        let opened = open_envelope(&req, &bob).unwrap();
        assert_eq!(opened.profile, None);
        assert_eq!(opened.reply_grant, None);
        assert_eq!(opened.intro, b"hi bob");
    }

    /// Phase 1.16e3b: v2 wire layout. `from_account_id` lands
    /// between `from_pubkey` (offset 1..33) and `from_pseudonym`
    /// (offset 1+32+16 = 49 in v2). Pin the offset down with a
    /// direct byte read so a future schema reshuffle surfaces
    /// here.
    #[test]
    fn v2_wire_carries_account_id_at_fixed_offset() {
        let alice = sender();
        let bob = recipient_x25519();
        let acct = [0xA5u8; ACCOUNT_ID_LEN];

        let wire = compose(
            &alice,
            &acct,
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            None,
            None,
            None,
            b"",
        )
        .unwrap();

        // Byte 0: schema version.
        assert_eq!(wire[0], SCHEMA_VERSION);
        // Bytes 1..=32: from_pubkey.
        assert_eq!(
            &wire[1..=PUBLIC_KEY_LEN],
            &alice.verifying_key().to_bytes()[..]
        );
        // Bytes 33..=48: from_account_id.
        let acct_start = 1 + PUBLIC_KEY_LEN;
        let acct_end = acct_start + ACCOUNT_ID_LEN;
        assert_eq!(&wire[acct_start..acct_end], &acct[..]);
    }

    #[test]
    fn empty_intro_message_is_supported() {
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0u8; PSEUDONYM_LEN];

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"",
            None,
            None,
            None,
            b"",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();
        assert_eq!(open_envelope(&req, &bob).unwrap().intro, b"");
    }

    #[test]
    fn tampered_signature_rejected() {
        let alice = sender();
        let bob = recipient_x25519();

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            None,
            None,
            None,
            b"intro",
        )
        .unwrap();
        // Flip a bit in the trailing signature.
        let last = wire.len() - 1;
        wire[last] ^= 0x01;

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn tampered_pseudonym_rejected() {
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0xAB; PSEUDONYM_LEN];

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"",
            None,
            None,
            None,
            b"intro",
        )
        .unwrap();
        // Flip a byte inside the v3 signed prefix. Offset is
        // schema-version + from_pubkey + from_account_id, i.e.
        // the first byte of from_pseudonym.
        wire[1 + PUBLIC_KEY_LEN + ACCOUNT_ID_LEN] ^= 0xFF;

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn tampered_envelope_breaks_open_not_verify() {
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0u8; PSEUDONYM_LEN];

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"",
            None,
            None,
            None,
            b"secret",
        )
        .unwrap();
        // The signature covers the envelope, so any tamper is
        // signature-detected first. Confirm that.
        let mid = wire.len() / 2;
        wire[mid] ^= 0xFF;
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn wrong_recipient_fails_open_envelope() {
        let alice = sender();
        let bob = recipient_x25519();
        let attacker = X25519SecretKey::from_bytes([0xEF; 32]);

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            None,
            None,
            None,
            b"private",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();
        let err = open_envelope(&req, &attacker).unwrap_err();
        assert!(matches!(err, ContactError::WrongRecipient));
    }

    #[test]
    fn wrong_schema_version_rejected() {
        let alice = sender();
        let bob = recipient_x25519();

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            None,
            None,
            None,
            b"x",
        )
        .unwrap();
        wire[0] = 99;

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn truncated_wire_rejected() {
        let alice = sender();
        let bob = recipient_x25519();

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            None,
            None,
            None,
            b"x",
        )
        .unwrap();
        let truncated = &wire[..wire.len() - 5];
        let err = decode_and_verify(truncated).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn trailing_bytes_rejected() {
        let alice = sender();
        let bob = recipient_x25519();

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            None,
            None,
            None,
            b"x",
        )
        .unwrap();
        wire.push(0x00);
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn fresh_compose_produces_distinct_wires() {
        // Same inputs but a fresh ephemeral envelope every call —
        // the bytes must differ.
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0u8; PSEUDONYM_LEN];

        let w1 = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"",
            None,
            None,
            None,
            b"intro",
        )
        .unwrap();
        let w2 = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"",
            None,
            None,
            None,
            b"intro",
        )
        .unwrap();
        assert_ne!(w1, w2);
    }

    // ========================================================
    // 1.9b2 — Privacy Pass token (v3 wire format)
    // ========================================================

    /// A v3 envelope with a non-empty token round-trips, the
    /// signature covers the token, and the parsed
    /// `privacy_pass_token` matches what the sender supplied.
    #[test]
    fn v3_privacy_pass_token_roundtrip() {
        let alice = sender();
        let bob = recipient_x25519();
        let token = b"\x01\x02\x03\x04 token-blob \xFF\xEE".as_slice();

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0xAB; PSEUDONYM_LEN],
            0,
            token,
            None,
            None,
            None,
            b"hello",
        )
        .unwrap();

        let req = decode_and_verify(&wire).unwrap();
        assert_eq!(req.privacy_pass_token.as_slice(), token);

        assert_eq!(open_envelope(&req, &bob).unwrap().intro, b"hello");
    }

    /// Tampering the on-wire token bytes invalidates the
    /// signature — token integrity is bound to the signed
    /// prefix, not stapled on as an afterthought.
    #[test]
    fn v3_tampered_token_breaks_signature() {
        let alice = sender();
        let bob = recipient_x25519();
        let token = b"\x10\x20\x30\x40".as_slice();

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            token,
            None,
            None,
            None,
            b"",
        )
        .unwrap();

        // Token lands at offset
        //   1 (version) + 32 (pubkey) + 16 (acct) + 32 (pseudo)
        //   + 8 (valid_after_ms) + 2 (token_len) = 91.
        let token_offset = 1 + PUBLIC_KEY_LEN + ACCOUNT_ID_LEN + PSEUDONYM_LEN + 8 + 2;
        wire[token_offset] ^= 0xFF;

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    /// `MAX_PRIVACY_PASS_TOKEN_LEN` is enforced on `compose`. The
    /// receive path also enforces it (via the u16 length cap
    /// check) so a malicious sender can't smuggle pathologically
    /// large blobs through a permissive composer.
    #[test]
    fn v3_token_over_cap_rejected_at_compose() {
        let alice = sender();
        let bob = recipient_x25519();
        let huge = vec![0u8; MAX_PRIVACY_PASS_TOKEN_LEN + 1];

        let err = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            &huge,
            None,
            None,
            None,
            b"",
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    // ========================================================
    // ADR-0048 — profile in the sealed plaintext
    // ========================================================

    fn alice_profile() -> Profile {
        Profile {
            display_name: Some("Alice Petrova".to_string()),
            username: Some("alice".to_string()),
            updated_at_ms: 1_700_000_000_000,
            account_id: None,
            username_certificate: None,
            grants: Vec::new(),
            avatar: None,
        }
    }

    /// Seal `plaintext` as-is (bypassing `compose`'s framing) inside an
    /// otherwise-valid, correctly signed v3 request.
    fn signed_request_sealing(plaintext: &[u8]) -> ContactRequest {
        let alice = sender();
        let bob = recipient_x25519();
        let sealed_envelope = envelope::seal(&bob.public_key(), ENVELOPE_AAD, plaintext).unwrap();
        let mut wire = encode_signing_prefix(
            &alice.verifying_key(),
            &sender_account_id(),
            &[0u8; PSEUDONYM_LEN],
            0,
            b"",
            &sealed_envelope,
        );
        let signature = alice.sign(&wire);
        wire.extend_from_slice(&signature.to_bytes());
        decode_and_verify(&wire).unwrap()
    }

    #[test]
    fn profile_and_intro_round_trip() {
        let alice = sender();
        let bob = recipient_x25519();
        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            Some(&alice_profile()),
            None,
            None,
            b"hi bob",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();
        let opened = open_envelope(&req, &bob).unwrap();
        assert_eq!(opened.profile, Some(alice_profile()));
        assert_eq!(opened.intro, b"hi bob");
    }

    #[test]
    fn profile_with_empty_intro_round_trips() {
        let bob = recipient_x25519();
        let wire = compose(
            &sender(),
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            Some(&alice_profile()),
            None,
            None,
            b"",
        )
        .unwrap();
        let opened = open_envelope(&decode_and_verify(&wire).unwrap(), &bob).unwrap();
        assert_eq!(opened.profile, Some(alice_profile()));
        assert!(opened.intro.is_empty());
    }

    #[test]
    fn sealed_plaintext_framing_is_pinned() {
        let grant = reply_grant();
        let framed =
            frame_sealed_plaintext(Some(&alice_profile()), Some(&grant), None, b"intro").unwrap();
        let profile_wire = crate::profile::compose_profile(&alice_profile()).unwrap();
        let len = u16::try_from(profile_wire.len()).unwrap();
        let p = profile_wire.len();
        assert_eq!(&framed[..2], &len.to_be_bytes());
        assert_eq!(&framed[2..2 + p], profile_wire.as_slice());
        assert_eq!(framed[2 + p], 134);
        assert_eq!(&framed[3 + p..3 + p + 134], &grant.encode());
        assert_eq!(framed[3 + p + 134], 0);
        assert_eq!(&framed[4 + p + 134..], b"intro");
        // Nothing: three zero prefixes, then the intro verbatim.
        assert_eq!(
            frame_sealed_plaintext(None, None, None, b"x").unwrap(),
            b"\x00\x00\x00\x00x"
        );
    }

    fn reply_grant() -> ReachGrant {
        // Derived from the sender's own account seed: its binding must
        // verify under `from_pubkey` (ADR-0078).
        ReachGrant::derive(&[7u8; 32], ReachClass::Reply, &[0x9A; 16], 0).unwrap()
    }

    /// ADR-0060 §6: the requester's reply grant rides the sealed plaintext
    /// beside a profile and an intro, and comes out whole.
    #[test]
    fn a_reply_grant_round_trips_with_and_without_the_rest() {
        let bob = recipient_x25519();
        let grant = reply_grant();
        for (profile, intro) in [
            (Some(alice_profile()), b"hi bob".as_slice()),
            (None, b"".as_slice()),
            (Some(alice_profile()), b"".as_slice()),
        ] {
            let wire = compose(
                &sender(),
                &sender_account_id(),
                &bob.public_key(),
                [0u8; PSEUDONYM_LEN],
                0,
                b"",
                profile.as_ref(),
                Some(&grant),
                None,
                intro,
            )
            .unwrap();
            let opened = open_envelope(&decode_and_verify(&wire).unwrap(), &bob).unwrap();
            assert_eq!(opened.profile, profile);
            assert_eq!(opened.reply_grant.as_ref(), Some(&grant));
            assert_eq!(opened.intro, intro);
        }
        // An intro that is itself a call envelope keeps its bytes exactly.
        let welcome = crate::call_welcome::wrap_call_welcome(&[0xAB; 64]);
        let wire = compose(
            &sender(),
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            None,
            Some(&grant),
            None,
            &welcome,
        )
        .unwrap();
        let opened = open_envelope(&decode_and_verify(&wire).unwrap(), &bob).unwrap();
        assert_eq!(opened.intro, welcome);
    }

    #[test]
    fn compose_refuses_a_reply_grant_bound_to_another_mailbox() {
        let bob = recipient_x25519();
        let foreign = ReachGrant::derive(&[0x3C; 32], ReachClass::Reply, &[0x9A; 16], 0).unwrap();
        let err = compose(
            &sender(),
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            None,
            Some(&foreign),
            None,
            b"",
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn compose_refuses_a_grant_of_another_class() {
        for class in ReachClass::ALL {
            if class == ReachClass::Reply {
                continue;
            }
            let scope: &[u8] = if class.is_account_wide() { b"" } else { b"s" };
            let grant = ReachGrant::derive(&[7; 32], class, scope, 1).unwrap();
            let err = compose(
                &sender(),
                &sender_account_id(),
                &recipient_x25519().public_key(),
                [0u8; PSEUDONYM_LEN],
                0,
                b"",
                None,
                Some(&grant),
                None,
                b"hi",
            )
            .unwrap_err();
            assert!(matches!(err, ContactError::InvalidArgument(_)), "{class:?}");
        }
    }

    /// The decision this carrier makes for the grant, unlike the profile: a
    /// present but invalid reply grant fails the open.
    #[test]
    fn an_invalid_reply_grant_fails_the_open() {
        let bob = recipient_x25519();
        let mut cases: Vec<Vec<u8>> = Vec::new();
        // Every other class.
        for class in ReachClass::ALL {
            if class == ReachClass::Reply {
                continue;
            }
            let scope: &[u8] = if class.is_account_wide() { b"" } else { b"s" };
            let grant = ReachGrant::derive(&[7; 32], class, scope, 1).unwrap();
            let mut plain = vec![0, 0, 134];
            plain.extend_from_slice(&grant.encode());
            plain.extend_from_slice(b"hi");
            cases.push(plain);
        }
        // An unknown class byte.
        let mut unknown = reply_grant().encode();
        unknown[0] = 0xEE;
        let mut plain = vec![0, 0, 134];
        plain.extend_from_slice(&unknown);
        cases.push(plain);
        // A reply grant whose binding names someone else's mailbox
        // (ADR-0078): it decodes, but does not verify under `from_pubkey`.
        let foreign = ReachGrant::derive(&[0x3C; 32], ReachClass::Reply, &[0x9A; 16], 0).unwrap();
        let mut plain = vec![0, 0, 134];
        plain.extend_from_slice(&foreign.encode());
        cases.push(plain);
        // A length that is neither 0 nor 134.
        for len in [1u8, 36, 37, 133, 135, 0xFF] {
            let mut plain = vec![0, 0, len];
            plain.extend_from_slice(&[3u8; 64]);
            cases.push(plain);
        }
        // 134 declared, fewer carried.
        let mut short = vec![0, 0, 134];
        short.extend_from_slice(&reply_grant().encode()[..133]);
        cases.push(short);
        for plaintext in cases {
            let req = signed_request_sealing(&plaintext);
            assert!(
                matches!(
                    open_envelope(&req, &bob),
                    Err(ContactError::InvalidEncoding(_))
                ),
                "{plaintext:?}"
            );
        }
        // Control: the well-formed grant opens.
        let mut good = vec![0, 0, 134];
        good.extend_from_slice(&reply_grant().encode());
        good.push(0);
        let opened = open_envelope(&signed_request_sealing(&good), &bob).unwrap();
        assert_eq!(opened.reply_grant, Some(reply_grant()));
        assert!(opened.intro.is_empty());
    }

    #[test]
    fn older_schema_versions_are_refused() {
        let bob = recipient_x25519();
        let wire = compose(
            &sender(),
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            None,
            None,
            None,
            b"x",
        )
        .unwrap();
        for version in [1u8, 2, 3, 4] {
            let mut old = wire.clone();
            old[0] = version;
            assert!(matches!(
                decode_and_verify(&old),
                Err(ContactError::InvalidEncoding(_))
            ));
        }
    }

    #[test]
    fn compose_refuses_an_out_of_bounds_profile() {
        let mut bad = alice_profile();
        bad.username = Some("p\u{430}vel".to_string());
        let err = compose(
            &sender(),
            &sender_account_id(),
            &recipient_x25519().public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            Some(&bad),
            None,
            None,
            b"hi",
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    /// The decision this carrier makes: a well-framed but invalid profile
    /// is dropped, the authentic request and its intro are kept.
    #[test]
    fn a_malformed_profile_opens_the_request_without_it() {
        let bob = recipient_x25519();
        // A profile whose name carries a right-to-left override.
        let name = "Alice\u{202E}".as_bytes();
        let mut bad_profile = crate::profile::PROFILE_ENVELOPE_DOMAIN_V1.to_vec();
        bad_profile.push(crate::profile::PROFILE_VERSION_V1);
        bad_profile.extend_from_slice(&0u64.to_be_bytes());
        bad_profile.push(u8::try_from(name.len()).unwrap());
        bad_profile.extend_from_slice(name);
        bad_profile.push(0);
        // Control: the same bytes are refused by the profile decoder.
        assert!(crate::profile::decode_profile(&bad_profile).is_err());

        for garbage in [bad_profile.clone(), b"not a profile at all".to_vec()] {
            let mut plaintext = u16::try_from(garbage.len()).unwrap().to_be_bytes().to_vec();
            plaintext.extend_from_slice(&garbage);
            plaintext.push(134);
            plaintext.extend_from_slice(&reply_grant().encode());
            plaintext.push(0);
            plaintext.extend_from_slice(b"hi bob");
            let req = signed_request_sealing(&plaintext);
            let opened = open_envelope(&req, &bob).unwrap();
            assert_eq!(opened.profile, None);
            // The grant after an invalid profile is still found: the
            // profile's length prefix, not its content, bounds it.
            assert_eq!(opened.reply_grant, Some(reply_grant()));
            assert_eq!(opened.intro, b"hi bob");
        }
    }

    /// Framing errors, unlike profile errors, leave no intro boundary and
    /// fail the open.
    #[test]
    fn broken_framing_fails_the_open() {
        let bob = recipient_x25519();
        for plaintext in [
            Vec::new(),
            vec![0x00],
            // No reply-grant length after an empty profile.
            vec![0x00, 0x00],
            // No link-answer length after an empty reply grant.
            vec![0x00, 0x00, 0x00],
            // A link-answer length that is neither 0 nor 28.
            vec![0x00, 0x00, 0x00, 27],
            vec![0x00, 0x00, 0x00, 29],
            // 28 declared, fewer carried.
            [vec![0x00, 0x00, 0x00, 28], vec![9u8; 27]].concat(),
            // Declares 10 profile bytes, carries 3.
            vec![0x00, 0x0A, 1, 2, 3],
            // A raw, unframed intro: "le" reads as a 27749-byte profile.
            b"legacy hi".to_vec(),
        ] {
            let req = signed_request_sealing(&plaintext);
            assert!(
                matches!(
                    open_envelope(&req, &bob),
                    Err(ContactError::InvalidEncoding(_))
                ),
                "{plaintext:?}"
            );
        }
    }

    fn link_answer() -> LinkAnswer {
        LinkAnswer {
            link_id: [0x1D; LINK_ID_LEN],
            link_secret: [0x5C; LINK_SECRET_LEN],
        }
    }

    /// ADR-0079 §2: the link answer rides sealed beside the profile and the
    /// reply grant, and comes back exactly.
    #[test]
    fn a_link_answer_round_trips_with_and_without_the_rest() {
        let bob = recipient_x25519();
        for (profile, grant, intro) in [
            (None, None, b"".as_slice()),
            (Some(alice_profile()), Some(reply_grant()), b"hi".as_slice()),
        ] {
            let wire = compose(
                &sender(),
                &sender_account_id(),
                &bob.public_key(),
                [0u8; PSEUDONYM_LEN],
                0,
                b"",
                profile.as_ref(),
                grant.as_ref(),
                Some(&link_answer()),
                intro,
            )
            .unwrap();
            let opened = open_envelope(&decode_and_verify(&wire).unwrap(), &bob).unwrap();
            assert_eq!(opened.link_answer, Some(link_answer()));
            assert_eq!(opened.profile, profile);
            assert_eq!(opened.reply_grant, grant);
            assert_eq!(opened.intro, intro);
        }
        // Framing: the length byte then the id then the secret.
        let framed = frame_sealed_plaintext(None, None, Some(&link_answer()), b"x").unwrap();
        let mut expected = vec![0, 0, 0, 28];
        expected.extend_from_slice(&[0x1D; LINK_ID_LEN]);
        expected.extend_from_slice(&[0x5C; LINK_SECRET_LEN]);
        expected.push(b'x');
        assert_eq!(framed, expected);
        assert_eq!(LINK_ANSWER_LEN, 28);
    }

    #[test]
    fn link_answer_matching_needs_the_id_and_the_secret() {
        let a = link_answer();
        assert!(a.matches(&[0x1D; LINK_ID_LEN], &[0x5C; LINK_SECRET_LEN]));
        assert!(!a.matches(&[0x1E; LINK_ID_LEN], &[0x5C; LINK_SECRET_LEN]));
        assert!(!a.matches(&[0x1D; LINK_ID_LEN], &[0x5D; LINK_SECRET_LEN]));
        let mut secret = [0x5C; LINK_SECRET_LEN];
        secret[LINK_SECRET_LEN - 1] ^= 1;
        assert!(!a.matches(&[0x1D; LINK_ID_LEN], &secret));
        // The secret never reaches Debug output.
        let shown = format!("{a:?}");
        assert!(!shown.contains("92") && !shown.contains("5c") && !shown.contains("5C"));
    }

    #[test]
    fn the_split_oracle_accepts_a_framed_link_answer() {
        let framed = frame_sealed_plaintext(
            Some(&alice_profile()),
            Some(&reply_grant()),
            Some(&link_answer()),
            b"intro",
        )
        .unwrap();
        assert_eq!(fuzz_split(&framed), Some(true));
    }
}
