// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// What "Show recovery phrase" (Settings > Security > Recovery) does.
enum PhraseReveal: Equatable {
    /// A recovery-credential replace is owed: the words would not sign in
    /// yet, so they are withheld and `Try again` is offered.
    case withheldCredentialPending
    /// Nothing is owed and the device holds the phrase: show it (after the
    /// device confirmed its owner).
    case show
    /// The device holds no phrase for this account.
    case notStored

    /// The rule. An owed replace withholds the words whether or not a
    /// phrase is stored; the phrase is shown only once nothing is owed.
    static func decide(replaceOwed: Bool, phraseStored: Bool) -> PhraseReveal {
        if replaceOwed { return .withheldCredentialPending }
        return phraseStored ? .show : .notStored
    }
}

/// Two debts a fresh identity can leave behind on this device, kept per
/// account in `UserDefaults` so they survive a relaunch. Mirror of the web
/// `recovery-credential-owed.ts` and the phrase-owed marker of
/// `phrase-gate.ts`.
///
///   * **Recovery credential owed.** "Start over with a new identity"
///     publishes the new identity first and then replaces the server's
///     recovery credential with one made from the NEW seed, so the new 12
///     words sign in and the old ones stop. When that replace fails the debt
///     is recorded here: the words are not shown until it succeeds, the
///     phrase screen offers `Try again`, and the next session landing retries
///     it in the background. The marker is set only after the new seed was
///     stored on this device.
///   * **Phrase owed.** The new phrase was stored on this device but the
///     person has not yet confirmed writing it down. A session landing while
///     this is set shows the phrase first.
///
/// Both are cleared on Forget device and account deletion
/// (``AccountLocalDataWipe``).
struct RecoveryMarkers {
    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func credentialOwedKey(accountIdB64: String) -> String {
        "lakko.recovery-credential-owed.v1.\(accountIdB64)"
    }

    static func phraseOwedKey(accountIdB64: String) -> String {
        "lakko.recovery-phrase-owed.v1.\(accountIdB64)"
    }

    func isCredentialOwed(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.credentialOwedKey(accountIdB64: accountIdB64))
    }

    func markCredentialOwed(accountIdB64: String) {
        defaults.set(true, forKey: Self.credentialOwedKey(accountIdB64: accountIdB64))
    }

    func clearCredentialOwed(accountIdB64: String) {
        defaults.removeObject(forKey: Self.credentialOwedKey(accountIdB64: accountIdB64))
    }

    func isPhraseOwed(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.phraseOwedKey(accountIdB64: accountIdB64))
    }

    func markPhraseOwed(accountIdB64: String) {
        defaults.set(true, forKey: Self.phraseOwedKey(accountIdB64: accountIdB64))
    }

    func clearPhraseOwed(accountIdB64: String) {
        defaults.removeObject(forKey: Self.phraseOwedKey(accountIdB64: accountIdB64))
    }

    /// Drop both markers of `accountIdB64`.
    func clear(accountIdB64: String) {
        clearCredentialOwed(accountIdB64: accountIdB64)
        clearPhraseOwed(accountIdB64: accountIdB64)
    }
}
