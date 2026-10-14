// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The recovery phrase a device holds for an account.
///
/// Every device that holds an account's keys also holds its recovery phrase
/// (``PhraseStore``, a this-device-only Keychain row). The phrase is what the
/// local three-word check compares against, what the hand-over to a new
/// device carries (as its 16-byte entropy), what "Show recovery phrase"
/// reveals, and what the encrypted backup is sealed under.
///
/// Mirror of `apps/web/src/backup/device-phrase.ts`. The phrase is written on
/// every landing path that gives the device keys: signup, guest signup,
/// phrase + new password recovery, a phrase typed in the restore prompt (which
/// also covers a passkey restore), another-device pairing, another-device
/// restore, and a fresh identity (after its publish). It is always the core's
/// canonical form, and it is written only after the published-identity check
/// where one applies.
enum DevicePhrase {

    /// The phrase stored for `accountIdB64`, or `nil` when none is stored,
    /// the Keychain cannot be read, or the stored bytes are not a phrase.
    static func read(accountIdB64: String, store: PhraseStore = PhraseStore()) -> String? {
        guard let bytes = try? store.getPhrase(accountIdB64: accountIdB64),
              let phrase = String(data: bytes, encoding: .utf8),
              !phrase.isEmpty else {
            return nil
        }
        return phrase
    }

    /// Whether a phrase is stored for `accountIdB64`. A read, not a reveal.
    static func isStored(accountIdB64: String, store: PhraseStore = PhraseStore()) -> Bool {
        read(accountIdB64: accountIdB64, store: store) != nil
    }

    /// Store `phrase` for `accountIdB64` in the core's canonical form.
    /// Best-effort: a phrase the core refuses, or a Keychain failure, is
    /// logged (by `context`, never with the words) and leaves the store as
    /// it was. A successful write lifts a "three words suspended" marker:
    /// every caller is a landing path that just proved the phrase.
    ///
    /// `since` is the account's ``AccountStoreGate/WipeMark`` from when the
    /// caller's route began (a sign-in, whose session may not have landed
    /// yet, ``SignInKeyWrites``): the phrase is then refused only if the
    /// account was wiped after that. `nil` is for a signed-in session, which
    /// is refused once the account was wiped and not landed again. Refused,
    /// nothing is written and nothing is logged as a failure.
    static func store(
        _ phrase: String,
        accountIdB64: String,
        context: String,
        since: AccountStoreGate.WipeMark?,
        store: PhraseStore = PhraseStore(),
        threeWords: ThreeWordsBudgetStore = ThreeWordsBudgetStore()
    ) {
        let hold = since.map { AccountStoreGate.enter(unlessWipedSince: $0) }
            ?? AccountStoreGate.enterUnlessWiped(accountIdB64)
        guard let hold else { return }
        defer { hold.leave() }
        do {
            let canonical = try RecoveryMnemonic.fromPhrase(phrase: phrase).phrase()
            try store.putPhrase(accountIdB64: accountIdB64, phraseBytes: Data(canonical.utf8))
            threeWords.clearSuspended(accountIdB64: accountIdB64)
        } catch {
            NSLog("recovery phrase not stored on this device (\(context))")
        }
    }

    /// The 16-byte entropy of the stored phrase (what the hand-over to a new
    /// device carries), or `nil` when no usable phrase is stored.
    static func entropy(accountIdB64: String, store: PhraseStore = PhraseStore()) -> Data? {
        guard let phrase = read(accountIdB64: accountIdB64, store: store),
              let mnemonic = try? RecoveryMnemonic.fromPhrase(phrase: phrase) else {
            return nil
        }
        return mnemonic.toEntropy()
    }
}
