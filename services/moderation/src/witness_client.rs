// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The witness client of the moderation log (ADR-0099 section 5).
//!
//! The same fan-out the KT log runs in `services/transparency`: after the
//! operator signs a new head, each configured witness is asked to co-sign it
//! (`POST /witness/cosign` with `"log": "moderation"`), handed a consistency
//! proof from that witness's own last co-signed size. Best effort: a witness
//! that is down, slow or refusing contributes nothing this round, and a
//! bundle is never served for a head it does not bind.
//!
//! Configuration: `LAKKO_MODERATION_WITNESS_URLS` and
//! `LAKKO_MODERATION_WITNESS_COSIGN_SECRETS`, comma lists, one to one by
//! position; `LAKKO_MODERATION_WITNESS_TIMEOUT_SECS` (default 10). Neither
//! list set: no witness, and roots are served with an empty bundle.

use std::time::Duration;

use anyhow::Context as _;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_transparency::{LogId, SignedRoot, TreeHash};
use serde::{Deserialize, Serialize};

/// Default total per-request timeout (seconds).
const DEFAULT_TIMEOUT_SECS: u64 = 10;
/// Connect-phase ceiling (seconds).
const CONNECT_TIMEOUT_SECS: u64 = 5;

/// One configured witness.
#[derive(Debug)]
pub struct WitnessClient {
    base_url: String,
    cosign_secret_b64: String,
    http: reqwest::Client,
}

#[derive(Serialize)]
struct SignedRootWire {
    tree_root_b64: String,
    log_size: u64,
    timestamp_unix: u64,
    signature_b64: String,
}

#[derive(Serialize)]
struct CosignReq {
    log: &'static str,
    new_root: SignedRootWire,
    consistency_proof_b64: Vec<String>,
}

#[derive(Deserialize)]
struct CosignRes {
    cosig_b64: String,
}

impl WitnessClient {
    /// The configured witnesses (empty when none).
    ///
    /// # Errors
    ///
    /// A list set without the other, a count mismatch, or an empty entry.
    pub fn list_from_env() -> anyhow::Result<Vec<Self>> {
        let endpoints = parse_endpoints(
            std::env::var("LAKKO_MODERATION_WITNESS_URLS").ok(),
            std::env::var("LAKKO_MODERATION_WITNESS_COSIGN_SECRETS").ok(),
        )?;
        let timeout = std::env::var("LAKKO_MODERATION_WITNESS_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        endpoints
            .into_iter()
            .map(|(url, secret)| Self::new(&url, &secret, timeout))
            .collect()
    }

    /// One client for `base_url`, presenting `cosign_secret_b64`.
    ///
    /// # Errors
    ///
    /// The HTTP client does not build.
    pub fn new(base_url: &str, cosign_secret_b64: &str, timeout_secs: u64) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(timeout_secs.min(CONNECT_TIMEOUT_SECS)))
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("build the witness HTTP client")?;
        Ok(Self {
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            cosign_secret_b64: cosign_secret_b64.trim().to_string(),
            http,
        })
    }

    /// Ask the witness to co-sign `root`, given a consistency proof from its
    /// last co-signed size.
    ///
    /// # Errors
    ///
    /// Network failure, a non-2xx answer, or a malformed response.
    pub async fn cosign(&self, root: &SignedRoot, proof: &[TreeHash]) -> anyhow::Result<Vec<u8>> {
        let req = CosignReq {
            log: LogId::Moderation.wire_name(),
            new_root: SignedRootWire {
                tree_root_b64: B64.encode(root.tree_root),
                log_size: root.log_size,
                timestamp_unix: root.timestamp_unix,
                signature_b64: B64.encode(root.signature.to_bytes()),
            },
            consistency_proof_b64: proof.iter().map(|h| B64.encode(h)).collect(),
        };
        let resp = self
            .http
            .post(format!("{}/witness/cosign", self.base_url))
            .bearer_auth(&self.cosign_secret_b64)
            .json(&req)
            .send()
            .await
            .context("witness cosign request")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("witness cosign returned HTTP {status}");
        }
        let res: CosignRes = resp.json().await.context("decode the cosign response")?;
        B64.decode(res.cosig_b64.as_bytes())
            .context("the cosig is not base64")
    }
}

/// The `(url, secret)` pairs of the two comma lists.
fn parse_endpoints(
    urls: Option<String>,
    secrets: Option<String>,
) -> anyhow::Result<Vec<(String, String)>> {
    let urls = urls.filter(|s| !s.trim().is_empty());
    let secrets = secrets.filter(|s| !s.trim().is_empty());
    match (urls, secrets) {
        (None, None) => Ok(Vec::new()),
        (Some(u), Some(s)) => {
            let u = split_csv(&u)?;
            let s = split_csv(&s)?;
            if u.len() != s.len() {
                anyhow::bail!(
                    "moderation witness config: {} URL(s) but {} secret(s)",
                    u.len(),
                    s.len()
                );
            }
            Ok(u.into_iter().zip(s).collect())
        },
        _ => anyhow::bail!(
            "set both LAKKO_MODERATION_WITNESS_URLS and LAKKO_MODERATION_WITNESS_COSIGN_SECRETS, \
             or neither"
        ),
    }
}

fn split_csv(s: &str) -> anyhow::Result<Vec<String>> {
    let parts: Vec<String> = s.split(',').map(|p| p.trim().to_string()).collect();
    if parts.iter().any(String::is_empty) {
        anyhow::bail!("moderation witness config has an empty entry");
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::parse_endpoints;

    #[allow(clippy::unnecessary_wraps)]
    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn endpoints_parse_one_to_one_and_refuse_bad_lists() {
        assert!(parse_endpoints(None, None).unwrap().is_empty());
        assert!(parse_endpoints(s(" "), s("")).unwrap().is_empty());
        assert_eq!(
            parse_endpoints(s("http://a, http://b"), s("x,y")).unwrap(),
            vec![
                ("http://a".to_string(), "x".to_string()),
                ("http://b".to_string(), "y".to_string())
            ]
        );
        assert!(parse_endpoints(s("http://a"), None).is_err());
        assert!(parse_endpoints(None, s("x")).is_err());
        assert!(parse_endpoints(s("http://a,http://b"), s("x")).is_err());
        assert!(parse_endpoints(s("http://a,"), s("x,")).is_err());
    }
}
