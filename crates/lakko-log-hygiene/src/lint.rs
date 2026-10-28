// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The source lint over `tracing` macros (ADR-0078 section 11, decision 5
//! half a).
//!
//! [`lint_source`] reads Rust source text and reports every `tracing` macro
//! at info level or above (`info!`, `warn!`, `error!`, their `_span`
//! variants, and `event!` at `Level::INFO` / `WARN` / `ERROR`) that names a
//! denied identifier:
//!
//! - a field whose name is denied (`account_id = %id`, `%call_id`, `?device`);
//! - a field value or a format argument whose expression mentions a denied
//!   name (`id = %hex::encode(group_id)`, `"joined {}", call_id`), unless the
//!   expression ends in a scalar derived from it (`peers.len()`,
//!   `tokens.is_empty()`);
//! - an inline capture in the format string (`"joined {call_id}"`).
//!
//! A name is denied when one of its `_`-separated components is a denied
//! word ([`DENIED_COMPONENTS`]): `account`, `identity`, `pubkey`, `pk`,
//! `journal`, `mailbox`, `call`, `room`, `channel`, `gid`, `pseudonym`,
//! `token`, `blob`, `device`, `peer`, `owner`, `issuer`, `uploader`,
//! `username`, `handle`, `email`, `address`, `sub`, `prefix`; or when it is a
//! group id (`group` alone, or `group` with `id` / `hex` / `b64`). A name
//! whose last component counts something (`peer_count`, `token_total`) is
//! not denied. [`ALLOWED_NAMES`] lists the names reviewed and kept.
//!
//! The lint is lexical, not a Rust parser: it skips comments, string and
//! character literals while looking for macros, then splits the macro's
//! arguments at top-level commas. It errs towards reporting; a reported
//! line that is not an identifier is either renamed or added to
//! [`ALLOWED_NAMES`] in review.

/// Name components that mark an identifier.
pub const DENIED_COMPONENTS: &[&str] = &[
    "account",
    "accounts",
    "identity",
    "pubkey",
    "pk",
    "journal",
    "mailbox",
    "call",
    "room",
    "channel",
    "gid",
    "pseudonym",
    "token",
    "blob",
    "device",
    "peer",
    "owner",
    "issuer",
    "uploader",
    "username",
    "handle",
    "email",
    "address",
    "sub",
    "prefix",
];

/// A last component that makes a name a count, not an identifier.
const COUNT_SUFFIXES: &[&str] = &[
    "count", "counts", "len", "total", "n", "size", "bytes", "kind", "kinds", "class", "classes",
    "ms", "secs", "limit", "max", "min", "context", "contexts", "code", "codes", "state", "status",
    "reason", "error", "outcome", "mode", "ok", "result",
];

/// Names reviewed and kept although a component matches: each is a count,
/// a category, a configuration value or a public constant, never an id.
pub const ALLOWED_NAMES: &[&str] = &[
    // Server-side configuration and bind addresses.
    "bind",
    "bind_address",
    "internal_address",
    "media_address",
    "advertise_address",
    // Anonymous-token context names (a category: `discovery-lookup`, ...).
    "token_context",
    // The SFU signaling frame kind.
    "frame_kind",
    // The account key generation a session token carries (ADR-0073): a
    // small counter, the same for every account that never reset.
    "token_generation",
    "key_generation",
    // The call-JWT signing key's variable; what is logged is its public
    // key id.
    "call_jwt_signing",
    // The storage service's blob directory (configuration).
    "blob_root",
];

/// The redaction a service may log in place of an id: `peer.log_tag()`
/// (the SFU's per-process keyed tag of a connection id, which nothing
/// outside the running process can map back). An expression containing it
/// is never reported, and neither is the field it is assigned to.
pub const REDACTIONS: &[&str] = &[".log_tag()"];

/// One reported macro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// 1-based line of the macro name.
    pub line: usize,
    /// The macro, as written (`info`, `warn_span`, ...).
    pub macro_name: String,
    /// The denied name that was found.
    pub name: String,
}

const CHECKED_MACROS: &[&str] = &[
    "info",
    "warn",
    "error",
    "info_span",
    "warn_span",
    "error_span",
    "event",
];

