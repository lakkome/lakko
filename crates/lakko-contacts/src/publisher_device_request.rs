// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A co-publisher's device asks an owner device to admit or retire one of its
//! own account's devices in the channel's PublisherGroup ([ADR-0090] section
//! 2.2, decision B: only an owner device commits).
//!
//! ## Why a carrier is needed
//!
//! Since ADR-0090 every leaf of a PublisherGroup is a device, and an owner
//! device adds and retires the devices of an account that already publishes.
//! For the owner's own account it reads the account's DeviceGroup roster. For
//! a co-publisher it cannot: only the co-publisher's own publishing device
//! sees that account's roster, and it cannot commit (the Delivery Service
//! registers no co-publisher epoch). So it asks, on the PublisherGroup
//! journal, with this envelope.
//!
//! ## Wire format (v1)
//!
//! ```text
//! PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1 ("lakko/publisher-device-request/v1\0")
//!   ‖ version(1)=0x01 ‖ channel_id(16) ‖ action(1)
//!   action 1, admit:  key_package_len(2 BE) ‖ key_package
//!                     ‖ mailbox_binding(97) ‖ delegation_count(1) ‖ delegations(count × 185)
//!   action 2, retire: device_key(32)
//! ```
//!
//! The admit form carries the requesting account's ADR-0078 mailbox binding
//! beside its ADR-0060 committer delegations (the ADR's sketch named the
//! delegations alone; a delegation names a mailbox, and the mailbox key is not
//! derivable from the account key, so an owner device can check the
//! delegations only against the signed binding, as for
//! [`crate::committer_delegation`]). `key_package_len` is 1 to
//! [`crate::call_invite::MAX_KEY_PACKAGE_LEN`]; `delegation_count` 0 to
//! [`crate::committer_delegation::MAX_COMMITTER_DELEGATIONS`]. Nothing may
//! trail; the version is checked before any length. Structural only: the
//! KeyPackage, the binding and the delegations are checked by the owner
//! device ([`crate::committer_delegation`]'s semantic half lives in
//! `lakko_ds_proof::committer`).
//!
//! ## The owner device's rule
//!
//! An owner device acts on a request only when it arrived on the
//! PublisherGroup journal of the channel `channel_id` names, the MLS sender
//! resolves at receipt (C-68) to a current publisher account, and:
//!
//! * admit: the KeyPackage's device binding names that same account, and the
//!   account stays within the leaf cap; the committer delegations are checked
//!   against the requester's account key and mailbox binding;
//! * retire: the named leaf belongs to that same account.
//!
//! A request from anyone else, for another channel, or naming another account
//! is dropped. A request not acted on within a delegation's 24 hours is sent
//! again by the requester's next pass.
//!
//! [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::call_invite::MAX_KEY_PACKAGE_LEN;
use crate::committer_delegation::{
    CommitterDelegationEnvelope, ADDED_ACCOUNT_KEY_LEN, COMMITTER_DELEGATION_BLOB_LEN,
    MAX_COMMITTER_DELEGATIONS,
};
use crate::mailbox::{MailboxBinding, MAILBOX_BINDING_LEN};

/// Domain separator prepended to the envelope. Frozen: the dispatcher
/// matches on it.
pub const PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1: &[u8] =
    b"lakko/publisher-device-request/v1\x00";

/// Schema-version byte carried first after the separator.
pub const PUBLISHER_DEVICE_REQUEST_VERSION_V1: u8 = 0x01;

/// Length of a channel id.
pub const PUBLISHER_DEVICE_REQUEST_CHANNEL_ID_LEN: usize = 16;

/// Length of a device (leaf signature) key.
pub const PUBLISHER_DEVICE_REQUEST_DEVICE_KEY_LEN: usize = 32;

const ACTION_ADMIT: u8 = 1;
const ACTION_RETIRE: u8 = 2;

/// Bytes after the separator before the action body: version, channel id,
/// action.
const HEADER_LEN: usize = 1 + PUBLISHER_DEVICE_REQUEST_CHANNEL_ID_LEN + 1;

