// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Resolve which account a recover-from-words flow should target.
///
/// Extracted from ``AuthRepository/recover(mnemonicPhrase:newPassword:username:)``
/// (G-29) as a pure, dependency-injected function so the cross-device
/// behaviour — a fresh device recovers given the username + phrase — is
/// locked by a unit test instead of living inlined in a large async
/// method where it can silently regress.
///
/// The rule (identical on web / iOS / Android `recover`): a local
/// `(accountId, username)` cache hit wins when the typed username is
/// empty or matches it; otherwise a discovery lookup on the typed
/// username binds the account. With neither we cannot target the
/// recover. A lookup miss collapses into the same error a bad phrase
/// yields downstream — we never confirm or deny a username's existence
/// (preserves the no-probing property).
///
/// Mirror of `apps/web/src/auth/recover-resolution.ts`'s
/// `resolveRecoverAccount`.

/// Minimal shape of a locally-cached `(accountId, username)` record.
struct CachedAccount {
    let accountIdB64: String
    let username: String
}

/// Minimal shape of a discovery account lookup result.
struct RecoverAccountLookup {
    /// Raw 16-byte account id, or `nil` when the username is unregistered.
    let account: Data?
    /// Server-normalised form of the looked-up username.
    let normalisedUsername: String
}

/// The resolved account the recover targets.
struct ResolvedRecoverAccount: Equatable {
    let accountIdB64: String
    let resolvedUsername: String
}

/// Resolve the account to recover.
///
/// - Parameters:
///   - cached: the locally-cached `(accountId, username)` record, or `nil`.
///   - username: the typed username (may be `nil`, empty, or whitespace-padded).
///   - lookup: a discovery lookup on the typed username (injected so the
///     helper is unit-testable without a network).
///   - encodeAccountId: encodes the raw looked-up account bytes to base64.
/// - Throws: ``AuthClientError/argument(_:)`` `"invalid recovery"` when the
///   typed username is unregistered (collapsed with the downstream
///   bad-phrase rejection — no username probing), or a fresh-device prompt
///   when neither a cached account nor a typed username is available.
func resolveRecoverAccount(
    cached: CachedAccount?,
    username: String?,
    lookup: (String) async throws -> RecoverAccountLookup,
    encodeAccountId: (Data) -> String
) async throws -> ResolvedRecoverAccount {
    let trimmedUsername = username?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

    // A local cache hit wins when the typed username is empty or matches it.
    if let cached,
       trimmedUsername.isEmpty || cached.username == trimmedUsername {
        return ResolvedRecoverAccount(
            accountIdB64: cached.accountIdB64,
            resolvedUsername: cached.username
        )
    }

    // Otherwise resolve the typed username via discovery so a FRESH device
    // can recover given username + phrase.
    if !trimmedUsername.isEmpty {
        let result = try await lookup(trimmedUsername)
        guard let accountBytes = result.account else {
            // Collapsed with "bad phrase" downstream by the OPAQUE
            // rejection — we don't help probe the username space.
            throw AuthClientError.argument("invalid recovery")
        }
        return ResolvedRecoverAccount(
            accountIdB64: encodeAccountId(accountBytes),
            resolvedUsername: result.normalisedUsername
        )
    }

    throw AuthClientError.argument(
        "no account on this device — enter your username so we " +
        "can look up the account to recover"
    )
}
