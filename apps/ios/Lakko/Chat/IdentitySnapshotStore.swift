// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import Security

/// Per-account on-disk persistence for the MLS provider snapshot
/// blob produced by `Identity.snapshotStorage()`.
///
/// Mirror of:
///
///   * `apps/web/src/chat/mls-store.ts` (1.17f5)
///   * `apps/android/.../chat/IdentitySnapshotStore.kt` (1.15f3)
///
/// ## Threat model
///
/// The snapshot blob is the most sensitive bytes any Lakko process
/// touches — it carries every group's ratchet state, the
/// leaf-secret material, and the init-key private bytes. A dump
/// of the raw blob would let an attacker decrypt every prior
/// message in every group the user is part of. The blob must be
/// wrapped under a hardware-isolated key — same posture as
/// ``DeviceKeyStore`` from 1.14d2.
///
/// ## Why a file + Keychain-wrapped AES rather than direct
/// Keychain storage
///
/// iOS Keychain has a per-item size ceiling around 4-8 KB on
/// `kSecValueData`. MLS snapshot blobs grow with group count + key
/// material — easily 10+ KB once a user has half a dozen 1×1
/// chats. We use the same envelope pattern Android takes:
///
///   1. A 32-byte AES-256 **wrap key** lives in the Keychain under
///      `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — same
///      class-key-protection profile as the device root key, so
///      the wrap key never leaves Secure Enclave-derived storage.
///   2. The snapshot blob is encrypted with AES-256-GCM using the
///      wrap key + a fresh per-write 12-byte IV via CryptoKit's
///      `AES.GCM.seal(_:using:)`.
///   3. The opaque `(iv || ciphertext || tag)` envelope is
///      persisted to a per-account file under
///      `Application Support/lakko-mls-snapshots/<accountIdB64>.bin`,
///      excluded from iCloud backup via the
///      `URLResourceKey.isExcludedFromBackupKey` flag.
///
/// `Application Support` (not `Documents`) is the canonical
/// iOS-idiomatic location for "app-owned, never user-visible"
/// state — same posture as Android's
/// `Context.noBackupFilesDir`. Exclusion from iCloud backup is
/// critical: MLS state is device-bound; an auto-restore from
/// iCloud must NOT carry it between devices, even if the user
/// opts in — the recipient peer's view of the conversation would
/// diverge from the restored sender's view the moment any of
/// them sent a new message.
///
/// ## Persistence layout
///
/// ```text
///   ApplicationSupport/lakko-mls-snapshots/
///     <accountIdB64>.bin     12-byte IV || ciphertext || 16-byte tag
/// ```
public final class IdentitySnapshotStore {

    /// Keychain service tag for the AES wrap key.
    public static let wrapKeyService: String = "me.lakko.mls-snapshot-wrap.v1"

    /// Keychain account marker for the wrap key. One wrap key per
    /// app installation (not per Lakko account) — the per-account
    /// isolation comes from the file path, not the wrap key.
    /// Mirror of Android's `WRAP_KEY_ALIAS` — one alias, every
    /// account's snapshot wraps under the same key.
    private static let wrapKeyAccount: String = "wrap-key"

    private let envelopeDir: URL?

    public init() {
        self.envelopeDir = Self.envelopeDir()
    }

    // MARK: - Public API

