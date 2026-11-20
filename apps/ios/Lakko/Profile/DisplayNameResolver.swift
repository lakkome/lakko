// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0048 §5 / UX spec §3 — the ONE display-name resolver.
///
/// Every surface that names a person asks this, so the precedence cannot drift
/// between the contacts list, a chat header, a group author label and a call
/// tile:
///
///   1. a name the viewer gave this contact themselves (local rename),
///   2. the person's profile display name,
///   3. the person's VERIFIED `@username`,
///   4. a neutral placeholder (``ResolvedDisplayName/placeholderMember``); a
///      request row says ``ResolvedDisplayName/placeholderSomeone`` instead.
///
/// The key fingerprint is NEVER a name (docs/ux/client-redesign.md section 2,
/// principle 4). It stays reachable on ``ResolvedDisplayName/fingerprint`` for
/// the safety-number and debug surfaces only.
///
/// A username is part of the result only when verified. An unverified claim is
/// not greyed out or question-marked; it is simply absent, because a displayed
/// `@pavel` that is not Pavel is the impersonation this must not enable.
public struct ResolvedDisplayName: Equatable, Sendable {
    /// Which rung of the precedence produced ``name``.
    public enum Source: Equatable, Sendable {
        case localRename
        case profileName
        case verifiedUsername
        /// Nothing is known about this person beyond their key; ``name`` is
        /// the neutral placeholder.
        case placeholder
        /// ADR-0098 — the person's account was deleted; ``name`` is
        /// ``deletedName`` whatever else is known about them.
        case deletedAccount
    }

    /// ADR-0098 — the name of a person whose account was deleted, in the
    /// interface language when it is read.
    public static var deletedName: String { Localization.text("person.deleted.name") }

    /// ADR-0098 — the one line shown where a deleted person can no longer be
    /// written to.
    public static var deletedBody: String { Localization.text("person.deleted.body") }

    /// The result for a deleted account: the name only, no username, no
    /// line about their own name.
    public static func deletedAccount(signatureKey: Data) -> ResolvedDisplayName {
        ResolvedDisplayName(
            name: deletedName, source: .deletedAccount, verifiedUsername: nil,
            fingerprint: displayFingerprint(signatureKey), theirName: nil, usernameNotVerified: false)
    }

    /// `true` for a person whose account was deleted.
    public var isDeletedAccount: Bool { source == .deletedAccount }

    /// The roster / sender label for a person nothing is known about, in the
    /// interface language when it is read (ADR-0061).
    public static var placeholderMember: String { Localization.text("chat.members.unnamed") }
    /// The request-row label for a person nothing is known about, in the
    /// interface language when it is read.
    public static var placeholderSomeone: String { Localization.text("contacts.request.someone") }

    /// The name to show. Chat bubbles and call tiles show only this.
    public let name: String
    public let source: Source
    /// The verified username in canonical form (no `@`), or `nil`.
    public let verifiedUsername: String?
    /// The short key fingerprint, `a1b2 c3d4`. Never shown as a name; for the
    /// safety-number and debug surfaces.
    public let fingerprint: String
    /// The person's own profile name when a local rename overrides it — the
    /// contact detail's `Their name: …` line.
    public let theirName: String?
    /// The person claims a username that did not verify (mismatch, or a lookup
    /// that failed). Contact detail shows a quiet "Username not verified"; no
    /// other surface shows anything.
    public let usernameNotVerified: Bool

    /// The label for rows with room for a second part: `Alice Petrova ·
    /// @alice` when a verified username sits under a name, otherwise just the
    /// name (which is itself `@alice` when the username is the best we have).
    public var listLabel: String {
        guard let username = verifiedUsername, source != .verifiedUsername else { return name }
        return "\(name) · @\(username)"
    }

    /// `@alice` for a second line under the name, or `nil` when the name
    /// already is the username or there is no verified username.
    public var usernameLine: String? {
        guard let username = verifiedUsername, source != .verifiedUsername else { return nil }
        return "@\(username)"
    }

    /// `true` when ``name`` is the neutral placeholder.
    public var isPlaceholder: Bool { source == .placeholder }

    /// The label for the sender of a request: the list label, or
    /// ``placeholderSomeone`` when nothing is known about them.
    public var requestSenderLabel: String {
        isPlaceholder ? Self.placeholderSomeone : listLabel
    }
}

/// Resolve the name for the person holding `signatureKey`.
///
/// - Parameters:
///   - localRename: the viewer's own name for this contact, if any.
///   - profile: the stored profile bound to this key (``ReceivedProfileStore``).
///   - verification: the verdict for `(key, profile.username)`, `nil` while
///     not yet verified.
public func resolveDisplayName(
    signatureKey: Data,
    localRename: String?,
    profile: ProfileClaim?,
    verification: UsernameVerificationStateView?
) -> ResolvedDisplayName {
    let fingerprint = displayFingerprint(signatureKey)
    let rename = nonBlank(localRename)
    let profileName = nonBlank(profile?.displayName)
    let claimedUsername = nonBlank(profile?.username)
    let verifiedUsername = (claimedUsername != nil && verification == .verified) ? claimedUsername : nil
    let notVerified = claimedUsername != nil && verification == .refused

    let name: String
    let source: ResolvedDisplayName.Source
    if let rename {
        name = rename
        source = .localRename
    } else if let profileName {
        name = profileName
        source = .profileName
    } else if let verifiedUsername {
        name = "@\(verifiedUsername)"
        source = .verifiedUsername
    } else {
        name = ResolvedDisplayName.placeholderMember
        source = .placeholder
    }
    return ResolvedDisplayName(
        name: name,
        source: source,
        verifiedUsername: verifiedUsername,
        fingerprint: fingerprint,
        theirName: rename != nil ? profileName : nil,
        usernameNotVerified: notVerified
    )
}

/// The resolver's view of a verification verdict, collapsed to the two answers
/// precedence cares about (`nil` is the third: not yet verified), so the
/// resolver stays independent of how the verifier stores failures.
public enum UsernameVerificationStateView: Equatable, Sendable {
    case verified
    case refused
}

extension UsernameVerificationStateView {
    /// `nil` for no verdict yet and for a check that could not complete: both
    /// read as a check still running.
    init?(_ state: UsernameVerificationState?) {
        switch state {
        case .verified?: self = .verified
        case .mismatch?: self = .refused
        case .failed?, nil: return nil
        }
    }
}

/// The short key fingerprint, the first four bytes as lowercase hex in two
/// groups, `a1b2 c3d4`. For the safety-number and debug surfaces; never a
/// name. Deliberately no `@` — it is not a username and must not read like one.
public func displayFingerprint(_ signatureKey: Data) -> String {
    let hex = signatureKey.prefix(4).map { String(format: "%02x", $0) }.joined()
    guard hex.count > 4 else { return hex }
    let split = hex.index(hex.startIndex, offsetBy: 4)
    return "\(hex[..<split]) \(hex[split...])"
}

private func nonBlank(_ value: String?) -> String? {
    guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else {
        return nil
    }
    return trimmed
}
