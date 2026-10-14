// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The key writes of ONE sign-in route, refused only when the account was
/// wiped after the route began.
///
/// A sign-in route (a password, a passkey, a recovery, the phrase prompt, a
/// pairing, a device restore, a key reset) writes the device's keys for an
/// account after one or more waits: the device root key, the export key, the
/// account identity seed, the device identity seed and its binding, the
/// recovery phrase. Forget device, Delete account and the other wipes run on
/// the main actor between those waits. A key written after such a wipe would
/// put the forgotten account's key back on the device.
///
/// The account's session is usually not landed yet when these writes run:
/// it may still be sealed by an earlier sign-out on this device, or by an
/// earlier wipe, and a sign-in after either must write. So the question is
/// not "is the account sealed?" but "was it wiped since this route began?"
/// (``AccountStoreGate/WipeMark``).
///
/// Take one at the START of the route, before its first wait, and hand it to
/// every helper that writes a key for the route. The marks of every account
/// are taken, so a route that learns its account id from the server's answer
/// is still measured from its start.
struct SignInKeyWrites {
    private let marks: AccountStoreGate.WipeMarks

    /// The marks as they stand now: call it when the route begins.
    init() {
        marks = AccountStoreGate.wipeMarks()
    }

    /// The mark of `accountIdB64` as of the route's start, for a store that
    /// takes one itself (``DevicePhrase/store(_:accountIdB64:context:since:store:threeWords:)``,
    /// ``BackupClient/cachePhrase(accountIdB64:phraseBytes:since:)``).
    func mark(_ accountIdB64: String) -> AccountStoreGate.WipeMark {
        marks.mark(accountIdB64)
    }

    /// `true` when a write for `accountIdB64` would be refused now.
    func isWiped(_ accountIdB64: String) -> Bool {
        guard let hold = AccountStoreGate.enter(unlessWipedSince: mark(accountIdB64)) else {
            return true
        }
        hold.leave()
        return false
    }

    /// A write the route must not go on without (the device root key):
    /// refused, it throws ``AccountStoreGate/Sealed``, a cancellation.
    func required<T>(_ accountIdB64: String, _ write: () throws -> T) throws -> T {
        let hold = try AccountStoreGate.enterOrEnd(unlessWipedSince: mark(accountIdB64))
        defer { hold.leave() }
        return try write()
    }

    /// A best-effort write: refused, it is not made and `false` comes back.
    /// An error the write itself throws is passed on.
    @discardableResult
    func ifNotWiped(_ accountIdB64: String, _ write: () throws -> Void) rethrows -> Bool {
        guard let hold = AccountStoreGate.enter(unlessWipedSince: mark(accountIdB64)) else {
            return false
        }
        defer { hold.leave() }
        try write()
        return true
    }
}
