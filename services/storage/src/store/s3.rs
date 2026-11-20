// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Blob bytes in an S3-compatible bucket (ADR-0065, amendment of
//! 2026-11-19).
//!
//! # Configuration
//!
//! | Variable | Meaning |
//! |---|---|
//! | `LAKKO_STORAGE_S3_ENDPOINT` | `https://nbg1.your-objectstorage.com` (no bucket in it) |
//! | `LAKKO_STORAGE_S3_BUCKET` | the bucket name |
//! | `LAKKO_STORAGE_S3_REGION` | the signing region (Hetzner: `nbg1`) |
//! | `LAKKO_STORAGE_S3_ACCESS_KEY` | access key id |
//! | `LAKKO_STORAGE_S3_SECRET_KEY` | secret key; never logged |
//! | `LAKKO_STORAGE_S3_PATH_STYLE` | `true` / `false` (default `false`, virtual-host style); MinIO needs `true` |
//!
//! All unset: the local directory is used. Some set and some not: startup
//! fails naming the missing ones.
//!
//! # Object keys
//!
//! A blob is the object `blobs/<64 lower-case hex of its id>`. Nothing else
//! is in the key: no account, no node, no date. The prefix leaves room for
//! other object kinds (ADR-0064's vault objects) in the same bucket.
//!
//! # Requests
//!
//! Each request is a URL presigned by `rusty-s3` (query-string SigV4 with an
//! unsigned payload) sent with `reqwest`; redirects are not followed (a
//! redirect would carry the signed URL to another host, and a streamed body
//! cannot be re-sent). A `PUT` streams the verified local file with its
//! `Content-Length`; a `GET` is streamed through to the client. Error text
//! never carries a URL (the URL holds the access key id and the signature).

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::header::CONTENT_LENGTH;
use reqwest::{StatusCode, Url};
use rusty_s3::{Bucket, Credentials, S3Action as _, UrlStyle};

use super::{BlobStore, BlobStream, StoreError};
use crate::index::BlobId;

/// The variables, in the order a missing one is named.
pub(crate) const VARS: [&str; 6] = [
    "LAKKO_STORAGE_S3_ENDPOINT",
    "LAKKO_STORAGE_S3_BUCKET",
    "LAKKO_STORAGE_S3_REGION",
    "LAKKO_STORAGE_S3_ACCESS_KEY",
    "LAKKO_STORAGE_S3_SECRET_KEY",
    "LAKKO_STORAGE_S3_PATH_STYLE",
];

/// The object key prefix of blobs.
const BLOB_PREFIX: &str = "blobs/";

/// A key that is never a blob, for the startup check: it must not exist.
const ABSENT_PROBE_KEY: &str = "lakko-probe/absent";

/// How long a presigned URL is valid. The bucket checks the signature when
/// a request starts, so this bounds only the gap between signing and
/// sending.
const PRESIGN: Duration = Duration::from_secs(15 * 60);

/// Connecting to the bucket. Chosen, not measured.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A stalled read of a response (reset by every chunk). Chosen.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// The bucket `HEAD` of `/ready`, which a probe waits for. Chosen.
const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Whole `HEAD` / `DELETE` requests. Chosen.
const SMALL_TIMEOUT: Duration = Duration::from_secs(30);
/// A whole `PUT` of at most 50 MiB inside one network zone. Chosen.
const PUT_TIMEOUT: Duration = Duration::from_secs(600);

/// The bucket configuration.
pub(crate) struct S3Config {
    endpoint: Url,
    bucket: String,
    region: String,
    access_key: String,
    secret_key: String,
    path_style: bool,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint.as_str())
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("access_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("path_style", &self.path_style)
            .finish()
    }
}

/// A configuration refusal; names variables, never their values.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ConfigError {
    /// Some variables set, these not.
    #[error("S3 storage is partly configured; missing: {}", .0.join(", "))]
    Missing(Vec<&'static str>),
    /// The endpoint is not an `http(s)` URL with a host and no path.
    #[error("LAKKO_STORAGE_S3_ENDPOINT must be an http(s) URL with a host")]
    Endpoint,
    /// The path-style flag is not a boolean.
    #[error("LAKKO_STORAGE_S3_PATH_STYLE must be true or false")]
    PathStyle,
    /// The bucket URL cannot be built (e.g. a virtual-host name that is not
    /// a host name).
    #[error("the bucket URL cannot be built: {0}")]
    Bucket(String),
}

