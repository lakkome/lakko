// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Link-founded note — ADR-0079 (ordering of 2026-10-17).
//!
//! The device that won a request sent through one of the account's invite
//! links founds the one-to-one locally, then — BEFORE it delivers any
//! Welcome to anyone — posts this note into the account's own DeviceGroup:
//! the request it answers, the founded group's id and the founding Welcome.
//! Only after the note's publish succeeded does it write the Welcome to the
//! account's own mailbox and then to the requester's.
//!
//! Every device of the account keeps the earliest note per request (journal
//! order, from a sender leaf the DeviceGroup roster resolves at receipt,
//! C-68). Before ANY device builds for a request, a held note means it never
//! founds: it writes the note's Welcome to the requester's mailbox with the
//! request's reply grant (the Welcome is byte-identical for every added leaf,
//! and the requester ignores a duplicate) and records the request answered.
//! So whether or not the device taking over is a member of the founded group,
//! and whether or not it ever joined it, the request ends in the one
//! one-to-one the note names.
//!
//! ## Wire format
//!
//! ```text
//! domain(22) "lakko/link-founded/v1\0" ‖ version(1) = 0x01
//!   ‖ link_id(16) ‖ requester_account_key(32) ‖ group_id(32)
//!   ‖ welcome_len(2, big-endian) ‖ welcome(welcome_len)
//! ```
//!
//! `welcome_len` is 1 to [`LINK_FOUNDED_MAX_WELCOME_LEN`]: a bound chosen
//! with headroom over a MEASURED Welcome. A one-to-one founded with the most
//! devices an account may hold on each side (ADR-0050:
//! `MAX_LEAVES_PER_ACCOUNT` = 8, so the founder plus 7 own devices and 8 of
//! the requester's, 15 added leaves) produced a 7 846-byte Welcome; the test
//! `a_maximal_one_to_one_welcome_fits_the_note` in `lakko-ffi-uniffi` builds
//! that group and pins the measured size under the bound. The whole note
//! stays far below the DeviceGroup carrier's 384 KiB.
//!
//! The note carries ciphertext the Delivery Service already holds (the
//! Welcome) and public ids; it is sealed again by the DeviceGroup.
//!
//! ## Cross-kind isolation
//!
//! The separator must be byte-distinct from, and not a prefix of or prefixed
//! by, every sibling envelope domain in the crate;
//! [`tests::separator_distinct_from_sibling_domains`] asserts it.

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::invite::LINK_ID_LEN;
use crate::link_claim::{LINK_CLAIM_REQUESTER_KEY_LEN, LINK_CLAIM_REQUEST_KEY_LEN};

/// 22-byte domain separator. Frozen: the clients route a DeviceGroup service
/// envelope on this exact prefix; the in-payload version byte is the knob
/// that moves.
pub const LINK_FOUNDED_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/link-founded/v1\x00";

/// Schema-version byte carried first after the separator.
pub const LINK_FOUNDED_VERSION_V1: u8 = 0x01;

/// Length of a conversation group id.
pub const LINK_FOUNDED_GROUP_ID_LEN: usize = 32;

/// Longest Welcome a note carries (32 KiB). Chosen, about four times a
/// measured maximal one-to-one Welcome (7 846 bytes, see the module doc).
pub const LINK_FOUNDED_MAX_WELCOME_LEN: usize = 32 * 1024;

/// Bytes after the separator before the Welcome: version, request key,
/// group id, Welcome length.
const FIXED_PAYLOAD_LEN: usize = 1 + LINK_CLAIM_REQUEST_KEY_LEN + LINK_FOUNDED_GROUP_ID_LEN + 2;

/// Shortest well-formed note (a one-byte Welcome).
pub const LINK_FOUNDED_ENVELOPE_MIN_LEN: usize =
    LINK_FOUNDED_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + 1;

/// Longest well-formed note.
pub const LINK_FOUNDED_ENVELOPE_MAX_LEN: usize =
    LINK_FOUNDED_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + LINK_FOUNDED_MAX_WELCOME_LEN;

