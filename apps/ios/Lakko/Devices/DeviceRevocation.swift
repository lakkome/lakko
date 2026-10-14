// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0074 section 2.3 — removing ANOTHER device of the account, as one
// operation with an owed record.
//
// The order, on the removing device:
//
//   1. a fresh proof (the surface's proof block);
//   2. `POST /account/devices/revoke`. If it fails, STOP: nothing else has
//      happened, and the screen says so;
//   3. `DELETE /discovery/keypackage/device/{id}` with the account statement
//      (first, not last: it is what closes the directory);
//   4. the DeviceGroup Remove commit (attested when the policy is on);
//   5. the per-chat leaf removal.
//
// Steps 3 to 5 are recorded as owed the moment step 2 answered, and retried
// at every session landing until each has an answer, so a crash or an
// offline step never leaves a device revoked at auth but still in the groups
// with nothing to retry it. The statement is signed right before each
// request and never stored (it is valid 120 s).
//
//   * `DeviceRemovalRecord` / `DeviceRemovalPlan`   the pure state machine;
//   * `DeviceRemovalStore`                          the owed records;
//   * `DeviceRemovalRunner`                         the flow over injected ports.

/// What a removal still owes after the device was signed out at auth.
/// Holds a public device id and three flags; no key material.
public struct DeviceRemovalRecord: Codable, Equatable, Identifiable {
    /// Standard base64 of the removed device's 32-byte id.
    public let deviceIdB64: String
    /// When auth answered the revoke (for the log only).
    public let revokedAtMs: Int64
    /// The directory row is deleted.
    public var directoryDone: Bool = false
    /// The DeviceGroup no longer holds the device's leaf.
    public var deviceGroupDone: Bool = false
    /// Every conversation this device is in has answered the leaf removal.
    public var chatsDone: Bool = false

    public var id: String { deviceIdB64 }

    public init(deviceIdB64: String, revokedAtMs: Int64) {
        self.deviceIdB64 = deviceIdB64
        self.revokedAtMs = revokedAtMs
    }

    enum CodingKeys: String, CodingKey {
        case deviceIdB64 = "device_id"
        case revokedAtMs = "revoked_at_ms"
        case directoryDone = "directory_done"
        case deviceGroupDone = "device_group_done"
        case chatsDone = "chats_done"
    }
}

/// The steps and the verdicts, pure.
public enum DeviceRemovalPlan {

    /// One owed step, in the order they run.
    public enum Step: String, Equatable, CaseIterable {
        case directory
        case deviceGroup
        case chats
    }

    /// The steps still to run for `record`, in order.
    public static func remaining(_ record: DeviceRemovalRecord) -> [Step] {
        var steps: [Step] = []
        if !record.directoryDone { steps.append(.directory) }
        if !record.deviceGroupDone { steps.append(.deviceGroup) }
        if !record.chatsDone { steps.append(.chats) }
        return steps
    }

    public static func isComplete(_ record: DeviceRemovalRecord) -> Bool {
        remaining(record).isEmpty
    }

    public static func marking(_ record: DeviceRemovalRecord, done step: Step) -> DeviceRemovalRecord {
        var next = record
        switch step {
        case .directory: next.directoryDone = true
        case .deviceGroup: next.deviceGroupDone = true
        case .chats: next.chatsDone = true
        }
        return next
    }

    /// Whether a removal may start at all.
    public enum Start: Equatable {
        case proceed
        /// A key-reset publish is owed: auth's recorded key and the
        /// directory's differ in that window (ADR-0074 "T1 as built" point
        /// 15), so the two halves of a removal would be checked under two
        /// keys. The surface shows the owed copy.
        case blockedPublishOwed
        /// This device holds no account keys, so it cannot sign the
        /// statement: it cannot remove devices (UX 18.9).
        case noAccountKeys
        /// The device named is this device (Forget this device is the way).
        case ownDevice
    }

