// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The HTTP routes the canary calls, one function each.
//!
//! The JSON shapes are written here against the services' own request and
//! response structs (`services/auth/src/opaque.rs`, `anon_tokens.rs`,
//! `call_jwt.rs`, `login_handle.rs`; `services/ds/src/journal.rs`;
//! `services/storage/src/blob.rs`), not taken from
//! `lakko_auth_client::wire`, which predates the proof-of-work, login handle
//! and session fields.

use std::time::Duration;

use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use lakko_anon_token::{epoch_at, TokenContext, TokenRequest, DEFAULT_WINDOW_SECS};
use lakko_auth_client::{LoginSession, RegistrationSession};
use lakko_ds_proof::{DsProofSigner, RequestParts};
use rand::RngCore as _;
use reqwest::{Response, StatusCode};
use serde::Deserialize;
use serde_json::json;

use crate::endpoints::{blob_path, Endpoints};
use crate::error::{safe_code, Error, Result};
use crate::state::Pass;

/// Request timeout: above the 25 s DS long poll.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The long-poll wait the canary asks the DS for, seconds.
pub(crate) const LONG_POLL_WAIT_SECS: u32 = 25;

/// The HTTP client and where it sends requests.
pub(crate) struct Api {
    http: reqwest::Client,
    /// The service bases.
    pub(crate) endpoints: Endpoints,
}

/// A signed-in session (memory only).
pub(crate) struct Session {
    /// The access token.
    pub(crate) token: String,
}

/// One DS journal row.
#[derive(Debug, Deserialize)]
pub(crate) struct JournalRow {
    /// Server-assigned id.
    pub(crate) id: u64,
    /// Standard base64 of the MLS message.
    pub(crate) payload_b64: String,
}

/// One page of a DS journal read.
#[derive(Debug, Deserialize)]
pub(crate) struct JournalPage {
    /// Rows past the cursor, in order.
    pub(crate) messages: Vec<JournalRow>,
}

/// Current unix time in milliseconds.
#[must_use]
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Current unix time in seconds.
#[must_use]
pub(crate) fn now_secs() -> u64 {
    now_ms() / 1000
}

/// 16 random bytes.
#[must_use]
pub(crate) fn nonce16() -> [u8; 16] {
    let mut n = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut n);
    n
}

async fn status_error(resp: Response) -> Error {
    let status = resp.status().as_u16();
    let body: Option<serde_json::Value> = resp.json().await.ok();
    let code = body
        .as_ref()
        .and_then(|v| v.get("error").or_else(|| v.get("code")))
        .and_then(serde_json::Value::as_str);
    Error::Http {
        status,
        code: safe_code(code),
    }
}

async fn expect_json<T: for<'de> Deserialize<'de>>(resp: Response) -> Result<T> {
    if !resp.status().is_success() {
        return Err(status_error(resp).await);
    }
    resp.json::<T>().await.map_err(|_| Error::Protocol("json"))
}

async fn expect_status(resp: Response, ok: &[StatusCode]) -> Result<Response> {
    if ok.contains(&resp.status()) {
        Ok(resp)
    } else {
        Err(status_error(resp).await)
    }
}

impl Api {
    /// Build the client.
    ///
    /// # Errors
    ///
    /// [`Error::Network`] when the TLS stack cannot be set up.
    pub(crate) fn new(endpoints: Endpoints) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("lakko-canary/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| Error::Network("client"))?;
        Ok(Self { http, endpoints })
    }

    // ------------------------------------------------------------------
    // auth: account creation and sign-in
    // ------------------------------------------------------------------

