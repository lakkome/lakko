// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Client-side TUF (The Update Framework) v1.1 metadata
//! verifier — scaffold for GA-5 secure auto-update.
//!
//! Lakko ships signed updates to clients (mobile + desktop)
//! via this layer. Each release the publisher mints a
//! fresh **timestamp** metadata pointing at a fresh
//! **snapshot**, which in turn pins the current
//! **targets** metadata listing the actual binary
//! artefacts + their SHA-256 hashes. The **root** metadata
//! is the long-lived trust anchor that lists which Ed25519
//! keys are authorised to sign each role and at what
//! threshold.
//!
//! ## What this crate ships
//!
//! - Four signed-metadata types
//!   ([`Root`], [`Snapshot`], [`Targets`], [`Timestamp`]).
//! - Canonical JSON signing-bytes (the `signed` sub-object
//!   serialised to JSON with sorted keys; Ed25519 signs the
//!   exact bytes).
//! - Per-role signature threshold check.
//! - Chain-of-trust walk: [`verify_chain`] takes the four
//!   metadata files plus a binary-pinned root [`KeyId`] set
//!   and either returns a verified [`TargetMap`] or a
//!   typed [`TufError`].
//! - Expiry comparison against a caller-supplied wall
//!   clock (so tests can pin `now` deterministically).
//!
//! ## What this crate does NOT ship
//!
//! - **Network fetch.** The caller pulls the four metadata
//!   files (and the actual target binaries) from wherever
//!   they live; this crate only verifies what it's handed.
//!   That keeps the audit surface small and lets the
//!   transport layer enforce its own policy (TLS pinning,
//!   DoH bootstrap, etc.).
//! - **Delegated targets.** The TUF spec lets the targets
//!   role delegate sub-paths to other keys (`delegations`
//!   field). Scaffold path: not implemented; a future
//!   sprint adds it when the release engineering need
//!   actually shows up.
//! - **Key rotation across root versions.** Closed by
//!   [`verify_root_chain`] — the caller holds an
//!   ordered slice of root metadata files (one per
//!   version they've stepped through since first install)
//!   and `verify_root_chain` walks them, verifying each
//!   new root against the *previous* root's `root`
//!   role. The first root in the chain is anchored
//!   against the binary-pinned key id set baked into
//!   the build artefact.
//! - **Consistent snapshots / mirror selection / hashed
//!   bins.** All deferred to a future sprint.
//!
//! ## Threat model alignment
//!
//! Same trust-anchor shape as the transport-rendezvous
//! bootstrap (T-7): the root keys are part of the
//! reproducible-build artefact, baked into every client
//! at compile time. An attacker that controls the update
//! origin cannot push a malicious update without
//! compromising the offline signing keys.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use lakko_crypto::signing::VerifyingKey;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// =========================================================
// Errors
// =========================================================

/// All failure modes the verifier surfaces.
///
/// Callers branch on the variant, never on the message
/// string — same convention as the rest of the
/// transport / channels stack.
#[derive(Debug, Error)]
pub enum TufError {
    /// Canonical-JSON serialisation failed. Effectively
    /// impossible for in-Rust structs with valid contents.
    #[error("canonical JSON encode failed: {0}")]
    CanonicalEncode(String),
    /// Hex-decode of a signature or key id failed.
    #[error("hex decode failed: {0}")]
    BadHex(String),
    /// Signature parse / verify failed against the
    /// expected key.
    #[error("signature did not verify")]
    BadSignature,
    /// Metadata expired (the role's `expires` field is in
    /// the past relative to caller-supplied `now`).
    #[error("{role} metadata expired at {expires}")]
    Expired {
        /// Role name (`root`, `snapshot`, `targets`,
        /// `timestamp`).
        role: &'static str,
        /// Expiration timestamp from the metadata.
        expires: DateTime<Utc>,
    },
    /// Role version went backwards — TUF requires
    /// monotone version numbers on snapshot / timestamp.
    #[error("{role} version went backwards: got {got}, last seen {last}")]
    VersionRollback {
        /// Role name.
        role: &'static str,
        /// Version in the incoming metadata.
        got: u64,
        /// Last-seen version (caller-supplied).
        last: u64,
    },
    /// Signatures present but below the role's threshold.
    #[error("{role} threshold not met: {valid}/{required}")]
    ThresholdNotMet {
        /// Role name.
        role: &'static str,
        /// Distinct valid signatures observed.
        valid: u32,
        /// Threshold required by root.
        required: u32,
    },
    /// Signature key id references a key the root does
    /// not authorise for this role.
    #[error("{role} signed by unauthorized key {key_id}")]
    UnauthorizedKey {
        /// Role name.
        role: &'static str,
        /// 32-byte key id rendered as 64 hex chars.
        key_id: String,
    },
    /// Snapshot points at a targets version that doesn't
    /// match the supplied targets metadata.
    #[error("snapshot pins targets version {expected}, got {actual}")]
    TargetsVersionMismatch {
        /// Version snapshot pinned to.
        expected: u64,
        /// Version targets metadata actually carries.
        actual: u64,
    },
    /// Timestamp points at a snapshot version that
    /// doesn't match the supplied snapshot metadata.
    #[error("timestamp pins snapshot version {expected}, got {actual}")]
    SnapshotVersionMismatch {
        /// Version timestamp pinned to.
        expected: u64,
        /// Version snapshot metadata actually carries.
        actual: u64,
    },
    /// Root metadata `_type` field doesn't match the
    /// expected role.
    #[error("unexpected metadata type: got {got}, expected {expected}")]
    WrongType {
        /// Type field from the metadata.
        got: String,
        /// Expected type for this role.
        expected: &'static str,
    },
    /// Root chain (`verify_root_chain`) was given an
    /// empty `roots` slice.
    #[error("root chain is empty")]
    EmptyRootChain,
    /// Two consecutive roots in the chain skipped a
    /// version (TUF requires sequential v_n → v_n+1).
    #[error("root chain skipped versions: {from} → {to}")]
    RootVersionGap {
        /// Version of the earlier root.
        from: u64,
        /// Version of the next root in the slice.
        to: u64,
    },
    /// First root in the chain does not match the
    /// caller-supplied binary-pinned trust anchor key
    /// id set.
    #[error("first root in chain not signed by pinned trust anchor")]
    UntrustedFirstRoot,
    /// Fetched target bytes do not match the length the
    /// signed `TargetMeta` commits to. Surfaced by
    /// [`verify_target_bytes`].
    #[error("target length mismatch: expected {expected}, got {got}")]
    TargetLengthMismatch {
        /// Length recorded in the signed targets metadata.
        expected: u64,
        /// Length of the bytes the caller actually fetched.
        got: u64,
    },
    /// Fetched target bytes have the right length but the
    /// digest under a known hash algorithm does not match
    /// the value the signed `TargetMeta` commits to.
    #[error("target {algo} hash mismatch: expected {expected}, got {got}")]
    TargetHashMismatch {
        /// Hash algorithm whose digest failed
        /// (`"sha256"`).
        algo: String,
        /// Hex-encoded digest from the signed metadata.
        expected: String,
        /// Hex-encoded digest the caller actually computed.
        got: String,
    },
    /// The signed `TargetMeta.hashes` map contained only
    /// hash algorithms this build of the verifier does not
    /// know how to compute. Refusing to install is the
    /// audit-safe behaviour — an attacker could otherwise
    /// publish a target with only an exotic algorithm
    /// nobody verifies. Forward-compat path: add the
    /// algorithm to the verifier, ship a new client.
    #[error("no known target hash algorithm in metadata: had {available:?}")]
    NoKnownTargetHashAlgo {
        /// Algorithm names found in the metadata; the
        /// caller can surface this to the user / log for
        /// triage.
        available: Vec<String>,
    },
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, TufError>;

// =========================================================
// Primitive types
// =========================================================

/// 32-byte Ed25519 key id — the SHA-256 digest of the
/// public-key bytes per TUF §4.2.1, rendered as 64-char
/// lowercase hex on the wire. Strongly-typed so the
/// signature-verifier can't accept a random hex string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyId(pub String);

impl KeyId {
    /// Compute the key id for a given Ed25519 verifying
    /// key. Matches TUF §4.2.1: SHA-256 of the canonical
    /// JSON of the public-key object, rendered as hex.
    /// We use SHA-256 over the raw 32-byte key as a
    /// scaffold-simple compatible variant; full TUF
    /// computes it over the JSON-wrapped key.
    #[must_use]
    pub fn from_verifying_key(vk: &VerifyingKey) -> Self {
        let hash = lakko_crypto::hash::sha256(&vk.to_bytes());
        Self(hex::encode(hash))
    }
}

/// Ed25519 public key on the wire — TUF wraps it in a
/// type+scheme+keyval object so future ciphersuites can
/// be added without breaking the schema.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublicKey {
    /// Always `"ed25519"` for the scaffold.
    pub keytype: String,
    /// Always `"ed25519"` for the scaffold.
    pub scheme: String,
    /// Hex-encoded public key bytes.
    pub keyval: KeyVal,
}