    public static func canStart(
        publishOwed: Bool,
        holdsAccountKeys: Bool,
        targetIsOwnDevice: Bool
    ) -> Start {
        if !holdsAccountKeys { return .noAccountKeys }
        if targetIsOwnDevice { return .ownDevice }
        if publishOwed { return .blockedPublishOwed }
        return .proceed
    }

    /// What `POST /account/devices/revoke` came to.
    public enum RevokeOutcome: Equatable {
        /// `204`: the device is signed out at the server.
        case revoked
        /// `403 fresh-auth-required`: the proof expired on the way.
        case freshAuthRequired
        /// `403 account-key-required`: auth has no recorded account key yet
        /// (or the statement was refused).
        case accountKeyRequired
        /// `429`.
        case rateLimited
        /// `401 device-revoked` / `key-generation-superseded`: THIS device
        /// is out; the session layer acts on it.
        case sessionEnded
        /// No answer.
        case offline
        /// Anything else (`400`, `5xx`).
        case failed
    }

    public static func revokeOutcome(status: Int, body: String) -> RevokeOutcome {
        let code = errorCode(body)
        switch (status, code) {
        case (200..<300, _): return .revoked
        case (403, "fresh-auth-required"): return .freshAuthRequired
        case (403, "account-key-required"): return .accountKeyRequired
        case (429, _): return .rateLimited
        case (401, "device-revoked"), (401, "key-generation-superseded"): return .sessionEnded
        default: return .failed
        }
    }

    public static func revokeOutcome(of error: Error?) -> RevokeOutcome {
        guard let error else { return .revoked }
        switch error as? AuthClientError {
        case .http(let status, _, let body): return revokeOutcome(status: status, body: body)
        case .network: return .offline
        default: return .failed
        }
    }

    /// What the directory delete came to.
    public enum DirectoryOutcome: Equatable {
        /// `204`, or `404` (the row is gone, which is the goal).
        case done
        /// `403 account-key-required`, offline, `5xx`: owed, retried later.
        case retry
        /// `401`: this device's session ended meanwhile.
        case sessionEnded
    }

    public static func directoryOutcome(status: Int) -> DirectoryOutcome {
        switch status {
        case 200..<300, 404: return .done
        case 401: return .sessionEnded
        default: return .retry
        }
    }

    public static func directoryOutcome(of error: Error?) -> DirectoryOutcome {
        guard let error else { return .done }
        if case .http(let status, _, _) = error as? DiscoveryClientError {
            return directoryOutcome(status: status)
        }
        return .retry
    }

    /// What the DeviceGroup step found.
    public enum DeviceGroupOutcome: Equatable {
        /// The Remove commit was published, or no leaf holds the key.
        case done
        /// The attested-removal policy is on and no attestation is in hand
        /// (a retry in the background cannot run the passkey gesture): owed
        /// until the person retries from the Devices screen.
        case needsAttestation
    }

    /// What the interactive run of a removal ended as (UX 18.1's states).
    public enum Result: Equatable {
        /// Signed out at the server and out of everything.
        case removed
        /// Signed out at the server; the rest finishes by itself.
        case removedPending
        /// The device could not be signed out, so nothing was removed.
        case notRevoked(RevokeOutcome)
        /// The removal did not start.
        case notStarted(Start)
    }

    public static func result(after record: DeviceRemovalRecord) -> Result {
        isComplete(record) ? .removed : .removedPending
    }

    /// What `Finish removing` on a device row came to.
    public enum FinishResult: Equatable {
        /// Every owed step has an answer: the device is out of everything.
        case removed
        /// Something is still owed and finishes by itself (offline, a chat
        /// that did not answer).
        case pending
        /// The passkey sheet was dismissed or failed: nothing changed, the
        /// button stays.
        case cancelled
        /// Nothing was owed for the device (it finished meanwhile).
        case nothingOwed
    }

