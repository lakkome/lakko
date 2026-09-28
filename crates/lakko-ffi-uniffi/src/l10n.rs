// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Interface localization (ADR-0058) — UniFFI surface.
//!
//! Mirror of `lakko-ffi-wasm/src/l10n.rs` over the same [`lakko_l10n`] core,
//! so iOS and Android pick the language, render plurals and accept a signed
//! translation update by one rule.
//!
//! Intended use, once per language switch:
//!
//! 1. `L10nSource(sourceJson)` — the English source compiled into the app;
//! 2. optionally `l10nAcceptBundle(...)` on a downloaded bundle;
//! 3. `L10nCatalog(source, languageCode, builtInJson, bundle)`;
//! 4. `plainStrings()` once, into a native dictionary; `format(...)` only for
//!    messages with arguments.

use std::collections::HashMap;
use std::sync::Arc;

use lakko_crypto::signing::VerifyingKey;
use lakko_l10n::{
    accept_bundle as inner_accept_bundle, resolve_language as inner_resolve_language,
    supported_languages as inner_supported_languages, AcceptancePolicy,
    AcceptedBundle as InnerAcceptedBundle, Arg, Catalog as InnerCatalog,
    Direction as InnerDirection, L10nError, SourceCatalog as InnerSourceCatalog,
};

use crate::FfiError;

impl From<L10nError> for FfiError {
    fn from(e: L10nError) -> Self {
        Self::L10n(e.to_string())
    }
}

/// Writing direction of a language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TextDirection {
    /// Left to right.
    Ltr,
    /// Right to left.
    Rtl,
}

impl From<InnerDirection> for TextDirection {
    fn from(d: InnerDirection) -> Self {
        match d {
            InnerDirection::Ltr => Self::Ltr,
            InnerDirection::Rtl => Self::Rtl,
        }
    }
}

/// One supported language, for the language list.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LanguageInfo {
    /// Lakko's code; what the client stores as the choice.
    pub code: String,
    /// The name in the language itself: the row title.
    pub own_name: String,
    /// The English name (the row subtitle comes from the catalog key
    /// `language.name.<code lowercased>`, not from this).
    pub english_name: String,
    /// Writing direction.
    pub direction: TextDirection,
}

/// Every supported language, English first.
#[uniffi::export]
#[must_use]
pub fn l10n_supported_languages() -> Vec<LanguageInfo> {
    inner_supported_languages()
        .iter()
        .map(|l| LanguageInfo {
            code: l.code.to_owned(),
            own_name: l.own_name.to_owned(),
            english_name: l.english_name.to_owned(),
            direction: l.direction.into(),
        })
        .collect()
}

/// Which language to show: the stored choice (`None` = "Same as system"),
/// else the first supported system preference, else English.
#[uniffi::export]
#[must_use]
pub fn l10n_resolve_language(selected: Option<String>, system_preferred: Vec<String>) -> String {
    inner_resolve_language(selected.as_deref(), &system_preferred)
        .code
        .to_owned()
}

/// The English source catalog, parsed once.
#[derive(Debug, uniffi::Object)]
pub struct L10nSource {
    inner: InnerSourceCatalog,
}

#[uniffi::export]
impl L10nSource {
    /// Parse the source catalog (`source.json` from `lakko-l10n compile`).
    #[uniffi::constructor]
    pub fn new(source_json: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            inner: InnerSourceCatalog::from_json(&source_json)?,
        }))
    }

    /// The catalog version compiled into this build; a bundle must exceed
    /// it.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.inner.version()
    }
}

/// A translation bundle that verified and passed the version rules.
#[derive(Debug, uniffi::Object)]
pub struct L10nBundle {
    inner: InnerAcceptedBundle,
}

#[uniffi::export]
impl L10nBundle {
    /// Store this as the last accepted version.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.inner.version()
    }

    /// When it was signed, for Settings > About.
    #[must_use]
    pub fn signed_at_millis(&self) -> i64 {
        self.inner.signed_at_millis()
    }
}

/// Verify a downloaded translation bundle against the pinned 32-byte
/// publisher key and the version rules. On any error the client keeps the
/// built-in translation, silently.
#[uniffi::export]
pub fn l10n_accept_bundle(
    bundle: Vec<u8>,
    verifying_key: Vec<u8>,
    current_release: u64,
    built_in_version: u64,
    last_accepted: Option<u64>,
) -> Result<Arc<L10nBundle>, FfiError> {
    let key = VerifyingKey::from_bytes(&verifying_key)
        .map_err(|_| FfiError::L10n("verifying key must be 32 bytes".into()))?;
    let inner = inner_accept_bundle(
        &bundle,
        &key,
        AcceptancePolicy {
            current_release,
            built_in_version,
            last_accepted,
        },
    )?;
    Ok(Arc::new(L10nBundle { inner }))
}

