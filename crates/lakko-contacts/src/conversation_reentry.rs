// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A device that fell behind a conversation asks to be brought back
//! ([ADR-0091] phase 2, section 3).
//!
//! ## Wire format (v1)
//!
//! ```text
//! CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1 ("lakko/conversation-reentry/v1\0")
//!   ‖ version(1)=0x01 ‖ group_id(32) ‖ epoch_left(8, BE) ‖ gap_from_ms(8, BE)
//!   ‖ kp_len(2, BE) ‖ key_package(kp_len)
//! ```
//!
//! - **`group_id`** is the conversation, **`epoch_left`** the `latest_epoch`
//!   the device read (the epoch the replacement commit must leave), and the
//!   KeyPackage `K` is the re-entry package minted by
//!   `lakko_mls::Identity::create_reentry_key_package` for that pair. The
//!   envelope's two fields repeat what `K`'s marker says so a re-admitter can
//!   route the request before validating `K`; the marker inside `K` is what
//!   every gate trusts.
//! - **`gap_from_ms`** is when the device first failed to read a row of the
//!   conversation (section 7: the device that brings it back sends the
//!   messages of the gap it holds, from this time to the replacement). It is
//!   the device's own claim and only bounds what the re-admitter sends.
//! - **`key_package`** is 1 to [`CONVERSATION_REENTRY_MAX_KEY_PACKAGE_LEN`]
//!   bytes, opaque here.
//!
//! The whole envelope is exact: no trailing bytes, the version checked before
//! any length.
//!
//! ## Routes
//!
//! 1. Another device of the account in the conversation: the envelope as a
//!    DeviceGroup service envelope, dispatched as
//!    [`crate::incoming_envelope::IncomingEnvelope::ConversationReentry`].
//! 2. A one-to-one: the envelope as the body of a sealed mailbox row of kind
//!    [`crate::mailbox::MailboxKind::ConversationReentry`].
//! 3. A group: the envelope sealed to the group's knock key
//!    ([`seal_reentry_knock`]). The row has the size and first bytes of a
//!    join request (ADR-0087 section 5): what tells the two apart is the
//!    associated data, so only a holder of the knock seed learns which it is;
//!    an admin tries [`crate::knock::open_knock`], then
//!    [`open_reentry_knock`].
//!
//! [ADR-0091]: ../../../docs/adr/0091-conversation-rejoin.md

use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey, SecretKey};
use thiserror::Error;

use crate::envelope;
use crate::knock::{
    is_knock_row_len, KNOCK_KEY_LEN, KNOCK_LARGE_ROW_LEN, KNOCK_ROW_OVERHEAD, KNOCK_ROW_VERSION,
    KNOCK_SMALL_ROW_LEN,
};

/// Domain separator. Frozen: the dispatcher matches on it.
pub const CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/conversation-reentry/v1\x00";

/// Schema-version byte carried first after the separator.
pub const CONVERSATION_REENTRY_VERSION_V1: u8 = 0x01;

/// Length of a group id.
pub const CONVERSATION_REENTRY_GROUP_ID_LEN: usize = 32;

/// Longest re-entry KeyPackage an envelope carries. Chosen, not measured:
/// the knock bound, far above a Lakko KeyPackage.
pub const CONVERSATION_REENTRY_MAX_KEY_PACKAGE_LEN: usize = crate::knock::KNOCK_MAX_KEY_PACKAGE_LEN;

/// Bytes after the separator before the KeyPackage.
const FIXED_PAYLOAD_LEN: usize = 1 + CONVERSATION_REENTRY_GROUP_ID_LEN + 8 + 8 + 2;

/// The shortest well-formed envelope (a one-byte KeyPackage).
pub const CONVERSATION_REENTRY_ENVELOPE_MIN_LEN: usize =
    CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + 1;

