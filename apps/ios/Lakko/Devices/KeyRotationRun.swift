// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0066 section 3.2 — the ROTATING device: Settings > Devices > Remove >
// "It was lost or stolen" (docs/ux/identity-recovery.md section 18.1). Remove
// the lost device AND change the account keys, keeping the other devices.
//
// One owed operation, resumable (the pattern of `DeviceRevocation.swift`):
//
//   1. a fresh proof; the owed record is written before anything is sent;
//   2. remove the device (revoke at auth, directory row, DeviceGroup Remove).
//      WAIT until the device is out of the DeviceGroup and the epoch advanced;
//   3. generate the new phrase and seed; sign the supersession proof with the
//      OLD seed (14 days);
//   4. PREPARE: the `key-rotation` envelope, into the DeviceGroup ONLY, sealed
//      at the POST-removal epoch (``KeyRotationAnnounce``, the one seam);
//   5. COMMIT: `POST /account/keys/rotate/{start,finish}`. From here the
//      account is at the new generation;
//   6. adopt locally (new seed and phrase, the old key kept as predecessor,
//      every local store kept);
//   7. the owed steps: the directory upload with proof AND attestation, this
//      device's KeyPackage, the DeviceGroup journal's root transfer, the
//      journals of owned groups, the owner key of owned channels, the new
//      mailbox and reach keys, the backup;
//   8. the new words, withheld while the publish is owed.
//
// A failure before step 5 changes nothing on the server: the device WAS
// removed, the keys were not changed. A failure after it resumes from the
// record with the same attestation.
//
//   * `KeyRotationRecord` / `KeyRotationPlan`   the pure state machine;
//   * `KeyRotationStore`                        the owed record;
//   * `KeyRotationAnnounce`                     the envelope-after-removal seam;
//   * `KeyRotationRunner`                       the flow over injected ports.

/// What a rotation still owes. Public artefacts and flags only: the new seed
/// lives in the pending store until the commit and in the account's own
/// stores after it; the old seed in the predecessor slot.
public struct KeyRotationRecord: Codable, Equatable {

    public enum Phase: String, Codable, Equatable {
        /// Written before anything was sent: the removal is running.
        case removing
        /// The device is out of the DeviceGroup; the keys are unchanged.
        case removed
        /// The `key-rotation` envelope was sent; the commit has not answered.
        case announced
        /// Auth answered `rotate/finish` (or this device learned the commit
        /// landed): the account is at the new generation.
        case committed
    }

    /// Standard base64 of the removed device's 32-byte id.
    public let targetDeviceIdB64: String
    public let startedAtMs: Int64
    public var phase: Phase
    /// The DeviceGroup epoch the `key-rotation` envelope must be sealed at or
    /// above: the epoch the removal produced. Set when the removal starts.
    public var announceEpochFloor: UInt64?
    /// The NEW account key the proof authorises (set with the envelope).
    public var newSignaturePubkey: Data?
    /// The 145-byte supersession proof (a public artefact).
    public var supersessionProof: Data?
    /// The 177-byte attestation of `rotate/finish`. `nil` when the answer was
    /// lost and the commit was learned by adoption: the owed steps then run
    /// without it.
    public var attestation: Data?
    public var keyGeneration: UInt64?
    public var discoveryDone = false
    public var keyPackageDone = false
    public var deviceGroupRootDone = false
    public var journalsDone = false
    public var channelsDone = false
    public var reachDone = false
    /// ADR-0066 section 3.5a rule 3 — the profile re-send. Optional so a
    /// record written before the step existed still decodes (as owed).
    public var profileDone: Bool?
    /// ADR-0066 section 3.5a rule 5 — the links revoked and the unanswered
    /// contact requests re-sent. Optional for the same reason.
    public var grantsDone: Bool?
    public var backupDone = false
    /// The person saw and confirmed the new words.
    public var phraseAcknowledged = false
    /// ADR-0067 section 3 — this rotation is "Save a recovery phrase": no
    /// device is removed (``targetDeviceIdB64`` is empty) and the commit
    /// marks the account phrase-protected. Optional so an older record
    /// still decodes.
    public var phraseSaved: Bool?

    /// Whether this record is a "Save a recovery phrase" rotation.
    public var savesPhrase: Bool { phraseSaved == true }

    public init(targetDeviceIdB64: String, startedAtMs: Int64) {
        self.targetDeviceIdB64 = targetDeviceIdB64
        self.startedAtMs = startedAtMs
        self.phase = .removing
    }

    enum CodingKeys: String, CodingKey {
        case targetDeviceIdB64 = "target_device_id"
        case startedAtMs = "started_at_ms"
        case phase
        case announceEpochFloor = "announce_epoch_floor"
        case newSignaturePubkey = "new_signature_pubkey"
        case supersessionProof = "supersession_proof"
        case attestation
        case keyGeneration = "key_generation"
        case discoveryDone = "discovery_done"
        case keyPackageDone = "key_package_done"
        case deviceGroupRootDone = "device_group_root_done"
        case journalsDone = "journals_done"
        case channelsDone = "channels_done"
        case reachDone = "reach_done"
        case profileDone = "profile_done"
        case grantsDone = "grants_done"
        case backupDone = "backup_done"
        case phraseAcknowledged = "phrase_acknowledged"
        case phraseSaved = "phrase_saved"
    }
}

/// The steps and the verdicts, pure.
public enum KeyRotationPlan {

    /// One step owed AFTER the commit, in the order they run.
    public enum Step: String, Equatable, CaseIterable {
        /// `POST /discovery/identity` with the proof and the attestation.
        case discovery
        /// This device's KeyPackage, re-published under the same device id.
        case keyPackage
        /// `PUT /ds/group/{hex}/root` for the DeviceGroup journal.
        case deviceGroupRoot
        /// The root transfer and the registrars of every owned group.
        case journals
        /// `PUT /v1/channels/{id}/owner` for every owned channel.
        case channels
        /// The new mailbox claim and the reach keys.
        case reach
        /// The own profile, with grants for the new mailbox, re-sent to every
        /// one-to-one and group (section 3.5a rule 3). Runs only after
        /// ``reach``: a grant must name a key the mailbox already knows.
        case profile
        /// What was granted before the rotation is spent (section 3.5a rule
        /// 5): this account's own `/i/` and `/c/` links are revoked at their
        /// ledger and its unanswered contact requests are sent again from the
        /// new key. Runs only after ``discovery`` (the list of links is
        /// signed by the published key) and ``reach`` (a re-sent request
        /// carries a reply grant for the new mailbox).
        case grants
        /// The backup, re-sealed under the new phrase.
        case backup
    }

    public static func remaining(_ record: KeyRotationRecord) -> [Step] {
        guard record.phase == .committed else { return [] }
        var steps: [Step] = []
        if !record.discoveryDone { steps.append(.discovery) }
        if !record.keyPackageDone { steps.append(.keyPackage) }
        if !record.deviceGroupRootDone { steps.append(.deviceGroupRoot) }
        if !record.journalsDone { steps.append(.journals) }
        if !record.channelsDone { steps.append(.channels) }
        if !record.reachDone { steps.append(.reach) }
        if record.profileDone != true { steps.append(.profile) }
        if record.grantsDone != true { steps.append(.grants) }
        if !record.backupDone { steps.append(.backup) }
        return steps
    }