/// Everything the UI renders in one language.
#[derive(Debug, uniffi::Object)]
pub struct L10nCatalog {
    inner: InnerCatalog,
}

#[uniffi::export]
impl L10nCatalog {
    /// Build the catalog for `language`. `built_in` is the compiled
    /// translation shipped in the app (ignored for English); `bundle` an
    /// accepted update. A string that does not fit its source is skipped
    /// and English is used for it; an update never replaces a
    /// security-critical string.
    #[uniffi::constructor]
    pub fn new(
        source: Arc<L10nSource>,
        language: String,
        built_in: Option<Vec<u8>>,
        bundle: Option<Arc<L10nBundle>>,
    ) -> Result<Arc<Self>, FfiError> {
        let inner = InnerCatalog::new(
            &source.inner,
            &language,
            built_in.as_deref(),
            bundle.as_ref().map(|b| &b.inner),
        )?;
        Ok(Arc::new(Self { inner }))
    }

    /// The language code this catalog renders.
    #[must_use]
    pub fn language(&self) -> String {
        self.inner.language().code.to_owned()
    }

    /// Its writing direction.
    #[must_use]
    pub fn direction(&self) -> TextDirection {
        self.inner.direction().into()
    }

    /// Translated strings skipped because they did not fit their source or
    /// were not allowed to replace it. For diagnostics.
    #[must_use]
    pub fn rejected(&self) -> u64 {
        self.inner.rejected() as u64
    }

    /// Every message without arguments, rendered; load once per switch.
    #[must_use]
    pub fn plain_strings(&self) -> HashMap<String, String> {
        self.inner.plain_strings().into_iter().collect()
    }

    /// Render a message with arguments. `texts` are inserted as text (each
    /// isolated for bidirectional layout); `counts` select plural forms. A
    /// count's display form, if the platform formatted it for the locale,
    /// goes in `texts` under the same name. `None` for an unknown key.
    #[must_use]
    pub fn format(
        &self,
        key: String,
        texts: HashMap<String, String>,
        counts: HashMap<String, u64>,
    ) -> Option<String> {
        let mut args: Vec<(&str, Arg<'_>)> = Vec::with_capacity(texts.len() + counts.len());
        for (name, value) in &counts {
            args.push((
                name.as_str(),
                Arg::Count {
                    value: *value,
                    display: texts.get(name).map(String::as_str),
                },
            ));
        }
        for (name, text) in &texts {
            if !counts.contains_key(name) {
                args.push((name.as_str(), Arg::Text(text.as_str())));
            }
        }
        self.inner.format(&key, &args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"{"format":1,"version":2,"messages":{
        "t":{"text":"Language","comment":"c"},
        "n":{"text":"{n, plural, one {# chat} other {# chats}}","comment":"c"},
        "w":{"text":"Hi {name}, {n, plural, one {# new} other {# new}}","comment":"c"}}}"#;

    #[test]
    fn english_catalog_through_the_surface() {
        let source = L10nSource::new(SOURCE.as_bytes().to_vec()).unwrap();
        assert_eq!(source.version(), 2);
        let cat = L10nCatalog::new(source, "en".into(), None, None).unwrap();
        assert_eq!(cat.language(), "en");
        assert_eq!(cat.direction(), TextDirection::Ltr);
        assert_eq!(
            cat.plain_strings().get("t").map(String::as_str),
            Some("Language")
        );
        let out = cat
            .format(
                "w".into(),
                HashMap::from([
                    ("name".to_owned(), "Ann".to_owned()),
                    ("n".to_owned(), "1,000".to_owned()),
                ]),
                HashMap::from([("n".to_owned(), 1000)]),
            )
            .unwrap();
        assert_eq!(out, "Hi \u{2068}Ann\u{2069}, 1,000 new");
        assert_eq!(
            cat.format("missing".into(), HashMap::new(), HashMap::new()),
            None
        );
    }

    #[test]
    fn resolution_and_list() {
        assert_eq!(l10n_resolve_language(None, vec!["fa-IR".into()]), "fa");
        assert_eq!(l10n_resolve_language(Some("he".into()), vec![]), "he");
        let list = l10n_supported_languages();
        assert_eq!(list[0].code, "en");
        assert!(list
            .iter()
            .any(|l| l.code == "ar" && l.direction == TextDirection::Rtl));
    }

    #[test]
    fn a_bad_key_or_bundle_is_an_error() {
        assert!(l10n_accept_bundle(vec![0; 10], vec![0; 31], 1, 0, None).is_err());
        assert!(l10n_accept_bundle(vec![0; 10], vec![0; 32], 1, 0, None).is_err());
        let source = L10nSource::new(SOURCE.as_bytes().to_vec()).unwrap();
        assert!(L10nCatalog::new(source, "de".into(), None, None).is_err());
    }
}
