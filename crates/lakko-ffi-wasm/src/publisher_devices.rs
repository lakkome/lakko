// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0090 publisher bookkeeping envelopes on the wasm surface: a
//! co-publisher device's admit / retire request and a publishing device's
//! admission mark. Mirror of the UniFFI `publisher_devices` module. Refusals
//! throw `"publisherDeviceRequest.<code>: <reason>"` /
//! `"channelAdmissionMark.<code>: <reason>"`.

use lakko_contacts::channel_admission_mark::{
    compose_channel_admission_mark as rust_compose_mark,
    decode_channel_admission_mark as rust_decode_mark, ChannelAdmissionMark as RustMark,
    ChannelAdmissionMarkError, CHANNEL_ADMISSION_MARK_ENVELOPE_LEN,
};
use lakko_contacts::committer_delegation::COMMITTER_DELEGATION_BLOB_LEN;
use lakko_contacts::mailbox::MailboxBinding;
use lakko_contacts::publisher_device_request::{
    compose_publisher_device_request as rust_compose_request,
    decode_publisher_device_request as rust_decode_request, PublisherDeviceAction,
    PublisherDeviceRequest as RustRequest, PublisherDeviceRequestError,
    PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN, PUBLISHER_DEVICE_REQUEST_ENVELOPE_MIN_LEN,
};
use lakko_contacts::publisher_reader_key::{
    compose_publisher_reader_key as rust_compose_reader_key,
    decode_publisher_reader_key as rust_decode_reader_key, PublisherReaderKey as RustReaderKey,
    PublisherReaderKeyError, PUBLISHER_READER_KEY_ENVELOPE_LEN,
};
use wasm_bindgen::prelude::*;

use crate::scoped_discovery::CommitterDelegationEnvelope;

fn request_error(e: PublisherDeviceRequestError) -> String {
    format!("publisherDeviceRequest.{}: {e}", e.code())
}

fn mark_error(e: ChannelAdmissionMarkError) -> String {
    format!("channelAdmissionMark.{}: {e}", e.code())
}

fn channel16(channel_id: &[u8], what: &str) -> Result<[u8; 16], String> {
    channel_id
        .try_into()
        .map_err(|_| format!("{what}.badLength: channel id must be 16 bytes"))
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A decoded publisher device request (ADR-0090 section 2.2).
#[wasm_bindgen]
pub struct PublisherDeviceRequest {
    inner: RustRequest,
}

#[wasm_bindgen]
impl PublisherDeviceRequest {
    /// The 16-byte channel id it names.
    #[wasm_bindgen(getter, js_name = channelId)]
    #[must_use]
    pub fn channel_id(&self) -> Vec<u8> {
        self.inner.channel_id.to_vec()
    }

    /// `"admit"` or `"retire"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn action(&self) -> String {
        match self.inner.action {
            PublisherDeviceAction::Admit { .. } => "admit".to_string(),
            PublisherDeviceAction::Retire { .. } => "retire".to_string(),
        }
    }

    /// The device's wire KeyPackage (admit), else `undefined`.
    #[wasm_bindgen(getter, js_name = keyPackage)]
    #[must_use]
    pub fn key_package(&self) -> Option<Vec<u8>> {
        match &self.inner.action {
            PublisherDeviceAction::Admit { key_package, .. } => Some(key_package.clone()),
            PublisherDeviceAction::Retire { .. } => None,
        }
    }

    /// The leaf signature key to retire (retire), else `undefined`.
    #[wasm_bindgen(getter, js_name = deviceKey)]
    #[must_use]
    pub fn device_key(&self) -> Option<Vec<u8>> {
        match &self.inner.action {
            PublisherDeviceAction::Retire { device_key } => Some(device_key.to_vec()),
            PublisherDeviceAction::Admit { .. } => None,
        }
    }

    /// The committer-delegation envelope an owner device checks an admit
    /// with (`checkCommitterDelegations`, `committerDelegationFor`), naming
    /// `requesterAccountKey` (the MLS sender resolved at receipt) as the
    /// added account; `undefined` for a retire.
    ///
    /// # Errors
    ///
    /// Throws `publisherDeviceRequest.badLength` for a key not 32 bytes.
    #[wasm_bindgen(js_name = committerDelegations)]
    pub fn committer_delegations(
        &self,
        requester_account_key: &[u8],
    ) -> Result<Option<CommitterDelegationEnvelope>, String> {
        let key = <[u8; 32]>::try_from(requester_account_key)
            .map_err(|_| request_error(PublisherDeviceRequestError::BadLength))?;
        Ok(self
            .inner
            .committer_delegations(&key)
            .map(CommitterDelegationEnvelope::from_rust))
    }
}

