// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Find identifier-shaped values in log output (ADR-0078 section 11,
//! decision 5 half b).
//!
//! What counts as an identifier here is a value **shaped like** a 16- or
//! 32-byte binary id, whatever field it sits in:
//!
//! - hex: a run of 32 or more hex digits (16 bytes and up);
//! - base64 (standard or URL alphabet): a run of exactly 22 or 43 characters
//!   (16 or 32 bytes unpadded; `==` / `=` padding is allowed after it) that
//!   mixes upper case, lower case and digits, which a random id does with
//!   overwhelming probability and an English word or a module path does not;
//! - a UUID in its dashed form (an account id is a UUIDv7);
//! - a `Debug`-printed byte array of exactly 16 or 32 numbers.
//!
//! A shorter prefix of an id (the 8-character `account_pseudonym` the SFU
//! used to log) cannot be told from any other short token by its shape; the
//! source lint ([`crate::lint`]) is what catches those, by field name.
//!
//! A value that is legitimately in a log line and still has one of these
//! shapes (a fixed test vector, a public build hash) goes on the caller's
//! allow-list, which is reviewed like code.

/// One identifier-shaped value found in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Which shape matched.
    pub kind: FindingKind,
    /// The matched text.
    pub value: String,
}

/// The shape of a [`Finding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// 32 or more hex digits.
    Hex,
    /// 22 or 43 base64 characters.
    Base64,
    /// A dashed UUID.
    Uuid,
    /// A printed array of 16 or 32 small integers.
    ByteArray,
}

const fn is_b64_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_')
}

fn looks_like_uuid(run: &str) -> bool {
    let bytes = run.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, b)| {
        if matches!(i, 8 | 13 | 18 | 23) {
            *b == b'-'
        } else {
            b.is_ascii_hexdigit()
        }
    })
}

fn looks_like_base64_id(run: &str) -> bool {
    if run.len() != 22 && run.len() != 43 {
        return false;
    }
    let upper = run.chars().any(|c| c.is_ascii_uppercase());
    let lower = run.chars().any(|c| c.is_ascii_lowercase());
    let digit = run.chars().any(|c| c.is_ascii_digit());
    upper && lower && digit
}

/// Every hex run of 32 or more digits inside `run`. A run of the base64
/// alphabet can hold a hex id between other characters (`id_0123...`).
fn hex_runs(run: &str, out: &mut Vec<Finding>) {
    let mut start: Option<usize> = None;
    let chars: Vec<(usize, char)> = run.char_indices().collect();
    let flush = |from: usize, to: usize, out: &mut Vec<Finding>| {
        if to - from >= 32 {
            out.push(Finding {
                kind: FindingKind::Hex,
                value: run[from..to].to_string(),
            });
        }
    };
    for &(i, c) in &chars {
        if c.is_ascii_hexdigit() {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            flush(s, i, out);
        }
    }
    if let Some(s) = start {
        flush(s, run.len(), out);
    }
}

/// Byte arrays as `Debug` prints them: `[1, 2, 3]`, exactly 16 or 32
/// integers each at most 255.
fn byte_arrays(text: &str, out: &mut Vec<Finding>) {
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            break;
        };
        let inner = &after[..close];
        let items: Vec<&str> = inner.split(',').map(str::trim).collect();
        let all_bytes = !inner.trim().is_empty()
            && items
                .iter()
                .all(|s| !s.is_empty() && s.parse::<u16>().is_ok_and(|n| n <= 255));
        if all_bytes && (items.len() == 16 || items.len() == 32) {
            out.push(Finding {
                kind: FindingKind::ByteArray,
                value: format!("[{inner}]"),
            });
        }
        rest = &after[close + 1..];
    }
}

/// Every identifier-shaped value in `text` that is not in `allow`.
#[must_use]
pub fn find_identifiers(text: &str, allow: &[&str]) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut run_start: Option<usize> = None;
    let mut runs: Vec<&str> = Vec::new();
    for (i, c) in text.char_indices() {
        if is_b64_char(c) {
            if run_start.is_none() {
                run_start = Some(i);
            }
        } else if let Some(s) = run_start.take() {
            runs.push(&text[s..i]);
        }
    }
    if let Some(s) = run_start {
        runs.push(&text[s..]);
    }
    for run in runs {
        if looks_like_uuid(run) {
            found.push(Finding {
                kind: FindingKind::Uuid,
                value: run.to_string(),
            });
            continue;
        }
        if looks_like_base64_id(run) {
            found.push(Finding {
                kind: FindingKind::Base64,
                value: run.to_string(),
            });
            continue;
        }
        hex_runs(run, &mut found);
    }
    byte_arrays(text, &mut found);
    found.retain(|f| !allow.iter().any(|a| f.value.contains(a)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<FindingKind> {
        find_identifiers(text, &[])
            .into_iter()
            .map(|f| f.kind)
            .collect()
    }

    #[test]
    fn hex_ids_of_sixteen_and_thirty_two_bytes_are_found() {
        assert_eq!(
            kinds("group=00112233445566778899aabbccddeeff"),
            [FindingKind::Hex]
        );
        assert_eq!(
            kinds(&format!("x {} y", "ab".repeat(32))),
            [FindingKind::Hex]
        );
        // Fifteen bytes is not.
        assert!(kinds(&"ab".repeat(15)).is_empty());
    }

    #[test]
    fn base64_ids_are_found_in_both_alphabets() {
        assert_eq!(kinds("id=Aq3_kZ9-Lm2xPq7Rt0Vw1A"), [FindingKind::Base64]);
        assert_eq!(kinds("id=Aq3/kZ9+Lm2xPq7Rt0Vw1A=="), [FindingKind::Base64]);
        assert_eq!(
            kinds("k=Q2xhdWRlIGlzIGEgbW9kZWwgYnkgQW50aHJvcGljLg0"),
            [FindingKind::Base64]
        );
    }

    #[test]
    fn words_and_module_paths_are_not_ids() {
        assert!(kinds("lakko_anon_token_server::gate").is_empty());
        assert!(kinds("invite already consumed by this joiner").is_empty());
        assert!(kinds("mailbox_reach_registration_ok").is_empty());
        assert!(kinds("rows=3 bytes=1073 used_count=2").is_empty());
    }

    #[test]
    fn uuids_and_byte_arrays_are_found() {
        assert_eq!(
            kinds("acct 0190a4b2-7c1e-7a3b-9f00-1234567890ab"),
            [FindingKind::Uuid]
        );
        let sixteen = (0..16)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(kinds(&format!("v=[{sixteen}]")), [FindingKind::ByteArray]);
        assert!(kinds("v=[1, 2, 3]").is_empty());
    }

    #[test]
    fn the_allow_list_suppresses_exactly_its_values() {
        let id = "00112233445566778899aabbccddeeff";
        assert!(find_identifiers(id, &[id]).is_empty());
        assert_eq!(find_identifiers(id, &["ffff"]).len(), 1);
    }
}