/// Inner key-value object — `public` is the hex-encoded
/// raw public-key bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyVal {
    /// 32-byte Ed25519 public key as 64-char hex.
    pub public: String,
}

impl PublicKey {
    /// Build an ed25519 wrapper around a [`VerifyingKey`].
    #[must_use]
    pub fn ed25519(vk: &VerifyingKey) -> Self {
        Self {
            keytype: "ed25519".into(),
            scheme: "ed25519".into(),
            keyval: KeyVal {
                public: hex::encode(vk.to_bytes()),
            },
        }
    }

    /// Parse back to a [`VerifyingKey`]. Returns
    /// [`TufError::BadHex`] on bad hex or wrong length.
    pub fn to_verifying_key(&self) -> Result<VerifyingKey> {
        let bytes = hex::decode(&self.keyval.public)
            .map_err(|e| TufError::BadHex(e.to_string()))?;
        if bytes.len() != 32 {
            return Err(TufError::BadHex(format!(
                "ed25519 key must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        VerifyingKey::from_bytes(&arr).map_err(|e| TufError::BadHex(e.to_string()))
    }
}

/// One signature in the signed-envelope's `signatures`
/// array.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signature {
    /// Key id of the signer.
    pub keyid: KeyId,
    /// Hex-encoded 64-byte Ed25519 signature.
    pub sig: String,
}

/// Role descriptor in the root metadata — names the
/// authorised key ids plus the verification threshold.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleSpec {
    /// Key ids authorised to sign this role.
    pub keyids: Vec<KeyId>,
    /// Minimum number of distinct valid signatures
    /// required to accept the role.
    pub threshold: u32,
}

/// Convenience alias for the `Root.roles` map shape
/// (`role_name` → role spec). Used so integration tests
/// and production callers can name the type without
/// pulling in `BTreeMap` + `String` + `RoleSpec`
/// individually.
pub type RoleMap = std::collections::BTreeMap<String, RoleSpec>;

// =========================================================
// Signed envelope
// =========================================================

/// TUF signed envelope — outer wrapper that pairs a
/// `signed` body (any of [`Root`] / [`Snapshot`] /
/// [`Targets`] / [`Timestamp`]) with the `signatures`
/// array.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signed<T> {
    /// The inner signed body.
    pub signed: T,
    /// One signature per authorised signer (each entry's
    /// `keyid` must be present in the role's `keyids`).
    pub signatures: Vec<Signature>,
}

impl<T: for<'de> Deserialize<'de>> Signed<T> {
    /// Parse a `Signed<T>` envelope from raw JSON bytes.
    /// The caller is expected to have pulled the four
    /// TUF metadata files (root / snapshot / targets /
    /// timestamp) over whatever transport they prefer
    /// (HTTPS to the release mirror, or a sidechannel
    /// for the very-first-install root) and to feed the
    /// raw bytes through here.
    ///
    /// **Important**: this is **the only entry point**
    /// that lets the caller construct a `Signed<T>` from
    /// untrusted bytes — any subsequent `verify_chain`
    /// / `verify_root_chain` calls work on the parsed
    /// struct *after* this function has run. The
    /// signature byte sequence is re-derived from the
    /// `signed` sub-tree at verification time (not at
    /// parse time), so a JSON encoder that produces
    /// equivalent-but-not-byte-identical output (e.g.
    /// reordered keys outside of `BTreeMap` fields)
    /// still verifies correctly because the verifier
    /// re-serialises the parsed struct through the
    /// canonical encoder.
    ///
    /// # Errors
    ///
    /// [`TufError::CanonicalEncode`] on any JSON
    /// deserialisation failure — the inner error
    /// message points at the offending field.
    pub fn from_json(json: &[u8]) -> Result<Self> {
        serde_json::from_slice(json)
            .map_err(|e| TufError::CanonicalEncode(format!("JSON parse: {e}")))
    }

