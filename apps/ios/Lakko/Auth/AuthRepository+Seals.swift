// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 10 and section 7 — the signed-in account's published
// protection and its owned-channels locks, as the session drives them.
//
// A device that holds the account keys publishes the account's seal
// announcement: at sign-up, at a landing when the account has none at its
// generation, after the password was proven on it, after a passkey was added
// or removed, after the phrase was saved, and after its keys changed. The
// same moments keep the owned-channels locks in step (the password lock
// follows the password, a passkey lock follows its passkey).

/// N (alignment 2026-11-27, the web rule): the password seal key and the
/// owned-channels password lock are made ONLY from an export key a password
/// produced in this run of the app (sign-up, a password sign-in, a password
/// change, a recall's new password), never from the one cached in the
/// Keychain, which may be a previous password's. Kept in memory only: a
/// relaunch forgets it, and the next password sign-in on a device with the
/// keys makes the seal and the lock. (Until 2026-11-27 a persisted "made
/// just now" mark stood here.)
@MainActor
enum RunExportKey {
    private struct Held {
        let accountIdB64: String
        let exportKey: Data
        /// Neither the announcement nor the owned-channels lock took it yet.
        var pending: Bool
    }

    private static var held: Held?

    /// A password just produced `exportKey` for `accountIdB64`.
    static func note(accountIdB64: String, exportKey: Data) {
        guard exportKey.count == 64 else { return }
        held = Held(accountIdB64: accountIdB64, exportKey: exportKey, pending: true)
    }

    /// The export key a password produced in this run for `accountIdB64`.
    static func get(accountIdB64: String) -> Data? {
        held.flatMap { $0.accountIdB64 == accountIdB64 ? $0.exportKey : nil }
    }

    /// Whether the announcement or the lock still owes it.
    static func isPending(accountIdB64: String) -> Bool {
        held.map { $0.accountIdB64 == accountIdB64 && $0.pending } ?? false
    }

    /// Both took it; it stays for later changes in this run.
    static func taken(accountIdB64: String) {
        guard held?.accountIdB64 == accountIdB64 else { return }
        held?.pending = false
    }

    /// The account left this device.
    static func clear(accountIdB64: String) {
        if held?.accountIdB64 == accountIdB64 { held = nil }
    }

    static func clearAll() { held = nil }
}

/// Which change a landing looks at the announcement with.
enum LandingSealChange {
    /// A password proven on this device since the last look replaces the
    /// announced password seal; otherwise the landing only fills in a
    /// missing announcement.
    static func decide(exportKeyFresh: Bool) -> SealChange {
        exportKeyFresh ? .password : .landing
    }
}

@MainActor
extension AuthRepository {

    /// Look at the signed-in account's seal announcement and publish the
    /// next one when `change` owes it (``SealAnnouncer``). `nil` when no
    /// session with keys is signed in, or the account is a guest (it has no
    /// username and no password of its own yet). Never throws.
    @discardableResult
    func ensureSealAnnouncement(
        _ change: SealChange, announcer: SealAnnouncer? = nil
    ) async -> SealAnnouncer.Result? {
        guard case .session(let account) = state, !account.username.isEmpty,
              let secrets = identitySecrets, base64Encode(secrets.accountId) == account.accountIdB64,
              let identity = try? Identity.fromSeed(seed: secrets.accountIdentitySeed) else { return nil }
        let accountIdB64 = account.accountIdB64
        var change = change
        let fresh = RunExportKey.isPending(accountIdB64: accountIdB64)
        if change == .landing { change = LandingSealChange.decide(exportKeyFresh: fresh) }
        // N — only an export key a password produced in this run.
        let exportKey = RunExportKey.get(accountIdB64: accountIdB64)
        // The sign-up's server half is still pending: the list and its
        // locks are made now (the device holds the export key of the
        // password the account has).
        if let pending = OwnedChannelsPendingSecret.get(accountIdB64: accountIdB64), let exportKey {
            await OwnedChannels.shared.create(
                accountIdB64: accountIdB64, currentSeed: secrets.accountIdentitySeed, exportKey: exportKey,
                serverHalf: pending)
        }
        let phrase = heldAccountPhrase().flatMap { try? RecoveryMnemonic.fromPhrase(phrase: $0) }
        let announcer = announcer ?? SealAnnouncer(ports: .live())
        let sealsStore = AccountSealsStore()
        func look(_ change: SealChange) async -> SealAnnouncer.Result {
            await announcer.ensure(
                accountId: secrets.accountId, accountIdB64: accountIdB64, identity: identity,
                keyGeneration: secrets.session.keyGeneration, sessionToken: secrets.sessionToken,
                exportKey: exportKey, phrase: phrase, change: change)
        }
        var result = await look(change)
        var announced = false
        if case .published = result { announced = true }
        if case .unchanged = result { announced = true }
        // The new words of a reset or a rotation were confirmed here
        // (``newWordsConfirmed(accountIdB64:)``) and are not listed yet:
        // they are, now that an announcement stands at this generation.
        if announced, change != .phraseSaved, sealsStore.phraseSealOwed(accountIdB64: accountIdB64) {
            let second = await look(.phraseSaved)
            if case .published = second { result = second }
            if case .unchanged = second { result = second }
        }
        switch result {
        case .published(let seals), .unchanged(let seals):
            // The announcement is the one place a saved phrase is recorded.
            // This device's own note of it is only ever raised from there:
            // after a reset or a rotation the new words are still to be
            // confirmed (they are listed once they are), and the note is
            // what shows them. (A recall writes the note itself; the account
            // wipe removes it.)
            if seals.phraseSaved {
                protectionStore.setPhraseSaved(true, accountIdB64: accountIdB64)
                sealsStore.setPhraseSealOwed(false, accountIdB64: accountIdB64)
            }
        case .failed, .noPasswordSeal:
            break
        }
        // The owned-channels password lock follows the password the same
        // way. The run's export key stays pending until both took it.
        if fresh, let exportKey {
            let locked = await OwnedChannels.shared.passwordProven(
                accountIdB64: accountIdB64, currentSeed: secrets.accountIdentitySeed, exportKey: exportKey)
            if announced, locked, change == .password {
                RunExportKey.taken(accountIdB64: accountIdB64)
            }
        }
        // A passkey's lock follows its passkey (section 7): made when the
        // passkey gave its secret, removed with the passkey.
        switch change {
        case .passkeyAdded(let credentialId, let prfOutput):
            await OwnedChannels.shared.passkeyAdded(
                accountIdB64: accountIdB64, currentSeed: secrets.accountIdentitySeed,
                credentialId: credentialId, prfOutput: prfOutput)
        case .passkeyRemoved(let credentialId):
            await OwnedChannels.shared.passkeyRemoved(
                accountIdB64: accountIdB64, currentSeed: secrets.accountIdentitySeed,
                credentialId: credentialId)
        default:
            break
        }
        return result
    }
}