/// The longest well-formed envelope.
pub const CONVERSATION_REENTRY_ENVELOPE_MAX_LEN: usize = CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1
    .len()
    + FIXED_PAYLOAD_LEN
    + CONVERSATION_REENTRY_MAX_KEY_PACKAGE_LEN;

/// AAD domain of a re-entry knock row: distinct from a join row's, so a row
/// opens as exactly one of the two. Frozen.
pub const REENTRY_KNOCK_AAD_DOMAIN: &[u8] = b"lakko/group/reentry-knock-row/v1\x00";

/// A request to be brought back into a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationReentry {
    /// The conversation.
    pub group_id: [u8; CONVERSATION_REENTRY_GROUP_ID_LEN],
    /// The `latest_epoch` the device read.
    pub epoch_left: u64,
    /// When the device first failed to read a row of the conversation.
    pub gap_from_ms: u64,
    /// The re-entry KeyPackage wire, opaque here.
    pub key_package: Vec<u8>,
}

/// Why a re-entry envelope or row was refused. A reason code; nothing from
/// the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConversationReentryError {
    /// No domain separator. Only from [`decode_conversation_reentry`].
    #[error("plaintext does not carry the conversation re-entry domain separator")]
    WrongDomain,
    /// The payload is shorter or longer than its fields say.
    #[error("conversation re-entry envelope has the wrong length")]
    BadLength,
    /// The version byte is not [`CONVERSATION_REENTRY_VERSION_V1`].
    #[error("conversation re-entry envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The KeyPackage is empty or longer than its bound.
    #[error("conversation re-entry key package has a bad length")]
    BadKeyPackage,
    /// A knock row of another shape (version, size).
    #[error("re-entry knock row has the wrong shape")]
    BadRow,
    /// The knock row did not open (another group's key, a join request, a
    /// tampered row).
    #[error("re-entry knock row did not open")]
    NotOpened,
    /// The knock public key is not usable.
    #[error("knock public key is not usable")]
    BadKey,
    /// The opened row's padding is not zero, or its length prefix overruns.
    #[error("re-entry knock plaintext is malformed")]
    BadFraming,
}

impl ConversationReentryError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadKeyPackage => "badKeyPackage",
            Self::BadRow => "badRow",
            Self::NotOpened => "notOpened",
            Self::BadKey => "badKey",
            Self::BadFraming => "badFraming",
        }
    }
}

/// Compose the envelope.
///
/// # Errors
///
/// [`ConversationReentryError::BadKeyPackage`] for an empty KeyPackage or one
/// above the bound: compose refuses exactly what decode refuses.
pub fn compose_conversation_reentry(
    request: &ConversationReentry,
) -> CoreResult<Vec<u8>, ConversationReentryError> {
    let kp_len = request.key_package.len();
    if kp_len == 0 || kp_len > CONVERSATION_REENTRY_MAX_KEY_PACKAGE_LEN {
        return Err(ConversationReentryError::BadKeyPackage);
    }
    let kp_len16 = u16::try_from(kp_len).map_err(|_| ConversationReentryError::BadKeyPackage)?;
    let mut out = Vec::with_capacity(
        CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + kp_len,
    );
    out.extend_from_slice(CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1);
    out.push(CONVERSATION_REENTRY_VERSION_V1);
    out.extend_from_slice(&request.group_id);
    out.extend_from_slice(&request.epoch_left.to_be_bytes());
    out.extend_from_slice(&request.gap_from_ms.to_be_bytes());
    out.extend_from_slice(&kp_len16.to_be_bytes());
    out.extend_from_slice(&request.key_package);
    Ok(out)
}

fn read_u64(bytes: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(bytes);
    u64::from_be_bytes(b)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`ConversationReentryError::UnknownVersion`] (checked first),
