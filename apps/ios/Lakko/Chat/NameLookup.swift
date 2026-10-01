// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Looking a name up on the network, from the conversation list.
///
/// `docs/ux/navigation.md` section 4. A group and a channel are kinds of
/// conversation, not kinds of contact, so finding one belongs in Chats —
/// Contacts is about people. And finding a person to talk to starts a
/// conversation too, so it belongs here as well.
///
/// **There is one lookup, not one per kind.** `@username` and a channel's
/// `@handle` are the same mechanism over two namespaces: blind the name, ask,
/// get an exact answer or nothing. A group handle would be a third namespace
/// and the same mechanism again. The person typing does not know which
/// namespace the name they were given lives in, and should not have to — so
/// this asks both and reports what it found.
///
/// **It cannot be fuzzy.** The server evaluates the blinded name without
/// learning it (invariant #5), which answers "does this exact name exist" and
/// cannot answer "which names start with these letters", because a prefix
/// search is the server learning the prefix. The caller must offer this as
/// "look up the whole name", never as a list of partial matches.
///
/// **The gate in front of it is not here.** Whether a typed string could be a
/// name at all is `looks_like_a_name` in `crates/lakko-ui-rules`, shared with
/// the web and Android clients; what this file owns is asking the two
/// namespaces and telling the difference between "no such name" and "no
/// answer".
///
/// Pure of the UI: the two lookups arrive as arguments, so this can be run by
/// a test without a network.

/// What a lookup turned up.
public enum FoundByName: Equatable, Sendable {
    /// The name is a channel handle. Its id is enough to open it.
    case channel(channelIdHex: String, handle: String)
    /// The name is a person's username. The contact request is Contacts' flow.
    case person(username: String)
    /// Both namespaces answered, and neither holds the name.
    case nothing
    /// Neither namespace could be reached. NOT the same as `nothing`.
    case unreachable
    /// ADR-0054 S3 — a lookup was refused because the account's daily
    /// allowance of lookups is used up (`anon-token-quota-exhausted`). Not a
    /// "nothing found": the name was never asked about.
    case allowanceUsed
}

/// The two namespaces, as the caller can ask them.
///
/// Each resolves to `nil` when the name is not in that namespace, and throws
/// when the network is the problem — the difference matters, because one is an
/// answer and the other is no answer at all.
public struct NameLookups: Sendable {
    public let channel: @Sendable (String) async throws -> String?
    public let person: @Sendable (String) async throws -> String?

    public init(
        channel: @escaping @Sendable (String) async throws -> String?,
        person: @escaping @Sendable (String) async throws -> String?
    ) {
        self.channel = channel
        self.person = person
    }
}

public enum NameLookup {

    /// Whether this is worth spending a lookup on, by the shared rule.
    ///
    /// The predicate is the canonical username rule the discovery blinder
    /// itself enforces, so asking first is the difference between a quiet
    /// "type the whole name" and making a person spend a lookup token to be
    /// told no.
    public static func looksLikeAName(_ typed: String) -> Bool {
        LakkoCore.looksLikeAName(typed: typed)
    }

    /// Strip the leading `@` people type and no namespace stores.
    public static func bareName(_ typed: String) -> String {
        LakkoCore.bareName(typed: typed)
    }

    /// The label on the one lookup row: the whole name, never a prefix.
    public static func rowLabel(for typed: String) -> String {
        Localization.text("chats.lookup.action", texts: ["name": "@\(bareName(typed))"])
    }

    /// Ask both namespaces for one name.
    ///
    /// The channel goes first only because its answer is actionable on its own
    /// — subscribing needs nothing else — while a person leads into the contact
    /// request flow. Nothing about correctness depends on the order: a name
    /// cannot be in both namespaces, since they are separate registries.
    ///
    /// A network failure is ``FoundByName/unreachable`` rather than
    /// ``FoundByName/nothing``: telling someone their friend does not exist
    /// because the connection dropped is the worst thing this can do.
    public static func find(typed: String, lookups: NameLookups) async -> FoundByName {
        let name = bareName(typed)
        if !LakkoCore.looksLikeAName(typed: name) { return .nothing }
        var reachedTheServer = false
        var allowanceUsed = false
        do {
            if let channelIdHex = try await lookups.channel(name) {
                return .channel(channelIdHex: channelIdHex, handle: name)
            }
            reachedTheServer = true
        } catch {
            // Fall through: the other namespace may still answer.
            if AnonTokenUnavailable.isQuotaExhausted(error) { allowanceUsed = true }
        }
        do {
            if let username = try await lookups.person(name) {
                return .person(username: username)
            }
            reachedTheServer = true
        } catch {
            // Handled below.
            if AnonTokenUnavailable.isQuotaExhausted(error) { allowanceUsed = true }
        }
        // A namespace that was never asked cannot report "nothing".
        if allowanceUsed { return .allowanceUsed }
        return reachedTheServer ? .nothing : .unreachable
    }

    /// What to say about a result. One sentence on what happened, one on what
    /// to do, and never a protocol word.
    public static func message(for result: FoundByName, typed: String) -> String? {
        switch result {
        case .nothing:
            return Localization.text("chats.lookup.nothing", texts: ["name": "@\(bareName(typed))"])
        case .unreachable:
            return Localization.text("chats.lookup.unreachable")
        case .allowanceUsed:
            return AnonTokenCopy.searchAllowanceUsed
        case .channel, .person:
            return nil
        }
    }
}
