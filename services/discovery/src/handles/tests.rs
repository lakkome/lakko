// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Route tests of ADR-0087 sections 1 to 4, through the production router.
//! The pure checks run everywhere; the routes are gated on
//! `LAKKO_DISCOVERY_TEST_DATABASE_URL` and skip without it.

use super::*;
use crate::anon_token_route_tests::{state, test_pool};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt as _;
use lakko_anon_token::{MasterSecret, TokenContext, DEFAULT_WINDOW_SECS, TOKEN_HEADER};
use lakko_anon_token_server::current_epoch;
use lakko_anon_token_server::testing::mint_header;
use lakko_attest::verify_handle_binding;
use lakko_contacts::handle::{handle_write_public_key, HANDLE_NONCE_LEN};
use lakko_contacts::handle_record::{
    compose_handle_record, open_search_record, HandleRecordKind, HandleRecordPayload, SearchRecord,
};
use lakko_discovery_client::discovery_lookup_key;
use serde_json::{json, Value};
use tower::ServiceExt as _;

const MASTER_HEX: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::thread_rng().fill_bytes(&mut out);
    out
}

#[test]
fn the_certificate_lengths_agree_across_crates() {
    assert_eq!(
        lakko_attest::HANDLE_CERT_LEN,
        lakko_contacts::conversation_handle::CONVERSATION_HANDLE_CERT_LEN
    );
    assert_eq!(
        lakko_attest::USERNAME_CERT_LEN,
        lakko_contacts::profile::USERNAME_CERT_LEN
    );
}

#[test]
fn the_lease_is_180_days() {
    assert_eq!(LEASE_SECS, 15_552_000);
}

/// A group, as its owner's devices see it: the handle seed, the OPRF output
/// of its name (which the server never sees) and its lookup key.
struct Group {
    seed: [u8; 32],
    name_output: [u8; 64],
    term_output: [u8; 64],
    /// Composed once: a record is sealed under fresh randomness, and the
    /// statement's content hash covers these exact bytes.
    record: std::cell::OnceCell<Vec<u8>>,
}

impl Group {
    fn new() -> Self {
        Self {
            seed: random(),
            name_output: random(),
            term_output: random(),
            record: std::cell::OnceCell::new(),
        }
    }

    fn lookup_key(&self) -> [u8; 64] {
        discovery_lookup_key(&self.name_output)
    }

    fn term_key(&self) -> [u8; 64] {
        discovery_lookup_key(&self.term_output)
    }

    fn write_pk(&self) -> [u8; 32] {
        handle_write_public_key(&self.seed)
    }

    fn record(&self) -> Vec<u8> {
        self.record.get_or_init(|| self.compose_record()).clone()
    }

    fn compose_record(&self) -> Vec<u8> {
        let payload = HandleRecordPayload {
            kind: HandleRecordKind::Group,
            updated_at_ms: 1,
            id: vec![0x33; 32],
            key: [0x44; 32],
            handle: "hiking_club".to_string(),
            name: "Hiking club".to_string(),
            description: String::new(),
            size_band: 1,
        };
        compose_handle_record(&payload, &[self.name_output, self.term_output], 1).unwrap()
    }

    /// The statement and the body of a write.
    fn body(&self, lookup_key: [u8; 64], body: HandleStatementBody, publish: bool) -> Value {
        self.body_at(lookup_key, body, publish, now_ms())
    }

    fn body_at(
        &self,
        lookup_key: [u8; 64],
        body: HandleStatementBody,
        publish: bool,
        at_ms: u64,
    ) -> Value {
        let statement = HandleStatement::sign(
            &self.seed,
            lookup_key,
            body,
            at_ms,
            random::<HANDLE_NONCE_LEN>(),
        );
        let mut v = json!({ "statement_b64": B64.encode(statement.encode()) });
        if publish {
            v["public_record_b64"] = json!(B64.encode(self.record()));
            v["terms_b64"] = json!([B64.encode(self.term_key())]);
        }
        v
    }

    fn content_hash(&self, public: bool) -> [u8; 32] {
        if public {
            handle_content_hash(Some(&self.record()), &[self.term_key()]).unwrap()
        } else {
            handle_content_hash(None, &[]).unwrap()
        }
    }