/// [`ConversationReentryError::BadLength`] and
/// [`ConversationReentryError::BadKeyPackage`].
pub fn try_unwrap_conversation_reentry(
    plaintext: &[u8],
) -> CoreResult<Option<ConversationReentry>, ConversationReentryError> {
    let Some(payload) = plaintext.strip_prefix(CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, _) = payload
        .split_first()
        .ok_or(ConversationReentryError::BadLength)?;
    if version != CONVERSATION_REENTRY_VERSION_V1 {
        return Err(ConversationReentryError::UnknownVersion(version));
    }
    if payload.len() < FIXED_PAYLOAD_LEN {
        return Err(ConversationReentryError::BadLength);
    }
    let (fixed, key_package) = payload.split_at(FIXED_PAYLOAD_LEN);
    let mut group_id = [0u8; CONVERSATION_REENTRY_GROUP_ID_LEN];
    group_id.copy_from_slice(&fixed[1..=CONVERSATION_REENTRY_GROUP_ID_LEN]);
    let at = 1 + CONVERSATION_REENTRY_GROUP_ID_LEN;
    let epoch_left = read_u64(&fixed[at..at + 8]);
    let gap_from_ms = read_u64(&fixed[at + 8..at + 16]);
    let kp_len = usize::from(u16::from_be_bytes([fixed[at + 16], fixed[at + 17]]));
    if kp_len == 0 || kp_len > CONVERSATION_REENTRY_MAX_KEY_PACKAGE_LEN {
        return Err(ConversationReentryError::BadKeyPackage);
    }
    if key_package.len() != kp_len {
        return Err(ConversationReentryError::BadLength);
    }
    Ok(Some(ConversationReentry {
        group_id,
        epoch_left,
        gap_from_ms,
        key_package: key_package.to_vec(),
    }))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`ConversationReentryError::WrongDomain`] and every error of
/// [`try_unwrap_conversation_reentry`].
pub fn decode_conversation_reentry(
    bytes: &[u8],
) -> CoreResult<ConversationReentry, ConversationReentryError> {
    try_unwrap_conversation_reentry(bytes)?.ok_or(ConversationReentryError::WrongDomain)
}

fn aad(version: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(REENTRY_KNOCK_AAD_DOMAIN.len() + 3);
    out.extend_from_slice(REENTRY_KNOCK_AAD_DOMAIN);
    out.push(KNOCK_ROW_VERSION);
    out.extend_from_slice(&version.to_be_bytes());
    out
}

/// Seal a re-entry envelope (as composed by [`compose_conversation_reentry`])
/// as a knock row for the group whose knock key of version `version` is
/// `knock_pk` (route 3). The row is padded to the same two sizes as a join
/// request, with the same first three bytes.
///
/// # Errors
///
/// [`ConversationReentryError::BadLength`] when `envelope` is not a
/// well-formed re-entry envelope, [`ConversationReentryError::BadKey`],
/// [`ConversationReentryError::NotOpened`] (the AEAD refused; not expected).
pub fn seal_reentry_knock(
    knock_pk: &[u8; KNOCK_KEY_LEN],
    version: u16,
    envelope_bytes: &[u8],
) -> CoreResult<Vec<u8>, ConversationReentryError> {
    decode_conversation_reentry(envelope_bytes)?;
    let len16 =
        u16::try_from(envelope_bytes.len()).map_err(|_| ConversationReentryError::BadLength)?;
    let mut plaintext = Vec::with_capacity(KNOCK_LARGE_ROW_LEN);
    plaintext.extend_from_slice(&len16.to_be_bytes());
    plaintext.extend_from_slice(envelope_bytes);
    let bucket = [KNOCK_SMALL_ROW_LEN, KNOCK_LARGE_ROW_LEN]
        .into_iter()
        .map(|row| row - KNOCK_ROW_OVERHEAD)
        .find(|room| plaintext.len() <= *room)
        .ok_or(ConversationReentryError::BadLength)?;
    plaintext.resize(bucket, 0);
    let recipient =
        PublicKey::from_bytes(knock_pk).map_err(|_| ConversationReentryError::BadKey)?;
    let sealed = envelope::seal_labeled(
        &crate::knock::SEAL_LABELS,
        &recipient,
        &aad(version),
        &plaintext,
    )
    .map_err(|_| ConversationReentryError::NotOpened)?;
    plaintext.fill(0);
    let mut row = Vec::with_capacity(3 + sealed.len());
    row.push(KNOCK_ROW_VERSION);
    row.extend_from_slice(&version.to_be_bytes());
    row.extend_from_slice(&sealed);
    Ok(row)
}

