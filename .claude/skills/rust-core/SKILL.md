---
name: rust-core
description: Background knowledge for Lakko Rust crate development — invariants, code-style rules, crate boundaries, and review checklist. Auto-applies when working on crates/, services/, or Cargo.toml.
user-invocable: false
---

# Lakko Rust core — domain knowledge

You are working in a security-critical E2EE messenger. Every change to `crates/` or `services/` touches code that an external auditor will review. Apply these rules without exception.

## Crate hygiene

- `#![forbid(unsafe_code)]` on every crate EXCEPT `lakko-ffi-uniffi` and `lakko-ffi-wasm` (justified unsafe with `// SAFETY:` comments).
- No `unwrap()` / `expect()` in production code — only in `#[cfg(test)]` and benchmarks. Use `?` + `thiserror`.
- No `panic!()` in library code. Allowed in `fn main()` for unrecoverable startup.
- No `println!` / `eprintln!` — use `tracing::{info, warn, error, debug, trace}`.
- `clippy::all` + `clippy::pedantic` + `clippy::nursery` with `-D warnings`.
- `missing_docs` warning on all public items.
<!-- REUSE-IgnoreStart -->
- SPDX header `// SPDX-License-Identifier: AGPL-3.0-or-later` on every file.
<!-- REUSE-IgnoreEnd -->

## Dependency discipline

- Do NOT add new dependencies without explicit justification. Every dep is supply-chain risk.
- `cargo deny` blocks unknown licences — check before adding.
- Pin versions in `[workspace.dependencies]`; crates inherit via `dep.workspace = true`.

## Crate boundary rules

| Crate | Allowed dependencies | Notes |
|---|---|---|
| `lakko-crypto` | Only `ring`, `hpke`, `argon2`, `blake3` | Crypto primitives — no network, no I/O |
| `lakko-mls` | `lakko-crypto`, `openmls` | MLS state machine — ADR + property tests for ANY change |
| `lakko-attest` | `lakko-crypto` (Ed25519 only) | `#![forbid(unsafe_code)]`, no JWS/JWKS dep |
| `lakko-transport` | `lakko-crypto`, network crates | Transport layer — must not import `lakko-mls` |
| `lakko-calls` | `lakko-crypto` | SFrame — must not import MLS directly |
| `lakko-ffi-*` | Everything | FFI boundary — the only crates that may use `unsafe` |

## Security invariants (NEVER violate)

1. No plaintext PII on the server
2. Server only sees ciphertext + public artefacts
3. MLS state machine changes require ADR + property tests + crypto audit
4. No fallback to non-E2EE — if handshake fails, connection fails
5. Social graph not materialised on the server
6. Identity rotation only via KT log supersession proof
7. Hardware-backed device keys mandatory on every platform

## When touching `lakko-mls`

- Check existing property tests in `crates/lakko-mls/tests/`
- Add or extend property tests for new behavior
- Reference the relevant ADR in the commit message
- Verify against I-1..I-8 invariants (see `docs/security/mls-state-machine.md`)

## When touching `lakko-transport`

- Check T-1..T-15 transport invariants (see `docs/security/transport.md`)
- Verify no silent downgrades — loud fail is always correct
- ECH / REALITY / SS-2022 / MASQUE all have specific wire-format constraints
- Fuzz targets exist — extend them if adding new parser surfaces

## When touching `lakko-calls` / SFrame

- Check C-1..C-14 media-key pipeline invariants
- Frame counter must be strictly monotonic per sender per epoch
- AEAD AAD = RFC 9605 SFrame header bytes (KID+CTR), nothing else (ADR-0013)
- Cross-platform interop: native C ABI + wasm-bindgen + web worker must all agree

## When touching services

- Each service has its own database (separate `DATABASE_URL`)
- Migrations are in `services/<name>/migrations/`
- `sqlx` tracks applied migrations — services must NOT share a DB
- Server stays opaque — never decrypts client data (invariant #2)

## Pre-commit checks

Run these before considering work done:
```
just fmt       # cargo fmt
just lint      # clippy with -D warnings
just test      # cargo test --workspace
just reuse     # SPDX headers
```

## Fuzz targets

When adding a new attacker-touchable parser surface, add a fuzz target in the crate's `fuzz/fuzz_targets/`. Existing fuzz crates: `lakko-mls`, `lakko-proto`, `lakko-contacts`, `lakko-channels`, `lakko-transport`, `lakko-tuf`, `lakko-call-jwt`, `lakko-backup`, `services/sfu`.