    /// Whether an owed removal is waiting on the PERSON: only when its
    /// DeviceGroup step is still owed and the last run found that the
    /// attested-removal policy needs a passkey gesture nothing in the
    /// background can make.
    public static func waitsOnPerson(_ record: DeviceRemovalRecord, deviceGroupNeedsAttestation: Bool) -> Bool {
        !record.deviceGroupDone && deviceGroupNeedsAttestation
    }

    private static func errorCode(_ body: String) -> String? {
        ((try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any])?["error"] as? String
    }
}

/// The owed removals of one account, in `UserDefaults`. Cleared by the
/// account wipe.
public struct DeviceRemovalStore {
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String {
        "lakko.device-removal-owed.v1.\(accountIdB64)"
    }

    public func load(accountIdB64: String) -> [DeviceRemovalRecord] {
        guard let data = defaults.data(forKey: Self.key(accountIdB64: accountIdB64)) else { return [] }
        return (try? JSONDecoder().decode([DeviceRemovalRecord].self, from: data)) ?? []
    }

    /// Store `record`, replacing the one for the same device, or drop it
    /// when nothing is owed any more.
    public func put(_ record: DeviceRemovalRecord, accountIdB64: String) {
        var all = load(accountIdB64: accountIdB64).filter { $0.deviceIdB64 != record.deviceIdB64 }
        if !DeviceRemovalPlan.isComplete(record) { all.append(record) }
        save(all, accountIdB64: accountIdB64)
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }

    private func save(_ records: [DeviceRemovalRecord], accountIdB64: String) {
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        if records.isEmpty {
            clear(accountIdB64: accountIdB64)
        } else if let data = try? JSONEncoder().encode(records) {
            defaults.set(data, forKey: Self.key(accountIdB64: accountIdB64))
        }
    }
}

/// The removal flow over injected ports. One per process; the composition
/// root wires the ports.
@MainActor
public final class DeviceRemovalRunner: ObservableObject {

    /// The session a removal runs under.
    public struct Session {
        public let accountId: Data
        public let accountIdB64: String
        /// The account identity seed this device holds (the statement is
        /// signed by the key it derives).
        public let accountIdentitySeed: Data
        /// This device's own id.
        public let ownDeviceId: Data?

        public init(accountId: Data, accountIdB64: String, accountIdentitySeed: Data, ownDeviceId: Data?) {
            self.accountId = accountId
            self.accountIdB64 = accountIdB64
            self.accountIdentitySeed = accountIdentitySeed
            self.ownDeviceId = ownDeviceId
        }
    }

    public struct Ports {
        /// The live session with keys, or `nil`.
        public var session: @MainActor () -> Session?
        /// The current access token.
        public var sessionToken: @MainActor () -> String?
        /// Whether a key-reset publish is owed for the account.
        public var publishOwed: @MainActor (_ accountIdB64: String) -> Bool
        /// The live fresh-auth grant.
        public var freshGrant: @MainActor () -> String?
        /// Sign the `device-revoke` statement (138 bytes) for `deviceId`.
        public var sign: @MainActor (_ session: Session, _ deviceId: Data) throws -> Data
        /// `POST /account/devices/revoke`.
        public var revoke: (_ token: String, _ grant: String, _ deviceId: Data, _ statement: Data) async throws -> Void
        /// `DELETE /discovery/keypackage/device/{id}` with the statement.
        public var deleteDirectoryRow: (_ token: String, _ deviceId: Data, _ statement: Data) async throws -> Void
        /// The DeviceGroup Remove commit for the leaf holding `deviceId`.
        public var removeFromDeviceGroup: @MainActor (
            _ session: Session, _ deviceId: Data, _ attestation: Data?
        ) async throws -> DeviceRemovalPlan.DeviceGroupOutcome
        /// The per-chat leaf removal; `true` when every chat answered.
        public var retireFromChats: @MainActor (_ deviceId: Data) async -> Bool
        /// Auth answered `account-key-required`: record the account key now
        /// (the genesis mint), so the next try can pass.
        public var recordAccountKey: @MainActor () -> Void
        public var nowMs: () -> Int64
        /// The ADR-0015 passkey gesture bound to `deviceId`, for `Finish
        /// removing`: the attestation an owed DeviceGroup step needs after a
        /// relaunch.
        var attestation: @MainActor (_ deviceId: Data) async -> RemoveDeviceAttestation.Outcome = { _ in
            .notNeeded
        }
    }

