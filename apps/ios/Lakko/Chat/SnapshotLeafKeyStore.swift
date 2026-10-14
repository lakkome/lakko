// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0050 — records which leaf identity the on-disk MLS snapshot of an
/// account was taken under, as base64 of the leaf signature key.
///
/// iOS mirror of the `leafKeyB64` field on the web `mls-store` row. Kept as a
/// sidecar rather than inside ``IdentitySnapshotStore``'s encrypted envelope:
/// the value is public (a signature public key) and the bootstrap needs it
/// before deciding whether to restore at all. A snapshot whose recorded leaf
/// key differs from the identity about to restore it (every pre-ADR-0050
/// snapshot was taken under the ACCOUNT identity) is not restored.
public struct SnapshotLeafKeyStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = SharedStorage.sharedDefaults() ?? .standard) {
        self.defaults = defaults
    }

    public func load(accountIdB64: String) -> String? {
        defaults.string(forKey: Self.key(accountIdB64))
    }

    public func save(accountIdB64: String, leafKeyB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(leafKeyB64, forKey: Self.key(accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }

    private static func key(_ accountIdB64: String) -> String {
        "me.lakko.snapshot-leaf-key.v1." + accountIdB64
    }
}
