// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The shared rules in front of a network search by name.
///
/// `@username` and a channel's `@handle` are the same mechanism over two
/// namespaces: blind the name, ask, get an exact answer or nothing. The search
/// field asks both by itself (`docs/ux/who-can-find-me.md` section 6.1,
/// ``PeopleSearch``); what this file owns is the gate: whether a typed string
/// could be a name at all is `looks_like_a_name` in `crates/lakko-ui-rules`,
/// shared with the web and Android clients, and the leading `@` people type
/// is stripped by the same crate.
public enum NameLookup {

    /// Whether this is worth spending a lookup on, by the shared rule.
    ///
    /// The predicate is the canonical username rule the discovery blinder
    /// itself enforces, so asking first is the difference between asking
    /// nothing and making a person spend a lookup token to be told no.
    public static func looksLikeAName(_ typed: String) -> Bool {
        LakkoCore.looksLikeAName(typed: typed)
    }

    /// Strip the leading `@` people type and no namespace stores.
    public static func bareName(_ typed: String) -> String {
        LakkoCore.bareName(typed: typed)
    }
}
