// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0060 section 5 - Everyone, name search and username certificates.
//!
//! - **Everyone** is the only setting the server knows about. A person in
//!   Everyone has `usernames.is_public` set and one `display_names` row per
//!   name search term; `POST /discovery/lookup` answers their username and
//!   `POST /discovery/name/lookup` finds them by name. Everyone else is
//!   invisible to both: a lookup of their username answers exactly as for an
//!   unregistered one. Who else can find them is decided by reach keys the
//!   server never sees (ADR-0060 sections 1-4).
//! - **Name terms** arrive as lookup keys
//!   (`lakko_discovery_client::discovery_lookup_key` of each term's OPRF
//!   output; `lakko_discovery_client::name` normalises and splits the name
//!   client-side); the plaintext name never reaches the server (invariant #1).
//!   Every `oprf_hash` this service stores is such a lookup key, never an
//!   OPRF output.
//! - **Public record**: entering Everyone also stores one opaque public
//!   record (`lakko_contacts::introduction::compose_public_record`, keyed
//!   per term over the OPRF outputs of the username and the name terms, no
//!   `fof` grant). Both lookups return it beside the account id and the
//!   searcher opens it with the OPRF output it already holds. The lookup
//!   keys in these tables are one-way from the outputs, so a copy of the
//!   database opens no record; the operator, who holds the OPRF key, can
//!   still evaluate guessed names (ADR-0060 section 5). The server checks
//!   the record's structure only.
//! - **Username certificate** (`lakko_attest::username_cert`): discovery signs
//!   `(lookup key of OPRF(username), account_id, issued_at)` with a dedicated Ed25519 key, so
//!   a viewer verifies a claimed username with one OPRF evaluation and no
//!   lookup, and no endpoint confirms a guessed username for a known account.

use anyhow::Context as _;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_attest::{UsernameCertificate, OPRF_HASH_LEN};
use lakko_contacts::introduction::{IntroductionRecord, INTRODUCTION_RECORD_MAX_LEN};
use lakko_crypto::signing::SigningKey;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;

use crate::auth::AuthenticatedAccount;
use crate::AppState;

/// The most name search terms one account may publish. Mirrors
/// `lakko_discovery_client::MAX_NAME_TERMS` (pinned by a test).
pub(crate) const MAX_NAME_TERMS: usize = 64;

/// The most account ids one name lookup returns. Chosen, not measured
/// (ADR-0060 open question 2); the client fetches each profile and filters
/// by the whole query.
pub(crate) const MAX_NAME_RESULTS: usize = 20;

/// The longest public record accepted: the longest introduction record the
/// codec composes. `migrations/0029_public_record_mailbox_binding.sql` sizes
/// its column to it (pinned by a test).
pub(crate) const MAX_PUBLIC_RECORD_LEN: usize = INTRODUCTION_RECORD_MAX_LEN;

/// Dev / e2e override of the certificate signing seed (64 hex characters),
/// so a test can pin the verifying key ahead of time. Never set in
/// production.
const CERT_SEED_ENV: &str = "LAKKO_DISCOVERY_USERNAME_CERT_SEED_HEX";

type HandlerError = (StatusCode, String);