/// `true` when `name` names an identifier per the module docs.
#[must_use]
pub fn is_denied_name(name: &str) -> bool {
    // An all-caps name is a constant (an environment variable name, a
    // limit), never a runtime id.
    if name.len() > 1 && !name.chars().any(|c| c.is_ascii_lowercase()) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    let lower = lower.trim_start_matches("r#");
    if ALLOWED_NAMES.contains(&lower) {
        return false;
    }
    let parts: Vec<&str> = lower.split('_').filter(|p| !p.is_empty()).collect();
    let Some(last) = parts.last() else {
        return false;
    };
    if parts.len() > 1 && COUNT_SUFFIXES.contains(last) {
        return false;
    }
    if parts.iter().any(|p| DENIED_COMPONENTS.contains(p)) {
        return true;
    }
    if parts.contains(&"group") {
        return parts.len() == 1
            || parts
                .iter()
                .any(|p| matches!(*p, "id" | "ids" | "hex" | "b64" | "bytes_hex"));
    }
    false
}

/// Every identifier token (`[A-Za-z_][A-Za-z0-9_]*`) of an expression, with
/// string literals skipped.
fn idents(expr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = expr.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            i = skip_string(&chars, i);
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
            continue;
        }
        i += 1;
    }
    out
}

/// Index just past the string literal starting at `i` (a `"`).
fn skip_string(chars: &[char], i: usize) -> usize {
    let mut j = i + 1;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 2,
            '"' => return j + 1,
            _ => j += 1,
        }
    }
    j
}

/// Index just past a raw string literal starting at `i` (the `r`), or `None`
/// when `i` does not start one.
fn skip_raw_string(chars: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let mut hashes = 0;
    while j < chars.len() && chars[j] == '#' {
        hashes += 1;
        j += 1;
    }
    if j >= chars.len() || chars[j] != '"' {
        return None;
    }
    j += 1;
    while j < chars.len() {
        if chars[j] == '"' {
            let mut k = j + 1;
            let mut seen = 0;
            while k < chars.len() && chars[k] == '#' && seen < hashes {
                seen += 1;
                k += 1;
            }
            if seen == hashes {
                return Some(k);
            }
        }
        j += 1;
    }
    Some(j)
}

/// Whether an expression is a scalar derived from a collection or an id
/// (`peers.len()`, `x.is_some()`): logging it reveals no identifier.
fn is_derived_scalar(expr: &str) -> bool {
    let e = expr.trim();
    if REDACTIONS.iter().any(|r| e.contains(r)) {
        return true;
    }
    [
        ".len()",
        ".count()",
        ".is_empty()",
        ".is_some()",
        ".is_none()",
        ".is_ok()",
        ".is_err()",
        ".elapsed()",
        ".kind()",
        ".kind_str()",
        // The SFU's media line ids of a peer's light and middle tracks.
        ".light_mid()",
        ".middle_mid()",
    ]
    .iter()
    .any(|s| e.ends_with(s))
}

