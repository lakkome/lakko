// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! TUF v1.1 metadata + target-artefact URL conventions.
//!
//! Pure URL builders for the non-consistent-snapshot
//! variant of TUF §5.0 — Lakko is small enough that
//! hashed-bin / consistent-snapshot URLs are unnecessary
//! at this point. If we ever switch, this module grows a
//! second set of `_consistent` helpers; existing callers
//! keep working.
//!
//! ## URL shape
//!
//! Given a `base` such as `https://updates.lakko.me/`:
//!
//! | Helper                            | Result                                        |
//! |---|---|
//! | [`root_url`]`(base, 7)`           | `https://updates.lakko.me/7.root.json`        |
//! | [`timestamp_url`]`(base)`         | `https://updates.lakko.me/timestamp.json`     |
//! | [`snapshot_url`]`(base)`          | `https://updates.lakko.me/snapshot.json`      |
//! | [`targets_url`]`(base)`           | `https://updates.lakko.me/targets.json`       |
//! | [`target_artifact_url`]`(base, "lakko-svc-channels")` | `https://updates.lakko.me/targets/lakko-svc-channels` |
//!
//! The trailing slash on `base` is normalised — both
//! `https://updates.lakko.me` and `https://updates.lakko.me/`
//! produce the same output.
//!
//! ## Why a dedicated module
//!
//! - Pins the URL convention in code so platform-specific
//!   install glue (Android / iOS / desktop / web) does not
//!   re-derive it from the spec and risk a typo.
//! - [`target_artifact_url`] is the one helper with a
//!   security boundary — target names come from signed
//!   targets metadata, but defense in depth says we still
//!   refuse path-traversal segments at the URL builder
//!   layer, so a publisher that accidentally minted a
//!   bad-looking target name cannot cause the client to
//!   GET `…/targets/../etc/passwd`.

use crate::{Result, TufError};

/// Build the URL for root metadata version `version` —
/// `{base}/{version}.root.json`.
///
/// Root is the only metadata kept under a versioned URL
/// in the non-consistent-snapshot variant: the client
/// walks the chain `{1}.root.json → {2}.root.json → …`
/// during rotation.
#[must_use]
pub fn root_url(base: &str, version: u64) -> String {
    format!("{}/{}.root.json", trim_trailing_slash(base), version)
}

/// Build the URL for the current timestamp metadata —
/// `{base}/timestamp.json`. Always unversioned; the
/// timestamp is the short-lived freshness proof.
#[must_use]
pub fn timestamp_url(base: &str) -> String {
    format!("{}/timestamp.json", trim_trailing_slash(base))
}

/// Build the URL for the current snapshot metadata —
/// `{base}/snapshot.json`. Unversioned in the
/// non-consistent-snapshot variant; the version is pinned
/// by the timestamp and verified by [`crate::verify_chain`].
#[must_use]
pub fn snapshot_url(base: &str) -> String {
    format!("{}/snapshot.json", trim_trailing_slash(base))
}

/// Build the URL for the current targets metadata —
/// `{base}/targets.json`.
#[must_use]
pub fn targets_url(base: &str) -> String {
    format!("{}/targets.json", trim_trailing_slash(base))
}

/// Build the URL for a target binary by name —
/// `{base}/targets/{name}`. The name is the key from the
/// signed `Targets.targets` map (e.g.
/// `"lakko-svc-channels"`).
///
/// # Path-traversal defence
///
/// Target names come from signed targets metadata, so
/// under the threat model they are publisher-controlled
/// not attacker-controlled. Defense in depth: the helper
/// still refuses any of the following:
///
/// - Empty name.
/// - Name containing `..` (path-traversal segment).
/// - Name containing `/` or `\\` (sub-paths are not
///   supported in the scaffold; once delegated targets
///   land, sub-paths get their own builder).
/// - Name containing a NUL byte.
///
/// An attacker who minted a malicious target metadata
/// would have to compromise the targets signing key
/// first; this gate just makes sure a careless publisher
/// (or a future scaffold bug) cannot turn a bad name into
/// a `GET …/targets/../etc/passwd`.
///
/// # Errors
///
/// [`TufError::BadTargetName`] on any of the conditions
/// above. The `reason` field is `&'static str` so the
/// caller can branch / log without allocation.
pub fn target_artifact_url(base: &str, name: &str) -> Result<String> {
    if name.is_empty() {
        return Err(TufError::BadTargetName {
            reason: "target name is empty",
        });
    }
    if name.contains('\0') {
        return Err(TufError::BadTargetName {
            reason: "target name contains NUL byte",
        });
    }
    // Refuse path separators outright; sub-paths in target
    // names will land with delegated targets, with their
    // own builder.
    if name.contains('/') || name.contains('\\') {
        return Err(TufError::BadTargetName {
            reason: "target name contains path separator",
        });
    }
    // `..` as a substring is enough to reject — even
    // `foo..bar` is suspicious (no legitimate target name
    // ever needs a double-dot), and the cost of being
    // strict here is zero.
    if name.contains("..") {
        return Err(TufError::BadTargetName {
            reason: "target name contains `..`",
        });
    }
    Ok(format!("{}/targets/{}", trim_trailing_slash(base), name))
}