    public static func marking(_ record: KeyRotationRecord, done step: Step) -> KeyRotationRecord {
        var next = record
        switch step {
        case .discovery: next.discoveryDone = true
        case .keyPackage: next.keyPackageDone = true
        case .deviceGroupRoot: next.deviceGroupRootDone = true
        case .journals: next.journalsDone = true
        case .channels: next.channelsDone = true
        case .reach: next.reachDone = true
        case .profile: next.profileDone = true
        case .grants: next.grantsDone = true
        case .backup: next.backupDone = true
        }
        return next
    }

    /// The steps the new WORDS wait for (UX 18.1, "Keys changed, publishing
    /// still owed"): until the directory, this device's KeyPackage row and
    /// the account's device list know the new keys, the contacts and the
    /// other devices cannot find them. The rest finishes in the background.
    public static let stepsWithholdingWords: [Step] = [.discovery, .keyPackage, .deviceGroupRoot]

    /// Whether the new words are withheld.
    public static func publishOwed(_ record: KeyRotationRecord) -> Bool {
        record.phase == .committed
            && !remaining(record).filter { stepsWithholdingWords.contains($0) }.isEmpty
    }

    /// Whether every step landed AND the words were confirmed: the record is
    /// then cleared.
    public static func isComplete(_ record: KeyRotationRecord) -> Bool {
        record.phase == .committed && remaining(record).isEmpty && record.phraseAcknowledged
    }

    /// The line of the sheet's step list that is active (UX 18.1).
    public enum Stage: Equatable {
        /// `devices.rotate.step_remove`.
        case removingDevice
        /// `devices.rotate.step_keys`.
        case changingKeys
        /// `devices.rotate.step_devices`.
        case updatingDevices
    }

    public static func stage(of record: KeyRotationRecord) -> Stage {
        switch record.phase {
        case .removing: return .removingDevice
        case .removed, .announced: return .changingKeys
        case .committed: return .updatingDevices
        }
    }

    /// What a stored record means when the app comes back to it (a relaunch,
    /// a session landing, the Devices screen opening).
    public enum Resume: Equatable {
        /// Nothing stored.
        case nothing
        /// The removal never finished or the keys were never changed: "The
        /// device was removed, but your account keys were not changed"
        /// (`devices.rotate.error.not_started`), with `Try again` / `Not now`.
        /// Nothing changed on the server beyond the removal.
        case removedNotRotated
        /// The commit landed but its answer never arrived: this device now
        /// holds the new key (it adopted its own envelope like a kept
        /// device). The owed steps run, without an attestation.
        case commitLandedUnseen
        /// The commit landed: finish the owed steps with the same
        /// attestation.
        case finishOwed
        /// Only the words are still to be confirmed.
        case showWords
    }

    /// - Parameters:
    ///   - heldAccountKey: the account key this device holds now.
    public static func resume(_ record: KeyRotationRecord?, heldAccountKey: Data?) -> Resume {
        guard let record else { return .nothing }
        // A "Save a recovery phrase" rotation removed no device: before its
        // commit nothing changed and there is nothing to say.
        switch record.phase {
        case .removing, .removed:
            return record.savesPhrase ? .nothing : .removedNotRotated
        case .announced:
            if let new = record.newSignaturePubkey, new == heldAccountKey { return .commitLandedUnseen }
            return record.savesPhrase ? .nothing : .removedNotRotated
        case .committed:
            return remaining(record).isEmpty ? .showWords : .finishOwed
        }
    }

    // MARK: - The server's answers

    /// What `POST /account/keys/rotate/{start,finish}` came to.
    public enum CommitOutcome: Equatable {
        case committed
        /// `403 fresh-auth-required`: the proof expired on the way.
        case freshAuthRequired
        /// `403 account-key-required`: auth has no recorded account key, or
        /// the proof was not by it.
        case accountKeyRequired
        /// `429 too-many-attempts`.
        case rateLimited(retryAfterSecs: Int?)
        /// `401 device-revoked` / `key-generation-superseded`: THIS device is
        /// out; the session layer acts on it.
        case sessionEnded
        /// No answer. The commit may or may not have landed: the record
        /// stays at `announced` and the next session landing finds out.
        case offline
        case failed
    }

    public static func commitOutcome(status: Int, body: String) -> CommitOutcome {
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        let code = json?["error"] as? String
        switch (status, code) {
        case (200..<300, _): return .committed
        case (403, "fresh-auth-required"): return .freshAuthRequired
        case (403, "account-key-required"): return .accountKeyRequired
        case (429, _):
            return .rateLimited(retryAfterSecs: (json?["retry_after_secs"] as? NSNumber)?.intValue)
        case (401, "device-revoked"), (401, "key-generation-superseded"): return .sessionEnded
        default: return .failed
        }
    }

    public static func commitOutcome(of error: Error?) -> CommitOutcome {
        guard let error else { return .committed }
        switch error as? AuthClientError {
        case .http(let status, _, let body): return commitOutcome(status: status, body: body)
        case .network: return .offline
        default: return .failed
        }
    }

    /// What the proven-rotation upload to discovery came to.
    public enum DirectoryOutcome: Equatable {
        /// Landed, or already landed (`409 key-generation-not-newer`).
        case done
        /// `400 supersession-proof-invalid`: the row no longer holds the key
        /// the proof was signed by (the lost device published a key change of
        /// its own before it was revoked). Upload again WITHOUT the proof:
        /// discovery takes it as an attested reset at the new generation.
        case retryWithoutProof
        /// `401 key-generation-superseded`: this device was signed out.
        case superseded
        /// Retry later.
        case retry
    }

    public static func directoryOutcome(status: Int, body: String, sentProof: Bool) -> DirectoryOutcome {
        let code = ((try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any])?["error"]
            as? String
        switch (status, code) {
        case (200..<300, _): return .done
        case (409, "key-generation-not-newer"): return .done
        case (400, "supersession-proof-invalid"): return sentProof ? .retryWithoutProof : .retry
        case (401, "key-generation-superseded"): return .superseded
        default: return .retry
        }
    }

    public static func directoryOutcome(of error: Error?, sentProof: Bool) -> DirectoryOutcome {
        guard let error else { return .done }
        if case .http(let status, _, let body) = error as? DiscoveryClientError {
            return directoryOutcome(status: status, body: body, sentProof: sentProof)
        }
        return .retry
    }

    /// What the interactive run ended as (UX 18.1's states).
    public enum Result: Equatable {
        /// The keys were changed and published: show the new words.
        case rotated(phrase: String)
        /// The keys were changed; publishing is still owed: the words are
        /// withheld, it retries by itself.
        case rotatedPublishOwed
        /// The device WAS removed; the keys were not changed
        /// (`devices.rotate.error.not_started`).
        case removedNotRotated(CommitOutcome?)
        /// The device could not be removed, so nothing happened: the plain
        /// removal's own states.
        case notRemoved(DeviceRemovalPlan.Result)
        /// The proof ran out before the commit.
        case freshAuthRequired
        /// The keys were changed on an account that is not phrase-protected
        /// (ADR-0067): no words are shown anywhere. What is still owed is
        /// finished in the background.
        case rotatedNoWords
    }

