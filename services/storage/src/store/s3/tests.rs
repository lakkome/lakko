// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Configuration and key-layout tests, and the bucket tests gated on a
//! test object store (`LAKKO_STORAGE_TEST_S3_ENDPOINT`,
//! `LAKKO_STORAGE_TEST_S3_ACCESS_KEY`, `LAKKO_STORAGE_TEST_S3_SECRET_KEY`,
//! optional `LAKKO_STORAGE_TEST_S3_REGION`, default `us-east-1`); without
//! them the bucket tests return early, as the MariaDB-gated tests do. Each
//! bucket test creates its own path-style bucket.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::store::blob_path;

fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| map.get(name).cloned()
}

const FULL: [(&str, &str); 5] = [
    (
        "LAKKO_STORAGE_S3_ENDPOINT",
        "https://nbg1.your-objectstorage.com",
    ),
    ("LAKKO_STORAGE_S3_BUCKET", "lakko-blobs"),
    ("LAKKO_STORAGE_S3_REGION", "nbg1"),
    ("LAKKO_STORAGE_S3_ACCESS_KEY", "AKEXAMPLE"),
    ("LAKKO_STORAGE_S3_SECRET_KEY", "very-secret-value"),
];

#[test]
fn nothing_set_is_the_local_directory() {
    assert!(S3Config::from_lookup(lookup(&[])).unwrap().is_none());
    // Blank values count as unset.
    assert!(
        S3Config::from_lookup(lookup(&[("LAKKO_STORAGE_S3_BUCKET", "  ")]))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_partial_configuration_names_what_is_missing() {
    let err = S3Config::from_lookup(lookup(&FULL[..2])).unwrap_err();
    assert_eq!(
        err,
        ConfigError::Missing(vec![
            "LAKKO_STORAGE_S3_REGION",
            "LAKKO_STORAGE_S3_ACCESS_KEY",
            "LAKKO_STORAGE_S3_SECRET_KEY",
        ])
    );
    // The path-style flag alone is a partial configuration too.
    assert!(matches!(
        S3Config::from_lookup(lookup(&[("LAKKO_STORAGE_S3_PATH_STYLE", "true")])),
        Err(ConfigError::Missing(m)) if m.len() == 5
    ));
}

#[test]
fn a_full_configuration_parses_and_never_shows_the_secret() {
    let c = S3Config::from_lookup(lookup(&FULL)).unwrap().unwrap();
    assert!(!c.path_style);
    assert_eq!(c.bucket, "lakko-blobs");
    let shown = format!("{c:?}");
    assert!(!shown.contains("very-secret-value"));
    assert!(!shown.contains("AKEXAMPLE"));

    let mut with_style = FULL.to_vec();
    with_style.push(("LAKKO_STORAGE_S3_PATH_STYLE", "TRUE"));
    assert!(
        S3Config::from_lookup(lookup(&with_style))
            .unwrap()
            .unwrap()
            .path_style
    );
    with_style.pop();
    with_style.push(("LAKKO_STORAGE_S3_PATH_STYLE", "maybe"));
    assert_eq!(
        S3Config::from_lookup(lookup(&with_style)).unwrap_err(),
        ConfigError::PathStyle
    );
}

#[test]
fn the_endpoint_must_be_an_http_url_with_a_host() {
    for bad in [
        "nbg1.your-objectstorage.com",
        "ftp://x",
        "https://u:p@host",
        "https://h/?q=1",
    ] {
        let mut v = FULL.to_vec();
        v[0] = ("LAKKO_STORAGE_S3_ENDPOINT", bad);
        assert_eq!(
            S3Config::from_lookup(lookup(&v)).unwrap_err(),
            ConfigError::Endpoint,
            "{bad}"
        );
    }
}

#[test]
fn the_object_key_is_the_prefixed_lower_hex_id() {
    let id = [0xab; 32];
    assert_eq!(object_key(&id), format!("blobs/{}", "ab".repeat(32)));
}

#[test]
fn the_bucket_url_follows_the_style() {
    let c = S3Config::from_lookup(lookup(&FULL)).unwrap().unwrap();
    let s = S3Store::new(&c, PathBuf::from("/tmp/x")).unwrap();
    let url = s.bucket.object_url(&object_key(&[1; 32]));
    assert_eq!(
        url.host_str(),
        Some("lakko-blobs.nbg1.your-objectstorage.com")
    );
    assert_eq!(url.path(), format!("/blobs/{}", "01".repeat(32)));
}

// ============================================================================
// Against a test object store
// ============================================================================

static BUCKET_SEQ: AtomicU64 = AtomicU64::new(0);

/// A store over a fresh bucket on the test object store, or `None` when the
/// test variables are unset.
pub(crate) async fn test_store(temp_root: PathBuf) -> Option<S3Store> {
    let endpoint = std::env::var("LAKKO_STORAGE_TEST_S3_ENDPOINT").ok()?;
    let access = std::env::var("LAKKO_STORAGE_TEST_S3_ACCESS_KEY").ok()?;
    let secret = std::env::var("LAKKO_STORAGE_TEST_S3_SECRET_KEY").ok()?;
    let region =
        std::env::var("LAKKO_STORAGE_TEST_S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let bucket = format!(
        "lakko-test-{}-{}-{}",
        std::process::id(),
        nanos % 1_000_000_000_000,
        BUCKET_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let config = S3Config::from_lookup(lookup(&[
        ("LAKKO_STORAGE_S3_ENDPOINT", &endpoint),
        ("LAKKO_STORAGE_S3_BUCKET", &bucket),
        ("LAKKO_STORAGE_S3_REGION", &region),
        ("LAKKO_STORAGE_S3_ACCESS_KEY", &access),
        ("LAKKO_STORAGE_S3_SECRET_KEY", &secret),
        ("LAKKO_STORAGE_S3_PATH_STYLE", "true"),
    ]))
    .expect("test S3 config")
    .expect("set");
    let store = S3Store::new(&config, temp_root).expect("store");
    let url = store.bucket.create_bucket(&store.credentials).sign(PRESIGN);
    let resp = store.http.put(url).send().await.expect("create bucket");
    assert!(
        resp.status().is_success(),
        "create bucket: {}",
        resp.status()
    );
    store.check().await.expect("bucket check");
    Some(store)
}

fn temp_with(store: &S3Store, bytes: &[u8]) -> tempfile::NamedTempFile {
    let dir = store.temp_dir(&[0; 32]);
    std::fs::create_dir_all(&dir).unwrap();
    let mut t = tempfile::NamedTempFile::new_in(dir).unwrap();
    std::io::Write::write_all(&mut t, bytes).unwrap();
    t
}

async fn read_all(store: &S3Store, id: &BlobId) -> Option<Vec<u8>> {
    let (len, mut s) = store.open(id).await.unwrap()?;
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    assert_eq!(u64::try_from(out.len()).unwrap(), len);
    Some(out)
}

#[tokio::test]
async fn the_bucket_round_trips_heads_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let Some(store) = test_store(dir.path().join("up")).await else {
        return;
    };
    let id = [0x11; 32];
    assert_eq!(store.head(&id).await.unwrap(), None);
    assert!(store.open(&id).await.unwrap().is_none());
    let temp = temp_with(&store, b"ciphertext");
    let temp_path = temp.path().to_path_buf();
    store.put_file(&id, temp, 10).await.unwrap();
    // The temporary is gone once the bucket has the bytes.
    assert!(!temp_path.exists());
    assert_eq!(store.head(&id).await.unwrap(), Some(10));
    assert_eq!(read_all(&store, &id).await.unwrap(), b"ciphertext");
    store.ready().await.unwrap();
    store.delete(&id).await.unwrap();
    store.delete(&id).await.unwrap();
    assert_eq!(store.head(&id).await.unwrap(), None);
}

#[tokio::test]
async fn a_failed_bucket_write_still_removes_the_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let Some(store) = test_store(dir.path().join("up")).await else {
        return;
    };
    let temp = temp_with(&store, b"short");
    let temp_path = temp.path().to_path_buf();
    // A declared length the file does not have: the request fails.
    assert!(store.put_file(&[0x12; 32], temp, 999).await.is_err());
    assert!(!temp_path.exists());
    assert_eq!(store.head(&[0x12; 32]).await.unwrap(), None);
}

#[tokio::test]
async fn wrong_credentials_fail_the_startup_check() {
    let dir = tempfile::tempdir().unwrap();
    let Some(store) = test_store(dir.path().join("up")).await else {
        return;
    };
    let bad = S3Store {
        bucket: store.bucket.clone(),
        credentials: Credentials::new("nobody", "wrong-secret"),
        http: store.http.clone(),
        temp_root: dir.path().join("up"),
    };
    let err = bad.check().await.unwrap_err().to_string();
    assert!(!err.contains("wrong-secret"));
    assert!(err.contains("403"), "{err}");
}

#[tokio::test]
async fn the_migration_copies_local_files_once_and_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("blobs");
    let Some(store) = test_store(root.join(".lakko-upload")).await else {
        return;
    };
    let ids = [[0x21u8; 32], [0x22; 32], [0x23; 32]];
    for (i, id) in ids.iter().enumerate() {
        let p = blob_path(&root, id);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![b'x'; 100 * (i + 1)]).unwrap();
    }
    // Not blobs: skipped.
    std::fs::create_dir_all(root.join("backup")).unwrap();
    std::fs::write(root.join("backup").join("00".repeat(16)), b"b").unwrap();

    let first = migrate_files(&store, &root).await.unwrap();
    assert_eq!(
        first,
        MigrateReport {
            copied: 3,
            present: 0,
            failed: 0
        }
    );
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(store.head(id).await.unwrap(), Some(100 * (i as u64 + 1)));
        assert!(blob_path(&root, id).exists());
    }
    let again = migrate_files(&store, &root).await.unwrap();
    assert_eq!(
        again,
        MigrateReport {
            copied: 0,
            present: 3,
            failed: 0
        }
    );
}

#[tokio::test]
async fn a_50_mib_object_is_written_and_read_back_streamed() {
    let dir = tempfile::tempdir().unwrap();
    let Some(store) = test_store(dir.path().join("up")).await else {
        return;
    };
    let payload: Vec<u8> = (0..crate::MAX_BLOB_BYTES)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let id = [0x31; 32];
    let temp = temp_with(&store, &payload);
    let len = u64::try_from(payload.len()).unwrap();
    store.put_file(&id, temp, len).await.unwrap();
    assert_eq!(store.head(&id).await.unwrap(), Some(len));
    assert!(read_all(&store, &id).await.unwrap() == payload);
}
