// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The check a service test runs over its rendered `/metrics` ([ADR-0096]
//! section 1.5).
//!
//! [`check`] parses the Prometheus text exposition and reports:
//!
//! - a metric whose name does not start with `lakko_`;
//! - a label whose name is not in [`BASE_LABELS`] or the caller's own list;
//! - a `route` value that is neither a template (`/...`) nor `unmatched`;
//! - a label value longer than [`MAX_LABEL_VALUE_LEN`];
//! - anything identifier-shaped anywhere in the text
//!   ([`lakko_log_hygiene::scan::find_identifiers`]), unless on the
//!   caller's allow-list.
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md

use lakko_log_hygiene::scan::find_identifiers;

/// Label names every service may use.
pub const BASE_LABELS: &[&str] = &[
    "service", "route", "method", "status", "code", "version", "task", "outcome", "state",
    // The shared anonymous-token spend counter (lakko-anon-token-server).
    "context", // Added by the exporter to histogram buckets and summaries.
    "le", "quantile",
];

/// The longest label value accepted. A route template is the longest value
/// today; an identifier glued into a value would usually also be caught by
/// the shape scan.
pub const MAX_LABEL_VALUE_LEN: usize = 160;

/// One sample line of the exposition: the metric name and its labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// The metric name (with any `_bucket` / `_sum` / `_count` suffix).
    pub name: String,
    /// The labels, in the order written.
    pub labels: Vec<(String, String)>,
}

/// Parse the sample lines of a text exposition. Comment lines (`# HELP`,
/// `# TYPE`) and blank lines are skipped. A line that does not parse is
/// returned as an `Err` with its text.
pub fn samples(text: &str) -> Vec<Result<Sample, String>> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| parse_line(l).ok_or_else(|| l.to_owned()))
        .collect()
}

fn parse_line(line: &str) -> Option<Sample> {
    let name_end = line
        .find(|c: char| c == '{' || c.is_whitespace())
        .unwrap_or(line.len());
    let name = line[..name_end].to_owned();
    if name.is_empty() {
        return None;
    }
    let mut labels = Vec::new();
    let rest = &line[name_end..];
    if let Some(body) = rest.strip_prefix('{') {
        let chars: Vec<char> = body.chars().collect();
        let mut i = 0;
        loop {
            while i < chars.len() && (chars[i] == ',' || chars[i].is_whitespace()) {
                i += 1;
            }
            if chars.get(i) == Some(&'}') {
                break;
            }
            let key_start = i;
            while i < chars.len() && chars[i] != '=' {
                i += 1;
            }
            let key: String = chars.get(key_start..i)?.iter().collect();
            i += 1;
            if chars.get(i) != Some(&'"') {
                return None;
            }
            i += 1;
            let mut value = String::new();
            loop {
                match chars.get(i)? {
                    '\\' => {
                        let escaped = chars.get(i + 1)?;
                        value.push(if *escaped == 'n' { '\n' } else { *escaped });
                        i += 2;
                    },
                    '"' => {
                        i += 1;
                        break;
                    },
                    c => {
                        value.push(*c);
                        i += 1;
                    },
                }
            }
            labels.push((key.trim().to_owned(), value));
        }
    }
    Some(Sample { name, labels })
}

/// Every problem in `text`, as readable lines; empty when it is clean.
///
/// `extra_labels` are label names this service uses beyond [`BASE_LABELS`]
/// (the SFU's `direction`, `kind`, `reason`, ...). `allow` are values that
/// have an identifier's shape and are reviewed to be harmless; empty in
/// every service today.
pub fn check(text: &str, extra_labels: &[&str], allow: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    for sample in samples(text) {
        let sample = match sample {
            Ok(s) => s,
            Err(line) => {
                problems.push(format!("unparsed line: {line}"));
                continue;
            },
        };
        if !sample.name.starts_with("lakko_") {
            problems.push(format!(
                "metric outside the lakko_ namespace: {}",
                sample.name
            ));
        }
        for (key, value) in &sample.labels {
            if !BASE_LABELS.contains(&key.as_str()) && !extra_labels.contains(&key.as_str()) {
                problems.push(format!("{}: label `{key}` is not allowed", sample.name));
            }
            if key == "route" && value != "unmatched" && !value.starts_with('/') {
                problems.push(format!(
                    "{}: route `{value}` is not a template",
                    sample.name
                ));
            }
            if value.len() > MAX_LABEL_VALUE_LEN {
                problems.push(format!(
                    "{}: label `{key}` is {} bytes long",
                    sample.name,
                    value.len()
                ));
            }
        }
    }
    for finding in find_identifiers(text, allow) {
        problems.push(format!(
            "identifier-shaped value ({:?}): {}",
            finding.kind, finding.value
        ));
    }
    problems
}

