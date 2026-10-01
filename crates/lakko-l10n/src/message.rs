// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The message syntax of ADR-0061: a subset of ICU MessageFormat.
//!
//! Supported:
//!
//! - literal text;
//! - `{name}` — an argument;
//! - `{count, plural, one {# message} other {# messages}}` — plural
//!   selection over a whole count, with `=N` exact cases and `#` for the
//!   count inside a case;
//! - ICU apostrophe quoting: `''` is one apostrophe, and `'` quotes a
//!   literal `{`, `}` (or `#` inside a plural case) up to the next single
//!   `'`. Any other apostrophe is literal, so "Couldn't" needs no escaping.
//!
//! Not supported, and refused rather than half-rendered: `select`,
//! `selectordinal`, nested plurals, `offset:`, number / date styles.
//! Numbers and dates are formatted by the platform for the locale and passed
//! in as arguments.
//!
//! Text arguments are wrapped in Unicode isolates (FSI ... PDI) when
//! formatted, so a name written right to left cannot reorder the sentence it
//! is placed in, and vice versa (`docs/ux/localization.md` section 6).

use std::collections::{BTreeMap, BTreeSet};

use crate::error::L10nError;
use crate::plural::{Category, Plurals};

/// Longest message source accepted, in bytes. Chosen, not measured: well
/// above any UI sentence, low enough to bound a hostile bundle's work.
pub const MAX_MESSAGE_BYTES: usize = 4096;
const MAX_CASES: usize = 16;
const MAX_ARG_NAME: usize = 64;

/// FIRST STRONG ISOLATE: lays the following run out in its own direction.
const FSI: char = '\u{2068}';
/// POP DIRECTIONAL ISOLATE: ends the run opened by [`FSI`].
const PDI: char = '\u{2069}';

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Selector {
    Exact(u64),
    Category(Category),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Arg(String),
    /// `#` inside a plural case.
    Count,
    Plural {
        arg: String,
        cases: Vec<(Selector, Vec<Self>)>,
    },
}

/// How a message uses one of its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// Inserted as text.
    Text,
    /// Selects a plural case (and may also be inserted).
    Count,
}

/// An argument value passed to [`Message::format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg<'a> {
    /// Text, typically something a person wrote or a formatted date. Wrapped
    /// in a directional isolate when inserted.
    Text(&'a str),
    /// A count. `display` is the count already formatted by the platform for
    /// the locale (native digits, grouping); without it the count is written
    /// in ASCII digits.
    Count {
        /// The value plural selection uses.
        value: u64,
        /// How to write it.
        display: Option<&'a str>,
    },
}

/// A parsed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    parts: Vec<Part>,
}

impl Message {
    /// Parse a message source. Refuses anything outside the subset.
    pub fn parse(src: &str) -> Result<Self, L10nError> {
        if src.len() > MAX_MESSAGE_BYTES {
            return Err(L10nError::Syntax("message too long".into()));
        }
        let chars: Vec<char> = src.chars().collect();
        let mut p = Parser { s: &chars, pos: 0 };
        let parts = p.parts(false)?;
        Ok(Self { parts })
    }

    /// The arguments the message uses and how.
    #[must_use]
    pub fn args(&self) -> BTreeMap<String, ArgKind> {
        let mut out = BTreeMap::new();
        collect_args(&self.parts, &mut out);
        out
    }

    /// Whether the message has no arguments at all.
    #[must_use]
    pub fn is_plain(&self) -> bool {
        self.parts.iter().all(|p| matches!(p, Part::Text(_)))
    }

    /// The text of a message without arguments; `None` if it has any.
    #[must_use]
    pub fn plain_text(&self) -> Option<String> {
        if !self.is_plain() {
            return None;
        }
        Some(
            self.parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect(),
        )
    }

    /// Every plural category keyword the message's cases use.
    #[must_use]
    pub fn plural_categories(&self) -> BTreeSet<Category> {
        let mut out = BTreeSet::new();
        for part in &self.parts {
            if let Part::Plural { cases, .. } = part {
                for (sel, _) in cases {
                    if let Selector::Category(c) = sel {
                        out.insert(*c);
                    }
                }
            }
        }
        out
    }

    /// Check the message only uses plural categories the language has.
    pub fn check_categories(&self, plurals: &Plurals) -> Result<(), L10nError> {
        let known = plurals.categories();
        for c in self.plural_categories() {
            if !known.contains(&c) {
                return Err(L10nError::Mismatch(format!(
                    "plural category `{}` does not exist in this language",
                    c.keyword()
                )));
            }
        }
        Ok(())
    }

