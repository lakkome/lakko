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

pub mod auth;
pub mod contacts;
pub mod mls;
pub mod pow_solver;
pub mod presence;
pub mod push;
pub mod storage;
pub mod sync;

// Re-export top-level public items so JS callers don't see the
// module path (wasm-bindgen flattens by default; explicit re-exports
// keep the rustdoc surface tidy).
pub use auth::{OpaqueLogin, OpaqueLoginOutput, OpaqueRegister, OpaqueRegisterOutput};
pub use contacts::{ContactRequest, Direction, IdentityPseudonym, PerRelationship, StrangerInbox};
pub use mls::{AddMember, Group, Identity, ProcessedMessage, ProcessedMessageKind};
pub use pow_solver::{PowProof, PowSolver};
pub use presence::{current_epoch, derive_token, PresenceToken, PresenceTracker};
pub use push::PushPayload;
pub use storage::{
    EncryptedStore, GroupStateStore, InMemoryStore, LocalMessage, MessageKind, MessageStore, Table,
};
pub use sync::{Draft, EditChain, EditOp, EditResolved, Hlc, HybridLogicalClock};