/// One note: the request, the one-to-one founded for it, and its Welcome.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct LinkFounded {
    /// The id of the invite link the request answered.
    pub link_id: [u8; LINK_ID_LEN],
    /// The requester's account key.
    pub requester_account_key: [u8; LINK_CLAIM_REQUESTER_KEY_LEN],
    /// The founded one-to-one's group id.
    pub group_id: [u8; LINK_FOUNDED_GROUP_ID_LEN],
    /// The founding Welcome (wire bytes), 1 to
    /// [`LINK_FOUNDED_MAX_WELCOME_LEN`] bytes.
    pub welcome: Vec<u8>,
}

impl core::fmt::Debug for LinkFounded {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LinkFounded")
            .field("welcome_len", &self.welcome.len())
            .finish_non_exhaustive()
    }
}

impl LinkFounded {
    /// The request key `link_id(16) ‖ requester_account_key(32)`, the name
    /// the claim bookkeeping uses too (`LinkClaim::request_key`).
    #[must_use]
    pub fn request_key(&self) -> [u8; LINK_CLAIM_REQUEST_KEY_LEN] {
        let mut out = [0u8; LINK_CLAIM_REQUEST_KEY_LEN];
        out[..LINK_ID_LEN].copy_from_slice(&self.link_id);
        out[LINK_ID_LEN..].copy_from_slice(&self.requester_account_key);
        out
    }
}

