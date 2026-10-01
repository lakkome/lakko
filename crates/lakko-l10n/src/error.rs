// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

use thiserror::Error;

/// Everything that can go wrong in this crate.
///
/// None of these reach a person: a client that gets one falls back to the
/// built-in catalog (or to English), silently, as the UX spec requires.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum L10nError {
    /// A message does not follow the supported MessageFormat subset.
    #[error("message syntax: {0}")]
    Syntax(String),
    /// A translation's arguments or plural forms do not fit its source.
    #[error("translation does not match its source: {0}")]
    Mismatch(String),
    /// A catalog file is malformed.
    #[error("catalog: {0}")]
    Catalog(String),
    /// The language code is not one Lakko ships.
    #[error("unsupported language: {0}")]
    UnsupportedLanguage(String),
    /// A translation bundle is malformed or larger than allowed.
    #[error("bundle malformed: {0}")]
    BundleMalformed(&'static str),
    /// A translation bundle's signature does not verify.
    #[error("bundle signature invalid")]
    BundleSignature,
    /// A translation bundle is not newer than what this device already has.
    #[error("bundle version {got} not newer than {floor}")]
    BundleNotNewer {
        /// The version the bundle carries.
        got: u64,
        /// The version it had to exceed.
        floor: u64,
    },
    /// A translation bundle was published for a newer app release.
    #[error("bundle needs release {needed}, this build is {current}")]
    BundleTooNew {
        /// The release the bundle requires.
        needed: u64,
        /// This build's release.
        current: u64,
    },
}