/// Strip a single trailing `/` from `base` so the
/// concatenation operator does not double-slash. Idempotent
/// — `trim_trailing_slash("https://x/")` and
/// `trim_trailing_slash("https://x")` both return
/// `"https://x"`.
fn trim_trailing_slash(base: &str) -> &str {
    base.strip_suffix('/').unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_url_versioned() {
        assert_eq!(
            root_url("https://updates.lakko.me", 1),
            "https://updates.lakko.me/1.root.json",
        );
        assert_eq!(
            root_url("https://updates.lakko.me/", 42),
            "https://updates.lakko.me/42.root.json",
        );
    }

    #[test]
    fn timestamp_url_unversioned() {
        assert_eq!(
            timestamp_url("https://updates.lakko.me"),
            "https://updates.lakko.me/timestamp.json",
        );
        assert_eq!(
            timestamp_url("https://updates.lakko.me/"),
            "https://updates.lakko.me/timestamp.json",
        );
    }

    #[test]
    fn snapshot_url_unversioned() {
        assert_eq!(
            snapshot_url("https://updates.lakko.me"),
            "https://updates.lakko.me/snapshot.json",
        );
    }

    #[test]
    fn targets_url_unversioned() {
        assert_eq!(
            targets_url("https://updates.lakko.me"),
            "https://updates.lakko.me/targets.json",
        );
    }

    #[test]
    fn target_artifact_url_happy_path() {
        let url = target_artifact_url("https://updates.lakko.me", "lakko-svc-channels")
            .expect("valid name");
        assert_eq!(url, "https://updates.lakko.me/targets/lakko-svc-channels");
    }

    #[test]
    fn target_artifact_url_normalises_trailing_slash() {
        let url = target_artifact_url("https://updates.lakko.me/", "lakko-svc-channels")
            .expect("valid name");
        assert_eq!(url, "https://updates.lakko.me/targets/lakko-svc-channels");
    }

    #[test]
    fn target_artifact_url_rejects_empty_name() {
        match target_artifact_url("https://updates.lakko.me", "") {
            Err(TufError::BadTargetName { reason }) => {
                assert!(reason.contains("empty"));
            },
            other => panic!("expected BadTargetName, got {other:?}"),
        }
    }

    #[test]
    fn target_artifact_url_rejects_double_dot() {
        for name in [
            "..",
            "../etc/passwd",
            "foo/..",
            "foo/../bar",
            "..foo",
            "foo..bar",
        ] {
            match target_artifact_url("https://updates.lakko.me", name) {
                Err(TufError::BadTargetName { .. }) => {},
                other => panic!("expected BadTargetName for {name:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn target_artifact_url_rejects_path_separators() {
        for name in ["sub/path", "back\\slash", "/leading", "trailing/"] {
            match target_artifact_url("https://updates.lakko.me", name) {
                Err(TufError::BadTargetName { reason }) => {
                    assert!(
                        reason.contains("separator"),
                        "wrong reason for {name:?}: {reason}"
                    );
                },
                other => panic!("expected BadTargetName for {name:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn target_artifact_url_rejects_nul_byte() {
        match target_artifact_url("https://updates.lakko.me", "foo\0bar") {
            Err(TufError::BadTargetName { reason }) => {
                assert!(reason.contains("NUL"));
            },
            other => panic!("expected BadTargetName, got {other:?}"),
        }
    }

    #[test]
    fn trim_trailing_slash_idempotent() {
        assert_eq!(trim_trailing_slash("https://x"), "https://x");
        assert_eq!(trim_trailing_slash("https://x/"), "https://x");
        // Only a single trailing slash is stripped; a
        // double-slash is a caller bug we surface rather
        // than paper over.
        assert_eq!(trim_trailing_slash("https://x//"), "https://x/");
    }
}
