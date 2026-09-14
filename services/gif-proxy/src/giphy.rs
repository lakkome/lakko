// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! GIPHY upstream client: search, trending, id lookup and media fetch.
//!
//! ## What never happens here
//!
//! - **No client-supplied URL is ever fetched.** A media fetch starts from a
//!   GIF id the proxy resolves through GIPHY's own API; the rendition URL comes
//!   out of GIPHY's JSON and is re-checked against [`MediaHostPolicy`] before
//!   the request, on every redirect hop, and on the final response URL.
//! - **No upstream body is read without a cap.** API JSON is capped at
//!   [`API_BODY_MAX_BYTES`]; media at the variant's cap. reqwest is built
//!   without decompression, so the cap bounds what is actually held.
//! - **No error carries a URL.** Every API URL contains the `api_key` query
//!   parameter and reqwest's own error `Display` embeds the URL, so a reqwest
//!   error is reduced to a [`UpstreamError`] kind before anything sees it.
//!
//! ## Rendition choice
//!
//! GIPHY's `images` object offers many renditions per GIF. The proxy picks,
//! in order, the first one that has a URL on an allowed host and a declared
//! size within the variant's cap:
//!
//! - **preview** (the picker grid): `fixed_width` WebP, `fixed_width` GIF,
//!   `fixed_width_small` WebP, `fixed_width_small` GIF, `preview_gif`.
//!   `fixed_width` is 200 px wide, which fills a two- or three-column grid
//!   cell; WebP comes first because an animated WebP of the same frames is
//!   smaller and every client decodes it.
//! - **full** (what the client downloads, encrypts and sends): `original` GIF,
//!   `downsized_medium` GIF, `downsized` GIF, `original` WebP. GIF comes first
//!   because the sent file becomes an attachment other clients render, and
//!   `image/gif` is the most widely supported animated type; the full rendition
//!   must also declare a width and height within [`MAX_DIMENSION`], the bound
//!   ADR-0047 §1 puts on the attachment envelope.
//!
//! The item's reported `width` / `height` are the full rendition's.

use std::time::Duration;

use axum::body::{Body, Bytes};
use futures::stream::{self, Stream};
use reqwest::header::{HeaderMap, ACCEPT, CONTENT_TYPE};
use reqwest::{redirect, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;

use crate::config::UpstreamSettings;

/// Results per page; fixed so a client cannot ask GIPHY for more.
pub(crate) const PAGE_SIZE: u32 = 24;
/// Highest accepted `offset` (GIPHY's documented search ceiling).
pub(crate) const MAX_OFFSET: u32 = 4999;
/// Content rating passed on every list request.
const RATING: &str = "pg-13";
/// Cap on a preview body.
pub(crate) const PREVIEW_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Cap on a full-size body.
pub(crate) const FULL_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Cap on a GIPHY API JSON body. A 24-item page is far below this.
pub(crate) const API_BODY_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Largest width or height a full rendition may declare (ADR-0047 §1).
pub(crate) const MAX_DIMENSION: u32 = 4096;
/// Longest accepted GIF id.
pub(crate) const MAX_ID_LEN: usize = 64;
/// Redirect hops a fetch may follow, each to an allowed host.
const MAX_REDIRECTS: usize = 3;
/// Connect-phase ceiling inside the total request timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Which hosts a media fetch may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaHostPolicy {
    /// Production: `https`, default port, no userinfo, host `giphy.com` or a
    /// subdomain of it.
    Giphy,
    /// Tests only: plain `http` to `127.0.0.1`, standing in for GIPHY.
    #[cfg(test)]
    Loopback,
}

impl MediaHostPolicy {
    pub(crate) fn allows(self, url: &Url) -> bool {
        if !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        match self {
            Self::Giphy => url.scheme() == "https" && url.port().is_none() && is_giphy_host(host),
            #[cfg(test)]
            Self::Loopback => url.scheme() == "http" && host == "127.0.0.1",
        }
    }
}