    private let ports: Ports
    private let store: DeviceRemovalStore
    private var running = false

    /// The device ids (base64) whose removal is signed out at the server
    /// and still finishing, for the Devices rows.
    @Published public private(set) var pendingDeviceIds: Set<String> = []

    /// The pending device ids whose removal is waiting on the person: the
    /// DeviceGroup step needs a passkey gesture (ADR-0015). Their rows carry
    /// `Finish removing`. In memory: every run of the owed steps finds it
    /// again.
    @Published public private(set) var waitingOnPersonDeviceIds: Set<String> = []
    /// A `Finish removing` is running for these ids.
    @Published public private(set) var finishingDeviceIds: Set<String> = []

    public init(ports: Ports, store: DeviceRemovalStore = DeviceRemovalStore()) {
        self.ports = ports
        self.store = store
    }

    /// Re-read the owed set (a landing, an account switch).
    public func reload() {
        guard let session = ports.session() else {
            pendingDeviceIds = []
            waitingOnPersonDeviceIds = []
            return
        }
        let owed = store.load(accountIdB64: session.accountIdB64)
        pendingDeviceIds = Set(owed.map(\.deviceIdB64))
        // A device whose DeviceGroup step landed (or whose record is gone)
        // waits on nobody.
        let stillWaiting = Set(owed.filter { !$0.deviceGroupDone }.map(\.deviceIdB64))
        waitingOnPersonDeviceIds.formIntersection(stillWaiting)
    }

    /// `Finish removing` on a device row: run the passkey gesture for the
    /// device, then every step its removal still owes. A device revoked at
    /// auth is never left in the DeviceGroup with no way to finish.
    public func finish(deviceId: Data) async -> DeviceRemovalPlan.FinishResult {
        let deviceIdB64 = deviceId.base64EncodedString()
        guard !finishingDeviceIds.contains(deviceIdB64), let session = ports.session(),
              let owed = store.load(accountIdB64: session.accountIdB64)
                .first(where: { $0.deviceIdB64 == deviceIdB64 }) else {
            reload()
            return .nothingOwed
        }
        finishingDeviceIds.insert(deviceIdB64)
        defer { finishingDeviceIds.remove(deviceIdB64) }
        let attestation: Data?
        switch await ports.attestation(deviceId) {
        case .notNeeded: attestation = nil
        case .obtained(let bytes): attestation = bytes
        case .cancelled: return .cancelled
        }
        let after = await runOwed(owed, session: session, attestation: attestation)
        return DeviceRemovalPlan.isComplete(after) ? .removed : .pending
    }

    /// Whether a removal of `deviceId` may start.
    public func canStart(deviceId: Data) -> DeviceRemovalPlan.Start {
        guard let session = ports.session() else { return .noAccountKeys }
        return DeviceRemovalPlan.canStart(
            publishOwed: ports.publishOwed(session.accountIdB64),
            holdsAccountKeys: !session.accountIdentitySeed.isEmpty,
            targetIsOwnDevice: session.ownDeviceId == deviceId)
    }

