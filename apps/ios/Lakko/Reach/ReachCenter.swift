// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// Whether the owner's reach keys on the Delivery Service match what this
/// device wants registered (UX spec section 8).
public enum ReachApplyStatus: Equatable {
    /// Nothing outstanding.
    case applied
    /// A registration is in flight.
    case applying
    /// The last attempt could not reach the server; retried on the next sync.
    case pendingOffline
    /// The server refused (or the session is gone); the rows show what was
    /// last in effect.
    case failed
}

/// ADR-0060 — the app-scoped composition root for reach keys and the
/// "who can find me" setting, in the posture of ``ProfileDirectory``: one
/// instance per process (``shared``), bound to the signed-in account.
///
/// It owns three things:
///
/// - **The owner's keys.** Every reach key is derived from the account's
///   Ed25519 seed (`reachSeed`), so this device computes the same keys as the
///   account's other devices from the version counters alone. ``sync()``
///   registers what ``ReachRules/desiredKeys`` asks for on the account mailbox
///   (`PUT /ds/mailbox/{hex}/reach`, signed by the identity key) and removes
///   the rest.
/// - **The grants to hand out**: `contact` + `member` in a one-to-one profile,
///   `group` (only where the setting allows it) + `member` in a group profile,
///   a fresh `reply` in every outgoing request, a `link` in every `/i/` and
///   `/c/` link.
/// - **The grants received** from other people, filed under the account key
///   resolved at receipt, used to sign writes to their mailbox.
///
/// **Carried between the account's devices** (ADR-0060 section 7): a local
/// change of the setting or of a reach-key version hands its envelope to
/// ``onDeviceEnvelope`` (the app shell sends it as a DeviceGroup
/// `ServiceEnvelope`); a received one comes back through
/// ``adoptDeviceEnvelope(_:)``: a setting replaces what is kept only when
/// strictly newer, reach-key counters are merged by the core
/// (`mergeReachState`). Envelopes are kept and re-sent as they are, so a
/// re-send carries its original stamp.
@MainActor
public final class ReachCenter: ObservableObject {
    public static let shared = ReachCenter()

    /// The setting in effect on this device.
    @Published public private(set) var setting: FfiVisibilitySetting = defaultVisibilitySetting()
    @Published public private(set) var status: ReachApplyStatus = .applied
    /// Bumped whenever the grants this device hands out may have changed (a
    /// rotation, a setting change): the profile republisher listens.
    @Published public private(set) var grantRevision: UInt64 = 0

    private let persistence: ReachPersistence
    private var accountIdB64: String?
    private var accountSeed: Data?
    /// This account's 32-byte identity (account) public key.
    public private(set) var ownAccountKey: Data?
    private var mailboxSigner: DsProofSigner?
    private var mailboxPseudonym: Data?
    private var record = ReachRecord()
    private var reachState = FfiReachState(entries: [], updatedAtMs: 0)
    private let ds: ReachDsTransport
    /// The ids of every group this account is in (set by the chat layer).
    public var groupIdsProvider: () -> [Data] = { [] }
    /// Every channel this account subscribes to, with its current seed epoch
    /// (set by the app shell): the `channel` reach scopes.
    public var channelEpochsProvider: () -> [(id: Data, epoch: UInt32)] = { [] }
    /// Every group and channel this account is in, as setting scopes (set by
    /// the app shell): what "All groups and channels" means when one of them is
    /// switched off from its info (UX section 5).
    public var allScopesProvider: () -> [FfiScopeId] = { [] }
    /// The session token discovery writes need (set by the app shell).
    public var sessionTokenProvider: () -> String? = { nil }
    /// This account's display name and canonical username (set by the app
    /// shell from ``ProfileDirectory``): what Everyone publishes and an
    /// introduction record is findable by.
    public var ownNameProvider: () -> (displayName: String?, username: String?, accountId: Data?) = {
        (nil, nil, nil)
    }
    /// Called after a setting change or a rotation so profiles are republished
    /// with the new grants (set by the chat layer).
    public var onGrantsChanged: (() -> Void)?
    /// ADR-0050 2.5a item 7 — called with the account key (base64) of a person
    /// a grant or a committer delegation was just kept for, so a Welcome owed
    /// to their mailbox for lack of a key is retried at once (set by the chat
    /// layer).
    public var onGrantKept: ((String) -> Void)?
    /// ADR-0060 section 7 — called with the Visibility or ReachState envelope
    /// a LOCAL change produced, so the account's other devices get it over
    /// the DeviceGroup (set by the app shell). Never called for an envelope
    /// adopted from another device: that one is already on the journal.
    public var onDeviceEnvelope: ((Data) -> Void)?
    /// ADR-0048 section 4a — where ``adoptDeviceEnvelope(_:)`` hands the own
    /// profile envelope another of this account's devices sent. The default is
    /// the process-wide ``ProfileDirectory``; tests inject their own.
    var ownProfileAdopter: @MainActor (Data) -> ProfileDirectory.OwnProfileAdoption = { envelope in
        ProfileDirectory.shared.adoptOwnProfileFromDevice(envelope: envelope)
    }
    private var syncTask: Task<Void, Never>?
    private var syncAgain = false

    init(persistence: ReachPersistence = KeychainReachPersistence(), ds: ReachDsTransport = DsClient()) {
        self.persistence = persistence
        self.ds = ds
    }

    // MARK: - Account lifecycle