/// Open a re-entry knock row with the knock seed of the version it names
/// (`crate::knock::knock_row_version`). A join request does not open here,
/// and a re-entry row does not open as a join request.
///
/// # Errors
///
/// [`ConversationReentryError::BadRow`], [`ConversationReentryError::NotOpened`],
/// [`ConversationReentryError::BadFraming`], and the envelope's own errors.
pub fn open_reentry_knock(
    seed: &[u8; KNOCK_KEY_LEN],
    row: &[u8],
) -> CoreResult<ConversationReentry, ConversationReentryError> {
    let (&first, rest) = row.split_first().ok_or(ConversationReentryError::BadRow)?;
    if first != KNOCK_ROW_VERSION || !is_knock_row_len(row.len()) {
        return Err(ConversationReentryError::BadRow);
    }
    let (version, sealed) = rest.split_at(2);
    let version = u16::from_be_bytes([version[0], version[1]]);
    let mut plaintext = envelope::open_labeled(
        &crate::knock::SEAL_LABELS,
        &SecretKey::from_bytes(*seed),
        &aad(version),
        sealed,
    )
    .map_err(|_| ConversationReentryError::NotOpened)?;
    let parsed = parse_knock_plaintext(&plaintext);
    plaintext.fill(0);
    parsed
}

/// Parse an opened re-entry knock plaintext: `len(2) ‖ envelope ‖ zeros`.
fn parse_knock_plaintext(
    plaintext: &[u8],
) -> CoreResult<ConversationReentry, ConversationReentryError> {
    if plaintext.len() < 2 {
        return Err(ConversationReentryError::BadFraming);
    }
    let (len, rest) = plaintext.split_at(2);
    let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
    if len > rest.len() {
        return Err(ConversationReentryError::BadFraming);
    }
    let (envelope_bytes, padding) = rest.split_at(len);
    if padding.iter().any(|b| *b != 0) {
        return Err(ConversationReentryError::BadFraming);
    }
    decode_conversation_reentry(envelope_bytes)
}

