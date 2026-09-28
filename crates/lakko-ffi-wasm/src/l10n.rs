// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Interface localization (ADR-0058) — wasm-bindgen surface.
//!
//! Mirror of `lakko-ffi-uniffi/src/l10n.rs` over the same [`lakko_l10n`]
//! core, minus the signed translation bundle: the web client's code already
//! comes from the server, so a separate signed channel would add nothing
//! (ADR-0058 section 5). A translation fix ships with the next web deploy.
//!
//! Errors are thrown as `Error("l10n: <reason>")`; the client falls back to
//! English and never shows the message to a person.

use js_sys::{Array, Object, Reflect};
use lakko_l10n::{
    resolve_language as inner_resolve_language, supported_languages as inner_supported_languages,
    Arg, Catalog as InnerCatalog, Direction, SourceCatalog as InnerSourceCatalog,
};
use wasm_bindgen::prelude::*;

fn coded(msg: &str) -> JsError {
    JsError::new(&format!("l10n: {msg}"))
}

/// One supported language, for the language list.
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct LanguageInfo {
    code: String,
    own_name: String,
    english_name: String,
    rtl: bool,
}

#[wasm_bindgen]
impl LanguageInfo {
    /// Lakko's code; what the client stores as the choice.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn code(&self) -> String {
        self.code.clone()
    }

    /// The name in the language itself: the row title.
    #[wasm_bindgen(getter, js_name = ownName)]
    #[must_use]
    pub fn own_name(&self) -> String {
        self.own_name.clone()
    }

    /// The English name (the row subtitle comes from the catalog key
    /// `language.name.<code lowercased>`).
    #[wasm_bindgen(getter, js_name = englishName)]
    #[must_use]
    pub fn english_name(&self) -> String {
        self.english_name.clone()
    }

    /// Whether the language is written right to left.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn rtl(&self) -> bool {
        self.rtl
    }
}

/// Every supported language, English first.
#[wasm_bindgen(js_name = l10nSupportedLanguages)]
#[must_use]
pub fn l10n_supported_languages() -> Vec<LanguageInfo> {
    inner_supported_languages()
        .iter()
        .map(|l| LanguageInfo {
            code: l.code.to_owned(),
            own_name: l.own_name.to_owned(),
            english_name: l.english_name.to_owned(),
            rtl: l.direction == Direction::Rtl,
        })
        .collect()
}

/// Which language to show: the stored choice (`undefined` = "Same as
/// system"), else the first supported entry of `navigator.languages`, else
/// English.
#[wasm_bindgen(js_name = l10nResolveLanguage)]
#[must_use]
pub fn l10n_resolve_language(selected: Option<String>, system_preferred: Vec<String>) -> String {
    inner_resolve_language(selected.as_deref(), &system_preferred)
        .code
        .to_owned()
}

/// The English source catalog, parsed once.
#[wasm_bindgen]
#[derive(Debug)]
pub struct L10nSource {
    inner: InnerSourceCatalog,
}

#[wasm_bindgen]
impl L10nSource {
    /// Parse the source catalog (`source.json` from `lakko-l10n compile`).
    #[wasm_bindgen(constructor)]
    pub fn new(source_json: &[u8]) -> Result<L10nSource, JsError> {
        InnerSourceCatalog::from_json(source_json)
            .map(|inner| Self { inner })
            .map_err(|e| coded(&e.to_string()))
    }

    /// The catalog version compiled into this build.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn version(&self) -> u64 {
        self.inner.version()
    }
}

/// Everything the UI renders in one language.
#[wasm_bindgen]
#[derive(Debug)]
pub struct L10nCatalog {
    inner: InnerCatalog,
}

fn read_count(value: &JsValue) -> Option<u64> {
    let f = value.as_f64()?;
    // Whole, non-negative, exactly representable: anything else is a caller
    // bug, and selecting a plural form for it would be a guess.
    if f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= 9_007_199_254_740_991.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(f as u64)
    } else {
        None
    }
}

fn entries(obj: &Object) -> Vec<(String, JsValue)> {
    Object::entries(obj)
        .iter()
        .filter_map(|pair| {
            let pair = Array::from(&pair);
            pair.get(0).as_string().map(|k| (k, pair.get(1)))
        })
        .collect()
}

#[wasm_bindgen]
impl L10nCatalog {
    /// Build the catalog for `language`; `built_in` is the compiled
    /// translation (ignored for English).
    #[wasm_bindgen(constructor)]
    pub fn new(
        source: &L10nSource,
        language: &str,
        built_in: Option<Vec<u8>>,
    ) -> Result<L10nCatalog, JsError> {
        InnerCatalog::new(&source.inner, language, built_in.as_deref(), None)
            .map(|inner| Self { inner })
            .map_err(|e| coded(&e.to_string()))
    }

    /// The language code this catalog renders.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn language(&self) -> String {
        self.inner.language().code.to_owned()
    }

    /// Whether it is written right to left (sets `dir="rtl"`).
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn rtl(&self) -> bool {
        self.inner.direction() == Direction::Rtl
    }

    /// Every message without arguments, rendered, as a plain object keyed by
    /// message key. Load once per language switch.
    #[wasm_bindgen(js_name = plainStrings)]
    pub fn plain_strings(&self) -> Result<Object, JsError> {
        let out = Object::new();
        for (k, v) in self.inner.plain_strings() {
            Reflect::set(&out, &JsValue::from_str(&k), &JsValue::from_str(&v))
                .map_err(|_| coded("cannot build the strings object"))?;
        }
        Ok(out)
    }

    /// Render a message with arguments. `texts` maps names to strings
    /// (inserted with bidirectional isolation); `counts` maps names to whole
    /// numbers that select plural forms. A count's display form, formatted by
    /// `Intl.NumberFormat` for the locale, goes in `texts` under the same
    /// name. `undefined` for an unknown key.
    #[must_use]
    pub fn format(&self, key: &str, texts: &Object, counts: &Object) -> Option<String> {
        let text_entries: Vec<(String, String)> = entries(texts)
            .into_iter()
            .filter_map(|(k, v)| v.as_string().map(|s| (k, s)))
            .collect();
        let count_entries: Vec<(String, u64)> = entries(counts)
            .into_iter()
            .filter_map(|(k, v)| read_count(&v).map(|n| (k, n)))
            .collect();
        let mut args: Vec<(&str, Arg<'_>)> = Vec::new();
        for (name, value) in &count_entries {
            let display = text_entries
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, s)| s.as_str());
            args.push((
                name.as_str(),
                Arg::Count {
                    value: *value,
                    display,
                },
            ));
        }
        for (name, text) in &text_entries {
            if !count_entries.iter().any(|(n, _)| n == name) {
                args.push((name.as_str(), Arg::Text(text.as_str())));
            }
        }
        self.inner.format(key, &args)
    }
}