    /// ADR-0067 — whether a rotation shows its new words: the save-phrase
    /// flow always (that is what it is for), any other rotation only on a
    /// phrase-protected account. An account that never saved a phrase has
    /// words nobody is meant to see.
    public static func showsNewWords(savesPhrase: Bool, phraseProtected: Bool) -> Bool {
        savesPhrase || phraseProtected
    }
}

/// The owed rotation of one account, in `UserDefaults`. Cleared by the
/// account wipe.
public struct KeyRotationStore {
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String {
        "lakko.key-rotation-owed.v1.\(accountIdB64)"
    }

    public func load(accountIdB64: String) -> KeyRotationRecord? {
        guard let data = defaults.data(forKey: Self.key(accountIdB64: accountIdB64)) else { return nil }
        return try? JSONDecoder().decode(KeyRotationRecord.self, from: data)
    }

    /// Store `record`, or drop it when nothing is owed any more.
    public func put(_ record: KeyRotationRecord, accountIdB64: String) {
        if KeyRotationPlan.isComplete(record) {
            clear(accountIdB64: accountIdB64)
        } else if let data = try? JSONEncoder().encode(record) {
            defaults.set(data, forKey: Self.key(accountIdB64: accountIdB64))
        }
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }

    /// Whether a rotation's publish is owed: the directory is BEHIND this
    /// device by design until it lands, so the landing rule must not read
    /// this device's own new keys as a mismatch, and no removal starts.
    public func publishOwed(accountIdB64: String) -> Bool {
        load(accountIdB64: accountIdB64).map(KeyRotationPlan.publishOwed) ?? false
    }
}

// MARK: - The envelope-after-removal seam

/// ADR-0066 section 3.2, "the stolen device is out first": the `key-rotation`
/// envelope carries the account's NEW seed, so it must be sealed at a
/// DeviceGroup epoch the removed device never held. Nothing else can check
/// this: not the crate, not any server. It is the SENDER's obligation, and
/// this is the ONE place it is discharged. The removal and the send are two
/// steps of one function so they cannot be reordered independently (the C-61
/// pattern), and the sealed epoch of the emitted message is returned so a
/// test asserts on it.
public enum KeyRotationAnnounce {

    /// The DeviceGroup, as the seam needs it.
    public struct Ports {
        /// The current epoch of the live DeviceGroup handle.
        public var epoch: () throws -> UInt64
        /// The device keys of the live DeviceGroup's leaves.
        public var roster: () throws -> [Data]
        /// Remove the target (revoke at auth, the directory row, the
        /// DeviceGroup Remove commit) and reload the live handle. Returns
        /// only once the commit was published or the device was already out.
        public var remove: () async throws -> Void
        /// Seal `envelope` as ONE application message of the DeviceGroup and
        /// publish it there, and nowhere else. Returns the epoch the emitted
        /// MLS message was sealed at.
        public var send: (_ envelope: Data) async throws -> UInt64

        public init(
            epoch: @escaping () throws -> UInt64,
            roster: @escaping () throws -> [Data],
            remove: @escaping () async throws -> Void,
            send: @escaping (_ envelope: Data) async throws -> UInt64
        ) {
            self.epoch = epoch
            self.roster = roster
            self.remove = remove
            self.send = send
        }
    }

    public enum Failure: Error, Equatable {
        /// The device is still a leaf of the DeviceGroup: nothing is sent.
        case deviceStillPresent
        /// The DeviceGroup did not move past the epoch the removed device
        /// held: nothing is sent.
        case epochNotAdvanced(current: UInt64, floor: UInt64)
        /// The message WAS emitted below the floor. Never expected (the two
        /// checks above run first); reported so the rotation stops before
        /// the commit.
        case sealedBeforeRemoval(sealed: UInt64, floor: UInt64)
    }

    public struct Announced: Equatable {
        /// The lowest epoch the envelope may be sealed at.
        public let floor: UInt64
        /// The epoch the emitted message was sealed at.
        public let sealedEpoch: UInt64
    }

    /// The floor for a removal about to run: one past the epoch the target
    /// still holds, or the current epoch when the target is already out (an
    /// earlier run removed it). A floor recorded by an earlier run wins.
    public static func floor(
        recorded: UInt64?, currentEpoch: UInt64, targetPresent: Bool
    ) -> UInt64 {
        if let recorded { return recorded }
        return targetPresent ? currentEpoch + 1 : currentEpoch
    }

    /// Remove `target`, THEN announce. `envelope` is built only after the
    /// removal landed, so the new seed does not exist while the lost device
    /// can still read the group. `onFloor` persists the floor before the
    /// removal starts (a retry after a crash keeps it).
    public static func removeThenAnnounce(
        target: Data,
        recordedFloor: UInt64?,
        onFloor: (UInt64) -> Void,
        ports: Ports,
        envelope: () throws -> Data
    ) async throws -> Announced {
        let before = try ports.epoch()
        let present = try ports.roster().contains(target)
        let floor = Self.floor(recorded: recordedFloor, currentEpoch: before, targetPresent: present)
        onFloor(floor)

        // 1. The removal. Nothing of the rotation exists yet.
        try await ports.remove()

        // 2. The device is out and the epoch advanced, as this device reads
        //    its own live DeviceGroup after the removal's reload.
        guard try !ports.roster().contains(target) else { throw Failure.deviceStillPresent }
        let current = try ports.epoch()
        guard current >= floor else { throw Failure.epochNotAdvanced(current: current, floor: floor) }

        // 3. The envelope, sealed at the post-removal epoch.
        let sealed = try await ports.send(try envelope())
        guard sealed >= floor else { throw Failure.sealedBeforeRemoval(sealed: sealed, floor: floor) }
        return Announced(floor: floor, sealedEpoch: sealed)
    }
}

/// The removal did not sign the device out (the plain removal's verdict).
struct KeyRotationRemovalFailed: Error {
    let result: DeviceRemovalPlan.Result
}

// MARK: - The runner

/// What the rotating device generated for one rotation.
public struct KeyRotationMaterial {
    public let phrase: String
    /// 16 bytes.
    public let entropy: Data
    /// The 64-byte phrase seed.
    public let seed: Data
    public let newSignaturePubkey: Data

    public init(phrase: String, entropy: Data, seed: Data, newSignaturePubkey: Data) {
        self.phrase = phrase
        self.entropy = entropy
        self.seed = seed
        self.newSignaturePubkey = newSignaturePubkey
    }

    /// A fresh 12-word phrase, as signup and a reset generate it.
    public static func generate() throws -> KeyRotationMaterial {
        let mnemonic = RecoveryMnemonic.generate()
        let seed = mnemonic.toSeed()
        return KeyRotationMaterial(
            phrase: mnemonic.phrase(),
            entropy: mnemonic.toEntropy(),
            seed: seed,
            newSignaturePubkey: try Identity.fromSeed(seed: seed).signaturePublicKey())
    }
}