fn db_err(what: &'static str) -> impl Fn(sqlx::Error) -> HandlerError {
    move |e| {
        tracing::error!(error = %e, "{what}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    }
}

fn bad_request(msg: impl Into<String>) -> HandlerError {
    (StatusCode::BAD_REQUEST, msg.into())
}

fn now_unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

// ============================================================================
// The signing key
// ============================================================================

/// The username-certificate signing key: the dev override when
/// [`CERT_SEED_ENV`] is set (loudly), else the persisted key, generated on
/// first boot. A racing second start loses the insert and reads the winner.
///
/// # Errors
///
/// A malformed override, a DB failure, or a persisted seed that is not 32
/// bytes (corruption).
pub(crate) async fn load_or_init_cert_key(pool: &MySqlPool) -> anyhow::Result<SigningKey> {
    if let Some(key) = cert_key_from_env()? {
        tracing::warn!(
            "{CERT_SEED_ENV} is set: username certificates are signed with a FIXED dev key. \
             Never set this in production."
        );
        return Ok(key);
    }
    let read = || async {
        sqlx::query_as::<_, (Vec<u8>,)>("SELECT seed FROM username_cert_signing_key WHERE id = 1")
            .fetch_optional(pool)
            .await
            .context("read username_cert_signing_key")
    };
    if let Some((seed,)) = read().await? {
        return key_from_seed(&seed);
    }
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    sqlx::query("INSERT IGNORE INTO username_cert_signing_key (id, seed) VALUES (1, ?)")
        .bind(&seed[..])
        .execute(pool)
        .await
        .context("insert username_cert_signing_key")?;
    let (seed,) = read()
        .await?
        .context("username_cert_signing_key empty after insert")?;
    key_from_seed(&seed)
}

fn key_from_seed(seed: &[u8]) -> anyhow::Result<SigningKey> {
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| anyhow::anyhow!("username_cert_signing_key.seed is not 32 bytes"))?;
    Ok(SigningKey::from_seed(&seed))
}

fn cert_key_from_env() -> anyhow::Result<Option<SigningKey>> {
    match std::env::var(CERT_SEED_ENV) {
        Ok(raw) => parse_cert_seed_hex(&raw),
        Err(_) => Ok(None),
    }
}

/// Pure half of [`cert_key_from_env`]: empty is unset, anything else must be
/// 64 hex characters.
fn parse_cert_seed_hex(raw: &str) -> anyhow::Result<Option<SigningKey>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let mut seed = [0u8; 32];
    hex::decode_to_slice(raw, &mut seed)
        .with_context(|| format!("{CERT_SEED_ENV} must be 64 hex characters"))?;
    Ok(Some(SigningKey::from_seed(&seed)))
}

/// Sign a certificate for `(oprf_hash, account_id)` now.
pub(crate) fn sign_certificate(
    key: &SigningKey,
    oprf_hash: [u8; OPRF_HASH_LEN],
    account_id: [u8; 16],
) -> String {
    B64.encode(UsernameCertificate::new(oprf_hash, account_id, now_unix_millis()).sign(key))
}

#[derive(Serialize)]
pub(crate) struct CertKeyResponse {
    /// Always `"Ed25519"`.
    pub(crate) alg: &'static str,
    /// The 32-byte verifying key, base64url without padding.
    pub(crate) key_b64url: String,
}

/// `GET /.well-known/lakko-username-cert-key` - the verifying key clients pin.
/// Unauthenticated; the zero-trust path pins the key at build time and does
/// not fetch it from here.
pub(crate) async fn cert_key(State(state): State<AppState>) -> Json<CertKeyResponse> {
    Json(CertKeyResponse {
        alg: "Ed25519",
        key_b64url: URL_SAFE_NO_PAD.encode(state.username_cert_key.verifying_key().to_bytes()),
    })
}

#[derive(Serialize)]
pub(crate) struct CertificateResponse {
    /// Standard base64 of the 153-byte certificate.
    pub(crate) username_certificate_b64: String,
}