    /// Create an account: proof of work, OPAQUE registration of the
    /// password and of the recovery seed, then the session.
    ///
    /// # Errors
    ///
    /// Any HTTP, OPAQUE or proof-of-work failure.
    pub(crate) async fn signup(
        &self,
        account_id: &[u8; 16],
        password: &[u8],
        recovery_seed: &[u8],
    ) -> Result<Session> {
        #[derive(Deserialize)]
        struct Challenge {
            seed: String,
            effort_bits: u8,
        }
        #[derive(Deserialize)]
        struct StartRes {
            password_response: String,
            recovery_response: Option<String>,
        }
        #[derive(Deserialize)]
        struct FinishRes {
            session_token: String,
        }
        let ch: Challenge = expect_json(
            self.http
                .post(self.endpoints.auth_url("/pow/challenge"))
                .send()
                .await?,
        )
        .await?;
        let seed: [u8; 16] = B64
            .decode(&ch.seed)
            .ok()
            .and_then(|v| <[u8; 16]>::try_from(v.as_slice()).ok())
            .ok_or(Error::Protocol("pow-seed"))?;
        let bits = ch.effort_bits;
        let proof = tokio::task::spawn_blocking(move || lakko_pow::solve(&seed, bits))
            .await
            .map_err(|_| Error::Crypto("pow-task"))?
            .map_err(|_| Error::Crypto("pow"))?;

        let (password_reg, password_req_b64) =
            RegistrationSession::start_password(password).map_err(|_| Error::Crypto("opaque"))?;
        let (recovery_reg, recovery_req_b64) = RegistrationSession::start_recovery(recovery_seed)
            .map_err(|_| Error::Crypto("opaque"))?;
        let account_b64 = B64.encode(account_id);
        let start: StartRes = expect_json(
            self.http
                .post(self.endpoints.auth_url("/signup/start"))
                .json(&json!({
                    "pow": {
                        "seed": B64.encode(proof.seed),
                        "nonce": B64.encode(proof.nonce),
                        "solution": B64.encode(proof.solution),
                    },
                    "account_id": account_b64,
                    "password_request": password_req_b64,
                    "recovery_request": recovery_req_b64,
                }))
                .send()
                .await?,
        )
        .await?;
        let recovery_response = start
            .recovery_response
            .ok_or(Error::Protocol("recovery-response"))?;
        let pw_up = password_reg
            .finish(password, &start.password_response)
            .map_err(|_| Error::Crypto("opaque"))?;
        let rc_up = recovery_reg
            .finish(recovery_seed, &recovery_response)
            .map_err(|_| Error::Crypto("opaque"))?;
        let fin: FinishRes = expect_json(
            self.http
                .post(self.endpoints.auth_url("/signup/finish"))
                .json(&json!({
                    "account_id": account_b64,
                    "password_upload": pw_up.upload_b64,
                    "recovery_upload": rc_up.upload_b64,
                }))
                .send()
                .await?,
        )
        .await?;
        Ok(Session {
            token: fin.session_token,
        })
    }

    /// Bind the account's login handle (first bind: no fresh grant).
    ///
    /// # Errors
    ///
    /// Any HTTP failure.
    pub(crate) async fn bind_handle(&self, session: &Session, handle: &[u8; 32]) -> Result<()> {
        expect_status(
            self.http
                .put(self.endpoints.auth_url("/auth/login-handle"))
                .bearer_auth(&session.token)
                .json(&json!({ "handle": B64URL.encode(handle) }))
                .send()
                .await?,
            &[StatusCode::OK],
        )
        .await?;
        Ok(())
    }

    /// OPAQUE password login by login handle.
    ///
    /// # Errors
    ///
    /// Any HTTP or OPAQUE failure.
    pub(crate) async fn login(&self, handle: &[u8; 32], password: &[u8]) -> Result<Session> {
        #[derive(Deserialize)]
        struct StartRes {
            session_id: String,
            response: String,
        }
        #[derive(Deserialize)]
        struct FinishRes {
            session_token: String,
        }
        let (login, request) =
            LoginSession::start_password(password).map_err(|_| Error::Crypto("opaque"))?;
        let start: StartRes = expect_json(
            self.http
                .post(self.endpoints.auth_url("/login/start"))
                .json(&json!({ "handle": B64URL.encode(handle), "request": request }))
                .send()
                .await?,
        )
        .await?;
        let out = login
            .finish(password, &start.response)
            .map_err(|_| Error::Crypto("opaque-login"))?;
        let fin: FinishRes = expect_json(
            self.http
                .post(self.endpoints.auth_url("/login/finish"))
                .json(&json!({ "session_id": start.session_id, "finalize": out.finalize_b64 }))
                .send()
                .await?,
        )
        .await?;
        Ok(Session {
            token: fin.session_token,
        })
    }

    // ------------------------------------------------------------------
    // auth: anonymous tokens and call admission
    // ------------------------------------------------------------------