impl S3Config {
    /// Read the configuration through `get` (the environment, or a map in
    /// tests). `Ok(None)` when every variable is unset or blank.
    pub(crate) fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        let values: Vec<Option<String>> = VARS
            .iter()
            .map(|v| {
                get(v)
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
            })
            .collect();
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        // The path-style flag is optional; the other five are required.
        let missing: Vec<&'static str> = VARS[..5]
            .iter()
            .zip(&values)
            .filter(|(_, v)| v.is_none())
            .map(|(n, _)| *n)
            .collect();
        if !missing.is_empty() {
            return Err(ConfigError::Missing(missing));
        }
        let take = |i: usize| values[i].clone().unwrap_or_default();
        let endpoint = Url::parse(&take(0)).map_err(|_| ConfigError::Endpoint)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || endpoint.query().is_some()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(ConfigError::Endpoint);
        }
        let path_style = match values[5].as_deref() {
            None => false,
            Some(v) => match v.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => true,
                "false" | "0" | "no" => false,
                _ => return Err(ConfigError::PathStyle),
            },
        };
        Ok(Some(Self {
            endpoint,
            bucket: take(1),
            region: take(2),
            access_key: take(3),
            secret_key: take(4),
            path_style,
        }))
    }

    /// Read the configuration from the process environment.
    pub(crate) fn from_env() -> Result<Option<Self>, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
}

/// The object key of blob `id`.
pub(crate) fn object_key(id: &BlobId) -> String {
    format!("{BLOB_PREFIX}{}", hex::encode(id))
}

/// Blob bytes in a bucket.
pub(crate) struct S3Store {
    bucket: Bucket,
    credentials: Credentials,
    http: reqwest::Client,
    temp_root: PathBuf,
}

/// Describe a transport error without its URL.
fn transport(op: &str, e: &reqwest::Error) -> StoreError {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "could not connect"
    } else if e.is_body() {
        "body failed"
    } else {
        "request failed"
    };
    StoreError::Remote(format!("{op} {kind}"))
}

fn unexpected(op: &str, status: StatusCode) -> StoreError {
    StoreError::Remote(format!("{op} answered {}", status.as_u16()))
}

/// The `Content-Length` header of a response (not `content_length()`, which
/// reports the body size hint: zero for `HEAD`).
fn header_length(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

impl S3Store {
    /// A store over the configured bucket. Upload temporaries go to
    /// `temp_root` (a local directory, created here).
    pub(crate) fn new(config: &S3Config, temp_root: PathBuf) -> Result<Self, ConfigError> {
        let style = if config.path_style {
            UrlStyle::Path
        } else {
            UrlStyle::VirtualHost
        };
        let bucket = Bucket::new(
            config.endpoint.clone(),
            style,
            config.bucket.clone(),
            config.region.clone(),
        )
        .map_err(|e| ConfigError::Bucket(e.to_string()))?;
        let credentials = Credentials::new(config.access_key.clone(), config.secret_key.clone());
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ConfigError::Bucket(e.without_url().to_string()))?;
        Ok(Self {
            bucket,
            credentials,
            http,
            temp_root,
        })
    }

    /// The bucket name, for logs.
    pub(crate) fn bucket_name(&self) -> &str {
        self.bucket.name()
    }

    /// Startup check: the bucket answers `HEAD` with 200 (it exists and the
    /// credentials are accepted) and a key that is never written answers 404
    /// (objects can be read). Creates the local temporary directory.
    pub(crate) async fn check(&self) -> Result<(), StoreError> {
        tokio::fs::create_dir_all(&self.temp_root).await?;
        self.head_bucket(SMALL_TIMEOUT).await?;
        match self.head_key(ABSENT_PROBE_KEY).await? {
            None => Ok(()),
            Some(_) => Err(StoreError::Remote(
                "the probe key that must be absent exists".into(),
            )),
        }
    }

    async fn head_bucket(&self, timeout: Duration) -> Result<(), StoreError> {
        let url = self
            .bucket
            .head_bucket(Some(&self.credentials))
            .sign(PRESIGN);
        let resp = self
            .http
            .head(url)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| transport("bucket HEAD", &e))?;
        match resp.status() {
            StatusCode::OK => Ok(()),
            StatusCode::NOT_FOUND => Err(StoreError::Remote(
                "bucket HEAD answered 404: the bucket does not exist".into(),
            )),
            StatusCode::FORBIDDEN => Err(StoreError::Remote(
                "bucket HEAD answered 403: the credentials are refused".into(),
            )),
            s => Err(unexpected("bucket HEAD", s)),
        }
    }

    async fn head_key(&self, key: &str) -> Result<Option<u64>, StoreError> {
        let url = self
            .bucket
            .head_object(Some(&self.credentials), key)
            .sign(PRESIGN);
        let resp = self
            .http
            .head(url)
            .timeout(SMALL_TIMEOUT)
            .send()
            .await
            .map_err(|e| transport("object HEAD", &e))?;
        match resp.status() {
            StatusCode::OK => header_length(&resp)
                .map(Some)
                .ok_or_else(|| StoreError::Remote("object HEAD without a length".into())),
            StatusCode::NOT_FOUND => Ok(None),
            s => Err(unexpected("object HEAD", s)),
        }
    }

    /// Write the local file `path` of `len` bytes as `id`, streamed. The
    /// file is left in place (the migration copies; the upload path removes
    /// its temporary afterwards).
    pub(crate) async fn put_path(
        &self,
        id: &BlobId,
        path: &Path,
        len: u64,
    ) -> Result<(), StoreError> {
        let file = tokio::fs::File::open(path).await?;
        let key = object_key(id);
        let url = self
            .bucket
            .put_object(Some(&self.credentials), &key)
            .sign(PRESIGN);
        let body = reqwest::Body::wrap_stream(crate::stream::ReaderStream::new(file));
        let resp = self
            .http
            .put(url)
            .header(CONTENT_LENGTH, len)
            .body(body)
            .timeout(PUT_TIMEOUT)
            .send()
            .await
            .map_err(|e| transport("object PUT", &e))?;
        match resp.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            s => Err(unexpected("object PUT", s)),
        }
    }
}

