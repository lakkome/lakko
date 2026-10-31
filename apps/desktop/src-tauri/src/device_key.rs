// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The device root key, held by the host (ADR-0085 section 2, decision 9.1).
//!
//! The page never sees the key. It asks the host, over the app scheme, to
//! derive it (the same HKDF-SHA-512 as `apps/web/src/auth/device-key.ts`),
//! to persist or load it, and to run AES-256-GCM with it; it holds only a
//! handle, valid for the life of this process.
//!
//! Routes, under [`PATH`]:
//! - `POST derive` `{accountIdB64, source, ikmB64}` -> `{handle}`
//! - `POST put` `{accountIdB64, handle}` -> 204
//! - `POST get` `{accountIdB64}` -> `{handle}` | 404
//! - `POST delete` `{accountIdB64}` -> 204
//! - `POST encrypt?handle=` body `iv(12) || plaintext` -> ciphertext || tag
//! - `POST decrypt?handle=` body `iv(12) || ciphertext || tag` -> plaintext | 400
//!
//! A key store the host cannot use answers `503 {"error": ...}`; the page
//! stops sign-in on it and nothing falls back (invariant #7, fail-closed).

use std::collections::HashMap;
use std::sync::Mutex;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use hkdf::Hkdf;
use http::{Method, Request, Response, StatusCode};
use sha2::Sha512;
use tracing::warn;
use zeroize::Zeroizing;

use crate::key_store::{self, KeyStoreError};
use crate::proxy::plain;

/// Path prefix of the device-key routes.
pub const PATH: &str = "/__host/device-key";

/// HKDF salt; must equal `HKDF_SALT` in `apps/web/src/auth/device-key.ts`.
const HKDF_SALT: &[u8] = b"lakko-device-root-v1";
/// HKDF info prefix for a key derived from the OPAQUE export key.
const INFO_EXPORT_KEY: &str = "lakko/device-root/v1|";
/// HKDF info prefix for a key derived from the account identity seed.
const INFO_IDENTITY_SEED: &str = "lakko/device-root/from-identity-seed/v1|";

/// AES-GCM IV length the web client uses.
const IV_LEN: usize = 12;
/// Upper bound on live handles; a page that leaks handles is refused rather
/// than growing host memory without bound.
const MAX_HANDLES: usize = 256;
/// Largest plaintext or ciphertext the host accepts in one call.
const MAX_BODY: usize = 64 * 1024 * 1024;

type Key = Zeroizing<[u8; 32]>;

/// Derive the device root key exactly as the web client does.
pub fn derive(source: &str, ikm: &[u8], account_id_b64: &str) -> Option<Key> {
    let prefix = match source {
        "export-key" => INFO_EXPORT_KEY,
        "identity-seed" => INFO_IDENTITY_SEED,
        _ => return None,
    };
    let info = format!("{prefix}{account_id_b64}");
    let hk = Hkdf::<Sha512>::new(Some(HKDF_SALT), ikm);
    let mut okm = Zeroizing::new([0u8; 32]);
    hk.expand(info.as_bytes(), okm.as_mut()).ok()?;
    Some(okm)
}

/// The keys the page holds handles to.
#[derive(Default)]
pub struct Handles {
    keys: Mutex<HashMap<String, Key>>,
    /// Account -> the handle of its stored key, so a repeated `get` (the web
    /// client reads the key for every store it opens) costs no keychain call
    /// and no new handle.
    stored: Mutex<HashMap<String, String>>,
}

impl Handles {
    fn stored_handle(&self, account: &str) -> Option<String> {
        let h = self.stored.lock().ok()?.get(account).cloned()?;
        self.keys.lock().ok()?.contains_key(&h).then_some(h)
    }

    fn remember_stored(&self, account: &str, handle: &str) {
        if let Ok(mut map) = self.stored.lock() {
            map.insert(account.to_owned(), handle.to_owned());
        }
    }

    fn insert(&self, key: Key) -> Result<String, &'static str> {
        let mut map = self.keys.lock().map_err(|_| "poisoned")?;
        if map.len() >= MAX_HANDLES {
            return Err("too-many-handles");
        }
        let mut raw = [0u8; 16];
        getrandom::getrandom(&mut raw).map_err(|_| "rng")?;
        let handle: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        map.insert(handle.clone(), key);
        Ok(handle)
    }

    fn get(&self, handle: &str) -> Option<Key> {
        let map = self.keys.lock().ok()?;
        map.get(handle).map(|k| Zeroizing::new(**k))
    }

    /// Forget every handle; called when an account's key is deleted, since
    /// the page may still hold handles to it.
    fn clear(&self) {
        if let Ok(mut map) = self.keys.lock() {
            map.clear();
        }
        if let Ok(mut map) = self.stored.lock() {
            map.clear();
        }
    }
}