/// Split the macro body at top-level commas.
fn split_args(body: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut i = 0;
    while i < body.len() {
        let c = body[i];
        match c {
            '"' => {
                let end = skip_string(body, i);
                cur.extend(&body[i..end.min(body.len())]);
                i = end;
                continue;
            },
            '\'' => {
                // A char literal ('x', '\n'); a lifetime has no closing quote
                // within three characters and is copied as is.
                if let Some(rel) = body[i + 1..body.len().min(i + 5)]
                    .iter()
                    .position(|&q| q == '\'')
                {
                    let end = i + 1 + rel + 1;
                    cur.extend(&body[i..end]);
                    i = end;
                    continue;
                }
            },
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                i += 1;
                continue;
            },
            _ => {},
        }
        cur.push(c);
        i += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Inline captures `{name}` / `{name:?}` of a format string literal.
fn inline_captures(literal: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = literal.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            if chars.get(i + 1) == Some(&'{') {
                i += 2;
                continue;
            }
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j > start && matches!(chars.get(j), Some('}' | ':')) {
                out.push(chars[start..j].iter().collect());
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// The denied names one macro body names, in order.
fn denied_in_body(macro_name: &str, body: &[char]) -> Vec<String> {
    let mut args = split_args(body);
    if macro_name == "event" {
        let Some(first) = args.first() else {
            return Vec::new();
        };
        let level_arg = if first.trim_start().starts_with("target:")
            || first.trim_start().starts_with("parent:")
        {
            args.get(1).cloned().unwrap_or_default()
        } else {
            first.clone()
        };
        let checked = ["Level::INFO", "Level::WARN", "Level::ERROR"]
            .iter()
            .any(|l| level_arg.contains(l));
        if !checked {
            return Vec::new();
        }
        args.retain(|a| !a.contains("Level::"));
    }
    if macro_name.ends_with("_span") && !args.is_empty() {
        // The first argument of a span macro is its name.
        args.remove(0);
    }
    let mut out: Vec<String> = Vec::new();
    let mut after_format = false;
    for raw in &args {
        let arg = raw.trim();
        if arg.is_empty()
            || arg.starts_with("target:")
            || arg.starts_with("parent:")
            || arg.starts_with("name:")
        {
            continue;
        }
        if arg.starts_with('"') || arg.starts_with("r\"") || arg.starts_with("r#") {
            for cap in inline_captures(arg) {
                if is_denied_name(&cap) {
                    out.push(cap);
                }
            }
            after_format = true;
            continue;
        }
        if after_format {
            // A positional format argument.
            if !is_derived_scalar(arg) {
                out.extend(idents(arg).into_iter().filter(|n| is_denied_name(n)));
            }
            continue;
        }
        // A field: `name = value`, `name.sub = value`, `%expr`, `?expr`, `expr`.
        if let Some((name, value)) = split_field(arg) {
            let redacted = REDACTIONS.iter().any(|r| value.contains(r));
            if !redacted && is_denied_name(name.rsplit('.').next().unwrap_or(name)) {
                out.push(name.to_string());
            }
            if !is_derived_scalar(value) {
                out.extend(
                    idents(value.trim_start_matches(['%', '?']))
                        .into_iter()
                        .filter(|n| is_denied_name(n)),
                );
            }
        } else {
            let expr = arg.trim_start_matches(['%', '?']);
            if !is_derived_scalar(expr) {
                out.extend(idents(expr).into_iter().filter(|n| is_denied_name(n)));
            }
        }
    }
    // One report per name per macro.
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|n| seen.insert(n.clone()));
    out
}

/// `name = value` at the top level of a field argument (not `==`).
fn split_field(arg: &str) -> Option<(&str, &str)> {
    let bytes = arg.as_bytes();
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'"' => return None,
            b'=' if depth == 0 => {
                let next = bytes.get(i + 1).copied();
                let prev = if i == 0 { None } else { Some(bytes[i - 1]) };
                if next == Some(b'=') || matches!(prev, Some(b'=' | b'!' | b'<' | b'>')) {
                    return None;
                }
                let name = arg[..i].trim();
                if name.is_empty()
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                {
                    return None;
                }
                return Some((name, arg[i + 1..].trim()));
            },
            _ => {},
        }
    }
    None
}

/// Lint one Rust source file. Returns every violation, in source order.
#[must_use]
pub fn lint_source(text: &str) -> Vec<Violation> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut line = 1usize;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == '"' {
            let end = skip_string(&chars, i);
            line += chars[i..end.min(chars.len())]
                .iter()
                .filter(|&&x| x == '\n')
                .count();
            i = end;
            continue;
        }
        if c == '\'' {
            // A char literal ('x', '\n', '\'', '\u{1F600}') or a lifetime.
            if chars.get(i + 1) == Some(&'\\') {
                if let Some(rel) = chars[i + 2..chars.len().min(i + 12)]
                    .iter()
                    .position(|&q| q == '\'')
                {
                    i += 2 + rel + 1;
                    continue;
                }
            } else if chars.get(i + 2) == Some(&'\'') {
                i += 3;
                continue;
            }
            i += 1;
            continue;
        }
        if c == 'r' && (i == 0 || !is_ident_char(chars[i - 1])) {
            if let Some(end) = skip_raw_string(&chars, i) {
                line += chars[i..end.min(chars.len())]
                    .iter()
                    .filter(|&&x| x == '\n')
                    .count();
                i = end;
                continue;
            }
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let preceded_ok = start == 0 || !is_ident_char(chars[start - 1]);
            if preceded_ok && CHECKED_MACROS.contains(&word.as_str()) {
                let mut j = i;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) == Some(&'!') {
                    j += 1;
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if matches!(chars.get(j), Some('(' | '[' | '{')) {
                        if let Some(close) = matching_close(&chars, j) {
                            for name in denied_in_body(&word, &chars[j + 1..close]) {
                                out.push(Violation {
                                    line,
                                    macro_name: word.clone(),
                                    name,
                                });
                            }
                        }
                    }
                }
            }
            continue;
        }
        i += 1;
    }
    out
}

const fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Index of the bracket closing the one at `open`, skipping strings.
fn matching_close(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                i = skip_string(chars, i);
                continue;
            },
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            },
            _ => {},
        }
        i += 1;
    }
    None
}

