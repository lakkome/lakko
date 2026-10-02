// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The mark that an account has a recovery phrase the person has not yet
/// acknowledged: the one a sign-up generated, or the one a guest account
/// (``GuestAccountClaim``) was made with and is shown when it is kept.
///
/// Mirror of the marker half of `apps/web/src/auth/phrase-gate.ts`.
///
/// The phrase screen comes before any other screen of the session, and the
/// phrase is shown only until it is acknowledged. Held in memory alone
/// (``AuthRepository/pendingSignupPhrase``), an app killed between the
/// sign-up and the acknowledgement would never show it. The mark is what a
/// relaunch reads: the phrase itself is NOT written here. It is already in
/// the Keychain phrase cache (``PhraseStore``) under the account's key, and a
/// relaunch reads it back from there. Only the fact that it is owed is kept
/// in plain storage.
struct SignupPhraseOwedStore {

    /// Key prefix; the base64 account id follows. The account wipe removes
    /// the row (``AccountLocalDataWipe``).
    static let keyPrefix = "lakko.signup-phrase-unacknowledged.v1."

    let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String {
        keyPrefix + accountIdB64
    }

    /// Record that `accountIdB64` owes the phrase screen.
    func mark(accountIdB64: String) {
        defaults.set(true, forKey: Self.key(accountIdB64: accountIdB64))
    }

    /// Whether a landing of `accountIdB64` still owes the phrase screen.
    func isOwed(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.key(accountIdB64: accountIdB64))
    }

    /// Clear the mark: on acknowledgement and on the account wipe.
    func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }
}