    /// Bind to `accountIdB64`. `accountIdentity` is the account's phrase-seeded
    /// identity: its Ed25519 seed derives every reach key and its mailbox
    /// signer registers them. Idempotent for the bound account.
    public func activate(accountIdB64: String, accountIdentity: Identity) {
        if self.accountIdB64 == accountIdB64, accountSeed != nil { return }
        self.accountIdB64 = accountIdB64
        accountSeed = accountIdentity.ed25519Seed()
        ownAccountKey = accountIdentity.signaturePublicKey()
        let signer = dsMailboxSigner(identity: accountIdentity)
        mailboxSigner = signer
        mailboxPseudonym = signer.mailboxPseudonym()
        record = persistence.load(accountIdB64: accountIdB64) ?? ReachRecord()
        if let b64 = record.settingEnvelopeB64, let bytes = Data(base64Encoded: b64),
           let decoded = try? decodeVisibilityEnvelope(plaintext: bytes) {
            setting = decoded
        } else {
            // A new account (or a device that has never stored one): the
            // default of UX section 1 at stamp 0, the core's own default and
            // what web and Android keep. It must NOT carry this device's
            // clock: a device newly admitted to the account would then hold a
            // default stamped later than the setting the person actually chose
            // on their first device, and refuse it when the DeviceGroup
            // delivers it (ADR-0060 section 7). Any change the person makes is
            // stamped by ``updateSetting(_:)`` and beats it.
            let fresh = defaultVisibilitySetting()
            setting = fresh
            record.settingEnvelopeB64 = (try? composeVisibilityEnvelope(setting: fresh))?
                .base64EncodedString()
        }
        if let b64 = record.reachStateEnvelopeB64, let bytes = Data(base64Encoded: b64),
           let decoded = try? decodeReachStateEnvelope(plaintext: bytes) {
            reachState = decoded
        } else {
            reachState = FfiReachState(entries: [], updatedAtMs: 0)
        }
        persist()
        grantRevision &+= 1
    }

    /// No signed-in account (logout). The record stays on disk.
    public func deactivate() {
        syncTask?.cancel()
        syncTask = nil
        accountIdB64 = nil
        accountSeed = nil
        ownAccountKey = nil
        mailboxSigner = nil
        mailboxPseudonym = nil
        record = ReachRecord()
        reachState = FfiReachState(entries: [], updatedAtMs: 0)
        setting = defaultVisibilitySetting()
        status = .applied
    }

    /// Forget-device: delete everything kept for `accountIdB64`.
    public func forget(accountIdB64: String) {
        persistence.delete(accountIdB64: accountIdB64)
        if self.accountIdB64 == accountIdB64 { deactivate() }
    }

    public var isActive: Bool { accountSeed != nil }

    // MARK: - Registration

    /// Register every desired key and remove the rest, coalescing overlapping
    /// calls into one follow-up run. Best effort; ``status`` reports the
    /// outcome.
    public func sync() {
        guard isActive else { return }
        if syncTask != nil {
            syncAgain = true
            return
        }
        syncTask = Task { @MainActor [weak self] in
            guard let self else { return }
            repeat {
                self.syncAgain = false
                _ = await self.syncOnce()
            } while self.syncAgain && !Task.isCancelled
            self.syncTask = nil
        }
    }

    /// One registration pass, awaited. Returns whether it applied.
    @discardableResult
    public func syncNow() async -> Bool {
        await syncOnce()
    }

    private func syncOnce() async -> Bool {
        guard let seed = accountSeed, let signer = mailboxSigner, let pseudonym = mailboxPseudonym,
              let accountIdB64 else { return false }
        status = .applying
        let now = Self.nowMs()
        func desiredSpecs() -> [ReachKeySpec] {
            ReachRules.desiredKeys(
                setting: setting, state: reachState, groupIds: groupIdsProvider(),
                links: record.links, replies: record.replies,
                channels: channelEpochsProvider(), nowMs: now,
                allowsScope: { allowsScope(setting: $0, scope: $1) },
                allowsFof: { allowsContactsOfContacts(setting: $0) })
        }
        // ADR-0060 section 1, "Versions after a restore" — a counter slot this
        // device holds no version for (a fresh install or a restore, where the
        // book is empty; a group it was re-admitted to) is first looked up
        // among the keys the DS holds on the mailbox, and continues from the
        // highest one registered. Until that read succeeds NOTHING is
        // registered: a guessed version could be one the owner rotated away.
        let unchecked = ReachRules.uncheckedSlots(
            specs: desiredSpecs(), state: reachState,
            checked: Set(record.versionCheckedSlots ?? []))
        if !unchecked.isEmpty {
            guard await recoverVersions(
                slots: unchecked, seed: seed, signer: signer, pseudonym: pseudonym,
                accountIdB64: accountIdB64) else { return false }
        }
        let specs = desiredSpecs()
        var desired: [(publicKey: Data, expiresAtMs: UInt64?)] = []
        for spec in specs {
            guard let pk = try? Self.publicKey(accountSeed: seed, spec: spec) else { continue }
            desired.append((pk, spec.expiresAtMs))
        }
        let registered = Set(record.registeredB64.compactMap { Data(base64Encoded: $0) })
        let diff = ReachRules.registrationDiff(desired: desired, registered: registered)
        let open = ReachRules.mailboxOpen(setting)
        do {
            try await ds.updateReach(
                pseudonym: pseudonym, add: diff.add, remove: diff.remove, open: open, signer: signer)
        } catch {
            guard self.accountIdB64 == accountIdB64 else { return false }
            NSLog("ADR-0060: reach registration failed: \(error)")
            status = Self.isOffline(error) ? .pendingOffline : .failed
            return false
        }
        guard self.accountIdB64 == accountIdB64 else { return false }
        record.registeredB64 = diff.add.map { $0.publicKey.base64EncodedString() }
        record.registeredOpen = open
        record.replies.removeAll { $0.expiresAtMs <= now }
        record.links.removeAll { $0.expiresAtMs <= now }
        persist()
        status = .applied
        await applyEveryone()
        return true
    }