/// Compose the admit request a co-publisher's publishing device sends on the
/// channel's PublisherGroup for a device of its own account that has no leaf
/// yet. `delegations` is an array of 185-byte blobs, at most 16.
///
/// # Errors
///
/// Throws `publisherDeviceRequest.<code>: ...`.
#[wasm_bindgen(js_name = composePublisherDeviceAdmit)]
pub fn compose_publisher_device_admit(
    channel_id: &[u8],
    key_package: &[u8],
    mailbox_binding: &[u8],
    delegations: Vec<js_sys::Uint8Array>,
) -> Result<Vec<u8>, String> {
    let channel_id = channel16(channel_id, "publisherDeviceRequest")?;
    let mailbox_binding = MailboxBinding::decode(mailbox_binding)
        .map_err(|_| request_error(PublisherDeviceRequestError::BadBinding))?;
    let delegations = delegations
        .iter()
        .map(|d| <[u8; COMMITTER_DELEGATION_BLOB_LEN]>::try_from(d.to_vec().as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| request_error(PublisherDeviceRequestError::BadLength))?;
    rust_compose_request(&RustRequest {
        channel_id,
        action: PublisherDeviceAction::Admit {
            key_package: key_package.to_vec(),
            mailbox_binding,
            delegations,
        },
    })
    .map_err(request_error)
}

/// Compose the retire request for the leaf `deviceKey` of the requester's
/// own account.
///
/// # Errors
///
/// Throws `publisherDeviceRequest.badLength: ...`.
#[wasm_bindgen(js_name = composePublisherDeviceRetire)]
pub fn compose_publisher_device_retire(
    channel_id: &[u8],
    device_key: &[u8],
) -> Result<Vec<u8>, String> {
    let channel_id = channel16(channel_id, "publisherDeviceRequest")?;
    let device_key = <[u8; 32]>::try_from(device_key)
        .map_err(|_| request_error(PublisherDeviceRequestError::BadLength))?;
    rust_compose_request(&RustRequest {
        channel_id,
        action: PublisherDeviceAction::Retire { device_key },
    })
    .map_err(request_error)
}

/// Strictly decode a publisher device request (structural only). An owner
/// device acts on it only when it came on that channel's PublisherGroup, the
/// MLS sender resolved to a current publisher account, and the KeyPackage's
/// binding (admit) or the named leaf (retire) belongs to that same account.
///
/// # Errors
///
/// Throws `publisherDeviceRequest.<code>: ...`.
#[wasm_bindgen(js_name = decodePublisherDeviceRequestEnvelope)]
pub fn decode_publisher_device_request_envelope(
    plaintext: &[u8],
) -> Result<PublisherDeviceRequest, String> {
    rust_decode_request(plaintext)
        .map(|inner| PublisherDeviceRequest { inner })
        .map_err(request_error)
}

/// Shortest well-formed device request (a retire).
#[wasm_bindgen(js_name = publisherDeviceRequestEnvelopeMinLen)]
#[must_use]
pub fn publisher_device_request_envelope_min_len() -> u32 {
    len_u32(PUBLISHER_DEVICE_REQUEST_ENVELOPE_MIN_LEN)
}

/// Longest well-formed device request.
#[wasm_bindgen(js_name = publisherDeviceRequestEnvelopeMaxLen)]
#[must_use]
pub fn publisher_device_request_envelope_max_len() -> u32 {
    len_u32(PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN)
}

/// A decoded admission mark (ADR-0090 section 6).
#[wasm_bindgen]
pub struct ChannelAdmissionMark {
    inner: RustMark,
}

#[wasm_bindgen]
impl ChannelAdmissionMark {
    /// The 16-byte channel id.
    #[wasm_bindgen(getter, js_name = channelId)]
    #[must_use]
    pub fn channel_id(&self) -> Vec<u8> {
        self.inner.channel_id.to_vec()
    }

    /// The service row sequence up to which every row was checked
    /// (`bigint`).
    #[wasm_bindgen(getter, js_name = throughSeq)]
    #[must_use]
    pub fn through_seq(&self) -> u64 {
        self.inner.through_seq
    }
}

/// Compose the admission mark a publishing device sends after advancing it.
///
/// # Errors
///
/// Throws `channelAdmissionMark.badLength: ...`.
#[wasm_bindgen(js_name = composeChannelAdmissionMarkEnvelope)]
pub fn compose_channel_admission_mark_envelope(
    channel_id: &[u8],
    through_seq: u64,
) -> Result<Vec<u8>, String> {
    let channel_id = channel16(channel_id, "channelAdmissionMark")?;
    Ok(rust_compose_mark(&RustMark {
        channel_id,
        through_seq,
    }))
}

/// Strictly decode an admission mark.
///
/// # Errors
///
/// Throws `channelAdmissionMark.<code>: ...`.
#[wasm_bindgen(js_name = decodeChannelAdmissionMarkEnvelope)]
pub fn decode_channel_admission_mark_envelope(
    plaintext: &[u8],
) -> Result<ChannelAdmissionMark, String> {
    rust_decode_mark(plaintext)
        .map(|inner| ChannelAdmissionMark { inner })
        .map_err(mark_error)
}

/// The one well-formed admission-mark length (57).
#[wasm_bindgen(js_name = channelAdmissionMarkEnvelopeLen)]
#[must_use]
pub fn channel_admission_mark_envelope_len() -> u32 {
    len_u32(CHANNEL_ADMISSION_MARK_ENVELOPE_LEN)
}

fn reader_key_error(e: PublisherReaderKeyError) -> String {
    format!("publisherReaderKey.{}: {e}", e.code())
}

/// A decoded publisher reader key (UX 15.1, live walk 2026-11-24).
#[wasm_bindgen]
pub struct PublisherReaderKey {
    inner: RustReaderKey,
}

#[wasm_bindgen]
impl PublisherReaderKey {
    /// The 16-byte channel id.
    #[wasm_bindgen(getter, js_name = channelId)]
    #[must_use]
    pub fn channel_id(&self) -> Vec<u8> {
        self.inner.channel_id.to_vec()
    }

    /// The sending account's 32-byte reader key.
    #[wasm_bindgen(getter, js_name = readerKey)]
    #[must_use]
    pub fn reader_key(&self) -> Vec<u8> {
        self.inner.reader_key.to_vec()
    }
}

/// Compose the envelope a publishing device sends on the PublisherGroup
/// after registering its account's reader key, and again on growth.
///
/// # Errors
///
/// Throws `publisherReaderKey.badLength: ...` for a channel id not 16 bytes
/// or a key not 32 bytes.
#[wasm_bindgen(js_name = composePublisherReaderKeyEnvelope)]
pub fn compose_publisher_reader_key_envelope(
    channel_id: &[u8],
    reader_key: &[u8],
) -> Result<Vec<u8>, String> {
    let channel_id = channel16(channel_id, "publisherReaderKey")?;
    let reader_key: [u8; 32] = reader_key
        .try_into()
        .map_err(|_| reader_key_error(PublisherReaderKeyError::BadLength))?;
    Ok(rust_compose_reader_key(&RustReaderKey {
        channel_id,
        reader_key,
    }))
}

/// Strictly decode a publisher reader-key envelope.
///
/// # Errors
///
/// Throws `publisherReaderKey.<code>: ...`.
#[wasm_bindgen(js_name = decodePublisherReaderKeyEnvelope)]
pub fn decode_publisher_reader_key_envelope(
    plaintext: &[u8],
) -> Result<PublisherReaderKey, String> {
    rust_decode_reader_key(plaintext)
        .map(|inner| PublisherReaderKey { inner })
        .map_err(reader_key_error)
}

/// The one well-formed publisher reader-key length (79).
#[wasm_bindgen(js_name = publisherReaderKeyEnvelopeLen)]
#[must_use]
pub fn publisher_reader_key_envelope_len() -> u32 {
    len_u32(PUBLISHER_READER_KEY_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retire_and_mark_round_trip() {
        let retire = compose_publisher_device_retire(&[7; 16], &[9; 32]).expect("retire");
        let decoded = decode_publisher_device_request_envelope(&retire).expect("decode");
        assert_eq!(decoded.action(), "retire");
        assert_eq!(decoded.device_key(), Some(vec![9; 32]));
        assert!(decoded.key_package().is_none());
        assert!(decoded
            .committer_delegations(&[1; 32])
            .expect("ok")
            .is_none());
        let mark = compose_channel_admission_mark_envelope(&[7; 16], 42).expect("mark");
        assert_eq!(mark.len(), channel_admission_mark_envelope_len() as usize);
        assert_eq!(
            decode_channel_admission_mark_envelope(&mark)
                .expect("decode")
                .through_seq(),
            42
        );
        assert!(compose_publisher_device_retire(&[7; 15], &[9; 32]).is_err());
    }
}
