// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Witness cosign client (ADR-0020 Sprint 4) — transparency -> witness.
//!
//! After the operator signs a new head root, it asks the witness to co-sign it
//! (`POST /witness/cosign`), handing over a Merkle consistency proof so the
//! witness can confirm the new root append-only-extends what it last co-signed.
//! The returned co-signature is cached + served on `GET /transparency/root`.
//! `None` (the integration unconfigured) is the transition fallback: roots are
//! served without a co-signature and clients keep the operator-only posture.

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
    /// Build from `LAKKO_TRANSPARENCY_WITNESS_URL` +
    /// `LAKKO_TRANSPARENCY_WITNESS_COSIGN_SECRET`. Returns `None` when either is
    /// unset/empty (the witness integration is OFF — the transition fallback).
    pub(crate) fn from_env() -> Option<Self> {
        let base_url = std::env::var("LAKKO_TRANSPARENCY_WITNESS_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())?;
        let cosign_secret_b64 = std::env::var("LAKKO_TRANSPARENCY_WITNESS_COSIGN_SECRET")
            .ok()
            .filter(|s| !s.trim().is_empty())?;
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