    /// Recover the versions of `slots` from the keys the DS holds on this
    /// account's mailbox (`GET /ds/mailbox/{hex}/reach`, signed by the identity
    /// key), then mark them checked. Returns `false` when the read failed or
    /// the account changed meanwhile: the caller registers nothing then, the
    /// keys already on the DS (registered by the account's earlier devices)
    /// stay as they are, and the next sync retries.
    private func recoverVersions(
        slots: [ReachRules.VersionSlot], seed: Data, signer: DsProofSigner, pseudonym: Data,
        accountIdB64: String
    ) async -> Bool {
        let listing: DsReachListing
        do {
            listing = try await ds.fetchReach(pseudonym: pseudonym, signer: signer)
        } catch {
            guard self.accountIdB64 == accountIdB64 else { return false }
            NSLog("ADR-0060: registered reach keys not read; nothing registered until they are: \(error)")
            status = Self.isOffline(error) ? .pendingOffline : .failed
            return false
        }
        let registered = Set(listing.keys.map(\.publicKey))
        // Up to 257 derivations per slot: off the main actor.
        let found = await Task.detached(priority: .utility) {
            ReachRules.recoveredVersions(slots: slots, registered: registered) { slot, version in
                try? Self.publicKey(accountSeed: seed, spec: ReachKeySpec(
                    reachClass: slot.reachClass, scopeId: slot.scopeId, version: version,
                    expiresAtMs: nil))
            }
        }.value
        guard self.accountIdB64 == accountIdB64 else { return false }
        let next = ReachRules.applying(recovered: found, to: reachState, nowMs: Self.nowMs())
        let changed = next != reachState
        if changed {
            reachState = next
            record.reachStateEnvelopeB64 = (try? composeReachStateEnvelope(state: next))?
                .base64EncodedString()
        }
        var checked = Set(record.versionCheckedSlots ?? [])
        for slot in slots { checked.insert(slot.storageKey) }
        record.versionCheckedSlots = checked.sorted()
        persist()
        NSLog("ADR-0060: reach versions checked for %d slot(s), %d resumed above 0",
              slots.count, found.values.filter { $0 > 0 }.count)
        if changed {
            // The grants handed out so far named the starting version: hand
            // out the recovered ones.
            grantRevision &+= 1
            onGrantsChanged?()
        }
        return true
    }

    nonisolated static func publicKey(accountSeed: Data, spec: ReachKeySpec) throws -> Data {
        let seed = try reachSeed(
            accountSeed: accountSeed, class: spec.reachClass, scopeId: spec.scopeId,
            version: spec.version)
        return try reachVerifyingKey(seed: seed)
    }

    private static func isOffline(_ error: Error) -> Bool {
        if case .network = error as? DsClientError { return true }
        return false
    }

    // MARK: - The setting

    /// Apply a setting a Settings transition produced: stamp it, keep it, and
    /// act on it (register / remove keys, open or close the mailbox, republish
    /// the profiles with the grants it now allows).
    public func updateSetting(_ next: FfiVisibilitySetting) {
        guard isActive else { return }
        var stamped = next
        stamped.updatedAtMs = max(Self.nowMs(), setting.updatedAtMs &+ 1)
        guard let envelope = try? composeVisibilityEnvelope(setting: stamped) else {
            NSLog("ADR-0060: a setting the core refuses was not applied")
            return
        }
        setting = stamped
        record.settingEnvelopeB64 = envelope.base64EncodedString()
        persist()
        grantRevision &+= 1
        sync()
        onGrantsChanged?()
        onDeviceEnvelope?(envelope)
    }

    /// Accept a setting another of this account's devices sent, when strictly
    /// newer. Kept verbatim (its own stamp), then acted on exactly as a local
    /// change is: keys registered or removed, Everyone published or
    /// unpublished (``sync()``), profiles republished and introductions sent
    /// (``onGrantsChanged``). Not handed back to ``onDeviceEnvelope``.
    @discardableResult
    public func adoptSetting(envelope: Data) -> Bool {
        guard isActive, let incoming = try? decodeVisibilityEnvelope(plaintext: envelope),
              visibilityIsNewer(
                  incomingUpdatedAtMs: incoming.updatedAtMs,
                  storedUpdatedAtMs: setting.updatedAtMs) else { return false }
        setting = incoming
        record.settingEnvelopeB64 = envelope.base64EncodedString()
        persist()
        grantRevision &+= 1
        sync()
        onGrantsChanged?()
        return true
    }

    /// Fold reach-key version counters another of this account's devices
    /// sent into the stored ones, by the core's one rule (`mergeReachState`,
    /// ADR-0060 section 7): counters by maximum per `(class, scope)`, never a
    /// whole-state replace, which would lose a rotation two devices made at
    /// the same time. When the core says `store`, the state it returns is kept
    /// exactly as returned (its stamp included) and acted on as a local change
    /// is (keys registered, profiles republished); when it says `send`, that
    /// same state goes to the other devices as returned, never re-stamped
    /// here. A merge the core refuses leaves the stored state as it was.
    @discardableResult
    public func adoptReachState(envelope: Data) -> DeviceEnvelopeOutcome {
        guard isActive else { return .inactive }
        guard let incoming = try? decodeReachStateEnvelope(plaintext: envelope) else { return .foreign }
        let merge: FfiReachStateMerge
        do {
            merge = try mergeReachState(stored: reachState, incoming: incoming, nowMs: Self.nowMs())
        } catch {
            NSLog("ADR-0060: a reach state from another device was not merged: \(error)")
            return .notNewer
        }
        guard merge.store || merge.send else { return .notNewer }
        guard let composed = try? composeReachStateEnvelope(state: merge.state) else {
            NSLog("ADR-0060: a merged reach state could not be composed")
            return .notNewer
        }
        if merge.store {
            reachState = merge.state
            record.reachStateEnvelopeB64 = composed.base64EncodedString()
            persist()
            grantRevision &+= 1
            sync()
            onGrantsChanged?()
        }
        if merge.send {
            onDeviceEnvelope?(composed)
            return .mergedReachState
        }
        return .adoptedReachState
    }

    /// What ``adoptDeviceEnvelope(_:)`` did with a DeviceGroup `ServiceEnvelope`.
    public enum DeviceEnvelopeOutcome: Equatable {
        case adoptedSetting
        /// The own display name another of this account's devices set, strictly
        /// newer than the one kept (ADR-0048 section 4a).
        case adoptedOwnProfile
        /// The incoming reach state was adopted as it came.
        case adoptedReachState
        /// The two devices each held a version the other lacked (or this one
        /// held one the incoming state would overwrite): the merge is kept
        /// and sent back to the other devices.
        case mergedReachState
        /// A Visibility, ReachState or own profile no newer than what is kept.
        case notNewer
        /// Anything else, bytes the dispatcher refuses, or a profile of
        /// another account: dropped.
        case foreign
        /// No account is active.
        case inactive
    }

