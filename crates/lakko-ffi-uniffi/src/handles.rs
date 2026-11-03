// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for a group's or channel's `@name` at discovery (ADR-0087
//! section 3): the handle key derived from the journal root, the signed
//! statements every handle route takes, and the sealed public record a
//! public group or channel publishes under its search terms.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/handles.rs`. Only the owner holds
//! the handle key; it names no account. Clock and nonce of a statement are
//! minted here; a statement is good for one request within its window.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_contacts::handle::{
    handle_content_hash as rust_handle_content_hash,
    handle_write_public_key as rust_handle_write_public_key,
    handle_write_seed as rust_handle_write_seed, HandleStatement, HandleStatementBody,
    HandleStatementError, HandleVisibility, HANDLE_NONCE_LEN, LOOKUP_KEY_LEN, MAX_HANDLE_TERMS,
};
use lakko_contacts::handle_record::{
    compose_handle_record as rust_compose_handle_record,
    open_handle_record as rust_open_handle_record, open_search_record as rust_open_search_record,
    HandleRecordError, HandleRecordKind, HandleRecordPayload, SearchRecord, MAX_SIZE_BAND,
};

use crate::contacts::FfiAvatar;
use crate::scoped_discovery::{oprf64, FfiIntroductionPayload};
use crate::FfiError;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fixed<const N: usize>(what: &str, bytes: &[u8]) -> Result<[u8; N], FfiError> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        FfiError::Contacts(format!(
            "badLength: {what} must be {N} bytes, got {}",
            bytes.len()
        ))
    })
}

fn statement_error(e: HandleStatementError) -> FfiError {
    FfiError::Contacts(format!("handleStatement.{}: {e}", e.code()))
}

fn record_error(e: HandleRecordError) -> FfiError {
    FfiError::Contacts(format!("handleRecord.{}: {e}", e.code()))
}

// ============================================================================
// Handle key and statements
// ============================================================================

/// The 32-byte handle seed of a group or channel, from its 32-byte journal
/// root seed (`dsJournalRootSeed`; a channel's is its PublisherGroup's).
///
/// # Errors
///
/// [`FfiError::Contacts`] when the root seed is not 32 bytes.
#[uniffi::export]
pub fn handle_write_seed(journal_root_seed: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_handle_write_seed(&fixed::<32>("journal root seed", &journal_root_seed)?).to_vec())
}

/// The 32-byte public handle key of a handle seed: what discovery certifies.
///
/// # Errors
///
/// [`FfiError::Contacts`] when the seed is not 32 bytes.
#[uniffi::export]
pub fn handle_write_public_key(handle_seed: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_handle_write_public_key(&fixed::<32>("handle seed", &handle_seed)?).to_vec())
}

/// Whether a name is published (public) or only reserved (private).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiHandleVisibility {
    /// Reserves the name and answers no lookup.
    Private,
    /// Published: a record and search terms.
    Public,
}

impl From<FfiHandleVisibility> for HandleVisibility {
    fn from(v: FfiHandleVisibility) -> Self {
        match v {
            FfiHandleVisibility::Private => Self::Private,
            FfiHandleVisibility::Public => Self::Public,
        }
    }
}

/// What a handle statement asks discovery for. The route is fixed by the
/// kind: claim `POST /discovery/handle/claim`, update `PUT
/// /discovery/handle`, replace `POST /discovery/handle/replace`, rekey `POST
/// /discovery/handle/rekey`, retire `DELETE /discovery/handle`.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiHandleStatementBody {
    /// Take the name.
    Claim {
        /// Public or private.
        visibility: FfiHandleVisibility,
        /// [`handle_content_hash`] of the record and terms sent with it.
        content_hash: Vec<u8>,
    },
    /// Replace the record and terms, set the visibility, renew the lease.
    Update {
        /// Public or private.
        visibility: FfiHandleVisibility,
        /// [`handle_content_hash`] of the record and terms sent with it.
        content_hash: Vec<u8>,
    },
    /// Move to a new name; the named row is retired.
    Replace {
        /// The 64-byte lookup key of the new name.
        new_lookup_key: Vec<u8>,
        /// Public or private.
        visibility: FfiHandleVisibility,
        /// [`handle_content_hash`] for the new name.
        content_hash: Vec<u8>,
    },
    /// Move the name's rows to a new 32-byte handle key.
    Rekey {
        /// The key the rows move to.
        new_write_pk: Vec<u8>,
    },
    /// Retire the name.
    Retire,
}

impl TryFrom<FfiHandleStatementBody> for HandleStatementBody {
    type Error = FfiError;