/// Whether a file mounts `TraceLayer::new_for_http()` without the shared
/// [`crate::request_span`]. Returns the 1-based lines of each such mount.
#[must_use]
pub fn bare_trace_layers(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let needle = "TraceLayer::new_for_http()";
    for (offset, _) in text.match_indices(needle) {
        let rest: String = text[offset + needle.len()..]
            .chars()
            .filter(|c| !c.is_whitespace())
            .take(64)
            .collect();
        if !rest.starts_with(".make_span_with(lakko_log_hygiene::request_span)") {
            out.push(text[..offset].matches('\n').count() + 1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<String> {
        lint_source(src).into_iter().map(|v| v.name).collect()
    }

    #[test]
    fn denied_field_names_are_reported() {
        assert_eq!(
            names(r#"tracing::info!(account_id = %a, "x");"#),
            ["account_id"]
        );
        assert_eq!(names(r#"warn!(%call_id, "x");"#), ["call_id"]);
        assert_eq!(names(r#"error!(?device, "x");"#), ["device"]);
        assert_eq!(
            names("tracing::info!(\n    blob_id = %b,\n    owner_account_id = %o,\n    \"stored\",\n);"),
            ["blob_id", "owner_account_id"]
        );
    }

    #[test]
    fn values_and_format_arguments_are_reported() {
        assert_eq!(
            names(r#"info!(id = %hex::encode(group_id), "x");"#),
            ["group_id"]
        );
        assert_eq!(names(r#"info!("joined {}", call_id);"#), ["call_id"]);
        assert_eq!(names(r#"info!("joined {room_id}");"#), ["room_id"]);
        assert_eq!(names(r#"info!("joined {room_id:?}");"#), ["room_id"]);
    }

    #[test]
    fn counts_categories_and_debug_are_not_reported() {
        assert!(names(r#"info!(peers = peers.len(), "x");"#).is_empty());
        assert!(names(r#"info!(peer_count = n, rows, "pruned {} rows", rows);"#).is_empty());
        assert!(names(r#"debug!(account_id = %a, "debug is not checked");"#).is_empty());
        assert!(names(r"tracing::trace!(%call_id);").is_empty());
        assert!(names(r#"info!(error = %e, "insert ledger row");"#).is_empty());
        assert!(names(r#"info!(token_context = %c, "spent");"#).is_empty());
    }

    #[test]
    fn comments_strings_and_other_macros_are_skipped() {
        assert!(names("// info!(account_id = %a);\nlet s = \"info!(account_id)\";").is_empty());
        assert!(names("my_info!(account_id = %a);").is_empty());
        assert!(names("/* warn!(call_id) */").is_empty());
    }

    #[test]
    fn event_is_checked_at_info_and_above_only() {
        assert_eq!(
            names("tracing::event!(Level::WARN, account_id = %a, \"x\");"),
            ["account_id"]
        );
        assert!(names("tracing::event!(Level::DEBUG, account_id = %a);").is_empty());
    }

    #[test]
    fn group_ids_are_denied_but_other_group_words_are_not() {
        assert!(is_denied_name("group_id"));
        assert!(is_denied_name("group"));
        assert!(is_denied_name("group_hex"));
        assert!(!is_denied_name("group_count"));
        assert!(!is_denied_name("groups"));
        assert!(!is_denied_name("grouping"));
    }

    #[test]
    fn constants_redactions_and_duplicates() {
        assert!(names(r#"info!(var = HANDLE_CHANGE_INTERVAL_VAR, "x");"#).is_empty());
        assert!(names(r#"info!(peer = %peer.id().log_tag(), "x");"#).is_empty());
        assert_eq!(
            names(r#"info!(account_id = %B64.encode(account_id), "x");"#),
            ["account_id"]
        );
    }

    #[test]
    fn the_line_number_points_at_the_macro() {
        let v = lint_source("fn f() {\n\n    tracing::info!(push_gid = %g);\n}");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].line, 3);
        assert_eq!(v[0].macro_name, "info");
    }

    #[test]
    fn a_bare_trace_layer_is_reported() {
        assert_eq!(
            bare_trace_layers("x\n.layer(TraceLayer::new_for_http())"),
            [2]
        );
        assert!(bare_trace_layers(
            ".layer(TraceLayer::new_for_http()\n   .make_span_with(lakko_log_hygiene::request_span))"
        )
        .is_empty());
    }
}
