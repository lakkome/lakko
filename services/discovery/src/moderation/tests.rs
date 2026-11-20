// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tests of ADR-0098 step 5 at discovery: the internal statement route, its
//! effect on the public routes, and the panel read.
//!
//! The pure checks run everywhere; the routes are gated on
//! `LAKKO_DISCOVERY_TEST_DATABASE_URL` and skip without it. Statements are
//! signed by a software Ed25519 security key
//! (`lakko_moderation::software_authenticator`) under a moderator list signed
//! by a test root.

use super::*;
use crate::anon_token_route_tests::{state, test_pool};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt as _;
use lakko_contacts::handle::{
    handle_content_hash, handle_write_public_key, HandleStatement, HandleStatementBody,
    HandleVisibility, HANDLE_NONCE_LEN,
};
use lakko_contacts::handle_record::{compose_handle_record, HandleRecordKind, HandleRecordPayload};
use lakko_crypto::signing::SigningKey;
use lakko_discovery_client::discovery_lookup_key;
use lakko_moderation::moderators::{ModeratorEntry, ModeratorList};
use lakko_moderation::software_authenticator::SoftwareAuthenticator;
use lakko_moderation::statement::{reason_commitment, Statement};
use lakko_moderation::target::{Target, TargetKind};
use lakko_moderation::Reason;
use rand::RngCore as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;

const ROOT_SEED: [u8; 32] = [0x42; 32];
const MODERATOR_SEED: [u8; 32] = [0x17; 32];
const SECRET: [u8; 32] = [0x5E; 32];
/// The list version every test signs under. The stored version only grows
/// and tests share one database, so every test uses the highest there is.
const V: u64 = u64::MAX;

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::thread_rng().fill_bytes(&mut out);
    out
}

fn root_public() -> [u8; 32] {
    SigningKey::from_seed(&ROOT_SEED).verifying_key().to_bytes()
}

fn config() -> InternalConfig {
    InternalConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        secret: Arc::new(SECRET.to_vec()),
        root_key: root_public(),
    }
}

fn moderator_list(version: u64) -> ModeratorList {
    ModeratorList {
        list_version: version,
        issued_at_ms: 1,
        rp_id: "moderation.example".into(),
        origin: "https://moderation.example".into(),
        entries: vec![ModeratorEntry {
            moderator_id: [1; 16],
            name: "m".into(),
            key: SoftwareAuthenticator::from_seed(&MODERATOR_SEED).moderator_key(),
            credential_id: vec![1],
            x25519_public: [9; 32],
            added_at_ms: 0,
            revoked_at_ms: 0,
        }],
    }
}

fn unix_ms() -> u64 {
    u64::try_from(now_ms()).unwrap()
}

/// The statement body for `action` on `target`, issued at `issued_at_ms`
/// under a list of `version`.
fn body_at(action: Action, target: &[u8], issued_at_ms: u64, version: u64) -> Value {
    let list = moderator_list(version);
    let salt: [u8; 16] = random();
    let statement = Statement {
        statement_id: random(),
        action,
        target: Target::new(action.target_kind(), target).unwrap(),
        reason_commitment: reason_commitment(Reason::Spam, &salt),
        until_ms: 0,
        issued_at_ms,
        moderator_id: [1; 16],
        list_version: version,
    };
    let signed = SoftwareAuthenticator::from_seed(&MODERATOR_SEED)
        .sign_statement(&list, &statement)
        .unwrap()
        .encode()
        .unwrap();
    serde_json::to_value(StatementBody::new(
        &signed,
        &list.sign(&ROOT_SEED).unwrap(),
        9,
        Reason::Spam,
        &salt,
    ))
    .unwrap()
}

fn body(action: Action, target: &[u8]) -> Value {
    body_at(action, target, unix_ms(), V)
}

struct Harness {
    state: AppState,
    public: axum::Router,
}

impl Harness {
    async fn new(name: &str) -> Option<Self> {
        let pool = test_pool(name).await?;
        let state = state(pool, true);
        Some(Self {
            public: crate::build_app(state.clone()),
            state,
        })
    }

    async fn internal(&self, uri: &str, body: &Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header(SECRET_HEADER, STANDARD.encode(SECRET))
            .body(Body::from(body.to_string()))
            .unwrap();
        respond(
            internal_router(self.state.clone(), config())
                .oneshot(req)
                .await
                .unwrap(),
        )
        .await
    }