    /// Remove `deviceId`: revoke at auth, then the owed steps. The caller
    /// obtained the fresh proof (and the attestation, when the account has a
    /// passkey) first.
    public func remove(deviceId: Data, attestation: Data?) async -> DeviceRemovalPlan.Result {
        let start = canStart(deviceId: deviceId)
        guard start == .proceed, let session = ports.session() else {
            return .notStarted(start == .proceed ? .noAccountKeys : start)
        }
        let deviceIdB64 = deviceId.base64EncodedString()
        // A removal already signed out at the server: only the rest is owed.
        if let owed = store.load(accountIdB64: session.accountIdB64).first(where: { $0.deviceIdB64 == deviceIdB64 }) {
            let after = await runOwed(owed, session: session, attestation: attestation)
            return DeviceRemovalPlan.result(after: after)
        }
        guard let grant = ports.freshGrant() else { return .notRevoked(.freshAuthRequired) }
        guard let token = ports.sessionToken() else { return .notRevoked(.failed) }
        var revokeError: Error?
        do {
            let statement = try ports.sign(session, deviceId)
            try await ports.revoke(token, grant, deviceId, statement)
        } catch {
            revokeError = error
        }
        let outcome = DeviceRemovalPlan.revokeOutcome(of: revokeError)
        guard outcome == .revoked else {
            NSLog("[devices] WARN the device was not signed out at the server (%@); nothing was removed",
                  String(describing: outcome))
            if outcome == .accountKeyRequired { ports.recordAccountKey() }
            return .notRevoked(outcome)
        }
        // Signed out at the server: everything after this is owed.
        let record = DeviceRemovalRecord(deviceIdB64: deviceIdB64, revokedAtMs: ports.nowMs())
        store.put(record, accountIdB64: session.accountIdB64)
        reload()
        let after = await runOwed(record, session: session, attestation: attestation)
        return DeviceRemovalPlan.result(after: after)
    }

    /// Retry every owed removal of the signed-in account. Called at every
    /// session landing; a second call while one runs is dropped.
    public func resumeOwed() async {
        guard !running, let session = ports.session() else { return }
        guard !ports.publishOwed(session.accountIdB64) else { return }
        running = true
        defer { running = false }
        for record in store.load(accountIdB64: session.accountIdB64) {
            _ = await runOwed(record, session: session, attestation: nil)
        }
        reload()
    }

    private func runOwed(
        _ record: DeviceRemovalRecord,
        session: Session,
        attestation: Data?
    ) async -> DeviceRemovalRecord {
        var record = record
        guard let deviceId = Data(base64Encoded: record.deviceIdB64) else {
            // A record that names no device cannot be retried: drop it.
            for step in DeviceRemovalPlan.Step.allCases {
                record = DeviceRemovalPlan.marking(record, done: step)
            }
            store.put(record, accountIdB64: session.accountIdB64)
            reload()
            return record
        }
        for step in DeviceRemovalPlan.remaining(record) {
            var landed = false
            switch step {
            case .directory:
                var failure: Error?
                do {
                    guard let token = ports.sessionToken() else { throw DeviceRemovalNoSession() }
                    // Signed right before the request, never stored.
                    let statement = try ports.sign(session, deviceId)
                    try await ports.deleteDirectoryRow(token, deviceId, statement)
                } catch {
                    failure = error
                }
                landed = DeviceRemovalPlan.directoryOutcome(of: failure) == .done
                if let failure, !landed {
                    NSLog("[devices] WARN the removed device's directory row is still owed: %@",
                          String(describing: type(of: failure)))
                }
            case .deviceGroup:
                do {
                    let outcome = try await ports.removeFromDeviceGroup(session, deviceId, attestation)
                    landed = outcome == .done
                    // Owed until the person makes the gesture: the row
                    // offers `Finish removing`.
                    if DeviceRemovalPlan.waitsOnPerson(
                        record, deviceGroupNeedsAttestation: outcome == .needsAttestation) {
                        waitingOnPersonDeviceIds.insert(record.deviceIdB64)
                    } else {
                        waitingOnPersonDeviceIds.remove(record.deviceIdB64)
                    }
                } catch {
                    NSLog("[devices] WARN the DeviceGroup removal is still owed: %@",
                          String(describing: type(of: error)))
                }
            case .chats:
                landed = await ports.retireFromChats(deviceId)
            }
            if landed {
                record = DeviceRemovalPlan.marking(record, done: step)
                store.put(record, accountIdB64: session.accountIdB64)
            }
            // A step that did not land does not hold the later ones back:
            // each is independent once the device is signed out, and the
            // directory row in particular must not wait for a conversation.
        }
        reload()
        return record
    }
}

