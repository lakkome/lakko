// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! TUF mirror selection + failover (GA-5).
//!
//! The verifier crate is OFFLINE — it never fetches. This module is the
//! pure-Rust orchestration a platform install-client uses to decide WHICH
//! mirror to pull a metadata file or target from, honouring each mirror's
//! confinement, and to FAIL OVER to the next candidate when a fetch fails. The
//! actual byte fetch is the caller's (e.g. `DohFetcher`); this module never
//! touches the network.
//!
//! Security: a mirror may be CONFINED to a set of target-path patterns
//! (`confined_target_dirs`, TUF mirrors metadata). A confined mirror is only
//! ever offered targets it is allowed to serve, so a low-trust mirror cannot
//! serve a target outside its remit even if it is reachable. (Metadata is
//! served by every mirror — the signed chain authenticates it regardless of
//! source.) Selection is a TRANSPORT choice and is INDEPENDENT of trust: the
//! bytes a mirror returns are trusted only after `verify_chain` /
//! `verify_target_bytes`, never because of which mirror served them.

use serde::{Deserialize, Serialize};

use crate::glob_match;

/// Maximum mirrors a [`MirrorList`] may carry. The list comes from signed
/// bootstrap config (not wire-parser-attacker-controlled), but a sane cap
/// keeps a pathological config from ballooning selection work.
pub const MAX_MIRRORS: usize = 64;

/// One TUF mirror — a base URL plus optional target-path confinement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mirror {
    /// Base URL the install-client prefixes onto the `lakko_tuf::urls` paths.
    pub url_base: String,
    /// Glob patterns (same fnmatch semantics as TUF `paths`) the mirror is
    /// allowed to serve TARGETS for. EMPTY means no restriction — the mirror
    /// serves every target.
    #[serde(default)]
    pub confined_target_dirs: Vec<String>,
}

impl Mirror {
    /// Is this mirror allowed to serve `target_name`? An empty confinement
    /// serves all; otherwise the name must match at least one confined-dir
    /// glob.
    #[must_use]
    pub fn serves_target(&self, target_name: &str) -> bool {
        self.confined_target_dirs.is_empty()
            || self
                .confined_target_dirs
                .iter()
                .any(|p| glob_match(p, target_name))
    }
}

/// An ordered list of mirrors. Order is the failover priority (first =
/// highest).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorList {
    /// Mirrors in descending failover priority.
    pub mirrors: Vec<Mirror>,
}

/// Why a mirror-selection / fetch orchestration failed.
#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    /// The mirror list is empty, larger than [`MAX_MIRRORS`], or carries a
    /// mirror with an empty `url_base`; or `try_in_order` got no candidates.
    #[error("invalid mirror selection: {reason}")]
    Invalid {
        /// Static description of the defect.
        reason: &'static str,
    },
    /// No mirror in the list is allowed to serve the requested target.
    #[error("no mirror serves target {target}")]
    NoMirrorForTarget {
        /// The target name with no serving mirror.
        target: String,
    },
    /// Every candidate mirror's fetch failed. Carries the per-mirror error
    /// message in attempt order.
    #[error("all {} mirror fetches failed", attempts.len())]
    AllFailed {
        /// `(url_base, error string)` per failed attempt, in order.
        attempts: Vec<(String, String)>,
    },
}

impl MirrorList {
    /// Validate the list shape (non-empty, within [`MAX_MIRRORS`], no empty
    /// `url_base`).
    ///
    /// # Errors
    ///
    /// [`MirrorError::Invalid`] on an empty / over-large list or a mirror with
    /// an empty `url_base`.
    pub fn validate(&self) -> Result<(), MirrorError> {
        if self.mirrors.is_empty() {
            return Err(MirrorError::Invalid {
                reason: "mirror list is empty",
            });
        }
        if self.mirrors.len() > MAX_MIRRORS {
            return Err(MirrorError::Invalid {
                reason: "mirror list exceeds MAX_MIRRORS",
            });
        }
        if self.mirrors.iter().any(|m| m.url_base.trim().is_empty()) {
            return Err(MirrorError::Invalid {
                reason: "a mirror has an empty url_base",
            });
        }
        Ok(())
    }

    /// The mirrors to try, in order, for a METADATA file. Every mirror serves
    /// metadata (the signed chain authenticates it regardless of source).
    #[must_use]
    pub fn for_metadata(&self) -> Vec<&Mirror> {
        self.mirrors.iter().collect()
    }

    /// The mirrors to try, in order, for `target_name` — only those whose
    /// confinement allows it.
    ///
    /// # Errors
    ///
    /// [`MirrorError::NoMirrorForTarget`] if no mirror is confined to serve the
    /// target (an explicit refusal rather than a silent empty candidate set).
    pub fn for_target(&self, target_name: &str) -> Result<Vec<&Mirror>, MirrorError> {
        let candidates: Vec<&Mirror> = self
            .mirrors
            .iter()
            .filter(|m| m.serves_target(target_name))
            .collect();
        if candidates.is_empty() {
            return Err(MirrorError::NoMirrorForTarget {
                target: target_name.to_owned(),
            });
        }
        Ok(candidates)
    }
}