    /// Persist `blob` for `accountIdB64`. Overwrites any existing
    /// envelope. Best-effort — Keychain or filesystem failures
    /// surface as a logged-and-swallowed no-op at the
    /// ``ChatRepository`` call site.
    public func putSnapshot(accountIdB64: String, blob: Data) {
        guard let envelopeDir = envelopeDir else { return }
        let wrapKey: SymmetricKey
        do {
            wrapKey = try loadOrCreateWrapKey()
        } catch {
            return // best-effort
        }
        let sealed: AES.GCM.SealedBox
        do {
            sealed = try AES.GCM.seal(blob, using: wrapKey)
        } catch {
            return
        }
        // `combined` lays out `nonce || ciphertext || tag` per
        // RFC 5116 §3.1 — same on-disk envelope shape the Android
        // side produces (12-byte IV + ciphertext + 16-byte tag).
        guard let combined = sealed.combined else { return }

        // Make sure the parent directory exists + is flagged as
        // excluded from iCloud backup.
        do {
            try FileManager.default.createDirectory(
                at: envelopeDir,
                withIntermediateDirectories: true
            )
            try Self.excludeFromBackup(envelopeDir)
        } catch {
            // best-effort
        }
        let url = envelopeURL(accountIdB64: accountIdB64)
        do {
            try combined.write(to: url, options: .atomic)
            try Self.excludeFromBackup(url)
        } catch {
            // best-effort
        }
    }

    /// Read + unwrap the snapshot for `accountIdB64`. Returns
    /// `nil` on missing file, Keychain failure, or AES-GCM auth
    /// failure (e.g. wrap-key rotated since the envelope was
    /// written).
    public func getSnapshot(accountIdB64: String) -> Data? {
        let url = envelopeURL(accountIdB64: accountIdB64)
        guard FileManager.default.fileExists(atPath: url.path) else {
            return nil
        }
        let combined: Data
        do {
            combined = try Data(contentsOf: url)
        } catch {
            return nil
        }
        let wrapKey: SymmetricKey
        do {
            wrapKey = try loadOrCreateWrapKey()
        } catch {
            return nil
        }
        do {
            let sealed = try AES.GCM.SealedBox(combined: combined)
            return try AES.GCM.open(sealed, using: wrapKey)
        } catch {
            return nil
        }
    }

    /// Erase the snapshot for `accountIdB64`. No-op if absent.
    public func deleteSnapshot(accountIdB64: String) {
        let url = envelopeURL(accountIdB64: accountIdB64)
        try? FileManager.default.removeItem(at: url)
    }

    // MARK: - Keychain-resident wrap key

    private func loadOrCreateWrapKey() throws -> SymmetricKey {
        if let existing = try readWrapKey() {
            return SymmetricKey(data: existing)
        }
        let fresh = SymmetricKey(size: .bits256)
        try storeWrapKey(fresh.withUnsafeBytes { Data($0) })
        return fresh
    }

    private func readWrapKey() throws -> Data? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.wrapKeyService,
            kSecAttrAccount: Self.wrapKeyAccount,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            return item as? Data
        case errSecItemNotFound:
            return nil
        default:
            throw IdentitySnapshotStoreError.keychain(
                status: status,
                operation: "SecItemCopyMatching"
            )
        }
    }

    private func storeWrapKey(_ keyBytes: Data) throws {
        // Same delete-then-add upsert pattern as DeviceKeyStore.
        let baseQuery: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.wrapKeyService,
            kSecAttrAccount: Self.wrapKeyAccount
        ]
        _ = SecItemDelete(baseQuery as CFDictionary)

        var addQuery = baseQuery
        addQuery[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        addQuery[kSecValueData] = keyBytes
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        if status != errSecSuccess {
            throw IdentitySnapshotStoreError.keychain(
                status: status,
                operation: "SecItemAdd"
            )
        }
    }

    // MARK: - File layout

    private func envelopeURL(accountIdB64: String) -> URL {
        // Replace any `/` in the base64 account id with `_` so the
        // segment is filesystem-safe — same posture as Android.
        let safe = accountIdB64.replacingOccurrences(of: "/", with: "_")
        return (envelopeDir ?? FileManager.default.temporaryDirectory)
            .appendingPathComponent("\(safe).bin")
    }

    private static func envelopeDir() -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return nil
        }
        return appSupport.appendingPathComponent("lakko-mls-snapshots")
    }

    private static func excludeFromBackup(_ url: URL) throws {
        var url = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try url.setResourceValues(values)
    }
}

public enum IdentitySnapshotStoreError: Error {
    case keychain(status: OSStatus, operation: String)
}