/// No access token was in hand for an owed step.
struct DeviceRemovalNoSession: Error {}

extension DeviceRemovalRunner.Ports {

    /// The production ports: the auth and discovery clients, the DeviceGroup
    /// removal on a handle of its own (after which the sync runtime reloads),
    /// and the chat repository's per-chat leaf removal.
    @MainActor
    static func live(
        auth: AuthRepository,
        chat: ChatRepository,
        sync: DeviceGroupSyncRepository
    ) -> DeviceRemovalRunner.Ports {
        DeviceRemovalRunner.Ports(
            session: { [weak auth] in
                guard let auth, let secrets = auth.identitySecrets, case .session = auth.state else {
                    return nil
                }
                let accountIdB64 = base64Encode(secrets.accountId)
                return DeviceRemovalRunner.Session(
                    accountId: secrets.accountId,
                    accountIdB64: accountIdB64,
                    accountIdentitySeed: secrets.accountIdentitySeed,
                    ownDeviceId: auth.deviceId(accountIdB64: accountIdB64))
            },
            sessionToken: { [weak auth] in auth?.identitySecrets?.sessionToken },
            publishOwed: { [weak auth] accountIdB64 in
                auth?.keyResetPublishOwed(accountIdB64: accountIdB64) ?? true
            },
            freshGrant: { [weak auth] in auth?.liveFreshGrant() },
            sign: { session, deviceId in
                // The statement is signed by the account identity's Ed25519
                // key, right before the request it goes into.
                let account = try Identity.fromSeed(seed: session.accountIdentitySeed)
                return try signDeviceRevokeStatement(
                    accountSeed: account.ed25519Seed(),
                    accountId: session.accountId,
                    deviceId: deviceId)
            },
            revoke: { token, grant, deviceId, statement in
                try await AuthClient().revokeDevice(
                    sessionToken: token, freshGrant: grant, deviceId: deviceId, statement: statement)
            },
            deleteDirectoryRow: { token, deviceId, statement in
                try await DiscoveryClient(sessionToken: token)
                    .deleteKeyPackage(deviceId: deviceId, accountStatement: statement)
            },
            removeFromDeviceGroup: { [weak auth, weak sync] session, deviceId, attestation in
                let outcome = try await DeviceRemoval.removeDevice(
                    accountIdBytes: session.accountId,
                    accountIdB64: session.accountIdB64,
                    deviceKey: deviceId,
                    attestation: attestation)
                // The removal ran on a DeviceGroup handle of its own; the
                // sync runtime reloads from the snapshot it persisted.
                if outcome == .done, let secrets = auth?.identitySecrets {
                    sync?.reloadAfterLocalMembershipChange(secrets: secrets)
                }
                return outcome
            },
            retireFromChats: { [weak chat] deviceId in
                guard let chat else { return false }
                let tally = await chat.retireDevice(deviceKey: deviceId)
                NSLog("ADR-0074 retire device: offered %d, skipped %d, failed %d",
                      tally.offered, tally.skipped, tally.failed)
                return tally.failed == 0
            },
            recordAccountKey: { [weak auth] in auth?.recordIdentityKeyNow() },
            nowMs: { Int64(Date().timeIntervalSince1970 * 1000) },
            attestation: { [weak auth] deviceId in
                guard let token = auth?.identitySecrets?.sessionToken else { return .cancelled }
                return await RemoveDeviceAttestation.obtain(sessionToken: token, deviceKey: deviceId)
            }
        )
    }
}