/// Try `fetch` against each candidate mirror IN ORDER, returning the first
/// success (the candidate index + the bytes), or [`MirrorError::AllFailed`]
/// aggregating every per-mirror error.
///
/// Pure orchestration — `fetch` owns the network; the bytes are NOT trusted
/// here (the caller verifies them via `verify_chain` / `verify_target_bytes`).
/// A mirror is a transport choice only.
///
/// # Errors
///
/// [`MirrorError::Invalid`] if `candidates` is empty; [`MirrorError::AllFailed`]
/// if every fetch fails.
pub fn try_in_order<E, F>(
    candidates: &[&Mirror],
    mut fetch: F,
) -> Result<(usize, Vec<u8>), MirrorError>
where
    E: std::fmt::Display,
    F: FnMut(&Mirror) -> Result<Vec<u8>, E>,
{
    if candidates.is_empty() {
        return Err(MirrorError::Invalid {
            reason: "no candidate mirrors",
        });
    }
    let mut attempts: Vec<(String, String)> = Vec::with_capacity(candidates.len());
    for (idx, mirror) in candidates.iter().enumerate() {
        match fetch(mirror) {
            Ok(bytes) => return Ok((idx, bytes)),
            Err(e) => attempts.push((mirror.url_base.clone(), e.to_string())),
        }
    }
    Err(MirrorError::AllFailed { attempts })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirror(url: &str, confined: &[&str]) -> Mirror {
        Mirror {
            url_base: url.to_owned(),
            confined_target_dirs: confined.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn unconfined_mirror_serves_every_target() {
        let m = mirror("https://m1", &[]);
        assert!(m.serves_target("lakko-svc-sfu"));
        assert!(m.serves_target("anything/at/all"));
    }

    #[test]
    fn confined_mirror_serves_only_matching_targets() {
        let m = mirror("https://m1", &["lakko-svc-*", "plugins/*"]);
        assert!(m.serves_target("lakko-svc-sfu"));
        assert!(m.serves_target("plugins/audio"));
        assert!(!m.serves_target("secret-binary"));
    }

    #[test]
    fn for_target_filters_by_confinement_in_order() {
        let list = MirrorList {
            mirrors: vec![
                mirror("https://core", &["lakko-svc-*"]),
                mirror("https://cdn", &[]),
                mirror("https://plugins", &["plugins/*"]),
            ],
        };
        let cands = list.for_target("lakko-svc-sfu").unwrap();
        // core (matches) + cdn (unconfined) serve it; plugins does not.
        let urls: Vec<&str> = cands.iter().map(|m| m.url_base.as_str()).collect();
        assert_eq!(urls, vec!["https://core", "https://cdn"]);
    }

    #[test]
    fn for_target_refuses_when_no_mirror_serves_it() {
        let list = MirrorList {
            mirrors: vec![mirror("https://core", &["lakko-svc-*"])],
        };
        let err = list.for_target("secret-binary").unwrap_err();
        assert!(
            matches!(err, MirrorError::NoMirrorForTarget { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn for_metadata_returns_all_mirrors_in_order() {
        let list = MirrorList {
            mirrors: vec![mirror("https://a", &["x"]), mirror("https://b", &[])],
        };
        let urls: Vec<&str> = list
            .for_metadata()
            .iter()
            .map(|m| m.url_base.as_str())
            .collect();
        assert_eq!(urls, vec!["https://a", "https://b"]);
    }

    #[test]
    fn validate_rejects_empty_over_large_and_blank_url() {
        assert!(MirrorList::default().validate().is_err());
        let blank = MirrorList {
            mirrors: vec![mirror("  ", &[])],
        };
        assert!(blank.validate().is_err());
        let too_many = MirrorList {
            mirrors: (0..=MAX_MIRRORS)
                .map(|i| mirror(&format!("https://m{i}"), &[]))
                .collect(),
        };
        assert!(too_many.validate().is_err());
        let ok = MirrorList {
            mirrors: vec![mirror("https://m", &[])],
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn try_in_order_returns_first_success_and_stops() {
        let m1 = mirror("https://down", &[]);
        let m2 = mirror("https://up", &[]);
        let m3 = mirror("https://never", &[]);
        let candidates = [&m1, &m2, &m3];
        let mut visited: Vec<String> = Vec::new();
        let (idx, bytes) = try_in_order(&candidates, |m| {
            visited.push(m.url_base.clone());
            if m.url_base == "https://up" {
                Ok(vec![1, 2, 3])
            } else {
                Err("connection refused")
            }
        })
        .unwrap();
        assert_eq!(idx, 1);
        assert_eq!(bytes, vec![1, 2, 3]);
        // The third mirror must NOT have been tried (failover stops on success).
        assert_eq!(visited, vec!["https://down", "https://up"]);
    }

    #[test]
    fn try_in_order_aggregates_when_all_fail() {
        let m1 = mirror("https://a", &[]);
        let m2 = mirror("https://b", &[]);
        let candidates = [&m1, &m2];
        let err =
            try_in_order(&candidates, |_m| -> Result<Vec<u8>, &str> { Err("boom") }).unwrap_err();
        match err {
            MirrorError::AllFailed { attempts } => {
                assert_eq!(attempts.len(), 2);
                assert_eq!(attempts[0].0, "https://a");
                assert_eq!(attempts[1].0, "https://b");
            },
            other => panic!("expected AllFailed, got {other:?}"),
        }
    }

    #[test]
    fn try_in_order_refuses_empty_candidates() {
        let candidates: [&Mirror; 0] = [];
        let err =
            try_in_order(&candidates, |_m| -> Result<Vec<u8>, &str> { Ok(vec![]) }).unwrap_err();
        assert!(matches!(err, MirrorError::Invalid { .. }), "got {err:?}");
    }
}
