// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The languages Lakko ships and the rule that picks one.
//!
//! The table is the single list of supported languages for every client
//! (`docs/ux/localization.md` section 2). Adding a language is a row here
//! plus a catalog file; no screen changes.

/// The language the source catalog is written in and every key falls back
/// to.
pub const SOURCE_LANGUAGE: &str = "en";

/// Which way a language is written. Drives `dir` on the web and the layout
/// direction on iOS and Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Left to right.
    Ltr,
    /// Right to left (Arabic, Persian, Hebrew).
    Rtl,
}

/// One supported language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// Lakko's code for it; the catalog file name and the stored choice.
    pub code: &'static str,
    /// The language's name in itself: the row title of the language list,
    /// which never changes with the UI language.
    pub own_name: &'static str,
    /// Its name in English, for logs and the source catalog comments.
    pub english_name: &'static str,
    /// Writing direction.
    pub direction: Direction,
    /// The ISO 15924 script the language is written in here. A system tag
    /// that names a different script does not match (`zh-Hant` is not
    /// `zh-Hans`).
    pub script: &'static str,
    /// The CLDR locale whose plural rules apply.
    pub(crate) plural_locale: &'static str,
}

const LANGUAGES: [Language; 11] = [
    lang("en", "English", "English", Direction::Ltr, "Latn", "en"),
    lang("ar", "العربية", "Arabic", Direction::Rtl, "Arab", "ar"),
    lang("es", "Español", "Spanish", Direction::Ltr, "Latn", "es"),
    lang("fa", "فارسی", "Persian", Direction::Rtl, "Arab", "fa"),
    lang("he", "עברית", "Hebrew", Direction::Rtl, "Hebr", "he"),
    lang("hi", "हिन्दी", "Hindi", Direction::Ltr, "Deva", "hi"),
    // CLDR `pt` is Brazilian Portuguese; `pt-PT` has different plural rules.
    lang(
        "pt-BR",
        "Português (Brasil)",
        "Portuguese (Brazil)",
        Direction::Ltr,
        "Latn",
        "pt",
    ),
    lang("ru", "Русский", "Russian", Direction::Ltr, "Cyrl", "ru"),
    lang("tr", "Türkçe", "Turkish", Direction::Ltr, "Latn", "tr"),
    lang(
        "uk",
        "Українська",
        "Ukrainian",
        Direction::Ltr,
        "Cyrl",
        "uk",
    ),
    lang(
        "zh-Hans",
        "简体中文",
        "Chinese (Simplified)",
        Direction::Ltr,
        "Hans",
        "zh",
    ),
];

const fn lang(
    code: &'static str,
    own_name: &'static str,
    english_name: &'static str,
    direction: Direction,
    script: &'static str,
    plural_locale: &'static str,
) -> Language {
    Language {
        code,
        own_name,
        english_name,
        direction,
        script,
        plural_locale,
    }
}

/// Every supported language, English first, then in code order.
#[must_use]
pub const fn supported_languages() -> &'static [Language] {
    &LANGUAGES
}

/// The supported language with exactly this Lakko code.
#[must_use]
pub fn language(code: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|l| l.code == code)
}

/// The English entry; the one language that always exists.
#[must_use]
pub const fn source_language() -> &'static Language {
    &LANGUAGES[0]
}