    /// Obtain exactly one anonymous token for `context`, as the header value
    /// a spend carries.
    ///
    /// # Errors
    ///
    /// Any HTTP or token failure (quota exhausted included).
    pub(crate) async fn anon_token(
        &self,
        session: &Session,
        context: TokenContext,
    ) -> Result<String> {
        let epoch =
            epoch_at(now_secs(), DEFAULT_WINDOW_SECS).map_err(|_| Error::Crypto("epoch"))?;
        match self.anon_token_at(session, context, epoch).await? {
            Ok(header) => Ok(header),
            // The issuer's clock is in another epoch: one retry at the epoch
            // it names.
            Err(current) => match self.anon_token_at(session, context, current).await? {
                Ok(header) => Ok(header),
                Err(_) => Err(Error::Protocol("token-epoch")),
            },
        }
    }

    /// One issuance at `epoch`. `Ok(Err(current))` is the issuer's
    /// `epoch-not-issuable` answer naming its current epoch.
    async fn anon_token_at(
        &self,
        session: &Session,
        context: TokenContext,
        epoch: u64,
    ) -> Result<std::result::Result<String, u64>> {
        #[derive(Deserialize)]
        struct IssueRes {
            epoch: u64,
            evaluations: Vec<String>,
            proof: String,
        }
        #[derive(Deserialize)]
        struct KeyRes {
            public_key: String,
        }
        let req = TokenRequest::new(context, epoch, 1).map_err(|_| Error::Crypto("token-req"))?;
        let resp = self
            .http
            .post(self.endpoints.auth_url("/auth/tokens"))
            .bearer_auth(&session.token)
            .json(&json!({
                "context": context.name(),
                "epoch": epoch,
                "blinded_elements": req.blinded_elements_b64url(),
            }))
            .send()
            .await?;
        if resp.status() == StatusCode::BAD_REQUEST {
            let body: serde_json::Value = resp.json().await.map_err(|_| Error::Protocol("json"))?;
            if body.get("error").and_then(serde_json::Value::as_str) == Some("epoch-not-issuable") {
                if let Some(current) = body
                    .get("current_epoch")
                    .and_then(serde_json::Value::as_u64)
                {
                    return Ok(Err(current));
                }
            }
            return Err(Error::Http {
                status: 400,
                code: safe_code(body.get("error").and_then(serde_json::Value::as_str)),
            });
        }
        let issued: IssueRes = expect_json(resp).await?;
        if issued.epoch != epoch {
            return Err(Error::Protocol("token-epoch"));
        }
        let key: KeyRes = expect_json(
            self.http
                .get(
                    self.endpoints
                        .auth_url(&format!("/auth/tokens/key/{}/{epoch}", context.name())),
                )
                .send()
                .await?,
        )
        .await?;
        let tokens = req
            .finalize_b64url(&issued.evaluations, &issued.proof, &key.public_key)
            .map_err(|_| Error::Crypto("token-finalize"))?;
        tokens
            .first()
            .map(lakko_anon_token::Token::to_header_value)
            .map(Ok)
            .ok_or(Error::Protocol("token-count"))
    }

    /// Mint a call-admission JWT for `room` with one `call-join` token.
    ///
    /// # Errors
    ///
    /// Any HTTP failure.
    pub(crate) async fn call_jwt(&self, token_header: &str, room_hex: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct JwtRes {
            jwt: String,
        }
        let res: JwtRes = expect_json(
            self.http
                .post(self.endpoints.auth_url("/call/jwt"))
                .header(lakko_anon_token::TOKEN_HEADER, token_header)
                .json(&json!({ "room": room_hex, "role": "participant" }))
                .send()
                .await?,
        )
        .await?;
        Ok(res.jwt)
    }

    // ------------------------------------------------------------------
    // ds
    // ------------------------------------------------------------------

