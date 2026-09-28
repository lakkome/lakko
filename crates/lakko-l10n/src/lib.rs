// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Interface localization, written once for the three clients
//! ([ADR-0058](../../docs/adr/0058-interface-localization.md)).
//!
//! What lives here is every rule the three clients must agree on about the
//! words on the screen:
//!
//! - which language to show ([`resolve_language`]);
//! - how a message with arguments and plural forms becomes a sentence
//!   ([`Message`], the ICU MessageFormat subset of the ADR);
//! - which translation wins for a key ([`Catalog`]: a verified update, then
//!   the built-in translation, then English — a key is never missing);
//! - whether a signed translation update is accepted, and which of its
//!   strings may be used ([`bundle`]).
//!
//! What does not live here: storing the chosen language, switching the UI,
//! mirroring layouts, formatting dates and numbers. Those differ per platform
//! by nature and are done with the platform's own tools for the locale.
//!
//! The one security property this crate carries: a translation update can
//! never change a string the built-in source catalog marks security-critical,
//! whatever the update itself claims ([`Catalog::new`]).

pub mod bundle;
pub mod catalog;
mod error;
#[doc(hidden)]
pub mod fuzz_api;
pub mod language;
pub mod message;
pub mod plural;

pub use bundle::{accept_bundle, sign_bundle, AcceptancePolicy, AcceptedBundle};
pub use catalog::{compile_translation, Catalog, SourceCatalog};
pub use error::L10nError;
pub use language::{
    language, match_tag, resolve_language, supported_languages, Direction, Language,
    SOURCE_LANGUAGE,
};
pub use message::{Arg, ArgKind, Message};
pub use plural::Category;

// `icu_provider` is a dependency only to switch on its `sync` feature.
use icu_provider as _;

#[cfg(test)]
mod send_sync {
    const fn assert_send_sync<T: Send + Sync>() {}
    const _: () = assert_send_sync::<crate::Catalog>();
    const _: () = assert_send_sync::<crate::SourceCatalog>();
    const _: () = assert_send_sync::<crate::AcceptedBundle>();
}