    async fn statement(&self, body: &Value) -> (StatusCode, Value) {
        self.internal("/internal/moderation/statement", body).await
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        account: Option<[u8; 16]>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-real-ip", "192.0.2.77")
            .header("content-type", "application/json");
        if let Some(account) = account {
            let token = self
                .state
                .session_token_secret
                .issue(
                    &account,
                    1,
                    &[0u8; 32],
                    0,
                    i64::try_from(unix_ms()).unwrap() + 60_000,
                )
                .unwrap();
            b = b.header("authorization", format!("Bearer {token}"));
        }
        let body = body.map_or_else(Body::empty, |v| Body::from(v.to_string()));
        respond(
            self.public
                .clone()
                .oneshot(b.body(body).unwrap())
                .await
                .unwrap(),
        )
        .await
    }

    async fn register(&self, account: [u8; 16], hash: [u8; 64]) -> (StatusCode, Value) {
        self.call(
            "POST",
            "/discovery/register",
            Some(account),
            Some(json!({
                "account_id": STANDARD.encode(account),
                "oprf_hash": STANDARD.encode(hash),
            })),
        )
        .await
    }

    /// Enter or leave Everyone with one name term.
    async fn visibility(&self, account: [u8; 16], public: bool) -> (StatusCode, Value) {
        use lakko_contacts::introduction::{compose_public_record, IntroductionPayload};
        let mut body = json!({ "public": public });
        if public {
            let payload = IntroductionPayload {
                identity_pk: SigningKey::from_seed(&[7; 32]).verifying_key().to_bytes(),
                account_id: account,
                display_name: Some("Anna Weber".to_string()),
                username: None,
                username_certificate: None,
                mailbox_binding: lakko_contacts::mailbox::MailboxBinding::for_account_seed(
                    &[7; 32],
                ),
                fof: None,
            };
            let record = compose_public_record(&payload, &[random::<64>()], 1).unwrap();
            body["public_record_b64"] = json!(STANDARD.encode(record));
            body["name_terms_b64"] = json!([STANDARD.encode(random::<64>())]);
        }
        self.call("PUT", "/discovery/visibility", Some(account), Some(body))
            .await
    }

