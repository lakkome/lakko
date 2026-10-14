// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation

/// Discover's state on this device (ADR-0059, `docs/ux/feed.md` section 6),
/// sealed per account under a device-bound key like the saved feeds
/// (``FeedStore``). Mirror of web `discover/discover-store.ts`. Nothing here
/// leaves the device.
///
/// ```text
///   Application Support/lakko-discover/<accountIdB64>.prefs.bin
///   Application Support/lakko-discover/<accountIdB64>.shared.bin
///     12-byte IV || ciphertext || 16-byte tag (AES-256-GCM, Keychain wrap key)
/// ```
///
/// `prefs` holds the picked topics and language, the "Mix in random topics"
/// switch, the not-interested channels and hidden topics, the month's decoy
/// draw and the cache of fetched buckets (``DiscoverPrefs``). `shared` holds
/// the group-shared lists (``SharedChannels/Row``). Both are erased by
/// `forgetDevice`.
final class DiscoverStore {

    enum Kind: String {
        case prefs
        case shared
    }

    static let wrapKeyService: String = "me.lakko.discover-wrap.v1"
    private static let wrapKeyAccount: String = "wrap-key"
    private static let subdir: String = "lakko-discover"

    init() {}

    /// The row's plaintext, or `nil` when absent or not openable.
    func load(accountIdB64: String, kind: Kind) -> Data? {
        guard let url = fileURL(accountIdB64: accountIdB64, kind: kind),
              let combined = try? Data(contentsOf: url),
              let keyBytes = KeychainWrapKey.read(
                service: Self.wrapKeyService, account: Self.wrapKeyAccount),
              let sealed = try? AES.GCM.SealedBox(combined: combined),
              let plaintext = try? AES.GCM.open(sealed, using: SymmetricKey(data: keyBytes))
        else { return nil }
        return plaintext
    }

    /// Throws ``AccountStoreGate/Sealed`` once the account's session has
    /// ended: the Discover model and the shared-channels hub write after
    /// waits, under the account they read before them.
    func save(accountIdB64: String, kind: Kind, plaintext: Data) throws {
        let gate = try AccountStoreGate.enterOrEnd(accountIdB64)
        defer { gate.leave() }
        guard let url = fileURL(accountIdB64: accountIdB64, kind: kind) else {
            throw CocoaError(.fileWriteUnknown)
        }
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        guard let combined = try AES.GCM.seal(plaintext, using: wrapKey).combined else {
            throw CocoaError(.fileWriteUnknown)
        }
        let dir = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        SharedStorage.excludeFromBackup(dir)
        try combined.write(to: url, options: .atomic)
        SharedStorage.excludeFromBackup(url)
    }

    func loadPrefs(accountIdB64: String) -> DiscoverPrefs {
        guard let data = load(accountIdB64: accountIdB64, kind: .prefs) else { return DiscoverPrefs() }
        return DiscoverPrefs.decode(data)
    }

    func savePrefs(accountIdB64: String, prefs: DiscoverPrefs) throws {
        try save(accountIdB64: accountIdB64, kind: .prefs, plaintext: prefs.encode())
    }

    func loadShared(accountIdB64: String) -> SharedChannels.Row {
        guard let data = load(accountIdB64: accountIdB64, kind: .shared) else { return SharedChannels.Row() }
        return SharedChannels.decode(data)
    }

    func saveShared(accountIdB64: String, row: SharedChannels.Row) throws {
        try save(accountIdB64: accountIdB64, kind: .shared, plaintext: SharedChannels.encode(row))
    }

    /// Erase both rows of an account. Called from `forgetDevice`.
    func delete(accountIdB64: String) {
        for kind in [Kind.prefs, .shared] {
            if let url = fileURL(accountIdB64: accountIdB64, kind: kind) {
                try? FileManager.default.removeItem(at: url)
            }
        }
    }

    private func fileURL(accountIdB64: String, kind: Kind) -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true)
        else { return nil }
        let safe = accountIdB64.replacingOccurrences(of: "/", with: "_")
        return appSupport
            .appendingPathComponent(Self.subdir)
            .appendingPathComponent("\(safe).\(kind.rawValue).bin")
    }
}
