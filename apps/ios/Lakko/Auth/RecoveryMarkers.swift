// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// What "Show recovery phrase" (Settings > Security > Recovery) does.
enum PhraseReveal: Equatable {
    /// The reset's publish is still owed: the new keys are not in the
    /// directory yet, so the words are withheld and `Try again` is offered
    /// (UX 15.4).
    case withheldCredentialPending
    /// Nothing is owed and the device holds the phrase: show it (after the
    /// device confirmed its owner).
    case show
    /// The device holds no phrase for this account.
    case notStored

    /// The rule. An owed publish withholds the words whether or not a
    /// phrase is stored; the phrase is shown only once nothing is owed.
    static func decide(replaceOwed: Bool, phraseStored: Bool) -> PhraseReveal {
        if replaceOwed { return .withheldCredentialPending }
        return phraseStored ? .show : .notStored
    }
}

/// Two debts a key reset can leave behind on this device, kept per account
/// in `UserDefaults` so they survive a relaunch. Mirror of the web
/// `key-reset.ts` publish record and the phrase-owed marker of
/// `phrase-gate.ts`.
///
///   * **Publish owed** (ADR-0065 section 2.3). "Reset account keys" resets
///     the keys in one server step; what can still be owed afterwards is
///     telling the directory, this device's KeyPackage row and the account's
///     DeviceGroup journal about the new keys. The record holds the
///     attestation the server minted and which steps landed, so a relaunch
///     retries the rest with the SAME attestation. The words are not shown
///     until it is cleared, the phrase screen offers `Try again`, and the
///     next session landing retries it in the background.
///   * **Phrase owed.** The new phrase was stored on this device but the
///     person has not yet confirmed writing it down. A session landing while
///     this is set shows the phrase first.
///
/// Both are cleared on Forget device, account deletion and the signed-out
/// wipe (``AccountLocalDataWipe``).
struct RecoveryMarkers {
    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func publishOwedKey(accountIdB64: String) -> String {
        "lakko.key-reset-publish-owed.v1.\(accountIdB64)"
    }

    static func phraseOwedKey(accountIdB64: String) -> String {
        "lakko.recovery-phrase-owed.v1.\(accountIdB64)"
    }

    /// The owed publish, or `nil` when nothing is owed (or the stored
    /// record is unreadable, which is logged and read as nothing owed: a
    /// record that cannot be read cannot be retried either).
    func publishOwed(accountIdB64: String) -> KeyResetPublishRecord? {
        guard let data = defaults.data(forKey: Self.publishOwedKey(accountIdB64: accountIdB64)) else {
            return nil
        }
        do {
            return try JSONDecoder().decode(KeyResetPublishRecord.self, from: data)
        } catch {
            NSLog("[key-reset] WARN the owed-publish record is unreadable; dropping it")
            clearPublishOwed(accountIdB64: accountIdB64)
            return nil
        }
    }

    func isPublishOwed(accountIdB64: String) -> Bool {
        publishOwed(accountIdB64: accountIdB64) != nil
    }

    /// Store (or replace) the owed publish.
    func markPublishOwed(_ record: KeyResetPublishRecord, accountIdB64: String) {
        do {
            defaults.set(try JSONEncoder().encode(record), forKey: Self.publishOwedKey(accountIdB64: accountIdB64))
        } catch {
            NSLog("[key-reset] ERR the owed-publish record could not be stored: %@", String(describing: error))
        }
    }

    func clearPublishOwed(accountIdB64: String) {
        defaults.removeObject(forKey: Self.publishOwedKey(accountIdB64: accountIdB64))
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
        clearPublishOwed(accountIdB64: accountIdB64)
        clearPhraseOwed(accountIdB64: accountIdB64)
    }
}