    fn claim_body(&self, public: bool) -> Value {
        let visibility = if public {
            HandleVisibility::Public
        } else {
            HandleVisibility::Private
        };
        self.body(
            self.lookup_key(),
            HandleStatementBody::Claim {
                visibility,
                content_hash: self.content_hash(public),
            },
            public,
        )
    }

    fn update_body(&self, lookup_key: [u8; 64], public: bool) -> Value {
        let visibility = if public {
            HandleVisibility::Public
        } else {
            HandleVisibility::Private
        };
        self.body(
            lookup_key,
            HandleStatementBody::Update {
                visibility,
                content_hash: self.content_hash(public),
            },
            public,
        )
    }
}

struct Client {
    app: axum::Router,
    state: AppState,
    master: MasterSecret,
}

impl Client {
    async fn new(name: &str) -> Option<Self> {
        let pool = test_pool(name).await?;
        let state = state(pool, true);
        Some(Self {
            app: crate::build_app(state.clone()),
            state,
            master: MasterSecret::from_hex(MASTER_HEX).unwrap(),
        })
    }

    async fn call(&self, method: &str, uri: &str, token: bool, body: Value) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-real-ip", "192.0.2.92")
            .header("content-type", "application/json");
        if token {
            b = b.header(
                TOKEN_HEADER,
                mint_header(
                    &self.master,
                    TokenContext::DiscoveryRegister,
                    current_epoch(DEFAULT_WINDOW_SECS),
                ),
            );
        }
        let resp = self
            .app
            .clone()
            .oneshot(b.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn claim(&self, body: Value) -> (StatusCode, Value) {
        self.call("POST", "/discovery/handle/claim", true, body)
            .await
    }

    async fn lookup(&self, key: [u8; 64]) -> (StatusCode, Value) {
        self.call(
            "POST",
            "/discovery/lookup",
            false,
            json!({ "oprf_hash": B64.encode(key) }),
        )
        .await
    }

    async fn name_lookup(&self, term: [u8; 64]) -> Vec<Value> {
        let (status, v) = self
            .call(
                "POST",
                "/discovery/name/lookup",
                false,
                json!({ "oprf_hash_b64": B64.encode(term) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["results"].as_array().unwrap().clone()
    }

    /// Register a username for a fresh account through the real route.
    async fn register_username(&self, key: [u8; 64]) -> (StatusCode, Value) {
        let account = random::<16>();
        let token = self
            .state
            .session_token_secret
            .issue(
                &account,
                1,
                &[0u8; 32],
                0,
                i64::try_from(now_ms()).unwrap() + 60_000,
            )
            .unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/discovery/register")
            .header("x-real-ip", "192.0.2.93")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(
                json!({ "account_id": B64.encode(account), "oprf_hash": B64.encode(key) })
                    .to_string(),
            ))
            .unwrap();
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn certified(&self, answer: &Value, lookup_key: [u8; 64]) -> [u8; 32] {
        let cert = B64
            .decode(answer["handle_certificate_b64"].as_str().unwrap())
            .unwrap();
        *verify_handle_binding(
            &cert,
            &self.state.username_cert_key.verifying_key(),
            &lookup_key,
        )
        .expect("certificate verifies")
        .write_pk()
    }

    async fn handle_rows(&self, write_pk: [u8; 32]) -> Vec<(Vec<u8>, bool, bool)> {
        sqlx::query_as(
            "SELECT lookup_key, retired_at IS NOT NULL, is_public FROM handles \
             WHERE write_pk = ? ORDER BY lookup_key",
        )
        .bind(&write_pk[..])
        .fetch_all(&self.state.db)
        .await
        .unwrap()
    }
}

fn code(v: &Value) -> &str {
    v["error"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn a_public_group_is_found_by_name_and_term_and_a_private_one_by_nothing() {
    let Some(c) = Client::new("handles_public_and_private").await else {
        return;
    };
    let public = Group::new();
    let (status, answer) = c.claim(public.claim_body(true)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(c.certified(&answer, public.lookup_key()), public.write_pk());

    // By @name: the record, no account.
    let (status, found) = c.lookup(public.lookup_key()).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["kind"], "handle");
    assert!(found.get("account_id").is_none());
    let record = B64
        .decode(found["public_record_b64"].as_str().unwrap())
        .unwrap();
    assert!(matches!(
        open_search_record(&record, &public.name_output).unwrap(),
        Some(SearchRecord::Conversation(_))
    ));
    // By name term.
    let hits = c.name_lookup(public.term_key()).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["kind"], "handle");

    // A private name reserves and answers nothing: the same 404 as absent.
    let private = Group::new();
    let (status, _) = c.claim(private.claim_body(false)).await;
    assert_eq!(status, StatusCode::OK);
    let absent = c.lookup(random()).await;
    assert_eq!(absent.0, StatusCode::NOT_FOUND);
    assert_eq!(c.lookup(private.lookup_key()).await, absent);
    for sql in [
        "SELECT COUNT(*) FROM public_records t JOIN handles h \
         ON t.kind = 2 AND t.target = h.handle_id WHERE h.lookup_key = ?",
        "SELECT COUNT(*) FROM search_terms t JOIN handles h \
         ON t.kind = 2 AND t.target = h.handle_id WHERE h.lookup_key = ?",
    ] {
        let (n,): (i64,) = sqlx::query_as(sql)
            .bind(&private.lookup_key()[..])
            .fetch_one(&c.state.db)
            .await
            .unwrap();
        assert_eq!(n, 0, "{sql}");
    }

    // Flip the public one private: it stops answering at once.
    let (status, _) = c
        .call(
            "PUT",
            "/discovery/handle",
            false,
            public.update_body(public.lookup_key(), false),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(c.lookup(public.lookup_key()).await.0, StatusCode::NOT_FOUND);
    assert!(c.name_lookup(public.term_key()).await.is_empty());
    // And back.
    let (status, _) = c
        .call(
            "PUT",
            "/discovery/handle",
            false,
            public.update_body(public.lookup_key(), true),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(c.lookup(public.lookup_key()).await.0, StatusCode::OK);
}

#[tokio::test]
async fn one_namespace_holds_people_groups_and_channels() {
    let Some(c) = Client::new("handles_one_namespace").await else {
        return;
    };
    // A group takes a name; a person cannot register it.
    let group = Group::new();
    assert_eq!(c.claim(group.claim_body(false)).await.0, StatusCode::OK);
    let (status, body) = c.register_username(group.lookup_key()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&body), "name-taken");

    // A person takes a name; a group cannot claim it.
    let person_name = random::<64>();
    assert_eq!(c.register_username(person_name).await.0, StatusCode::OK);
    let mut squatter = Group::new();
    squatter.name_output = [0; 64];
    let body = squatter.body(
        person_name,
        HandleStatementBody::Claim {
            visibility: HandleVisibility::Private,
            content_hash: handle_content_hash(None, &[]).unwrap(),
        },
        false,
    );
    let (status, answer) = c.claim(body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&answer), "name-taken");

    // Another group cannot claim a held group name either, and the same key
    // cannot hold two current names.
    let rival = Group::new();
    let body = rival.body(
        group.lookup_key(),
        HandleStatementBody::Claim {
            visibility: HandleVisibility::Private,
            content_hash: handle_content_hash(None, &[]).unwrap(),
        },
        false,
    );
    let (status, answer) = c.claim(body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&answer), "name-taken");
    let mut second = Group::new();
    second.seed = group.seed;
    let (status, answer) = c.claim(second.claim_body(false)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&answer), "handle-already-set");
}

#[tokio::test]
async fn statements_are_checked_before_any_row_is_touched() {
    let Some(c) = Client::new("handles_statement_checks").await else {
        return;
    };
    let group = Group::new();
    // No token: the gate refuses before the handler.
    let (status, _) = c
        .call(
            "POST",
            "/discovery/handle/claim",
            false,
            group.claim_body(false),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Outside the window.
    let old = group.body_at(
        group.lookup_key(),
        HandleStatementBody::Claim {
            visibility: HandleVisibility::Private,
            content_hash: handle_content_hash(None, &[]).unwrap(),
        },
        false,
        now_ms() - 10 * HANDLE_STATEMENT_WINDOW_MS,
    );
    let (status, body) = c.claim(old).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&body), "statement-refused");
    // The wrong kind for the route.
    let (status, _) = c.claim(group.update_body(group.lookup_key(), false)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Content that the statement does not cover.
    let mut body = group.claim_body(true);
    body["terms_b64"] = json!([B64.encode(random::<64>())]);
    assert_eq!(c.claim(body).await.0, StatusCode::BAD_REQUEST);
    // A public claim without a record; a private one with terms.
    let mut body = group.claim_body(true);
    body.as_object_mut().unwrap().remove("public_record_b64");
    assert_eq!(c.claim(body).await.0, StatusCode::BAD_REQUEST);
    let mut body = group.claim_body(false);
    body["terms_b64"] = json!([B64.encode(random::<64>())]);
    assert_eq!(c.claim(body).await.0, StatusCode::BAD_REQUEST);
    // Nothing was claimed by any of these.
    assert!(c.handle_rows(group.write_pk()).await.is_empty());

    // A replayed statement is refused.
    let body = group.claim_body(false);
    assert_eq!(c.claim(body.clone()).await.0, StatusCode::OK);
    let (status, answer) = c
        .call("PUT", "/discovery/handle", false, {
            let mut b = group.update_body(group.lookup_key(), false);
            b["statement_b64"] = body["statement_b64"].clone();
            b
        })
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    let update = group.update_body(group.lookup_key(), false);
    assert_eq!(
        c.call("PUT", "/discovery/handle", false, update.clone())
            .await
            .0,
        StatusCode::OK
    );
    let (status, answer) = c.call("PUT", "/discovery/handle", false, update).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&answer), "statement-refused");

    // Another key cannot write the row.
    let mut thief = Group::new();
    thief.name_output = group.name_output;
    let (status, answer) = c
        .call(
            "PUT",
            "/discovery/handle",
            false,
            thief.update_body(group.lookup_key(), false),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&answer), "statement-refused");
}