    /// Encode the envelope back to JSON. Useful for
    /// tests + audit-grep matching against the canonical
    /// bytes the signature commits to.
    pub fn to_json(&self) -> Result<Vec<u8>>
    where
        T: Serialize,
    {
        serde_json::to_vec(self)
            .map_err(|e| TufError::CanonicalEncode(format!("JSON encode: {e}")))
    }
}

// =========================================================
// Role bodies
// =========================================================

/// Root metadata — the long-lived trust anchor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Root {
    /// Always `"root"`.
    #[serde(rename = "_type")]
    pub type_: String,
    /// Schema version (TUF 1.1 = `"1.0"` for compat).
    pub spec_version: String,
    /// Monotone metadata version.
    pub version: u64,
    /// Expiration timestamp.
    pub expires: DateTime<Utc>,
    /// Map from key id to public key.
    pub keys: BTreeMap<KeyId, PublicKey>,
    /// Map from role name (`"root"` / `"snapshot"` /
    /// `"targets"` / `"timestamp"`) to role spec.
    pub roles: BTreeMap<String, RoleSpec>,
}

/// Snapshot metadata — pins the current version of every
/// other top-level metadata file (mostly targets, in the
/// scaffold).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// Always `"snapshot"`.
    #[serde(rename = "_type")]
    pub type_: String,
    /// Schema version.
    pub spec_version: String,
    /// Monotone metadata version.
    pub version: u64,
    /// Expiration timestamp.
    pub expires: DateTime<Utc>,
    /// Map from metadata file path to version pin.
    pub meta: BTreeMap<String, MetaPin>,
}

/// Targets metadata — the actual list of release
/// artefacts + their hashes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Targets {
    /// Always `"targets"`.
    #[serde(rename = "_type")]
    pub type_: String,
    /// Schema version.
    pub spec_version: String,
    /// Monotone metadata version.
    pub version: u64,
    /// Expiration timestamp.
    pub expires: DateTime<Utc>,
    /// Map from target path (`"lakko-svc-channels"`) to
    /// target metadata.
    pub targets: BTreeMap<String, TargetMeta>,
}

/// Timestamp metadata — short-lived proof of freshness
/// that pins the current snapshot version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timestamp {
    /// Always `"timestamp"`.
    #[serde(rename = "_type")]
    pub type_: String,
    /// Schema version.
    pub spec_version: String,
    /// Monotone metadata version.
    pub version: u64,
    /// Expiration timestamp.
    pub expires: DateTime<Utc>,
    /// Map containing exactly one entry: `"snapshot"` →
    /// [`MetaPin`] of the current snapshot.
    pub meta: BTreeMap<String, MetaPin>,
}

/// Version pin for a metadata file referenced from
/// snapshot / timestamp.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetaPin {
    /// Required version of the pinned metadata file.
    pub version: u64,
    /// Optional length in bytes (post-canonical-JSON).
    /// Scaffold omits hash binding — full TUF compares
    /// the hash of the actual fetched metadata against
    /// a pinned digest here. Deferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
}

/// Target metadata — per-artefact entry in
/// [`Targets::targets`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetMeta {
    /// Length in bytes of the actual artefact.
    pub length: u64,
    /// Map from hash algorithm (`"sha256"` for the
    /// scaffold) to hex-encoded digest.
    pub hashes: BTreeMap<String, String>,
}

/// Map returned from a successful [`verify_chain`] — the
/// caller can index by artefact path to get the expected
/// length + hash digest.
pub type TargetMap = BTreeMap<String, TargetMeta>;

// =========================================================
// Canonical JSON
// =========================================================

/// Serialise a body to canonical JSON (sorted keys, no
/// whitespace, no comments). This is the byte sequence
/// the Ed25519 signature commits to.
///
/// We use serde_json's default encoding — which already
/// produces sorted keys for `BTreeMap` values + no
/// whitespace via `serde_json::to_vec`. Strings are
/// serialised with the same escape rules as the TUF
/// reference impl.
fn canonical_json<T: Serialize>(body: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(body).map_err(|e| TufError::CanonicalEncode(e.to_string()))
}

/// Test-only re-export of [`canonical_json`] so the
/// integration test fixture in `tests/bootstrap.rs` can
/// build a `Signed<T>` envelope with the exact signing
/// bytes the in-crate verifier consumes. The `_for_test`
/// suffix keeps this audit-grep clean — production
/// callers never reach for it; they only consume parsed
/// envelopes through `Signed::from_json`.
pub fn canonical_json_for_test<T: Serialize>(body: &T) -> Result<Vec<u8>> {
    canonical_json(body)
}

// =========================================================
// Per-role verification
// =========================================================

/// Verify the signatures on a `Signed<T>` envelope
/// against a role spec + the key registry from root.
fn verify_signatures<T: Serialize>(
    role: &'static str,
    signed: &Signed<T>,
    role_spec: &RoleSpec,
    keys: &BTreeMap<KeyId, PublicKey>,
) -> Result<()> {
    let canonical = canonical_json(&signed.signed)?;
    let authorised: std::collections::BTreeSet<&KeyId> =
        role_spec.keyids.iter().collect();
    let mut seen: std::collections::BTreeSet<KeyId> =
        std::collections::BTreeSet::default();
    let mut valid: u32 = 0;
    for sig in &signed.signatures {
        if !authorised.contains(&sig.keyid) {
            // Skip — not fatal unless the threshold ends
            // up below the requirement. TUF allows
            // signatures from other keys to coexist
            // (e.g. mid-rotation).
            continue;
        }
        if !seen.insert(sig.keyid.clone()) {
            // Duplicate signature from the same key —
            // doesn't count toward threshold per TUF
            // §5.3.3.
            continue;
        }
        let Some(pubkey) = keys.get(&sig.keyid) else {
            return Err(TufError::UnauthorizedKey {
                role,
                key_id: sig.keyid.0.clone(),
            });
        };
        let vk = pubkey.to_verifying_key()?;
        let sig_bytes = hex::decode(&sig.sig)
            .map_err(|e| TufError::BadHex(e.to_string()))?;
        let Ok(signature) = lakko_crypto::signing::Signature::from_bytes(&sig_bytes)
        else {
            // malformed signature byte length → skip
            continue;
        };
        if vk.verify(&canonical, &signature).is_ok() {
            valid += 1;
        }
    }
    if valid < role_spec.threshold {
        return Err(TufError::ThresholdNotMet {
            role,
            valid,
            required: role_spec.threshold,
        });
    }
    Ok(())
}