fn is_giphy_host(host: &str) -> bool {
    host == "giphy.com" || host.ends_with(".giphy.com")
}

/// A GIPHY id is ASCII alphanumeric; anything else is refused before it can
/// reach a URL path.
pub(crate) fn is_valid_gif_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The two media sizes a client can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variant {
    Preview,
    Full,
}

impl Variant {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "preview" => Some(Self::Preview),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Full => "full",
        }
    }

    pub(crate) const fn max_bytes(self) -> u64 {
        match self {
            Self::Preview => PREVIEW_MAX_BYTES,
            Self::Full => FULL_MAX_BYTES,
        }
    }
}

/// One chosen upstream file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenditionRef {
    pub(crate) url: String,
    pub(crate) declared_size: Option<u64>,
}

/// Both files for one GIF plus the full rendition's dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Renditions {
    pub(crate) preview: RenditionRef,
    pub(crate) full: RenditionRef,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Renditions {
    pub(crate) const fn for_variant(&self, variant: Variant) -> &RenditionRef {
        match variant {
            Variant::Preview => &self.preview,
            Variant::Full => &self.full,
        }
    }
}

/// One usable result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GifItem {
    pub(crate) id: String,
    pub(crate) renditions: Renditions,
}

/// One page of results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Page {
    pub(crate) items: Vec<GifItem>,
    pub(crate) next_offset: Option<u32>,
}

/// Why an upstream call failed. No variant carries a URL, a query or an id,
/// so a value is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UpstreamError {
    #[error("upstream request timed out")]
    Timeout,
    #[error("upstream transport error")]
    Transport,
    #[error("upstream answered HTTP {0}")]
    Status(u16),
    #[error("upstream has no such GIF")]
    NotFound,
    #[error("upstream body exceeded its size cap")]
    TooLarge,
    #[error("upstream body did not decode")]
    Decode,
    #[error("upstream offered no usable rendition")]
    NoRendition,
    #[error("rendition URL is not on an allowed media host")]
    DisallowedHost,
    #[error("upstream content type is not allowlisted")]
    DisallowedContentType,
}

fn classify(error: &reqwest::Error) -> UpstreamError {
    if error.is_timeout() {
        UpstreamError::Timeout
    } else {
        UpstreamError::Transport
    }
}

// ---- GIPHY JSON ------------------------------------------------------------
// Only the fields the proxy uses. GIPHY encodes sizes and dimensions as
// strings; `Value` plus `as_u64` accepts either form.