/// Answer a request under [`PATH`].
// The error of an operation is the response to send, built once and handed
// straight back, so its size costs nothing here.
#[allow(clippy::result_large_err)]
pub fn handle(handles: &Handles, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if request.method() != Method::POST {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "method");
    }
    let op = request.uri().path().strip_prefix(PATH).unwrap_or("");
    match op {
        "/derive" => json_op(request, |v| {
            let account = str_field(v, "accountIdB64")?;
            let source = str_field(v, "source")?;
            let ikm = Zeroizing::new(
                base64::engine::general_purpose::STANDARD
                    .decode(str_field(v, "ikmB64")?)
                    .map_err(|_| bad("ikm"))?,
            );
            let key = derive(source, &ikm, account).ok_or_else(|| bad("source"))?;
            let h = handles.insert(key).map_err(internal)?;
            Ok(json(StatusCode::OK, &serde_json::json!({ "handle": h })))
        }),
        "/put" => json_op(request, |v| {
            let account = str_field(v, "accountIdB64")?;
            let handle = str_field(v, "handle")?;
            let key = handles.get(handle).ok_or_else(|| bad("handle"))?;
            key_store::save(account, &key).map_err(store_error)?;
            handles.remember_stored(account, handle);
            Ok(no_content())
        }),
        "/get" => json_op(request, |v| {
            let account = str_field(v, "accountIdB64")?;
            if let Some(h) = handles.stored_handle(account) {
                return Ok(json(StatusCode::OK, &serde_json::json!({ "handle": h })));
            }
            match key_store::load(account).map_err(store_error)? {
                None => Ok(json(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": "absent" }),
                )),
                Some(key) => {
                    let h = handles.insert(key).map_err(internal)?;
                    handles.remember_stored(account, &h);
                    Ok(json(StatusCode::OK, &serde_json::json!({ "handle": h })))
                },
            }
        }),
        "/delete" => json_op(request, |v| {
            let account = str_field(v, "accountIdB64")?;
            key_store::delete(account).map_err(store_error)?;
            handles.clear();
            Ok(no_content())
        }),
        "/encrypt" | "/decrypt" => cipher_op(handles, request, op == "/encrypt"),
        _ => plain(StatusCode::NOT_FOUND, "not found"),
    }
}

fn cipher_op(handles: &Handles, request: &Request<Vec<u8>>, encrypt: bool) -> Response<Vec<u8>> {
    let handle = request
        .uri()
        .query()
        .and_then(|q| q.strip_prefix("handle="))
        .unwrap_or("");
    let Some(key) = handles.get(handle) else {
        return json(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "handle" }),
        );
    };
    let body = request.body();
    if body.len() < IV_LEN || body.len() > MAX_BODY {
        return json(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "length" }),
        );
    }
    let (iv, data) = body.split_at(IV_LEN);
    let Ok(cipher) = Aes256Gcm::new_from_slice(key.as_ref()) else {
        return plain(StatusCode::INTERNAL_SERVER_ERROR, "cipher");
    };
    let nonce = Nonce::from_slice(iv);
    let result = if encrypt {
        cipher.encrypt(nonce, data)
    } else {
        cipher.decrypt(nonce, data)
    };
    match result {
        Ok(out) => Response::builder()
            .status(StatusCode::OK)
            .header(http::header::CONTENT_TYPE, "application/octet-stream")
            .body(out)
            .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "response")),
        Err(_) => json(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "decrypt-failed" }),
        ),
    }
}

type OpResult = Result<Response<Vec<u8>>, Response<Vec<u8>>>;

fn json_op(
    request: &Request<Vec<u8>>,
    f: impl FnOnce(&serde_json::Value) -> OpResult,
) -> Response<Vec<u8>> {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(request.body()) else {
        return bad("json");
    };
    match f(&v) {
        Ok(r) | Err(r) => r,
    }
}

