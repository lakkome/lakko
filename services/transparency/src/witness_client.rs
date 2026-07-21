// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Witness cosign client (ADR-0020 Sprint 4; ADR-0039 S3 K-of-N) —
//! transparency -> witness(es).
//!
//! After the operator signs a new head root, it asks EACH configured witness to
//! co-sign it (`POST /witness/cosign`), handing over a Merkle consistency proof
//! so the witness can confirm the new root append-only-extends what it last
//! co-signed. The returned co-signatures are collected into a BUNDLE, cached,
//! and served on `GET /transparency/root` (ADR-0039 finding 1 — a K-of-N witness
//! quorum, so a single dishonest witness cannot help the operator equivocate).
//! An empty witness list (the integration unconfigured) is the transition
//! fallback: roots are served without a co-signature bundle and clients keep the
//! operator-only posture.

use anyhow::Context as _;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_transparency::{SignedRoot, TreeHash};
use serde::{Deserialize, Serialize};

/// A configured witness cosign client.
pub(crate) struct WitnessClient {
    base_url: String,
    /// The base64 secret presented as `Authorization: Bearer <secret-b64>`,
    /// shared with the witness's `LAKKO_WITNESS_COSIGN_SECRET`.
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
    new_root: SignedRootWire,
    consistency_proof_b64: Vec<String>,
}

#[derive(Deserialize)]
struct CosignRes {
    cosig_b64: String,
}

impl WitnessClient {
    /// Build the configured witness list (ADR-0039 S3 — K-of-N fan-out).
    ///
    /// Primary source (the LIST form): `LAKKO_TRANSPARENCY_WITNESS_URLS` +
    /// `LAKKO_TRANSPARENCY_WITNESS_COSIGN_SECRETS`, each a comma-separated list,
    /// POSITIONAL 1:1 (the i-th URL is bearer-authed with the i-th secret). A
    /// count mismatch — or an empty CSV entry (a stray comma / blank field) — is
    /// a loud, fail-closed configuration error.
    ///
    /// Backward-compat: when neither LIST var is set, fall back to the singular
    /// `LAKKO_TRANSPARENCY_WITNESS_URL` + `LAKKO_TRANSPARENCY_WITNESS_COSIGN_SECRET`
    /// pair as a one-element list (both required, else dormant — exactly the
    /// pre-S3 `from_env` behaviour). When nothing is configured, an EMPTY list is
    /// returned — the witness integration is dormant (the transition fallback).
    ///
    /// # Errors
    ///
    /// A URL/secret count mismatch, a partially-set LIST pair, or an empty CSV
    /// entry — all fail-closed configuration errors.
    pub(crate) fn list_from_env() -> anyhow::Result<Vec<Self>> {
        let endpoints = parse_witness_endpoints(
            std::env::var("LAKKO_TRANSPARENCY_WITNESS_URLS").ok(),
            std::env::var("LAKKO_TRANSPARENCY_WITNESS_COSIGN_SECRETS").ok(),
            std::env::var("LAKKO_TRANSPARENCY_WITNESS_URL").ok(),
            std::env::var("LAKKO_TRANSPARENCY_WITNESS_COSIGN_SECRET").ok(),
        )?;
        let mut clients = Vec::with_capacity(endpoints.len());
        for (base_url, cosign_secret_b64) in endpoints {
            clients.push(Self::build(&base_url, &cosign_secret_b64).ok_or_else(|| {
                anyhow::anyhow!("failed to build the HTTP client for a configured witness endpoint")
            })?);
        }
        Ok(clients)
    }

    /// Construct one client for an `(base_url, cosign_secret_b64)` pair.
    /// `None` only when the reqwest client fails to build.
    fn build(base_url: &str, cosign_secret_b64: &str) -> Option<Self> {
        let http = reqwest::Client::builder().build().ok()?;
        Some(Self {
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            cosign_secret_b64: cosign_secret_b64.trim().to_string(),
            http,
        })
    }