    /// ADR-0060 section 7 — the receiving end of the DeviceGroup
    /// `ServiceEnvelope`: dispatch the carried bytes and keep only a
    /// Visibility or a ReachState, each only when strictly newer by its own
    /// stamp, and (ADR-0048 section 4a) this account's own profile, whose
    /// display name ``ownProfileAdopter`` keeps when strictly newer. Every
    /// other envelope kind is dropped.
    @discardableResult
    public func adoptDeviceEnvelope(_ envelope: Data) -> DeviceEnvelopeOutcome {
        guard isActive else { return .inactive }
        guard let dispatched = try? dispatchIncomingEnvelope(plaintext: envelope) else {
            return .foreign
        }
        switch dispatched.kind {
        case .visibility:
            return adoptSetting(envelope: envelope) ? .adoptedSetting : .notNewer
        case .reachState:
            return adoptReachState(envelope: envelope)
        case .profile:
            switch ownProfileAdopter(envelope) {
            case .adopted:
                // ADR-0060 section 5 — Everyone follows the name, one adopted
                // from another of the account's devices included (the group
                // profile is not republished: ADR-0048 section 4a).
                sync()
                return .adoptedOwnProfile
            case .notNewer: return .notNewer
            case .inactive: return .inactive
            case .foreignAccount, .notAProfile: return .foreign
            }
        default:
            return .foreign
        }
    }

    /// The Visibility and ReachState envelopes kept for `accountIdB64`,
    /// verbatim, for a device this one has just admitted to the DeviceGroup.
    /// Empty when another account (or none) is active. One still at stamp 0
    /// (the untouched default, nothing rotated) is left out: the new device
    /// holds the same at the same stamp and would refuse it.
    public func deviceEnvelopes(accountIdB64: String) -> [Data] {
        guard isActive, self.accountIdB64 == accountIdB64 else { return [] }
        var out: [Data] = []
        if setting.updatedAtMs > 0, let b64 = record.settingEnvelopeB64,
           let bytes = Data(base64Encoded: b64) {
            out.append(bytes)
        }
        if reachState.updatedAtMs > 0, let b64 = record.reachStateEnvelopeB64,
           let bytes = Data(base64Encoded: b64) {
            out.append(bytes)
        }
        return out
    }

    /// UX section 5 — the `Members can find me` switch of one group or
    /// channel. Under "Only the ones I choose" it is that scope's switch in the
    /// list; under "All" turning it off moves the setting to "chosen" with
    /// every other scope on and this one off. Returns whether the form changed
    /// (the caller says so once, in place).
    @discardableResult
    public func setMembersCanFindMe(_ scope: FfiScopeId, on: Bool) -> Bool {
        guard isActive, setting.groups != nil else { return false }
        if setting.groups == .all {
            guard !on,
                  let next = try? excludeScopeUnderAll(
                      setting: setting, allScopes: allScopesProvider(), excluded: scope)
            else { return false }
            updateSetting(next)
            return true
        }
        guard let next = try? setScopeChosen(setting: setting, scope: scope, on: on) else {
            return false
        }
        updateSetting(next)
        return false
    }

    // MARK: - Rotation

    /// Rotate `(class, scope)` — a new version, the old key unregistered on the
    /// next sync. Called when a contact is removed (`contact`, `fof`) and when a
    /// group shrinks (`group` and `member` of that group).
    public func rotate(_ reachClass: FfiReachClass, scope: Data = Data()) {
        guard isActive else { return }
        reachState = ReachRules.bumping(reachClass, scope: scope, in: reachState, nowMs: Self.nowMs())
        let envelope = try? composeReachStateEnvelope(state: reachState)
        record.reachStateEnvelopeB64 = envelope?.base64EncodedString()
        persist()
        grantRevision &+= 1
        sync()
        onGrantsChanged?()
        if let envelope { onDeviceEnvelope?(envelope) }
    }

    /// A group lost a member: rotate this account's `group` and `member` keys
    /// for it in one registration and one republish.
    public func rotateGroup(_ groupId: Data) {
        guard isActive, !groupId.isEmpty, groupId.count <= 64 else { return }
        let now = Self.nowMs()
        reachState = ReachRules.bumping(.group, scope: groupId, in: reachState, nowMs: now)
        reachState = ReachRules.bumping(.member, scope: groupId, in: reachState, nowMs: now)
        let envelope = try? composeReachStateEnvelope(state: reachState)
        record.reachStateEnvelopeB64 = envelope?.base64EncodedString()
        persist()
        grantRevision &+= 1
        sync()
        onGrantsChanged?()
        if let envelope { onDeviceEnvelope?(envelope) }
    }

    // MARK: - Channel member card (ADR-0060 section 3)

    /// Seal this account's member card: the card is signed by the account
    /// identity seed, which stays inside this type. `nil` while no account is
    /// active (a reader drops an unbound card, so none is uploaded).
    func sealOwnMemberCard(
        seed: Data, epochId: UInt32, channelId: Data, subscriberPk: Data, profile: Data
    ) throws -> Data? {
        guard let accountSeed else { return nil }
        return try sealMemberCard(
            seed: seed, epochId: epochId, channelId: channelId, subscriberPk: subscriberPk,
            accountIdentitySeed: accountSeed, profile: profile)
    }

    // MARK: - Grants handed out

    private func grant(_ reachClass: FfiReachClass, scope: Data) -> FfiReachGrant? {
        guard let seed = accountSeed else { return nil }
        return try? deriveReachGrant(
            accountSeed: seed, class: reachClass, scopeId: scope,
            version: ReachRules.version(of: reachClass, scope: scope, in: reachState))
    }