// The error of an operation is the response to send, built once and handed
// straight back, so its size costs nothing here.
#[allow(clippy::result_large_err)]
fn str_field<'a>(v: &'a serde_json::Value, name: &str) -> Result<&'a str, Response<Vec<u8>>> {
    v.get(name)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(name))
}

fn store_error(e: KeyStoreError) -> Response<Vec<u8>> {
    warn!(error = %e, "device key store");
    match e {
        KeyStoreError::Unavailable(reason) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            &serde_json::json!({ "error": reason }),
        ),
        KeyStoreError::Failed(_) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({ "error": "key-store-failed" }),
        ),
    }
}

fn internal(code: &'static str) -> Response<Vec<u8>> {
    json(
        StatusCode::INTERNAL_SERVER_ERROR,
        &serde_json::json!({ "error": code }),
    )
}

fn bad(field: &str) -> Response<Vec<u8>> {
    json(
        StatusCode::BAD_REQUEST,
        &serde_json::json!({ "error": format!("bad-{field}") }),
    )
}

fn no_content() -> Response<Vec<u8>> {
    let mut r = Response::new(Vec::new());
    *r.status_mut() = StatusCode::NO_CONTENT;
    r
}

fn json(status: StatusCode, value: &serde_json::Value) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(value.to_string().into_bytes())
        .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Pinned against WebCrypto (`crypto.subtle.deriveBits` HKDF-SHA-512 with
    /// the web client's salt and info), computed with Node's webcrypto.
    #[test]
    fn derivation_matches_the_web_client() {
        let ikm: Vec<u8> = (0u8..64).collect();
        let account = "AZJ0tN9YdkCSm4zqZ0T5oQ==";
        let a = derive("export-key", &ikm, account).expect("derive");
        let b = derive("identity-seed", &ikm, account).expect("derive");
        assert_eq!(hex(a.as_ref()), GOLDEN_EXPORT_KEY);
        assert_eq!(hex(b.as_ref()), GOLDEN_IDENTITY_SEED);
        assert!(derive("other", &ikm, account).is_none());
    }

    const GOLDEN_EXPORT_KEY: &str =
        "580dd978cdf210bff5cbdf52be9c591fdb34fdff8f3a2a848bf261c0b82713ce";
    const GOLDEN_IDENTITY_SEED: &str =
        "5589abd90eb4e362ba1ed2d2d50e156604d0929c3934c57d79af31483f8b8347";

    #[test]
    fn encrypt_then_decrypt_through_the_routes() {
        let handles = Handles::default();
        let key = derive("export-key", &[1u8; 64], "acct").expect("derive");
        let h = handles.insert(key).expect("insert");
        let iv = [9u8; IV_LEN];
        let mut body = iv.to_vec();
        body.extend_from_slice(b"hello");
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("lakkoapp://localhost{PATH}/encrypt?handle={h}"))
            .body(body)
            .expect("req");
        let ct = handle(&handles, &req);
        assert_eq!(ct.status(), StatusCode::OK);
        assert_eq!(ct.body().len(), 5 + 16);

        let mut body = iv.to_vec();
        body.extend_from_slice(ct.body());
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("lakkoapp://localhost{PATH}/decrypt?handle={h}"))
            .body(body.clone())
            .expect("req");
        let pt = handle(&handles, &req);
        assert_eq!(pt.body().as_slice(), b"hello");

        let last = body.len() - 1;
        body[last] ^= 1;
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("lakkoapp://localhost{PATH}/decrypt?handle={h}"))
            .body(body)
            .expect("req");
        assert_eq!(handle(&handles, &req).status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_stored_key_keeps_one_handle_until_cleared() {
        let handles = Handles::default();
        let h = handles.insert(Zeroizing::new([3u8; 32])).expect("insert");
        assert!(handles.stored_handle("acct").is_none());
        handles.remember_stored("acct", &h);
        assert_eq!(handles.stored_handle("acct").as_deref(), Some(h.as_str()));
        assert_eq!(handles.stored_handle("acct").as_deref(), Some(h.as_str()));
        handles.clear();
        assert!(handles.stored_handle("acct").is_none());
        assert!(handles.get(&h).is_none());
    }

    #[test]
    fn unknown_handle_and_short_body_are_refused() {
        let handles = Handles::default();
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("lakkoapp://localhost{PATH}/encrypt?handle=nope"))
            .body(vec![0u8; 20])
            .expect("req");
        assert_eq!(handle(&handles, &req).status(), StatusCode::BAD_REQUEST);
    }
}