/// The fuzz oracle behind `fuzz_api::conversation_reentry_decode`: `None`
/// when the decoder refuses; `Some(true)` when what decoded re-composes to
/// the same bytes within the advertised bounds, and the knock-plaintext
/// parser fed the same bytes never panics.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let _ = parse_knock_plaintext(bytes);
    let request = decode_conversation_reentry(bytes).ok()?;
    let recomposed = compose_conversation_reentry(&request).ok()?;
    Some(
        recomposed == bytes
            && (CONVERSATION_REENTRY_ENVELOPE_MIN_LEN..=CONVERSATION_REENTRY_ENVELOPE_MAX_LEN)
                .contains(&bytes.len()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample(kp_len: usize) -> ConversationReentry {
        ConversationReentry {
            group_id: [0xC1; 32],
            epoch_left: 16,
            gap_from_ms: 1_763_000_000_000,
            key_package: vec![0x5A; kp_len],
        }
    }

    #[test]
    fn compose_decode_round_trips_within_bounds() {
        for len in [1, 400, CONVERSATION_REENTRY_MAX_KEY_PACKAGE_LEN] {
            let r = sample(len);
            let bytes = compose_conversation_reentry(&r).expect("compose");
            assert_eq!(decode_conversation_reentry(&bytes), Ok(r));
            assert_eq!(fuzz_decode(&bytes), Some(true));
        }
        assert_eq!(
            CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + 1,
            CONVERSATION_REENTRY_ENVELOPE_MIN_LEN
        );
        assert_eq!(CONVERSATION_REENTRY_ENVELOPE_MIN_LEN, 82);
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        assert_eq!(
            compose_conversation_reentry(&sample(0)),
            Err(ConversationReentryError::BadKeyPackage)
        );
        assert_eq!(
            compose_conversation_reentry(&sample(CONVERSATION_REENTRY_MAX_KEY_PACKAGE_LEN + 1)),
            Err(ConversationReentryError::BadKeyPackage)
        );
    }

    #[test]
    fn version_and_lengths_are_checked() {
        let good = compose_conversation_reentry(&sample(10)).expect("compose");
        let domain = CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1.len();
        let mut v2 = good.clone();
        v2[domain] = 2;
        assert_eq!(
            try_unwrap_conversation_reentry(&v2),
            Err(ConversationReentryError::UnknownVersion(2))
        );
        assert_eq!(
            try_unwrap_conversation_reentry(&good[..good.len() - 1]),
            Err(ConversationReentryError::BadLength)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(
            try_unwrap_conversation_reentry(&long),
            Err(ConversationReentryError::BadLength)
        );
        assert_eq!(
            try_unwrap_conversation_reentry(CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1),
            Err(ConversationReentryError::BadLength)
        );
        assert_eq!(try_unwrap_conversation_reentry(b"hello"), Ok(None));
        assert_eq!(
            decode_conversation_reentry(b"hello"),
            Err(ConversationReentryError::WrongDomain)
        );
    }

    #[test]
    fn a_knock_row_opens_as_reentry_and_never_as_a_join_request() {
        let seed = crate::knock::knock_seed(&[7u8; 32], 3);
        let pk = crate::knock::knock_public_key(&seed);
        let envelope_bytes = compose_conversation_reentry(&sample(500)).expect("compose");
        let row = seal_reentry_knock(&pk, 3, &envelope_bytes).expect("seal");
        assert_eq!(
            row.len(),
            KNOCK_SMALL_ROW_LEN,
            "same size as a small join request"
        );
        assert_eq!(crate::knock::knock_row_version(&row), Some(3));
        assert_eq!(open_reentry_knock(&seed, &row), Ok(sample(500)));
        assert_eq!(
            crate::knock::open_knock(&seed, &row),
            Err(crate::knock::KnockError::NotOpened),
            "a join-request opener cannot read it",
        );
        let other = crate::knock::knock_seed(&[8u8; 32], 3);
        assert_eq!(
            open_reentry_knock(&other, &row),
            Err(ConversationReentryError::NotOpened)
        );
        let big = compose_conversation_reentry(&sample(CONVERSATION_REENTRY_MAX_KEY_PACKAGE_LEN))
            .expect("compose");
        assert_eq!(
            seal_reentry_knock(&pk, 3, &big).expect("seal").len(),
            KNOCK_LARGE_ROW_LEN
        );
    }

    #[test]
    fn the_dispatcher_classifies_it_and_never_renders_it() {
        let bytes = compose_conversation_reentry(&sample(64)).expect("compose");
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::ConversationReentry(r)) if r == sample(64)
        ));
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let mut framed = CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1.to_vec();
            framed.extend_from_slice(&bytes);
            if let Some(ok) = fuzz_decode(&framed) {
                prop_assert!(ok);
            }
            let _ = fuzz_decode(&bytes);
        }

        #[test]
        fn any_request_round_trips(
            group_id in any::<[u8; 32]>(),
            epoch_left in any::<u64>(),
            gap_from_ms in any::<u64>(),
            kp in proptest::collection::vec(any::<u8>(), 1..600),
        ) {
            let r = ConversationReentry { group_id, epoch_left, gap_from_ms, key_package: kp };
            let bytes = compose_conversation_reentry(&r).expect("compose");
            prop_assert_eq!(decode_conversation_reentry(&bytes), Ok(r));
        }
    }
}
