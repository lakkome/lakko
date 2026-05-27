// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end integration test for the TUF install walk
//! — ties together every module the GA-5 series shipped:
//!
//! 1. [`urls`] builds the URLs the publisher exposes.
//! 2. The publisher serialises each role to canonical JSON
//!    at that URL.
//! 3. The client "fetches" those bytes (in the test, just
//!    `to_json` from the same fixture).
//! 4. `Signed::<T>::from_json` parses the bytes back.
//! 5. [`verify_chain`] runs the chain-of-trust walk and
//!    returns a `TargetMap`.
//! 6. For each target, the client builds the target's
//!    artefact URL, "fetches" the binary, and runs
//!    [`verify_target_bytes`] to confirm length + SHA-256.
//!
//! If any module silently drifts against its sibling —
//! the URL the urls module produces doesn't match what
//! the publisher serialises at, the parse stops feeding
//! verify_chain, or the TargetMap stops feeding
//! verify_target_bytes — this test breaks. It is the
//! single integration anchor for the post-Phase-3
//! auto-update flow.
//!
//! What this test does NOT cover: the actual network
//! fetch and the platform-side install glue (atomic-swap
//! binary replacement). Both are caller responsibilities
//! outside the verifier crate.

use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use lakko_crypto::signing::SigningKey;
use lakko_tuf::{
    canonical_json_for_test, urls, verify_chain, verify_target_bytes, KeyId, KeyVal, MetaPin,
    PublicKey, RoleMap, RoleSpec, Root, Signature, Signed, Snapshot, TargetMeta, Targets,
    Timestamp,
};

const PUBLISHER_BASE: &str = "https://updates.lakko.me";
const TARGET_NAME: &str = "lakko-svc-channels";

/// Full happy-path walk. Every cross-module hand-off is
/// exercised in series.
#[test]
fn full_install_walk_round_trips() {
    let signer = SigningKey::from_seed(&[0xC0; 32]);
    let now = Utc::now();

    // Build a deterministic binary the publisher would
    // ship as the target — its length + SHA-256 land in
    // the signed targets metadata.
    let target_bytes: Vec<u8> = (0..2048).map(|i| u8::try_from(i % 256).unwrap()).collect();

    let (root, timestamp, snapshot, targets) = build_chain(&signer, now, &target_bytes);

    // -----------------------------------------------------
    // Stage 1 — urls module commits to a URL convention.
    // The test never actually hits these URLs; it just
    // confirms the strings have the shape the publisher
    // would serve at and the client would GET.
    // -----------------------------------------------------
    let root_v1 = urls::root_url(PUBLISHER_BASE, root.signed.version);
    let ts_url = urls::timestamp_url(PUBLISHER_BASE);
    let snap_url = urls::snapshot_url(PUBLISHER_BASE);
    let tgts_url = urls::targets_url(PUBLISHER_BASE);
    let target_url =
        urls::target_artifact_url(PUBLISHER_BASE, TARGET_NAME).expect("safe target name");

    assert_eq!(root_v1, format!("{PUBLISHER_BASE}/1.root.json"));
    assert_eq!(ts_url, format!("{PUBLISHER_BASE}/timestamp.json"));
    assert_eq!(snap_url, format!("{PUBLISHER_BASE}/snapshot.json"));
    assert_eq!(tgts_url, format!("{PUBLISHER_BASE}/targets.json"));
    assert_eq!(
        target_url,
        format!("{PUBLISHER_BASE}/targets/{TARGET_NAME}")
    );

    // -----------------------------------------------------
    // Stage 2 — publisher serialises each role.
    // Stage 3 — client "fetches" those bytes (in this
    // test, we just route the JSON byte slices through).
    // Stage 4 — parse back via Signed::from_json.
    // -----------------------------------------------------
    let root_json = root.to_json().expect("serialise root");
    let ts_json = timestamp.to_json().expect("serialise timestamp");
    let snap_json = snapshot.to_json().expect("serialise snapshot");
    let tgts_json = targets.to_json().expect("serialise targets");

    let parsed_root: Signed<Root> = Signed::from_json(&root_json).expect("parse root");
    let parsed_ts: Signed<Timestamp> = Signed::from_json(&ts_json).expect("parse timestamp");
    let parsed_snap: Signed<Snapshot> = Signed::from_json(&snap_json).expect("parse snapshot");
    let parsed_tgts: Signed<Targets> = Signed::from_json(&tgts_json).expect("parse targets");

    // -----------------------------------------------------
    // Stage 5 — chain-of-trust walk. verify_chain
    // re-canonicalises each parsed body and verifies the
    // signature, so the parse round-trip must preserve
    // signing bytes byte-for-byte; if from_json or to_json
    // ever drifts, this assertion fails.
    // -----------------------------------------------------
    let target_map = verify_chain(
        &parsed_root,
        &parsed_ts,
        &parsed_snap,
        &parsed_tgts,
        now,
        None,
    )
    .expect("chain verifies");

    let target_meta = target_map
        .get(TARGET_NAME)
        .expect("publisher target present in verified map");

    // -----------------------------------------------------
    // Stage 6 — verify the actual binary. In production
    // the client GETs target_url; here we already have
    // the bytes the publisher minted in build_chain.
    // -----------------------------------------------------
    verify_target_bytes(target_meta, &target_bytes)
        .expect("publisher target bytes verify against signed meta");
}