/// The rotation flow over injected ports. One per process; the composition
/// root wires the ports.
@MainActor
public final class KeyRotationRunner: ObservableObject {

    public struct Session {
        public let accountId: Data
        public let accountIdB64: String
        /// The account identity seed this device holds NOW (the old one
        /// before the commit, the new one after it).
        public let accountIdentitySeed: Data
        public let ownDeviceId: Data?

        public init(accountId: Data, accountIdB64: String, accountIdentitySeed: Data, ownDeviceId: Data?) {
            self.accountId = accountId
            self.accountIdB64 = accountIdB64
            self.accountIdentitySeed = accountIdentitySeed
            self.ownDeviceId = ownDeviceId
        }
    }

    public struct Ports {
        public var session: @MainActor () -> Session?
        public var sessionToken: @MainActor () -> String?
        public var freshGrant: @MainActor () -> String?
        /// The plain removal (``DeviceRemovalRunner/remove(deviceId:attestation:)``).
        public var removeDevice: @MainActor (_ deviceId: Data, _ attestation: Data?) async -> DeviceRemovalPlan.Result
        /// The live DeviceGroup: its epoch, its roster, and the one send.
        public var deviceGroupEpoch: @MainActor () throws -> UInt64
        public var deviceGroupRoster: @MainActor () throws -> [Data]
        public var sendToDeviceGroup: @MainActor (_ envelope: Data) async throws -> UInt64
        public var generate: () throws -> KeyRotationMaterial
        /// `signSupersessionProof(old, new, issuedAt, expiresAt)` over the two
        /// 64-byte phrase seeds.
        public var signProof: (_ oldSeed: Data, _ newSeed: Data, _ issuedAtSecs: Int64, _ expiresAtSecs: Int64) throws -> Data
        public var composeEnvelope: (_ preparedAtMs: UInt64, _ entropy: Data, _ proof: Data) throws -> Data
        /// Keep this device's own prepared rotation as a pending envelope, so
        /// a commit whose answer is lost is adopted like a kept device's.
        public var storeOwnPending: @MainActor (_ pending: PendingKeyRotation, _ accountIdB64: String) throws -> Void
        /// `POST /account/keys/rotate/{start,finish}`.
        public var commit: (
            _ token: String, _ grant: String, _ material: KeyRotationMaterial, _ proof: Data,
            _ revokeDeviceIds: [Data]
        ) async throws -> KeyResetResult
        /// Step 6: the new seed, phrase and pair replace the old ones; the
        /// old seed goes to the predecessor slot; everything else is kept.
        public var adopt: @MainActor (_ material: KeyRotationMaterial, _ session: SessionPair) async throws -> Void
        /// `POST /discovery/identity` on the new session, with the
        /// attestation when there is one, and the proof when `withProof`.
        public var publishIdentity: @MainActor (
            _ proof: Data, _ attestation: Data?, _ withProof: Bool
        ) async throws -> Void
        public var republishKeyPackage: @MainActor () async throws -> Void
        /// `PUT /ds/group/{hex}/root` for the DeviceGroup journal.
        public var transferDeviceGroupRoot: @MainActor (_ attestation: Data?, _ keyGeneration: UInt64?) async throws -> Void
        public var transferOwnedJournals: @MainActor () async throws -> Void
        public var handOverChannels: @MainActor (_ keyGeneration: UInt64?) async throws -> Void
        public var rekeyReach: @MainActor () async throws -> Void
        /// Section 3.5a rule 3: re-send the profile to every conversation.
        /// Throws while any send has not landed.
        public var resendProfile: @MainActor () async throws -> Void
        /// Section 3.5a rule 5: revoke this account's own links and re-send
        /// its unanswered contact requests from the new key. Throws while
        /// anything is still owed.
        public var spendOldGrants: @MainActor () async throws -> Void = {}
        /// ADR-0067 section 3 — the commit of "Save a recovery phrase":
        /// `rotate/finish` with `phrase_saved: true` and no device revoked.
        public var commitSavingPhrase: (
            _ token: String, _ grant: String, _ material: KeyRotationMaterial, _ proof: Data
        ) async throws -> KeyResetResult = { _, _, _, _ in
            throw AuthClientError.argument("saving a recovery phrase is not wired")
        }
        /// The commit of a "Save a recovery phrase" rotation landed: the
        /// account is phrase-protected from here (the local flag, and the
        /// marker that keeps this device from being told about its own act).
        public var phraseWasSaved: @MainActor (_ accountIdB64: String) -> Void = { _ in }
        /// Whether the account is phrase-protected (ADR-0067): only then are
        /// the new words of a device rotation shown.
        public var phraseProtected: @MainActor (_ accountIdB64: String) -> Bool = { _ in true }
        public var resealBackup: @MainActor () async throws -> Void
        /// The new words for the screen, read from this device's own store.
        public var devicePhrase: @MainActor (_ accountIdB64: String) -> String?
        /// A successful rotation spends the proof.
        public var dropFreshAuth: @MainActor () -> Void
        public var nowMs: () -> Int64
    }

    private let ports: Ports
    private let store: KeyRotationStore
    private var running = false

    /// The active line of the step list, `nil` when no rotation runs.
    @Published public private(set) var stage: KeyRotationPlan.Stage?
    /// What a stored record asks of the Devices screen.
    @Published public private(set) var resume: KeyRotationPlan.Resume = .nothing

    public init(ports: Ports, store: KeyRotationStore = KeyRotationStore()) {
        self.ports = ports
        self.store = store
    }

    /// Re-read the stored record (a landing, the Devices screen opening).
    public func reload() {
        guard let session = ports.session() else {
            resume = .nothing
            return
        }
        // A rotation that only waits for its words to be confirmed, on an
        // account that is shown none: nothing is left of it.
        if var record = store.load(accountIdB64: session.accountIdB64),
           record.phase == .committed, KeyRotationPlan.remaining(record).isEmpty,
           !showsWords(record, accountIdB64: session.accountIdB64) {
            record.phraseAcknowledged = true
            store.put(record, accountIdB64: session.accountIdB64)
        }
        resume = KeyRotationPlan.resume(
            store.load(accountIdB64: session.accountIdB64),
            heldAccountKey: try? Identity.fromSeed(seed: session.accountIdentitySeed).signaturePublicKey())
    }

    private func showsWords(_ record: KeyRotationRecord, accountIdB64: String) -> Bool {
        KeyRotationPlan.showsNewWords(
            savesPhrase: record.savesPhrase, phraseProtected: ports.phraseProtected(accountIdB64))
    }

    /// The device id (base64) a stored record names, for `Try again`.
    public func storedTargetDeviceId() -> Data? {
        guard let session = ports.session(),
              let record = store.load(accountIdB64: session.accountIdB64) else { return nil }
        return Data(base64Encoded: record.targetDeviceIdB64)
    }

    /// `Not now` on "removed, keys not changed": forget the attempt. The
    /// device stays removed; nothing else was changed.
    public func abandonUnstarted() {
        guard let session = ports.session(),
              let record = store.load(accountIdB64: session.accountIdB64),
              record.phase != .committed else { return }
        store.clear(accountIdB64: session.accountIdB64)
        reload()
    }