    /// `POST /witness/cosign` — submit the operator-signed `root` + a
    /// consistency proof, returning the witness co-signature wire blob.
    ///
    /// # Errors
    ///
    /// Network failure, a non-2xx witness status (e.g. 409 if the witness
    /// refuses a forked root), or a malformed response.
    pub(crate) async fn cosign(
        &self,
        root: &SignedRoot,
        proof: &[TreeHash],
    ) -> anyhow::Result<Vec<u8>> {
        let req = CosignReq {
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
        let res: CosignRes = resp
            .json()
            .await
            .context("decode witness cosign response")?;
        B64.decode(res.cosig_b64.as_bytes())
            .context("witness cosig is not valid base64")
    }
}

/// Pure env-parse half of [`WitnessClient::list_from_env`] — resolves the
/// configured witness endpoints as `(base_url, cosign_secret_b64)` pairs,
/// kept separate so it is unit-testable without mutating the process env.
///
/// LIST mode (either LIST var non-empty) requires BOTH lists, 1:1 positional,
/// with no empty entries. Otherwise falls back to the singular pair, or an empty
/// list (dormant) when nothing is configured.
///
/// # Errors
///
/// A partially-set LIST pair, a URL/secret count mismatch, or an empty CSV entry.
fn parse_witness_endpoints(
    urls_csv: Option<String>,
    secrets_csv: Option<String>,
    single_url: Option<String>,
    single_secret: Option<String>,
) -> anyhow::Result<Vec<(String, String)>> {
    let urls_csv = urls_csv.filter(|s| !s.trim().is_empty());
    let secrets_csv = secrets_csv.filter(|s| !s.trim().is_empty());

    if urls_csv.is_some() || secrets_csv.is_some() {
        // LIST mode — both required, 1:1 positional.
        let urls = urls_csv.ok_or_else(|| {
            anyhow::anyhow!(
                "LAKKO_TRANSPARENCY_WITNESS_COSIGN_SECRETS is set but \
                 LAKKO_TRANSPARENCY_WITNESS_URLS is not"
            )
        })?;
        let secrets = secrets_csv.ok_or_else(|| {
            anyhow::anyhow!(
                "LAKKO_TRANSPARENCY_WITNESS_URLS is set but \
                 LAKKO_TRANSPARENCY_WITNESS_COSIGN_SECRETS is not"
            )
        })?;
        let urls = split_csv(&urls)?;
        let secrets = split_csv(&secrets)?;
        if urls.len() != secrets.len() {
            anyhow::bail!(
                "witness config mismatch: {} URL(s) but {} cosign secret(s) — \
                 they must be 1:1 positional",
                urls.len(),
                secrets.len()
            );
        }
        Ok(urls.into_iter().zip(secrets).collect())
    } else {
        // Backward-compat singular pair (pre-S3 `from_env` shape): both required,
        // else dormant. Neither set → an empty list (integration off).
        match (
            single_url.filter(|s| !s.trim().is_empty()),
            single_secret.filter(|s| !s.trim().is_empty()),
        ) {
            (Some(u), Some(s)) => Ok(vec![(u.trim().to_string(), s.trim().to_string())]),
            _ => Ok(Vec::new()),
        }
    }
}

/// Split a comma-separated config value into trimmed entries, refusing an empty
/// entry (a stray comma or a blank field) so a mis-typed list fails closed
/// rather than silently dropping / mis-aligning a witness.
///
/// # Errors
///
/// Any entry that is empty after trimming.
fn split_csv(s: &str) -> anyhow::Result<Vec<String>> {
    let parts: Vec<String> = s.split(',').map(|p| p.trim().to_string()).collect();
    if parts.iter().any(String::is_empty) {
        anyhow::bail!("witness config list has an empty entry (a stray comma or a blank field)");
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::parse_witness_endpoints;

    /// Terse `Some(owned)` for the `Option<String>` parse args.
    #[allow(clippy::unnecessary_wraps)]
    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn list_mode_zips_urls_and_secrets_positionally() {
        let got = parse_witness_endpoints(
            s("http://w1:8088, http://w2:8088 ,http://w3:8088"),
            s("sec1,sec2,sec3"),
            None,
            None,
        )
        .expect("valid 3-of-N list");
        assert_eq!(
            got,
            vec![
                ("http://w1:8088".to_string(), "sec1".to_string()),
                ("http://w2:8088".to_string(), "sec2".to_string()),
                ("http://w3:8088".to_string(), "sec3".to_string()),
            ]
        );
    }

    #[test]
    fn list_mode_count_mismatch_is_a_loud_error() {
        let err = parse_witness_endpoints(s("http://w1,http://w2"), s("only-one"), None, None)
            .expect_err("2 urls vs 1 secret must error");
        assert!(err.to_string().contains("mismatch"));
    }

    #[test]
    fn list_mode_partial_pair_is_a_loud_error() {
        assert!(parse_witness_endpoints(s("http://w1,http://w2"), None, None, None).is_err());
        assert!(parse_witness_endpoints(None, s("s1,s2"), None, None).is_err());
    }

    #[test]
    fn list_mode_empty_entry_is_a_loud_error() {
        // A trailing / stray comma yields an empty entry — fail closed.
        assert!(
            parse_witness_endpoints(s("http://w1,,http://w2"), s("s1,,s2"), None, None).is_err()
        );
        assert!(parse_witness_endpoints(s("http://w1,"), s("s1,"), None, None).is_err());
    }

    #[test]
    fn falls_back_to_the_singular_pair_when_no_list_is_set() {
        let got = parse_witness_endpoints(None, None, s("http://witness:8088/"), s("secret"))
            .expect("singular pair");
        assert_eq!(
            got,
            vec![("http://witness:8088/".to_string(), "secret".to_string())]
        );
    }

    #[test]
    fn empty_list_vars_fall_through_to_the_singular_pair() {
        // The LIST vars present-but-blank must not shadow the singular pair.
        let got = parse_witness_endpoints(s("   "), s(""), s("http://witness:8088"), s("secret"))
            .expect("blank list vars fall through");
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn nothing_configured_yields_an_empty_dormant_list() {
        assert!(parse_witness_endpoints(None, None, None, None)
            .expect("no config")
            .is_empty());
        // A singular URL without its secret stays dormant (pre-S3 behaviour).
        assert!(
            parse_witness_endpoints(None, None, s("http://witness:8088"), None)
                .expect("partial singular")
                .is_empty()
        );
    }
}
