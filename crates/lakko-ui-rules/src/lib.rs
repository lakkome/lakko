// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Presentation rules the three clients must agree on, written once.
//!
//! Every other shared rule in this workspace lives in Rust for one reason:
//! three hand-written copies of one rule drift, and the drift is only found
//! when a person notices the clients disagreeing. `should_request_profiles`,
//! `acting_committer_verdict` and `push_routing_secret_is_newer` are here for
//! that reason, and so is this.
//!
//! What belongs here is a rule that decides something a person SEES and that
//! has no platform in it - an order, a predicate, a classification. What does
//! not belong is anything that touches a screen, a store or the network:
//! those differ per platform by nature, and a rule that reaches for them
//! stops being a rule.
//!
//! Deliberately its own crate rather than a module of `lakko-contacts`.
//! Sorting a conversation list is not a fact about contacts, and the crates
//! an external reviewer reads should not grow presentation logic
//! ([`docs/security/external-review-scope.md`]). Nothing here is
//! security-relevant, and nothing here should become so: if a rule in this
//! crate ever needs a key, a secret or a peer's assertion, it is in the wrong
//! crate.

pub mod av_align;
pub mod call_bar;
pub mod call_group;
pub mod capture;
pub mod conversation;
pub mod emoji;
pub mod folder;
pub mod guest_name;
pub mod handle;
pub mod info;
pub mod layer_display;
pub mod lookup;
pub mod message_links;
pub mod reaction;
pub mod ring;
pub mod speaking;
pub mod visibility;