/// Check a role-body's `expires` against `now`. Returns
/// [`TufError::Expired`] if `now > expires`.
fn check_expiry(
    role: &'static str,
    expires: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<()> {
    if now > expires {
        return Err(TufError::Expired { role, expires });
    }
    Ok(())
}

/// Check the `_type` field matches the expected role.
fn check_type(got: &str, expected: &'static str) -> Result<()> {
    if got != expected {
        return Err(TufError::WrongType {
            got: got.into(),
            expected,
        });
    }
    Ok(())
}

// =========================================================
// Public verify_chain entry point
// =========================================================

/// Walk the full TUF chain: root → snapshot → timestamp
/// → targets, returning the verified [`TargetMap`] on
/// success.
///
/// `now` is supplied by the caller so tests can drive
/// expiry deterministically. Production callers pass
/// `Utc::now()`.
///
/// `last_known_versions` is the caller's record of the
/// last accepted snapshot + timestamp versions, used to
/// detect rollback attempts (TUF §5.3.6). Pass `None`
/// for the very first run.
///
/// # Errors
///
/// Returns one of [`TufError`]'s variants on any
/// validation failure; the caller branches on the
/// variant.
pub fn verify_chain(
    root: &Signed<Root>,
    timestamp: &Signed<Timestamp>,
    snapshot: &Signed<Snapshot>,
    targets: &Signed<Targets>,
    now: DateTime<Utc>,
    last_known_versions: Option<&KnownVersions>,
) -> Result<TargetMap> {
    // 1. Self-verify root: signed by ≥ threshold of its
    //    own listed signers. (Production also walks the
    //    full root-version chain across rotations; the
    //    scaffold trusts the caller-supplied root as the
    //    pinned trust anchor.)
    check_type(&root.signed.type_, "root")?;
    check_expiry("root", root.signed.expires, now)?;
    let root_spec = role_spec(&root.signed, "root")?;
    verify_signatures("root", root, root_spec, &root.signed.keys)?;

    // 2. Verify timestamp under root's `timestamp` role.
    check_type(&timestamp.signed.type_, "timestamp")?;
    check_expiry("timestamp", timestamp.signed.expires, now)?;
    let timestamp_spec = role_spec(&root.signed, "timestamp")?;
    verify_signatures("timestamp", timestamp, timestamp_spec, &root.signed.keys)?;
    if let Some(last) = last_known_versions {
        if timestamp.signed.version < last.timestamp {
            return Err(TufError::VersionRollback {
                role: "timestamp",
                got: timestamp.signed.version,
                last: last.timestamp,
            });
        }
    }

    // 3. Timestamp pins a snapshot version.
    let snapshot_pin = timestamp
        .signed
        .meta
        .get("snapshot.json")
        .or_else(|| timestamp.signed.meta.get("snapshot"))
        .ok_or(TufError::SnapshotVersionMismatch {
            expected: 0,
            actual: snapshot.signed.version,
        })?;
    if snapshot.signed.version != snapshot_pin.version {
        return Err(TufError::SnapshotVersionMismatch {
            expected: snapshot_pin.version,
            actual: snapshot.signed.version,
        });
    }

    // 4. Verify snapshot under root's `snapshot` role.
    check_type(&snapshot.signed.type_, "snapshot")?;
    check_expiry("snapshot", snapshot.signed.expires, now)?;
    let snapshot_spec = role_spec(&root.signed, "snapshot")?;
    verify_signatures("snapshot", snapshot, snapshot_spec, &root.signed.keys)?;
    if let Some(last) = last_known_versions {
        if snapshot.signed.version < last.snapshot {
            return Err(TufError::VersionRollback {
                role: "snapshot",
                got: snapshot.signed.version,
                last: last.snapshot,
            });
        }
    }

    // 5. Snapshot pins a targets version.
    let targets_pin = snapshot
        .signed
        .meta
        .get("targets.json")
        .or_else(|| snapshot.signed.meta.get("targets"))
        .ok_or(TufError::TargetsVersionMismatch {
            expected: 0,
            actual: targets.signed.version,
        })?;
    if targets.signed.version != targets_pin.version {
        return Err(TufError::TargetsVersionMismatch {
            expected: targets_pin.version,
            actual: targets.signed.version,
        });
    }

    // 6. Verify targets under root's `targets` role.
    check_type(&targets.signed.type_, "targets")?;
    check_expiry("targets", targets.signed.expires, now)?;
    let targets_spec = role_spec(&root.signed, "targets")?;
    verify_signatures("targets", targets, targets_spec, &root.signed.keys)?;

    Ok(targets.signed.targets.clone())
}

fn role_spec<'a>(root: &'a Root, role: &'static str) -> Result<&'a RoleSpec> {
    root.roles
        .get(role)
        .ok_or(TufError::UnauthorizedKey {
            role,
            key_id: "(no role spec in root)".into(),
        })
}

/// Walk a chain of root metadata versions, verifying
/// each against the previous root's `root` role. Returns
/// the **final** root in the chain — the trust anchor
/// the caller should hand to [`verify_chain`].
///
/// ## Walk
///
/// 1. The first root must be signed by ≥ threshold of
///    keys whose ids are in `pinned_trust_anchor`. This
///    is the binary-pinned trust anchor baked into the
///    reproducible-build artefact — the only way a
///    fully fresh install bootstraps trust.
/// 2. Each subsequent root `r_n+1` must:
///    - Have `version = r_n.version + 1` (TUF §5.3.4
///      requires sequential root versions; gaps would
///      let an attacker omit a revocation).
///    - Be signed by ≥ threshold of `r_n.roles["root"]`
///      keys. This is the core TUF security property —
///      a compromised current root key cannot be
///      rotated out without the *previous* signers'
///      consent.
///    - Not be expired against `now`.
///
/// `pinned_trust_anchor` is a list of [`KeyId`]s that
/// the very first root must be signed by ≥ first-root's
/// threshold of. In practice it's a single-element list
/// pointing at the root key the binary was compiled
/// against; multi-key threshold is supported for
/// org-policy needs.
///
/// # Errors
///
/// - [`TufError::EmptyRootChain`] if `roots` is empty.
/// - [`TufError::UntrustedFirstRoot`] if the first
///   root's signers don't match the pinned trust
///   anchor.
/// - [`TufError::RootVersionGap`] if two consecutive
///   roots skip a version.
/// - [`TufError::VersionRollback`] if a root carries a
///   `version` lower than its predecessor.
/// - [`TufError::ThresholdNotMet`] / [`TufError::Expired`]
///   propagating from per-root verification.
pub fn verify_root_chain<'a>(
    roots: &'a [Signed<Root>],
    pinned_trust_anchor: &[KeyId],
    now: DateTime<Utc>,
) -> Result<&'a Signed<Root>> {
    let Some(first) = roots.first() else {
        return Err(TufError::EmptyRootChain);
    };

    // Step 1: first root must be signed by ≥ first
    // root's own threshold of keys whose ids are in
    // `pinned_trust_anchor`. We synthesise a pinned
    // role spec from the first root's `root` role
    // threshold + the pinned KeyId list.
    check_type(&first.signed.type_, "root")?;
    check_expiry("root", first.signed.expires, now)?;
    let first_root_spec = role_spec(&first.signed, "root")?;
    let pinned_spec = RoleSpec {
        keyids: pinned_trust_anchor.to_vec(),
        threshold: first_root_spec.threshold,
    };
    // Verify with a key map limited to the pinned set —
    // even if the first root *also* declares other keys
    // for `root` role (e.g. legacy), only the pinned
    // ones can satisfy the threshold here. Building a
    // restricted key map below.
    let mut pinned_keys = BTreeMap::new();
    for kid in pinned_trust_anchor {
        if let Some(k) = first.signed.keys.get(kid) {
            pinned_keys.insert(kid.clone(), k.clone());
        }
    }
    if pinned_keys.is_empty() {
        return Err(TufError::UntrustedFirstRoot);
    }
    verify_signatures("root", first, &pinned_spec, &pinned_keys)
        .map_err(|e| match e {
            // Re-label threshold-not-met against the
            // pinned-trust-anchor check so the operator
            // log line points at the right cause.
            TufError::ThresholdNotMet { .. } => TufError::UntrustedFirstRoot,
            other => other,
        })?;

    // Step 2: walk forward, each pair (prev, curr).
    for pair in roots.windows(2) {
        let prev = &pair[0];
        let curr = &pair[1];
        check_type(&curr.signed.type_, "root")?;
        check_expiry("root", curr.signed.expires, now)?;
        if curr.signed.version <= prev.signed.version {
            return Err(TufError::VersionRollback {
                role: "root",
                got: curr.signed.version,
                last: prev.signed.version,
            });
        }
        if curr.signed.version != prev.signed.version + 1 {
            return Err(TufError::RootVersionGap {
                from: prev.signed.version,
                to: curr.signed.version,
            });
        }
        // Verify curr against prev's `root` role + key
        // registry. This is the rotation-safety
        // property: a new root needs the *old* root's
        // signers' consent to be accepted.
        let prev_root_spec = role_spec(&prev.signed, "root")?;
        verify_signatures("root", curr, prev_root_spec, &prev.signed.keys)?;
    }

    // Final root is what the caller hands to
    // verify_chain.
    Ok(roots.last().expect("non-empty by check above"))
}