/// `GET /discovery/username-certificate` - the caller's certificate, signed
/// now. Session-gated; the account comes from the token. `404` when the
/// account has no current username (ADR-0076: never a retired one).
pub(crate) async fn get_certificate(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<AuthenticatedAccount>,
) -> Result<Json<CertificateResponse>, HandlerError> {
    // The CURRENT name only (ADR-0076): an account holds at most one, and a
    // name it renamed away from is never certified again.
    let row: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT oprf_hash FROM usernames WHERE account_id = ? AND retired_at IS NULL",
    )
    .bind(&authed.0[..])
    .fetch_optional(&state.db)
    .await
    .map_err(db_err("load username for certificate"))?;
    let Some((hash,)) = row else {
        return Err((StatusCode::NOT_FOUND, "no username".to_string()));
    };
    let hash: [u8; OPRF_HASH_LEN] = hash.try_into().map_err(|_| {
        tracing::error!("usernames.oprf_hash is not 64 bytes");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;
    Ok(Json(CertificateResponse {
        username_certificate_b64: sign_certificate(&state.username_cert_key, hash, authed.0),
    }))
}

// ============================================================================
// Everyone
// ============================================================================

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VisibilityRequest {
    /// `true` for Everyone.
    pub(crate) public: bool,
    /// Standard base64 of each name term's 64-byte lookup key (derived from
    /// the term's OPRF output, which never reaches the server). At most
    /// [`MAX_NAME_TERMS`], only when `public`.
    #[serde(default)]
    pub(crate) name_terms_b64: Vec<String>,
    /// Standard base64 of the public record (ADR-0060 section 5). Required
    /// when `public`, refused otherwise; at most [`MAX_PUBLIC_RECORD_LEN`]
    /// bytes and a well-formed introduction record (at least one term).
    #[serde(default)]
    pub(crate) public_record_b64: Option<String>,
}

/// Decode and structurally check a public record. The length is checked on
/// the base64 text before decoding so an oversized body never allocates the
/// decoded bytes.
fn decode_public_record(b64: &str) -> Result<Vec<u8>, HandlerError> {
    if b64.len() > MAX_PUBLIC_RECORD_LEN.div_ceil(3) * 4 {
        return Err(bad_request(format!(
            "public record: at most {MAX_PUBLIC_RECORD_LEN} bytes"
        )));
    }
    let bytes = B64
        .decode(b64)
        .map_err(|_| bad_request("public record: bad base64"))?;
    if bytes.len() > MAX_PUBLIC_RECORD_LEN {
        return Err(bad_request(format!(
            "public record: at most {MAX_PUBLIC_RECORD_LEN} bytes"
        )));
    }
    // Structure only (entries, distinct tags, sealed length): the payload
    // stays sealed. A record that parses carries at least one tag.
    IntroductionRecord::decode(&bytes)
        .map_err(|e| bad_request(format!("public record: {}", e.code())))?;
    Ok(bytes)
}

fn decode_hash(b64: &str) -> Result<[u8; OPRF_HASH_LEN], HandlerError> {
    let bytes = B64
        .decode(b64)
        .map_err(|_| bad_request("name term: bad base64"))?;
    bytes
        .try_into()
        .map_err(|_| bad_request(format!("name term must be {OPRF_HASH_LEN} bytes")))
}

/// `PUT /discovery/visibility` - enter or leave Everyone.
///
/// Session-gated; the account comes from the token, never the body. In one
/// transaction: sets `is_public` on the caller's username, replaces the
/// caller's name terms with the given set and stores the public record
/// (entering), or deletes both (leaving). Entering Everyone without a
/// username is a `409`: the name lookup only ever returns accounts whose
/// username is public. Entering without a public record, or leaving with
/// one, is a `400`. Idempotent.
pub(crate) async fn put_visibility(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<AuthenticatedAccount>,
    Json(req): Json<VisibilityRequest>,
) -> Result<StatusCode, HandlerError> {
    if req.name_terms_b64.len() > MAX_NAME_TERMS {
        return Err(bad_request(format!("at most {MAX_NAME_TERMS} name terms")));
    }
    if !req.public && !req.name_terms_b64.is_empty() {
        return Err(bad_request("name terms are published only in Everyone"));
    }
    let record = match (req.public, req.public_record_b64.as_deref()) {
        (true, Some(b64)) => Some(decode_public_record(b64)?),
        (true, None) => return Err(bad_request("Everyone requires a public record")),
        (false, Some(_)) => {
            return Err(bad_request("a public record is published only in Everyone"));
        },
        (false, None) => None,
    };
    let mut terms = Vec::with_capacity(req.name_terms_b64.len());
    for b64 in &req.name_terms_b64 {
        terms.push(decode_hash(b64)?);
    }
    terms.sort_unstable();
    terms.dedup();

    let account = &authed.0[..];
    let mut conn = state
        .db
        .acquire()
        .await
        .map_err(db_err("acquire visibility connection"))?;
    // READ COMMITTED for the next transaction on this connection only (the
    // DS reach update does the same). Under the default REPEATABLE READ the
    // UPDATE and DELETE below take gap locks on the `account_id` indexes, and
    // two accounts updating at once deadlocked on each other's inserts (seen
    // as an intermittent 500 when the tests ran in parallel). Updates of one
    // account need no further serialization: the last commit wins, and each
    // one replaces the whole term set.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *conn)
        .await
        .map_err(db_err("set visibility isolation"))?;
    let mut tx = sqlx::Connection::begin(&mut *conn)
        .await
        .map_err(db_err("begin visibility"))?;
    // The current name only (ADR-0076): a retired row stays hidden.
    sqlx::query("UPDATE usernames SET is_public = ? WHERE account_id = ? AND retired_at IS NULL")
        .bind(req.public)
        .bind(account)
        .execute(&mut *tx)
        .await
        .map_err(db_err("set username visibility"))?;
    if req.public {
        // `rows_affected` counts changed rows only, so an idempotent repeat
        // reads 0; ask whether a username exists instead.
        let has_username: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM usernames WHERE account_id = ? AND retired_at IS NULL LIMIT 1",
        )
        .bind(account)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err("check username"))?;
        if has_username.is_none() {
            return Err((
                StatusCode::CONFLICT,
                "register a username before choosing Everyone".to_string(),
            ));
        }
    }
    sqlx::query("DELETE FROM display_names WHERE account_id = ?")
        .bind(account)
        .execute(&mut *tx)
        .await
        .map_err(db_err("clear name terms"))?;
    if !terms.is_empty() {
        let mut q = sqlx::QueryBuilder::<sqlx::MySql>::new(
            "INSERT INTO display_names (oprf_hash, account_id) ",
        );
        q.push_values(&terms, |mut row, hash| {
            row.push_bind(hash.to_vec()).push_bind(authed.0.to_vec());
        });
        q.build()
            .execute(&mut *tx)
            .await
            .map_err(db_err("store name terms"))?;
    }
    match &record {
        Some(record) => {
            sqlx::query(
                "INSERT INTO public_records (account_id, record) VALUES (?, ?) \
                 ON DUPLICATE KEY UPDATE record = VALUES(record)",
            )
            .bind(account)
            .bind(record.as_slice())
            .execute(&mut *tx)
            .await
            .map_err(db_err("store public record"))?;
        },
        None => {
            sqlx::query("DELETE FROM public_records WHERE account_id = ?")
                .bind(account)
                .execute(&mut *tx)
                .await
                .map_err(db_err("clear public record"))?;
        },
    }
    tx.commit().await.map_err(db_err("commit visibility"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NameLookupRequest {
    /// Standard base64 of one name term's 64-byte lookup key.
    pub(crate) oprf_hash_b64: String,
}

/// One person in Everyone found by a name term.
#[derive(Serialize)]
pub(crate) struct NameLookupResult {
    /// Standard base64 of the account id.
    pub(crate) account_id_b64: String,
    /// Standard base64 of their public record; the searcher opens it with
    /// the OPRF output of the term it looked up (`openPublicRecord`).
    pub(crate) public_record_b64: String,
}

#[derive(Serialize)]
pub(crate) struct NameLookupResponse {
    /// At most [`MAX_NAME_RESULTS`], ascending by account id. Empty when
    /// nobody in Everyone has the term.
    pub(crate) results: Vec<NameLookupResult>,
}

/// `POST /discovery/name/lookup` - the accounts in Everyone that published a
/// name term. Anonymous and per-IP rate-limited like `/discovery/lookup`; its
/// enumeration step is the token-gated OPRF evaluation.
pub(crate) async fn name_lookup(
    State(state): State<AppState>,
    Json(req): Json<NameLookupRequest>,
) -> Result<Json<NameLookupResponse>, HandlerError> {
    let hash = decode_hash(&req.oprf_hash_b64)?;
    let rows: Vec<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT d.account_id, p.record FROM display_names d \
         JOIN public_records p ON p.account_id = d.account_id \
         WHERE d.oprf_hash = ? \
           AND EXISTS (SELECT 1 FROM usernames u \
                       WHERE u.account_id = d.account_id AND u.is_public = TRUE \
                         AND u.retired_at IS NULL) \
         ORDER BY d.account_id ASC LIMIT ?",
    )
    .bind(&hash[..])
    .bind(MAX_NAME_RESULTS as u64)
    .fetch_all(&state.db)
    .await
    .map_err(db_err("name lookup"))?;
    Ok(Json(NameLookupResponse {
        results: rows
            .into_iter()
            .map(|(id, record)| NameLookupResult {
                account_id_b64: B64.encode(id),
                public_record_b64: B64.encode(record),
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_attest::{verify_username_binding, UsernameCertError};

    #[test]
    fn the_public_record_column_fits_the_longest_record() {
        assert_eq!(MAX_PUBLIC_RECORD_LEN, 6744);
        let migration = include_str!("../migrations/0029_public_record_mailbox_binding.sql");
        assert!(
            migration.contains(&format!("VARBINARY({MAX_PUBLIC_RECORD_LEN})")),
            "the migration sizes public_records.record to MAX_PUBLIC_RECORD_LEN"
        );
    }

    #[test]
    fn the_caps_mirror_the_client_crate() {
        assert_eq!(MAX_NAME_TERMS, lakko_discovery_client::MAX_NAME_TERMS);
        assert_eq!(OPRF_HASH_LEN, lakko_discovery_client::OPRF_HASH_LEN);
    }

    #[test]
    fn a_signed_certificate_verifies_under_the_published_key_only() {
        let key = SigningKey::from_seed(&[9u8; 32]);
        let hash = [0x11u8; OPRF_HASH_LEN];
        let account = [0x22u8; 16];
        let cert = B64
            .decode(sign_certificate(&key, hash, account))
            .expect("base64");
        assert_eq!(cert.len(), lakko_attest::USERNAME_CERT_LEN);
        let opened =
            verify_username_binding(&cert, &key.verifying_key(), &hash, &account).expect("valid");
        assert_eq!(opened.account_id(), &account);
        assert!(opened.issued_at_unix_millis() > 0);
        assert_eq!(
            verify_username_binding(&cert, &key.verifying_key(), &hash, &[0x23u8; 16]).err(),
            Some(UsernameCertError::AccountMismatch)
        );
        assert_eq!(
            verify_username_binding(
                &cert,
                &SigningKey::from_seed(&[8u8; 32]).verifying_key(),
                &hash,
                &account
            )
            .err(),
            Some(UsernameCertError::BadSignature)
        );
    }

    #[test]
    fn the_dev_seed_override_parses_or_fails_loudly() {
        assert!(parse_cert_seed_hex("").unwrap().is_none());
        assert!(parse_cert_seed_hex("  ").unwrap().is_none());
        let key = parse_cert_seed_hex(&"ab".repeat(32)).unwrap().unwrap();
        assert_eq!(
            key.verifying_key().to_bytes(),
            SigningKey::from_seed(&[0xABu8; 32])
                .verifying_key()
                .to_bytes()
        );
        assert!(parse_cert_seed_hex("abc").is_err());
        assert!(parse_cert_seed_hex(&"zz".repeat(32)).is_err());
    }

    // ------------------------------------------------------------------
    // Routes - gated on LAKKO_DISCOVERY_TEST_DATABASE_URL, skipped without
    // ------------------------------------------------------------------

    use crate::anon_token_route_tests::{state, test_pool};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use lakko_discovery_client::discovery_lookup_key;
    use tower::ServiceExt as _;

    fn random<const N: usize>() -> [u8; N] {
        let mut out = [0u8; N];
        rand::thread_rng().fill_bytes(&mut out);
        out
    }

    struct Client {
        app: axum::Router,
        state: AppState,
    }

    impl Client {
        async fn call(
            &self,
            method: &str,
            uri: &str,
            account: Option<[u8; 16]>,
            body: Option<serde_json::Value>,
        ) -> (StatusCode, serde_json::Value, String) {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header("x-real-ip", "192.0.2.77")
                .header("content-type", "application/json");
            if let Some(account) = account {
                let token = self
                    .state
                    .session_token_secret
                    .issue(&account, 1, &[0u8; 32], 0, now_unix_millis() + 60_000)
                    .unwrap();
                b = b.header("authorization", format!("Bearer {token}"));
            }
            let body = body.map_or_else(Body::empty, |v| Body::from(v.to_string()));
            let resp = self
                .app
                .clone()
                .oneshot(b.body(body).unwrap())
                .await
                .unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            (status, json, text)
        }

        async fn register(&self, account: [u8; 16], hash: [u8; 64]) -> serde_json::Value {
            let (status, json, text) = self
                .call(
                    "POST",
                    "/discovery/register",
                    Some(account),
                    Some(serde_json::json!({
                        "account_id": B64.encode(account),
                        "oprf_hash": B64.encode(hash),
                    })),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{text}");
            json
        }

        async fn lookup(&self, hash: [u8; 64]) -> (StatusCode, String) {
            let (status, _, text) = self
                .call(
                    "POST",
                    "/discovery/lookup",
                    None,
                    Some(serde_json::json!({ "oprf_hash": B64.encode(hash) })),
                )
                .await;
            (status, text)
        }

        /// `(account_id_b64, public_record_b64)` per result.
        async fn name_lookup_records(&self, term: [u8; 64]) -> Vec<(String, String)> {
            let (status, json, text) = self
                .call(
                    "POST",
                    "/discovery/name/lookup",
                    None,
                    Some(serde_json::json!({ "oprf_hash_b64": B64.encode(term) })),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{text}");
            json["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    (
                        v["account_id_b64"].as_str().unwrap().to_owned(),
                        v["public_record_b64"].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        }

        async fn name_lookup(&self, term: [u8; 64]) -> Vec<String> {
            self.name_lookup_records(term)
                .await
                .into_iter()
                .map(|(id, _)| id)
                .collect()
        }

        /// Enter (with a public record keyed over `terms`) or leave
        /// Everyone, the way a client does.
        async fn visibility(
            &self,
            account: [u8; 16],
            public: bool,
            terms: &[[u8; 64]],
        ) -> StatusCode {
            let record = public.then(|| B64.encode(public_record(account, terms)));
            self.put_visibility(account, public, terms, record).await
        }

        async fn put_visibility(
            &self,
            account: [u8; 16],
            public: bool,
            terms: &[[u8; 64]],
            record_b64: Option<String>,
        ) -> StatusCode {
            let terms: Vec<String> = terms.iter().map(|t| B64.encode(t)).collect();
            let mut body = serde_json::json!({ "public": public, "name_terms_b64": terms });
            if let Some(record) = record_b64 {
                body["public_record_b64"] = serde_json::Value::String(record);
            }
            self.call("PUT", "/discovery/visibility", Some(account), Some(body))
                .await
                .0
        }
    }

    async fn count_rows(db: &MySqlPool, table: &str, account: [u8; 16]) -> i64 {
        let sql = match table {
            "display_names" => "SELECT COUNT(*) FROM display_names WHERE account_id = ?",
            "public_records" => "SELECT COUNT(*) FROM public_records WHERE account_id = ?",
            other => panic!("no such table in this test: {other}"),
        };
        let (rows,): (i64,) = sqlx::query_as(sql)
            .bind(&account[..])
            .fetch_one(db)
            .await
            .unwrap();
        rows
    }

    /// A public record for `account`, findable by `terms` (or by one random
    /// term when none: a person in Everyone with no name terms still has a
    /// username, whose OPRF output keys the record in a real client).
    /// `terms` are OPRF outputs, not the lookup keys the server stores.
    fn public_record(account: [u8; 16], terms: &[[u8; 64]]) -> Vec<u8> {
        use lakko_contacts::introduction::{compose_public_record, IntroductionPayload};
        let payload = IntroductionPayload {
            identity_pk: lakko_crypto::signing::SigningKey::from_seed(&[7; 32])
                .verifying_key()
                .to_bytes(),
            account_id: account,
            display_name: Some("Anna Weber".to_string()),
            username: None,
            username_certificate: None,
            mailbox_binding: lakko_contacts::mailbox::MailboxBinding::for_account_seed(&[7; 32]),
            fof: None,
        };
        let terms = if terms.is_empty() {
            vec![random::<64>()]
        } else {
            terms.to_vec()
        };
        compose_public_record(&payload, &terms, 1).expect("compose public record")
    }

    async fn client(name: &str) -> Option<Client> {
        let pool = test_pool(name).await?;
        let state = state(pool, true);
        Some(Client {
            app: crate::build_app(state.clone()),
            state,
        })
    }

    #[tokio::test]
    async fn a_hidden_username_looks_unregistered_until_everyone() {
        let Some(c) = client("a_hidden_username_looks_unregistered_until_everyone").await else {
            return;
        };
        let account = random::<16>();
        // What the client holds and what it hands the server: the record is
        // sealed under the OPRF output, the server only ever sees the lookup
        // key derived from it.
        let output = random::<64>();
        let hash = discovery_lookup_key(&output);
        let registered = c.register(account, hash).await;

        // The certificate verifies under the key the service publishes.
        let (status, key, _) = c
            .call("GET", "/.well-known/lakko-username-cert-key", None, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        let key_bytes = URL_SAFE_NO_PAD
            .decode(key["key_b64url"].as_str().unwrap())
            .unwrap();
        let key = lakko_crypto::signing::VerifyingKey::from_bytes(&key_bytes).unwrap();
        let cert = B64
            .decode(registered["username_certificate_b64"].as_str().unwrap())
            .unwrap();
        verify_username_binding(&cert, &key, &hash, &account).expect("certificate verifies");

        // Hidden: the same answer as a name nobody registered.
        let unregistered = c.lookup(random::<64>()).await;
        assert_eq!(unregistered.0, StatusCode::NOT_FOUND);
        assert_eq!(c.lookup(hash).await, unregistered);

        // Everyone: found by username and by each name term, each answer
        // carrying the public record, which opens under the OPRF output the
        // searcher used (username or term) and carries no grant.
        let term_outputs = [random::<64>(), random::<64>()];
        let terms = term_outputs.map(|o| discovery_lookup_key(&o));
        let record = public_record(account, &[output, term_outputs[0], term_outputs[1]]);
        assert_eq!(
            c.put_visibility(account, true, &terms, Some(B64.encode(&record)))
                .await,
            StatusCode::NO_CONTENT
        );
        let (status, body) = c.lookup(hash).await;
        assert_eq!(status, StatusCode::OK);
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["account_id"], B64.encode(account));
        let served = B64
            .decode(body["public_record_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(served, record);
        // The lookup key the server holds does not open it.
        assert!(
            lakko_contacts::introduction::open_public_record(&served, &hash)
                .unwrap()
                .is_none()
        );
        let opened = lakko_contacts::introduction::open_public_record(&served, &output)
            .unwrap()
            .expect("opens under the username's OPRF output");
        assert_eq!(opened.account_id, account);
        assert!(opened.fof.is_none());
        for (term, term_output) in terms.into_iter().zip(term_outputs) {
            let found = c.name_lookup_records(term).await;
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].0, B64.encode(account));
            let served = B64.decode(&found[0].1).unwrap();
            assert!(
                lakko_contacts::introduction::open_public_record(&served, &term_output)
                    .unwrap()
                    .is_some()
            );
        }
        // Idempotent, and a new set replaces the old one.
        let renamed = [random::<64>()];
        assert_eq!(
            c.visibility(account, true, &renamed).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            c.visibility(account, true, &renamed).await,
            StatusCode::NO_CONTENT
        );
        assert!(c.name_lookup(terms[0]).await.is_empty());
        assert_eq!(c.name_lookup(renamed[0]).await, vec![B64.encode(account)]);

        // Leaving Everyone: hidden again, and the terms are gone.
        assert_eq!(
            c.visibility(account, false, &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(c.lookup(hash).await, unregistered);
        assert!(c.name_lookup(renamed[0]).await.is_empty());
        for table in ["display_names", "public_records"] {
            assert_eq!(
                count_rows(&c.state.db, table, account).await,
                0,
                "{table} emptied on leaving Everyone"
            );
        }

        // The certificate can be fetched again later.
        let (status, json, _) = c
            .call(
                "GET",
                "/discovery/username-certificate",
                Some(account),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let cert = B64
            .decode(json["username_certificate_b64"].as_str().unwrap())
            .unwrap();
        verify_username_binding(&cert, &key, &hash, &account).expect("re-signed");
    }

    #[tokio::test]
    async fn visibility_updates_are_bounded_and_bound_to_the_session() {
        let Some(c) = client("visibility_updates_are_bounded_and_bound_to_the_session").await
        else {
            return;
        };
        let account = random::<16>();
        // No username yet: Everyone refused, and no certificate.
        assert_eq!(c.visibility(account, true, &[]).await, StatusCode::CONFLICT);
        let (status, _, _) = c
            .call(
                "GET",
                "/discovery/username-certificate",
                Some(account),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        c.register(account, random::<64>()).await;

        let too_many: Vec<[u8; 64]> = (0..=MAX_NAME_TERMS).map(|_| random::<64>()).collect();
        assert_eq!(
            c.visibility(account, true, &too_many).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            c.visibility(account, true, &too_many[..MAX_NAME_TERMS])
                .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            c.visibility(account, false, &[random::<64>()]).await,
            StatusCode::BAD_REQUEST
        );
        let (status, _, _) = c
            .call(
                "PUT",
                "/discovery/visibility",
                Some(account),
                Some(serde_json::json!({ "public": true, "name_terms_b64": [B64.encode([1u8; 63])] })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // The public record: required in Everyone, refused outside it,
        // parsed so garbage and oversized bodies are refused.
        let term = [random::<64>()];
        assert_eq!(
            c.put_visibility(account, true, &term, None).await,
            StatusCode::BAD_REQUEST,
            "Everyone without a public record"
        );
        assert_eq!(
            c.put_visibility(
                account,
                false,
                &[],
                Some(B64.encode(public_record(account, &term)))
            )
            .await,
            StatusCode::BAD_REQUEST,
            "a public record outside Everyone"
        );
        for garbage in [
            B64.encode([0u8; 40]),
            "not base64!".to_string(),
            B64.encode(vec![1u8; MAX_PUBLIC_RECORD_LEN + 1]),
        ] {
            assert_eq!(
                c.put_visibility(account, true, &term, Some(garbage)).await,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            c.visibility(account, true, &term).await,
            StatusCode::NO_CONTENT
        );
        // No session: refused before anything is read.
        let (status, _, _) = c
            .call(
                "PUT",
                "/discovery/visibility",
                None,
                Some(serde_json::json!({ "public": true })),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Erasure takes the terms and the public record with it.
        let (status, _, _) = c
            .call("DELETE", "/discovery/account", Some(account), None)
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        for table in ["display_names", "public_records"] {
            assert_eq!(
                count_rows(&c.state.db, table, account).await,
                0,
                "{table} erased"
            );
        }
    }

    #[tokio::test]
    async fn a_name_lookup_returns_at_most_twenty_public_accounts_in_order() {
        let Some(c) = client("a_name_lookup_returns_at_most_twenty_public_accounts_in_order").await
        else {
            return;
        };
        let term = random::<64>();
        let mut public = Vec::new();
        for _ in 0..=MAX_NAME_RESULTS {
            let account = random::<16>();
            c.register(account, random::<64>()).await;
            assert_eq!(
                c.visibility(account, true, &[term]).await,
                StatusCode::NO_CONTENT
            );
            public.push(account);
        }
        // A hidden account holding a stale row for the term never shows.
        let hidden = [0u8; 16];
        sqlx::query("INSERT IGNORE INTO display_names (oprf_hash, account_id) VALUES (?, ?)")
            .bind(&term[..])
            .bind(&hidden[..])
            .execute(&c.state.db)
            .await
            .unwrap();
        public.sort_unstable();
        let expected: Vec<String> = public
            .iter()
            .take(MAX_NAME_RESULTS)
            .map(|a| B64.encode(a))
            .collect();
        assert_eq!(c.name_lookup(term).await, expected);
    }
}
