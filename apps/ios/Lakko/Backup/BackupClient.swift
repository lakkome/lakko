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
/// The v1 payload carries a whole-storage MLS snapshot from
/// `Identity.snapshotStorage()` (Phase 2a3d3) — passed through
/// `uploadNow` / `uploadFromCache` as `wholeStorageSnapshot`,
/// empty only on the signup baseline before any MLS state
/// exists.
public final class BackupClient {

    private let storage: StorageClient
    private let phraseStore: PhraseStore
    /// Auth-service base for the `POST /auth/backup/head` mint (ADR-0037). The
    /// storage `/backup` routes live on ``StorageClient/baseURL`` (a different
    /// service); the head mint is an auth-service call, so it carries its own
    /// base — same split the web keeps between the storage + auth clients.
    private let authBaseURL: String
    /// Bearer token for BOTH the storage `/backup` routes AND the
    /// `/auth/backup/head` mint (ADR-0037). Set via ``setSessionToken(_:)``.
    private var sessionToken: String?

    public init(
        storage: StorageClient = StorageClient(),
        phraseStore: PhraseStore = PhraseStore(),
        authBaseURL: String = AuthClient.defaultBaseURL
    ) {
        self.storage = storage
        self.phraseStore = phraseStore
        self.authBaseURL = authBaseURL
        self.sessionToken = nil
    }