#[tokio::test]
async fn a_replace_retires_and_holds_the_old_name_and_frees_earlier_ones() {
    let Some(c) = Client::new("handles_replace").await else {
        return;
    };
    let group = Group::new();
    assert_eq!(c.claim(group.claim_body(true)).await.0, StatusCode::OK);
    let replace = |to: [u8; 64], public: bool| {
        let visibility = if public {
            HandleVisibility::Public
        } else {
            HandleVisibility::Private
        };
        group.body(
            group.lookup_key(),
            HandleStatementBody::Replace {
                new_lookup_key: to,
                visibility,
                content_hash: group.content_hash(public),
            },
            public,
        )
    };
    let second = random::<64>();
    let (status, answer) = c
        .call(
            "POST",
            "/discovery/handle/replace",
            true,
            replace(second, false),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(c.certified(&answer, second), group.write_pk());
    // The old name answers nothing and is held: nobody else can take it.
    assert_eq!(c.lookup(group.lookup_key()).await.0, StatusCode::NOT_FOUND);
    assert!(c.name_lookup(group.term_key()).await.is_empty());
    let (status, body) = c.register_username(group.lookup_key()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&body), "name-taken");
    let rows = c.handle_rows(group.write_pk()).await;
    assert_eq!(rows.len(), 2);

    // Moving again (from the second name) frees the first at once.
    let third = random::<64>();
    let body = group.body(
        second,
        HandleStatementBody::Replace {
            new_lookup_key: third,
            visibility: HandleVisibility::Private,
            content_hash: group.content_hash(false),
        },
        false,
    );
    let (status, _) = c
        .call("POST", "/discovery/handle/replace", true, body)
        .await;
    assert_eq!(status, StatusCode::OK);
    let rows = c.handle_rows(group.write_pk()).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(
        c.register_username(group.lookup_key()).await.0,
        StatusCode::OK
    );

    // Retire: answered 204, held.
    let body = group.body(third, HandleStatementBody::Retire, false);
    let (status, _) = c.call("DELETE", "/discovery/handle", false, body).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(c
        .handle_rows(group.write_pk())
        .await
        .iter()
        .all(|(_, retired, _)| *retired));
    // The key takes it back during the hold.
    let (status, _) = c
        .call(
            "PUT",
            "/discovery/handle",
            false,
            group.update_body(third, false),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_rekey_moves_every_row_to_the_new_key() {
    let Some(c) = Client::new("handles_rekey").await else {
        return;
    };
    let group = Group::new();
    assert_eq!(c.claim(group.claim_body(false)).await.0, StatusCode::OK);
    let next = Group::new();
    let body = group.body(
        group.lookup_key(),
        HandleStatementBody::Rekey {
            new_write_pk: next.write_pk(),
        },
        false,
    );
    let (status, answer) = c.call("POST", "/discovery/handle/rekey", false, body).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(c.certified(&answer, group.lookup_key()), next.write_pk());
    assert!(answer["lease_until_ms"].as_u64().unwrap() > now_ms());
    assert!(c.handle_rows(group.write_pk()).await.is_empty());
    assert_eq!(c.handle_rows(next.write_pk()).await.len(), 1);
    // The old key no longer writes; the new one does.
    let (status, _) = c
        .call(
            "PUT",
            "/discovery/handle",
            false,
            group.update_body(group.lookup_key(), false),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let mut moved = next;
    moved.name_output = group.name_output;
    let (status, _) = c
        .call(
            "PUT",
            "/discovery/handle",
            false,
            moved.update_body(group.lookup_key(), false),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_lapsed_lease_stops_answering_and_is_freed_after_the_hold() {
    let Some(c) = Client::new("handles_lease").await else {
        return;
    };
    let group = Group::new();
    assert_eq!(c.claim(group.claim_body(true)).await.0, StatusCode::OK);
    assert_eq!(c.lookup(group.lookup_key()).await.0, StatusCode::OK);
    // The lease passes: no answer, before the pruner runs.
    sqlx::query("UPDATE handles SET lease_until = NOW(3) - INTERVAL 1 SECOND WHERE lookup_key = ?")
        .bind(&group.lookup_key()[..])
        .execute(&c.state.db)
        .await
        .unwrap();
    assert_eq!(c.lookup(group.lookup_key()).await.0, StatusCode::NOT_FOUND);
    assert!(c.name_lookup(group.term_key()).await.is_empty());
    let hold = crate::registry::NamePolicy::defaults().retired_hold_secs;
    prune(&c.state.db, hold).await.unwrap();
    let rows = c.handle_rows(group.write_pk()).await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].1, "retired by the pruner");
    // Still held.
    assert_eq!(
        c.register_username(group.lookup_key()).await.0,
        StatusCode::CONFLICT
    );
    // Past the hold: freed, claim and all.
    sqlx::query(
        "UPDATE handles SET retired_at = retired_at - INTERVAL ? SECOND WHERE lookup_key = ?",
    )
    .bind(i64::try_from(hold + 60).unwrap())
    .bind(&group.lookup_key()[..])
    .execute(&c.state.db)
    .await
    .unwrap();
    prune(&c.state.db, hold).await.unwrap();
    assert!(c.handle_rows(group.write_pk()).await.is_empty());
    assert_eq!(
        c.register_username(group.lookup_key()).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_claim_spends_a_discovery_register_token() {
    let Some(c) = Client::new("handles_claim_token").await else {
        return;
    };
    let group = Group::new();
    let wrong = mint_header(
        &c.master,
        TokenContext::DiscoveryLookup,
        current_epoch(DEFAULT_WINDOW_SECS),
    );
    let req = Request::builder()
        .method("POST")
        .uri("/discovery/handle/claim")
        .header("x-real-ip", "192.0.2.94")
        .header("content-type", "application/json")
        .header(TOKEN_HEADER, wrong)
        .body(Body::from(group.claim_body(false).to_string()))
        .unwrap();
    let resp = c.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(c.claim(group.claim_body(false)).await.0, StatusCode::OK);
}

/// A transaction that met a deadlock is run again, at most
/// `LOCK_ATTEMPTS` times; any other refusal is returned at once.
#[tokio::test]
async fn a_lock_conflict_is_retried_and_nothing_else_is() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn conflict() -> Response {
        let mut resp = StatusCode::INTERNAL_SERVER_ERROR.into_response();
        resp.extensions_mut().insert(super::LockConflict);
        resp
    }

    let runs = AtomicUsize::new(0);
    let got = super::retry_on_lock_conflict(|| async {
        if runs.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(conflict())
        } else {
            Ok(7)
        }
    })
    .await;
    assert_eq!(got.ok(), Some(7));
    assert_eq!(runs.load(Ordering::SeqCst), 2);

    let runs = AtomicUsize::new(0);
    let got: Result<(), Response> = super::retry_on_lock_conflict(|| async {
        runs.fetch_add(1, Ordering::SeqCst);
        Err(conflict())
    })
    .await;
    assert_eq!(got.unwrap_err().status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(runs.load(Ordering::SeqCst), super::LOCK_ATTEMPTS);

    let runs = AtomicUsize::new(0);
    let got: Result<(), Response> = super::retry_on_lock_conflict(|| async {
        runs.fetch_add(1, Ordering::SeqCst);
        Err(super::refused())
    })
    .await;
    assert_eq!(got.unwrap_err().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}