/// Compose a note: the plaintext handed to the DeviceGroup's
/// `ServiceEnvelope` carrier. [`decode_link_founded`] accepts exactly what
/// this produces.
///
/// # Errors
///
/// [`LinkFoundedError::WelcomeLength`] when the Welcome is empty or longer
/// than [`LINK_FOUNDED_MAX_WELCOME_LEN`] (what the decoder refuses).
pub fn compose_link_founded(note: &LinkFounded) -> CoreResult<Vec<u8>, LinkFoundedError> {
    let len = note.welcome.len();
    if len == 0 || len > LINK_FOUNDED_MAX_WELCOME_LEN {
        return Err(LinkFoundedError::WelcomeLength { len });
    }
    let len16 = u16::try_from(len).map_err(|_| LinkFoundedError::WelcomeLength { len })?;
    let mut out =
        Vec::with_capacity(LINK_FOUNDED_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + len);
    out.extend_from_slice(LINK_FOUNDED_ENVELOPE_DOMAIN_V1);
    out.push(LINK_FOUNDED_VERSION_V1);
    out.extend_from_slice(&note.link_id);
    out.extend_from_slice(&note.requester_account_key);
    out.extend_from_slice(&note.group_id);
    out.extend_from_slice(&len16.to_be_bytes());
    out.extend_from_slice(&note.welcome);
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(note))` — the separator matched and the note is well-formed.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the payload is malformed; dropped.
///
/// # Errors
///
/// * [`LinkFoundedError::UnknownVersion`] — checked before any length.
/// * [`LinkFoundedError::BadLength`] — the payload is shorter than its fixed
///   part, or the Welcome does not fill exactly the rest.
/// * [`LinkFoundedError::WelcomeLength`] — the declared Welcome length is 0
///   or above [`LINK_FOUNDED_MAX_WELCOME_LEN`].
pub fn try_unwrap_link_founded(
    plaintext: &[u8],
) -> CoreResult<Option<LinkFounded>, LinkFoundedError> {
    let Some(payload) = plaintext.strip_prefix(LINK_FOUNDED_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let bad = || LinkFoundedError::BadLength {
        actual: payload.len(),
    };
    let (&version, rest) = payload.split_first().ok_or_else(bad)?;
    if version != LINK_FOUNDED_VERSION_V1 {
        return Err(LinkFoundedError::UnknownVersion(version));
    }
    if rest.len() < FIXED_PAYLOAD_LEN - 1 {
        return Err(bad());
    }
    let (link_id, rest) = rest.split_at(LINK_ID_LEN);
    let (requester, rest) = rest.split_at(LINK_CLAIM_REQUESTER_KEY_LEN);
    let (group_id, rest) = rest.split_at(LINK_FOUNDED_GROUP_ID_LEN);
    let (len_bytes, welcome) = rest.split_at(2);
    let declared = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
    if declared == 0 || declared > LINK_FOUNDED_MAX_WELCOME_LEN {
        return Err(LinkFoundedError::WelcomeLength { len: declared });
    }
    if welcome.len() != declared {
        return Err(bad());
    }
    Ok(Some(LinkFounded {
        link_id: link_id.try_into().map_err(|_| bad())?,
        requester_account_key: requester.try_into().map_err(|_| bad())?,
        group_id: group_id.try_into().map_err(|_| bad())?,
        welcome: welcome.to_vec(),
    }))
}

/// Strict standalone decoder: a non-note plaintext is a hard error.
///
/// # Errors
///
/// * [`LinkFoundedError::WrongDomain`] — `bytes` does not start with
///   [`LINK_FOUNDED_ENVELOPE_DOMAIN_V1`].
/// * Every error [`try_unwrap_link_founded`] returns.
pub fn decode_link_founded(bytes: &[u8]) -> CoreResult<LinkFounded, LinkFoundedError> {
    try_unwrap_link_founded(bytes)?.ok_or(LinkFoundedError::WrongDomain)
}

/// Why a note was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LinkFoundedError {
    /// The plaintext does not carry the note's domain separator. Only from
    /// [`decode_link_founded`].
    #[error("plaintext does not carry the link-founded domain separator")]
    WrongDomain,
    /// The payload does not have the layout's length.
    #[error("link-founded envelope has {actual} payload bytes, which is not its layout")]
    BadLength {
        /// Payload bytes after the separator.
        actual: usize,
    },
    /// The Welcome length is 0 or above [`LINK_FOUNDED_MAX_WELCOME_LEN`].
    #[error("link-founded Welcome of {len} bytes is outside 1..={LINK_FOUNDED_MAX_WELCOME_LEN}")]
    WelcomeLength {
        /// The declared (or given) Welcome length.
        len: usize,
    },
    /// The version byte is not [`LINK_FOUNDED_VERSION_V1`].
    #[error("link-founded envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

impl LinkFoundedError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength { .. } => "badLength",
            Self::WelcomeLength { .. } => "welcomeLength",
            Self::UnknownVersion(_) => "unknownVersion",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn note(welcome: Vec<u8>) -> LinkFounded {
        LinkFounded {
            link_id: [1; 16],
            requester_account_key: [2; 32],
            group_id: [3; 32],
            welcome,
        }
    }

    #[test]
    fn domain_separator_is_frozen() {
        assert_eq!(
            LINK_FOUNDED_ENVELOPE_DOMAIN_V1,
            b"lakko/link-founded/v1\x00"
        );
        assert_eq!(LINK_FOUNDED_ENVELOPE_DOMAIN_V1.len(), 22);
        assert_eq!(LINK_FOUNDED_ENVELOPE_MIN_LEN, 22 + 1 + 16 + 32 + 32 + 2 + 1);
        assert_eq!(
            LINK_FOUNDED_ENVELOPE_MAX_LEN,
            22 + 1 + 16 + 32 + 32 + 2 + 32 * 1024
        );
    }

    #[test]
    fn compose_then_decode_round_trips() {
        let n = note(vec![9; 7846]);
        let wire = compose_link_founded(&n).unwrap();
        assert_eq!(decode_link_founded(&wire).unwrap(), n);
        assert_eq!(n.request_key()[..16], [1; 16]);
        assert_eq!(n.request_key()[16..], [2; 32]);
        let max = note(vec![7; LINK_FOUNDED_MAX_WELCOME_LEN]);
        let wire = compose_link_founded(&max).unwrap();
        assert_eq!(wire.len(), LINK_FOUNDED_ENVELOPE_MAX_LEN);
        assert_eq!(decode_link_founded(&wire).unwrap(), max);
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        assert_eq!(
            compose_link_founded(&note(Vec::new())),
            Err(LinkFoundedError::WelcomeLength { len: 0 })
        );
        assert_eq!(
            compose_link_founded(&note(vec![0; LINK_FOUNDED_MAX_WELCOME_LEN + 1])),
            Err(LinkFoundedError::WelcomeLength {
                len: LINK_FOUNDED_MAX_WELCOME_LEN + 1
            })
        );
    }

    #[test]
    fn a_foreign_plaintext_falls_through_and_decode_refuses_it() {
        assert_eq!(try_unwrap_link_founded(b"hello"), Ok(None));
        assert_eq!(
            decode_link_founded(b"hello"),
            Err(LinkFoundedError::WrongDomain)
        );
        let claim = crate::link_claim::compose_link_claim(&crate::link_claim::LinkClaim {
            link_id: [1; 16],
            requester_account_key: [2; 32],
        });
        assert_eq!(try_unwrap_link_founded(&claim), Ok(None));
    }

    #[test]
    fn a_bare_separator_is_an_error_not_a_fall_through() {
        assert!(matches!(
            try_unwrap_link_founded(LINK_FOUNDED_ENVELOPE_DOMAIN_V1),
            Err(LinkFoundedError::BadLength { actual: 0 })
        ));
    }

    #[test]
    fn version_is_checked_before_length() {
        let mut wire = compose_link_founded(&note(vec![5; 10])).unwrap();
        wire[22] = 0x02;
        wire.truncate(30);
        assert_eq!(
            try_unwrap_link_founded(&wire),
            Err(LinkFoundedError::UnknownVersion(0x02))
        );
    }

    #[test]
    fn a_wrong_welcome_length_is_refused() {
        let wire = compose_link_founded(&note(vec![5; 10])).unwrap();
        // One byte short and one byte long.
        assert!(matches!(
            try_unwrap_link_founded(&wire[..wire.len() - 1]),
            Err(LinkFoundedError::BadLength { .. })
        ));
        let mut long = wire.clone();
        long.push(0);
        assert!(matches!(
            try_unwrap_link_founded(&long),
            Err(LinkFoundedError::BadLength { .. })
        ));
        // A declared length of 0, and one above the bound.
        let at = LINK_FOUNDED_ENVELOPE_DOMAIN_V1.len() + 1 + 16 + 32 + 32;
        let mut zero = wire.clone();
        zero[at] = 0;
        zero[at + 1] = 0;
        assert_eq!(
            try_unwrap_link_founded(&zero),
            Err(LinkFoundedError::WelcomeLength { len: 0 })
        );
        let mut over = wire;
        over[at] = 0x80;
        over[at + 1] = 0x01;
        assert_eq!(
            try_unwrap_link_founded(&over),
            Err(LinkFoundedError::WelcomeLength { len: 0x8001 })
        );
        // Shorter than the fixed part.
        let short = [LINK_FOUNDED_ENVELOPE_DOMAIN_V1, &[0x01, 0, 0][..]].concat();
        assert!(matches!(
            try_unwrap_link_founded(&short),
            Err(LinkFoundedError::BadLength { .. })
        ));
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::link_claim::LINK_CLAIM_ENVELOPE_DOMAIN_V1,
            crate::device_card::DEVICE_CARD_ENVELOPE_DOMAIN_V1,
            crate::key_rotation::KEY_ROTATION_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            b"lakko/sync/link-record/v1\0",
            b"lakko/sync/lineage/v1\0",
            b"lakko/sync/recovery-email/v1\0",
            b"lakko/sync/own-username/v1\0",
        ];
        for d in other_domains {
            assert_ne!(*d, LINK_FOUNDED_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(LINK_FOUNDED_ENVELOPE_DOMAIN_V1)
                    && !LINK_FOUNDED_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    proptest! {
        /// Decode is total: any input either decodes to a note that
        /// re-composes to exactly the input, or is refused; never a panic.
        #[test]
        fn decode_is_total_and_canonical(bytes in prop::collection::vec(any::<u8>(), 0..400)) {
            if let Ok(n) = decode_link_founded(&bytes) {
                prop_assert_eq!(compose_link_founded(&n).unwrap(), bytes);
            }
        }

        /// Every encodable note round-trips.
        #[test]
        fn every_encodable_note_round_trips(
            link_id in any::<[u8; 16]>(),
            key in any::<[u8; 32]>(),
            group in any::<[u8; 32]>(),
            welcome in prop::collection::vec(any::<u8>(), 1..2048),
        ) {
            let n = LinkFounded { link_id, requester_account_key: key, group_id: group, welcome };
            let wire = compose_link_founded(&n).unwrap();
            prop_assert!(wire.len() >= LINK_FOUNDED_ENVELOPE_MIN_LEN);
            prop_assert!(wire.len() <= LINK_FOUNDED_ENVELOPE_MAX_LEN);
            prop_assert_eq!(decode_link_founded(&wire).unwrap(), n);
        }
    }
}
