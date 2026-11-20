// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0078 section 11, decision 5 half a: no `tracing` macro at info level
//! or above in any service names an identifier, and every service mounts
//! the shared request span.
//!
//! Walks every `services/*/src` tree and the server-side crates the services
//! link (`lakko-anon-token-server`, `lakko-pow-server`) and fails with the
//! list of offending lines. Run alone with `just log-lint`.

use std::fs;
use std::path::{Path, PathBuf};

use lakko_log_hygiene::lint::{bare_trace_layers, lint_metric_labels, lint_source};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn scanned_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = Vec::new();
    let services = root.join("services");
    for entry in fs::read_dir(&services).expect("services/ exists").flatten() {
        let src = entry.path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    for krate in [
        "lakko-anon-token-server",
        "lakko-pow-server",
        "lakko-metrics",
    ] {
        rust_files(&root.join("crates").join(krate).join("src"), &mut files);
    }
    files.sort();
    files
}

#[test]
fn no_service_log_line_at_info_or_above_names_an_identifier() {
    let root = repo_root();
    let files = scanned_files();
    assert!(
        files.len() > 50,
        "the walk found {} files; is the repository layout intact?",
        files.len()
    );
    let mut report = Vec::new();
    for path in &files {
        let text = fs::read_to_string(path).expect("readable source");
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        for v in lint_source(&text) {
            report.push(format!(
                "{rel}:{} {}! names `{}`",
                v.line, v.macro_name, v.name
            ));
        }
    }
    assert!(
        report.is_empty(),
        "ADR-0078 section 11: {} log line(s) name an identifier:\n{}",
        report.len(),
        report.join("\n")
    );
}

#[test]
fn every_trace_layer_records_the_route_template_not_the_uri() {
    let root = repo_root();
    let mut report = Vec::new();
    for path in scanned_files() {
        let text = fs::read_to_string(&path).expect("readable source");
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for line in bare_trace_layers(&text) {
            report.push(format!("{rel}:{line}"));
        }
    }
    assert!(
        report.is_empty(),
        "ADR-0078 section 11: mount TraceLayer with \
         .make_span_with(lakko_log_hygiene::request_span):\n{}",
        report.join("\n")
    );
}

#[test]
fn no_metric_label_names_or_builds_an_identifier() {
    let root = repo_root();
    let mut report = Vec::new();
    for path in scanned_files() {
        let text = fs::read_to_string(&path).expect("readable source");
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for v in lint_metric_labels(&text) {
            report.push(format!(
                "{rel}:{} {}! label names `{}`",
                v.line, v.macro_name, v.name
            ));
        }
    }
    assert!(
        report.is_empty(),
        "ADR-0096 section 1: {} metric label(s) name or build an identifier:\n{}",
        report.len(),
        report.join("\n")
    );
}

/// Services that serve nothing yet (Phase-0 stubs) and so mount no router.
const STUB_SERVICES: &[&str] = &["edge", "transport-rendezvous"];

#[test]
fn every_service_mounts_the_red_layer_and_a_metrics_listener() {
    let root = repo_root();
    let mut report = Vec::new();
    for entry in fs::read_dir(root.join("services"))
        .expect("services/ exists")
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let src = entry.path().join("src");
        if !src.is_dir() || STUB_SERVICES.contains(&name.as_str()) {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        let text: String = files
            .iter()
            .map(|f| fs::read_to_string(f).expect("readable source"))
            .collect();
        if !text.contains("lakko_metrics::red_layer(") {
            report.push(format!("services/{name}: no lakko_metrics::red_layer"));
        }
        if !text.contains("lakko_metrics::start_from_env(")
            && !text.contains("lakko_metrics::serve(")
        {
            report.push(format!("services/{name}: no metrics listener"));
        }
    }
    assert!(
        report.is_empty(),
        "ADR-0096 section 2: every service records RED metrics and serves /metrics:\n{}",
        report.join("\n")
    );
}