    /// The grants a profile in `groupId` carries (ADR-0060 section 3 / 6): in a
    /// one-to-one conversation `contact` + `member`; in a group `member`, plus
    /// `group` when the setting lets that group's members find the owner.
    public func profileGrants(groupId: Data, oneToOne: Bool) -> [FfiReachGrant] {
        guard !groupId.isEmpty, groupId.count <= 64 else { return [] }
        var out: [FfiReachGrant] = []
        if oneToOne {
            if let contact = grant(.contact, scope: Data()) { out.append(contact) }
        } else if allowsScope(setting: setting, scope: ReachRules.groupScope(groupId)),
                  let group = grant(.group, scope: groupId) {
            out.append(group)
        }
        if let member = grant(.member, scope: groupId) { out.append(member) }
        return out
    }

    /// The `channel` grant a member card for `channelId` at seed epoch
    /// `epoch` carries, or `nil` when the setting does not let that channel's
    /// members find the owner.
    public func channelGrant(channelId: Data, epoch: UInt32) -> FfiReachGrant? {
        guard let seed = accountSeed,
              allowsScope(setting: setting, scope: FfiScopeId(kind: .channel, id: channelId))
        else { return nil }
        return try? deriveReachGrant(
            accountSeed: seed, class: .channel, scopeId: channelId, version: epoch)
    }

    /// Whether the members of `groupId` may find the owner (UX section 6.2).
    public func allowsGroup(_ groupId: Data) -> Bool {
        allowsScope(setting: setting, scope: ReachRules.groupScope(groupId))
    }

    /// The owner's own `contact` seed. Every device of the account derives it,
    /// and it is registered, so a write to this account's own mailbox (a
    /// Welcome for one of its own devices) is signed with it.
    public func ownContactSeed() -> Data? {
        grant(.contact, scope: Data())?.seed
    }

    /// The owner's `fof` grant, for an introduction record — `nil` while
    /// contacts of contacts may not find the owner.
    public func fofGrant() -> FfiReachGrant? {
        guard allowsContactsOfContacts(setting: setting) else { return nil }
        return grant(.fof, scope: Data())
    }

