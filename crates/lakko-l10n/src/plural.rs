// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! CLDR plural categories, through ICU4X's compiled data.
//!
//! Only whole counts are supported (message counts, members, minutes): the
//! message subset of ADR-0061 has no fractional plural argument.

use icu_locale_core::Locale;
use icu_plurals::{PluralCategory, PluralRules};

use crate::error::L10nError;
use crate::language::Language;

/// A CLDR plural category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// `zero`
    Zero,
    /// `one`
    One,
    /// `two`
    Two,
    /// `few`
    Few,
    /// `many`
    Many,
    /// `other` — every language has it, every plural message must carry it.
    Other,
}

impl Category {
    /// The keyword used in message syntax.
    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::Two => "two",
            Self::Few => "few",
            Self::Many => "many",
            Self::Other => "other",
        }
    }

    /// Parse a keyword; `None` for anything else.
    #[must_use]
    pub fn from_keyword(s: &str) -> Option<Self> {
        Some(match s {
            "zero" => Self::Zero,
            "one" => Self::One,
            "two" => Self::Two,
            "few" => Self::Few,
            "many" => Self::Many,
            "other" => Self::Other,
            _ => return None,
        })
    }

    const fn from_icu(c: PluralCategory) -> Self {
        match c {
            PluralCategory::Zero => Self::Zero,
            PluralCategory::One => Self::One,
            PluralCategory::Two => Self::Two,
            PluralCategory::Few => Self::Few,
            PluralCategory::Many => Self::Many,
            PluralCategory::Other => Self::Other,
        }
    }
}

/// The cardinal plural rules of one supported language.
pub struct Plurals {
    rules: PluralRules,
}

impl std::fmt::Debug for Plurals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plurals").finish_non_exhaustive()
    }
}

impl Plurals {
    /// The rules for a supported language.
    pub fn for_language(language: &Language) -> Result<Self, L10nError> {
        let locale: Locale = language
            .plural_locale
            .parse()
            .map_err(|_| L10nError::UnsupportedLanguage(language.code.to_owned()))?;
        let rules = PluralRules::try_new_cardinal(locale.into())
            .map_err(|_| L10nError::UnsupportedLanguage(language.code.to_owned()))?;
        Ok(Self { rules })
    }

    /// The category a count falls into.
    #[must_use]
    pub fn category(&self, n: u64) -> Category {
        Category::from_icu(self.rules.category_for(n))
    }

    /// Every category this language distinguishes.
    #[must_use]
    pub fn categories(&self) -> Vec<Category> {
        self.rules.categories().map(Category::from_icu).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::language;

    fn plurals(code: &str) -> Plurals {
        Plurals::for_language(language(code).unwrap()).unwrap()
    }

    #[test]
    fn every_supported_language_has_rules_with_other() {
        for l in crate::language::supported_languages() {
            let p = Plurals::for_language(l).unwrap();
            assert!(p.categories().contains(&Category::Other), "{}", l.code);
        }
    }

    #[test]
    fn russian_and_arabic_counts() {
        let ru = plurals("ru");
        assert_eq!(ru.category(1), Category::One);
        assert_eq!(ru.category(3), Category::Few);
        assert_eq!(ru.category(5), Category::Many);
        assert_eq!(ru.category(21), Category::One);
        let ar = plurals("ar");
        assert_eq!(ar.category(0), Category::Zero);
        assert_eq!(ar.category(2), Category::Two);
        assert_eq!(ar.category(11), Category::Many);
        assert_eq!(plurals("zh-Hans").categories(), vec![Category::Other]);
    }

    #[test]
    fn keywords_round_trip() {
        for c in [
            Category::Zero,
            Category::One,
            Category::Two,
            Category::Few,
            Category::Many,
            Category::Other,
        ] {
            assert_eq!(Category::from_keyword(c.keyword()), Some(c));
        }
        assert_eq!(Category::from_keyword("One"), None);
    }
}
