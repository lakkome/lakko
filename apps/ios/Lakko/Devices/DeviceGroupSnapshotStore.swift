// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import Security

/// File-backed encrypted DeviceGroup snapshot cache
/// (Phase 2a3b3, ADR-0007).
///
/// Mirror of:
///
///   * `apps/web/src/devices/device-group-snapshot-store.ts`
///   * `apps/android/.../devices/DeviceGroupSnapshotStore.kt`
///     (Phase 2a3b4)
///
/// The **DeviceGroup** is a per-user MLS group whose leaves are
/// the user's own devices (ADR-0007). To keep the same handle
/// across app launches we snapshot the underlying OpenMLS
/// provider state via `Identity.snapshotStorage()` and persist
/// the resulting blob here, wrapped under an AES-256-GCM
/// Keychain-resident key.
///
/// Distinct from ``IdentitySnapshotStore`` (conversation-group
/// snapshot for the AccountIdentity provider): the
/// DeviceIdentity provider is a separate OpenMLS instance from
/// the AccountIdentity provider. ADR-0007 §"binding the two".
///
/// ## Persistence layout
///
/// ```text
///   ApplicationSupport/lakko-device-group-snapshots/
///     <accountIdB64>.bin     12-byte IV || ciphertext || 16-byte tag
/// ```
public final class DeviceGroupSnapshotStore {

    /// Keychain service tag for the AES wrap key. Versioned
    /// so a future rotation has somewhere to go without
    /// colliding with v1 entries on upgrade.
    public static let wrapKeyService: String = "me.lakko.device-group-snapshot-wrap.v1"

    /// Keychain account marker — one wrap key per app
    /// installation (not per Lakko account). Per-account
    /// isolation comes from the file path, not the wrap key.
    private static let wrapKeyAccount: String = "wrap-key"

    private let envelopeDir: URL?

    /// - Parameter directoryName: which folder under Application Support this
    ///   instance owns. A second kind of envelope gets its own folder and
    ///   shares the wrap key, which is correct because isolation here comes
    ///   from the path: the key is one per installation, not per account or
    ///   per kind, and the comment on ``wrapKeyAccount`` has always said so.
    public init(directoryName: String = "lakko-device-group-snapshots") {
        self.envelopeDir = Self.envelopeDir(named: directoryName)
    }

    // MARK: - Public API

    /// Persist `blob` for `accountIdB64`. Overwrites any
    /// existing envelope. Best-effort — Keychain / FS failures
    /// surface as logged-and-swallowed no-ops at the caller. For a write
    /// nothing is published on the strength of (state applied from a
    /// received row); a write that precedes a publish uses
    /// ``putSnapshotDurable(accountIdB64:blob:session:)``.
    ///
    /// `session`: the session the handle whose state this is was made in
    /// (``AccountStoreGate/Session``); refused once it has ended.
    public func putSnapshot(
        accountIdB64: String, blob: Data, session: AccountStoreGate.Session? = nil
    ) {
        try? write(accountIdB64: accountIdB64, blob: blob, session: session, failClosed: false)
    }

    /// Persist `blob` and THROW when it did not durably land, or when the
    /// account's session has ended (``AccountStoreGate/Sealed``). For the
    /// write that precedes a DeviceGroup publish: the provider state in
    /// `blob` holds the ratchet step the message about to be published took,
    /// so a publish after a write that was not made is a ciphertext whose
    /// generation the next session would use again.
    public func putSnapshotDurable(
        accountIdB64: String, blob: Data, session: AccountStoreGate.Session? = nil
    ) throws {
        try write(accountIdB64: accountIdB64, blob: blob, session: session, failClosed: true)
    }

    private func write(
        accountIdB64: String, blob: Data, session: AccountStoreGate.Session?, failClosed: Bool
    ) throws {
        let entered: AccountStoreGate.Hold?
        if let session {
            entered = AccountStoreGate.enter(session: session)
        } else {
            entered = AccountStoreGate.enter(accountIdB64)
        }
        guard let gate = entered else {
            if failClosed { throw AccountStoreGate.Sealed() }
            return
        }
        defer { gate.leave() }
        guard let envelopeDir = envelopeDir else {
            throw CocoaError(.fileNoSuchFile)
        }
        let wrapKey = try loadOrCreateWrapKey()
        let sealed = try AES.GCM.seal(blob, using: wrapKey)
        guard let combined = sealed.combined else {
            throw CocoaError(.fileWriteUnknown)
        }
        do {
            try FileManager.default.createDirectory(
                at: envelopeDir,
                withIntermediateDirectories: true
            )
            try Self.excludeFromBackup(envelopeDir)
        } catch {
            // best-effort: the write below fails if the directory is missing
        }
        let url = envelopeURL(accountIdB64: accountIdB64)
        try combined.write(to: url, options: .atomic)
        do {
            try Self.excludeFromBackup(url)
        } catch {
            // best-effort
        }
    }

    /// Read + unwrap the snapshot. Returns `nil` on missing
    /// file, Keychain failure, or AES-GCM auth failure (e.g.
    /// wrap-key rotated since the envelope was written).
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

    /// Erase the snapshot for `accountIdB64`. No-op when
    /// absent. Called from `forgetDevice`.
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
            throw DeviceGroupSnapshotStoreError.keychain(
                status: status,
                operation: "SecItemCopyMatching"
            )
        }
    }

    private func storeWrapKey(_ keyBytes: Data) throws {
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
            throw DeviceGroupSnapshotStoreError.keychain(
                status: status,
                operation: "SecItemAdd"
            )
        }
    }

    // MARK: - File layout

    private func envelopeURL(accountIdB64: String) -> URL {
        let safe = accountIdB64.replacingOccurrences(of: "/", with: "_")
        return (envelopeDir ?? FileManager.default.temporaryDirectory)
            .appendingPathComponent("\(safe).bin")
    }

    private static func envelopeDir(named directoryName: String) -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return nil
        }
        return appSupport.appendingPathComponent(directoryName)
    }

    private static func excludeFromBackup(_ url: URL) throws {
        var url = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try url.setResourceValues(values)
    }
}

public enum DeviceGroupSnapshotStoreError: Error {
    case keychain(status: OSStatus, operation: String)
}