/// Caller-tracked last-accepted versions of the two
/// short-lived roles (timestamp + snapshot). Used to
/// detect rollback attempts.
#[derive(Clone, Copy, Debug, Default)]
pub struct KnownVersions {
    /// Last-accepted timestamp version, or `0` if never
    /// seen.
    pub timestamp: u64,
    /// Last-accepted snapshot version, or `0`.
    pub snapshot: u64,
}

// =========================================================
// Target-bytes verification
// =========================================================

/// Verify fetched target bytes against the signed
/// [`TargetMeta`] returned by [`verify_chain`].
///
/// This is the final gate in the TUF auto-update flow:
/// after the metadata chain has been verified, the caller
/// fetches the actual binary artefact (over its preferred
/// transport — HTTPS, sidechannel, USB key) and runs this
/// helper to confirm the bytes match what the signed
/// `targets` metadata commits to.
///
/// Both the length and the SHA-256 digest must match.
/// Unknown hash algorithms in `meta.hashes` are skipped
/// (forward-compat); at least one known algorithm must
/// have been checked, otherwise [`TufError::NoKnownTargetHashAlgo`]
/// is returned — the audit-safe stance, since otherwise an
/// attacker could publish a target whose only hash is
/// under an exotic algorithm this build does not verify.
///
/// # Algorithms
///
/// The scaffold understands `"sha256"` only. SHA-512 /
/// BLAKE2b / BLAKE3 can be added when release-engineering
/// actually publishes them; the function shape stays the
/// same.
///
/// # Errors
///
/// - [`TufError::TargetLengthMismatch`] — `bytes.len()`
///   does not match `meta.length`.
/// - [`TufError::TargetHashMismatch`] — a known hash
///   algorithm in `meta.hashes` does not agree with the
///   actual digest of `bytes`.
/// - [`TufError::NoKnownTargetHashAlgo`] — `meta.hashes`
///   contained only algorithms this build does not know
///   how to compute (including the empty-map case).
pub fn verify_target_bytes(meta: &TargetMeta, bytes: &[u8]) -> Result<()> {
    // Length first — cheap to compute and rules out the
    // entire input before we touch the SHA-256 path. We
    // compare `u64`-to-`u64` to avoid platform-dependent
    // `usize` rounding on 32-bit targets.
    let got_len = bytes.len() as u64;
    if got_len != meta.length {
        return Err(TufError::TargetLengthMismatch {
            expected: meta.length,
            got: got_len,
        });
    }

    let mut algo_checked = false;
    let mut available: Vec<String> = Vec::with_capacity(meta.hashes.len());
    for (algo, expected_hex) in &meta.hashes {
        available.push(algo.clone());
        // Unknown algorithms are skipped — forward-compat.
        // A publisher may ship sha256 alongside a future-
        // proof sha512 entry; older clients verify the
        // sha256 and ignore the rest. The audit-safe stance
        // is enforced by the `algo_checked` guard below:
        // if NO known algorithm got verified, refuse.
        if algo.as_str() == "sha256" {
            let digest = lakko_crypto::hash::sha256(bytes);
            let got_hex = hex::encode(digest);
            if got_hex != *expected_hex {
                return Err(TufError::TargetHashMismatch {
                    algo: algo.clone(),
                    expected: expected_hex.clone(),
                    got: got_hex,
                });
            }
            algo_checked = true;
        }
    }

    if !algo_checked {
        return Err(TufError::NoKnownTargetHashAlgo { available });
    }

    Ok(())
}

