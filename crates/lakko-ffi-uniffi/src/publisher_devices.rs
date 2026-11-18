// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0090 publisher bookkeeping envelopes on the UniFFI surface: a
//! co-publisher device's admit / retire request
//! (`lakko_contacts::publisher_device_request`) and a publishing device's
//! admission mark (`lakko_contacts::channel_admission_mark`). Both ride
//! `PublisherGroup.createApplicationMessage` on the channel's PublisherGroup
//! journal and arrive through `dispatchIncomingEnvelope` as the appended
//! kinds `PublisherDeviceRequest` and `ChannelAdmissionMark`.

use lakko_contacts::channel_admission_mark::{
    compose_channel_admission_mark as rust_compose_mark,
    decode_channel_admission_mark as rust_decode_mark, ChannelAdmissionMark,
    ChannelAdmissionMarkError, CHANNEL_ADMISSION_MARK_ENVELOPE_LEN,
};
use lakko_contacts::committer_delegation::COMMITTER_DELEGATION_BLOB_LEN;
use lakko_contacts::mailbox::MailboxBinding;
use lakko_contacts::publisher_device_request::{
    compose_publisher_device_request as rust_compose_request,
    decode_publisher_device_request as rust_decode_request, PublisherDeviceAction,
    PublisherDeviceRequest, PublisherDeviceRequestError, PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN,
    PUBLISHER_DEVICE_REQUEST_ENVELOPE_MIN_LEN,
};

use crate::scoped_discovery::FfiCommitterDelegationEnvelope;
use crate::FfiError;

fn request_error(e: PublisherDeviceRequestError) -> FfiError {
    FfiError::Contacts(format!("publisherDeviceRequest.{}: {e}", e.code()))
}

fn mark_error(e: ChannelAdmissionMarkError) -> FfiError {
    FfiError::Contacts(format!("channelAdmissionMark.{}: {e}", e.code()))
}

fn channel16(channel_id: &[u8], what: &str) -> Result<[u8; 16], FfiError> {
    channel_id
        .try_into()
        .map_err(|_| FfiError::Contacts(format!("{what}.badLength: channel id must be 16 bytes")))
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// What a co-publisher device asks an owner device to do (ADR-0090
/// section 2.2).
#[derive(uniffi::Enum, Debug, Clone, PartialEq, Eq)]
pub enum FfiPublisherDeviceAction {
    /// Admit the device whose wire KeyPackage this is. The requester's
    /// mailbox binding and committer delegations let the owner device write
    /// the Welcome to the requester's mailbox
    /// (`publisherDeviceRequestDelegations`).
    Admit {
        /// The device's wire KeyPackage.
        key_package: Vec<u8>,
        /// The requesting account's 97-byte mailbox binding.
        mailbox_binding: Vec<u8>,
        /// 0 to 16 raw 185-byte delegation blobs.
        delegations: Vec<Vec<u8>>,
    },
    /// Retire the leaf whose 32-byte signature key this is.
    Retire {
        /// The leaf's signature key.
        device_key: Vec<u8>,
    },
}

/// A decoded publisher device request.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiPublisherDeviceRequest {
    /// The 16-byte channel id it names.
    pub channel_id: Vec<u8>,
    /// What it asks for.
    pub action: FfiPublisherDeviceAction,
}

impl From<PublisherDeviceRequest> for FfiPublisherDeviceRequest {
    fn from(r: PublisherDeviceRequest) -> Self {
        Self {
            channel_id: r.channel_id.to_vec(),
            action: match r.action {
                PublisherDeviceAction::Admit {
                    key_package,
                    mailbox_binding,
                    delegations,
                } => FfiPublisherDeviceAction::Admit {
                    key_package,
                    mailbox_binding: mailbox_binding.encode().to_vec(),
                    delegations: delegations.iter().map(|d| d.to_vec()).collect(),
                },
                PublisherDeviceAction::Retire { device_key } => FfiPublisherDeviceAction::Retire {
                    device_key: device_key.to_vec(),
                },
            },
        }
    }
}

