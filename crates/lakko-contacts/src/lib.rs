// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Contact-request envelopes and pseudonym lifecycle for Lakko.
//!
//! Phase 1.9a covers ADR-0005 scenario A — "I know `@username`,
//! add them as a contact" — minus the username discovery step
//! (handled by `services/discovery` + the OPRF round-trip in
//! `lakko-transport`).
//!
//! ## What's here (1.9a)
//!
//! - [`pseudonym`] — three pseudonym classes per ADR-0005 §13.7,
//!   each with a distinct derivation domain so leaks don't cross
//!   classes. `BLAKE3-keyed` for client-side variants;
//!   `BLAKE3(domain || pubkey)` for the public identity pseudonym.
//! - [`envelope`] — a sealed-box AEAD envelope (HPKE-Base-equivalent
//!   over X25519 + HKDF-SHA-256 + ChaCha20-Poly1305). Used to
//!   encrypt the optional intro message inside a ContactRequest to
//!   the recipient's identity public key.
//! - [`request`] — the [`ContactRequest`] wire type plus
//!   [`compose`] / [`decode_and_verify`] / [`open_envelope`].
//!   Manual length-prefixed binary codec, frozen at v1.
//!   Ed25519-signed by the sender's identity key.
//!
//! ## What's NOT here (1.9b and later)
//!
//! - QR safety numbers (scenario D) — Signal-style fingerprint
//!   verification.
//! - Group-invite envelope (scenario E).
//! - PoW + Privacy Pass tokens carried inside the request — the
//!   server side already exists in 1.1e; the wire-format extension
//!   waits for a real client integration sprint.
//! - `ContactAck` reply path (recipient → sender) — easy follow-up
//!   to add once we have UI to drive it.
//!
//! ## Threat-model notes (cross-link to ADR-0005)
//!
//! - Sealed envelopes provide **confidentiality** but not
//!   sender-authentication. The Ed25519 signature on the request
//!   itself is what proves "this came from `from_pubkey`".
//! - The `from_pseudonym` is fresh per recipient so the server
//!   cannot link two contact-requests Alice sends to different
//!   people. KT-log inclusion proofs over `from_pubkey` (handled
//!   in `lakko-transparency`) anchor the public key to a
//!   verifiable identity record.
//! - Server sees only `(stranger_inbox_pseudonym → wire bytes)`;
//!   it learns nothing about who Alice is or what Bob does with
//!   the request.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ack;
pub mod call_invite;
pub mod call_kp_forward;
pub mod call_welcome;
pub mod envelope;
pub mod error;
pub mod group_invite;
pub mod incoming_envelope;
pub mod invite;
pub mod pseudonym;
pub mod request;
pub mod safety;

pub use ack::{compose_ack, decode_and_verify_ack, ContactAck};
pub use call_invite::{
    call_revoke_canonical_bytes, call_sign_revocation, compose as compose_call_invite,
    decode_and_verify as decode_and_verify_call_invite, CallInviteCapability, CallInviteMedia,
    CALL_ID_LEN, CALL_INVITE_REVOKE_DOMAIN_V1, CALL_NEVER_EXPIRES, CALL_REVOKE_REPLAY_WINDOW_MS,
    CALL_SINGLE_USE, MAX_KEY_PACKAGE_LEN,
};
pub use call_kp_forward::{
    try_unwrap_call_kp_forward, wrap_call_kp_forward, CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
    CALL_KP_FORWARD_MIN_PAYLOAD_LEN,
};
pub use call_welcome::{
    try_unwrap_call_welcome, wrap_call_welcome, CallWelcomeError, CALL_WELCOME_ENVELOPE_DOMAIN_V1,
    CALL_WELCOME_MIN_PAYLOAD_LEN,
};
pub use error::{ContactError, Result};
pub use group_invite::{
    compose as compose_group_invite, decode_and_verify as decode_and_verify_group_invite,
    GroupInviteCapability, GROUP_INVITE_NEVER_EXPIRES, MAX_GROUP_ID_LEN,
};
pub use incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};
pub use invite::{
    compute_joiner_tag, owner_list_canonical_bytes, revoke_canonical_bytes, sign_owner_list,
    sign_revocation, InviteCapability, BLOB_ID_LEN, JOINER_TAG_LEN, JOINER_TAG_SALT_DOMAIN_V1,
    MAX_LABEL_LEN, NEVER_EXPIRES, OWNER_LIST_DOMAIN_V1, REVOKE_DOMAIN_V1, REVOKE_REPLAY_WINDOW_MS,
    SINGLE_USE,
};
pub use pseudonym::{Direction, IdentityPseudonym, PerRelationship, StrangerInbox, PSEUDONYM_LEN};
pub use request::{compose, decode_and_verify, open_envelope, ContactRequest, ACCOUNT_ID_LEN};
pub use safety::{
    safety_number, safety_number_bytes, SAFETY_NUMBER_DIGITS, SAFETY_NUMBER_DIGITS_PER_GROUP,
    SAFETY_NUMBER_GROUPS,
};
