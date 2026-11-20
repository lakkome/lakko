// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// Learning the username after a sign-in that named none (a passkey sign-in
// on a new device needs no name typed). Without the name the device has no
// login handle: it cannot knock on the account's DeviceGroup (ADR-0081) and
// Settings shows no username. Same rule on web, iOS and Android:
//
//   1. a name the restored backup carries (the own profile envelope, the
//      own-username register) is checked with auth and adopted silently;
//   2. else the person is asked (``UsernamePromptSheet``);
//   3. a candidate is checked with `POST /auth/login-handle/check`, which
//      answers for THIS account only (`same` / `other` / `none`).
//
// The flow over the network is `AuthRepository.learnUsernameAfterLanding`
// and `AuthRepository.submitPromptedUsername(_:)`. Neither logs the name or
// its handle.

/// The pure rules of the username prompt.
enum UsernamePrompt {

    /// What checking one candidate came to.
    enum Outcome: Equatable {
        /// Auth answered `same`: the name is the account's, adopted.
        case adopted(String)
        /// Auth answered `other`: the account is bound to another name.
        case notThisAccount
        /// Auth answered `none`: the account has no username at all.
        case accountHasNone
        /// `429`: the check's own limit.
        case tooMany
        /// No answer, or any other refusal.
        case network
        /// The name fails the username rule; nothing was sent.
        case rule(UsernameRuleReason)
        /// Nothing typed; nothing was sent.
        case empty
    }

    /// Whether a landed session with `username` should learn its name:
    /// an empty username on an account that is not a guest (a guest of a
    /// call link has no name by design).
    static func shouldLearn(username: String, isGuest: Bool) -> Bool {
        !isGuest && username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// The names the backup gave, in the order they are tried: the own
    /// profile envelope's username, then the own-username register. Empty
    /// and repeated names are dropped.
    static func backupCandidates(profileUsername: String?, register: String?) -> [String] {
        var out: [String] = []
        for name in [profileUsername, register] {
            guard let trimmed = name?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !trimmed.isEmpty, !out.contains(trimmed) else { continue }
            out.append(trimmed)
        }
        return out
    }

    /// The outcome of auth's answer for a name that passed the rule.
    static func outcome(of state: LoginHandleState, canonical: String) -> Outcome {
        switch state {
        case .same: return .adopted(canonical)
        case .other: return .notThisAccount
        case .none: return .accountHasNone
        }
    }

    /// The outcome of a check that threw.
    static func outcome(of error: Error) -> Outcome {
        if case .http(let status, _, _) = error as? AuthClientError, status == 429 { return .tooMany }
        return .network
    }

    /// Whether the prompt closes after `outcome`: the name was adopted, or
    /// the account has no name to learn.
    static func closesPrompt(_ outcome: Outcome) -> Bool {
        switch outcome {
        case .adopted, .accountHasNone: return true
        case .notThisAccount, .tooMany, .network, .rule, .empty: return false
        }
    }

    /// The error line the prompt shows for `outcome`, as a catalog key, or
    /// `nil` (closed, or a rule refusal, whose copy is the username rule's
    /// own: ``UsernameRuleReason/message``).
    static func errorKey(_ outcome: Outcome) -> String? {
        switch outcome {
        case .notThisAccount: return "auth.username_prompt.error.not_this"
        case .tooMany: return "auth.username_prompt.error.too_many"
        case .network: return "auth.username_prompt.error.network"
        case .adopted, .accountHasNone, .rule, .empty: return nil
        }
    }
}

/// Nothing was typed in the username prompt.
struct UsernamePromptEmpty: Error, Equatable {}
