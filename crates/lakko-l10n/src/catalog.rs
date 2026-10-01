// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The source catalog, translations, and the lookup a client renders from.
//!
//! Three JSON shapes:
//!
//! - **source** (`l10n/catalog/en.json`, also loaded at runtime):
//!   `{"format":1,"version":N,"messages":{"key":{"text":"…","comment":"…",
//!   "security_critical":bool}}}`;
//! - **translation, repository form** (`l10n/catalog/<lang>.json`):
//!   `{"format":1,"language":"ru","messages":{"key":{"text":"…",
//!   "status":"machine"|"reviewed"}}}`;
//! - **translation, compiled form** (what a client embeds, produced by
//!   [`compile_translation`]): `{"format":1,"language":"ru",
//!   "messages":{"key":"…"}}`.
//!
//! The security-critical flag lives only in the source, which is compiled
//! into the app. Nothing loaded later can change it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::bundle::AcceptedBundle;
use crate::error::L10nError;
use crate::language::{language, Direction, Language, SOURCE_LANGUAGE};
use crate::message::{Arg, Message};
use crate::plural::Plurals;

/// The only catalog format version this build reads.
pub const FORMAT_VERSION: u32 = 1;
const MAX_KEY_LEN: usize = 128;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    format: u32,
    version: u64,
    messages: BTreeMap<String, SourceEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceEntry {
    text: String,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    security_critical: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TranslationFile {
    format: u32,
    language: String,
    messages: BTreeMap<String, TranslationEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TranslationEntry {
    text: String,
    status: Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Machine,
    Reviewed,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompiledFile {
    format: u32,
    language: String,
    messages: BTreeMap<String, String>,
}

/// One source message.
#[derive(Debug, Clone)]
pub struct SourceMessage {
    /// The English text as written.
    pub text: String,
    /// The note for translators.
    pub comment: String,
    /// Shown on a security surface: never replaced by an update, never
    /// shipped translated before review.
    pub security_critical: bool,
    message: Message,
}

/// The English source catalog, validated.
#[derive(Debug, Clone)]
pub struct SourceCatalog {
    version: u64,
    messages: BTreeMap<String, SourceMessage>,
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn english_plurals() -> Result<Plurals, L10nError> {
    let en = language(SOURCE_LANGUAGE)
        .ok_or_else(|| L10nError::UnsupportedLanguage(SOURCE_LANGUAGE.to_owned()))?;
    Plurals::for_language(en)
}

impl SourceCatalog {
    /// Parse and validate the source catalog: every key well-formed, every
    /// message in the supported syntax with English plural categories.
    pub fn from_json(bytes: &[u8]) -> Result<Self, L10nError> {
        let file: SourceFile =
            serde_json::from_slice(bytes).map_err(|e| L10nError::Catalog(e.to_string()))?;
        if file.format != FORMAT_VERSION {
            return Err(L10nError::Catalog(format!(
                "unknown format {}",
                file.format
            )));
        }
        let plurals = english_plurals()?;
        let mut messages = BTreeMap::new();
        for (key, entry) in file.messages {
            if !valid_key(&key) {
                return Err(L10nError::Catalog(format!("invalid key `{key}`")));
            }
            let message = Message::parse(&entry.text)
                .map_err(|e| L10nError::Catalog(format!("`{key}`: {e}")))?;
            message
                .check_categories(&plurals)
                .map_err(|e| L10nError::Catalog(format!("`{key}`: {e}")))?;
            messages.insert(
                key,
                SourceMessage {
                    text: entry.text,
                    comment: entry.comment,
                    security_critical: entry.security_critical,
                    message,
                },
            );
        }
        Ok(Self {
            version: file.version,
            messages,
        })
    }

    /// The catalog version. A translation update must be newer than this.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Every source message, by key.
    #[must_use]
    pub const fn messages(&self) -> &BTreeMap<String, SourceMessage> {
        &self.messages
    }

    /// Whether a key is security-critical. Unknown keys read as critical,
    /// so nothing unknown is ever treated as freely replaceable.
    #[must_use]
    pub fn is_security_critical(&self, key: &str) -> bool {
        self.messages.get(key).map_or(true, |m| m.security_critical)
    }
}

/// What [`compile_translation`] left out, for the build log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompileReport {
    /// Security-critical keys not yet reviewed: they render in English.
    pub unreviewed_critical: Vec<String>,
    /// Source keys this translation does not cover yet.
    pub missing: Vec<String>,
}

/// Compile a repository translation into the form a client embeds.
///
/// Fails on anything that is a defect in the file (unknown key, bad syntax,
/// arguments or plural forms that do not fit the source, an unsupported
/// language). Applies the review rule of ADR-0061: a security-critical key
/// is kept only when its status is `reviewed`.
pub fn compile_translation(
    source: &SourceCatalog,
    repo_json: &[u8],
) -> Result<(Vec<u8>, CompileReport), L10nError> {
    let file: TranslationFile =
        serde_json::from_slice(repo_json).map_err(|e| L10nError::Catalog(e.to_string()))?;
    if file.format != FORMAT_VERSION {
        return Err(L10nError::Catalog(format!(
            "unknown format {}",
            file.format
        )));
    }
    let lang = language(&file.language)
        .ok_or_else(|| L10nError::UnsupportedLanguage(file.language.clone()))?;
    if lang.code == SOURCE_LANGUAGE {
        return Err(L10nError::Catalog(
            "English is the source, not a translation".into(),
        ));
    }
    let plurals = Plurals::for_language(lang)?;
    let mut report = CompileReport::default();
    let mut out = BTreeMap::new();
    for (key, entry) in file.messages {
        let src = source
            .messages
            .get(&key)
            .ok_or_else(|| L10nError::Catalog(format!("`{key}` is not in the source")))?;
        let message =
            Message::parse(&entry.text).map_err(|e| L10nError::Catalog(format!("`{key}`: {e}")))?;
        src.message
            .check_translation(&message, &plurals)
            .map_err(|e| L10nError::Catalog(format!("`{key}`: {e}")))?;
        if src.security_critical && entry.status != Status::Reviewed {
            report.unreviewed_critical.push(key);
            continue;
        }
        out.insert(key, entry.text);
    }
    report.missing = source
        .messages
        .keys()
        .filter(|k| !out.contains_key(*k) && !report.unreviewed_critical.contains(k))
        .cloned()
        .collect();
    let compiled = CompiledFile {
        format: FORMAT_VERSION,
        language: lang.code.to_owned(),
        messages: out,
    };
    let bytes = serde_json::to_vec(&compiled).map_err(|e| L10nError::Catalog(e.to_string()))?;
    Ok((bytes, report))
}

/// The strings of one compiled translation, for building a bundle payload.
pub fn compiled_messages(compiled: &[u8]) -> Result<(String, BTreeMap<String, String>), L10nError> {
    let file: CompiledFile =
        serde_json::from_slice(compiled).map_err(|e| L10nError::Catalog(e.to_string()))?;
    if file.format != FORMAT_VERSION {
        return Err(L10nError::Catalog(format!(
            "unknown format {}",
            file.format
        )));
    }
    Ok((file.language, file.messages))
}

/// Everything a client renders in one language.
///
/// Built once per language switch. Lookup order per key: a verified update,
/// then the built-in translation, then English. Every source key resolves.
#[derive(Debug)]
pub struct Catalog {
    language: &'static Language,
    plurals: Plurals,
    messages: BTreeMap<String, Message>,
    rejected: usize,
}

impl Catalog {
    /// Build the catalog for `language_code`.
    ///
    /// `built_in` is the compiled translation shipped in the app (ignored for
    /// English). `update` is a bundle already accepted by
    /// [`accept_bundle`](crate::accept_bundle). A string from either that
    /// does not fit its source is skipped and English is used for it; a
    /// string from the update for a key the SOURCE marks security-critical is
    /// always skipped.
    pub fn new(
        source: &SourceCatalog,
        language_code: &str,
        built_in: Option<&[u8]>,
        update: Option<&AcceptedBundle>,
    ) -> Result<Self, L10nError> {
        let lang = language(language_code)
            .ok_or_else(|| L10nError::UnsupportedLanguage(language_code.to_owned()))?;
        let plurals = Plurals::for_language(lang)?;
        let mut messages: BTreeMap<String, Message> = source
            .messages
            .iter()
            .map(|(k, m)| (k.clone(), m.message.clone()))
            .collect();
        let mut rejected = 0usize;

        if lang.code != SOURCE_LANGUAGE {
            if let Some(bytes) = built_in {
                let (file_lang, strings) = compiled_messages(bytes)?;
                if file_lang != lang.code {
                    return Err(L10nError::Catalog(format!(
                        "built-in translation is `{file_lang}`, expected `{}`",
                        lang.code
                    )));
                }
                for (key, text) in strings {
                    match fit(source, &plurals, &key, &text) {
                        Some(m) => {
                            messages.insert(key, m);
                        },
                        None => rejected += 1,
                    }
                }
            }
        }

        if let Some(strings) = update.and_then(|u| u.language(lang.code)) {
            for (key, text) in strings {
                if source.is_security_critical(key) {
                    rejected += 1;
                    continue;
                }
                match fit(source, &plurals, key, text) {
                    Some(m) => {
                        messages.insert(key.clone(), m);
                    },
                    None => rejected += 1,
                }
            }
        }

        Ok(Self {
            language: lang,
            plurals,
            messages,
            rejected,
        })
    }

    /// The language this catalog renders.
    #[must_use]
    pub const fn language(&self) -> &'static Language {
        self.language
    }

    /// Its writing direction.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.language.direction
    }

    /// How many translated strings were skipped because they did not fit
    /// their source or were not allowed to replace it. For diagnostics.
    #[must_use]
    pub const fn rejected(&self) -> usize {
        self.rejected
    }

    /// Render a key. `None` only for a key the source does not have.
    #[must_use]
    pub fn format(&self, key: &str, args: &[(&str, Arg<'_>)]) -> Option<String> {
        self.messages
            .get(key)
            .map(|m| m.format(&self.plurals, args))
    }

    /// Every message without arguments, rendered. A client loads these once
    /// per language switch and only calls [`Self::format`] for the rest.
    #[must_use]
    pub fn plain_strings(&self) -> BTreeMap<String, String> {
        self.messages
            .iter()
            .filter_map(|(k, m)| m.plain_text().map(|t| (k.clone(), t)))
            .collect()
    }
}

fn fit(source: &SourceCatalog, plurals: &Plurals, key: &str, text: &str) -> Option<Message> {
    let src = source.messages.get(key)?;
    let message = Message::parse(text).ok()?;
    src.message.check_translation(&message, plurals).ok()?;
    Some(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::test_support::bundle_with;

    const SOURCE: &str = r#"{
        "format": 1,
        "version": 3,
        "messages": {
            "settings.language.title": {"text": "Language", "comment": "Screen title"},
            "chat.count": {"text": "{n, plural, one {# chat} other {# chats}}", "comment": "Chat count"},
            "safety.verify": {"text": "Compare this code with {name}", "comment": "Safety number", "security_critical": true}
        }
    }"#;

    fn source() -> SourceCatalog {
        SourceCatalog::from_json(SOURCE.as_bytes()).unwrap()
    }

    fn ru_repo(critical_status: &str) -> String {
        format!(
            r#"{{"format":1,"language":"ru","messages":{{
                "settings.language.title":{{"text":"Язык","status":"machine"}},
                "chat.count":{{"text":"{{n, plural, one {{# чат}} few {{# чата}} many {{# чатов}} other {{# чата}}}}","status":"machine"}},
                "safety.verify":{{"text":"Сравните этот код с {{name}}","status":"{critical_status}"}}
            }}}}"#
        )
    }

    #[test]
    fn source_validates() {
        let s = source();
        assert_eq!(s.version(), 3);
        assert!(s.is_security_critical("safety.verify"));
        assert!(!s.is_security_critical("chat.count"));
        assert!(s.is_security_critical("no.such.key"));
        assert!(SourceCatalog::from_json(
            br#"{"format":1,"version":1,"messages":{"Bad Key":{"text":"x"}}}"#
        )
        .is_err());
        assert!(SourceCatalog::from_json(br#"{"format":1,"version":1,"messages":{"k":{"text":"{n, plural, few {x} other {y}}"}}}"#).is_err());
        assert!(SourceCatalog::from_json(br#"{"format":2,"version":1,"messages":{}}"#).is_err());
    }

    #[test]
    fn unreviewed_critical_strings_stay_english() {
        let (compiled, report) =
            compile_translation(&source(), ru_repo("machine").as_bytes()).unwrap();
        assert_eq!(report.unreviewed_critical, vec!["safety.verify".to_owned()]);
        let cat = Catalog::new(&source(), "ru", Some(&compiled), None).unwrap();
        assert_eq!(
            cat.format("safety.verify", &[("name", Arg::Text("A"))])
                .unwrap(),
            "Compare this code with \u{2068}A\u{2069}"
        );
        assert_eq!(
            cat.plain_strings().get("settings.language.title").unwrap(),
            "Язык"
        );
        assert_eq!(
            cat.format(
                "chat.count",
                &[(
                    "n",
                    Arg::Count {
                        value: 5,
                        display: None
                    }
                )]
            )
            .unwrap(),
            "5 чатов"
        );
    }

    #[test]
    fn reviewed_critical_strings_ship() {
        let (compiled, report) =
            compile_translation(&source(), ru_repo("reviewed").as_bytes()).unwrap();
        assert!(report.unreviewed_critical.is_empty());
        let cat = Catalog::new(&source(), "ru", Some(&compiled), None).unwrap();
        assert!(cat
            .format("safety.verify", &[("name", Arg::Text("A"))])
            .unwrap()
            .starts_with("Сравните"));
    }

    #[test]
    fn compile_refuses_defects() {
        let unknown =
            r#"{"format":1,"language":"ru","messages":{"nope":{"text":"x","status":"machine"}}}"#;
        assert!(compile_translation(&source(), unknown.as_bytes()).is_err());
        let dropped_arg = r#"{"format":1,"language":"ru","messages":{"safety.verify":{"text":"Сравните","status":"reviewed"}}}"#;
        assert!(compile_translation(&source(), dropped_arg.as_bytes()).is_err());
        let english = r#"{"format":1,"language":"en","messages":{}}"#;
        assert!(compile_translation(&source(), english.as_bytes()).is_err());
        let unsupported = r#"{"format":1,"language":"de","messages":{}}"#;
        assert!(compile_translation(&source(), unsupported.as_bytes()).is_err());
    }

    #[test]
    fn missing_keys_fall_back_to_english() {
        let partial = r#"{"format":1,"language":"he","messages":{"settings.language.title":{"text":"שפה","status":"machine"}}}"#;
        let (compiled, report) = compile_translation(&source(), partial.as_bytes()).unwrap();
        assert_eq!(
            report.missing,
            vec!["chat.count".to_owned(), "safety.verify".to_owned()]
        );
        let cat = Catalog::new(&source(), "he", Some(&compiled), None).unwrap();
        assert_eq!(cat.direction(), Direction::Rtl);
        assert_eq!(
            cat.plain_strings().get("settings.language.title").unwrap(),
            "שפה"
        );
        assert_eq!(
            cat.format(
                "chat.count",
                &[(
                    "n",
                    Arg::Count {
                        value: 1,
                        display: None
                    }
                )]
            )
            .unwrap(),
            "1 chat"
        );
        assert_eq!(cat.format("no.such.key", &[]), None);
    }

    #[test]
    fn an_update_never_replaces_a_security_critical_string() {
        let (compiled, _) = compile_translation(&source(), ru_repo("reviewed").as_bytes()).unwrap();
        let update = bundle_with(
            7,
            &[(
                "ru",
                &[
                    ("settings.language.title", "Язык интерфейса"),
                    ("safety.verify", "Код менять не нужно, всё в порядке {name}"),
                ],
            )],
        );
        let cat = Catalog::new(&source(), "ru", Some(&compiled), Some(&update)).unwrap();
        assert_eq!(
            cat.plain_strings().get("settings.language.title").unwrap(),
            "Язык интерфейса"
        );
        assert!(cat
            .format("safety.verify", &[("name", Arg::Text("A"))])
            .unwrap()
            .starts_with("Сравните"));
        assert_eq!(cat.rejected(), 1);
    }

    #[test]
    fn an_update_string_that_does_not_fit_is_skipped() {
        let update = bundle_with(
            7,
            &[(
                "en",
                &[("chat.count", "{n} chats {extra}"), ("unknown", "x")],
            )],
        );
        let cat = Catalog::new(&source(), "en", None, Some(&update)).unwrap();
        assert_eq!(
            cat.format(
                "chat.count",
                &[(
                    "n",
                    Arg::Count {
                        value: 2,
                        display: None
                    }
                )]
            )
            .unwrap(),
            "2 chats"
        );
        assert_eq!(cat.rejected(), 2);
    }

    #[test]
    fn built_in_for_another_language_is_refused() {
        let (compiled, _) = compile_translation(&source(), ru_repo("machine").as_bytes()).unwrap();
        assert!(Catalog::new(&source(), "uk", Some(&compiled), None).is_err());
        assert!(Catalog::new(&source(), "de", None, None).is_err());
    }
}