    /// Bind / rebind the bearer token used for the `/backup` routes AND the
    /// `/auth/backup/head` mint (ADR-0037). Mirror of the web
    /// `BackupClient.setSessionToken`.
    /// Decode one hex group id, or `nil` when it is not one.
    ///
    /// Lakko mints 32-byte ids; anything else names no group this device could
    /// load, so carrying it would only make a restore try and fail.
    static func groupIdBytes(fromHex hex: String) -> Data? {
        guard hex.count == 64, hex.count % 2 == 0 else { return nil }
        var out = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            out.append(byte)
            index = next
        }
        return out
    }

    public func setSessionToken(_ token: String?) {
        self.sessionToken = token
        storage.setSessionToken(token)
    }

    // MARK: - Upload

    /// Compose + seal + PUT in one step. Caller has the phrase on
    /// hand right now (signup just minted it, recover just
    /// consumed it). No phrase-cache hop.
    ///
    /// ADR-0037 (G-11): after sealing the envelope, mint the auth-signed head,
    /// frame `TAG ‖ head ‖ envelope`, and PUT the framed blob. Soft-fail — a
    /// mint / framing failure writes the legacy unframed envelope rather than
    /// failing the whole backup. On a successful mint the auth-assigned
    /// `backup_version` floor is persisted into the ``BackupFloorStore`` — the
    /// monotonic anchor a future returning-client restore refuses to go below.
    public func uploadNow(
        accountId: Data,
        accountIdB64: String,
        ed25519Seed: Data,
        phraseBytes: Data,
        contacts: [BackupContactRecord],
        // Phase 2a3d3: whole-storage MLS snapshot from
        // `Identity.snapshotStorage()`. Pass `Data()` on signup
        // baseline when there is no MLS state yet.
        wholeStorageSnapshot: Data = Data(),
        /// The MLS `group_id`s this device knows about, hex as the repository
        /// holds them. The index the snapshot cannot supply: a restored
        /// provider holds every group and offers no way to enumerate them.
        knownGroupIdsHex: [String] = []
    ) async throws {
        let payloadBytes = try buildBackupPayload(
            accountId: accountId,
            createdAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
            ed25519Seed: ed25519Seed,
            contacts: contacts,
            wholeStorageSnapshot: wholeStorageSnapshot,
            // A row that is not a group id is dropped rather than failing the
            // backup: one malformed local entry must not cost the whole
            // envelope.
            knownGroupIds: knownGroupIdsHex.compactMap(Self.groupIdBytes(fromHex:))
        )
        let envelope = try sealBackup(phrase: phraseBytes, payloadBytes: payloadBytes)
        let (blob, floor) = await frameWithHead(envelope: envelope)
        try await storage.putBackup(envelopeBytes: blob)
        // Persist the floor ONLY on a successful mint (ADR-0037 §6).
        // `putFloor` never lowers an existing floor.
        if let floor {
            do {
                try BackupFloorStore.putFloor(accountIdB64: accountIdB64, floor: floor)
            } catch {
                NSLog("backup floor persist failed: \(error)")
            }
        }
    }

    /// Mint the auth-signed backup head for `envelope`, then frame
    /// `TAG ‖ head ‖ envelope` (ADR-0037 §5). Soft-fail: a mint / framing
    /// failure (network, 5xx, no session token) falls back to the UNFRAMED
    /// legacy envelope so a backup never fails because the head could not be
    /// minted — the mirror image of the restore-side fail-open on an absent head.
    private func frameWithHead(envelope: Data) async -> (blob: Data, floor: BackupFloor?) {
        do {
            let hash = backupEnvelopeHash(envelopeBytes: envelope)
            let minted = try await mintBackupHead(
                sessionToken: sessionToken,
                envelopeHash: hash,
                baseURL: authBaseURL
            )
            let blob = try frameBackupBlob(head: minted.head, envelope: envelope)
            return (blob, BackupFloor(version: minted.version, envelopeHash: hash))
        } catch {
            NSLog("backup-head mint failed, writing legacy envelope: \(error)")
            return (envelope, nil)
        }
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
        wholeStorageSnapshot: Data = Data(),
        knownGroupIdsHex: [String] = []
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
                accountIdB64: accountIdB64,
                ed25519Seed: ed25519Seed,
                phraseBytes: phraseBytes,
                contacts: contacts,
                wholeStorageSnapshot: wholeStorageSnapshot,
                knownGroupIdsHex: knownGroupIdsHex
            )
            return .uploaded
        } catch {
            return .transportFailed(error)
        }
    }

    // MARK: - Restore

    /// Fetch + decrypt + parse the server-stored backup, applying the ADR-0037
    /// §6 rollback gate (G-11).
    ///
    /// Mirror of the web `BackupClient.restore(phraseBytes, gate)`. The gate is
    /// **mandatory** (F2) — there is no ungated restore path: a served blob is
    /// always run through the fail-open / fail-closed policy before its envelope
    /// is opened. `hardFail` → refuse + alarm; `staleOverride` → ask the user
    /// (never silent); `failOpen` / `proceed` → open the envelope (and, on a
    /// clean `proceed`, advance the monotone floor).
    public func restore(
        phraseBytes: Data,
        gate: RestoreGateContext
    ) async -> RestoreOutcome {
        let blob: Data?
        do {
            blob = try await storage.getBackup()
        } catch {
            return .transportFailed(error)
        }
        guard let blob else { return .noBackup }

        // ADR-0037 §6 (F2): the rollback gate is MANDATORY — always peel
        // `TAG ‖ head` and decide proceed / fail-open / hard-fail / stale-override
        // BEFORE opening the envelope. There is no ungated bypass that could strip
        // a head unchecked.
        var envelope: Data = blob
        var cleanProceed: BackupFloor?
        let evaluation = await evaluateRestoreBlob(
            RestoreEvaluationInput(
                blob: blob,
                accountId: gate.accountId,
                accountIdB64: gate.accountIdB64,
                authBaseUrl: gate.authBaseUrl,
                nowMillis: gate.nowMillis,
                stalenessMillis: gate.stalenessMillis
            )
        )
        switch evaluation.verdict {
        case .hardFail(let reason):
            // Refuse + alarm, no override (invariant #4 — no silent downgrade).
            return .rollbackBlocked(hard: true, reason: reason)
        case .staleOverride(let reason):
            // Never silent-accept a stale head — require an explicit user ack.
            let accepted = await gate.confirmStaleOverride(reason)
            if !accepted {
                return .rollbackBlocked(hard: false, reason: reason)
            }
            // Accepted: proceed with the (older) envelope, but DO NOT advance the
            // floor down — `putFloor` is monotone, so a deliberate downgrade never
            // lowers this device's future protection.
        case .failOpen, .proceed:
            break
        }
        envelope = evaluation.envelope
        if evaluation.verdict == .proceed,
            let version = evaluation.verifiedVersion,
            let hash = evaluation.envelopeHash {
            cleanProceed = BackupFloor(version: version, envelopeHash: hash)
        }

        let payloadBytes: Data
        do {
            payloadBytes = try openBackup(phrase: phraseBytes, wire: envelope)
        } catch {
            // Wrong phrase or tampered blob — the core folds both into one error.
            NSLog("BackupClient: openBackup failed: \(error)")
            return .wrongPhraseOrTampered
        }
        let parsed: ParsedBackupPayload
        do {
            parsed = try parseBackupPayload(wire: payloadBytes)
        } catch {
            NSLog("BackupClient: parseBackupPayload failed: \(error)")
            return .wrongPhraseOrTampered
        }

        // ADR-0037 §6: on a clean verified proceed, advance the monotone floor so
        // subsequent restores on this device are hard-protected (returning-client
        // path). Skipped on fail-open (no verified head) and stale-override
        // (never lower the floor).
        if let cleanProceed {
            do {
                try BackupFloorStore.putFloor(
                    accountIdB64: gate.accountIdB64,
                    floor: cleanProceed
                )
            } catch {
                NSLog("BackupClient: floor persist (restore) failed: \(error)")
            }
        }

        return .restored(parsed)
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

/// Context the restore seam threads into ``BackupClient/restore(phraseBytes:gate:)``
/// to arm the ADR-0037 §6 rollback gate (G-11). **Required** (F2) — there is no
/// ungated restore path: a caller cannot omit it, so a served blob is always run
/// through the fail-open / fail-closed policy before its envelope is opened.
/// Mirror of the web `RestoreGateContext`.
public struct RestoreGateContext {
    /// The 16-byte account_id the client OPAQUE-authenticated as (head binding).
    public let accountId: Data
    /// Base64 account id — the floor-store key.
    public let accountIdB64: String
    /// Auth base for the attest-key fetch (default the DeviceAttestKey base).
    public let authBaseUrl: String
    /// Clock in ms (default now).
    public let nowMillis: Int64
    /// Staleness window in ms (default from the Info.plist knob).
    public let stalenessMillis: Int64
    /// Blocking confirmation for a stale head — MUST NOT auto-accept. A caller
    /// with no UI (headless / tests) passes a closure returning `false`
    /// (fail-closed — never a silent downgrade).
    public let confirmStaleOverride: (String) async -> Bool

    public init(
        accountId: Data,
        accountIdB64: String,
        authBaseUrl: String = WebauthnClient.defaultBaseURL,
        nowMillis: Int64 = Int64(Date().timeIntervalSince1970 * 1000),
        stalenessMillis: Int64 = backupHeadStalenessMillis(),
        confirmStaleOverride: @escaping (String) async -> Bool
    ) {
        self.accountId = accountId
        self.accountIdB64 = accountIdB64
        self.authBaseUrl = authBaseUrl
        self.nowMillis = nowMillis
        self.stalenessMillis = stalenessMillis
        self.confirmStaleOverride = confirmStaleOverride
    }
}

/// Discriminated restore-flow outcome (ADR-0037 §6). Mirror of the web
/// `RestoreResult`.
public enum RestoreOutcome {
    /// Envelope opened + parsed. Load the payload.
    case restored(ParsedBackupPayload)
    /// No blob has ever been uploaded.
    case noBackup
    /// Wrong phrase or tampered blob (the core folds both into one).
    case wrongPhraseOrTampered
    /// The §6 rollback gate refused the served blob. `hard == true` = an
    /// unambiguous tamper / corruption (no override); `hard == false` = a stale
    /// head the user was offered and DECLINED to override. Either way no restore
    /// happened — the caller alarms (hard) or notes (declined).
    case rollbackBlocked(hard: Bool, reason: String)
    /// Transport failure fetching the blob.
    case transportFailed(Error)
}
