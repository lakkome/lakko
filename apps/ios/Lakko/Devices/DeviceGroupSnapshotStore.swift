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
///     <accountIdB64>.bin             12-byte IV || ciphertext || 16-byte tag
///     <accountIdB64>.<kind>.pending  the same envelope, one per pending kind
/// ```
///
/// ## ADR-0081 — the outbox beside the snapshot, and the generation
///
/// A DeviceGroup commit in flight is written as a PENDING row beside the
/// snapshot (``PendingKind/commit``), never over it: the snapshot stays at
/// epoch N until the DS has answered (decision 2). A rejoin keeps its knock
/// (``PendingKind/rejoin``) and a sibling its admission record
/// (``PendingKind/admissions``) the same way. ``deleteSnapshot(accountIdB64:)``
/// (Forget device) removes every pending row with the snapshot; the writes go
/// through ``AccountStoreGate`` like the snapshot's, so a sealed account
/// takes no late write (ADR-0077).
///
/// Every snapshot write bumps a per-account GENERATION (decision 3). A
/// write may name the generation it read (`expectedGeneration`) and is then
/// refused (``SnapshotChanged``) when another handle wrote since: a
/// long-lived handle still at N never puts N back over the N+1 a commit made
/// current through a handle of its own. The generation is kept in memory per
/// process (only this process writes the DeviceGroup snapshot), starting at 0.
public final class DeviceGroupSnapshotStore {

    /// Which pending row (ADR-0081).
    public enum PendingKind: String, CaseIterable {
        /// The DeviceGroup commit in flight (``DeviceGroupCommitEntry``).
        case commit
        /// The rejoin in progress (``DeviceGroupRejoinState``).
        case rejoin
        /// A sibling's admission record (``DeviceGroupAdmissionRecord``).
        case admissions
        /// Where the receive loop resumes, bound to the journal's group id
        /// (``DeviceGroupJournalCursor``). Kept by a rejoin: the position it
        /// names is before the commit that admits this device again, so
        /// nothing the new membership can read is skipped. Deleted with the
        /// snapshot on Forget device.
        case journalCursor = "cursor"
    }

    /// Refused: another handle wrote the snapshot since the generation this
    /// write named.
    public struct SnapshotChanged: Error, Equatable {}