#[derive(Debug, Default, Deserialize)]
struct RawRendition {
    url: Option<String>,
    width: Option<Value>,
    height: Option<Value>,
    size: Option<Value>,
    webp: Option<String>,
    webp_size: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct RawImages {
    fixed_width: Option<RawRendition>,
    fixed_width_small: Option<RawRendition>,
    downsized: Option<RawRendition>,
    downsized_medium: Option<RawRendition>,
    original: Option<RawRendition>,
    preview_gif: Option<RawRendition>,
}

#[derive(Debug, Deserialize)]
struct RawGif {
    id: String,
    #[serde(default)]
    images: RawImages,
}

#[derive(Debug, Deserialize)]
struct RawList {
    #[serde(default)]
    data: Vec<Value>,
    #[serde(default)]
    pagination: Option<RawPagination>,
}

#[derive(Debug, Deserialize)]
struct RawPagination {
    total_count: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RawSingle {
    data: Option<Value>,
}

fn as_u64(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
enum Format {
    Gif,
    Webp,
}

fn pick(
    candidates: &[(Option<&RawRendition>, Format)],
    max_bytes: u64,
    policy: MediaHostPolicy,
    need_dimensions: bool,
) -> Option<(RenditionRef, Option<(u32, u32)>)> {
    candidates.iter().find_map(|(rendition, format)| {
        let rendition = (*rendition)?;
        let (url, size) = match format {
            Format::Gif => (rendition.url.as_deref(), rendition.size.as_ref()),
            Format::Webp => (rendition.webp.as_deref(), rendition.webp_size.as_ref()),
        };
        let url = url.filter(|u| !u.is_empty())?;
        let parsed = Url::parse(url).ok()?;
        if !policy.allows(&parsed) {
            return None;
        }
        let declared_size = as_u64(size);
        if declared_size.is_some_and(|s| s > max_bytes) {
            return None;
        }
        let dimensions = if need_dimensions {
            let dim = |v: Option<&Value>| {
                as_u64(v)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| (1..=MAX_DIMENSION).contains(n))
            };
            Some((
                dim(rendition.width.as_ref())?,
                dim(rendition.height.as_ref())?,
            ))
        } else {
            None
        };
        Some((
            RenditionRef {
                url: url.to_owned(),
                declared_size,
            },
            dimensions,
        ))
    })
}

fn select_renditions(images: &RawImages, policy: MediaHostPolicy) -> Option<Renditions> {
    let preview_candidates = [
        (images.fixed_width.as_ref(), Format::Webp),
        (images.fixed_width.as_ref(), Format::Gif),
        (images.fixed_width_small.as_ref(), Format::Webp),
        (images.fixed_width_small.as_ref(), Format::Gif),
        (images.preview_gif.as_ref(), Format::Gif),
    ];
    let full_candidates = [
        (images.original.as_ref(), Format::Gif),
        (images.downsized_medium.as_ref(), Format::Gif),
        (images.downsized.as_ref(), Format::Gif),
        (images.original.as_ref(), Format::Webp),
    ];
    let (preview, _) = pick(&preview_candidates, PREVIEW_MAX_BYTES, policy, false)?;
    let (full, dimensions) = pick(&full_candidates, FULL_MAX_BYTES, policy, true)?;
    let (width, height) = dimensions?;
    Some(Renditions {
        preview,
        full,
        width,
        height,
    })
}

fn item_from_value(value: Value, policy: MediaHostPolicy) -> Option<GifItem> {
    let raw: RawGif = serde_json::from_value(value).ok()?;
    if !is_valid_gif_id(&raw.id) {
        return None;
    }
    let renditions = select_renditions(&raw.images, policy)?;
    Some(GifItem {
        id: raw.id,
        renditions,
    })
}

/// The offset of the next page, or `None` when there is none a client may ask
/// for. Counts every entry GIPHY returned, including ones this proxy skipped,
/// so a skipped item never causes a repeat on the next page.
pub(crate) fn next_offset(
    requested: u32,
    returned: usize,
    total_count: Option<u64>,
) -> Option<u32> {
    if returned == 0 {
        return None;
    }
    let next = requested.checked_add(u32::try_from(returned).ok()?)?;
    if total_count.is_some_and(|total| u64::from(next) >= total) || next > MAX_OFFSET {
        return None;
    }
    Some(next)
}

/// Parse a GIPHY list (`trending` / `search`) response body.
pub(crate) fn parse_page(
    body: &[u8],
    requested_offset: u32,
    policy: MediaHostPolicy,
) -> Result<Page, UpstreamError> {
    let raw: RawList = serde_json::from_slice(body).map_err(|_| UpstreamError::Decode)?;
    let returned = raw.data.len();
    let total_count = raw
        .pagination
        .as_ref()
        .and_then(|p| as_u64(p.total_count.as_ref()));
    let items = raw
        .data
        .into_iter()
        .filter_map(|v| item_from_value(v, policy))
        .collect();
    Ok(Page {
        items,
        next_offset: next_offset(requested_offset, returned, total_count),
    })
}

/// Parse a GIPHY single-GIF (`/gifs/{id}`) response body for `expected_id`.
pub(crate) fn parse_single(
    body: &[u8],
    expected_id: &str,
    policy: MediaHostPolicy,
) -> Result<Renditions, UpstreamError> {
    let raw: RawSingle = serde_json::from_slice(body).map_err(|_| UpstreamError::Decode)?;
    let Some(data @ Value::Object(_)) = raw.data else {
        return Err(UpstreamError::NotFound);
    };
    let gif: RawGif = serde_json::from_value(data).map_err(|_| UpstreamError::Decode)?;
    if gif.id != expected_id {
        return Err(UpstreamError::NotFound);
    }
    select_renditions(&gif.images, policy).ok_or(UpstreamError::NoRendition)
}

/// The response content type, if it is one the proxy may serve.
pub(crate) fn allowed_content_type(headers: &HeaderMap) -> Option<&'static str> {
    let raw = headers.get(CONTENT_TYPE)?.to_str().ok()?;
    let essence = raw.split(';').next()?.trim().to_ascii_lowercase();
    match essence.as_str() {
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "video/mp4" => Some("video/mp4"),
        _ => None,
    }
}

/// A media body ready to hand to axum.
pub(crate) struct MediaBody {
    pub(crate) content_type: &'static str,
    pub(crate) content_length: Option<u64>,
    pub(crate) body: Body,
}

#[derive(Debug, thiserror::Error)]
enum MediaStreamError {
    #[error("media body exceeded its size cap")]
    TooLarge,
    #[error("media body read failed")]
    Upstream,
}

/// Stream `response` through, failing the stream once more than `max_bytes`
/// have passed. Only used when the upstream declared a length within the cap,
/// so this is defence in depth; the response headers are already sent by the
/// time it could trip, so it aborts the transfer rather than answering 502.
fn capped_stream(
    response: reqwest::Response,
    max_bytes: u64,
) -> impl Stream<Item = Result<Bytes, MediaStreamError>> {
    stream::try_unfold((response, 0_u64), move |(mut response, seen)| async move {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                let seen = seen.saturating_add(len);
                if seen > max_bytes {
                    tracing::warn!("gif media stream aborted: size cap exceeded");
                    return Err(MediaStreamError::TooLarge);
                }
                Ok(Some((chunk, (response, seen))))
            },
            Ok(None) => Ok(None),
            Err(_) => {
                tracing::warn!("gif media stream aborted: upstream read failed");
                Err(MediaStreamError::Upstream)
            },
        }
    })
}

