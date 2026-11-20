// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the another-device recovery payload v3 and the
//! own binding verdict (ADR-0018 Path B; ADR-0074 "Joining after a rotation",
//! G-56). Mirror of `crates/lakko-ffi-uniffi/src/recovery_payload.rs`, with
//! the same JS names.
//!
//! The seal stays in the client: it derives the key with
//! `deviceGroup.exportSecret(identity, recoveryExporterLabel(),
//! rendezvousPseudonym, recoveryExporterKeyLen())` and calls `sealWithKey` /
//! `openWithKey` on the plaintext these functions build and read.
//!
//! Predecessors cross as flat-concatenated 64-byte seeds plus one adoption
//! time each (a `BigUint64Array`), the shape of `buildBackupPayload`. A
//! refusal throws an `Error` whose message is `"<code>: <reason>"`. Every
//! byte returned from a payload is secret: never log it.

use lakko_contacts::own_binding::{binding_verdict, BindingVerdict, OwnLink};
use lakko_mnemonic::recovery_payload::{
    self as rp, RecoveryPayloadError, RecoveryPredecessor, RECOVERY_PREDECESSOR_SEED_LEN,
};
use wasm_bindgen::prelude::*;

const _: () = assert!(rp::RECOVERY_PREDECESSORS_MAX == lakko_backup::MAX_OWN_PREDECESSORS);
const _: () = assert!(RECOVERY_PREDECESSOR_SEED_LEN == lakko_backup::PREDECESSOR_SEED_LEN);
const _: () = assert!(rp::RECOVERY_EXPORTER_KEY_LEN == 32);
const _: () = assert!(rp::RECOVERY_PREDECESSORS_MAX == 8);

fn refusal(code: &str, reason: &str) -> String {
    format!("{code}: {reason}")
}

fn payload_err(e: RecoveryPayloadError) -> String {
    refusal(e.code(), &e.to_string())
}

fn key32(bytes: &[u8], what: &str) -> Result<[u8; 32], String> {
    bytes
        .try_into()
        .map_err(|_| refusal("bad-key-length", &format!("{what} must be 32 bytes")))
}

/// The MLS exporter label of the per-pairing recovery key,
/// `"lakko/device-recovery/v3"`.
#[wasm_bindgen(js_name = recoveryExporterLabel)]
#[must_use]
pub fn recovery_exporter_label() -> String {
    rp::RECOVERY_EXPORTER_LABEL.to_owned()
}

/// The exporter output length, 32.
#[wasm_bindgen(js_name = recoveryExporterKeyLen)]
#[must_use]
pub fn recovery_exporter_key_len() -> u32 {
    32
}

/// Most predecessors one payload carries, 8.
#[wasm_bindgen(js_name = recoveryPredecessorsMax)]
#[must_use]
pub fn recovery_predecessors_max() -> u32 {
    8
}

fn compose_inner(
    grant: &[u8],
    phrase_entropy: &[u8],
    predecessor_seeds: &[u8],
    predecessor_adopted_at_ms: &[u64],
) -> Result<Vec<u8>, String> {
    if predecessor_seeds.len() != predecessor_adopted_at_ms.len() * RECOVERY_PREDECESSOR_SEED_LEN {
        return Err(refusal(
            "bad-seed-length",
            "predecessors must be one 64-byte seed per adoption time",
        ));
    }
    let preds: Vec<RecoveryPredecessor> = predecessor_seeds
        .chunks_exact(RECOVERY_PREDECESSOR_SEED_LEN)
        .zip(predecessor_adopted_at_ms)
        .map(|(chunk, at)| {
            let mut seed = [0u8; RECOVERY_PREDECESSOR_SEED_LEN];
            seed.copy_from_slice(chunk);
            RecoveryPredecessor {
                seed,
                adopted_at_ms: *at,
            }
        })
        .collect();
    rp::compose_recovery_payload(grant, phrase_entropy, &preds)
        .map(|wire| wire.to_vec())
        .map_err(payload_err)
}

