// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation

/// ADR-0089 section 6 — the account's blocked people, mirrored into the App
/// Group for the notification extension (ADR-0041).
///
/// The block register lives in the account's synced state, which the
/// extension may not open (C-59). A notification of anything of a blocked
/// person's is not shown on any path (UX `reporting-and-moderation.md` 1.2,
/// `blockVerdict(.notification)` is `drop`), so the app writes the blocked
/// account keys here whenever its blocked set changes and at every launch,
/// and the extension only READS them, dropping a row from such a sender the
/// way it drops a row from a key an accepted reset retired.
///
/// Who someone blocked is private (security fix F9): the set is stored as
/// one AES-GCM sealed file per account in the App Group container, under
/// the same shared Keychain wrap key the message log is sealed under
/// (``MessageLogStore/wrapKeyService``, readable by the extension through
/// the shared access group, ADR-0041), with the account id as associated
/// data so one account's file never opens as another's. The file is
/// excluded from backup. The extension only reads the key and never
/// creates one.
///
/// Foundation + CryptoKit only (compiled into both targets). One file per
/// account, removed with the account's data.
public enum BlockedSenderMirror {

    /// Where the sealed files live and the key they are sealed under;
    /// injectable for tests.
    public struct Store {
        /// The directory holding the per-account files, or `nil` (then
        /// nothing is written and everything reads as none).
        public var directory: () -> URL?
        /// The key a write seals under (created when absent: the app only).
        public var sealKey: () -> SymmetricKey?
        /// The key a read opens with (never created).
        public var openKey: () -> SymmetricKey?

        public init(
            directory: @escaping () -> URL?,
            sealKey: @escaping () -> SymmetricKey?,
            openKey: @escaping () -> SymmetricKey?
        ) {
            self.directory = directory
            self.sealKey = sealKey
            self.openKey = openKey
        }

        /// The App Group container and the message log's shared wrap key.
        public static var live: Store {
            Store(
                directory: { SharedStorage.containerURL()?.appendingPathComponent(subdirectory) },
                sealKey: {
                    SymmetricKey(data: KeychainWrapKey.loadOrCreate(
                        service: MessageLogStore.wrapKeyService, account: MessageLogStore.wrapKeyAccount))
                },
                openKey: {
                    KeychainWrapKey.read(
                        service: MessageLogStore.wrapKeyService, account: MessageLogStore.wrapKeyAccount
                    ).map { SymmetricKey(data: $0) }
                })
        }
    }

    static let subdirectory = "lakko-blocked-senders"
    private static let aadDomain = "lakko/ios/blocked-senders/v1\u{0}"

    private static func fileURL(_ accountIdB64: String, in directory: URL) -> URL {
        let digest = SHA256.hash(data: Data(accountIdB64.utf8))
        let name = digest.map { String(format: "%02x", $0) }.joined()
        return directory.appendingPathComponent(name + ".sealed")
    }

    private static func aad(_ accountIdB64: String) -> Data {
        Data((aadDomain + accountIdB64).utf8)
    }

    /// The blocked account keys (standard base64) mirrored for the account,
    /// sorted. A missing mirror, an unprovisioned container, a missing key
    /// and a file that does not open all read as none: the extension then
    /// shows what it would have shown without this file.
    public static func read(accountIdB64: String?, store: Store = .live) -> [String] {
        guard let accountIdB64, let directory = store.directory() else { return [] }
        let url = fileURL(accountIdB64, in: directory)
        guard let sealed = try? Data(contentsOf: url), let key = store.openKey(),
              let box = try? AES.GCM.SealedBox(combined: sealed),
              let plaintext = try? AES.GCM.open(box, using: key, authenticating: aad(accountIdB64)),
              let keys = try? JSONDecoder().decode([String].self, from: plaintext) else { return [] }
        return keys
    }

    /// Replace the account's set with `accountKeys`. An equal stored set is
    /// not rewritten; an account wiped on this device writes nothing.
    public static func replace(accountIdB64: String?, accountKeys: [Data], store: Store = .live) {
        guard let accountIdB64, let directory = store.directory() else { return }
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        let keys = accountKeys.map { $0.base64EncodedString() }.sorted()
        let url = fileURL(accountIdB64, in: directory)
        if FileManager.default.fileExists(atPath: url.path),
           read(accountIdB64: accountIdB64, store: store) == keys { return }
        guard let key = store.sealKey(),
              let plaintext = try? JSONEncoder().encode(keys),
              let sealed = try? AES.GCM.seal(plaintext, using: key, authenticating: aad(accountIdB64)).combined
        else { return }
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            SharedStorage.excludeFromBackup(directory)
            try sealed.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            SharedStorage.excludeFromBackup(url)
        } catch {
            // Best-effort: the next launch or fold writes it again.
        }
    }

    /// Remove the account's file.
    public static func clear(accountIdB64: String, store: Store = .live) {
        guard let directory = store.directory() else { return }
        try? FileManager.default.removeItem(at: fileURL(accountIdB64, in: directory))
    }

    /// The extension's decision: a row from `senderKeyB64` (the account key
    /// the core resolved the MLS sender to, standard base64) shows nothing
    /// when that account is in `blockedKeysB64`. An unresolved sender is
    /// never treated as blocked. This is `blockVerdict(.notification, true)
    /// == .drop` for the one process that cannot reach the core rule's
    /// synced input; the membership is the whole of it.
    public static func dropsNotification(senderKeyB64: String?, blockedKeysB64: [String]) -> Bool {
        guard let senderKeyB64, !senderKeyB64.isEmpty else { return false }
        return blockedKeysB64.contains(senderKeyB64)
    }
}