/// The one length of a retire envelope.
pub const PUBLISHER_DEVICE_REQUEST_RETIRE_LEN: usize = PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1
    .len()
    + HEADER_LEN
    + PUBLISHER_DEVICE_REQUEST_DEVICE_KEY_LEN;

/// Shortest well-formed envelope (a retire is shorter than any admit).
pub const PUBLISHER_DEVICE_REQUEST_ENVELOPE_MIN_LEN: usize = PUBLISHER_DEVICE_REQUEST_RETIRE_LEN;

/// Longest well-formed envelope: an admit with the longest KeyPackage and
/// every delegation.
pub const PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN: usize =
    PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1.len()
        + HEADER_LEN
        + 2
        + MAX_KEY_PACKAGE_LEN
        + MAILBOX_BINDING_LEN
        + 1
        + MAX_COMMITTER_DELEGATIONS * COMMITTER_DELEGATION_BLOB_LEN;

/// What the request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublisherDeviceAction {
    /// Admit the device whose KeyPackage this is.
    Admit {
        /// The device's wire KeyPackage (1 to [`MAX_KEY_PACKAGE_LEN`] bytes).
        key_package: Vec<u8>,
        /// The requesting account's mailbox binding (ADR-0078).
        mailbox_binding: MailboxBinding,
        /// 0 to [`MAX_COMMITTER_DELEGATIONS`] committer delegations for that
        /// mailbox (ADR-0060), one per owner device that may admit.
        delegations: Vec<[u8; COMMITTER_DELEGATION_BLOB_LEN]>,
    },
    /// Retire the leaf whose signature key this is.
    Retire {
        /// The leaf's 32-byte signature key.
        device_key: [u8; PUBLISHER_DEVICE_REQUEST_DEVICE_KEY_LEN],
    },
}

/// A decoded publisher device request (structurally checked only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherDeviceRequest {
    /// The channel the request is for.
    pub channel_id: [u8; PUBLISHER_DEVICE_REQUEST_CHANNEL_ID_LEN],
    /// What it asks for.
    pub action: PublisherDeviceAction,
}

impl PublisherDeviceRequest {
    /// The committer-delegation envelope an owner device checks an admit's
    /// delegations with (`lakko_ds_proof::committer`), naming
    /// `requester_account_key` (the MLS sender resolved at receipt) as the
    /// added account. `None` for a retire.
    #[must_use]
    pub fn committer_delegations(
        &self,
        requester_account_key: &[u8; ADDED_ACCOUNT_KEY_LEN],
    ) -> Option<CommitterDelegationEnvelope> {
        match &self.action {
            PublisherDeviceAction::Admit {
                mailbox_binding,
                delegations,
                ..
            } => Some(CommitterDelegationEnvelope {
                added_account_key: *requester_account_key,
                added_mailbox_binding: *mailbox_binding,
                delegations: delegations.clone(),
            }),
            PublisherDeviceAction::Retire { .. } => None,
        }
    }
}

/// Why a publisher device request was refused. A reason code; nothing from
/// the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PublisherDeviceRequestError {
    /// No domain separator. Only from [`decode_publisher_device_request`].
    #[error("plaintext does not carry the publisher device-request domain separator")]
    WrongDomain,
    /// The version byte is not [`PUBLISHER_DEVICE_REQUEST_VERSION_V1`].
    #[error("publisher device request carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The action byte is neither admit nor retire.
    #[error("publisher device request carries an unknown action")]
    UnknownAction,
    /// A KeyPackage of no bytes or longer than [`MAX_KEY_PACKAGE_LEN`].
    #[error("publisher device request carries a KeyPackage of a length out of bounds")]
    BadKeyPackageLength,
    /// More than [`MAX_COMMITTER_DELEGATIONS`] delegations.
    #[error("publisher device request carries too many delegations")]
    BadCount,
    /// The mailbox binding's layout is malformed.
    #[error("publisher device request carries a malformed mailbox binding")]
    BadBinding,
    /// The bytes are not exactly the length the header declares.
    #[error("publisher device request has the wrong length")]
    BadLength,
}