/// Build the v3 plaintext `grant ‖ entropy ‖ count ‖ count × (seed ‖
/// adopted_at_ms)`. `predecessorSeeds` is flat 64-byte seeds, oldest first;
/// `predecessorAdoptedAtMs` one time per seed.
///
/// Throws `bad-grant-length`, `bad-entropy-length`, `bad-seed-length`,
/// `too-many-predecessors`, `duplicate-predecessor`,
/// `current-key-predecessor`.
#[wasm_bindgen(js_name = composeRecoveryPayload)]
pub fn compose_recovery_payload(
    grant: &[u8],
    phrase_entropy: &[u8],
    predecessor_seeds: &[u8],
    predecessor_adopted_at_ms: Vec<u64>,
) -> Result<Vec<u8>, JsError> {
    compose_inner(
        grant,
        phrase_entropy,
        predecessor_seeds,
        &predecessor_adopted_at_ms,
    )
    .map_err(|e| JsError::new(&e))
}

/// A decoded recovery payload. Every byte getter is secret.
#[wasm_bindgen(js_name = RecoveryPayload)]
pub struct WasmRecoveryPayload {
    inner: rp::RecoveryPayload,
}

#[wasm_bindgen(js_class = RecoveryPayload)]
impl WasmRecoveryPayload {
    /// The 32-byte single-use device-authorisation grant.
    #[wasm_bindgen(getter)]
    pub fn grant(&self) -> Vec<u8> {
        self.inner.grant.to_vec()
    }

    /// The 16-byte entropy of the current 12-word phrase.
    #[wasm_bindgen(getter, js_name = phraseEntropy)]
    pub fn phrase_entropy(&self) -> Vec<u8> {
        self.inner.phrase_entropy.to_vec()
    }

    /// How many predecessors the payload carries (0..=8). May be logged.
    #[wasm_bindgen(getter, js_name = predecessorCount)]
    pub fn predecessor_count(&self) -> u32 {
        u32::try_from(self.inner.predecessors.len()).unwrap_or(u32::MAX)
    }

    /// The predecessor seeds, oldest first, flat-concatenated 64 bytes each.
    #[wasm_bindgen(getter, js_name = predecessorSeeds)]
    pub fn predecessor_seeds(&self) -> Vec<u8> {
        self.inner
            .predecessors
            .iter()
            .flat_map(|p| p.seed)
            .collect()
    }

    /// One adoption time per seed, unix milliseconds, in the same order.
    #[wasm_bindgen(getter, js_name = predecessorAdoptedAtMs)]
    pub fn predecessor_adopted_at_ms(&self) -> Vec<u64> {
        self.inner
            .predecessors
            .iter()
            .map(|p| p.adopted_at_ms)
            .collect()
    }
}

/// Parse a v3 plaintext opened from a recovery row. Throws `bad-length` (a
/// v2 payload included), `too-many-predecessors`, `duplicate-predecessor`,
/// `current-key-predecessor`: the keys were not handed over.
#[wasm_bindgen(js_name = decodeRecoveryPayload)]
pub fn decode_recovery_payload(plaintext: &[u8]) -> Result<WasmRecoveryPayload, JsError> {
    rp::decode_recovery_payload(plaintext)
        .map(|inner| WasmRecoveryPayload { inner })
        .map_err(|e| JsError::new(&payload_err(e)))
}

/// The mailbox row `"lakko/recovery-payload/v3" ‖ sealed`.
#[wasm_bindgen(js_name = wrapRecoveryRow)]
#[must_use]
pub fn wrap_recovery_row(sealed: &[u8]) -> Vec<u8> {
    rp::wrap_recovery_row(sealed)
}