/// Map a platform locale tag (BCP 47, `en-US`, `zh_Hant_TW`, `iw`) to a
/// supported language, or `None`.
///
/// The rule of the UX spec: match by language and script, then by language
/// alone — but a tag that names (or implies) a different script never
/// matches, so traditional Chinese does not silently become simplified.
#[must_use]
pub fn match_tag(tag: &str) -> Option<&'static Language> {
    let normalised = tag.trim().replace('_', "-");
    let mut parts = normalised.split('-').filter(|p| !p.is_empty());
    let primary = parts.next()?.to_ascii_lowercase();
    let mut script: Option<String> = None;
    let mut region: Option<String> = None;
    for part in parts {
        if part.len() == 4 && part.chars().all(|c| c.is_ascii_alphabetic()) && script.is_none() {
            script = Some(title_case(part));
        } else if region.is_none()
            && ((part.len() == 2 && part.chars().all(|c| c.is_ascii_alphabetic()))
                || (part.len() == 3 && part.chars().all(|c| c.is_ascii_digit())))
        {
            region = Some(part.to_ascii_uppercase());
        }
    }
    // Deprecated ISO 639 code still reported by some Java and Android stacks.
    let primary = match primary.as_str() {
        "iw" => "he".to_owned(),
        other => other.to_owned(),
    };
    let candidate = match primary.as_str() {
        "zh" => {
            let implied = match region.as_deref() {
                Some("TW" | "HK" | "MO") => "Hant",
                _ => "Hans",
            };
            let effective = script.clone().unwrap_or_else(|| implied.to_owned());
            if effective == "Hans" {
                language("zh-Hans")
            } else {
                None
            }
        },
        "pt" => language("pt-BR"),
        other => LANGUAGES
            .iter()
            .find(|l| !l.code.contains('-') && l.code == other),
    }?;
    match script {
        Some(s) if s != candidate.script => None,
        _ => Some(candidate),
    }
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        if i == 0 {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// Which language to show.
///
/// `selected` is the person's choice on this device (`None` = "Same as
/// system"); `system_preferred` is the platform's ordered preference list.
/// The first of these that maps to a supported language wins; otherwise
/// English. Never fails: an unknown stored choice is treated as "Same as
/// system" rather than as an error, since the choice may have been stored by
/// a build that shipped a language this one does not.
#[must_use]
pub fn resolve_language(selected: Option<&str>, system_preferred: &[String]) -> &'static Language {
    if let Some(chosen) = selected.and_then(|s| language(s).or_else(|| match_tag(s))) {
        return chosen;
    }
    system_preferred
        .iter()
        .find_map(|tag| match_tag(tag))
        .unwrap_or_else(source_language)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn english_is_first_and_the_source() {
        assert_eq!(supported_languages()[0].code, SOURCE_LANGUAGE);
        assert_eq!(source_language().code, "en");
    }

    #[test]
    fn codes_are_unique() {
        let mut codes: Vec<_> = supported_languages().iter().map(|l| l.code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), supported_languages().len());
    }

    #[test]
    fn exactly_the_three_rtl_languages_are_rtl() {
        let rtl: Vec<_> = supported_languages()
            .iter()
            .filter(|l| l.direction == Direction::Rtl)
            .map(|l| l.code)
            .collect();
        assert_eq!(rtl, ["ar", "fa", "he"]);
    }

    #[test]
    fn system_tags_map_by_language() {
        assert_eq!(match_tag("en-US").map(|l| l.code), Some("en"));
        assert_eq!(match_tag("ru_RU").map(|l| l.code), Some("ru"));
        assert_eq!(match_tag("fa-IR").map(|l| l.code), Some("fa"));
        assert_eq!(match_tag("fa-AF").map(|l| l.code), Some("fa"));
        assert_eq!(match_tag("ES-mx").map(|l| l.code), Some("es"));
        assert_eq!(match_tag("iw-IL").map(|l| l.code), Some("he"));
        assert_eq!(match_tag("pt-PT").map(|l| l.code), Some("pt-BR"));
        assert_eq!(match_tag("pt").map(|l| l.code), Some("pt-BR"));
    }

    #[test]
    fn chinese_script_decides() {
        assert_eq!(match_tag("zh").map(|l| l.code), Some("zh-Hans"));
        assert_eq!(match_tag("zh-CN").map(|l| l.code), Some("zh-Hans"));
        assert_eq!(match_tag("zh-Hans-SG").map(|l| l.code), Some("zh-Hans"));
        assert_eq!(match_tag("zh_Hans_CN").map(|l| l.code), Some("zh-Hans"));
        assert_eq!(match_tag("zh-TW"), None);
        assert_eq!(match_tag("zh-HK"), None);
        assert_eq!(match_tag("zh-Hant"), None);
        // An explicit script beats the region's implication.
        assert_eq!(match_tag("zh-Hans-TW").map(|l| l.code), Some("zh-Hans"));
    }

    #[test]
    fn a_different_explicit_script_never_matches() {
        assert_eq!(match_tag("ru-Latn"), None);
        assert_eq!(match_tag("en-Latn-US").map(|l| l.code), Some("en"));
    }

    #[test]
    fn unsupported_and_garbage_tags_do_not_match() {
        for t in ["de-DE", "ja", "", "-", "x", "zh-Hant-TW", "sr-Cyrl"] {
            assert_eq!(match_tag(t), None, "{t}");
        }
    }

    #[test]
    fn resolution_order() {
        // The choice wins.
        assert_eq!(resolve_language(Some("he"), &tags(&["ru-RU"])).code, "he");
        // "Same as system": the first supported preference.
        assert_eq!(
            resolve_language(None, &tags(&["de-DE", "uk-UA", "ru-RU"])).code,
            "uk"
        );
        // Nothing supported: English.
        assert_eq!(resolve_language(None, &tags(&["de", "ja"])).code, "en");
        assert_eq!(resolve_language(None, &[]).code, "en");
        // A stored choice this build does not know falls back to the system.
        assert_eq!(resolve_language(Some("ja"), &tags(&["tr-TR"])).code, "tr");
    }
}
