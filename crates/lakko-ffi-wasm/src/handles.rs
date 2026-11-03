// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for a group's or channel's `@name` at discovery
//! (ADR-0087 section 3): the handle key, the signed statements every handle
//! route takes, and the sealed public record of a public group or channel.
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/handles.rs`. Lists of 64-byte
//! lookup keys / OPRF outputs cross as one flat concatenation.
//!
//! # Errors
//!
//! Every fallible function throws a string `"<area>.<code>: <reason>"`.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_contacts::handle::{
    handle_content_hash as rust_handle_content_hash,
    handle_write_public_key as rust_handle_write_public_key,
    handle_write_seed as rust_handle_write_seed, HandleStatement, HandleStatementBody,
    HandleVisibility, HANDLE_NONCE_LEN, LOOKUP_KEY_LEN, MAX_HANDLE_TERMS,
};
use lakko_contacts::handle_record::{
    compose_handle_record as rust_compose_handle_record,
    open_handle_record as rust_open_handle_record, open_search_record as rust_open_search_record,
    HandleRecordError, HandleRecordKind, HandleRecordPayload, SearchRecord, MAX_SIZE_BAND,
};
use wasm_bindgen::prelude::*;

use crate::channel_access::{fixed, now_ms};
use crate::contacts::ProfileAvatar;
use crate::scoped_discovery::IntroductionPayload;

fn flat64(area: &str, what: &str, bytes: &[u8]) -> Result<Vec<[u8; 64]>, String> {
    if bytes.len() % 64 != 0 {
        return Err(format!(
            "{area}.badLength: {what} must be a concatenation of 64-byte values"
        ));
    }
    Ok(bytes
        .chunks_exact(64)
        .filter_map(|c| <[u8; 64]>::try_from(c).ok())
        .collect())
}

fn record_error(e: HandleRecordError) -> String {
    format!("handleRecord.{}: {e}", e.code())
}

fn visibility(public: bool) -> HandleVisibility {
    if public {
        HandleVisibility::Public
    } else {
        HandleVisibility::Private
    }
}

/// The 32-byte handle seed from the 32-byte journal root seed.
///
/// # Errors
///
/// Throws `handleStatement.badLength: ...`.
#[wasm_bindgen(js_name = handleWriteSeed)]
pub fn handle_write_seed(journal_root_seed: &[u8]) -> Result<Vec<u8>, String> {
    Ok(rust_handle_write_seed(&fixed::<32>(
        "handleStatement",
        "journal root seed",
        journal_root_seed,
    )?)
    .to_vec())
}

/// The 32-byte public handle key of a handle seed.
///
/// # Errors
///
/// Throws `handleStatement.badLength: ...`.
#[wasm_bindgen(js_name = handleWritePublicKey)]
pub fn handle_write_public_key(handle_seed: &[u8]) -> Result<Vec<u8>, String> {
    Ok(
        rust_handle_write_public_key(&fixed::<32>("handleStatement", "handle seed", handle_seed)?)
            .to_vec(),
    )
}

/// The 32-byte content hash over the sealed `record` (`undefined` for a
/// private name) and the concatenated 64-byte lookup keys of its terms.
///
/// # Errors
///
/// Throws `handleStatement.badContent: ...` (or a length code).
#[wasm_bindgen(js_name = handleContentHash)]
pub fn handle_content_hash(record: Option<Vec<u8>>, terms: &[u8]) -> Result<Vec<u8>, String> {
    let terms = flat64("handleStatement", "terms", terms)?;
    rust_handle_content_hash(record.as_deref(), &terms)
        .map(|h| h.to_vec())
        .map_err(|e| format!("handleStatement.{}: {e}", e.code()))
}

fn sign_statement(
    handle_seed: &[u8],
    lookup_key: &[u8],
    body: HandleStatementBody,
) -> Result<String, String> {
    let seed = fixed::<32>("handleStatement", "handle seed", handle_seed)?;
    let lookup = fixed::<LOOKUP_KEY_LEN>("handleStatement", "lookup key", lookup_key)?;
    let mut nonce = [0u8; HANDLE_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    Ok(B64.encode(HandleStatement::sign(&seed, lookup, body, now_ms(), nonce).encode()))
}

/// A claim statement (`POST /discovery/handle/claim`), standard base64.
///
/// # Errors
///
/// Throws `handleStatement.badLength: ...`.
#[wasm_bindgen(js_name = signHandleClaim)]
pub fn sign_handle_claim(
    handle_seed: &[u8],
    lookup_key: &[u8],
    public: bool,
    content_hash: &[u8],
) -> Result<String, String> {
    let content_hash = fixed("handleStatement", "content hash", content_hash)?;
    sign_statement(
        handle_seed,
        lookup_key,
        HandleStatementBody::Claim {
            visibility: visibility(public),
            content_hash,
        },
    )
}

/// An update statement (`PUT /discovery/handle`), standard base64.
///
/// # Errors
///
/// Throws `handleStatement.badLength: ...`.
#[wasm_bindgen(js_name = signHandleUpdate)]
pub fn sign_handle_update(
    handle_seed: &[u8],
    lookup_key: &[u8],
    public: bool,
    content_hash: &[u8],
) -> Result<String, String> {
    let content_hash = fixed("handleStatement", "content hash", content_hash)?;
    sign_statement(
        handle_seed,
        lookup_key,
        HandleStatementBody::Update {
            visibility: visibility(public),
            content_hash,
        },
    )
}

/// A replace statement (`POST /discovery/handle/replace`), standard base64.
///
/// # Errors
///
/// Throws `handleStatement.badLength: ...`.
#[wasm_bindgen(js_name = signHandleReplace)]
pub fn sign_handle_replace(
    handle_seed: &[u8],
    lookup_key: &[u8],
    new_lookup_key: &[u8],
    public: bool,
    content_hash: &[u8],
) -> Result<String, String> {
    let content_hash = fixed("handleStatement", "content hash", content_hash)?;
    let new_lookup_key =
        fixed::<LOOKUP_KEY_LEN>("handleStatement", "new lookup key", new_lookup_key)?;
    sign_statement(
        handle_seed,
        lookup_key,
        HandleStatementBody::Replace {
            new_lookup_key,
            visibility: visibility(public),
            content_hash,
        },
    )
}

/// A rekey statement (`POST /discovery/handle/rekey`), standard base64.
///
/// # Errors
///
/// Throws `handleStatement.badLength: ...`.
#[wasm_bindgen(js_name = signHandleRekey)]
pub fn sign_handle_rekey(
    handle_seed: &[u8],
    lookup_key: &[u8],
    new_write_pk: &[u8],
) -> Result<String, String> {
    let new_write_pk = fixed("handleStatement", "new write key", new_write_pk)?;
    sign_statement(
        handle_seed,
        lookup_key,
        HandleStatementBody::Rekey { new_write_pk },
    )
}

/// A retire statement (`DELETE /discovery/handle`), standard base64.
///
/// # Errors
///
/// Throws `handleStatement.badLength: ...`.
#[wasm_bindgen(js_name = signHandleRetire)]
pub fn sign_handle_retire(handle_seed: &[u8], lookup_key: &[u8]) -> Result<String, String> {
    sign_statement(handle_seed, lookup_key, HandleStatementBody::Retire)
}

/// The most search terms one record carries.
#[wasm_bindgen(js_name = maxHandleTerms)]
#[must_use]
pub fn max_handle_terms() -> u32 {
    u32::try_from(MAX_HANDLE_TERMS).unwrap_or(u32::MAX)
}

/// What a public group's or channel's record reveals.
#[wasm_bindgen(js_name = HandleRecordPayload)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmHandleRecordPayload {
    inner: HandleRecordPayload,
}

#[wasm_bindgen(js_class = HandleRecordPayload)]
impl WasmHandleRecordPayload {
    /// Build a payload: `kind` `"group"` (32-byte id, knock key) or
    /// `"channel"` (16-byte id, read key).
    ///
    /// # Errors
    ///
    /// Throws `handleRecord.unknownKind` / `handleRecord.badLength: ...`.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::too_many_arguments)] // one argument per record field
    pub fn new(
        kind: &str,
        updated_at_ms: u64,
        id: &[u8],
        key: &[u8],
        handle: String,
        name: String,
        description: String,
        size_band: u8,
        avatar: Option<ProfileAvatar>,
    ) -> Result<WasmHandleRecordPayload, String> {
        let kind = match kind {
            "group" => HandleRecordKind::Group,
            "channel" => HandleRecordKind::Channel,
            _ => {
                return Err(
                    "handleRecord.unknownKind: kind must be \"group\" or \"channel\"".to_string(),
                )
            },
        };
        Ok(Self {
            inner: HandleRecordPayload {
                kind,
                updated_at_ms,
                id: id.to_vec(),
                key: fixed("handleRecord", "record key", key)?,
                handle,
                name,
                description,
                size_band,
                avatar: avatar.map(|a| a.inner),
            },
        })
    }

    /// `"group"` or `"channel"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> String {
        match self.inner.kind {
            HandleRecordKind::Group => "group",
            HandleRecordKind::Channel => "channel",
        }
        .to_string()
    }

    /// Owner-clock UNIX milliseconds.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    #[must_use]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }

    /// The group id or channel id.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn id(&self) -> Vec<u8> {
        self.inner.id.clone()
    }

    /// The knock public key (group) or read key (channel).
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn key(&self) -> Vec<u8> {
        self.inner.key.to_vec()
    }

    /// The canonical `@name`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn handle(&self) -> String {
        self.inner.handle.clone()
    }

    /// The name.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn name(&self) -> String {
        self.inner.name.clone()
    }

    /// The description.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn description(&self) -> String {
        self.inner.description.clone()
    }

    /// The size band, `0..=5`.
    #[wasm_bindgen(getter, js_name = sizeBand)]
    #[must_use]
    pub fn size_band(&self) -> u8 {
        self.inner.size_band
    }

    /// The photo (its keys are public), or `undefined`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn avatar(&self) -> Option<ProfileAvatar> {
        self.inner
            .avatar
            .clone()
            .map(|inner| ProfileAvatar { inner })
    }
}

