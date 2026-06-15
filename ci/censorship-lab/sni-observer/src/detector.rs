// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! GFW-style SNI string-match censor emulation (G5-7).
//!
//! The observer normally just RECORDS what it sees. This module turns it into a
//! (crude) adversary: given a blocklist, classify each observed connection the
//! way a string-matching censor (the GFW / TSPU SNI keyword filter) would —
//! exact domain, domain suffix, or raw keyword substring.
//!
//! The load-bearing case is [`Verdict::Allowed`] for a HIDDEN SNI
//! ([`Blocklist::classify`] of `None`): a string-match censor cannot block what
//! it cannot read. Lakko's anti-censorship transports (REALITY cover SNI, ECH)
//! keep the real inner SNI off the wire, so this emulated censor never matches
//! them — which is exactly what the lab's transport-matrix tests assert (a
//! naive TLS flow's real SNI is Blocked; the Lakko cover/ECH flow is Allowed).
//!
//! Pure `std`, no I/O — the caller reads the blocklist from env and feeds the
//! parsed SNI in.

/// One blocklist rule. GFW-class censors match SNI by exact domain, by suffix
/// (a domain and its subdomains), or by a raw keyword substring (the crudest,
/// highest-collateral-damage form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Match the SNI exactly (`example.com`).
    Exact(String),
    /// Match the domain itself AND any subdomain (`example.com` +
    /// `*.example.com`).
    Suffix(String),
    /// Match the keyword anywhere in the SNI (substring).
    Keyword(String),
}

impl Rule {
    /// A short stable description for logs / the JSONL `blocked_by` field.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Exact(d) => format!("exact:{d}"),
            Self::Suffix(d) => format!("suffix:{d}"),
            Self::Keyword(k) => format!("keyword:{k}"),
        }
    }

    /// Does this rule match `sni` (already lower-cased)?
    #[must_use]
    fn matches(&self, sni: &str) -> bool {
        match self {
            Self::Exact(d) => sni == d,
            Self::Suffix(d) => sni == d || sni.ends_with(&format!(".{d}")),
            Self::Keyword(k) => sni.contains(k.as_str()),
        }
    }
}

/// A parsed GFW-style blocklist.
#[derive(Clone, Debug, Default)]
pub struct Blocklist {
    rules: Vec<Rule>,
}

/// The emulated censor's verdict on an observed connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No rule matched, OR no SNI was visible (the evasion case — a
    /// string-match censor cannot block what it cannot read).
    Allowed,
    /// A rule matched the observed SNI; the censor would reset the connection.
    Blocked {
        /// The matched rule, [`Rule::describe`]-formatted.
        rule: String,
    },
}

impl Blocklist {
    /// Parse a comma-separated pattern list (env-friendly via
    /// `LAKKO_SNI_BLOCKLIST`):
    ///
    /// - `example.com`   -> [`Rule::Exact`]
    /// - `*.example.com` -> [`Rule::Suffix`] (the domain + every subdomain)
    /// - `~keyword`      -> [`Rule::Keyword`] (substring anywhere)
    ///
    /// Blank / whitespace-only entries are skipped. Patterns are lower-cased
    /// (a GFW-class filter matches case-insensitively). A bare `*.` / `~` with
    /// no domain is skipped.
    #[must_use]
    pub fn from_csv(s: &str) -> Self {
        let mut rules = Vec::new();
        for raw in s.split(',') {
            let entry = raw.trim().to_ascii_lowercase();
            if entry.is_empty() {
                continue;
            }
            if let Some(kw) = entry.strip_prefix('~') {
                if !kw.is_empty() {
                    rules.push(Rule::Keyword(kw.to_owned()));
                }
            } else if let Some(dom) = entry.strip_prefix("*.") {
                if !dom.is_empty() {
                    rules.push(Rule::Suffix(dom.to_owned()));
                }
            } else {
                rules.push(Rule::Exact(entry));
            }
        }
        Self { rules }
    }