/// Read a whole body, refusing it once it passes `max_bytes`.
async fn read_bounded(
    mut response: reqwest::Response,
    max_bytes: u64,
) -> Result<Vec<u8>, UpstreamError> {
    if response.content_length().is_some_and(|len| len > max_bytes) {
        return Err(UpstreamError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| classify(&e))? {
        let total = u64::try_from(body.len().saturating_add(chunk.len())).unwrap_or(u64::MAX);
        if total > max_bytes {
            return Err(UpstreamError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The configured GIPHY client.
pub(crate) struct GiphyClient {
    http: reqwest::Client,
    api_base: String,
    api_key: String,
    media_hosts: MediaHostPolicy,
}

impl GiphyClient {
    pub(crate) fn new(settings: &UpstreamSettings, api_key: String) -> Result<Self, UpstreamError> {
        let policy = settings.media_hosts;
        let redirects = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if policy.allows(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let http = reqwest::Client::builder()
            .timeout(settings.timeout)
            .connect_timeout(CONNECT_TIMEOUT.min(settings.timeout))
            .redirect(redirects)
            // A redirect must not carry the previous URL (which holds the
            // api_key) to the next host in a Referer header.
            .referer(false)
            .user_agent(concat!("lakko-gif-proxy/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| UpstreamError::Transport)?;
        Ok(Self {
            http,
            api_base: settings.api_base.trim_end_matches('/').to_owned(),
            api_key,
            media_hosts: policy,
        })
    }

    pub(crate) async fn trending(&self, offset: u32) -> Result<Page, UpstreamError> {
        let url = format!("{}/trending", self.api_base);
        let body = self
            .get_api(&url, &[("offset", &offset.to_string())])
            .await?;
        parse_page(&body, offset, self.media_hosts)
    }

    pub(crate) async fn search(&self, query: &str, offset: u32) -> Result<Page, UpstreamError> {
        let url = format!("{}/search", self.api_base);
        let body = self
            .get_api(&url, &[("q", query), ("offset", &offset.to_string())])
            .await?;
        parse_page(&body, offset, self.media_hosts)
    }

    /// Resolve an id. The caller has already checked [`is_valid_gif_id`], so
    /// the id cannot alter the URL path.
    pub(crate) async fn lookup(&self, id: &str) -> Result<Renditions, UpstreamError> {
        if !is_valid_gif_id(id) {
            return Err(UpstreamError::NotFound);
        }
        let url = format!("{}/{id}", self.api_base);
        let response = self
            .http
            .get(&url)
            .query(&[("api_key", self.api_key.as_str())])
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| classify(&e))?;
        match response.status() {
            StatusCode::OK => {},
            StatusCode::NOT_FOUND => return Err(UpstreamError::NotFound),
            other => return Err(UpstreamError::Status(other.as_u16())),
        }
        let body = read_bounded(response, API_BODY_MAX_BYTES).await?;
        parse_single(&body, id, self.media_hosts)
    }

    async fn get_api(&self, url: &str, params: &[(&str, &str)]) -> Result<Vec<u8>, UpstreamError> {
        let limit = PAGE_SIZE.to_string();
        let response = self
            .http
            .get(url)
            .query(&[
                ("api_key", self.api_key.as_str()),
                ("limit", limit.as_str()),
                ("rating", RATING),
            ])
            .query(params)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| classify(&e))?;
        if response.status() != StatusCode::OK {
            return Err(UpstreamError::Status(response.status().as_u16()));
        }
        read_bounded(response, API_BODY_MAX_BYTES).await
    }

    /// Fetch one rendition. A declared length over the cap, or an undeclared
    /// length that reads past it, is refused before any header is returned.
    pub(crate) async fn fetch_media(
        &self,
        rendition: &RenditionRef,
        max_bytes: u64,
    ) -> Result<MediaBody, UpstreamError> {
        let url = Url::parse(&rendition.url).map_err(|_| UpstreamError::DisallowedHost)?;
        if !self.media_hosts.allows(&url) {
            return Err(UpstreamError::DisallowedHost);
        }
        let response = self.http.get(url).send().await.map_err(|e| classify(&e))?;
        if !self.media_hosts.allows(response.url()) {
            return Err(UpstreamError::DisallowedHost);
        }
        if response.status() != StatusCode::OK {
            return Err(UpstreamError::Status(response.status().as_u16()));
        }
        let content_type =
            allowed_content_type(response.headers()).ok_or(UpstreamError::DisallowedContentType)?;
        match response.content_length() {
            Some(len) if len > max_bytes => Err(UpstreamError::TooLarge),
            Some(len) => Ok(MediaBody {
                content_type,
                content_length: Some(len),
                body: Body::from_stream(capped_stream(response, max_bytes)),
            }),
            None => {
                let bytes = read_bounded(response, max_bytes).await?;
                Ok(MediaBody {
                    content_type,
                    content_length: u64::try_from(bytes.len()).ok(),
                    body: Body::from(bytes),
                })
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    /// A trimmed but structurally faithful GIPHY search response: string-typed
    /// numbers, `webp` beside `url`, analytics query strings on media URLs,
    /// and one item whose `original` is too large for the full cap.
    const FIXTURE: &str = r#"{
      "data": [
        {
          "type": "gif",
          "id": "3o7TKtnuHOHHUjR38Y",
          "url": "https://giphy.com/gifs/cat-3o7TKtnuHOHHUjR38Y",
          "images": {
            "fixed_width": {
              "height": "150", "width": "200", "size": "412345",
              "url": "https://media1.giphy.com/media/3o7TKtnuHOHHUjR38Y/200w.gif?cid=abc&rid=200w.gif&ct=g",
              "webp_size": "98765",
              "webp": "https://media1.giphy.com/media/3o7TKtnuHOHHUjR38Y/200w.webp?cid=abc&rid=200w.webp&ct=g"
            },
            "fixed_width_small": {
              "height": "75", "width": "100", "size": "90000",
              "url": "https://media1.giphy.com/media/3o7TKtnuHOHHUjR38Y/100w.gif?cid=abc&ct=g"
            },
            "original": {
              "height": "360", "width": "480", "size": "2345678", "frames": "42",
              "url": "https://media1.giphy.com/media/3o7TKtnuHOHHUjR38Y/giphy.gif?cid=abc&rid=giphy.gif&ct=g",
              "webp": "https://media1.giphy.com/media/3o7TKtnuHOHHUjR38Y/giphy.webp?cid=abc&ct=g",
              "webp_size": "1234567"
            },
            "downsized": {
              "height": "270", "width": "360", "size": "1500000",
              "url": "https://media1.giphy.com/media/3o7TKtnuHOHHUjR38Y/giphy-downsized.gif?cid=abc&ct=g"
            }
          }
        },
        {
          "type": "gif",
          "id": "l0HlBO7eyXzSZkJri",
          "images": {
            "fixed_width": {
              "height": "113", "width": "200", "size": "300000",
              "url": "https://media2.giphy.com/media/l0HlBO7eyXzSZkJri/200w.gif?cid=abc&ct=g"
            },
            "original": {
              "height": "281", "width": "500", "size": "12000000",
              "url": "https://media2.giphy.com/media/l0HlBO7eyXzSZkJri/giphy.gif?cid=abc&ct=g"
            },
            "downsized_medium": {
              "height": "281", "width": "500", "size": "4100000",
              "url": "https://media2.giphy.com/media/l0HlBO7eyXzSZkJri/giphy-downsized-medium.gif?cid=abc&ct=g"
            }
          }
        },
        {
          "type": "gif",
          "id": "not/a/valid/id",
          "images": { "original": { "url": "https://media.giphy.com/x.gif", "width": "1", "height": "1" } }
        },
        {
          "type": "gif",
          "id": "evilHostOnly",
          "images": {
            "fixed_width": { "url": "https://evil.example/200w.gif", "width": "200", "height": "150" },
            "original": { "url": "https://media.giphy.com.evil.example/x.gif", "width": "480", "height": "360" }
          }
        }
      ],
      "pagination": { "total_count": 5000, "count": 4, "offset": 0 },
      "meta": { "status": 200, "msg": "OK" }
    }"#;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("url")
    }

    #[test]
    fn giphy_policy_accepts_only_https_giphy_hosts() {
        let p = MediaHostPolicy::Giphy;
        assert!(p.allows(&url("https://media1.giphy.com/media/x/giphy.gif?cid=1")));
        assert!(p.allows(&url("https://giphy.com/x.gif")));
        assert!(p.allows(&url("https://i.giphy.com:443/x.gif")));
        assert!(!p.allows(&url("http://media1.giphy.com/x.gif")), "http");
        assert!(
            !p.allows(&url("https://media1.giphy.com:8443/x.gif")),
            "port"
        );
        assert!(
            !p.allows(&url("https://notgiphy.com/x.gif")),
            "suffix without dot"
        );
        assert!(
            !p.allows(&url("https://giphy.com.evil.example/x.gif")),
            "prefix"
        );
        assert!(
            !p.allows(&url("https://user:pw@media.giphy.com/x.gif")),
            "userinfo"
        );
        assert!(!p.allows(&url("https://127.0.0.1/x.gif")), "ip");
        assert!(!p.allows(&url("https://[::1]/x.gif")), "ipv6");
        assert!(!p.allows(&url("file:///etc/passwd")), "file");
        assert!(
            !p.allows(&url("https://media.giphy.com./x.gif")),
            "trailing dot"
        );
    }

    #[test]
    fn gif_ids_are_strictly_alphanumeric() {
        assert!(is_valid_gif_id("3o7TKtnuHOHHUjR38Y"));
        assert!(is_valid_gif_id(&"a".repeat(MAX_ID_LEN)));
        for bad in [
            "",
            "abc-def",
            "abc_def",
            "abc/def",
            "..",
            "abc%2F",
            "abc def",
            "abc?x=1",
            "\u{0430}bc", // Cyrillic a
        ] {
            assert!(!is_valid_gif_id(bad), "{bad:?}");
        }
        assert!(!is_valid_gif_id(&"a".repeat(MAX_ID_LEN + 1)));
    }

    #[test]
    fn variants_parse_exactly() {
        assert_eq!(Variant::parse("preview"), Some(Variant::Preview));
        assert_eq!(Variant::parse("full"), Some(Variant::Full));
        assert_eq!(Variant::parse("Preview"), None);
        assert_eq!(Variant::parse("original"), None);
        assert_eq!(Variant::Preview.max_bytes(), 2 * 1024 * 1024);
        assert_eq!(Variant::Full.max_bytes(), 8 * 1024 * 1024);
    }

    #[test]
    fn fixture_renditions_are_selected_in_preference_order() {
        let page = parse_page(FIXTURE.as_bytes(), 0, MediaHostPolicy::Giphy).expect("page");
        assert_eq!(page.items.len(), 2, "invalid id and bad-host items dropped");

        let first = &page.items[0];
        assert_eq!(first.id, "3o7TKtnuHOHHUjR38Y");
        // Preview: fixed_width WebP ahead of fixed_width GIF.
        assert!(first.renditions.preview.url.contains("/200w.webp"));
        assert_eq!(first.renditions.preview.declared_size, Some(98_765));
        // Full: original GIF (within 8 MiB), with its dimensions.
        assert!(first.renditions.full.url.contains("/giphy.gif"));
        assert_eq!(
            (first.renditions.width, first.renditions.height),
            (480, 360)
        );

        let second = &page.items[1];
        // No WebP: the fixed_width GIF.
        assert!(second.renditions.preview.url.contains("/200w.gif"));
        // original is 12 MB: falls back to downsized_medium.
        assert!(second.renditions.full.url.contains("downsized-medium"));
    }

    #[test]
    fn next_offset_follows_pagination() {
        let page = parse_page(FIXTURE.as_bytes(), 0, MediaHostPolicy::Giphy).expect("page");
        // Four entries returned (two skipped) still advance by four.
        assert_eq!(page.next_offset, Some(4));

        assert_eq!(next_offset(0, 24, Some(1000)), Some(24));
        assert_eq!(next_offset(0, 0, Some(1000)), None, "empty page");
        assert_eq!(next_offset(976, 24, Some(1000)), None, "reached total");
        assert_eq!(next_offset(4990, 24, None), None, "past MAX_OFFSET");
        assert_eq!(next_offset(4975, 24, None), Some(4999));
        assert_eq!(next_offset(48, 24, None), Some(72), "no total_count");
    }

    #[test]
    fn a_too_large_preview_or_missing_dimensions_skips_a_candidate() {
        let body = r#"{"data":{"id":"abc","images":{
            "fixed_width":{"url":"https://media.giphy.com/a.gif","size":"3000000"},
            "fixed_width_small":{"url":"https://media.giphy.com/b.gif","size":"1000"},
            "original":{"url":"https://media.giphy.com/c.gif","size":"1000"},
            "downsized":{"url":"https://media.giphy.com/d.gif","width":"320","height":"240"}
        }}}"#;
        let r = parse_single(body.as_bytes(), "abc", MediaHostPolicy::Giphy).expect("renditions");
        assert!(r.preview.url.ends_with("/b.gif"));
        assert!(r.full.url.ends_with("/d.gif"));
        assert_eq!((r.width, r.height), (320, 240));
    }

    #[test]
    fn oversized_dimensions_are_not_offered_as_full() {
        let body = r#"{"data":{"id":"abc","images":{
            "fixed_width":{"url":"https://media.giphy.com/a.gif"},
            "original":{"url":"https://media.giphy.com/c.gif","width":"5000","height":"100"}
        }}}"#;
        assert_eq!(
            parse_single(body.as_bytes(), "abc", MediaHostPolicy::Giphy),
            Err(UpstreamError::NoRendition)
        );
    }

    #[test]
    fn single_lookup_edge_cases() {
        let p = MediaHostPolicy::Giphy;
        assert_eq!(
            parse_single(br#"{"data":[],"meta":{"status":404}}"#, "abc", p),
            Err(UpstreamError::NotFound)
        );
        assert_eq!(
            parse_single(b"not json", "abc", p),
            Err(UpstreamError::Decode)
        );
        let other_id = r#"{"data":{"id":"xyz","images":{
            "fixed_width":{"url":"https://media.giphy.com/a.gif"},
            "original":{"url":"https://media.giphy.com/c.gif","width":"10","height":"10"}}}}"#;
        assert_eq!(
            parse_single(other_id.as_bytes(), "abc", p),
            Err(UpstreamError::NotFound)
        );
    }

    #[test]
    fn content_type_allowlist() {
        let mut h = HeaderMap::new();
        for (raw, expected) in [
            ("image/gif", Some("image/gif")),
            ("IMAGE/WEBP", Some("image/webp")),
            ("video/mp4; codecs=avc1", Some("video/mp4")),
            ("text/html", None),
            ("image/svg+xml", None),
            ("image/gifx", None),
        ] {
            h.insert(CONTENT_TYPE, HeaderValue::from_static(raw));
            assert_eq!(allowed_content_type(&h), expected, "{raw}");
        }
        h.remove(CONTENT_TYPE);
        assert_eq!(allowed_content_type(&h), None);
    }

    fn response_of(len: usize) -> reqwest::Response {
        reqwest::Response::from(axum::http::Response::new(vec![7_u8; len]))
    }

    async fn drain(max_bytes: u64, len: usize) -> Result<usize, MediaStreamError> {
        use futures::TryStreamExt as _;
        let chunks: Vec<Bytes> = capped_stream(response_of(len), max_bytes)
            .try_collect()
            .await?;
        Ok(chunks.iter().map(Bytes::len).sum())
    }

    #[tokio::test]
    async fn the_stream_guard_fails_a_body_past_its_cap() {
        // The guard behind a declared length: an upstream that sends more than
        // the cap fails the stream instead of passing the extra bytes.
        assert_eq!(drain(100, 100).await.expect("at cap"), 100);
        assert!(matches!(
            drain(100, 101).await,
            Err(MediaStreamError::TooLarge)
        ));
    }

    #[tokio::test]
    async fn bounded_reads_refuse_past_the_cap() {
        assert_eq!(
            read_bounded(response_of(64), 64)
                .await
                .expect("at cap")
                .len(),
            64
        );
        assert_eq!(
            read_bounded(response_of(65), 64).await,
            Err(UpstreamError::TooLarge)
        );
    }

    #[test]
    fn upstream_error_text_carries_no_url() {
        for e in [
            UpstreamError::Timeout,
            UpstreamError::Transport,
            UpstreamError::Status(500),
            UpstreamError::NotFound,
            UpstreamError::TooLarge,
            UpstreamError::Decode,
            UpstreamError::NoRendition,
            UpstreamError::DisallowedHost,
            UpstreamError::DisallowedContentType,
        ] {
            let text = e.to_string();
            assert!(!text.contains("http"), "{text}");
            assert!(!text.contains("api_key"), "{text}");
        }
    }
}