/// Seal a public record under the concatenated 64-byte OPRF outputs of its
/// search terms.
///
/// # Errors
///
/// Throws `handleRecord.<code>: ...`.
#[wasm_bindgen(js_name = composeHandleRecord)]
pub fn compose_handle_record(
    payload: &WasmHandleRecordPayload,
    term_oprfs: &[u8],
    issued_at_ms: u64,
) -> Result<Vec<u8>, String> {
    let terms = flat64("handleRecord", "term OPRF outputs", term_oprfs)?;
    rust_compose_handle_record(&payload.inner, &terms, issued_at_ms).map_err(record_error)
}

/// Open a group's or channel's record with the 64-byte OPRF output of the
/// searched term; `undefined` when no entry matches.
///
/// # Errors
///
/// Throws `handleRecord.<code>: ...`.
#[wasm_bindgen(js_name = openHandleRecord)]
pub fn open_handle_record(
    record: &[u8],
    oprf_output: &[u8],
) -> Result<Option<WasmHandleRecordPayload>, String> {
    let oprf = fixed::<64>("handleRecord", "OPRF output", oprf_output)?;
    rust_open_handle_record(record, &oprf)
        .map(|p| p.map(|inner| WasmHandleRecordPayload { inner }))
        .map_err(record_error)
}