    /// The person confirmed the new words.
    public func acknowledgePhrase() {
        guard let session = ports.session(),
              var record = store.load(accountIdB64: session.accountIdB64) else { return }
        record.phraseAcknowledged = true
        store.put(record, accountIdB64: session.accountIdB64)
        reload()
    }

    /// The new words, once nothing withholds them.
    public func wordsToShow() -> String? {
        guard let session = ports.session(),
              let record = store.load(accountIdB64: session.accountIdB64),
              record.phase == .committed, !KeyRotationPlan.publishOwed(record),
              showsWords(record, accountIdB64: session.accountIdB64) else { return nil }
        return ports.devicePhrase(session.accountIdB64)
    }

    /// Remove `deviceId` and change the account keys. The caller obtained
    /// the fresh proof (and the ADR-0015 attestation, when the account has a
    /// passkey) first.
    public func rotate(deviceId: Data, attestation: Data?) async -> KeyRotationPlan.Result {
        guard !running, let session = ports.session() else { return .removedNotRotated(nil) }
        running = true
        defer {
            running = false
            stage = nil
            reload()
        }
        let accountIdB64 = session.accountIdB64
        let deviceIdB64 = deviceId.base64EncodedString()

        // A record of an earlier attempt at the SAME device is continued; one
        // that already committed only owes its steps.
        var record: KeyRotationRecord
        if let stored = store.load(accountIdB64: accountIdB64), stored.targetDeviceIdB64 == deviceIdB64 {
            record = stored
        } else {
            // An earlier rotation (of another device) that committed still
            // owes its steps: they are finished before its record is
            // replaced, and a second rotation never starts over an owed
            // publish.
            if let earlier = store.load(accountIdB64: accountIdB64), earlier.phase == .committed,
               !KeyRotationPlan.remaining(earlier).isEmpty {
                _ = await finishOwed(earlier, session: session)
                if let after = store.load(accountIdB64: accountIdB64), KeyRotationPlan.publishOwed(after) {
                    return .rotatedPublishOwed
                }
            }
            // 1. The owed record, before anything is sent.
            record = KeyRotationRecord(targetDeviceIdB64: deviceIdB64, startedAtMs: ports.nowMs())
            store.put(record, accountIdB64: accountIdB64)
        }
        if record.phase == .committed {
            return await finishOwed(record, session: session)
        }
        if record.phase == .announced {
            // An earlier commit got no answer. If it landed, this device has
            // adopted it by now (``KeyRotationPlan/Resume/commitLandedUnseen``)
            // and `resumeOwed` takes over; otherwise start over from the
            // removal's check with a new phrase.
            record.phase = .removed
            record.newSignaturePubkey = nil
            record.supersessionProof = nil
            store.put(record, accountIdB64: accountIdB64)
        }
        guard let grant = ports.freshGrant() else { return .freshAuthRequired }

        // 2 to 4. Remove, wait, generate, announce: the one seam.
        stage = .removingDevice
        var material: KeyRotationMaterial?
        var proof: Data?
        do {
            let announced = try await KeyRotationAnnounce.removeThenAnnounce(
                target: deviceId,
                recordedFloor: record.announceEpochFloor,
                onFloor: { floor in
                    record.announceEpochFloor = floor
                    self.store.put(record, accountIdB64: accountIdB64)
                },
                ports: KeyRotationAnnounce.Ports(
                    epoch: { try self.ports.deviceGroupEpoch() },
                    roster: { try self.ports.deviceGroupRoster() },
                    remove: {
                        // An earlier run already removed the device: it is out
                        // of the DeviceGroup and nothing is sent to auth again.
                        let alreadyOut = record.phase != .removing
                            && (try? self.ports.deviceGroupRoster())?.contains(deviceId) == false
                        if !alreadyOut {
                            let result = await self.ports.removeDevice(deviceId, attestation)
                            guard result == .removed || result == .removedPending else {
                                throw KeyRotationRemovalFailed(result: result)
                            }
                        }
                        record.phase = .removed
                        self.store.put(record, accountIdB64: accountIdB64)
                        self.stage = .changingKeys
                    },
                    send: { envelope in try await self.ports.sendToDeviceGroup(envelope) }),
                envelope: {
                    // 3. The new phrase and the proof by the OLD key, built
                    //    only now that the lost device is out.
                    let fresh = try self.ports.generate()
                    let nowSecs = self.ports.nowMs() / 1000
                    let signed = try self.ports.signProof(
                        session.accountIdentitySeed, fresh.seed, nowSecs,
                        nowSecs + KeyRotationWire.proofWindowSecs)
                    let preparedAtMs = UInt64(max(0, self.ports.nowMs()))
                    // This device's own copy, BEFORE the envelope leaves: a
                    // commit whose answer is lost is then adopted here too.
                    try self.ports.storeOwnPending(
                        PendingKeyRotation(
                            newSignaturePubkey: fresh.newSignaturePubkey,
                            newEntropy: fresh.entropy,
                            supersessionProof: signed,
                            preparedAtMs: preparedAtMs,
                            proofExpiresAtUnixSecs: nowSecs + KeyRotationWire.proofWindowSecs),
                        accountIdB64)
                    material = fresh
                    proof = signed
                    return try self.ports.composeEnvelope(preparedAtMs, fresh.entropy, signed)
                })
            NSLog("[devices] key rotation announced in the DeviceGroup at epoch %llu (floor %llu)",
                  announced.sealedEpoch, announced.floor)
        } catch let failed as KeyRotationRemovalFailed {
            // Nothing was removed: the record goes, the removal's own state
            // is what the sheet shows.
            store.clear(accountIdB64: accountIdB64)
            return .notRemoved(failed.result)
        } catch {
            NSLog("[devices] WARN the key rotation stopped before the commit: %@",
                  String(describing: type(of: error)))
            if record.phase == .removing {
                // The removal never ran (the DeviceGroup could not be read):
                // nothing was removed, so nothing is owed.
                store.clear(accountIdB64: accountIdB64)
                return .notRemoved(.notRevoked(.failed))
            }
            return .removedNotRotated(nil)
        }
        guard let material, let proof else { return .removedNotRotated(nil) }
        record.phase = .announced
        record.newSignaturePubkey = material.newSignaturePubkey
        record.supersessionProof = proof
        store.put(record, accountIdB64: accountIdB64)

        // 5. The commit.
        guard let token = ports.sessionToken() else { return .removedNotRotated(.failed) }
        let committed: KeyResetResult
        do {
            committed = try await ports.commit(token, grant, material, proof, [deviceId])
        } catch {
            let outcome = KeyRotationPlan.commitOutcome(of: error)
            NSLog("[devices] WARN the key rotation was not committed (%@); the device was removed",
                  String(describing: outcome))
            if outcome == .offline {
                // Unknown: the record stays `announced`, the next landing
                // finds out which it was.
                return .removedNotRotated(outcome)
            }
            record.phase = .removed
            record.newSignaturePubkey = nil
            record.supersessionProof = nil
            store.put(record, accountIdB64: accountIdB64)
            return outcome == .freshAuthRequired ? .freshAuthRequired : .removedNotRotated(outcome)
        }
        ports.dropFreshAuth()

        // 6. Adopt locally, before any further network call.
        record.phase = .committed
        record.attestation = committed.attestation
        record.keyGeneration = committed.session.keyGeneration
        store.put(record, accountIdB64: accountIdB64)
        stage = .updatingDevices
        do {
            try await ports.adopt(material, committed.session)
        } catch {
            NSLog("[devices] ERR the rotated keys could not be stored on this device: %@",
                  String(describing: type(of: error)))
            return .rotatedPublishOwed
        }

        // 7 and 8.
        guard let adopted = ports.session() else { return .rotatedPublishOwed }
        return await finishOwed(record, session: adopted)
    }