impl PublisherDeviceRequestError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::UnknownAction => "unknownAction",
            Self::BadKeyPackageLength => "badKeyPackageLength",
            Self::BadCount => "badCount",
            Self::BadBinding => "badBinding",
            Self::BadLength => "badLength",
        }
    }
}

/// Compose the envelope a co-publisher's device sends on the channel's
/// PublisherGroup journal.
///
/// # Errors
///
/// [`PublisherDeviceRequestError::BadKeyPackageLength`] and
/// [`PublisherDeviceRequestError::BadCount`]: the bounds a receiver refuses,
/// so a conforming sender never emits what is dropped.
pub fn compose_publisher_device_request(
    request: &PublisherDeviceRequest,
) -> CoreResult<Vec<u8>, PublisherDeviceRequestError> {
    let mut out = Vec::new();
    out.extend_from_slice(PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1);
    out.push(PUBLISHER_DEVICE_REQUEST_VERSION_V1);
    out.extend_from_slice(&request.channel_id);
    match &request.action {
        PublisherDeviceAction::Admit {
            key_package,
            mailbox_binding,
            delegations,
        } => {
            if key_package.is_empty() || key_package.len() > MAX_KEY_PACKAGE_LEN {
                return Err(PublisherDeviceRequestError::BadKeyPackageLength);
            }
            if delegations.len() > MAX_COMMITTER_DELEGATIONS {
                return Err(PublisherDeviceRequestError::BadCount);
            }
            let kp_len = u16::try_from(key_package.len())
                .map_err(|_| PublisherDeviceRequestError::BadKeyPackageLength)?;
            let count = u8::try_from(delegations.len())
                .map_err(|_| PublisherDeviceRequestError::BadCount)?;
            out.push(ACTION_ADMIT);
            out.extend_from_slice(&kp_len.to_be_bytes());
            out.extend_from_slice(key_package);
            out.extend_from_slice(&mailbox_binding.encode());
            out.push(count);
            for blob in delegations {
                out.extend_from_slice(blob);
            }
        },
        PublisherDeviceAction::Retire { device_key } => {
            out.push(ACTION_RETIRE);
            out.extend_from_slice(device_key);
        },
    }
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// Every [`PublisherDeviceRequestError`] but `WrongDomain`; the version is
/// checked before any length.
pub fn try_unwrap_publisher_device_request(
    plaintext: &[u8],
) -> CoreResult<Option<PublisherDeviceRequest>, PublisherDeviceRequestError> {
    let Some(payload) = plaintext.strip_prefix(PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload
        .split_first()
        .ok_or(PublisherDeviceRequestError::BadLength)?;
    if version != PUBLISHER_DEVICE_REQUEST_VERSION_V1 {
        return Err(PublisherDeviceRequestError::UnknownVersion(version));
    }
    if rest.len() < PUBLISHER_DEVICE_REQUEST_CHANNEL_ID_LEN + 1 {
        return Err(PublisherDeviceRequestError::BadLength);
    }
    let (channel, rest) = rest.split_at(PUBLISHER_DEVICE_REQUEST_CHANNEL_ID_LEN);
    let mut channel_id = [0u8; PUBLISHER_DEVICE_REQUEST_CHANNEL_ID_LEN];
    channel_id.copy_from_slice(channel);
    let (&action, body) = rest
        .split_first()
        .ok_or(PublisherDeviceRequestError::BadLength)?;
    let action = match action {
        ACTION_RETIRE => {
            let device_key: [u8; PUBLISHER_DEVICE_REQUEST_DEVICE_KEY_LEN] = body
                .try_into()
                .map_err(|_| PublisherDeviceRequestError::BadLength)?;
            PublisherDeviceAction::Retire { device_key }
        },
        ACTION_ADMIT => take_admit(body)?,
        _ => return Err(PublisherDeviceRequestError::UnknownAction),
    };
    Ok(Some(PublisherDeviceRequest { channel_id, action }))
}

fn take_admit(body: &[u8]) -> CoreResult<PublisherDeviceAction, PublisherDeviceRequestError> {
    if body.len() < 2 {
        return Err(PublisherDeviceRequestError::BadLength);
    }
    let (len_bytes, rest) = body.split_at(2);
    let kp_len = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
    if kp_len == 0 || kp_len > MAX_KEY_PACKAGE_LEN {
        return Err(PublisherDeviceRequestError::BadKeyPackageLength);
    }
    if rest.len() < kp_len + MAILBOX_BINDING_LEN + 1 {
        return Err(PublisherDeviceRequestError::BadLength);
    }
    let (key_package, rest) = rest.split_at(kp_len);
    let (binding, rest) = rest.split_at(MAILBOX_BINDING_LEN);
    let mailbox_binding =
        MailboxBinding::decode(binding).map_err(|_| PublisherDeviceRequestError::BadBinding)?;
    let (&count, blobs) = rest
        .split_first()
        .ok_or(PublisherDeviceRequestError::BadLength)?;
    let count = usize::from(count);
    if count > MAX_COMMITTER_DELEGATIONS {
        return Err(PublisherDeviceRequestError::BadCount);
    }
    if blobs.len() != count * COMMITTER_DELEGATION_BLOB_LEN {
        return Err(PublisherDeviceRequestError::BadLength);
    }
    let delegations = blobs
        .chunks_exact(COMMITTER_DELEGATION_BLOB_LEN)
        .map(|chunk| {
            let mut blob = [0u8; COMMITTER_DELEGATION_BLOB_LEN];
            blob.copy_from_slice(chunk);
            blob
        })
        .collect();
    Ok(PublisherDeviceAction::Admit {
        key_package: key_package.to_vec(),
        mailbox_binding,
        delegations,
    })
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`PublisherDeviceRequestError::WrongDomain`] and every error of
/// [`try_unwrap_publisher_device_request`].
pub fn decode_publisher_device_request(
    bytes: &[u8],
) -> CoreResult<PublisherDeviceRequest, PublisherDeviceRequestError> {
    try_unwrap_publisher_device_request(bytes)?.ok_or(PublisherDeviceRequestError::WrongDomain)
}

/// The fuzz oracle behind `fuzz_api::publisher_device_request_decode`:
/// `None` when the decoder refuses; `Some(true)` when what decoded
/// re-composes to the same bytes within the advertised bounds.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let request = decode_publisher_device_request(bytes).ok()?;
    Some(
        compose_publisher_device_request(&request).as_deref() == Ok(bytes)
            && (PUBLISHER_DEVICE_REQUEST_ENVELOPE_MIN_LEN
                ..=PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN)
                .contains(&bytes.len()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const CHANNEL: [u8; 16] = [0x0C; 16];

    fn binding() -> MailboxBinding {
        let mut bytes = [0u8; MAILBOX_BINDING_LEN];
        bytes[0] = 0x01;
        bytes[1..33].copy_from_slice(&[0x4D; 32]);
        MailboxBinding::decode(&bytes).expect("layout")
    }

    fn admit(kp_len: usize, delegations: usize) -> PublisherDeviceRequest {
        PublisherDeviceRequest {
            channel_id: CHANNEL,
            action: PublisherDeviceAction::Admit {
                key_package: vec![0x4B; kp_len],
                mailbox_binding: binding(),
                delegations: vec![[0xDE; COMMITTER_DELEGATION_BLOB_LEN]; delegations],
            },
        }
    }

    fn retire() -> PublisherDeviceRequest {
        PublisherDeviceRequest {
            channel_id: CHANNEL,
            action: PublisherDeviceAction::Retire {
                device_key: [0x7E; 32],
            },
        }
    }

    #[test]
    fn the_layout_is_pinned() {
        assert_eq!(PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1.len(), 34);
        let r = compose_publisher_device_request(&retire()).expect("compose");
        assert_eq!(r.len(), PUBLISHER_DEVICE_REQUEST_RETIRE_LEN);
        assert_eq!(r.len(), 34 + 1 + 16 + 1 + 32);
        assert_eq!(r[34], 0x01);
        assert_eq!(&r[35..51], &CHANNEL);
        assert_eq!(r[51], ACTION_RETIRE);
        let a = compose_publisher_device_request(&admit(300, 2)).expect("compose");
        assert_eq!(a[51], ACTION_ADMIT);
        assert_eq!(&a[52..54], &300u16.to_be_bytes());
        assert_eq!(
            a.len(),
            34 + 18 + 2 + 300 + MAILBOX_BINDING_LEN + 1 + 2 * 185
        );
        assert_eq!(
            PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN,
            34 + 18 + 2 + 4096 + 97 + 1 + 16 * 185
        );
    }

    #[test]
    fn round_trips_and_dispatches() {
        for request in [retire(), admit(1, 0), admit(4096, 16)] {
            let bytes = compose_publisher_device_request(&request).expect("compose");
            assert_eq!(decode_publisher_device_request(&bytes), Ok(request.clone()));
            assert_eq!(fuzz_decode(&bytes), Some(true));
            assert!(matches!(
                crate::incoming_envelope::dispatch_incoming_envelope(&bytes),
                Ok(crate::incoming_envelope::IncomingEnvelope::PublisherDeviceRequest(r)) if r == request
            ));
        }
        let env = admit(10, 1)
            .committer_delegations(&[0x11; 32])
            .expect("admit carries delegations");
        assert_eq!(env.added_account_key, [0x11; 32]);
        assert_eq!(env.delegations.len(), 1);
        assert!(retire().committer_delegations(&[0x11; 32]).is_none());
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        assert_eq!(
            compose_publisher_device_request(&admit(0, 0)),
            Err(PublisherDeviceRequestError::BadKeyPackageLength)
        );
        assert_eq!(
            compose_publisher_device_request(&admit(4097, 0)),
            Err(PublisherDeviceRequestError::BadKeyPackageLength)
        );
        assert_eq!(
            compose_publisher_device_request(&admit(1, 17)),
            Err(PublisherDeviceRequestError::BadCount)
        );
    }

    #[test]
    fn every_malformed_shape_is_refused() {
        let good = compose_publisher_device_request(&admit(5, 1)).expect("compose");
        for cut in PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1.len()..good.len() {
            assert!(
                try_unwrap_publisher_device_request(&good[..cut]).is_err(),
                "{cut}"
            );
        }
        let mut long = good.clone();
        long.push(0);
        assert_eq!(
            try_unwrap_publisher_device_request(&long),
            Err(PublisherDeviceRequestError::BadLength)
        );
        let mut v2 = good.clone();
        v2[34] = 2;
        assert_eq!(
            try_unwrap_publisher_device_request(&v2),
            Err(PublisherDeviceRequestError::UnknownVersion(2))
        );
        let mut action = good.clone();
        action[51] = 3;
        assert_eq!(
            try_unwrap_publisher_device_request(&action),
            Err(PublisherDeviceRequestError::UnknownAction)
        );
        let mut zero_kp = good.clone();
        zero_kp[52] = 0;
        zero_kp[53] = 0;
        assert_eq!(
            try_unwrap_publisher_device_request(&zero_kp),
            Err(PublisherDeviceRequestError::BadKeyPackageLength)
        );
        let mut count = good.clone();
        count[54 + 5 + MAILBOX_BINDING_LEN] = 17;
        assert_eq!(
            try_unwrap_publisher_device_request(&count),
            Err(PublisherDeviceRequestError::BadCount)
        );
        let mut retire_long = compose_publisher_device_request(&retire()).expect("compose");
        retire_long.push(1);
        assert_eq!(
            try_unwrap_publisher_device_request(&retire_long),
            Err(PublisherDeviceRequestError::BadLength)
        );
        assert_eq!(try_unwrap_publisher_device_request(b"hello"), Ok(None));
        assert_eq!(
            decode_publisher_device_request(b"hello"),
            Err(PublisherDeviceRequestError::WrongDomain)
        );
    }

    /// Cross-kind isolation (ADR-0090): neither new domain is a prefix of any
    /// sibling envelope domain or the reverse, and each dispatches as itself,
    /// never as another kind and never as an intro message.
    #[test]
    fn separators_are_isolated_from_every_sibling_domain() {
        let ours: [&[u8]; 2] = [
            PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::channel_admission_mark::CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1,
        ];
        let siblings: &[&[u8]] = &[
            crate::admission_seed::ADMISSION_SEED_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::channel_epoch_seeds::CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1,
            crate::channel_read_key::CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1,
            crate::channel_rotate_request::CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::conversation_handle::CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1,
            crate::device_card::DEVICE_CARD_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::group_contact_request::GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::group_photo::GROUP_PHOTO_ENVELOPE_DOMAIN_V1,
            crate::group_reactions::GROUP_REACTIONS_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::key_rotation::KEY_ROTATION_ENVELOPE_DOMAIN_V1,
            crate::knock_seed::KNOCK_SEED_ENVELOPE_DOMAIN_V1,
            crate::link_claim::LINK_CLAIM_ENVELOPE_DOMAIN_V1,
            crate::link_founded::LINK_FOUNDED_ENVELOPE_DOMAIN_V1,
            crate::link_refusal::LINK_REFUSAL_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::rejoin_present::REJOIN_PRESENT_DOMAIN_V1,
            crate::shared_channels::SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            b"lakko/publisher-step-down/v1\x00".as_slice(),
        ];
        for d in ours {
            for s in siblings {
                assert!(!d.starts_with(s) && !s.starts_with(d), "{d:?} vs {s:?}");
            }
        }
        assert!(!ours[0].starts_with(ours[1]) && !ours[1].starts_with(ours[0]));
        let mark = crate::channel_admission_mark::compose_channel_admission_mark(
            &crate::channel_admission_mark::ChannelAdmissionMark {
                channel_id: CHANNEL,
                through_seq: 7,
            },
        );
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&mark),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelAdmissionMark(_))
        ));
        let request = compose_publisher_device_request(&retire()).expect("compose");
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&request),
            Ok(crate::incoming_envelope::IncomingEnvelope::PublisherDeviceRequest(_))
        ));
        let mut broken = PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1.to_vec();
        broken.push(1);
        assert!(crate::incoming_envelope::dispatch_incoming_envelope(&broken).is_err());
        let mut broken =
            crate::channel_admission_mark::CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1.to_vec();
        broken.push(1);
        assert!(crate::incoming_envelope::dispatch_incoming_envelope(&broken).is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn arbitrary_bytes_behind_the_domain_never_panic(tail in proptest::collection::vec(any::<u8>(), 0..800)) {
            let mut bytes = PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1.to_vec();
            bytes.extend_from_slice(&tail);
            if let Some(ok) = fuzz_decode(&bytes) {
                prop_assert!(ok);
            }
        }

        #[test]
        fn any_admit_round_trips(kp in proptest::collection::vec(any::<u8>(), 1..600), n in 0usize..4, channel in any::<[u8; 16]>()) {
            let request = PublisherDeviceRequest {
                channel_id: channel,
                action: PublisherDeviceAction::Admit {
                    key_package: kp,
                    mailbox_binding: binding(),
                    delegations: vec![[0x5A; COMMITTER_DELEGATION_BLOB_LEN]; n],
                },
            };
            let bytes = compose_publisher_device_request(&request).expect("compose");
            prop_assert_eq!(decode_publisher_device_request(&bytes), Ok(request));
        }
    }
}