    fn try_from(b: FfiHandleStatementBody) -> Result<Self, FfiError> {
        Ok(match b {
            FfiHandleStatementBody::Claim {
                visibility,
                content_hash,
            } => Self::Claim {
                visibility: visibility.into(),
                content_hash: fixed("content hash", &content_hash)?,
            },
            FfiHandleStatementBody::Update {
                visibility,
                content_hash,
            } => Self::Update {
                visibility: visibility.into(),
                content_hash: fixed("content hash", &content_hash)?,
            },
            FfiHandleStatementBody::Replace {
                new_lookup_key,
                visibility,
                content_hash,
            } => Self::Replace {
                new_lookup_key: fixed::<LOOKUP_KEY_LEN>("new lookup key", &new_lookup_key)?,
                visibility: visibility.into(),
                content_hash: fixed("content hash", &content_hash)?,
            },
            FfiHandleStatementBody::Rekey { new_write_pk } => Self::Rekey {
                new_write_pk: fixed("new write key", &new_write_pk)?,
            },
            FfiHandleStatementBody::Retire => Self::Retire,
        })
    }
}

/// The 32-byte content hash a claim / update / replace statement covers:
/// over the sealed record (`None` for a private name) and the 64-byte
/// lookup keys of its search terms, exactly as the request body carries them.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `handleStatement.badContent` for terms
/// without a record or more than the term limit, or a term not 64 bytes.
#[uniffi::export]
pub fn handle_content_hash(
    record: Option<Vec<u8>>,
    terms: Vec<Vec<u8>>,
) -> Result<Vec<u8>, FfiError> {
    let terms = terms
        .iter()
        .map(|t| fixed::<LOOKUP_KEY_LEN>("term", t))
        .collect::<Result<Vec<_>, _>>()?;
    rust_handle_content_hash(record.as_deref(), &terms)
        .map(|h| h.to_vec())
        .map_err(statement_error)
}

/// Sign one handle statement for the name whose 64-byte lookup key is
/// `lookup_key`, with the 32-byte handle seed. Returns the statement as
/// standard base64, the `statement_b64` of the route's JSON body.
///
/// # Errors
///
/// [`FfiError::Contacts`] for a wrong-length argument.
#[uniffi::export]
pub fn sign_handle_statement(
    handle_seed: Vec<u8>,
    lookup_key: Vec<u8>,
    body: FfiHandleStatementBody,
) -> Result<String, FfiError> {
    let seed = fixed::<32>("handle seed", &handle_seed)?;
    let lookup = fixed::<LOOKUP_KEY_LEN>("lookup key", &lookup_key)?;
    let mut nonce = [0u8; HANDLE_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    let statement = HandleStatement::sign(&seed, lookup, body.try_into()?, now_ms(), nonce);
    Ok(B64.encode(statement.encode()))
}

/// The most search terms one record carries.
#[uniffi::export]
#[must_use]
pub fn max_handle_terms() -> u32 {
    u32::try_from(MAX_HANDLE_TERMS).unwrap_or(u32::MAX)
}

// ============================================================================
// The public record of a group or channel
// ============================================================================

/// Group or channel.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiHandleRecordKind {
    /// A public group: `id` is its 32-byte group id, `key` its knock key.
    Group,
    /// A public channel: `id` is its 16-byte channel id, `key` its read key.
    Channel,
}

impl From<HandleRecordKind> for FfiHandleRecordKind {
    fn from(k: HandleRecordKind) -> Self {
        match k {
            HandleRecordKind::Group => Self::Group,
            HandleRecordKind::Channel => Self::Channel,
        }
    }
}

impl From<FfiHandleRecordKind> for HandleRecordKind {
    fn from(k: FfiHandleRecordKind) -> Self {
        match k {
            FfiHandleRecordKind::Group => Self::Group,
            FfiHandleRecordKind::Channel => Self::Channel,
        }
    }
}

/// What a public group's or channel's record reveals to whoever holds a
/// matching OPRF output: the owner's claim, signed by nobody inside; trust
/// it as far as the handle certificate the lookup returns.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiHandleRecordPayload {
    /// Group or channel.
    pub kind: FfiHandleRecordKind,
    /// Owner-clock UNIX milliseconds of this version.
    pub updated_at_ms: u64,
    /// The group id (32 bytes) or channel id (16 bytes).
    pub id: Vec<u8>,
    /// The 32-byte knock public key (group) or read key (channel).
    pub key: Vec<u8>,
    /// The canonical `@name`.
    pub handle: String,
    /// The name, non-empty.
    pub name: String,
    /// The description, possibly empty.
    pub description: String,
    /// The size band, `0..=5`.
    pub size_band: u8,
    /// The photo, or `None`. Its keys are public.
    #[uniffi(default = None)]
    pub avatar: Option<FfiAvatar>,
}

impl From<HandleRecordPayload> for FfiHandleRecordPayload {
    fn from(p: HandleRecordPayload) -> Self {
        Self {
            kind: p.kind.into(),
            updated_at_ms: p.updated_at_ms,
            id: p.id,
            key: p.key.to_vec(),
            handle: p.handle,
            name: p.name,
            description: p.description,
            size_band: p.size_band,
            avatar: p.avatar.map(Into::into),
        }
    }
}

impl TryFrom<FfiHandleRecordPayload> for HandleRecordPayload {
    type Error = FfiError;

    fn try_from(p: FfiHandleRecordPayload) -> Result<Self, FfiError> {
        Ok(Self {
            kind: p.kind.into(),
            updated_at_ms: p.updated_at_ms,
            id: p.id,
            key: fixed("record key", &p.key)?,
            handle: p.handle,
            name: p.name,
            description: p.description,
            size_band: p.size_band,
            avatar: p.avatar.map(TryInto::try_into).transpose()?,
        })
    }
}