    /// Claim a group journal with a signed root statement (expects 204).
    ///
    /// # Errors
    ///
    /// Any HTTP failure.
    pub(crate) async fn ds_claim(&self, service_path: &str, body_json: String) -> Result<()> {
        expect_status(
            self.http
                .put(self.endpoints.ds_url(service_path, ""))
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body_json)
                .send()
                .await?,
            &[StatusCode::NO_CONTENT],
        )
        .await?;
        Ok(())
    }

    /// Publish an MLS message to a group journal; returns the row id.
    ///
    /// # Errors
    ///
    /// Any HTTP failure.
    pub(crate) async fn ds_publish(
        &self,
        signer: &DsProofSigner,
        service_path: &str,
        body: Vec<u8>,
    ) -> Result<u64> {
        #[derive(Deserialize)]
        struct PubRes {
            id: u64,
        }
        let proof = signer
            .sign_at(
                &RequestParts {
                    method: "POST",
                    canonical_path: service_path,
                    query: "",
                    body: &body,
                },
                now_ms(),
                nonce16(),
            )
            .encode();
        let res: PubRes = expect_json(
            self.http
                .post(self.endpoints.ds_url(service_path, ""))
                .header(lakko_ds_proof::PROOF_HEADER_NAME, proof)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(body)
                .send()
                .await?,
        )
        .await?;
        Ok(res.id)
    }

    /// Read a group journal after `after`, long-polling up to `wait` seconds.
    ///
    /// # Errors
    ///
    /// Any HTTP failure.
    pub(crate) async fn ds_fetch(
        &self,
        signer: &DsProofSigner,
        service_path: &str,
        after: u64,
        wait: u32,
    ) -> Result<JournalPage> {
        let query = format!("after={after}&limit=100&wait={wait}");
        let proof = signer
            .sign_at(
                &RequestParts {
                    method: "GET",
                    canonical_path: service_path,
                    query: &query,
                    body: &[],
                },
                now_ms(),
                nonce16(),
            )
            .encode();
        expect_json(
            self.http
                .get(self.endpoints.ds_url(service_path, &query))
                .header(lakko_ds_proof::PROOF_HEADER_NAME, proof)
                .send()
                .await?,
        )
        .await
    }

    // ------------------------------------------------------------------
    // storage
    // ------------------------------------------------------------------

    /// Buy a storage pass with one token (`upload` or `read`).
    ///
    /// # Errors
    ///
    /// Any HTTP failure.
    pub(crate) async fn blob_pass(&self, kind: &str, token_header: &str) -> Result<Pass> {
        #[derive(Deserialize)]
        struct PassRes {
            pass: String,
            expires_at_unix_secs: u64,
        }
        let res: PassRes = expect_json(
            self.http
                .post(self.endpoints.storage_url(&format!("/blob/pass/{kind}")))
                .header(lakko_anon_token::TOKEN_HEADER, token_header)
                .send()
                .await?,
        )
        .await?;
        Ok(Pass {
            value: res.pass,
            expires_at_unix: res.expires_at_unix_secs,
        })
    }

    /// Upload a ciphertext blob (idle life one day).
    ///
    /// # Errors
    ///
    /// Any HTTP failure.
    pub(crate) async fn blob_put(
        &self,
        pass: &str,
        blob_id: &[u8; 32],
        delete_public_key: &[u8; 32],
        ciphertext: Vec<u8>,
    ) -> Result<()> {
        expect_status(
            self.http
                .put(self.endpoints.storage_url(&blob_path(blob_id)))
                .header("x-lakko-blob-pass", pass)
                .header("x-lakko-blob-delete-key", B64URL.encode(delete_public_key))
                .header("x-lakko-blob-idle-days", "1")
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(ciphertext)
                .send()
                .await?,
            &[StatusCode::CREATED, StatusCode::OK],
        )
        .await?;
        Ok(())
    }

    /// Download a blob.
    ///
    /// # Errors
    ///
    /// Any HTTP failure.
    pub(crate) async fn blob_get(&self, pass: &str, blob_id: &[u8; 32]) -> Result<Vec<u8>> {
        let resp = expect_status(
            self.http
                .get(self.endpoints.storage_url(&blob_path(blob_id)))
                .header("x-lakko-blob-pass", pass)
                .send()
                .await?,
            &[StatusCode::OK],
        )
        .await?;
        Ok(resp.bytes().await?.to_vec())
    }

    /// Delete a blob with the owner's storage proof (expects 204).
    ///
    /// # Errors
    ///
    /// Any HTTP or signing failure.
    pub(crate) async fn blob_delete(
        &self,
        delete_seed: &[u8; 32],
        blob_id: &[u8; 32],
    ) -> Result<()> {
        let path = blob_path(blob_id);
        let proof = lakko_vault::proof::sign(delete_seed, "DELETE", &path, &[], now_ms())
            .map_err(|_| Error::Crypto("storage-proof"))?;
        expect_status(
            self.http
                .delete(self.endpoints.storage_url(&path))
                .header(lakko_vault::proof::PROOF_HEADER_NAME, proof)
                .send()
                .await?,
            &[StatusCode::NO_CONTENT],
        )
        .await?;
        Ok(())
    }
}