    /// "Save a recovery phrase" (ADR-0067 section 3, UX 19.5): the same
    /// rotation with no device removed, committed with `phrase_saved: true`.
    /// The account gets new keys and new words; the other devices are kept
    /// and adopt them; the email vault of the old phrase is deleted by the
    /// commit. The caller obtained the fresh proof first.
    ///
    /// The result reads as the device rotation's: `.rotated` shows the NEW
    /// words (only once the server step and the withholding publish steps
    /// landed), `.rotatedPublishOwed` withholds them, `.freshAuthRequired`
    /// asks for the proof again, and every `.removedNotRotated` /
    /// `.notRemoved` means nothing changed.
    public func savePhrase() async -> KeyRotationPlan.Result {
        guard !running, let session = ports.session() else { return .removedNotRotated(nil) }
        running = true
        defer {
            running = false
            stage = nil
            reload()
        }
        let accountIdB64 = session.accountIdB64
        if let stored = store.load(accountIdB64: accountIdB64), stored.phase == .committed {
            if stored.savesPhrase {
                // The commit already landed: only its steps and words remain.
                return await finishOwed(stored, session: session)
            }
            // A device rotation that committed still owes its steps: they
            // finish before a second rotation starts over an owed publish.
            if !KeyRotationPlan.remaining(stored).isEmpty {
                _ = await finishOwed(stored, session: session)
                if let after = store.load(accountIdB64: accountIdB64), KeyRotationPlan.publishOwed(after) {
                    return .rotatedPublishOwed
                }
            }
        }
        guard let grant = ports.freshGrant() else { return .freshAuthRequired }
        // Re-read: finishing an earlier rotation's steps does not move the
        // seed, but the session is read again for what this run signs with.
        guard let current = ports.session() else { return .removedNotRotated(nil) }

        // 1. The owed record, before anything is sent. No device to remove.
        var record = KeyRotationRecord(targetDeviceIdB64: "", startedAtMs: ports.nowMs())
        record.phase = .removed
        record.phraseSaved = true
        store.put(record, accountIdB64: accountIdB64)

        // 3 and 4. The new phrase, the proof by the OLD key, the envelope
        // for the other devices.
        stage = .changingKeys
        let material: KeyRotationMaterial
        let proof: Data
        do {
            let fresh = try ports.generate()
            let nowSecs = ports.nowMs() / 1000
            let signed = try ports.signProof(
                current.accountIdentitySeed, fresh.seed, nowSecs, nowSecs + KeyRotationWire.proofWindowSecs)
            let preparedAtMs = UInt64(max(0, ports.nowMs()))
            try ports.storeOwnPending(
                PendingKeyRotation(
                    newSignaturePubkey: fresh.newSignaturePubkey,
                    newEntropy: fresh.entropy,
                    supersessionProof: signed,
                    preparedAtMs: preparedAtMs,
                    proofExpiresAtUnixSecs: nowSecs + KeyRotationWire.proofWindowSecs),
                accountIdB64)
            _ = try await ports.sendToDeviceGroup(
                try ports.composeEnvelope(preparedAtMs, fresh.entropy, signed))
            material = fresh
            proof = signed
        } catch {
            NSLog("[protect] WARN saving a recovery phrase stopped before the commit: %@",
                  String(describing: type(of: error)))
            // Nothing changed on the server: nothing is owed.
            store.clear(accountIdB64: accountIdB64)
            return .removedNotRotated(nil)
        }
        record.phase = .announced
        record.newSignaturePubkey = material.newSignaturePubkey
        record.supersessionProof = proof
        store.put(record, accountIdB64: accountIdB64)

        // 5. The commit.
        guard let token = ports.sessionToken() else {
            store.clear(accountIdB64: accountIdB64)
            return .removedNotRotated(.failed)
        }
        let committed: KeyResetResult
        do {
            committed = try await ports.commitSavingPhrase(token, grant, material, proof)
        } catch {
            let outcome = KeyRotationPlan.commitOutcome(of: error)
            NSLog("[protect] WARN the recovery phrase was not saved (%@)", String(describing: outcome))
            if outcome == .offline {
                // Unknown: the record stays `announced`, the next landing
                // finds out which it was.
                return .removedNotRotated(outcome)
            }
            store.clear(accountIdB64: accountIdB64)
            return outcome == .freshAuthRequired ? .freshAuthRequired : .removedNotRotated(outcome)
        }
        ports.dropFreshAuth()
        ports.phraseWasSaved(accountIdB64)

        // 6. Adopt locally, before any further network call.
        record.phase = .committed
        record.attestation = committed.attestation
        record.keyGeneration = committed.session.keyGeneration
        store.put(record, accountIdB64: accountIdB64)
        stage = .updatingDevices
        do {
            try await ports.adopt(material, committed.session)
        } catch {
            NSLog("[protect] ERR the new keys could not be stored on this device: %@",
                  String(describing: type(of: error)))
            return .rotatedPublishOwed
        }
        guard let adopted = ports.session() else { return .rotatedPublishOwed }
        return await finishOwed(record, session: adopted)
    }

    /// Retry what a rotation still owes. Called at every session landing and
    /// when the Devices screen opens; a second call while one runs is
    /// dropped.
    public func resumeOwed() async {
        guard !running, let session = ports.session(),
              var record = store.load(accountIdB64: session.accountIdB64) else { return }
        let held = try? Identity.fromSeed(seed: session.accountIdentitySeed).signaturePublicKey()
        switch KeyRotationPlan.resume(record, heldAccountKey: held) {
        case .nothing, .removedNotRotated, .showWords:
            reload()
            return
        case .commitLandedUnseen:
            // The answer was lost and this device adopted its own envelope:
            // the commit landed. No attestation is in hand.
            record.phase = .committed
            store.put(record, accountIdB64: session.accountIdB64)
            if record.savesPhrase { ports.phraseWasSaved(session.accountIdB64) }
        case .finishOwed:
            break
        }
        running = true
        defer {
            running = false
            reload()
        }
        _ = await finishOwed(record, session: session)
    }

