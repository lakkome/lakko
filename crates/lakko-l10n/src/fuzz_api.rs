// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Entry points for the fuzz harnesses. Not part of the supported API.

use lakko_crypto::signing::SigningKey;

use crate::bundle::{accept_bundle, AcceptancePolicy};
use crate::catalog::{Catalog, SourceCatalog};
use crate::language::language;
use crate::message::{Arg, Message};
use crate::plural::Plurals;

/// Accept arbitrary bytes as a bundle under a fixed key and policy. The
/// fuzzer cannot forge the signature, so this exercises the length, layout
/// and signature-refusal paths.
pub fn accept_arbitrary_bundle(data: &[u8]) {
    let key = SigningKey::from_seed(&[42u8; 32]).verifying_key();
    let _ = accept_bundle(
        data,
        &key,
        AcceptancePolicy {
            current_release: 1,
            built_in_version: 0,
            last_accepted: None,
        },
    );
}

/// Parse arbitrary text as a message and, when it parses, format it in
/// every supported language with a fixed argument set, and check it as a
/// translation of itself. Must never panic.
pub fn parse_and_format_message(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(message) = Message::parse(text) else {
        return;
    };
    let args = [
        (
            "n",
            Arg::Count {
                value: 3,
                display: None,
            },
        ),
        ("name", Arg::Text("x")),
    ];
    for l in crate::language::supported_languages() {
        if let Ok(p) = Plurals::for_language(l) {
            let _ = message.format(&p, &args);
            let _ = message.check_translation(&message, &p);
        }
    }
}

/// Load arbitrary bytes as a source catalog and, when it loads, build a
/// catalog from it. Must never panic.
pub fn load_arbitrary_source(data: &[u8]) {
    if let Ok(source) = SourceCatalog::from_json(data) {
        if language("ru").is_some() {
            let _ = Catalog::new(&source, "ru", None, None);
        }
    }
}
