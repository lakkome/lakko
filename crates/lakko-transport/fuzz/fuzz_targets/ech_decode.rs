// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `parse_ech_config_list` per
//! draft-ietf-tls-esni-18 §4.
//!
//! The DNS HTTPS resource record (RR type 65) carries
//! the `ech=...` parameter as opaque bytes the client
//! MUST parse before configuring rustls. Any attacker
//! that can spoof / poison the DNS answer (or any cache
//! between the recursive resolver and the client) can
//! deliver arbitrary bytes through this parser. The
//! parser is the gate that separates attacker-controlled
//! bytes from rustls — a panic / overflow / stack abuse
//! here is a vulnerability on the anti-censorship
//! bootstrap path. Pinned in audit-pack invariant T-12.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_transport::parse_ech_config_list;

fuzz_target!(|data: &[u8]| {
    let _ = parse_ech_config_list(data);
});