    /// Check a translation against its source: the same argument names, a
    /// plural only over an argument the source counts, and only categories
    /// the translation's language has.
    ///
    /// A translation may write a counted argument as plain text (Chinese has
    /// no plural forms), but may not drop or invent an argument.
    pub fn check_translation(
        &self,
        translation: &Self,
        plurals: &Plurals,
    ) -> Result<(), L10nError> {
        let src = self.args();
        let tr = translation.args();
        let src_names: BTreeSet<_> = src.keys().collect();
        let tr_names: BTreeSet<_> = tr.keys().collect();
        if src_names != tr_names {
            return Err(L10nError::Mismatch(format!(
                "arguments differ: source {src_names:?}, translation {tr_names:?}"
            )));
        }
        for (name, kind) in &tr {
            if *kind == ArgKind::Count && src.get(name) != Some(&ArgKind::Count) {
                return Err(L10nError::Mismatch(format!(
                    "`{name}` is not a count in the source"
                )));
            }
        }
        translation.check_categories(plurals)
    }

    /// Render the message. A missing argument is written as `{name}` so the
    /// defect is visible rather than a silently shortened sentence.
    #[must_use]
    pub fn format(&self, plurals: &Plurals, args: &[(&str, Arg<'_>)]) -> String {
        let mut out = String::new();
        render(&self.parts, plurals, args, None, &mut out);
        out
    }
}

fn collect_args(parts: &[Part], out: &mut BTreeMap<String, ArgKind>) {
    for part in parts {
        match part {
            Part::Arg(name) => {
                out.entry(name.clone()).or_insert(ArgKind::Text);
            },
            Part::Plural { arg, cases } => {
                out.insert(arg.clone(), ArgKind::Count);
                for (_, body) in cases {
                    collect_args(body, out);
                }
            },
            Part::Text(_) | Part::Count => {},
        }
    }
}

fn lookup<'a>(args: &'a [(&str, Arg<'a>)], name: &str) -> Option<Arg<'a>> {
    args.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
}

fn write_count(value: u64, display: Option<&str>, out: &mut String) {
    match display {
        Some(d) => out.push_str(d),
        None => out.push_str(&value.to_string()),
    }
}

fn render(
    parts: &[Part],
    plurals: &Plurals,
    args: &[(&str, Arg<'_>)],
    count: Option<(u64, Option<&str>)>,
    out: &mut String,
) {
    for part in parts {
        match part {
            Part::Text(t) => out.push_str(t),
            Part::Arg(name) => match lookup(args, name) {
                Some(Arg::Text(s)) => {
                    out.push(FSI);
                    out.push_str(s);
                    out.push(PDI);
                },
                Some(Arg::Count { value, display }) => write_count(value, display, out),
                None => {
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                },
            },
            Part::Count => match count {
                Some((value, display)) => write_count(value, display, out),
                None => out.push('#'),
            },
            Part::Plural { arg, cases } => {
                let (value, display) = match lookup(args, arg) {
                    Some(Arg::Count { value, display }) => (value, display),
                    Some(Arg::Text(s)) => match s.parse::<u64>() {
                        Ok(v) => (v, Some(s)),
                        Err(_) => (0, None),
                    },
                    None => (0, None),
                };
                let exact = cases.iter().find(|(sel, _)| *sel == Selector::Exact(value));
                let category = plurals.category(value);
                let chosen = exact
                    .or_else(|| {
                        cases
                            .iter()
                            .find(|(sel, _)| *sel == Selector::Category(category))
                    })
                    .or_else(|| {
                        cases
                            .iter()
                            .find(|(sel, _)| *sel == Selector::Category(Category::Other))
                    });
                if let Some((_, body)) = chosen {
                    render(body, plurals, args, Some((value, display)), out);
                }
            },
        }
    }
}

struct Parser<'a> {
    s: &'a [char],
    pos: usize,
}

fn syntax(msg: &str) -> L10nError {
    L10nError::Syntax(msg.to_owned())
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, want: char) -> Result<(), L10nError> {
        if self.bump() == Some(want) {
            Ok(())
        } else {
            Err(L10nError::Syntax(format!("expected `{want}`")))
        }
    }

    fn ident(&mut self) -> Result<String, L10nError> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                out.push(c);
                self.pos += 1;
                if out.len() > MAX_ARG_NAME {
                    return Err(syntax("argument name too long"));
                }
            } else {
                break;
            }
        }
        if out.is_empty() {
            return Err(syntax("expected a name"));
        }
        Ok(out)
    }

    fn parts(&mut self, in_plural: bool) -> Result<Vec<Part>, L10nError> {
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            let Some(c) = self.peek() else {
                if in_plural {
                    return Err(syntax("unclosed plural case"));
                }
                break;
            };
            match c {
                '{' => {
                    self.pos += 1;
                    if !text.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut text)));
                    }
                    parts.push(self.placeholder(in_plural)?);
                },
                '}' => {
                    if in_plural {
                        break;
                    }
                    return Err(syntax("unmatched `}`"));
                },
                '#' if in_plural => {
                    self.pos += 1;
                    if !text.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut text)));
                    }
                    parts.push(Part::Count);
                },
                '\'' => {
                    self.pos += 1;
                    match self.peek() {
                        Some('\'') => {
                            self.pos += 1;
                            text.push('\'');
                        },
                        Some(n) if n == '{' || n == '}' || (in_plural && n == '#') => {
                            self.quoted(&mut text)?;
                        },
                        _ => text.push('\''),
                    }
                },
                _ => {
                    self.pos += 1;
                    text.push(c);
                },
            }
        }
        if !text.is_empty() {
            parts.push(Part::Text(text));
        }
        Ok(parts)
    }

    fn quoted(&mut self, text: &mut String) -> Result<(), L10nError> {
        loop {
            match self.bump() {
                None => return Err(syntax("unterminated quote")),
                Some('\'') => {
                    if self.peek() == Some('\'') {
                        self.pos += 1;
                        text.push('\'');
                    } else {
                        return Ok(());
                    }
                },
                Some(c) => text.push(c),
            }
        }
    }

    fn placeholder(&mut self, in_plural: bool) -> Result<Part, L10nError> {
        self.skip_ws();
        let name = self.ident()?;
        self.skip_ws();
        match self.bump() {
            Some('}') => Ok(Part::Arg(name)),
            Some(',') => {
                self.skip_ws();
                let kind = self.ident()?;
                if kind != "plural" {
                    return Err(L10nError::Syntax(format!(
                        "unsupported argument type `{kind}`"
                    )));
                }
                if in_plural {
                    return Err(syntax("nested plural"));
                }
                self.skip_ws();
                self.expect(',')?;
                self.cases(name)
            },
            _ => Err(syntax("expected `}` or `,` after an argument name")),
        }
    }

    fn cases(&mut self, arg: String) -> Result<Part, L10nError> {
        let mut cases: Vec<(Selector, Vec<Part>)> = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(syntax("unclosed plural")),
                Some('}') => {
                    self.pos += 1;
                    break;
                },
                Some('=') => {
                    self.pos += 1;
                    let digits = self.ident()?;
                    if !digits.chars().all(|c| c.is_ascii_digit()) {
                        return Err(syntax("`=` must be followed by a number"));
                    }
                    let n = digits
                        .parse::<u64>()
                        .map_err(|_| syntax("exact case out of range"))?;
                    cases.push((Selector::Exact(n), Vec::new()));
                },
                Some(_) => {
                    let kw = self.ident()?;
                    if kw == "offset" {
                        return Err(syntax("`offset:` is not supported"));
                    }
                    let c = Category::from_keyword(&kw)
                        .ok_or_else(|| L10nError::Syntax(format!("unknown plural case `{kw}`")))?;
                    cases.push((Selector::Category(c), Vec::new()));
                },
            }
            self.skip_ws();
            self.expect('{')?;
            let body = self.parts(true)?;
            self.expect('}')?;
            let len = cases.len();
            let sel = cases.last().map(|(s, _)| *s);
            if cases[..len - 1].iter().any(|(s, _)| Some(*s) == sel) {
                return Err(syntax("duplicate plural case"));
            }
            if let Some(last) = cases.last_mut() {
                last.1 = body;
            }
            if cases.len() > MAX_CASES {
                return Err(syntax("too many plural cases"));
            }
        }
        if !cases
            .iter()
            .any(|(s, _)| *s == Selector::Category(Category::Other))
        {
            return Err(syntax("a plural needs an `other` case"));
        }
        Ok(Part::Plural { arg, cases })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::language;

    fn plurals(code: &str) -> Plurals {
        Plurals::for_language(language(code).unwrap()).unwrap()
    }

    fn fmt(src: &str, code: &str, args: &[(&str, Arg<'_>)]) -> String {
        Message::parse(src).unwrap().format(&plurals(code), args)
    }

    fn count(n: u64) -> Arg<'static> {
        Arg::Count {
            value: n,
            display: None,
        }
    }

    const EN_MESSAGES: &str =
        "{count, plural, =0 {No messages} one {# message} other {# messages}}";

    #[test]
    fn plain_text_and_apostrophes() {
        let m = Message::parse("Couldn't load this language.").unwrap();
        assert_eq!(
            m.plain_text().as_deref(),
            Some("Couldn't load this language.")
        );
        assert_eq!(
            Message::parse("It''s '{literal}' braces")
                .unwrap()
                .plain_text()
                .as_deref(),
            Some("It's {literal} braces")
        );
        assert_eq!(
            Message::parse("").unwrap().plain_text().as_deref(),
            Some("")
        );
    }

    #[test]
    fn english_plural_with_exact_zero() {
        assert_eq!(
            fmt(EN_MESSAGES, "en", &[("count", count(0))]),
            "No messages"
        );
        assert_eq!(fmt(EN_MESSAGES, "en", &[("count", count(1))]), "1 message");
        assert_eq!(fmt(EN_MESSAGES, "en", &[("count", count(7))]), "7 messages");
    }

    #[test]
    fn russian_plural_uses_cldr_categories() {
        let src = "{n, plural, one {# сообщение} few {# сообщения} many {# сообщений} other {# сообщения}}";
        assert_eq!(fmt(src, "ru", &[("n", count(1))]), "1 сообщение");
        assert_eq!(fmt(src, "ru", &[("n", count(3))]), "3 сообщения");
        assert_eq!(fmt(src, "ru", &[("n", count(11))]), "11 сообщений");
        assert_eq!(fmt(src, "ru", &[("n", count(21))]), "21 сообщение");
    }

    #[test]
    fn display_carries_platform_digits() {
        let src = "{n, plural, one {# پیام} other {# پیام}}";
        let out = fmt(
            src,
            "fa",
            &[(
                "n",
                Arg::Count {
                    value: 12,
                    display: Some("۱۲"),
                },
            )],
        );
        assert_eq!(out, "۱۲ پیام");
    }

    #[test]
    fn text_arguments_are_isolated() {
        let out = fmt("{name} joined", "en", &[("name", Arg::Text("علی"))]);
        assert_eq!(out, "\u{2068}علی\u{2069} joined");
    }

    #[test]
    fn missing_argument_stays_visible() {
        assert_eq!(fmt("Hi {name}", "en", &[]), "Hi {name}");
    }

    #[test]
    fn refused_syntax() {
        for bad in [
            "{",
            "}",
            "{name",
            "{ }",
            "{n, select, a {x} other {y}}",
            "{n, plural, one {x}}",
            "{n, plural, one {x} one {y} other {z}}",
            "{n, plural, other {{m, plural, other {x}}}}",
            "{n, plural, offset:1 other {x}}",
            "{n, plural, lots {x} other {y}}",
            "{n, plural, =x {a} other {b}}",
            "'{unterminated",
        ] {
            assert!(Message::parse(bad).is_err(), "{bad}");
        }
        assert!(Message::parse(&"a".repeat(MAX_MESSAGE_BYTES + 1)).is_err());
    }

    #[test]
    fn args_and_kinds() {
        let m = Message::parse("{who} sent {n, plural, one {# photo} other {# photos}}").unwrap();
        let args = m.args();
        assert_eq!(args.get("who"), Some(&ArgKind::Text));
        assert_eq!(args.get("n"), Some(&ArgKind::Count));
    }

    #[test]
    fn translation_checks() {
        let src = Message::parse("{who} sent {n, plural, one {# photo} other {# photos}}").unwrap();
        // Chinese: the count as plain text is fine.
        let zh = Message::parse("{who} 发送了 {n} 张照片").unwrap();
        assert!(src.check_translation(&zh, &plurals("zh-Hans")).is_ok());
        // Dropping an argument is not.
        let dropped = Message::parse("发送了 {n} 张照片").unwrap();
        assert!(src
            .check_translation(&dropped, &plurals("zh-Hans"))
            .is_err());
        // Inventing one is not.
        let extra = Message::parse("{who} {x} {n}").unwrap();
        assert!(src.check_translation(&extra, &plurals("zh-Hans")).is_err());
        // A plural over an argument the source does not count is not.
        let wrong = Message::parse("{n} {who, plural, other {x}}").unwrap();
        assert!(src.check_translation(&wrong, &plurals("en")).is_err());
        // A category the language lacks is not.
        let few = Message::parse("{who} {n, plural, few {x} other {y}}").unwrap();
        assert!(src.check_translation(&few, &plurals("en")).is_err());
        assert!(src.check_translation(&few, &plurals("ru")).is_ok());
    }
}