    private static var generations: [String: Int] = [:]
    private static let generationsLock = NSLock()


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
        _ = try? write(accountIdB64: accountIdB64, blob: blob, session: session, failClosed: false)
    }

    /// Persist `blob` and THROW when it did not durably land, or when the
    /// account's session has ended (``AccountStoreGate/Sealed``). For the
    /// write that precedes a DeviceGroup publish: the provider state in
    /// `blob` holds the ratchet step the message about to be published took,
    /// so a publish after a write that was not made is a ciphertext whose
    /// generation the next session would use again.
    @discardableResult
    public func putSnapshotDurable(
        accountIdB64: String, blob: Data, session: AccountStoreGate.Session? = nil,
        expectedGeneration: Int? = nil
    ) throws -> Int {
        try write(
            accountIdB64: accountIdB64, blob: blob, session: session, failClosed: true,
            expectedGeneration: expectedGeneration)
    }

    /// The generation the snapshot of `accountIdB64` is at (0 before the
    /// first write of this process).
    public func generation(accountIdB64: String) -> Int {
        Self.generationsLock.lock()
        defer { Self.generationsLock.unlock() }
        return Self.generations[generationKey(accountIdB64)] ?? 0
    }

    private func generationKey(_ accountIdB64: String) -> String {
        (envelopeDir?.path ?? "") + "|" + accountIdB64
    }

    @discardableResult
    private func write(
        accountIdB64: String, blob: Data, session: AccountStoreGate.Session?, failClosed: Bool,
        expectedGeneration: Int? = nil
    ) throws -> Int {
        // The check and the write under one lock: two handles writing at once
        // must not both pass the generation check.
        Self.generationsLock.lock()
        defer { Self.generationsLock.unlock() }
        let key = generationKey(accountIdB64)
        let current = Self.generations[key] ?? 0
        if let expectedGeneration, expectedGeneration != current {
            throw SnapshotChanged()
        }
        try writeEnvelope(
            url: envelopeURL(accountIdB64: accountIdB64), accountIdB64: accountIdB64, blob: blob,
            session: session, failClosed: failClosed)
        Self.generations[key] = current + 1
        return current + 1
    }

    private func writeEnvelope(
        url: URL, accountIdB64: String, blob: Data, session: AccountStoreGate.Session?, failClosed: Bool
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
        readEnvelope(url: envelopeURL(accountIdB64: accountIdB64))
    }

    /// ADR-0081 — write a pending row beside the snapshot, FAIL-CLOSED: a
    /// failed write throws, and so does a write for an account whose session
    /// has ended (``AccountStoreGate/Sealed``).
    public func putPending(
        accountIdB64: String, kind: PendingKind, blob: Data, session: AccountStoreGate.Session? = nil
    ) throws {
        try writeEnvelope(
            url: pendingURL(accountIdB64: accountIdB64, kind: kind), accountIdB64: accountIdB64,
            blob: blob, session: session, failClosed: true)
    }

    /// The pending row of `kind`, or `nil` when absent or unreadable.
    public func getPending(accountIdB64: String, kind: PendingKind) -> Data? {
        readEnvelope(url: pendingURL(accountIdB64: accountIdB64, kind: kind))
    }

    /// Delete one pending row. No-op when absent.
    public func deletePending(accountIdB64: String, kind: PendingKind) {
        try? FileManager.default.removeItem(at: pendingURL(accountIdB64: accountIdB64, kind: kind))
    }

    /// ADR-0081 decision 4 — a device that lost or fell behind its
    /// DeviceGroup drops its state to rejoin: the snapshot, the commit in
    /// flight and the admission record go; the rejoin row (the knock this
    /// device is waiting on) stays.
    public func dropStateForRejoin(accountIdB64: String) {
        deleteSnapshotFile(accountIdB64: accountIdB64)
        deletePending(accountIdB64: accountIdB64, kind: .commit)
        deletePending(accountIdB64: accountIdB64, kind: .admissions)
    }

    private func readEnvelope(url: URL) -> Data? {
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

    /// Erase the snapshot for `accountIdB64` and every pending row beside it
    /// (ADR-0081: the commit outbox, the rejoin, the admission record). No-op
    /// when absent. Called from `forgetDevice`.
    public func deleteSnapshot(accountIdB64: String) {
        deleteSnapshotFile(accountIdB64: accountIdB64)
        for kind in PendingKind.allCases {
            deletePending(accountIdB64: accountIdB64, kind: kind)
        }
    }

    /// Delete the snapshot file alone (the pending rows stay) when one is on
    /// disk: a snapshot that does not decrypt or does not restore can never
    /// become usable (ADR-0081 decision 4).
    public func dropUnusableSnapshot(accountIdB64: String) {
        guard FileManager.default.fileExists(atPath: envelopeURL(accountIdB64: accountIdB64).path) else {
            return
        }
        deleteSnapshotFile(accountIdB64: accountIdB64)
    }

    private func deleteSnapshotFile(accountIdB64: String) {
        Self.generationsLock.lock()
        defer { Self.generationsLock.unlock() }
        let url = envelopeURL(accountIdB64: accountIdB64)
        try? FileManager.default.removeItem(at: url)
        // A deletion is a change: a handle that read the deleted snapshot
        // must not write it back.
        let key = generationKey(accountIdB64)
        Self.generations[key] = (Self.generations[key] ?? 0) + 1
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

    private func pendingURL(accountIdB64: String, kind: PendingKind) -> URL {
        let safe = accountIdB64.replacingOccurrences(of: "/", with: "_")
        return (envelopeDir ?? FileManager.default.temporaryDirectory)
            .appendingPathComponent("\(safe).\(kind.rawValue).pending")
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