    /// A fresh single-use `reply` grant for an outgoing request, REGISTERED
    /// (7-day expiry) before it is returned: the answer must never find the
    /// mailbox closed to it. Throws when the registration fails, so the caller
    /// can refuse to send a request nobody could answer.
    public func newReplyGrant(purpose: String) async throws -> FfiReachGrant {
        guard let seed = accountSeed else { throw ReachError.notActive }
        var scope = Data(count: 16)
        let ok = scope.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!) }
        guard ok == errSecSuccess else { throw ReachError.random }
        let grant = try deriveReachGrant(
            accountSeed: seed, class: .reply, scopeId: scope, version: ReachRules.initialVersion)
        record.replies.append(ReplyReach(
            scopeIdB64: scope.base64EncodedString(),
            expiresAtMs: Self.nowMs() + ReachRules.replyLifetimeMs,
            purpose: purpose))
        persist()
        guard await syncOnce() else {
            record.replies.removeAll { $0.scopeIdB64 == scope.base64EncodedString() }
            persist()
            throw ReachError.registrationFailed
        }
        return grant
    }

    /// Retire every reply issued for `purpose` (the request was answered, or
    /// the group joined). Unregistered on the next sync.
    public func retireReplies(purpose: String) {
        let before = record.replies.count
        record.replies.removeAll { $0.purpose == purpose }
        guard record.replies.count != before else { return }
        persist()
        sync()
    }

    /// A `link` grant for a new `/i/` or `/c/` link, REGISTERED before it is
    /// returned (a link shared before its key is registered would be refused).
    public func newLinkGrant(linkId: Data, expiresAtMs: UInt64) async throws -> FfiReachGrant {
        guard let seed = accountSeed else { throw ReachError.notActive }
        let grant = try deriveReachGrant(
            accountSeed: seed, class: .link, scopeId: linkId, version: ReachRules.initialVersion)
        let entry = LinkReach(
            linkIdB64: linkId.base64EncodedString(), version: ReachRules.initialVersion,
            expiresAtMs: expiresAtMs)
        record.links.removeAll { $0.linkIdB64 == entry.linkIdB64 }
        record.links.append(entry)
        persist()
        guard await syncOnce() else {
            record.links.removeAll { $0.linkIdB64 == entry.linkIdB64 }
            persist()
            throw ReachError.registrationFailed
        }
        return grant
    }

    /// Record the ledger id a link was uploaded under, so a revoke by that id
    /// can unregister its key.
    public func attachBlob(linkId: Data, blobId: Data) {
        let b64 = linkId.base64EncodedString()
        guard let index = record.links.firstIndex(where: { $0.linkIdB64 == b64 }) else { return }
        record.links[index].blobIdB64 = blobId.base64EncodedString()
        persist()
    }

    /// Unregister the key of the link uploaded under `blobId` (revoked).
    public func removeLink(blobId: Data) {
        let b64 = blobId.base64EncodedString()
        guard record.links.contains(where: { $0.blobIdB64 == b64 }) else { return }
        record.links.removeAll { $0.blobIdB64 == b64 }
        persist()
        sync()
    }

    /// Unregister a revoked link's key.
    public func removeLink(linkId: Data) {
        let b64 = linkId.base64EncodedString()
        guard record.links.contains(where: { $0.linkIdB64 == b64 }) else { return }
        record.links.removeAll { $0.linkIdB64 == b64 }
        persist()
        sync()
    }

    // MARK: - Grants received

    /// Keep `grant` received through `carrier` from the person whose account
    /// key (base64) was resolved at receipt, when ``ReachRules/keeps(_:carrier:senderResolved:)``
    /// allows it. `scopeHex` is the group (or channel) the grant was given in.
    @discardableResult
    public func keep(
        _ grant: FfiReachGrant, carrier: ReachRules.Carrier,
        fromAccountKeyB64: String?, scopeHex: String?
    ) -> Bool {
        guard isActive,
              ReachRules.keeps(grant, carrier: carrier, senderResolved: fromAccountKeyB64 != nil),
              let key = fromAccountKeyB64 else { return false }
        let scope: String? = (grant.class == .contact || grant.class == .reply) ? nil : scopeHex
        let held = ReachRules.HeldGrant(grant: grant, scopeHex: scope, storedAtMs: Self.nowMs())
        record.held[key] = ReachRules.merging(held, into: record.held[key] ?? [])
        persist()
        onGrantKept?(key)
        return true
    }

    /// Withdraw the grant of `reachClass` held from the person with account
    /// key `fromAccountKeyB64` in `scopeHex` (a channel member card that no
    /// longer carries it: the person narrowed their setting). Returns whether
    /// one was held.
    @discardableResult
    public func withdrawHeld(
        _ reachClass: FfiReachClass, fromAccountKeyB64 key: String, scopeHex: String
    ) -> Bool {
        let name = ReachRules.className(reachClass)
        guard var held = record.held[key],
              held.contains(where: { $0.classRaw == name && $0.scopeHex == scopeHex }) else { return false }
        held.removeAll { $0.classRaw == name && $0.scopeHex == scopeHex }
        record.held[key] = held.isEmpty ? nil : held
        persist()
        return true
    }

    /// Whether a grant of `reachClass` is held from the person with account
    /// key `fromAccountKeyB64` in `scopeHex`.
    public func holdsGrant(
        _ reachClass: FfiReachClass, fromAccountKeyB64 key: String, scopeHex: String
    ) -> Bool {
        let name = ReachRules.className(reachClass)
        return record.held[key]?.contains { $0.classRaw == name && $0.scopeHex == scopeHex } ?? false
    }

    /// Drop every grant held for a person (they were removed as a contact and
    /// left every shared group) — best effort; their rotation retires them
    /// anyway.
    public func dropHeld(accountKeyB64: String) {
        guard record.held[accountKeyB64] != nil else { return }
        record.held[accountKeyB64] = nil
        persist()
    }

    /// How to sign a write to the mailbox of the person with account key
    /// `accountKey`: this account's own contact seed for its own mailbox, else
    /// the best grant held for them. `nil` when nothing is held — the write
    /// then goes unsigned, which only an Everyone mailbox stores.
    public func writeAuth(toAccountKey accountKey: Data, groupIdHex: String? = nil) -> DsMailboxWriteAuth? {
        if let own = ownAccountKey, own == accountKey, let seed = ownContactSeed() {
            return DsMailboxWriteAuth(reachSeed: seed)
        }
        let held = record.held[accountKey.base64EncodedString()] ?? []
        guard let seed = ReachRules.writeSeed(from: held, groupIdHex: groupIdHex) else { return nil }
        return DsMailboxWriteAuth(reachSeed: seed)
    }

    /// ``writeAuth(toAccountKey:groupIdHex:)`` for a write addressed by its
    /// mailbox pseudonym alone (a Welcome recorded in the ADR-0040 outbox):
    /// this account's own mailbox, then any person a grant is held for, then a
    /// committer delegation naming this account.
    public func writeAuth(toMailbox pseudonym: Data, groupIdHex: String? = nil) -> DsMailboxWriteAuth? {
        if let own = ownAccountKey, Self.mailbox(of: own) == pseudonym, let seed = ownContactSeed() {
            return DsMailboxWriteAuth(reachSeed: seed)
        }
        for (keyB64, held) in record.held {
            guard let key = Data(base64Encoded: keyB64), Self.mailbox(of: key) == pseudonym,
                  let seed = ReachRules.writeSeed(from: held, groupIdHex: groupIdHex) else { continue }
            return DsMailboxWriteAuth(reachSeed: seed)
        }
        return delegatedAuth(toMailbox: pseudonym)
    }

    /// ADR-0050 2.5a item 7 — every way this device can sign a write to the
    /// mailbox `pseudonym`, in the order a sweep's Welcome tries them, moving
    /// to the next on a `401`: this account's own `contact` key alone for its
    /// own mailbox (unchanged); otherwise every distinct grant held for the
    /// person whose mailbox it is (``ReachRules/writeSeeds(from:groupIdHex:)``),
    /// then a committer delegation naming this account. Empty when nothing is
    /// held: the caller then keeps the Welcome owed and never writes unsigned.
    public func writeAuthCandidates(toMailbox pseudonym: Data, groupIdHex: String? = nil) -> [DsMailboxWriteAuth] {
        if let own = ownAccountKey, Self.mailbox(of: own) == pseudonym {
            return ownContactSeed().map { [DsMailboxWriteAuth(reachSeed: $0)] } ?? []
        }
        var out: [DsMailboxWriteAuth] = []
        for (keyB64, held) in record.held.sorted(by: { $0.key < $1.key }) {
            guard let key = Data(base64Encoded: keyB64), Self.mailbox(of: key) == pseudonym else { continue }
            out += ReachRules.writeSeeds(from: held, groupIdHex: groupIdHex)
                .map { DsMailboxWriteAuth(reachSeed: $0) }
        }
        if let delegated = delegatedAuth(toMailbox: pseudonym) { out.append(delegated) }
        return out
    }

    static func mailbox(of accountKey: Data) -> Data? {
        try? IdentityPseudonym.fromIdentity(pubkey: accountKey).asBytes()
    }

    // MARK: - Committer delegations (ADR-0060 section 6)

    /// Newest delegation envelope per (sender key resolved at receipt, added
    /// account key). In memory only: a delegation lives at most 24 hours and
    /// its proposer re-sends while its proposal is pending.
    private var delegations: [String: FfiCommitterDelegationEnvelope] = [:]

    /// Keep a committer-delegation envelope received in a group from the
    /// member whose account key was resolved at receipt. Refused unless every
    /// delegation in it checks (`checkCommitterDelegations`).
    public func keepDelegation(_ envelope: FfiCommitterDelegationEnvelope, fromAccountKeyB64: String?) {
        guard let sender = fromAccountKeyB64,
              (try? checkCommitterDelegations(envelope: envelope)) != nil else { return }
        delegations[sender + "|" + envelope.addedAccountKey.base64EncodedString()] = envelope
        onGrantKept?(envelope.addedAccountKey.base64EncodedString())
    }

    /// The delegated write authorisation for a Welcome to `pseudonym`: a kept
    /// envelope for the account whose mailbox it is that names this account,
    /// signed with this account's own seed (the key the delegation names).
    private func delegatedAuth(toMailbox pseudonym: Data) -> DsMailboxWriteAuth? {
        guard let own = ownAccountKey, let seed = accountSeed else { return nil }
        for envelope in delegations.values where Self.mailbox(of: envelope.addedAccountKey) == pseudonym {
            if let header = try? committerDelegationFor(envelope: envelope, ownAccountKey: own) {
                return DsMailboxWriteAuth(reachSeed: seed, delegationHeader: header)
            }
        }
        return nil
    }

    /// The proposer's side: one delegation per committer candidate (the owner
    /// and each admin, by account key, at most 16), each signed with the grant
    /// this device holds for the added person, as a committer-delegation
    /// envelope ready to send in the group. `nil` when no grant is held for
    /// them (nothing to delegate) or there is no candidate.
    public func composeDelegations(
        addedAccountKey: Data, candidateAccountKeys: [Data], groupIdHex: String?
    ) -> Data? {
        guard let issuer = heldSeed(
            forAccountKeyB64: addedAccountKey.base64EncodedString(), groupIdHex: groupIdHex),
              let mailbox = Self.mailbox(of: addedAccountKey) else { return nil }
        let expires = Self.nowMs() + ReachRules.delegationLifetimeMs
        var blobs: [Data] = []
        var seen = Set<Data>()
        for candidate in candidateAccountKeys where seen.insert(candidate).inserted {
            guard blobs.count < Int(maxCommitterDelegations()) else { break }
            var nonce = Data(count: 16)
            guard nonce.withUnsafeMutableBytes({
                SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
            }) == errSecSuccess else { return nil }
            guard let blob = try? signDelegationBlob(
                issuerSeed: issuer, mailbox: mailbox, delegatePk: candidate,
                expiresAtMs: expires, nonce: nonce) else { continue }
            blobs.append(blob)
        }
        guard !blobs.isEmpty else { return nil }
        return try? composeCommitterDelegationEnvelope(
            envelope: FfiCommitterDelegationEnvelope(
                addedAccountKey: addedAccountKey, delegations: blobs))
    }

    /// The account keys (b64) this device holds any grant for.
    public func heldAccountKeys() -> [String] {
        Array(record.held.keys)
    }

    /// The seed of the best grant held for the person with account key `key`
    /// (b64): what a proposer signs committer delegations with, and what local
    /// people search offers a request under.
    public func heldSeed(forAccountKeyB64 key: String, groupIdHex: String?) -> Data? {
        ReachRules.writeSeed(from: record.held[key] ?? [], groupIdHex: groupIdHex)
    }


    // MARK: - Username certificate

    public var ownUsernameCertificate: Data? {
        record.ownUsernameCertificateB64.flatMap { Data(base64Encoded: $0) }
    }

    public func setOwnUsernameCertificate(_ cert: Data?) {
        guard isActive else { return }
        record.ownUsernameCertificateB64 = cert?.base64EncodedString()
        persist()
        grantRevision &+= 1
    }

    // MARK: - Everyone bookkeeping

    public var publishedEveryone: Bool? { record.publishedEveryone }

    public func setPublishedEveryone(_ value: Bool) {
        record.publishedEveryone = value
        persist()
    }

    // MARK: - Introductions storage (ADR-0060 section 4)

    func storeOffer(_ record: Data, fromAccountKeyB64 key: String) {
        guard isActive else { return }
        storeOfferValue(key, record.base64EncodedString())
    }

    private func storeOfferValue(_ key: String, _ value: String) {
        guard record.offers[key] != value else { return }
        record.offers[key] = value
        persist()
    }

    func storeBundle(_ bundle: FfiIntroductionsBundle, fromAccountKeyB64 key: String) {
        guard isActive else { return }
        if let held = record.bundles[key],
           !bundleIsNewer(incomingUpdatedAtMs: bundle.updatedAtMs, storedUpdatedAtMs: held.updatedAtMs) {
            return
        }
        record.bundles[key] = ReachRecord.StoredBundle(
            updatedAtMs: bundle.updatedAtMs,
            recordsB64: bundle.records.map { $0.base64EncodedString() })
        persist()
    }

    func relayedRecords() -> [Data] {
        record.bundles.values.flatMap { $0.recordsB64.compactMap { Data(base64Encoded: $0) } }
    }

    func offeredRecords() -> [Data] {
        record.offers.values.compactMap { Data(base64Encoded: $0) }
    }

    /// Forget what a removed contact offered and relayed.
    public func dropIntroductions(fromAccountKeyB64 key: String) {
        guard record.offers[key] != nil || record.bundles[key] != nil else { return }
        record.offers[key] = nil
        record.bundles[key] = nil
        persist()
    }

    // MARK: - Everyone (ADR-0060 section 5)

    /// What this device last published in Everyone (name, username and
    /// certificate), to republish when any of them changes. In memory only, so
    /// every session start republishes once.
    private var publishedEveryoneFor: String?

    /// Enter or leave Everyone in discovery to match the setting (ADR-0060
    /// section 5): on entering (and on a name, username or certificate change
    /// while in it) the lookup key of every `nameSearchTerms` term and the
    /// public record sealed under the OPRF outputs of those terms and the
    /// username; on leaving `public: false`. Best effort; retried on the next
    /// sync.
    public func applyEveryone(discovery: DiscoveryClient? = nil) async {
        guard isActive, let token = sessionTokenProvider() else { return }
        let wantPublic = setting.mode == .everyone
        let own = ownNameProvider()
        let name = own.displayName ?? ""
        let cert = own.username == nil ? nil : ownUsernameCertificate
        let publishKey = "\(name)|\(own.username ?? "")|\(cert?.base64EncodedString() ?? "")"
        if record.publishedEveryone == wantPublic, !wantPublic || publishedEveryoneFor == publishKey {
            return
        }
        let client = discovery ?? DiscoveryClient(sessionToken: token)
        do {
            var nameTermLookupKeys: [Data] = []
            var publicRecord: Data?
            if wantPublic {
                // The lookup keys go to discovery; the outputs seal the record
                // and never leave the device.
                var recordTermOutputs: [Data] = []
                let username = cert == nil ? nil : own.username
                if let username {
                    recordTermOutputs.append(try await client.usernameOprf(username).output)
                }
                for term in nameSearchTerms(displayName: name) {
                    let oprf = try await client.nameTermOprf(term)
                    nameTermLookupKeys.append(oprf.lookupKey)
                    recordTermOutputs.append(oprf.output)
                }
                guard let identityPk = ownAccountKey,
                      let accountId = own.accountId, accountId.count == 16,
                      !recordTermOutputs.isEmpty else {
                    NSLog("ADR-0060: Everyone not published: no identity, account id or search term")
                    return
                }
                let payload = FfiIntroductionPayload(
                    identityPk: identityPk, accountId: accountId,
                    displayName: name.isEmpty ? nil : name,
                    username: username, usernameCertificate: username == nil ? nil : cert, fof: nil)
                publicRecord = try composePublicRecord(
                    payload: payload, termOprfs: recordTermOutputs, issuedAtMs: Self.nowMs())
            }
            try await client.putVisibility(
                public: wantPublic, nameTermLookupKeys: nameTermLookupKeys, publicRecord: publicRecord)
            record.publishedEveryone = wantPublic
            publishedEveryoneFor = wantPublic ? publishKey : nil
            persist()
        } catch {
            NSLog("ADR-0060: Everyone publish failed: \(error)")
            if status == .applied { status = Self.isOffline(error) ? .pendingOffline : .failed }
        }
    }

    // MARK: - Contacts of contacts: this account's own record (section 4)

    /// This account's introduction record at the current `fof` version and
    /// name, or `nil` while contacts of contacts may not find it. Cached: every
    /// term costs one OPRF evaluation (one lookup token), so it is recomposed
    /// only when the name, the username or the `fof` version changed.
    public func ownIntroductionRecord(discovery: DiscoveryClient = DiscoveryClient()) async -> Data? {
        guard isActive, let fof = fofGrant(), let identityPk = ownAccountKey else { return nil }
        let own = ownNameProvider()
        guard let accountId = own.accountId, accountId.count == 16 else { return nil }
        let cacheKey = "\(fof.version)|\(own.displayName ?? "")|\(own.username ?? "")|"
            + (ownUsernameCertificate?.base64EncodedString() ?? "")
        if record.ownOfferFofVersion == fof.version, let cached = record.ownOfferB64,
           record.ownOfferCacheKey.first == cacheKey, let bytes = Data(base64Encoded: cached) {
            return bytes
        }
        // The username rides in the record whether or not its certificate is
        // held yet, as on web: the searcher shows it only after the
        // certificate verifies, and the record is findable by it either way.
        let username = own.username
        let cert = username == nil ? nil : ownUsernameCertificate
        // The record is sealed under the OPRF outputs; they never leave the
        // device (ADR-0060 section 5).
        var termOutputs: [Data] = []
        do {
            if let username { termOutputs.append(try await discovery.usernameOprf(username).output) }
            for term in nameSearchTerms(displayName: own.displayName ?? "") {
                termOutputs.append(try await discovery.nameTermOprf(term).output)
            }
        } catch {
            NSLog("ADR-0060: introduction terms not evaluated: \(error)")
            return nil
        }
        guard !termOutputs.isEmpty else { return nil }
        let payload = FfiIntroductionPayload(
            identityPk: identityPk, accountId: accountId, displayName: own.displayName,
            username: username, usernameCertificate: cert, fof: fof)
        guard let record = try? composeIntroduction(
            payload: payload, termOprfs: termOutputs, issuedAtMs: Self.nowMs()) else { return nil }
        self.record.ownOfferB64 = record.base64EncodedString()
        self.record.ownOfferFofVersion = fof.version
        self.record.ownOfferCacheKey = [cacheKey]
        persist()
        return record
    }

    /// Whether the own introduction record keyed `key` has not been offered
    /// in `groupIdHex` yet. Marked by ``markOfferSent(groupIdHex:key:)``
    /// after the send succeeded, so a failed send is retried.
    public func offerNeedsSending(groupIdHex: String, key: String) -> Bool {
        record.introductionsSentDigest[groupIdHex] != key
    }

    public func markOfferSent(groupIdHex: String, key: String) {
        guard record.introductionsSentDigest[groupIdHex] != key else { return }
        record.introductionsSentDigest[groupIdHex] = key
        persist()
    }

    /// Whether the relayed set digested `digest` has not been sent as a bundle
    /// in `groupIdHex` yet.
    public func bundleNeedsSending(groupIdHex: String, digest: String) -> Bool {
        record.bundleSentDigest?[groupIdHex] != digest
    }

    public func markBundleSent(groupIdHex: String, digest: String) {
        guard record.bundleSentDigest?[groupIdHex] != digest else { return }
        var sent = record.bundleSentDigest ?? [:]
        sent[groupIdHex] = digest
        record.bundleSentDigest = sent
        persist()
    }

    // MARK: - Internals

    private func persist() {
        guard let accountIdB64 else { return }
        persistence.save(record, accountIdB64: accountIdB64)
    }

    static func nowMs() -> UInt64 {
        UInt64(max(0, Date().timeIntervalSince1970 * 1000))
    }

    public enum ReachError: Error, LocalizedError {
        case notActive
        case random
        case registrationFailed

        public var errorDescription: String? {
            switch self {
            case .notActive: return "Sign in again to continue."
            case .random: return "Could not prepare the request. Try again."
            case .registrationFailed:
                return "Couldn't prepare the request. Check your connection and try again."
            }
        }
    }
}