/// One opened search record: a person or a group / channel.
#[wasm_bindgen(js_name = SearchRecord)]
pub struct WasmSearchRecord {
    person: Option<IntroductionPayload>,
    conversation: Option<WasmHandleRecordPayload>,
}

#[wasm_bindgen(js_class = SearchRecord)]
impl WasmSearchRecord {
    /// `"person"` or `"conversation"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> String {
        if self.person.is_some() {
            "person"
        } else {
            "conversation"
        }
        .to_string()
    }

    /// The person's public record payload, or `undefined`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn person(&self) -> Option<IntroductionPayload> {
        self.person.clone()
    }

    /// The group's or channel's record payload, or `undefined`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn conversation(&self) -> Option<WasmHandleRecordPayload> {
        self.conversation.clone()
    }
}

/// Open any record a name or `@name` lookup returns (one namespace for
/// people, groups and channels); `undefined` when no entry matches.
///
/// # Errors
///
/// Throws `handleRecord.<code>: ...`.
#[wasm_bindgen(js_name = openSearchRecord)]
pub fn open_search_record(
    record: &[u8],
    oprf_output: &[u8],
) -> Result<Option<WasmSearchRecord>, String> {
    let oprf = fixed::<64>("handleRecord", "OPRF output", oprf_output)?;
    Ok(rust_open_search_record(record, &oprf)
        .map_err(record_error)?
        .map(|r| match r {
            SearchRecord::Person(p) => WasmSearchRecord {
                person: Some(IntroductionPayload::from_rust(*p)),
                conversation: None,
            },
            SearchRecord::Conversation(c) => WasmSearchRecord {
                person: None,
                conversation: Some(WasmHandleRecordPayload { inner: *c }),
            },
        }))
}

/// The largest size band a record carries.
#[wasm_bindgen(js_name = handleRecordMaxSizeBand)]
#[must_use]
pub fn handle_record_max_size_band() -> u8 {
    MAX_SIZE_BAND
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_and_record_round_trip() {
        let seed = handle_write_seed(&[0x11; 32]).unwrap();
        let public = handle_write_public_key(&seed).unwrap();
        let hash = handle_content_hash(None, &[]).unwrap();
        let b64 = sign_handle_claim(&seed, &[0xAB; 64], false, &hash).unwrap();
        let st = HandleStatement::decode(&B64.decode(b64).unwrap()).unwrap();
        assert_eq!(st.write_pk.to_vec(), public);
        st.verify(st.unix_ms).unwrap();
        assert!(sign_handle_retire(&seed, &[0xAB; 64]).is_ok());

        let payload = WasmHandleRecordPayload::new(
            "group",
            3,
            &[0x42; 32],
            &[0x4B; 32],
            "book_club".into(),
            "Book club".into(),
            String::new(),
            1,
            None,
        )
        .unwrap();
        let term = [0x33_u8; 64];
        let record = compose_handle_record(&payload, &term, 10).unwrap();
        assert_eq!(
            open_handle_record(&record, &term).unwrap(),
            Some(payload.clone())
        );
        let hit = open_search_record(&record, &term).unwrap().unwrap();
        assert_eq!(hit.kind(), "conversation");
        assert_eq!(hit.conversation(), Some(payload));
        assert!(WasmHandleRecordPayload::new(
            "x",
            0,
            &[],
            &[0; 32],
            String::new(),
            String::new(),
            String::new(),
            0,
            None
        )
        .is_err());
    }
}