    /// An identity bundle for `account` whose key has `target`.
    async fn bundle(&self, account: [u8; 16]) -> [u8; 32] {
        let key: [u8; 32] = random();
        sqlx::query(
            "INSERT INTO identity_bundles \
                (account_id, signature_pubkey, x25519_pubkey, account_target) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&account[..])
        .bind(&key[..])
        .bind(&[9u8; 32][..])
        .bind(&crate::identity_bundle::account_target(&key)[..])
        .execute(&self.state.db)
        .await
        .unwrap();
        crate::identity_bundle::account_target(&key)
    }

    async fn public_records(&self, kind: u8, target: &[u8]) -> i64 {
        sqlx::query_scalar(
            "SELECT CAST(COUNT(*) AS SIGNED) FROM public_records WHERE kind = ? AND target = ?",
        )
        .bind(kind)
        .bind(target)
        .fetch_one(&self.state.db)
        .await
        .unwrap()
    }
}

async fn respond(resp: Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

fn code(v: &Value) -> &str {
    v["error"].as_str().unwrap_or_default()
}

macro_rules! db_or_skip {
    ($name:literal) => {{
        let Some(h) = Harness::new($name).await else {
            return;
        };
        h
    }};
}

// ---------------------------------------------------------------------------
// Pure
// ---------------------------------------------------------------------------

#[test]
fn the_internal_config_needs_both_secret_and_root() {
    let secret = STANDARD.encode([7u8; 32]);
    let root = URL_SAFE_NO_PAD.encode([9u8; 32]);
    assert!(parse_internal(None, None, None).unwrap().is_none());
    assert!(parse_internal(None, Some(&secret), None).unwrap().is_none());
    assert!(parse_internal(None, None, Some(&root)).unwrap().is_none());
    let cfg = parse_internal(None, Some(&secret), Some(&root))
        .unwrap()
        .unwrap();
    assert_eq!(cfg.bind.to_string(), DEFAULT_INTERNAL_BIND);
    assert_eq!(cfg.root_key, [9u8; 32]);
    assert!(parse_internal(None, Some("!!"), Some(&root)).is_err());
    assert!(parse_internal(None, Some(&STANDARD.encode([1u8; 8])), Some(&root)).is_err());
    assert!(parse_internal(None, Some(&secret), Some("short")).is_err());
    assert!(parse_internal(Some("nope"), Some(&secret), Some(&root)).is_err());
}

#[test]
fn a_moderated_handle_refuses_what_it_should() {
    assert!(handle_write_refusal(HANDLE_OPEN, true).is_none());
    assert!(handle_write_refusal(HANDLE_DELISTED, false).is_none());
    assert_eq!(
        handle_write_refusal(HANDLE_DELISTED, true)
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    for publishes in [false, true] {
        assert_eq!(
            handle_write_refusal(HANDLE_BLOCKED, publishes)
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}

#[test]
fn the_hold_is_180_days() {
    assert_eq!(HOLD_MS, 15_552_000_000);
}

#[tokio::test]
async fn the_internal_routes_need_the_secret() {
    let db = sqlx::mysql::MySqlPoolOptions::new()
        .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/none")
        .unwrap();
    let app = internal_router(state(db, false), config());
    for secret in [None, Some([0x11u8; 32])] {
        let mut b = Request::builder()
            .method("POST")
            .uri("/internal/moderation/statement")
            .header("content-type", "application/json");
        if let Some(s) = secret {
            b = b.header(SECRET_HEADER, STANDARD.encode(s));
        }
        let resp = app
            .clone()
            .oneshot(b.body(Body::from("{}")).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

// ---------------------------------------------------------------------------
// Routes (DB)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_taken_back_name_is_held_and_released() {
    let h = db_or_skip!("a_taken_back_name_is_held_and_released");
    let (owner, other) = (random::<16>(), random::<16>());
    let name = discovery_lookup_key(&random::<64>());
    let _ = h.bundle(owner).await;
    assert_eq!(h.register(owner, name).await.0, StatusCode::OK);
    assert_eq!(h.visibility(owner, true).await.0, StatusCode::NO_CONTENT);
    assert_eq!(h.public_records(TARGET_PERSON, &owner).await, 1);

    let take = body(Action::TakeBackName, &name);
    let (st, v) = h.statement(&take).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v, json!({ "applied": true }));
    // The name, the owner's record and terms are gone; the claim is held.
    let rows: i64 =
        sqlx::query_scalar("SELECT CAST(COUNT(*) AS SIGNED) FROM usernames WHERE oprf_hash = ?")
            .bind(&name[..])
            .fetch_one(&h.state.db)
            .await
            .unwrap();
    assert_eq!(rows, 0);
    assert_eq!(h.public_records(TARGET_PERSON, &owner).await, 0);
    let kind: u8 = sqlx::query_scalar("SELECT kind FROM name_claims WHERE lookup_key = ?")
        .bind(&name[..])
        .fetch_one(&h.state.db)
        .await
        .unwrap();
    assert_eq!(kind, CLAIM_MODERATION_HOLD);
    let (st, v) = h.register(other, name).await;
    assert_eq!((st, code(&v)), (StatusCode::CONFLICT, "name-taken"));
    // The owner is free to take another name.
    assert_eq!(
        h.register(owner, discovery_lookup_key(&random::<64>()))
            .await
            .0,
        StatusCode::OK
    );
    // The same statement again changes nothing.
    assert_eq!(
        h.statement(&take).await,
        (StatusCode::OK, json!({ "applied": false, "repeat": true }))
    );

    let (st, v) = h.statement(&body(Action::ReleaseName, &name)).await;
    assert_eq!((st, v), (StatusCode::OK, json!({ "applied": true })));
    assert_eq!(h.register(other, name).await.0, StatusCode::OK);
}

#[tokio::test]
async fn an_expired_hold_frees_the_name() {
    let h = db_or_skip!("an_expired_hold_frees_the_name");
    let name = discovery_lookup_key(&random::<64>());
    let (st, v) = h.statement(&body(Action::TakeBackName, &name)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // A statement is refused 24 h after its issue, so a hold cannot be made
    // old by its statement: end this one by hand, a day ago.
    sqlx::query("UPDATE moderation_name_holds SET until_ms = ? WHERE lookup_key = ?")
        .bind(now_ms() - 86_400_000)
        .bind(&name[..])
        .execute(&h.state.db)
        .await
        .unwrap();
    assert!(prune_holds(&h.state.db, now_ms()).await.unwrap() >= 1);
    assert_eq!(h.register(random(), name).await.0, StatusCode::OK);
}

#[tokio::test]
async fn hide_from_search_refuses_everyone_until_shown() {
    let h = db_or_skip!("hide_from_search_refuses_everyone_until_shown");
    let account = random::<16>();
    let target = h.bundle(account).await;
    let name = discovery_lookup_key(&random::<64>());
    assert_eq!(h.register(account, name).await.0, StatusCode::OK);
    assert_eq!(h.visibility(account, true).await.0, StatusCode::NO_CONTENT);

    let hidden_at = unix_ms();
    let (st, v) = h
        .statement(&body_at(Action::HideFromSearch, &target, hidden_at, V))
        .await;
    assert_eq!((st, v), (StatusCode::OK, json!({ "applied": true })));
    assert_eq!(h.public_records(TARGET_PERSON, &account).await, 0);
    let (st, v) = h.visibility(account, true).await;
    assert_eq!((st, code(&v)), (StatusCode::FORBIDDEN, CODE_HIDDEN));
    // Leaving stays allowed.
    assert_eq!(h.visibility(account, false).await.0, StatusCode::NO_CONTENT);
    let (_, read) = h
        .internal(
            "/internal/moderation/account",
            &json!({ "target_b64": STANDARD.encode(target) }),
        )
        .await;
    assert_eq!(read["hidden_from_search"], json!(true));
    assert_eq!(read["has_username"], json!(true));
    assert_eq!(read["in_everyone"], json!(false));
    assert_eq!(read["name_lookup_keys_b64"], json!([STANDARD.encode(name)]));

    // A Show the moderator made before the Hide changes nothing.
    let (st, v) = h
        .statement(&body_at(Action::ShowInSearch, &target, hidden_at - 1000, V))
        .await;
    assert_eq!(
        (st, v),
        (
            StatusCode::OK,
            json!({ "applied": false, "superseded": true })
        )
    );
    assert_eq!(h.visibility(account, true).await.0, StatusCode::FORBIDDEN);

    let (st, v) = h
        .statement(&body_at(Action::ShowInSearch, &target, hidden_at + 1000, V))
        .await;
    assert_eq!((st, v), (StatusCode::OK, json!({ "applied": true })));
    assert_eq!(h.visibility(account, true).await.0, StatusCode::NO_CONTENT);
    let (_, read) = h
        .internal(
            "/internal/moderation/account",
            &json!({ "target_b64": STANDARD.encode(target) }),
        )
        .await;
    assert_eq!(read["in_everyone"], json!(true));
    assert_eq!(read["hidden_from_search"], json!(false));
}

#[tokio::test]
async fn unknown_targets_and_bad_statements_change_nothing() {
    let h = db_or_skip!("unknown_targets_and_bad_statements_change_nothing");
    let unknown = random::<32>();
    let (st, v) = h.statement(&body(Action::HideFromSearch, &unknown)).await;
    assert_eq!((st, code(&v)), (StatusCode::NOT_FOUND, "unknown-account"));
    let (st, v) = h
        .statement(&body(Action::GroupDelist, &random::<64>()))
        .await;
    assert_eq!((st, code(&v)), (StatusCode::NOT_FOUND, "unknown-handle"));
    // A refused statement was not recorded: the same body applies later.
    let account = random::<16>();
    let target = h.bundle(account).await;
    let later = body(Action::HideFromSearch, &target);
    assert_eq!(h.statement(&later).await.0, StatusCode::OK);

    // Not discovery's action.
    let (st, v) = h
        .statement(&body(Action::ChannelRemove, &random::<16>()))
        .await;
    assert_eq!((st, code(&v)), (StatusCode::BAD_REQUEST, "wrong-service"));
    // A list older than one already seen.
    let (st, v) = h
        .statement(&body_at(Action::ShowInSearch, &target, unix_ms(), V - 1))
        .await;
    assert_eq!((st, code(&v)), (StatusCode::CONFLICT, "stale-list"));
    // A reason that does not open the commitment.
    let mut wrong = body(Action::ShowInSearch, &target);
    wrong["reason"] = json!(Reason::Hate.as_u8());
    let (st, v) = h.statement(&wrong).await;
    assert_eq!((st, code(&v)), (StatusCode::BAD_REQUEST, "reason-mismatch"));
    // A list under another root.
    let mut forged = body(Action::ShowInSearch, &target);
    forged["moderator_list_b64"] =
        json!(STANDARD.encode(moderator_list(V).sign(&[0x01; 32]).unwrap()));
    let (st, v) = h.statement(&forged).await;
    assert_eq!((st, code(&v)), (StatusCode::BAD_REQUEST, "bad-list"));
    // A statement issued more than 24 h ago, or more than 5 min ahead.
    let (st, v) = h
        .statement(&body_at(
            Action::ShowInSearch,
            &target,
            unix_ms() - 25 * 3_600_000,
            V,
        ))
        .await;
    assert_eq!((st, code(&v)), (StatusCode::BAD_REQUEST, "stale-statement"));
    let (st, v) = h
        .statement(&body_at(
            Action::ShowInSearch,
            &target,
            unix_ms() + 10 * 60_000,
            V,
        ))
        .await;
    assert_eq!(
        (st, code(&v)),
        (StatusCode::BAD_REQUEST, "future-statement")
    );
}

#[tokio::test]
async fn the_moderation_service_pushes_its_list_floor() {
    let h = db_or_skip!("the_moderation_service_pushes_its_list_floor");
    let push = |version: u64, root: [u8; 32]| {
        serde_json::to_value(ListBody::new(&moderator_list(version).sign(&root).unwrap())).unwrap()
    };
    // The shared database's floor is V (every test signs under it).
    let (st, v) = h
        .internal("/internal/moderation/list", &push(V, ROOT_SEED))
        .await;
    assert_eq!((st, v), (StatusCode::OK, json!({ "list_version": V })));
    let (st, v) = h
        .internal("/internal/moderation/list", &push(V - 1, ROOT_SEED))
        .await;
    assert_eq!((st, code(&v)), (StatusCode::CONFLICT, "stale-list"));
    let (st, v) = h
        .internal("/internal/moderation/list", &push(V, [0x01; 32]))
        .await;
    assert_eq!((st, code(&v)), (StatusCode::BAD_REQUEST, "bad-list"));
    // Behind the secret like every internal route.
    let req = Request::builder()
        .method("POST")
        .uri("/internal/moderation/list")
        .header("content-type", "application/json")
        .body(Body::from(push(V, ROOT_SEED).to_string()))
        .unwrap();
    let resp = internal_router(h.state.clone(), config())
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_username_held_by_a_group_is_not_taken_back() {
    let h = db_or_skip!("a_username_held_by_a_group_is_not_taken_back");
    let g = Group::new();
    g.insert(&h).await;
    let (st, v) = h
        .statement(&body(Action::TakeBackName, &g.lookup_key()))
        .await;
    assert_eq!((st, code(&v)), (StatusCode::CONFLICT, "not-a-username"));
}

/// A group's handle, written straight to the table (its claim route spends a
/// token; what is under test is the moderation of the row).
struct Group {
    seed: [u8; 32],
    name_output: [u8; 64],
    term_output: [u8; 64],
    handle_id: [u8; 16],
    record: Vec<u8>,
}

impl Group {
    fn new() -> Self {
        let name_output = random::<64>();
        let term_output = random::<64>();
        let payload = HandleRecordPayload {
            kind: HandleRecordKind::Group,
            updated_at_ms: 1,
            id: vec![0x33; 32],
            key: [0x44; 32],
            knock_version: 1,
            handle: "hiking_club".to_string(),
            name: "Hiking club".to_string(),
            description: String::new(),
            size_band: 1,
            avatar: None,
        };
        let record = compose_handle_record(&payload, &[name_output, term_output], 1).unwrap();
        Self {
            seed: random(),
            name_output,
            term_output,
            handle_id: random(),
            record,
        }
    }

    fn lookup_key(&self) -> [u8; 64] {
        discovery_lookup_key(&self.name_output)
    }

    fn term_key(&self) -> [u8; 64] {
        discovery_lookup_key(&self.term_output)
    }

    async fn insert(&self, h: &Harness) {
        let key = self.lookup_key();
        sqlx::query("INSERT INTO name_claims (lookup_key, kind) VALUES (?, ?)")
            .bind(&key[..])
            .bind(CLAIM_HANDLE)
            .execute(&h.state.db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO handles (lookup_key, handle_id, write_pk, is_public, lease_until) \
             VALUES (?, ?, ?, TRUE, NOW(3) + INTERVAL 1 DAY)",
        )
        .bind(&key[..])
        .bind(&self.handle_id[..])
        .bind(&handle_write_public_key(&self.seed)[..])
        .execute(&h.state.db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO public_records (kind, target, record) VALUES (?, ?, ?)")
            .bind(TARGET_HANDLE)
            .bind(&self.handle_id[..])
            .bind(&self.record)
            .execute(&h.state.db)
            .await
            .unwrap();
    }

    /// A handle write of `kind` to the row, through the public route.
    async fn write(
        &self,
        h: &Harness,
        method: &str,
        uri: &str,
        body: HandleStatementBody,
        publish: bool,
    ) -> (StatusCode, Value) {
        let statement = HandleStatement::sign(
            &self.seed,
            self.lookup_key(),
            body,
            unix_ms(),
            random::<HANDLE_NONCE_LEN>(),
        );
        let mut v = json!({ "statement_b64": STANDARD.encode(statement.encode()) });
        if publish {
            v["public_record_b64"] = json!(STANDARD.encode(&self.record));
            v["terms_b64"] = json!([STANDARD.encode(self.term_key())]);
        }
        h.call(method, uri, None, Some(v)).await
    }

    async fn update(&self, h: &Harness, public: bool) -> (StatusCode, Value) {
        let (visibility, hash) = if public {
            (
                HandleVisibility::Public,
                handle_content_hash(Some(&self.record), &[self.term_key()]).unwrap(),
            )
        } else {
            (
                HandleVisibility::Private,
                handle_content_hash(None, &[]).unwrap(),
            )
        };
        self.write(
            h,
            "PUT",
            "/discovery/handle",
            HandleStatementBody::Update {
                visibility,
                content_hash: hash,
            },
            public,
        )
        .await
    }

    async fn moderation(&self, h: &Harness) -> (u8, bool) {
        sqlx::query_as("SELECT moderation, is_public FROM handles WHERE lookup_key = ?")
            .bind(&self.lookup_key()[..])
            .fetch_one(&h.state.db)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn delisted_and_blocked_handles_refuse_their_writes() {
    let h = db_or_skip!("delisted_and_blocked_handles_refuse_their_writes");
    let g = Group::new();
    g.insert(&h).await;
    let key = g.lookup_key();

    let (st, v) = h.statement(&body(Action::GroupDelist, &key)).await;
    assert_eq!((st, v), (StatusCode::OK, json!({ "applied": true })));
    assert_eq!(g.moderation(&h).await, (HANDLE_DELISTED, false));
    assert_eq!(h.public_records(TARGET_HANDLE, &g.handle_id).await, 0);
    // A delisted name may stay private and renew; publishing is refused.
    assert_eq!(g.update(&h, false).await.0, StatusCode::OK);
    let (st, v) = g.update(&h, true).await;
    assert_eq!((st, code(&v)), (StatusCode::FORBIDDEN, CODE_DELISTED));

    let (st, _) = h.statement(&body(Action::GroupBlockHandle, &key)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(g.moderation(&h).await.0, HANDLE_BLOCKED);
    let (st, v) = g.update(&h, false).await;
    assert_eq!((st, code(&v)), (StatusCode::FORBIDDEN, CODE_BLOCKED));
    let (st, v) = g
        .write(
            &h,
            "DELETE",
            "/discovery/handle",
            HandleStatementBody::Retire,
            false,
        )
        .await;
    assert_eq!((st, code(&v)), (StatusCode::FORBIDDEN, CODE_BLOCKED));

    let (st, _) = h.statement(&body(Action::GroupRestore, &key)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(g.moderation(&h).await.0, HANDLE_OPEN);
    assert_eq!(g.update(&h, true).await.0, StatusCode::OK);
    assert_eq!(h.public_records(TARGET_HANDLE, &g.handle_id).await, 1);
}

#[tokio::test]
async fn a_blocked_retired_handle_is_never_freed() {
    let h = db_or_skip!("a_blocked_retired_handle_is_never_freed");
    let g = Group::new();
    g.insert(&h).await;
    let key = g.lookup_key();
    assert_eq!(
        h.statement(&body(Action::GroupBlockHandle, &key)).await.0,
        StatusCode::OK
    );
    sqlx::query("UPDATE handles SET retired_at = NOW(3) - INTERVAL 400 DAY WHERE lookup_key = ?")
        .bind(&key[..])
        .execute(&h.state.db)
        .await
        .unwrap();
    crate::handles::prune(&h.state.db, 1).await.unwrap();
    let held: i64 =
        sqlx::query_scalar("SELECT CAST(COUNT(*) AS SIGNED) FROM name_claims WHERE lookup_key = ?")
            .bind(&key[..])
            .fetch_one(&h.state.db)
            .await
            .unwrap();
    assert_eq!(held, 1);
}

#[test]
fn the_target_kind_of_each_discovery_action() {
    for action in ApplyingService::Discovery.actions() {
        let len = action.target_kind().target_len();
        match action {
            Action::HideFromSearch | Action::ShowInSearch => {
                assert_eq!(action.target_kind(), TargetKind::Account);
            },
            _ => assert_eq!(len, 64),
        }
    }
}