#[async_trait]
impl BlobStore for S3Store {
    fn name(&self) -> &'static str {
        "s3"
    }

    fn temp_dir(&self, _id: &BlobId) -> PathBuf {
        self.temp_root.clone()
    }

    async fn put_file(
        &self,
        id: &BlobId,
        temp: tempfile::NamedTempFile,
        len: u64,
    ) -> Result<(), StoreError> {
        let result = self.put_path(id, temp.path(), len).await;
        // The temporary is removed whether the bucket took it or not.
        if let Err(e) = temp.close() {
            tracing::warn!(error = %e, "remove an upload temporary");
        }
        result
    }

    async fn open(&self, id: &BlobId) -> Result<Option<(u64, BlobStream)>, StoreError> {
        let key = object_key(id);
        let url = self
            .bucket
            .get_object(Some(&self.credentials), &key)
            .sign(PRESIGN);
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| transport("object GET", &e))?;
        match resp.status() {
            StatusCode::OK => {
                let len = header_length(&resp)
                    .ok_or_else(|| StoreError::Remote("object GET without a length".into()))?;
                let stream = resp
                    .bytes_stream()
                    .map(|r| r.map_err(|e| std::io::Error::other(e.without_url())));
                let stream: BlobStream = Box::pin(stream);
                Ok(Some((len, stream)))
            },
            StatusCode::NOT_FOUND => Ok(None),
            s => Err(unexpected("object GET", s)),
        }
    }

    async fn head(&self, id: &BlobId) -> Result<Option<u64>, StoreError> {
        self.head_key(&object_key(id)).await
    }

    async fn delete(&self, id: &BlobId) -> Result<(), StoreError> {
        let key = object_key(id);
        let url = self
            .bucket
            .delete_object(Some(&self.credentials), &key)
            .sign(PRESIGN);
        let resp = self
            .http
            .delete(url)
            .timeout(SMALL_TIMEOUT)
            .send()
            .await
            .map_err(|e| transport("object DELETE", &e))?;
        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(()),
            s => Err(unexpected("object DELETE", s)),
        }
    }

    async fn ready(&self) -> Result<(), StoreError> {
        super::probe_writable(&self.temp_root).await?;
        self.head_bucket(READY_TIMEOUT).await
    }
}

/// What [`migrate_files`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MigrateReport {
    /// Files written to the bucket and checked.
    pub(crate) copied: u64,
    /// Files already in the bucket at the same length.
    pub(crate) present: u64,
    /// Files whose copy failed or did not check.
    pub(crate) failed: u64,
}

/// Copy every blob file under `root` into the bucket: `HEAD`; when absent or
/// of another length, `PUT`, then `HEAD` again and compare the length.
/// Deletes nothing locally.
pub(crate) async fn migrate_files(store: &S3Store, root: &Path) -> anyhow::Result<MigrateReport> {
    let owned = root.to_path_buf();
    let files =
        tokio::task::spawn_blocking(move || crate::sweep::list_blob_files(&owned)).await??;
    let mut report = MigrateReport::default();
    for (id, len) in files {
        match store.head(&id).await {
            Ok(Some(n)) if n == len => {
                report.present += 1;
                continue;
            },
            Ok(_) => {},
            Err(e) => {
                tracing::warn!(error = %e, "migrate: HEAD before the copy failed");
                report.failed += 1;
                continue;
            },
        }
        let path = super::blob_path(root, &id);
        if let Err(e) = store.put_path(&id, &path, len).await {
            tracing::warn!(error = %e, "migrate: PUT failed");
            report.failed += 1;
            continue;
        }
        match store.head(&id).await {
            Ok(Some(n)) if n == len => report.copied += 1,
            Ok(_) => {
                tracing::warn!("migrate: the copy does not have the file's length");
                report.failed += 1;
            },
            Err(e) => {
                tracing::warn!(error = %e, "migrate: HEAD after the copy failed");
                report.failed += 1;
            },
        }
    }
    Ok(report)
}

#[cfg(test)]
pub(crate) mod tests;