    /// True when the blocklist has no rules (the censor blocks nothing).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Classify an observed SNI. `None` (the SNI was hidden — ECH / REALITY
    /// cover, or a non-TLS flow) is [`Verdict::Allowed`]: the evasion case.
    /// The first matching rule (in list order) wins.
    #[must_use]
    pub fn classify(&self, sni: Option<&str>) -> Verdict {
        let Some(sni) = sni else {
            return Verdict::Allowed;
        };
        let sni = sni.to_ascii_lowercase();
        for rule in &self.rules {
            if rule.matches(&sni) {
                return Verdict::Blocked {
                    rule: rule.describe(),
                };
            }
        }
        Verdict::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_rule_matches_only_the_exact_domain() {
        let bl = Blocklist::from_csv("blocked.example");
        assert_eq!(
            bl.classify(Some("blocked.example")),
            Verdict::Blocked {
                rule: "exact:blocked.example".to_owned()
            }
        );
        assert_eq!(bl.classify(Some("sub.blocked.example")), Verdict::Allowed);
        assert_eq!(bl.classify(Some("other.example")), Verdict::Allowed);
    }

    #[test]
    fn suffix_rule_matches_domain_and_subdomains_only() {
        let bl = Blocklist::from_csv("*.evil.example");
        assert!(matches!(
            bl.classify(Some("evil.example")),
            Verdict::Blocked { .. }
        ));
        assert!(matches!(
            bl.classify(Some("a.b.evil.example")),
            Verdict::Blocked { .. }
        ));
        // Anchored at the end — not a substring.
        assert_eq!(bl.classify(Some("notevil.example")), Verdict::Allowed);
        assert_eq!(
            bl.classify(Some("evil.example.attacker.net")),
            Verdict::Allowed
        );
    }

    #[test]
    fn keyword_rule_matches_substring_anywhere() {
        let bl = Blocklist::from_csv("~torproject");
        assert!(matches!(
            bl.classify(Some("bridges.torproject.org")),
            Verdict::Blocked { .. }
        ));
        assert_eq!(bl.classify(Some("example.com")), Verdict::Allowed);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let bl = Blocklist::from_csv("Blocked.Example");
        assert!(matches!(
            bl.classify(Some("BLOCKED.EXAMPLE")),
            Verdict::Blocked { .. }
        ));
    }

    #[test]
    fn hidden_sni_is_allowed_the_evasion_case() {
        // REALITY / ECH keep the real SNI off the wire -> None -> Allowed.
        let bl = Blocklist::from_csv("*.evil.example,~tor,blocked.example");
        assert_eq!(bl.classify(None), Verdict::Allowed);
    }

    #[test]
    fn empty_blocklist_allows_everything() {
        let bl = Blocklist::from_csv("   ,  , ");
        assert!(bl.is_empty());
        assert_eq!(bl.classify(Some("blocked.example")), Verdict::Allowed);
    }

    #[test]
    fn from_csv_parses_mixed_forms_and_skips_blanks() {
        let bl = Blocklist::from_csv(" blocked.example , *.evil.example ,~tor, , *. , ~ ");
        // 3 valid rules; the bare `*.` and `~` are skipped.
        assert!(matches!(
            bl.classify(Some("blocked.example")),
            Verdict::Blocked { .. }
        ));
        assert!(matches!(
            bl.classify(Some("x.evil.example")),
            Verdict::Blocked { .. }
        ));
        assert!(matches!(
            bl.classify(Some("a-tor-relay")),
            Verdict::Blocked { .. }
        ));
        assert_eq!(bl.classify(Some("safe.example")), Verdict::Allowed);
    }

    #[test]
    fn first_matching_rule_wins() {
        let bl = Blocklist::from_csv("~ample,exact.example");
        // "exact.example" matches the keyword "ample" first (list order).
        assert_eq!(
            bl.classify(Some("exact.example")),
            Verdict::Blocked {
                rule: "keyword:ample".to_owned()
            }
        );
    }
}