/// The sealed blob of a v3 recovery row, or `undefined` for a row that is not
/// a recovery row (skip it). Throws `unsupported-row` for a recovery row of
/// another version (v2 included).
#[wasm_bindgen(js_name = unwrapRecoveryRow)]
pub fn unwrap_recovery_row(row: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
    rp::unwrap_recovery_row(row)
        .map(|sealed| sealed.map(<[u8]>::to_vec))
        .map_err(|e| JsError::new(&payload_err(e)))
}

/// The account's own conversation key from the `conversation_link` of its
/// directory bundle (`undefined` or empty when absent), verified against THIS
/// device's current account key, never the fetched one. `undefined` for an
/// absent or empty link, one that does not verify and one naming the current
/// key. Throws `bad-key-length` when `currentKey` is not 32 bytes.
#[wasm_bindgen(js_name = ownConversationKey)]
pub fn own_conversation_key(
    link: Option<Vec<u8>>,
    current_key: &[u8],
) -> Result<Option<Vec<u8>>, JsError> {
    let current = key32(current_key, "the current key").map_err(|e| JsError::new(&e))?;
    Ok(lakko_transparency::own_conversation_key(link.as_deref(), &current).map(|k| k.to_vec()))
}

/// What the account's own conversation link said.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnLinkKind {
    /// No link, or one that did not verify.
    Absent = 0,
    /// The link names a conversation key (pass it as `linkKey`).
    Key = 1,
    /// The bundle could not be read.
    Unknown = 2,
}

/// A binding verdict: `kind` is `"keep"`, `"keep-under-current-key"`,
/// `"wait"` or `"mint"`; `mintKey` is the 32-byte key for `"mint"`.
#[wasm_bindgen(js_name = BindingVerdict)]
pub struct WasmBindingVerdict {
    inner: BindingVerdict,
}

#[wasm_bindgen(js_class = BindingVerdict)]
impl WasmBindingVerdict {
    /// `"keep"`, `"keep-under-current-key"`, `"wait"` or `"mint"`.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        match self.inner {
            BindingVerdict::Keep => "keep",
            BindingVerdict::KeepUnderCurrentKey => "keep-under-current-key",
            BindingVerdict::Wait => "wait",
            BindingVerdict::Mint(_) => "mint",
        }
        .to_owned()
    }

    /// The key to mint under, for `"mint"`; `undefined` otherwise.
    #[wasm_bindgen(getter, js_name = mintKey)]
    pub fn mint_key(&self) -> Option<Vec<u8>> {
        match self.inner {
            BindingVerdict::Mint(k) => Some(k.to_vec()),
            _ => None,
        }
    }

    /// Whether the stored binding is kept (`"keep"` or
    /// `"keep-under-current-key"`).
    #[wasm_bindgen(getter, js_name = isKept)]
    pub fn is_kept(&self) -> bool {
        self.inner.is_kept()
    }
}

fn verdict_inner(
    bound_key: Option<&[u8]>,
    current_key: &[u8],
    predecessor_keys: &[u8],
    link_kind: OwnLinkKind,
    link_key: Option<&[u8]>,
    holds_conversations: bool,
) -> Result<BindingVerdict, String> {
    let bound = bound_key.map(|b| key32(b, "the bound key")).transpose()?;
    let current = key32(current_key, "the current key")?;
    if predecessor_keys.len() % 32 != 0 {
        return Err(refusal(
            "bad-key-length",
            "predecessor keys must be flat 32-byte keys",
        ));
    }
    let preds: Vec<[u8; 32]> = predecessor_keys
        .chunks_exact(32)
        .map(|c| key32(c, "a predecessor key"))
        .collect::<Result<_, _>>()?;
    let link = match link_kind {
        OwnLinkKind::Absent => OwnLink::Absent,
        OwnLinkKind::Unknown => OwnLink::Unknown,
        OwnLinkKind::Key => OwnLink::Key(key32(link_key.unwrap_or_default(), "the link key")?),
    };
    Ok(binding_verdict(
        bound.as_ref(),
        &current,
        &preds,
        &link,
        holds_conversations,
    ))
}