/// Compose the admit request a co-publisher's publishing device sends on the
/// channel's PublisherGroup (`createApplicationMessage`) for a device of its
/// own account that has no leaf yet (ADR-0090 section 2.2). `mailbox_binding`
/// is the account's own binding; `delegations` one ADR-0060 delegation per
/// owner device that may admit (`signDelegationBlob`), at most 16.
///
/// # Errors
///
/// [`FfiError::Contacts`] `publisherDeviceRequest.<code>` for a channel id
/// not 16 bytes, a KeyPackage of 0 or more than 4096 bytes, a malformed
/// binding, more than 16 delegations or a blob not 185 bytes.
#[uniffi::export]
pub fn compose_publisher_device_admit(
    channel_id: Vec<u8>,
    key_package: Vec<u8>,
    mailbox_binding: Vec<u8>,
    delegations: Vec<Vec<u8>>,
) -> Result<Vec<u8>, FfiError> {
    let channel_id = channel16(&channel_id, "publisherDeviceRequest")?;
    let mailbox_binding = MailboxBinding::decode(&mailbox_binding)
        .map_err(|_| request_error(PublisherDeviceRequestError::BadBinding))?;
    let delegations = delegations
        .iter()
        .map(|d| <[u8; COMMITTER_DELEGATION_BLOB_LEN]>::try_from(d.as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| request_error(PublisherDeviceRequestError::BadLength))?;
    rust_compose_request(&PublisherDeviceRequest {
        channel_id,
        action: PublisherDeviceAction::Admit {
            key_package,
            mailbox_binding,
            delegations,
        },
    })
    .map_err(request_error)
}

/// Compose the retire request for the leaf `device_key` of the requester's
/// own account (ADR-0090 sections 2.2, 2.4: a device leaving, or one the
/// account's other publishing device saw leave the account).
///
/// # Errors
///
/// [`FfiError::Contacts`] `publisherDeviceRequest.badLength` for a channel
/// id not 16 bytes or a key not 32 bytes.
#[uniffi::export]
pub fn compose_publisher_device_retire(
    channel_id: Vec<u8>,
    device_key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel_id = channel16(&channel_id, "publisherDeviceRequest")?;
    let device_key = <[u8; 32]>::try_from(device_key.as_slice())
        .map_err(|_| request_error(PublisherDeviceRequestError::BadLength))?;
    rust_compose_request(&PublisherDeviceRequest {
        channel_id,
        action: PublisherDeviceAction::Retire { device_key },
    })
    .map_err(request_error)
}

/// Strictly decode a publisher device request (structural only). An owner
/// device acts on it only when it came on that channel's PublisherGroup, the
/// MLS sender resolved at receipt to a current publisher account, and the
/// KeyPackage's binding (admit) or the named leaf (retire) belongs to that
/// same account.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `publisherDeviceRequest.<code>` first.
#[uniffi::export]
pub fn decode_publisher_device_request_envelope(
    plaintext: Vec<u8>,
) -> Result<FfiPublisherDeviceRequest, FfiError> {
    rust_decode_request(&plaintext)
        .map(Into::into)
        .map_err(request_error)
}

/// The committer-delegation envelope an owner device checks an admit with
/// (`checkCommitterDelegations`, then `committerDelegationFor` to pick the
/// one naming itself), naming `requester_account_key` (the MLS sender
/// resolved at receipt) as the added account. `None` for a retire.
///
/// # Errors
///
/// What [`decode_publisher_device_request_envelope`] returns, and
/// `publisherDeviceRequest.badLength` for a key not 32 bytes.
#[uniffi::export]
pub fn publisher_device_request_delegations(
    plaintext: Vec<u8>,
    requester_account_key: Vec<u8>,
) -> Result<Option<FfiCommitterDelegationEnvelope>, FfiError> {
    let key = <[u8; 32]>::try_from(requester_account_key.as_slice())
        .map_err(|_| request_error(PublisherDeviceRequestError::BadLength))?;
    let request = rust_decode_request(&plaintext).map_err(request_error)?;
    Ok(request.committer_delegations(&key).map(Into::into))
}

/// Shortest well-formed device request (a retire).
#[uniffi::export]
#[must_use]
pub fn publisher_device_request_envelope_min_len() -> u32 {
    len_u32(PUBLISHER_DEVICE_REQUEST_ENVELOPE_MIN_LEN)
}

/// Longest well-formed device request.
#[uniffi::export]
#[must_use]
pub fn publisher_device_request_envelope_max_len() -> u32 {
    len_u32(PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN)
}

/// A decoded admission mark (ADR-0090 section 6).
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiChannelAdmissionMark {
    /// The 16-byte channel id.
    pub channel_id: Vec<u8>,
    /// The service row sequence up to which every row was checked.
    pub through_seq: u64,
}

/// Compose the admission mark a publishing device sends after advancing its
/// mark; every publishing device keeps the largest from a current publisher.
///
/// # Errors
///
/// [`FfiError::Contacts`] `channelAdmissionMark.badLength` for a channel id
/// not 16 bytes.
#[uniffi::export]
pub fn compose_channel_admission_mark_envelope(
    channel_id: Vec<u8>,
    through_seq: u64,
) -> Result<Vec<u8>, FfiError> {
    let channel_id = channel16(&channel_id, "channelAdmissionMark")?;
    Ok(rust_compose_mark(&ChannelAdmissionMark {
        channel_id,
        through_seq,
    }))
}

/// Strictly decode an admission mark.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `channelAdmissionMark.<code>` first.
#[uniffi::export]
pub fn decode_channel_admission_mark_envelope(
    plaintext: Vec<u8>,
) -> Result<FfiChannelAdmissionMark, FfiError> {
    rust_decode_mark(&plaintext)
        .map(|m| FfiChannelAdmissionMark {
            channel_id: m.channel_id.to_vec(),
            through_seq: m.through_seq,
        })
        .map_err(mark_error)
}

/// The one well-formed admission-mark length (57).
#[uniffi::export]
#[must_use]
pub fn channel_admission_mark_envelope_len() -> u32 {
    len_u32(CHANNEL_ADMISSION_MARK_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_marks_round_trip_and_dispatch() {
        let binding = MailboxBinding::for_account_seed(&[0x31; 32])
            .encode()
            .to_vec();
        let admit = compose_publisher_device_admit(
            vec![7; 16],
            vec![0x4B; 40],
            binding.clone(),
            vec![vec![0xDE; COMMITTER_DELEGATION_BLOB_LEN]],
        )
        .expect("compose admit");
        let decoded = decode_publisher_device_request_envelope(admit.clone()).expect("decode");
        assert_eq!(decoded.channel_id, vec![7; 16]);
        assert!(
            matches!(decoded.action, FfiPublisherDeviceAction::Admit { ref key_package, .. } if key_package.len() == 40)
        );
        let env = publisher_device_request_delegations(admit.clone(), vec![0x22; 32])
            .expect("delegations")
            .expect("admit");
        assert_eq!(env.added_mailbox_binding, binding);
        assert_eq!(env.added_account_key, vec![0x22; 32]);
        let dispatched = crate::contacts::dispatch_incoming_envelope(admit).expect("dispatch");
        assert_eq!(
            dispatched.kind,
            crate::contacts::FfiIncomingEnvelopeKind::PublisherDeviceRequest
        );

        let retire = compose_publisher_device_retire(vec![7; 16], vec![9; 32]).expect("retire");
        assert_eq!(
            retire.len(),
            publisher_device_request_envelope_min_len() as usize
        );
        assert!(publisher_device_request_delegations(retire, vec![0x22; 32])
            .expect("ok")
            .is_none());

        let mark = compose_channel_admission_mark_envelope(vec![7; 16], 42).expect("mark");
        assert_eq!(mark.len(), channel_admission_mark_envelope_len() as usize);
        assert_eq!(
            decode_channel_admission_mark_envelope(mark.clone())
                .expect("decode")
                .through_seq,
            42
        );
        assert_eq!(
            crate::contacts::dispatch_incoming_envelope(mark)
                .expect("dispatch")
                .kind,
            crate::contacts::FfiIncomingEnvelopeKind::ChannelAdmissionMark
        );
        assert!(compose_publisher_device_admit(vec![7; 15], vec![1], binding, vec![]).is_err());
    }
}