    /// Steps 7 and 8: each owed step, recorded as it lands. A step that did
    /// not land does not hold the later ones back, except that nothing after
    /// the directory upload runs before it (a KeyPackage row published
    /// against the old directory row would be deleted by the upload).
    private func finishOwed(
        _ record: KeyRotationRecord, session: Session
    ) async -> KeyRotationPlan.Result {
        var record = record
        let accountIdB64 = session.accountIdB64
        for step in KeyRotationPlan.remaining(record) {
            var landed = false
            do {
                switch step {
                case .discovery:
                    landed = try await publishIdentity(record)
                    if !landed { break }
                case .keyPackage:
                    guard record.discoveryDone else { continue }
                    try await ports.republishKeyPackage()
                    landed = true
                case .deviceGroupRoot:
                    try await ports.transferDeviceGroupRoot(record.attestation, record.keyGeneration)
                    landed = true
                case .journals:
                    try await ports.transferOwnedJournals()
                    landed = true
                case .channels:
                    try await ports.handOverChannels(record.keyGeneration)
                    landed = true
                case .reach:
                    try await ports.rekeyReach()
                    landed = true
                case .profile:
                    // The grants the profile carries must already be
                    // registered on the new mailbox.
                    guard record.reachDone else { continue }
                    try await ports.resendProfile()
                    landed = true
                case .grants:
                    guard record.discoveryDone, record.reachDone else { continue }
                    try await ports.spendOldGrants()
                    landed = true
                case .backup:
                    try await ports.resealBackup()
                    landed = true
                }
            } catch {
                NSLog("[devices] WARN key rotation step %@ is still owed: %@",
                      step.rawValue, String(describing: type(of: error)))
            }
            if landed {
                record = KeyRotationPlan.marking(record, done: step)
                store.put(record, accountIdB64: accountIdB64)
            }
        }
        if !showsWords(record, accountIdB64: accountIdB64) {
            // Not phrase-protected: no words, no withheld-words state. The
            // record stays only while steps are owed.
            record.phraseAcknowledged = true
            store.put(record, accountIdB64: accountIdB64)
            return .rotatedNoWords
        }
        if KeyRotationPlan.publishOwed(record) { return .rotatedPublishOwed }
        guard let phrase = ports.devicePhrase(accountIdB64) else { return .rotatedPublishOwed }
        return .rotated(phrase: phrase)
    }

    /// The proven-rotation upload, with its fallback: when the directory no
    /// longer holds the key the proof was signed by, the same upload goes
    /// again WITHOUT the proof (an attested reset to the peers). Without an
    /// attestation (a lost commit answer) only the proof can be sent.
    private func publishIdentity(_ record: KeyRotationRecord) async throws -> Bool {
        guard let proof = record.supersessionProof else { return false }
        do {
            try await ports.publishIdentity(proof, record.attestation, true)
            return true
        } catch {
            switch KeyRotationPlan.directoryOutcome(of: error, sentProof: true) {
            case .done:
                return true
            case .retryWithoutProof:
                guard record.attestation != nil else { throw error }
                NSLog("[devices] WARN the directory refused the proof; publishing the new key by attestation alone")
                do {
                    try await ports.publishIdentity(proof, record.attestation, false)
                    return true
                } catch {
                    if KeyRotationPlan.directoryOutcome(of: error, sentProof: false) == .done { return true }
                    throw error
                }
            case .superseded, .retry:
                throw error
            }
        }
    }
}

extension KeyRotationRunner.Ports {

    /// The production ports.
    @MainActor
    static func live(
        auth: AuthRepository,
        chat: ChatRepository,
        contacts: ContactsRepository,
        sync: DeviceGroupSyncRepository,
        removal: DeviceRemovalRunner
    ) -> KeyRotationRunner.Ports {
        KeyRotationRunner.Ports(
            session: { [weak auth] in
                guard let auth, let secrets = auth.identitySecrets, case .session = auth.state else {
                    return nil
                }
                let accountIdB64 = base64Encode(secrets.accountId)
                return KeyRotationRunner.Session(
                    accountId: secrets.accountId,
                    accountIdB64: accountIdB64,
                    accountIdentitySeed: secrets.accountIdentitySeed,
                    ownDeviceId: auth.deviceId(accountIdB64: accountIdB64))
            },
            sessionToken: { [weak auth] in auth?.identitySecrets?.sessionToken },
            freshGrant: { [weak auth] in auth?.liveFreshGrant() },
            removeDevice: { [weak removal] deviceId, attestation in
                guard let removal else { return .notRevoked(.failed) }
                return await removal.remove(deviceId: deviceId, attestation: attestation)
            },
            deviceGroupEpoch: { [weak sync] in
                guard let sync else { throw SessionNotReady() }
                return try sync.currentEpoch()
            },
            deviceGroupRoster: { [weak sync] in
                guard let roster = sync?.rosterDeviceKeys() else { throw SessionNotReady() }
                return roster
            },
            sendToDeviceGroup: { [weak sync] envelope in
                guard let sync else { throw SessionNotReady() }
                return try await sync.sendKeyRotationEnvelope(envelope)
            },
            generate: { try KeyRotationMaterial.generate() },
            signProof: { oldSeed, newSeed, issuedAt, expiresAt in
                // The proof is by the OLD account key over the new one.
                try signSupersessionProof(
                    oldEd25519Seed: try Identity.fromSeed(seed: oldSeed).ed25519Seed(),
                    newEd25519Seed: try Identity.fromSeed(seed: newSeed).ed25519Seed(),
                    issuedAtUnixSecs: issuedAt,
                    expiresAtUnixSecs: expiresAt)
            },
            composeEnvelope: { preparedAtMs, entropy, proof in
                try composeKeyRotationEnvelope(
                    preparedAtMs: preparedAtMs, newEntropy: entropy, supersessionProof: proof)
            },
            storeOwnPending: { [weak auth] pending, accountIdB64 in
                guard let auth else { throw SessionNotReady() }
                try auth.storeOwnPendingKeyRotation(pending, accountIdB64: accountIdB64)
            },
            commit: { token, grant, material, proof, revokeDeviceIds in
                try await AuthClient().rotateAccountKeys(
                    sessionToken: token,
                    freshGrant: grant,
                    seed: material.seed,
                    newSignaturePubkey: material.newSignaturePubkey,
                    supersessionProof: proof,
                    revokeDeviceIds: revokeDeviceIds)
            },
            adopt: { [weak auth] material, session in
                guard let auth else { throw SessionNotReady() }
                try await auth.adoptOwnKeyRotation(material: material, session: session)
            },
            publishIdentity: { [weak auth] proof, attestation, withProof in
                guard let auth else { throw SessionNotReady() }
                try await auth.publishRotatedIdentity(
                    supersessionProof: proof, attestation: attestation, withProof: withProof)
            },
            republishKeyPackage: { [weak chat] in
                guard let chat else { throw SessionNotReady() }
                try await chat.republishKeyPackageAfterKeyRotation()
            },
            transferDeviceGroupRoot: { [weak auth] attestation, keyGeneration in
                guard let auth, let secrets = auth.identitySecrets,
                      let predecessor = auth.ownPredecessorSeeds(
                        accountIdB64: base64Encode(secrets.accountId)).last else {
                    throw SessionNotReady()
                }
                try await DeviceGroupDsAccess.transferRootAfterKeyRotation(
                    accountIdBytes: secrets.accountId,
                    predecessorSeed: predecessor,
                    newAccountSeed: secrets.accountIdentitySeed,
                    attestation: attestation,
                    keyGeneration: keyGeneration,
                    ds: DsClient())
            },
            transferOwnedJournals: { [weak auth, weak chat] in
                guard let auth, let chat, let secrets = auth.identitySecrets,
                      let predecessor = auth.ownPredecessorSeeds(
                        accountIdB64: base64Encode(secrets.accountId)).last else {
                    throw SessionNotReady()
                }
                try chat.transferOwnedJournalRoots(predecessorSeed: predecessor)
            },
            handOverChannels: { [weak auth] keyGeneration in
                guard let auth else { throw SessionNotReady() }
                try await KeyRotationChannels.handOverAll(auth: auth, keyGeneration: keyGeneration)
            },
            rekeyReach: {
                // The keys moved in `AuthRepository.onAccountKeysRotated`;
                // this registers them on the new mailbox.
                guard await ReachCenter.shared.syncNow() else { throw SessionNotReady() }
            },
            resendProfile: { [weak chat] in
                guard let chat else { throw SessionNotReady() }
                // The marker is written when the keys move; a step that runs
                // without one (the chat layer was not up at that moment)
                // writes it now, so the step can never pass without sending.
                chat.markProfileResendOwed()
                try await chat.resendProfileAfterKeyRotation()
            },
            spendOldGrants: { [weak auth, weak contacts] in
                guard let auth, let contacts, let secrets = auth.identitySecrets else {
                    throw SessionNotReady()
                }
                try await contacts.spendGrantsAfterKeyRotation(
                    predecessorSeeds: auth.ownPredecessorSeeds(
                        accountIdB64: base64Encode(secrets.accountId)))
            },
            commitSavingPhrase: { token, grant, material, proof in
                try await AuthClient().rotateAccountKeys(
                    sessionToken: token,
                    freshGrant: grant,
                    seed: material.seed,
                    newSignaturePubkey: material.newSignaturePubkey,
                    supersessionProof: proof,
                    revokeDeviceIds: [],
                    phraseSaved: true)
            },
            phraseWasSaved: { [weak auth] accountIdB64 in
                auth?.protectionStore.setPhraseSaved(true, accountIdB64: accountIdB64)
                auth?.protectionStore.markOwnAction(
                    .phraseSaved, atMs: Int64(Date().timeIntervalSince1970 * 1000), accountIdB64: accountIdB64)
                auth?.onProtectionChanged?()
            },
            phraseProtected: { [weak auth] accountIdB64 in
                auth?.protectionStore.phraseSaved(accountIdB64: accountIdB64) ?? false
            },
            resealBackup: { [weak auth, weak chat] in
                guard let auth else { throw SessionNotReady() }
                try await KeyRotationBackup.reseal(
                    chatBootstrapped: chat?.state.bootstrapped ?? false,
                    snapshot: { chat?.currentStorageSnapshot() },
                    knownGroupIdsHex: { chat?.knownGroupIdHexes ?? [] },
                    upload: { snapshot, groupIds in
                        try await auth.resealBackupAfterKeyRotation(
                            wholeStorageSnapshot: snapshot,
                            knownGroupIdsHex: groupIds,
                            syncedState: auth.identitySecrets.flatMap {
                                SyncedStateStore.bytes(accountIdB64: base64Encode($0.accountId))
                            })
                    })
            },
            devicePhrase: { accountIdB64 in DevicePhrase.read(accountIdB64: accountIdB64) },
            dropFreshAuth: { [weak auth] in auth?.dropFreshAuth() },
            nowMs: { Int64(Date().timeIntervalSince1970 * 1000) }
        )
    }
}

