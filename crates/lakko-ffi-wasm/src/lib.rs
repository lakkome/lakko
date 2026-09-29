// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for Lakko Web and Tauri clients.
//!
//! Mirror of the UniFFI surface exposed by `lakko-ffi-uniffi`. Same
//! Rust core, two FFI boundaries — UniFFI for Swift / Kotlin,
//! wasm-bindgen for JavaScript / TypeScript — with semantically
//! identical APIs.
//!
//! ## Module map
//!
//! - [`mls`] — `Identity`, `Group`, `AddMember`, `ProcessedMessage`
//!   (Phase 1.13a). The MLS protocol surface every conversation
//!   eventually drives.
//! - [`sync`] — `Hlc`, `HybridLogicalClock`, `EditChain`, `Draft`
//!   from `lakko-sync` (Phase 1.13b).
//! - [`push`] — `PushPayload` from `lakko-push` (1.13b).
//! - [`presence`] — `PresenceToken`, `PresenceTracker`, derivation
//!   helpers from `lakko-presence` (1.13b).
//! - [`contacts`] — pseudonym lifecycle (`IdentityPseudonym`,
//!   `StrangerInbox`, `PerRelationship`) and `ContactRequest`
//!   compose / decode / open helpers from `lakko-contacts` (1.13b).
//! - [`storage`] — `Store`-backed types from `lakko-storage`
//!   (`InMemoryStore`, `EncryptedStore`, `MessageStore`,
//!   `GroupStateStore`). Web persistent storage (`IndexedDbStore`)
//!   ships in 1.13c.
//! - [`auth`] — OPAQUE-PAKE client from `lakko-auth-client`
//!   (`OpaqueRegister`, `OpaqueLogin`) for `apps/web` signup /
//!   login (Phase 1.16c2).
//! - [`pow_solver`] — Equix PoW solver from `lakko-pow`
//!   (`PowSolver`, `PowProof`) for the anti-spam gate on the
//!   expensive auth endpoints (Phase 1.16c3a).
//! - [`mnemonic`] — BIP39 24-word recovery passphrase from
//!   `lakko-mnemonic` (`RecoveryMnemonic`) for the parallel
//!   OPAQUE recovery enrollment at signup (Phase 1.16c3c1).
//! - [`discovery`] — VOPRF client from `lakko-discovery-client`
//!   (`OprfBlinder`, `normalizeUsername`) for username
//!   register / lookup against `lakko-svc-discovery` (Phase 1.16d1).
//!
//! ## Naming convention
//!
//! Public Rust items keep snake_case. JS-visible names are
//! camelCase via `#[wasm_bindgen(js_name = ...)]`. This way the
//! Rust impl reads like normal Rust while the TypeScript surface
//! matches what JS / TS programmers expect.
//!
//! Errors map to `JsError` with the source `Display` preserved.
//! Structured error classes can be added later (when `apps/web`
//! actually wants to discriminate).
//!
//! ## What's NOT here (1.13c)
//!
//! - **IndexedDB-backed `Store` impl** — Web persistence. Requires
//!   `web-sys::IdbDatabase` plus an async-aware `Store` trait
//!   variant; that's a self-contained sub-sprint.
//! - **wasm `fetch`-based HTTP transport** — `reqwest` already
//!   routes through `web-sys::fetch` on wasm32, but plumbing it
//!   through `lakko-transport::http` and exposing it from this
//!   crate wants its own sprint.
//! - **TypeScript `.d.ts` post-processing** — wasm-bindgen produces
//!   reasonable `.d.ts` automatically; richer types (branded
//!   strings, JSDoc passthrough) when `apps/web` (1.16) actually
//!   consumes this surface.
//! - **wasm-pack publish pipeline** — driven by `apps/web`'s build
//!   in 1.16.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod anon_token;
pub mod attachments;
pub mod auth;
pub mod backup;
pub mod channels;
pub mod contacts;
pub mod device_link;
pub mod discovery;
pub mod ds_proof;
pub mod ds_statement;
pub mod mls;
pub mod mnemonic;
pub mod pairing;
pub mod pow_solver;
pub mod presence;
pub mod push;
pub mod release;
pub mod scoped_discovery;
pub mod sframe;
pub mod sfu_signaling;
pub mod shamir;
pub mod storage;
pub mod supersession;
pub mod sync;
pub mod transparency;
pub mod ui_rules;
pub mod word_vault;

// Phase 1.16f3 — wasm-bindgen-test smoke suite. Gated on BOTH
// `cfg(test)` (so the lib build never tries to pull in the
// `wasm-bindgen-test` dev-dep that isn't on the dependency
// graph outside test builds) AND `cfg(target_arch = "wasm32")`
// (so a host `cargo test -p lakko-ffi-wasm` keeps using the
// regular `#[cfg(test)] mod tests` blocks scattered through the
// other modules and never tries to load these wasm-only smokes).
// `wasm-pack test --node` flips both flags on at once.
#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests;

// Re-export top-level public items so JS callers don't see the
// module path (wasm-bindgen flattens by default; explicit re-exports
// keep the rustdoc surface tidy).
pub use auth::{OpaqueLogin, OpaqueLoginOutput, OpaqueRegister, OpaqueRegisterOutput};
pub use contacts::{
    compose_profile_envelope, decode_profile_envelope, normalize_display_name, profile_is_newer,
    ContactRequest, Direction, IdentityPseudonym, InviteCapability, OpenedContactRequest,
    PerRelationship, StrangerInbox, WasmProfile,
};
pub use discovery::{login_handle, normalize_username, validate_username, OprfBlinder};
pub use mls::{AddMember, Group, Identity, ProcessedMessage, ProcessedMessageKind};
pub use mnemonic::RecoveryMnemonic;
pub use pow_solver::{PowProof, PowSolver};
pub use presence::{current_epoch, derive_token, PresenceToken, PresenceTracker};
pub use push::PushPayload;
pub use storage::{
    EncryptedStore, GroupStateStore, InMemoryStore, LocalMessage, MessageKind, MessageStore, Table,
};
pub use sync::{Draft, EditChain, EditOp, EditResolved, Hlc, HybridLogicalClock};
pub use word_vault::{
    passkey_vault_open, passkey_vault_seal, word_vault_answer, word_vault_enroll, WordVaultAnswer,
    WordVaultEnrollment,
};