/// Run `f` with a recorder of its own on this thread and return what it
/// recorded, rendered. For tests of code that records synchronously: the
/// process-wide recorder is shared by every test of a binary, this one is not.
pub fn record_locally(f: impl FnOnce()) -> String {
    let Ok(builder) =
        metrics_exporter_prometheus::PrometheusBuilder::new().set_buckets(crate::DURATION_BUCKETS)
    else {
        // The bucket list is a non-empty constant; this cannot fail.
        return String::new();
    };
    let recorder = builder.build_recorder();
    let handle = recorder.handle();
    metrics::with_local_recorder(&recorder, f);
    handle.render()
}

/// Panic with every problem [`check`] finds. For tests.
pub fn assert_clean(text: &str, extra_labels: &[&str], allow: &[&str]) {
    let problems = check(text, extra_labels, allow);
    assert!(
        problems.is_empty(),
        "ADR-0096 section 1: {} problem(s) in /metrics:\n{}\n--- exposition ---\n{text}",
        problems.len(),
        problems.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_parse_with_escapes() {
        let s = samples(r#"lakko_x_total{service="ds",route="/a/{b}",q="x\"y"} 3"#);
        assert_eq!(
            s,
            [Ok(Sample {
                name: "lakko_x_total".into(),
                labels: vec![
                    ("service".into(), "ds".into()),
                    ("route".into(), "/a/{b}".into()),
                    ("q".into(), "x\"y".into()),
                ],
            })]
        );
        assert_eq!(
            samples("# HELP x y\nlakko_up 1\n"),
            [Ok(Sample {
                name: "lakko_up".into(),
                labels: vec![]
            })]
        );
    }

    #[test]
    fn a_clean_exposition_passes() {
        let text = "# TYPE lakko_http_requests_total counter\n\
            lakko_http_requests_total{service=\"ds\",route=\"/ds/group/{group_hex}/messages\",method=\"GET\",status=\"2xx\"} 4\n\
            lakko_http_request_duration_seconds_bucket{service=\"ds\",route=\"unmatched\",method=\"other\",le=\"0.5\"} 1\n";
        assert!(
            check(text, &[], &[]).is_empty(),
            "{:?}",
            check(text, &[], &[])
        );
    }

    #[test]
    fn an_identifier_in_a_route_is_reported() {
        let id = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let text = format!(
            "lakko_http_requests_total{{service=\"ds\",route=\"/ds/group/{id}/messages\",method=\"GET\",status=\"2xx\"}} 1\n"
        );
        let problems = check(&text, &[], &[]);
        assert!(
            problems.iter().any(|p| p.contains("identifier-shaped")),
            "{problems:?}"
        );
    }

    #[test]
    fn foreign_labels_and_names_are_reported() {
        let text =
            "lakko_x_total{room=\"general\"} 1\nhttp_requests_total 1\nlakko_y{route=\"ds\"} 1\n";
        let problems = check(text, &[], &[]);
        assert!(problems.iter().any(|p| p.contains("label `room`")));
        assert!(problems.iter().any(|p| p.contains("outside the lakko_")));
        assert!(problems.iter().any(|p| p.contains("not a template")));
        assert!(check("lakko_x_total{kind=\"a\"} 1\n", &["kind"], &[]).is_empty());
    }
}