/// The backup step of a rotation (ADR-0066 section 3.5, "Backup"): the
/// decision of whether there is anything that may be uploaded, apart from the
/// upload itself so a test can run it.
enum KeyRotationBackup {

    /// Why the step did not run. Thrown, so the runner keeps the step owed.
    enum NotReady: Error, Equatable {
        /// The chat layer has not bootstrapped: its group list is not known.
        case chatNotBootstrapped
        /// No MLS storage snapshot could be taken, or it is empty.
        case noSnapshot
    }

    /// Re-seal and upload, or throw. Returns only when `upload` returned,
    /// which it does only for an accepted upload. A chat layer that is not
    /// ready throws before `upload` is called: an envelope with an empty
    /// snapshot would overwrite the one the server holds.
    @MainActor
    static func reseal(
        chatBootstrapped: Bool,
        snapshot: @MainActor () -> Data?,
        knownGroupIdsHex: @MainActor () -> [String],
        upload: @MainActor (_ snapshot: Data, _ knownGroupIdsHex: [String]) async throws -> Void
    ) async throws {
        guard chatBootstrapped else { throw NotReady.chatNotBootstrapped }
        guard var taken = snapshot(), !taken.isEmpty else { throw NotReady.noSnapshot }
        defer { taken.resetBytes(in: 0..<taken.count) }
        try await upload(taken, knownGroupIdsHex())
    }
}

/// The channel owner-key handover of a rotation, over the live clients.
enum KeyRotationChannels {

    /// Hand every channel this device created to the key the account's NEW
    /// seed derives (``ChannelOwnerHandover``), recording the new key
    /// generation for each one that is owned under it. Throws when a channel
    /// got no verdict, so the step stays owed.
    @MainActor
    static func handOverAll(auth: AuthRepository, keyGeneration: UInt64?) async throws {
        guard let secrets = auth.identitySecrets else { throw SessionNotReady() }
        let accountIdB64 = base64Encode(secrets.accountId)
        let channelIds = OwnedChannelGenerations.load(accountIdB64: accountIdB64).keys.sorted()
        guard !channelIds.isEmpty else { return }
        guard let predecessor = auth.ownPredecessorSeeds(accountIdB64: accountIdB64).last else {
            throw SessionNotReady()
        }
        let old = try Identity.fromSeed(seed: predecessor)
        let new = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        let client = ChannelsClient()
        var outcomes: [ChannelOwnerHandover.Outcome] = []
        var owned: [String] = []
        for channelId in channelIds {
            let outcome = await ChannelOwnerHandover.handOver(old: old, new: new) { signing, naming in
                try await client.replaceOwner(
                    channelIdHex: channelId, signingIdentity: signing, newOwnerIdentity: naming)
            }
            outcomes.append(outcome)
            if outcome == .owned { owned.append(channelId) }
        }
        // A role learned under the old key is not this key's.
        ChannelPublisherRoleCache.shared.clearAll()
        let generation = keyGeneration ?? secrets.session.keyGeneration
        OwnedChannelGenerations.record(
            channelIds: owned, generation: generation, accountIdB64: accountIdB64)
        NSLog("ADR-0066: channel owner handover: %d owned, %d not owner, %d owed",
              owned.count, outcomes.filter { $0 == .notOwner }.count,
              outcomes.filter { $0 == .retry }.count)
        NotificationCenter.default.post(name: .lakkoAccountKeysRotated, object: nil)
        guard ChannelOwnerHandover.settled(outcomes) else {
            throw AuthClientError.argument("a channel owner handover got no verdict")
        }
    }
}