/// Seal a public record under the 64-byte OPRF outputs of its search terms
/// (the `@name` and the name's prefix terms), the way a person's public
/// record is.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `handleRecord.<code>` first, or a
/// wrong-length argument.
#[uniffi::export]
pub fn compose_handle_record(
    payload: FfiHandleRecordPayload,
    term_oprfs: Vec<Vec<u8>>,
    issued_at_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    let payload = HandleRecordPayload::try_from(payload)?;
    let terms = term_oprfs
        .iter()
        .map(|t| oprf64(t))
        .collect::<Result<Vec<_>, _>>()?;
    rust_compose_handle_record(&payload, &terms, issued_at_ms).map_err(record_error)
}

/// Open a group's or channel's public record with the 64-byte OPRF output
/// of the searched term. `None` when no entry matches.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `handleRecord.<code>` first.
#[uniffi::export]
pub fn open_handle_record(
    record: Vec<u8>,
    oprf_output: Vec<u8>,
) -> Result<Option<FfiHandleRecordPayload>, FfiError> {
    rust_open_handle_record(&record, &oprf64(&oprf_output)?)
        .map(|p| p.map(Into::into))
        .map_err(record_error)
}

/// What a search hit turned out to be.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiSearchRecordKind {
    /// A person in Everyone (`person` is set).
    Person,
    /// A public group or channel (`conversation` is set).
    Conversation,
}

/// One opened search record: a person or a group / channel, never both.
#[derive(uniffi::Record, Debug, Clone)]
pub struct FfiSearchRecord {
    /// Which field is set.
    pub kind: FfiSearchRecordKind,
    /// The person's public record payload.
    pub person: Option<FfiIntroductionPayload>,
    /// The group's or channel's record payload.
    pub conversation: Option<FfiHandleRecordPayload>,
}

/// Open any record a name or `@name` lookup returns (one namespace for
/// people, groups and channels, ADR-0087 section 1) with the 64-byte OPRF
/// output of the searched term. `None` when no entry matches.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `handleRecord.<code>` first.
#[uniffi::export]
pub fn open_search_record(
    record: Vec<u8>,
    oprf_output: Vec<u8>,
) -> Result<Option<FfiSearchRecord>, FfiError> {
    Ok(rust_open_search_record(&record, &oprf64(&oprf_output)?)
        .map_err(record_error)?
        .map(|r| match r {
            SearchRecord::Person(p) => FfiSearchRecord {
                kind: FfiSearchRecordKind::Person,
                person: Some((*p).into()),
                conversation: None,
            },
            SearchRecord::Conversation(c) => FfiSearchRecord {
                kind: FfiSearchRecordKind::Conversation,
                person: None,
                conversation: Some((*c).into()),
            },
        }))
}

/// The largest size band a record carries.
#[uniffi::export]
#[must_use]
pub const fn handle_record_max_size_band() -> u8 {
    MAX_SIZE_BAND
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::handle::HandleStatement as Statement;

    fn payload() -> FfiHandleRecordPayload {
        FfiHandleRecordPayload {
            kind: FfiHandleRecordKind::Group,
            updated_at_ms: 3,
            id: vec![0x42; 32],
            key: vec![0x4B; 32],
            handle: "book_club".into(),
            name: "Book club".into(),
            description: "Monthly".into(),
            size_band: 1,
            avatar: None,
        }
    }

    #[test]
    fn statements_sign_under_the_derived_handle_key() {
        let seed = handle_write_seed(vec![0x11; 32]).unwrap();
        let public = handle_write_public_key(seed.clone()).unwrap();
        let hash = handle_content_hash(None, Vec::new()).unwrap();
        let b64 = sign_handle_statement(
            seed,
            vec![0xAB; 64],
            FfiHandleStatementBody::Claim {
                visibility: FfiHandleVisibility::Private,
                content_hash: hash,
            },
        )
        .unwrap();
        let statement = Statement::decode(&B64.decode(b64).unwrap()).unwrap();
        assert_eq!(statement.write_pk.to_vec(), public);
        statement.verify(statement.unix_ms).unwrap();
        assert!(handle_content_hash(None, vec![vec![0; 64]]).is_err());
    }

    #[test]
    fn handle_record_opens_by_term_and_as_a_search_record() {
        let term = vec![0x33_u8; 64];
        let record = compose_handle_record(payload(), vec![term.clone()], 10).unwrap();
        assert_eq!(
            open_handle_record(record.clone(), term.clone()).unwrap(),
            Some(payload())
        );
        assert_eq!(
            open_handle_record(record.clone(), vec![0x34; 64]).unwrap(),
            None
        );
        let hit = open_search_record(record, term).unwrap().unwrap();
        assert_eq!(hit.kind, FfiSearchRecordKind::Conversation);
        assert_eq!(hit.conversation, Some(payload()));
        assert!(handle_record_max_size_band() >= 1);
    }
}
