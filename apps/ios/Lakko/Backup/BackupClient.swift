// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// `BackupClient` — orchestrates the encrypted-backup upload and
/// restore flows on iOS (Phase 2a1c).
///
/// Mirror of `apps/web/src/backup/client.ts`. Glues four layers:
///
///   1. **Phrase cache** (``PhraseStore``): the canonical
///      recovery phrase, persisted in the iOS Keychain under
///      `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`.
///   2. **`lakko-backup` UniFFI surface** (`buildBackupPayload`,
///      `sealBackup`, `openBackup`, `parseBackupPayload`) —
///      `LakkoCore`-exposed primitives that do the AEAD work.
///   3. **``StorageClient``**: the three `/backup` HTTP routes
///      (`PUT`, `GET`, `DELETE`).
///   4. **Local state sources**: ``IdentitySecrets`` from
///      ``AuthRepository`` + the accepted-contacts list from
///      ``ContactsRepository.state.accepted``.
///
/// MLS group snapshots are reserved in the v1 payload but
/// emitted empty — the DeviceGroup-side snapshot publishing
/// path lands in 2a3.
public final class BackupClient {

    private let storage: StorageClient
    private let phraseStore: PhraseStore

    public init(
        storage: StorageClient = StorageClient(),
        phraseStore: PhraseStore = PhraseStore()
    ) {
        self.storage = storage
        self.phraseStore = phraseStore
    }

    /// Bind / rebind the bearer token for `/backup` routes.
    public func setSessionToken(_ token: String?) {
        storage.setSessionToken(token)
    }

    // MARK: - Upload

    /// Compose + seal + PUT in one step. Caller has the phrase on
    /// hand right now (signup just minted it, recover just
    /// consumed it). No phrase-cache hop.
    public func uploadNow(
        accountId: Data,
        ed25519Seed: Data,
        phraseBytes: Data,
        contacts: [BackupContactRecord],
        // Phase 2a3d3: whole-storage MLS snapshot from
        // `Identity.snapshotStorage()`. Pass `Data()` on signup
        // baseline when there is no MLS state yet.
        wholeStorageSnapshot: Data = Data()
    ) async throws {
        let payloadBytes = try buildBackupPayload(
            accountId: accountId,
            createdAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
            ed25519Seed: ed25519Seed,
            contacts: contacts,
            wholeStorageSnapshot: wholeStorageSnapshot
        )
        let envelope = try sealBackup(phrase: phraseBytes, payloadBytes: payloadBytes)
        try await storage.putBackup(envelopeBytes: envelope)
    }

    /// Read the phrase from the Keychain cache and run
    /// ``uploadNow`` with it. Returns a discriminated
    /// ``UploadResult`` so the route layer can branch on
    /// `cacheMiss` (user needs to run recovery to enable backups
    /// on this device).
    public func uploadFromCache(
        accountId: Data,
        accountIdB64: String,
        ed25519Seed: Data,
        contacts: [BackupContactRecord],
        wholeStorageSnapshot: Data = Data()
    ) async -> UploadResult {
        let cachedOrNil: Data?
        do {
            cachedOrNil = try phraseStore.getPhrase(accountIdB64: accountIdB64)
        } catch {
            return .keychainFailed(error)
        }
        guard let cached = cachedOrNil else {
            return .cacheMiss
        }
        var phraseBytes = cached
        defer {
            // Best-effort scrub — `Data` is value-typed; this
            // only zeroes the local copy. The Keychain row is
            // untouched (the cache stays valid).
            phraseBytes.resetBytes(in: 0..<phraseBytes.count)
        }
        do {
            try await uploadNow(
                accountId: accountId,
                ed25519Seed: ed25519Seed,
                phraseBytes: phraseBytes,
                contacts: contacts,
                wholeStorageSnapshot: wholeStorageSnapshot
            )
            return .uploaded
        } catch {
            return .transportFailed(error)
        }
    }

    // MARK: - Restore

    /// Fetch + decrypt + parse the server-stored backup. Returns
    /// `nil` when no blob has ever been uploaded; throws on
    /// transport / wrong-phrase / decode failures (caller
    /// distinguishes via the error type).
    public func restore(phraseBytes: Data) async throws -> ParsedBackupPayload? {
        guard let envelope = try await storage.getBackup() else {
            return nil
        }
        let payloadBytes = try openBackup(phrase: phraseBytes, wire: envelope)
        return try parseBackupPayload(wire: payloadBytes)
    }

    /// Cache the phrase under the Keychain. Called from the
    /// post-signup / post-recover bootstrap path.
    public func cachePhrase(accountIdB64: String, phraseBytes: Data) throws {
        try phraseStore.putPhrase(
            accountIdB64: accountIdB64,
            phraseBytes: phraseBytes
        )
    }

    /// Wipe the cached phrase. Called from the `forgetDevice`
    /// cleanup. No-op when absent.
    public func wipePhrase(accountIdB64: String) throws {
        try phraseStore.deletePhrase(accountIdB64: accountIdB64)
    }
}

/// Discriminated outcome of ``BackupClient/uploadFromCache``.
public enum UploadResult {
    case uploaded
    case cacheMiss
    case keychainFailed(Error)
    case transportFailed(Error)
}