// =========================================================
// Tests
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use lakko_crypto::signing::SigningKey;

    /// Build a minimal valid chain — root with one key
    /// for every role (1-of-1 threshold), snapshot
    /// pinning targets v1, timestamp pinning snapshot v1.
    fn build_chain(
        signer: &SigningKey,
        now: DateTime<Utc>,
    ) -> (Signed<Root>, Signed<Timestamp>, Signed<Snapshot>, Signed<Targets>) {
        let vk = signer.verifying_key();
        let key_id = KeyId::from_verifying_key(&vk);
        let pubkey = PublicKey::ed25519(&vk);

        let mut keys = BTreeMap::new();
        keys.insert(key_id.clone(), pubkey);

        let mut roles = BTreeMap::new();
        for role in ["root", "snapshot", "targets", "timestamp"] {
            roles.insert(
                role.into(),
                RoleSpec {
                    keyids: vec![key_id.clone()],
                    threshold: 1,
                },
            );
        }

        let expires = now + Duration::days(30);

        let root_body = Root {
            type_: "root".into(),
            spec_version: "1.0".into(),
            version: 1,
            expires,
            keys,
            roles,
        };
        let mut targets_targets = BTreeMap::new();
        let mut hashes = BTreeMap::new();
        hashes.insert(
            "sha256".into(),
            "deadbeef".repeat(8), // 64 chars
        );
        targets_targets.insert(
            "lakko-svc-channels".into(),
            TargetMeta {
                length: 12_345_678,
                hashes,
            },
        );
        let targets_body = Targets {
            type_: "targets".into(),
            spec_version: "1.0".into(),
            version: 1,
            expires,
            targets: targets_targets,
        };
        let mut snapshot_meta = BTreeMap::new();
        snapshot_meta.insert(
            "targets.json".into(),
            MetaPin {
                version: 1,
                length: None,
            },
        );
        let snapshot_body = Snapshot {
            type_: "snapshot".into(),
            spec_version: "1.0".into(),
            version: 1,
            expires,
            meta: snapshot_meta,
        };
        let mut timestamp_meta = BTreeMap::new();
        timestamp_meta.insert(
            "snapshot.json".into(),
            MetaPin {
                version: 1,
                length: None,
            },
        );
        let timestamp_body = Timestamp {
            type_: "timestamp".into(),
            spec_version: "1.0".into(),
            version: 1,
            expires,
            meta: timestamp_meta,
        };

        (
            sign(root_body, signer, &key_id),
            sign(timestamp_body, signer, &key_id),
            sign(snapshot_body, signer, &key_id),
            sign(targets_body, signer, &key_id),
        )
    }

    /// Wrap a body in a Signed<T> envelope, signing it
    /// under `signer`.
    fn sign<T: Serialize>(body: T, signer: &SigningKey, key_id: &KeyId) -> Signed<T> {
        let canonical = canonical_json(&body).unwrap();
        let sig = signer.sign(&canonical);
        Signed {
            signed: body,
            signatures: vec![Signature {
                keyid: key_id.clone(),
                sig: hex::encode(sig.to_bytes()),
            }],
        }
    }

    #[test]
    fn verify_chain_happy_path() {
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let (root, timestamp, snapshot, targets) = build_chain(&signer, now);
        let targets_map =
            verify_chain(&root, &timestamp, &snapshot, &targets, now, None).unwrap();
        assert_eq!(targets_map.len(), 1);
        let entry = targets_map.get("lakko-svc-channels").unwrap();
        assert_eq!(entry.length, 12_345_678);
    }

    #[test]
    fn verify_chain_rejects_tampered_targets() {
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let (root, timestamp, snapshot, mut targets) = build_chain(&signer, now);
        // Tamper a target hash post-signing — the
        // signature is now over the original body.
        targets
            .signed
            .targets
            .get_mut("lakko-svc-channels")
            .unwrap()
            .hashes
            .insert("sha256".into(), "00".repeat(32));
        let err = verify_chain(&root, &timestamp, &snapshot, &targets, now, None)
            .unwrap_err();
        assert!(matches!(err, TufError::ThresholdNotMet { role: "targets", .. }));
    }

    #[test]
    fn verify_chain_rejects_wrong_signer() {
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let attacker = SigningKey::from_seed(&[0xFE; 32]);
        let now = Utc::now();
        let (root, timestamp, snapshot, mut targets) = build_chain(&signer, now);
        // Replace targets signature with one from the
        // attacker. The attacker's key id isn't in
        // root's `keyids` for the targets role.
        let attacker_id = KeyId::from_verifying_key(&attacker.verifying_key());
        let canonical = canonical_json(&targets.signed).unwrap();
        let attacker_sig = attacker.sign(&canonical);
        targets.signatures = vec![Signature {
            keyid: attacker_id,
            sig: hex::encode(attacker_sig.to_bytes()),
        }];
        let err = verify_chain(&root, &timestamp, &snapshot, &targets, now, None)
            .unwrap_err();
        assert!(matches!(err, TufError::ThresholdNotMet { role: "targets", .. }));
    }

    #[test]
    fn verify_chain_rejects_expired_timestamp() {
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let (root, mut timestamp, snapshot, targets) = build_chain(&signer, now);
        // Backdate timestamp's expires to before `now`.
        timestamp.signed.expires = now - Duration::seconds(1);
        // Re-sign so the signature still verifies — we
        // want the test to exercise the expiry check,
        // not the signature check.
        let key_id = KeyId::from_verifying_key(&signer.verifying_key());
        timestamp = sign(timestamp.signed, &signer, &key_id);
        let err = verify_chain(&root, &timestamp, &snapshot, &targets, now, None)
            .unwrap_err();
        assert!(matches!(err, TufError::Expired { role: "timestamp", .. }));
    }

    #[test]
    fn verify_chain_rejects_snapshot_version_mismatch() {
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let (root, timestamp, mut snapshot, targets) = build_chain(&signer, now);
        // Bump snapshot to v2 + re-sign so the signature
        // still verifies. Timestamp still pins v1 →
        // mismatch.
        snapshot.signed.version = 2;
        let key_id = KeyId::from_verifying_key(&signer.verifying_key());
        let resigned = sign(snapshot.signed, &signer, &key_id);
        let err = verify_chain(&root, &timestamp, &resigned, &targets, now, None)
            .unwrap_err();
        assert!(matches!(err, TufError::SnapshotVersionMismatch { .. }));
    }

    #[test]
    fn verify_chain_rejects_targets_version_mismatch() {
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let (root, timestamp, snapshot, mut targets) = build_chain(&signer, now);
        targets.signed.version = 2;
        let key_id = KeyId::from_verifying_key(&signer.verifying_key());
        let resigned = sign(targets.signed, &signer, &key_id);
        let err = verify_chain(&root, &timestamp, &snapshot, &resigned, now, None)
            .unwrap_err();
        assert!(matches!(err, TufError::TargetsVersionMismatch { .. }));
    }

    #[test]
    fn verify_chain_rejects_timestamp_version_rollback() {
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let (root, timestamp, snapshot, targets) = build_chain(&signer, now);
        // Pretend the client previously accepted
        // timestamp v5.
        let last = KnownVersions {
            timestamp: 5,
            snapshot: 1,
        };
        let err =
            verify_chain(&root, &timestamp, &snapshot, &targets, now, Some(&last))
                .unwrap_err();
        assert!(matches!(
            err,
            TufError::VersionRollback {
                role: "timestamp",
                got: 1,
                last: 5
            }
        ));
    }

    #[test]
    fn key_id_is_sha256_of_pubkey_bytes() {
        let signer = SigningKey::from_seed(&[0xAB; 32]);
        let vk = signer.verifying_key();
        let id = KeyId::from_verifying_key(&vk);
        // Determinism — same key always produces same
        // id. Pinned by recomputing.
        let expected = hex::encode(lakko_crypto::hash::sha256(&vk.to_bytes()));
        assert_eq!(id.0, expected);
    }

    #[test]
    fn role_spec_lookup_missing_returns_unauthorized() {
        // A root that omits the `targets` role spec
        // entirely surfaces as UnauthorizedKey when the
        // verifier looks it up.
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let (mut root, timestamp, snapshot, targets) = build_chain(&signer, now);
        root.signed.roles.remove("targets");
        let key_id = KeyId::from_verifying_key(&signer.verifying_key());
        let resigned = sign(root.signed, &signer, &key_id);
        let err =
            verify_chain(&resigned, &timestamp, &snapshot, &targets, now, None)
                .unwrap_err();
        assert!(matches!(err, TufError::UnauthorizedKey { role: "targets", .. }));
    }

    // -----------------------------------------------------
    // verify_root_chain (GA-5-rotation)
    // -----------------------------------------------------

    /// Build a single root metadata at the given
    /// version, signers, and threshold. Used by the
    /// rotation tests to construct hand-crafted chain
    /// segments.
    fn build_root_at(
        signers: &[&SigningKey],
        threshold: u32,
        version: u64,
        now: DateTime<Utc>,
    ) -> Signed<Root> {
        let mut keys = BTreeMap::new();
        let mut keyids = Vec::new();
        for signer in signers {
            let vk = signer.verifying_key();
            let kid = KeyId::from_verifying_key(&vk);
            keys.insert(kid.clone(), PublicKey::ed25519(&vk));
            keyids.push(kid);
        }
        let mut roles = BTreeMap::new();
        for role in ["root", "snapshot", "targets", "timestamp"] {
            roles.insert(
                role.into(),
                RoleSpec {
                    keyids: keyids.clone(),
                    threshold,
                },
            );
        }
        let body = Root {
            type_: "root".into(),
            spec_version: "1.0".into(),
            version,
            expires: now + Duration::days(30),
            keys,
            roles,
        };
        // Multi-signer sign — every signer attaches one
        // signature.
        let canonical = canonical_json(&body).unwrap();
        let signatures = signers
            .iter()
            .map(|s| {
                let kid = KeyId::from_verifying_key(&s.verifying_key());
                Signature {
                    keyid: kid,
                    sig: hex::encode(s.sign(&canonical).to_bytes()),
                }
            })
            .collect();
        Signed {
            signed: body,
            signatures,
        }
    }

    #[test]
    fn root_chain_single_element_returns_first() {
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let root_v1 = build_root_at(&[&signer], 1, 1, now);
        let pinned = vec![KeyId::from_verifying_key(&signer.verifying_key())];
        let chain = [root_v1];
        let final_root = verify_root_chain(&chain, &pinned, now).unwrap();
        assert_eq!(final_root.signed.version, 1);
    }

    #[test]
    fn root_chain_empty_slice_rejected() {
        let err = verify_root_chain(&[], &[], Utc::now()).unwrap_err();
        assert!(matches!(err, TufError::EmptyRootChain));
    }

    #[test]
    fn root_chain_walks_three_versions() {
        let signer_old = SigningKey::from_seed(&[0xAA; 32]);
        let signer_new = SigningKey::from_seed(&[0xBB; 32]);
        let now = Utc::now();
        // v1: signed by old key only.
        let v1 = build_root_at(&[&signer_old], 1, 1, now);
        // v2: signed by BOTH old + new — transition root.
        // The new key gains authority once v2 is
        // accepted; the old key signs it so v2 is
        // accepted by anyone holding v1's key set.
        let v2 = build_root_at(&[&signer_old, &signer_new], 1, 2, now);
        // v3: signed by new key only — old key fully
        // retired.
        let v3 = build_root_at(&[&signer_new], 1, 3, now);
        let pinned = vec![KeyId::from_verifying_key(&signer_old.verifying_key())];
        let chain = [v1, v2, v3];
        let final_root = verify_root_chain(&chain, &pinned, now).unwrap();
        assert_eq!(final_root.signed.version, 3);
    }

    #[test]
    fn root_chain_rejects_missing_intermediate() {
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let v1 = build_root_at(&[&signer], 1, 1, now);
        // Jump straight from v1 to v3 — missing the v2
        // intermediate. Even though v3 is signed by
        // the same key, the version gap is a fatal
        // protocol violation.
        let v3 = build_root_at(&[&signer], 1, 3, now);
        let pinned = vec![KeyId::from_verifying_key(&signer.verifying_key())];
        let chain = [v1, v3];
        let err = verify_root_chain(&chain, &pinned, now).unwrap_err();
        assert!(matches!(
            err,
            TufError::RootVersionGap { from: 1, to: 3 }
        ));
    }

    #[test]
    fn root_chain_rejects_unrelated_new_root() {
        // v2 signed by a key the v1 root never
        // authorised — attacker tries to inject their
        // own root mid-chain. v2 fails to verify against
        // v1's `root` role.
        let signer_old = SigningKey::from_seed(&[0x42; 32]);
        let attacker = SigningKey::from_seed(&[0xFE; 32]);
        let now = Utc::now();
        let v1 = build_root_at(&[&signer_old], 1, 1, now);
        let v2 = build_root_at(&[&attacker], 1, 2, now);
        let pinned = vec![KeyId::from_verifying_key(&signer_old.verifying_key())];
        let chain = [v1, v2];
        let err = verify_root_chain(&chain, &pinned, now).unwrap_err();
        assert!(matches!(
            err,
            TufError::ThresholdNotMet { role: "root", .. }
        ));
    }

    #[test]
    fn root_chain_rejects_untrusted_first_root() {
        // First root in the chain is NOT signed by any
        // pinned trust anchor — fresh-install bootstrap
        // refuses it.
        let attacker = SigningKey::from_seed(&[0xFE; 32]);
        let trusted = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let v1 = build_root_at(&[&attacker], 1, 1, now);
        // Pinned anchor is the *trusted* key, not the
        // attacker.
        let pinned = vec![KeyId::from_verifying_key(&trusted.verifying_key())];
        let chain = [v1];
        let err = verify_root_chain(&chain, &pinned, now).unwrap_err();
        assert!(matches!(err, TufError::UntrustedFirstRoot));
    }

    #[test]
    fn root_chain_rejects_version_rollback() {
        // v1 followed by v0 — version went down.
        let signer = SigningKey::from_seed(&[0x42; 32]);
        let now = Utc::now();
        let v1 = build_root_at(&[&signer], 1, 1, now);
        let v0 = build_root_at(&[&signer], 1, 0, now);
        let pinned = vec![KeyId::from_verifying_key(&signer.verifying_key())];
        let chain = [v1, v0];
        let err = verify_root_chain(&chain, &pinned, now).unwrap_err();
        assert!(matches!(
            err,
            TufError::VersionRollback {
                role: "root",
                got: 0,
                last: 1
            }
        ));
    }

    #[test]
    fn root_chain_final_root_drives_verify_chain() {
        // The whole point of the rotation walk: the
        // final root in a valid chain becomes the trust
        // anchor that `verify_chain` consumes. Build a
        // 2-version chain and prove the rest of the
        // metadata verifies under the final root.
        let signer_old = SigningKey::from_seed(&[0x10; 32]);
        let signer_new = SigningKey::from_seed(&[0x20; 32]);
        let now = Utc::now();
        let v1 = build_root_at(&[&signer_old], 1, 1, now);
        let v2 = build_root_at(&[&signer_old, &signer_new], 1, 2, now);
        let pinned = vec![KeyId::from_verifying_key(&signer_old.verifying_key())];
        let chain = [v1, v2];
        let final_root = verify_root_chain(&chain, &pinned, now).unwrap();
        // Sanity — final root is v2.
        assert_eq!(final_root.signed.version, 2);
        // Build the rest of the chain signed under the
        // v2 root (which authorises both signers); use
        // the new signer to prove the trust transition
        // actually granted it authority.
        let (_root_unused, timestamp, snapshot, targets) =
            build_chain(&signer_new, now);
        // Replace the canned root from build_chain with
        // the multi-version v2 — verify_chain accepts it
        // because v2's `targets` role authorises both
        // signer_old and signer_new.
        let targets_map =
            verify_chain(final_root, &timestamp, &snapshot, &targets, now, None)
                .unwrap();
        assert_eq!(targets_map.len(), 1);
    }

    // ---------------------------------------------------
    // verify_target_bytes tests
    // ---------------------------------------------------

    /// Build a `TargetMeta` whose `length` + `sha256`
    /// agree with the supplied bytes. Used as the happy
    /// fixture across the verify_target_bytes tests.
    fn target_meta_for(bytes: &[u8]) -> TargetMeta {
        let digest = lakko_crypto::hash::sha256(bytes);
        let mut hashes = BTreeMap::new();
        hashes.insert("sha256".into(), hex::encode(digest));
        TargetMeta {
            length: bytes.len() as u64,
            hashes,
        }
    }

    #[test]
    fn verify_target_bytes_happy_path() {
        let bytes = b"hello, lakko TUF target bytes";
        let meta = target_meta_for(bytes);
        verify_target_bytes(&meta, bytes).expect("happy path verifies");
    }

    #[test]
    fn verify_target_bytes_wrong_length_rejected() {
        let bytes = b"hello, lakko TUF target bytes";
        let mut meta = target_meta_for(bytes);
        meta.length += 1; // claim one byte more than we have

        match verify_target_bytes(&meta, bytes) {
            Err(TufError::TargetLengthMismatch { expected, got }) => {
                assert_eq!(expected, meta.length);
                assert_eq!(got, bytes.len() as u64);
            }
            other => panic!("expected TargetLengthMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_target_bytes_wrong_digest_rejected() {
        let bytes = b"hello, lakko TUF target bytes";
        let mut meta = target_meta_for(bytes);
        // Flip one hex char in the expected digest — still
        // valid hex of the right length, but no longer the
        // correct digest.
        let entry = meta.hashes.get_mut("sha256").unwrap();
        let mut chars: Vec<char> = entry.chars().collect();
        chars[0] = if chars[0] == 'f' { '0' } else { 'f' };
        *entry = chars.into_iter().collect();

        match verify_target_bytes(&meta, bytes) {
            Err(TufError::TargetHashMismatch { algo, .. }) => {
                assert_eq!(algo, "sha256");
            }
            other => panic!("expected TargetHashMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_target_bytes_no_known_algo_rejected() {
        let bytes = b"hello, lakko TUF target bytes";
        // Length matches, but the only hash is under a
        // future algorithm this build does not understand.
        let mut hashes = BTreeMap::new();
        hashes.insert("sha512".into(), "00".repeat(64));
        hashes.insert("blake2b256".into(), "00".repeat(32));
        let meta = TargetMeta {
            length: bytes.len() as u64,
            hashes,
        };

        match verify_target_bytes(&meta, bytes) {
            Err(TufError::NoKnownTargetHashAlgo { available }) => {
                // BTreeMap iteration order is sorted by
                // key; both unknown algos are surfaced.
                assert!(available.contains(&"sha512".to_string()));
                assert!(available.contains(&"blake2b256".to_string()));
            }
            other => panic!("expected NoKnownTargetHashAlgo, got {other:?}"),
        }
    }

    #[test]
    fn verify_target_bytes_unknown_algo_alongside_sha256_accepted() {
        let bytes = b"hello, lakko TUF target bytes";
        let digest = lakko_crypto::hash::sha256(bytes);
        let mut hashes = BTreeMap::new();
        // A real sha256 plus a forward-compat algorithm we
        // don't know how to compute. Verifier MUST accept
        // — the sha256 is enough, the unknown is skipped.
        hashes.insert("sha256".into(), hex::encode(digest));
        hashes.insert("sha512".into(), "ff".repeat(64));
        let meta = TargetMeta {
            length: bytes.len() as u64,
            hashes,
        };

        verify_target_bytes(&meta, bytes)
            .expect("unknown algo alongside sha256 still verifies");
    }

    #[test]
    fn verify_target_bytes_empty_hashes_rejected() {
        // Edge case: a `TargetMeta` whose `hashes` map is
        // empty. Length-only verification is unsafe, so we
        // must reject the same way as the unknown-algo
        // case.
        let bytes = b"hello, lakko TUF target bytes";
        let meta = TargetMeta {
            length: bytes.len() as u64,
            hashes: BTreeMap::new(),
        };
        match verify_target_bytes(&meta, bytes) {
            Err(TufError::NoKnownTargetHashAlgo { available }) => {
                assert!(available.is_empty());
            }
            other => panic!("expected NoKnownTargetHashAlgo, got {other:?}"),
        }
    }
}
