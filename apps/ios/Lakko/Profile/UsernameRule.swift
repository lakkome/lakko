// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0048 §4 — the username character rule, as the forms apply it.
///
/// The rule itself lives in the shared Rust core (`validateUsername`, and the
/// OPRF blinder refuses the same names), so nothing here re-implements the
/// character table. What this file owns is the UI half: turning the core's
/// refusal into the UX spec's reason copy, and the one decision every search
/// field shares — a lookup is never sent for a name that fails the rule.
///
/// The reason is read off the typed `FfiError` variant, never off its message
/// text: the core's message deliberately never contains the input, and
/// matching prose is the drift class the typed variants exist to remove.
public enum UsernameRuleReason: Equatable, Sendable {
    /// Contains something other than Latin letters, digits and `_`.
    case invalidCharacters
    /// Fewer than 3 characters after normalisation.
    case tooShort
    /// More than 32 characters after normalisation.
    case tooLong

    /// The UX spec §4 reason copy, in the interface language (ADR-0061).
    public var message: String {
        switch self {
        case .invalidCharacters: return Localization.text("username.error.invalid_characters")
        case .tooShort: return Localization.text("username.error.too_short", counts: ["count": 3])
        case .tooLong: return Localization.text("username.error.too_long", counts: ["count": 32])
        }
    }
}

/// What a username field holds right now.
public enum UsernameCheck: Equatable, Sendable {
    /// Nothing typed (or only `@` / whitespace).
    case empty
    /// Passes the rule; `canonical` is the normalised form (lowercase, no `@`).
    case valid(canonical: String)
    /// Fails the rule, with the reason to show.
    case invalid(UsernameRuleReason)

    /// `true` only for a name a lookup / claim may be sent for.
    public var allowsLookup: Bool {
        if case .valid = self { return true }
        return false
    }

    /// The canonical name when valid.
    public var canonical: String? {
        if case .valid(let canonical) = self { return canonical }
        return nil
    }

    /// The inline message a field shows while the user types: the reason for
    /// an invalid name, nothing for an empty or valid one (an empty field is
    /// not yet an error — the submit handler says "Type a username first").
    public var inlineMessage: String? {
        if case .invalid(let reason) = self { return reason.message }
        return nil
    }
}

/// Classify `input` against the ADR-0048 §4 rule.
///
/// `validate` is the core's `validateUsername` in production; tests inject a
/// stand-in that throws the real `FfiError` variants so the mapping below is
/// what they exercise.
///
/// Fail-closed: an error that is not one of the four username variants is
/// treated as an invalid name, so an unexpected core refusal never lets a
/// lookup through.
public func checkUsername(
    _ input: String,
    validate: (String) throws -> String = { try validateUsername(input: $0) }
) -> UsernameCheck {
    do {
        return .valid(canonical: try validate(input))
    } catch let error as FfiError {
        switch error {
        case .UsernameEmpty: return .empty
        case .UsernameTooShort: return .invalid(.tooShort)
        case .UsernameTooLong: return .invalid(.tooLong)
        case .UsernameInvalidCharacters: return .invalid(.invalidCharacters)
        default: return .invalid(.invalidCharacters)
        }
    } catch {
        return .invalid(.invalidCharacters)
    }
}

/// A username a form refused before any network call, carrying the reason
/// copy. Thrown by the auth flows and the search fields so the caller never
/// reaches discovery with a non-conforming name.
public struct UsernameRuleViolation: Error, LocalizedError, Equatable {
    public let reason: UsernameRuleReason

    public init(reason: UsernameRuleReason) {
        self.reason = reason
    }

    public var errorDescription: String? { reason.message }
}

/// The canonical form of `input`, or a throw with the reason to show.
///
/// `emptyError` is what an empty field raises, so each call site keeps the
/// wording it already had for "nothing typed".
func requireValidUsername(
    _ input: String,
    emptyError: @autoclosure () -> Error,
    validate: (String) throws -> String = { try validateUsername(input: $0) }
) throws -> String {
    switch checkUsername(input, validate: validate) {
    case .valid(let canonical):
        return canonical
    case .empty:
        throw emptyError()
    case .invalid(let reason):
        throw UsernameRuleViolation(reason: reason)
    }
}