/// The binding verdict (ADR-0074 "Joining after a rotation"; the 17 shared
/// vectors live in `lakko_contacts::own_binding`). Run it with
/// `OwnLinkKind.Absent` first; read the link only when that answer is not
/// kept.
///
/// `boundKey`: the account key the stored binding names, `undefined` when
/// none or it did not verify. `predecessorKeys`: the own predecessor keys
/// this device holds, oldest first, flat 32 bytes each, current excluded.
/// `linkKey`: the conversation key, required when `linkKind` is `Key`.
/// Throws `bad-key-length`.
#[wasm_bindgen(js_name = ownBindingVerdict)]
pub fn own_binding_verdict(
    bound_key: Option<Vec<u8>>,
    current_key: &[u8],
    predecessor_keys: &[u8],
    link_kind: OwnLinkKind,
    link_key: Option<Vec<u8>>,
    holds_conversations: bool,
) -> Result<WasmBindingVerdict, JsError> {
    verdict_inner(
        bound_key.as_deref(),
        current_key,
        predecessor_keys,
        link_kind,
        link_key.as_deref(),
        holds_conversations,
    )
    .map(|inner| WasmBindingVerdict { inner })
    .map_err(|e| JsError::new(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_plaintext_over_the_wasm_shape() {
        let mut seeds = vec![3u8; 64];
        seeds.extend_from_slice(&[4u8; 64]);
        let wire = compose_inner(
            &[1; 32],
            &[2; 16],
            &seeds,
            &[1_700_000_000_000, 1_700_000_100_000],
        )
        .unwrap();
        assert_eq!(wire.len(), 193);
        let hex: String =
            lakko_crypto::hash::sha256(&wire)
                .iter()
                .fold(String::new(), |mut s, b| {
                    use core::fmt::Write as _;
                    let _ = write!(s, "{b:02x}");
                    s
                });
        assert_eq!(
            hex,
            "43260742d3c1e705179d7ac8baf1f0566b5fd9621eb1df08bb9a48e34dd56a41"
        );
        let back = WasmRecoveryPayload {
            inner: rp::decode_recovery_payload(&wire).unwrap(),
        };
        assert_eq!(back.predecessor_count(), 2);
        assert_eq!(back.predecessor_seeds(), seeds);
        assert_eq!(
            back.predecessor_adopted_at_ms(),
            vec![1_700_000_000_000, 1_700_000_100_000]
        );
    }

    #[test]
    fn refusals_carry_codes() {
        let e = compose_inner(&[1; 32], &[2; 16], &[3; 63], &[0]).unwrap_err();
        assert!(e.starts_with("bad-seed-length:"), "{e}");
        let e = payload_err(rp::decode_recovery_payload(&[0; 48]).unwrap_err());
        assert!(e.starts_with("bad-length:"), "{e}");
        assert_eq!(recovery_exporter_label(), "lakko/device-recovery/v3");
    }

    #[test]
    fn the_verdict_over_the_wasm_shape() {
        let cur = [0xC0u8; 32];
        let k0 = [0x10u8; 32];
        let v = verdict_inner(None, &cur, &[], OwnLinkKind::Key, Some(&k0), false).unwrap();
        assert_eq!(v, BindingVerdict::Wait);
        let v = verdict_inner(Some(&cur), &cur, &k0, OwnLinkKind::Key, Some(&k0), false).unwrap();
        assert_eq!(v, BindingVerdict::Mint(k0));
        let w = WasmBindingVerdict { inner: v };
        assert_eq!(w.kind(), "mint");
        assert_eq!(w.mint_key(), Some(k0.to_vec()));
        assert!(verdict_inner(None, &cur, &[], OwnLinkKind::Key, None, false).is_err());
        assert!(verdict_inner(None, &cur, &[0; 31], OwnLinkKind::Absent, None, false).is_err());
    }
}
