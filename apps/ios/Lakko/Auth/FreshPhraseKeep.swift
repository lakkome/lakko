// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// A new recovery phrase written to its account's phrase row BEFORE the
/// server step that makes it the account's only way back (a key reset, a
/// recall that takes the account back), and taken out again when that step
/// does not happen (ADR-0077 decision 3, rule 1).
///
/// The order is the point. Written after the server step, a failed write, a
/// killed process or a session that changed underneath leaves an account
/// whose new phrase exists nowhere. Written before, the worst case is a row
/// naming a phrase the server never learned of, and ``undo()`` removes
/// exactly that.
///
/// Mirror of Android `auth/StagedPhrase.kt`.
final class FreshPhraseKeep {

    /// One account's phrase row (production: the Keychain ``PhraseStore``).
    struct Ports {
        /// The stored bytes, or `nil` when the row is absent.
        var read: () throws -> Data?
        /// Throws when the bytes were not written (a Keychain failure, or a
        /// write the account store gate refused).
        var write: (Data) throws -> Void
        var delete: () throws -> Void
    }

    /// Thrown by ``keep(phrase:ports:)``-based routes when the phrase could
    /// not be written: nothing irreversible happened, and the route stops.
    struct NotStored: Error, Equatable {}

    private let ports: Ports
    private var phraseBytes: Data
    private var previous: Data?

    private init(ports: Ports, phraseBytes: Data, previous: Data?) {
        self.ports = ports
        self.phraseBytes = phraseBytes
        self.previous = previous
    }

    deinit {
        Self.zero(&phraseBytes)
        if var bytes = previous {
            Self.zero(&bytes)
        }
    }

    /// Remember what the row holds and write `phrase` over it.
    ///
    /// Throws ``NotStored`` when the row cannot be read or written; the row
    /// is then as it was, and the caller must not go on to the server step.
    static func keep(phrase: String, ports: Ports) throws -> FreshPhraseKeep {
        var bytes = Data(phrase.utf8)
        let previous: Data?
        do {
            // A row that cannot be read cannot be put back either: refuse
            // rather than overwrite something a rollback could not restore.
            previous = try ports.read()
        } catch {
            zero(&bytes)
            throw NotStored()
        }
        do {
            try ports.write(bytes)
        } catch {
            zero(&bytes)
            if var old = previous { zero(&old) }
            throw NotStored()
        }
        return FreshPhraseKeep(ports: ports, phraseBytes: bytes, previous: previous)
    }

    /// Whether the row holds this phrase now. A wipe in between removed it;
    /// a later write replaced it. Never throws.
    var isHeld: Bool {
        guard let held = try? ports.read() else { return false }
        return held == phraseBytes
    }

    /// The server step did not happen: put the row back as ``keep(phrase:ports:)``
    /// found it. Only while the row still holds THIS phrase; a row wiped or
    /// rewritten meanwhile is somebody else's decision and is left as it is.
    /// `false` when the row could not be put back. Never throws.
    @discardableResult
    func undo() -> Bool {
        guard isHeld else { return true }
        do {
            if let previous {
                try ports.write(previous)
            } else {
                try ports.delete()
            }
            return true
        } catch {
            NSLog("[auth] the recovery phrase row could not be put back after a refused server step")
            return false
        }
    }

    private static func zero(_ data: inout Data) {
        data.resetBytes(in: 0..<data.count)
    }
}

extension FreshPhraseKeep {

    /// Keep `phrase` in `accountIdB64`'s row of `store`, refused when the
    /// account was wiped since `since` was taken (the route's
    /// ``SignInKeyWrites`` mark): a write refused by the gate counts as not
    /// written. The row is replaced, so the bytes are the phrase as given; a
    /// caller passes the core's canonical form (a freshly generated phrase
    /// is one).
    static func keep(
        phrase: String,
        accountIdB64: String,
        since: AccountStoreGate.WipeMark,
        store: PhraseStore
    ) throws -> FreshPhraseKeep {
        try keep(phrase: phrase, ports: Ports(
            read: { try store.getPhrase(accountIdB64: accountIdB64) },
            write: { bytes in
                let hold = try AccountStoreGate.enterOrEnd(unlessWipedSince: since)
                defer { hold.leave() }
                try store.putPhrase(accountIdB64: accountIdB64, phraseBytes: bytes)
            },
            delete: { try store.deletePhrase(accountIdB64: accountIdB64) }))
    }
}
