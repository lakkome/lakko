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
/// The payload (wire v7) carries the accepted contacts, the ids of the
/// account's conversations, the ADR-0058 synced state, the account's own
/// predecessor seeds (ADR-0074 section 3.7) and its own profile and "who can
/// find me" envelopes. It carries NO MLS provider state: a snapshot belongs to the device that made it. Loaded on
/// another device it would clone a leaf (ADR-0050); loaded on the same device
/// after a reinstall it would roll the device back behind commits it already
/// published (ADR-0040) and re-use ratchet positions (G-34). A restored
/// device is admitted to its chats by their other members (ADR-0050 section
/// 2.5a), never from the backup.
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

    /// The session the head mint travels on (the storage routes travel on
    /// the storage client's own).
    private let authSession: URLSession

    /// ADR-0074 section 3.7 — the account's own predecessor seeds. Read at
    /// the one place a payload is composed for upload, so no backup route can
    /// leave them out.
    private let ownPredecessors: OwnPredecessorStoring
    private let adoptions = PredecessorMailbox.AdoptionStore()

    /// The account's own profile and "who can find me" envelopes, read at the
    /// same one place, for the same reason.
    private let ownEnvelopes: @MainActor (_ accountIdB64: String) -> OwnBackupEnvelopes

    init(
        storage: StorageClient = StorageClient(),
        phraseStore: PhraseStore = PhraseStore(),
        authBaseURL: String = AuthClient.defaultBaseURL,
        authSession: URLSession = .shared,
        ownPredecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore(),
        ownEnvelopes: @escaping @MainActor (_ accountIdB64: String) -> OwnBackupEnvelopes = {
            OwnBackupEnvelopes.current(accountIdB64: $0)
        }
    ) {
        self.storage = storage
        self.phraseStore = phraseStore
        self.authBaseURL = authBaseURL
        self.authSession = authSession
        self.ownPredecessors = ownPredecessors
        self.ownEnvelopes = ownEnvelopes
        self.sessionToken = nil
    }

    /// Decode one hex group id, or `nil` when it is not one.
    ///
    /// Lakko mints 32-byte ids; anything else names no conversation, so
    /// carrying it would only put a row in the list that nothing can match.
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

    /// ADR-0074 section 3.7 — the records a backup carries for `seeds`
    /// (oldest first, as the predecessor slot holds them), each with the time
    /// its key was retired. The rule of the pairing state bundle: at most
    /// ``StateBundle/maxPredecessors``, the oldest always among them.
    static func ownPredecessorRecords(
        seeds: [Data], adoptedAtMs: (_ keyB64: String) -> Int64
    ) -> [BackupOwnPredecessor] {
        StateBundle.carriedOwnPredecessors(seeds: seeds, adoptedAtMs: adoptedAtMs).compactMap { own in
            Data(base64Encoded: own.seedB64).map {
                BackupOwnPredecessor(seed: $0, adoptedAtMs: UInt64(max(0, own.adoptedAtMs)))
            }
        }
    }

    /// Compose the backup payload bytes (wire v7). Pure: no clock beyond
    /// `createdAtMs`, no network, no store.
    ///
    /// There is no MLS snapshot argument, and none may be added: see the type
    /// comment. A row of `knownGroupIdsHex` that is not a group id is dropped
    /// rather than failing the backup, so one malformed local entry never
    /// costs the whole envelope.
    static func composePayload(
        accountId: Data,
        createdAtMs: UInt64,
        ed25519Seed: Data,
        contacts: [BackupContactRecord],
        knownGroupIdsHex: [String],
        syncedState: Data?,
        ownPredecessors: [BackupOwnPredecessor] = [],
        own: OwnBackupEnvelopes = OwnBackupEnvelopes()
    ) throws -> Data {
        try buildBackupPayload(
            accountId: accountId,
            createdAtMs: createdAtMs,
            ed25519Seed: ed25519Seed,
            contacts: contacts,
            knownGroupIds: knownGroupIdsHex.compactMap(Self.groupIdBytes(fromHex:)),
            syncedState: syncedState,
            ownPredecessors: ownPredecessors.isEmpty ? nil : ownPredecessors,
            ownProfile: own.ownProfile,
            visibility: own.visibility,
            receivedProfiles: own.receivedProfiles.isEmpty ? nil : own.receivedProfiles
        )
    }

    /// Bind / rebind the bearer token used for the `/backup` routes AND the
    /// `/auth/backup/head` mint (ADR-0037). Mirror of the web
    /// `BackupClient.setSessionToken`.
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
        /// The ids of the account's conversations, hex: the groups this device
        /// holds plus the ones a restored backup named that it has not joined
        /// (``RestoredKnownGroupsStore/uploadGroupIds(own:restored:)``), so a
        /// restored device knows which conversations exist before any member
        /// has admitted it.
        knownGroupIdsHex: [String] = [],
        /// ADR-0058 section 5.2 — `SyncedState.toBytes()`, or `nil`.
        syncedState: Data? = nil
    ) async throws {
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        // A key with no recorded adoption is stamped now, exactly as the
        // pairing bundle and the mailbox poll stamp it.
        let predecessors = Self.ownPredecessorRecords(
            seeds: ownPredecessors.load(accountIdB64: accountIdB64)
        ) { adoptions.recordIfAbsent(accountIdB64: accountIdB64, keyB64: $0, nowMs: nowMs) }
        let payloadBytes = try Self.composePayload(
            accountId: accountId,
            createdAtMs: UInt64(nowMs),
            ed25519Seed: ed25519Seed,
            contacts: contacts,
            knownGroupIdsHex: knownGroupIdsHex,
            syncedState: syncedState,
            ownPredecessors: predecessors,
            own: await ownEnvelopes(accountIdB64)
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
                baseURL: authBaseURL,
                session: authSession
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
        knownGroupIdsHex: [String] = [],
        syncedState: Data? = nil
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
                knownGroupIdsHex: knownGroupIdsHex,
                syncedState: syncedState
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
    ///
    /// Refused (``AccountStoreGate/Sealed``) when the account was wiped on
    /// this device: this client is not main-actor bound, so a caller may run
    /// it off the main actor or after a wait, and a phrase written after
    /// Forget device would put the forgotten account's recovery phrase back.
    /// `since` is the account's ``AccountStoreGate/WipeMark`` taken when the
    /// work began, for a caller whose session has not landed yet (the account
    /// may still be sealed by an earlier sign-out then, which refuses
    /// nothing). Without it the caller is a signed-in session (the one
    /// caller today, the key rotation's re-seal, on the main actor), and a
    /// wipe not yet followed by a new landing refuses.
    public func cachePhrase(
        accountIdB64: String, phraseBytes: Data, since: AccountStoreGate.WipeMark? = nil
    ) throws {
        let gate = try since.map { try AccountStoreGate.enterOrEnd(unlessWipedSince: $0) }
            ?? AccountStoreGate.enterUnlessWipedOrEnd(accountIdB64)
        defer { gate.leave() }
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

/// What a backup says about the account itself: its own profile (name,
/// username, photo references, with the profile's own stamp) and its "who can
/// find me" setting, each as the core's envelope. `nil` for one that was
/// never set.
struct OwnBackupEnvelopes: Equatable {
    var ownProfile: Data?
    var visibility: Data?
    /// The names this device holds for other people (ADR-0048 profiles), so a
    /// device restored with no other device alive names its contacts and
    /// group members at once. The core keeps what fits its bounds, in order.
    var receivedProfiles: [BackupReceivedProfile] = []

    /// What this device holds for `accountIdB64` now, whether or not its
    /// session is up: a baseline can go out before the shell mounts.
    @MainActor
    static func current(accountIdB64: String) -> OwnBackupEnvelopes {
        OwnBackupEnvelopes(
            ownProfile: ProfileDirectory.shared.backupProfileEnvelope(accountIdB64: accountIdB64),
            visibility: ReachCenter.shared.backupSettingEnvelope(accountIdB64: accountIdB64),
            receivedProfiles: receivedProfileRecords(
                profiles: ProfileDirectory.shared.receivedProfiles(accountIdB64: accountIdB64),
                contactKeysB64: AcceptedContactsStore.load(accountIdB64: accountIdB64)
                    .map { base64Encode($0.request.fromPubkey) },
                recentPeopleB64: BackupPeopleOrder.recent(),
                ownKeysB64: StateBundle.ownKeysB64(accountIdB64: accountIdB64)))
    }

    /// The received profiles a backup carries, in the order the core keeps
    /// them when it must cut: the account's contacts first (in contact
    /// order), then everyone else by recent use (``BackupPeopleOrder``), then
    /// the rest in key order. Never this account's own keys. Each entry is
    /// the person key the profile is stored under and its envelope, composed
    /// from the stored profile with its original stamp (grants are never
    /// kept, so none travel).
    static func receivedProfileRecords(
        profiles: [String: ProfileClaim],
        contactKeysB64: [String],
        recentPeopleB64: [String],
        ownKeysB64: Set<String>
    ) -> [BackupReceivedProfile] {
        var order: [String] = []
        var seen = Set<String>()
        for key in contactKeysB64 + recentPeopleB64 + profiles.keys.sorted()
        where profiles[key] != nil && !ownKeysB64.contains(key) && seen.insert(key).inserted {
            order.append(key)
        }
        return order.compactMap { key in
            guard let personKey = Data(base64Encoded: key), personKey.count == 32,
                  let claim = profiles[key],
                  let envelope = StateBundle.profileEnvelope(claim) else { return nil }
            return BackupReceivedProfile(personKey: personKey, envelope: envelope)
        }
    }
}

/// Who this device talked with most recently, as person keys (base64), most
/// recent first: the order a backup keeps received names in after contacts.
/// The chat repository installs the source at bootstrap; with none, no order.
@MainActor
enum BackupPeopleOrder {
    static var source: (() -> [String])?

    static func recent() -> [String] { source?() ?? [] }
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