/// Negative path — if the binary at the target URL is
/// even one byte off from the length the signed metadata
/// commits to, the install MUST refuse. Confirms that
/// the verify_target_bytes gate is wired all the way to
/// the end of the walk, not bypassed somewhere.
#[test]
fn full_install_walk_rejects_tampered_target_bytes() {
    let signer = SigningKey::from_seed(&[0xC1; 32]);
    let now = Utc::now();
    let target_bytes: Vec<u8> = (0..2048).map(|i| u8::try_from(i % 256).unwrap()).collect();
    let (root, timestamp, snapshot, targets) = build_chain(&signer, now, &target_bytes);
    let root_json = root.to_json().unwrap();
    let ts_json = timestamp.to_json().unwrap();
    let snap_json = snapshot.to_json().unwrap();
    let tgts_json = targets.to_json().unwrap();
    let parsed_root: Signed<Root> = Signed::from_json(&root_json).unwrap();
    let parsed_ts: Signed<Timestamp> = Signed::from_json(&ts_json).unwrap();
    let parsed_snap: Signed<Snapshot> = Signed::from_json(&snap_json).unwrap();
    let parsed_tgts: Signed<Targets> = Signed::from_json(&tgts_json).unwrap();
    let target_map = verify_chain(
        &parsed_root,
        &parsed_ts,
        &parsed_snap,
        &parsed_tgts,
        now,
        None,
    )
    .unwrap();
    let target_meta = target_map.get(TARGET_NAME).unwrap();

    // Attacker substitutes one byte mid-stream — same
    // length, different digest.
    let mut tampered = target_bytes.clone();
    tampered[1024] ^= 0xFF;
    let err = verify_target_bytes(target_meta, &tampered)
        .expect_err("flipped byte must be caught by sha256 gate");
    assert!(
        matches!(err, lakko_tuf::TufError::TargetHashMismatch { .. }),
        "expected TargetHashMismatch, got {err:?}"
    );

    // Attacker truncates the binary — length mismatch
    // catches it before the digest path runs.
    let truncated = &target_bytes[..target_bytes.len() - 1];
    let err = verify_target_bytes(target_meta, truncated)
        .expect_err("truncated bytes must be caught by length gate");
    assert!(
        matches!(err, lakko_tuf::TufError::TargetLengthMismatch { .. }),
        "expected TargetLengthMismatch, got {err:?}"
    );
}

// ---------------------------------------------------------
// Test fixture — single-signer chain with one target
// whose `length` + `sha256` are computed from the actual
// bytes the test threads through. Symmetric to the
// fixture in `tests/bootstrap.rs` but parameterised over
// the target bytes so the negative-path test can flip a
// byte and have the digest still bind to the metadata.
// ---------------------------------------------------------

fn build_chain(
    signer: &SigningKey,
    now: chrono::DateTime<Utc>,
    target_bytes: &[u8],
) -> (
    Signed<Root>,
    Signed<Timestamp>,
    Signed<Snapshot>,
    Signed<Targets>,
) {
    let vk = signer.verifying_key();
    let key_id = KeyId::from_verifying_key(&vk);
    let pubkey = PublicKey {
        keytype: "ed25519".into(),
        scheme: "ed25519".into(),
        keyval: KeyVal {
            public: hex::encode(vk.to_bytes()),
        },
    };

    let mut keys = BTreeMap::new();
    keys.insert(key_id.clone(), pubkey);

    let mut roles: RoleMap = BTreeMap::new();
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

    let digest = lakko_crypto::hash::sha256(target_bytes);
    let mut hashes = BTreeMap::new();
    hashes.insert("sha256".into(), hex::encode(digest));
    let mut targets_targets = BTreeMap::new();
    targets_targets.insert(
        TARGET_NAME.into(),
        TargetMeta {
            length: target_bytes.len() as u64,
            hashes,
        },
    );
    let targets_body = Targets {
        type_: "targets".into(),
        spec_version: "1.0".into(),
        version: 1,
        expires,
        targets: targets_targets,
        delegations: None,
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

fn sign<T: serde::Serialize>(body: T, signer: &SigningKey, key_id: &KeyId) -> Signed<T> {
    let canonical = canonical_json_for_test(&body).unwrap();
    let sig = signer.sign(&canonical);
    Signed {
        signed: body,
        signatures: vec![Signature {
            keyid: key_id.clone(),
            sig: hex::encode(sig.to_bytes()),
        }],
    }
}
