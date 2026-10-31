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
    /// ADR-0078 — this account's own mailbox (derived from the account seed;
    /// every device derives the same). Moves with a rotation.
    public private(set) var ownMailbox: MailboxTarget?
    /// The predecessor-mailbox addresses of this account's own proven old
    /// keys (ADR-0074 rule 4), from their seeds: a write recorded to one of
    /// them goes to ``ownMailbox`` instead.
    private var ownPredecessorMailboxes: Set<Data> = []
    /// Verified bindings, by `binding | account key`, so the per-write
    /// lookups do not repeat the same signature check.
    private var bindingCache: [Data: MailboxTarget?] = [:]
    private var record = ReachRecord()
    private var reachState = FfiReachState(entries: [], updatedAtMs: 0)
    private let ds: ReachDsTransport
    /// The ids of every group this account is in (set by the chat layer).
    public var groupIdsProvider: () -> [Data] = { [] }
    /// The ids of every channel this account subscribes to (set by the app
    /// shell): whether the one `channel` key is wanted, and which channel the
    /// account left (ADR-0078 section 9).
    public var channelIdsProvider: () -> [Data] = { [] }
    /// Every group and channel this account is in, as setting scopes (set by
    /// the app shell): what "All groups and channels" means when one of them is
    /// switched off from its info (UX section 5).
    public var allScopesProvider: () -> [FfiScopeId] = { [] }
    /// This account's contacts with a known account id, as (account key,
    /// account id): the list "Only the ones I choose" is made of. Set by the
    /// app.
    public var contactsProvider: () -> [(accountKey: Data, accountId: Data)] = { [] }
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
    /// Which ``sync()`` run ``syncTask`` belongs to. ``deactivate()`` moves
    /// it on, so a run it cancelled, finishing later, cannot clear the
    /// reference to the run the next account started (a second run would
    /// then start beside it).
    private var syncRun: UInt64 = 0

    /// ADR-0074 section 3.5a — who an account key is NOW: the proven
    /// predecessors of the people this account knows and this account's own
    /// old keys. Every mailbox this type derives from an account key, and
    /// every grant lookup, goes through it. Reloaded on activation, on a
    /// re-key and whenever the proven-predecessor store changes.
    public private(set) var keyResolver = AccountKeyResolver()
    /// Where ``keyResolver`` is read from. The default reads the per-account
    /// stores; tests inject their own.
    var keyResolverProvider: @MainActor (_ accountIdB64: String, _ ownCurrentKey: Data?) -> AccountKeyResolver = {
        accountIdB64, ownCurrentKey in
        AccountKeyResolver(
            proven: ProvenPredecessorStore(ownerAccountIdB64: accountIdB64).load(),
            ownCurrentKey: ownCurrentKey,
            ownPredecessorKeys: OwnAccountKeys.predecessorPublicKeys(accountIdB64: accountIdB64))
    }
    private var predecessorObserver: NSObjectProtocol?
    /// What the committer-delegation code reads the time from. Tests inject
    /// their own.
    var clockMs: @MainActor () -> UInt64 = { ReachCenter.nowMs() }
    /// This account's own predecessor keys with the Ed25519 seed of each
    /// (newest first): what signs a delegated write a proposer addressed to
    /// an old key of this account. The default reads the predecessor slot;
    /// tests inject their own.
    var ownPredecessorSigningSeeds: @MainActor (_ accountIdB64: String) -> [(key: Data, seed: Data)] = {
        accountIdB64 in
        KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64).reversed().compactMap { seed in
            guard let identity = try? Identity.fromSeed(seed: seed) else { return nil }
            return (identity.signaturePublicKey(), identity.ed25519Seed())
        }
    }

    /// ADR-0078 — the mailbox addresses of this account's own predecessor
    /// keys, each derived from its predecessor SEED. The default reads the
    /// predecessor slot; tests inject their own.
    var ownPredecessorMailboxAddresses: @MainActor (_ accountIdB64: String) -> [Data] = { accountIdB64 in
        KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64).compactMap { seed in
            (try? Identity.fromSeed(seed: seed)).map {
                MailboxKey.fromAccountIdentity(accountIdentity: $0).address()
            }
        }
    }

    init(persistence: ReachPersistence = KeychainReachPersistence(), ds: ReachDsTransport = DsClient()) {
        self.persistence = persistence
        self.ds = ds
    }

    /// Re-read ``keyResolver`` (a proven rotation was recorded, or this
    /// account adopted a rotation of its own keys). A change of who a key
    /// belongs to can unblock a Welcome owed for lack of a key.
    public func reloadKeyResolver() {
        guard let accountIdB64 else {
            keyResolver = AccountKeyResolver()
            return
        }
        ownPredecessorMailboxes = Set(ownPredecessorMailboxAddresses(accountIdB64))
        let next = keyResolverProvider(accountIdB64, ownAccountKey)
        guard next != keyResolver else { return }
        let before = keyResolver
        keyResolver = next
        // Tell the Welcome retry about every person whose current key moved.
        for key in next.predecessorKeys where before.current(of: key) != next.current(of: key) {
            onGrantKept?(next.current(of: key).base64EncodedString())
        }
    }

    private func observePredecessors() {
        guard predecessorObserver == nil else { return }
        predecessorObserver = NotificationCenter.default.addObserver(
            forName: ProvenPredecessorStore.changed, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.reloadKeyResolver() }
        }
    }

    // MARK: - Resolve before addressing (ADR-0074 section 3.5a rule 1)

    /// The current account key of the person `accountKey` belongs to: itself
    /// unless it was superseded BY PROOF (this account's own old keys
    /// included). An unproven change is never resolved.
    public func currentAccountKey(of accountKey: Data) -> Data {
        keyResolver.current(of: accountKey)
    }

    /// The address a write for the person holding `accountKey` goes to
    /// (ADR-0078: from a verified binding, never derived from the key).
    /// `nil` when this device knows no mailbox for them.
    public func mailbox(forAccountKey accountKey: Data) -> Data? {
        writePlan(toAccountKey: accountKey)?.target.address
    }

    /// Re-address a mailbox recorded earlier (a Welcome recorded in the
    /// ADR-0040 outbox before a rotation was observed): the person's current
    /// mailbox when `address` is one of their known mailboxes, else
    /// `address` itself.
    public func currentMailbox(for address: Data) -> Data {
        writePlan(toMailbox: address)?.target.address ?? address
    }
    /// Every grant held for the person `accountKey` belongs to, whichever of
    /// their keys each was filed under (rule 2): the newest of a class and
    /// scope wins, exactly as ``ReachRules/merging(_:into:)`` decides for one
    /// key.
    private func heldForPerson(_ accountKey: Data) -> [ReachRules.HeldGrant] {
        let keys = keyResolver.allKeys(of: accountKey).map { $0.base64EncodedString() }
        if keys.count == 1 { return record.held[keys[0]] ?? [] }
        // Rule 6 — the person's current address is known (a proven rotation
        // was observed), so a grant their rotation displaced opens nothing
        // this device will write to: only the current ones count.
        // Oldest first, so the newest of a class and scope is merged last; at
        // an equal stamp the grant filed under the nearer key (the current
        // one last) is the newer.
        let ranked = keys.reversed().enumerated().flatMap { rank, key in
            ReachRules.droppingSuperseded(record.held[key] ?? []).map { (rank: rank, grant: $0) }
        }.sorted { ($0.grant.storedAtMs, $0.rank) < ($1.grant.storedAtMs, $1.rank) }
        return ReachRules.droppingSuperseded(ranked.reduce(into: [ReachRules.HeldGrant]()) { out, entry in
            out = ReachRules.merging(entry.grant, into: out)
        })
    }

    /// The account key (current) of the person whose mailbox `address` is,
    /// among the people this device holds a grant or a committer delegation
    /// for, under any of their keys. `nil` for this account's own mailbox and
    /// for a mailbox nothing here names. What a refused write addressed by
    /// its mailbox alone re-checks (rule 6).
    public func accountKey(forMailbox address: Data) -> Data? {
        guard !isOwnMailbox(address) else { return nil }
        return person(forMailbox: address)
    }

    /// The person (current key) one of whose verified mailboxes is `address`.
    private func person(forMailbox address: Data) -> Data? {
        for person in heldPeople() + delegatedPeople()
        where knownTargets(ofPerson: person).contains(where: { $0.address == address }) {
            return keyResolver.isOwn(person) ? nil : person
        }
        return nil
    }

    private func isOwnMailbox(_ address: Data) -> Bool {
        address == ownMailbox?.address || ownPredecessorMailboxes.contains(address)
    }

    /// The distinct people grants are held for, by current account key.
    private func heldPeople() -> [Data] {
        var seen = Set<Data>()
        return record.held.keys.sorted().compactMap { Data(base64Encoded: $0) }
            .map { keyResolver.current(of: $0) }
            .filter { seen.insert($0).inserted }
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
        let signer = dsMailboxSigner(accountIdentity: accountIdentity)
        mailboxSigner = signer
        mailboxPseudonym = signer.mailboxAddress()
        ownMailbox = MailboxTarget.own(accountIdentity: accountIdentity)
        bindingCache = [:]
        record = persistence.load(accountIdB64: accountIdB64) ?? ReachRecord()
        heldEnvelopes = (record.keptEnvelopes ?? []).compactMap(Self.heldEnvelope(from:))
        pruneHeldEnvelopes()
        record.keptEnvelopes = heldEnvelopes.isEmpty ? nil : heldEnvelopes.map(\.row)
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
        observePredecessors()
        reloadKeyResolver()
        grantRevision &+= 1
    }

    /// ADR-0074 section 3.5, "DS" / "Reach" — the account's keys were
    /// rotated: every reach key and the mailbox derive from the account
    /// seed, so they move to the new one. The setting, the version counters
    /// and the grants this account HOLDS for other people are kept (they are
    /// filed under the contact, not under this account's key). What was
    /// registered was registered on the OLD mailbox: nothing is registered on
    /// the new one yet, so the next ``sync()`` registers everything there.
    /// The old mailbox and its keys are left as they are: nothing is ever
    /// registered there again (section 3.5a rule 4). The grants handed out so
    /// far open the OLD mailbox only; the profile re-send that carries the
    /// new ones is the caller's step (rule 3).
    public func rekey(accountIdB64: String, accountIdentity: Identity) {
        guard self.accountIdB64 == accountIdB64, accountSeed != nil else {
            activate(accountIdB64: accountIdB64, accountIdentity: accountIdentity)
            return
        }
        syncTask?.cancel()
        syncTask = nil
        syncAgain = false
        accountSeed = accountIdentity.ed25519Seed()
        ownAccountKey = accountIdentity.signaturePublicKey()
        let signer = dsMailboxSigner(accountIdentity: accountIdentity)
        mailboxSigner = signer
        mailboxPseudonym = signer.mailboxAddress()
        ownMailbox = MailboxTarget.own(accountIdentity: accountIdentity)
        bindingCache = [:]
        record.registeredB64 = []
        record.registeredOpen = nil
        // Section 3.5a rule 5 — the links and the reply grants handed out so
        // far were derived from the old seed and name the old mailbox: spent.
        // Nothing is registered for them on the new mailbox; they are kept
        // only so the rotation's owed step can revoke and re-send.
        let spent = ReachRules.spending(links: record.links, replies: record.replies,
                                        spentLinks: record.spentLinks ?? [],
                                        spentReplies: record.spentReplies ?? [])
        record.links = []
        record.replies = []
        record.spentLinks = spent.links
        record.spentReplies = spent.replies
        persist()
        // The old key is this account's own predecessor from now on.
        reloadKeyResolver()
        grantRevision &+= 1
    }

    /// No signed-in account (logout). The record stays on disk.
    public func deactivate() {
        syncTask?.cancel()
        syncTask = nil
        syncAgain = false
        syncRun &+= 1
        accountIdB64 = nil
        accountSeed = nil
        ownAccountKey = nil
        mailboxSigner = nil
        mailboxPseudonym = nil
        ownMailbox = nil
        ownPredecessorMailboxes = []
        bindingCache = [:]
        keyResolver = AccountKeyResolver()
        record = ReachRecord()
        heldEnvelopes = []
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
        let run = syncRun
        syncTask = Task { @MainActor [weak self] in
            guard let self else { return }
            repeat {
                // A run the account left behind neither consumes the next
                // run's follow-up request nor starts another pass.
                guard self.syncRun == run else { return }
                self.syncAgain = false
                _ = await self.syncOnce()
            } while self.syncRun == run && self.syncAgain && !Task.isCancelled
            if self.syncRun == run { self.syncTask = nil }
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
        let groupIds = groupIdsProvider()
        let channelIds = channelIdsProvider()
        // ADR-0078 section 9 — a group or channel left since the last
        // registration: its members still hold this account's one key of
        // that class, so the class rotates (and the profile carrying the new
        // grants is republished in every remaining group).
        let departures = ReachRules.departures(
            previous: record.memberOf, groupIds: groupIds, channelIds: channelIds)
        var leftClasses: [FfiReachClass] = []
        if departures.leftGroup { leftClasses += [.group, .member] }
        if departures.leftChannel { leftClasses.append(.channel) }
        if record.memberOf != departures.membership {
            record.memberOf = departures.membership
            persist()
        }
        if !leftClasses.isEmpty {
            rotateClasses(leftClasses, sync: false)
        }
        func desiredSpecs() -> [ReachKeySpec] {
            ReachRules.desiredKeys(
                setting: setting, state: reachState, groupIds: groupIds,
                links: record.links, replies: record.replies,
                channelIds: channelIds, nowMs: now,
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
        // ADR-0078 — the ADR-0073 R.4 continuity key is no longer registered
        // here: it was derivable from the account id so that peers could
        // reach a reset account at the mailbox derived from its new identity
        // key, and a mailbox is no longer derivable from an identity key. A
        // key still registered from before is removed by the diff below.
        let registered = Set(record.registeredB64.compactMap { Data(base64Encoded: $0) })
        let diff = ReachRules.registrationDiff(desired: desired, registered: registered)
        let open = ReachRules.mailboxOpen(setting)
        do {
            try await ds.updateReach(
                pseudonym: pseudonym, add: diff.add, remove: diff.remove, open: open, signer: signer)
        } catch {
            guard self.accountIdB64 == accountIdB64 else { return false }
            NSLog("ADR-0060: reach registration failed: \(type(of: error))")
            status = Self.isOffline(error) ? .pendingOffline : .failed
            return false
        }
        guard self.accountIdB64 == accountIdB64 else { return false }
        record.registeredB64 = diff.add.map { $0.publicKey.base64EncodedString() }
        record.registeredOpen = open
        record.replies.removeAll { $0.expiresAtMs <= now }
        record.links.removeAll { !$0.isLive(nowMs: now) }
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
            NSLog("ADR-0060: registered reach keys not read; nothing registered until they are: \(type(of: error))")
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
    ///
    /// ADR-0078 section 9, decision 4 — a change made HERE rotates the `group`
    /// and `channel` keys, so a group or channel the setting no longer allows
    /// stops holding a working key (one key serves every group and channel).
    /// A setting adopted from another device arrives with that device's
    /// bumped counters and is not rotated again.
    ///
    /// Returns whether the `fof` key moved: a change that narrows who is
    /// reached through the person's contacts bumps it at once
    /// (``ReachRules/narrowsContacts(from:to:allowsFof:)``).
    @discardableResult
    public func updateSetting(_ next: FfiVisibilitySetting) -> Bool {
        guard isActive else { return false }
        var stamped = next
        stamped.updatedAtMs = max(Self.nowMs(), setting.updatedAtMs &+ 1)
        guard let envelope = try? composeVisibilityEnvelope(setting: stamped) else {
            NSLog("ADR-0060: a setting the core refuses was not applied")
            return false
        }
        let narrowed = ReachRules.narrowsContacts(
            from: setting, to: stamped, allowsFof: { allowsContactsOfContacts(setting: $0) })
        setting = stamped
        record.settingEnvelopeB64 = envelope.base64EncodedString()
        persist()
        onDeviceEnvelope?(envelope)
        rotateClasses(narrowed ? [.group, .channel, .fof] : [.group, .channel], sync: true)
        return narrowed
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

    /// `Their contacts can find me` on a contact's screen (UX spec section
    /// 5.1). Returns whether the switch moved the setting to "Only the ones I
    /// choose", which is said once in place.
    @discardableResult
    public func setContactCanFindMe(accountId: Data, on: Bool) -> Bool {
        guard isActive, setting.contactsOfContacts else { return false }
        if setting.contactsForm == .all {
            guard !on,
                  let next = try? excludeContactUnderAll(
                      setting: setting, allContactIds: contactsProvider().map(\.accountId),
                      excluded: accountId)
            else { return false }
            updateSetting(next)
            return true
        }
        guard let next = try? setContactChosen(setting: setting, accountId: accountId, on: on) else {
            return false
        }
        updateSetting(next)
        return false
    }

    /// Removed contacts leave the chosen list (UX spec section 4.1). Returns
    /// whether that moved the `fof` key.
    public func dropChosenContacts(_ accountIds: [Data]) -> Bool {
        guard isActive else { return false }
        var next = setting
        for id in accountIds where next.chosenContacts.contains(id) {
            if let without = try? setContactChosen(setting: next, accountId: id, on: false) {
                next = without
            }
        }
        guard next.chosenContacts != setting.chosenContacts else { return false }
        return updateSetting(next)
    }

    /// The account id of the contact holding `accountKey`, when known.
    public func contactAccountId(ofKey accountKey: Data) -> Data? {
        contactsProvider().first { $0.accountKey == accountKey }?.accountId
    }

    // MARK: - Rotation

    /// Rotate one class — a new version, the old key unregistered on the next
    /// sync. Called when a contact is removed (`contact`, `fof`).
    public func rotate(_ reachClass: FfiReachClass) {
        rotateClasses([reachClass], sync: true)
    }

    /// A group lost a member: rotate this account's `group` and `member` keys
    /// in one registration and one republish. Since ADR-0078 section 9 they
    /// are the account's one key per class, so the profile carrying them is
    /// republished in EVERY group. `_groupId` is kept for the callers.
    public func rotateGroup(_ groupId: Data) {
        guard !groupId.isEmpty, groupId.count <= 64 else { return }
        rotateClasses([.group, .member], sync: true)
    }

    /// Bump each account-wide class in `classes` once, persist and hand the
    /// state to the other devices, then republish the profiles. With `sync`
    /// a registration pass follows (a pass already running asks for it).
    private func rotateClasses(_ classes: [FfiReachClass], sync runSync: Bool) {
        guard isActive, !classes.isEmpty else { return }
        let now = Self.nowMs()
        for reachClass in classes where ReachRules.isAccountWide(reachClass) {
            reachState = ReachRules.bumping(reachClass, scope: Data(), in: reachState, nowMs: now)
        }
        let envelope = try? composeReachStateEnvelope(state: reachState)
        record.reachStateEnvelopeB64 = envelope?.base64EncodedString()
        persist()
        grantRevision &+= 1
        if runSync { sync() }
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
        let scope = ReachRules.isAccountWide(reachClass) ? Data() : scope
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

    /// The `channel` grant a member card for `channelId` carries, or `nil`
    /// when the setting does not let that channel's members find the owner.
    /// ADR-0078 section 9: the account's one `channel` key, the same in every
    /// channel and no longer tied to the channel's seed epoch; it rotates when
    /// a channel is left or the setting changes.
    public func channelGrant(channelId: Data) -> FfiReachGrant? {
        guard allowsScope(setting: setting, scope: FfiScopeId(kind: .channel, id: channelId))
        else { return nil }
        return grant(.channel, scope: Data())
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
    /// `expiresAtMs` `nil` = the link never expires.
    public func newLinkGrant(linkId: Data, expiresAtMs: UInt64?) async throws -> FfiReachGrant {
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
    public func attachBlob(linkId: Data, blobId: Data, kind: String? = nil) {
        let b64 = linkId.base64EncodedString()
        guard let index = record.links.firstIndex(where: { $0.linkIdB64 == b64 }) else { return }
        record.links[index].blobIdB64 = blobId.base64EncodedString()
        record.links[index].kind = kind
        persist()
    }

    // MARK: - Spent by a key rotation (ADR-0074 section 3.5a rule 5)

    /// The link scopes that were live when the account's keys were rotated.
    public var spentLinks: [LinkReach] { record.spentLinks ?? [] }

    /// The reply scopes that were outstanding when the keys were rotated.
    public var spentReplies: [ReplyReach] { record.spentReplies ?? [] }

    /// A spent link was revoked at its ledger (or is gone there).
    public func forgetSpentLink(linkIdB64: String) {
        guard let links = record.spentLinks, links.contains(where: { $0.linkIdB64 == linkIdB64 }) else { return }
        record.spentLinks = links.filter { $0.linkIdB64 != linkIdB64 }
        persist()
    }

    /// The request a spent reply belonged to was re-sent, or cannot be.
    public func forgetSpentReplies(purpose: String) {
        guard let replies = record.spentReplies, replies.contains(where: { $0.purpose == purpose }) else { return }
        record.spentReplies = replies.filter { $0.purpose != purpose }
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
              let receivedUnder = fromAccountKeyB64 else { return false }
        // ADR-0078 — a grant names its owner's mailbox through the binding it
        // carries; it is kept only when that binding verifies against the
        // account key the sender was resolved to at receipt (C-68). A grant
        // whose binding was signed by someone else would route the person's
        // Welcomes to a mailbox they do not read.
        // A sender still bound to a PROVEN predecessor key (ADR-0074 Stage A)
        // hands out grants of its new seed: the binding may verify under any
        // key of the same person, and under no other.
        guard let senderKey = Data(base64Encoded: receivedUnder),
              keyResolver.allKeys(of: senderKey).contains(where: { key in
                  (try? reachGrantMailboxPkVerified(grant: grant, accountPk: key)) != nil
              }) else {
            NSLog("ADR-0078: a reach grant whose mailbox binding does not verify was not kept")
            return false
        }
        // ADR-0074 section 3.5a rule 2 — filed for the PERSON: a grant that
        // arrived from a leaf still bound to a proven predecessor key belongs
        // to the account's current key.
        let key = Data(base64Encoded: receivedUnder)
            .map { keyResolver.current(of: $0).base64EncodedString() } ?? receivedUnder
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
        var withdrew = false
        for personKey in personKeysB64(key) {
            guard var held = record.held[personKey],
                  held.contains(where: { $0.classRaw == name && $0.scopeHex == scopeHex }) else { continue }
            held.removeAll { $0.classRaw == name && $0.scopeHex == scopeHex }
            record.held[personKey] = held.isEmpty ? nil : held
            withdrew = true
        }
        if withdrew { persist() }
        return withdrew
    }

    /// ADR-0087 section 9 — forget every grant learned in one group or channel
    /// this account is no longer in. A grant held from the same person in
    /// another scope stays.
    public func forgetHeld(scopeHex: String) {
        var changed = false
        for (person, held) in record.held where held.contains(where: { $0.scopeHex == scopeHex }) {
            let kept = held.filter { $0.scopeHex != scopeHex }
            record.held[person] = kept.isEmpty ? nil : kept
            changed = true
        }
        if changed { persist() }
    }

    /// Every key (base64) of the person `keyB64` belongs to, `keyB64` itself
    /// when it does not decode.
    private func personKeysB64(_ keyB64: String) -> [String] {
        guard let key = Data(base64Encoded: keyB64) else { return [keyB64] }
        return keyResolver.allKeys(of: key).map { $0.base64EncodedString() }
    }

    private func heldForPerson(b64 keyB64: String) -> [ReachRules.HeldGrant] {
        guard let key = Data(base64Encoded: keyB64) else { return record.held[keyB64] ?? [] }
        return heldForPerson(key)
    }

    /// Whether a grant of `reachClass` is held from the person with account
    /// key `fromAccountKeyB64` in `scopeHex`.
    public func holdsGrant(
        _ reachClass: FfiReachClass, fromAccountKeyB64 key: String, scopeHex: String
    ) -> Bool {
        let name = ReachRules.className(reachClass)
        return heldForPerson(b64: key).contains { $0.classRaw == name && $0.scopeHex == scopeHex }
    }

    /// Drop every grant held for a person (they were removed as a contact and
    /// left every shared group) — best effort; their rotation retires them
    /// anyway.
    public func dropHeld(accountKeyB64: String) {
        let keys = personKeysB64(accountKeyB64).filter { record.held[$0] != nil }
        guard !keys.isEmpty else { return }
        for key in keys { record.held[key] = nil }
        persist()
    }

    // MARK: - Where a write goes (ADR-0078)

    public typealias WritePlan = MailboxWritePlan

    /// The verified binding `binding` names under `accountKey`, cached.
    private func target(binding: Data, accountKey: Data) -> MailboxTarget? {
        let cacheKey = binding + accountKey
        if let cached = bindingCache[cacheKey] { return cached }
        let verified = MailboxTarget.verifiedOrNil(binding: binding, accountKey: accountKey)
        bindingCache[cacheKey] = verified
        return verified
    }

    /// The mailbox a held grant writes to: its binding verified against one
    /// of the person's account keys (the key it was filed under first).
    private func target(of grant: ReachRules.HeldGrant, personKeys: [Data]) -> MailboxTarget? {
        guard let binding = grant.binding else { return nil }
        for key in personKeys {
            if let verified = target(binding: binding, accountKey: key) { return verified }
        }
        return nil
    }

    /// Every verified mailbox known for the person `person` (current key),
    /// from the grants held for them and the committer delegations kept for
    /// them, best first.
    private func knownTargets(ofPerson person: Data) -> [MailboxTarget] {
        var out = grantMailboxes(ofPerson: person, groupIdHex: nil).map(\.target)
        for target in delegationTargets(ofPerson: person) where !out.contains(target) {
            out.append(target)
        }
        return out
    }

    private func grantMailboxes(ofPerson person: Data, groupIdHex: String?) -> [(target: MailboxTarget, seeds: [Data])] {
        let keys = keyResolver.allKeys(of: person)
        let held = keys.flatMap { record.held[$0.base64EncodedString()] ?? [] }
        return ReachRules.writeMailboxes(from: held, groupIdHex: groupIdHex) { [self] grant in
            target(of: grant, personKeys: keys)
        }
    }

    /// How a write to the person with account key `accountKey` goes: this
    /// account's own mailbox (signed with its own contact seed) for its own
    /// key; otherwise the mailbox of the best grant held for the person
    /// (signed with every seed held for that mailbox), then a committer
    /// delegation's mailbox. `nil` when no mailbox is known: the caller
    /// writes nothing (there is no address to derive any more).
    ///
    /// ADR-0074 section 3.5a — `accountKey` is resolved first: a proven
    /// predecessor (this account's own old key included) is the person's
    /// current key.
    public func writePlan(toAccountKey accountKey: Data, groupIdHex: String? = nil) -> WritePlan? {
        if keyResolver.isOwn(accountKey) || accountKey == ownAccountKey {
            guard let ownMailbox else { return nil }
            return WritePlan(
                target: ownMailbox,
                candidates: ownContactSeed().map { [DsMailboxWriteAuth(reachSeed: $0)] } ?? [])
        }
        let person = currentAccountKey(of: accountKey)
        if let best = grantMailboxes(ofPerson: person, groupIdHex: groupIdHex).first {
            var candidates = best.seeds.map { DsMailboxWriteAuth(reachSeed: $0) }
            if let delegated = delegatedAuth(toMailbox: best.target.address) { candidates.append(delegated) }
            return WritePlan(target: best.target, candidates: candidates)
        }
        for target in delegationTargets(ofPerson: person) {
            if let delegated = delegatedAuth(toMailbox: target.address) {
                return WritePlan(target: target, candidates: [delegated])
            }
        }
        return nil
    }

    /// ``writePlan(toAccountKey:groupIdHex:)`` for a write addressed by a
    /// mailbox recorded earlier (a Welcome in the ADR-0040 outbox): this
    /// account's own mailbox (a predecessor's included, re-addressed to the
    /// current one), else the person one of whose known mailboxes it is,
    /// written to at their CURRENT best mailbox. `nil` for a mailbox nothing
    /// here names.
    public func writePlan(toMailbox address: Data, groupIdHex: String? = nil) -> WritePlan? {
        if isOwnMailbox(address), let own = ownAccountKey {
            return writePlan(toAccountKey: own, groupIdHex: groupIdHex)
        }
        guard let person = person(forMailbox: address) else { return nil }
        return writePlan(toAccountKey: person, groupIdHex: groupIdHex)
    }

    /// The first authorisation of ``writePlan(toAccountKey:groupIdHex:)``.
    public func writeAuth(toAccountKey accountKey: Data, groupIdHex: String? = nil) -> DsMailboxWriteAuth? {
        writePlan(toAccountKey: accountKey, groupIdHex: groupIdHex)?.candidates.first
    }

    /// The first authorisation of ``writePlan(toMailbox:groupIdHex:)``.
    public func writeAuth(toMailbox address: Data, groupIdHex: String? = nil) -> DsMailboxWriteAuth? {
        writePlan(toMailbox: address, groupIdHex: groupIdHex)?.candidates.first
    }

    /// ADR-0050 2.5a item 7 — every authorisation for a write recorded to
    /// `address`, in the order a sweep's Welcome tries them, moving to the
    /// next on a `401`. Empty when nothing is held: the caller then keeps the
    /// Welcome owed and never writes unsigned.
    public func writeAuthCandidates(toMailbox address: Data, groupIdHex: String? = nil) -> [DsMailboxWriteAuth] {
        writePlan(toMailbox: address, groupIdHex: groupIdHex)?.candidates ?? []
    }

    // ADR-0073 R.4 / ADR-0078 open question — a peer that accepted a reset
    // used to write a Welcome to the reset account's new mailbox under the
    // continuity key, at the address derived from the new identity key. A
    // mailbox address is no longer derivable from an identity key, and no
    // carrier yet brings the reset account's new mailbox binding, so the
    // continuity key is neither registered nor used on this client. A grant
    // the new identity itself hands out is the only way to its mailbox.

    // MARK: - Committer delegations (ADR-0060 section 6)

    /// One kept committer-delegation envelope, decoded from its durable row.
    private struct HeldEnvelope {
        let row: ReachRecord.KeptEnvelope
        let envelope: FfiCommitterDelegationEnvelope
        /// When the last of its delegations stops being honoured. `nil` for an
        /// envelope with no delegation: it names a binding, which does not
        /// expire.
        let expiresAtMs: UInt64?

        var isOpen: Bool { envelope.delegations.isEmpty }

        func sameSlot(as other: HeldEnvelope) -> Bool {
            row.groupIdHex == other.row.groupIdHex
                && row.senderKeyB64 == other.row.senderKeyB64
                && envelope.addedAccountKey == other.envelope.addedAccountKey
        }
    }

    /// The envelopes received in this account's groups, newest per (group,
    /// sender key resolved at receipt, added account key), oldest first. The
    /// decoded form of ``ReachRecord/keptEnvelopes``: a committer that
    /// restarts still holds them, and a Welcome it owes can still be written.
    private var heldEnvelopes: [HeldEnvelope] = []

    /// At most this many envelopes are kept, the oldest dropped first: a
    /// proposer re-sends a signed one while its Add waits. Chosen, not
    /// measured.
    static let maxKeptEnvelopes = 128

    private static func heldEnvelope(from row: ReachRecord.KeptEnvelope) -> HeldEnvelope? {
        guard let wire = Data(base64Encoded: row.envelopeB64),
              let envelope = try? decodeCommitterDelegationEnvelope(plaintext: wire),
              let checked = try? checkCommitterDelegations(envelope: envelope) else { return nil }
        return HeldEnvelope(row: row, envelope: envelope, expiresAtMs: checked.map(\.expiresAtMs).max())
    }

    /// Drop what expired, then the oldest over the cap.
    private func pruneHeldEnvelopes() {
        let now = clockMs()
        heldEnvelopes.removeAll { held in held.expiresAtMs.map { $0 <= now } ?? false }
        if heldEnvelopes.count > Self.maxKeptEnvelopes {
            heldEnvelopes.removeFirst(heldEnvelopes.count - Self.maxKeptEnvelopes)
        }
    }

    private func storeHeldEnvelopes() {
        record.keptEnvelopes = heldEnvelopes.isEmpty ? nil : heldEnvelopes.map(\.row)
        persist()
    }

    /// The kept envelopes that carry delegations, newest first.
    private var signedEnvelopes: [HeldEnvelope] {
        heldEnvelopes.reversed().filter { !$0.isOpen }
    }

    /// The kept envelopes with no delegation received in `groupIdHex`
    /// (lower-case), newest first.
    private func openEnvelopes(inGroup group: String) -> [HeldEnvelope] {
        heldEnvelopes.reversed().filter { $0.isOpen && $0.row.groupIdHex == group }
    }

    /// Keep a committer-delegation envelope received in a group from the
    /// member whose account key was resolved at receipt. Refused unless every
    /// delegation in it checks (`checkCommitterDelegations`, which also
    /// verifies the added person's mailbox binding against
    /// `addedAccountKey`). Kept durably, and every envelope, a re-sent one
    /// included, retries a Welcome owed to the person: the one it replaces may
    /// have expired.
    ///
    /// ADR-0060 section 6, "An add of a person found by search" — an
    /// envelope with NO delegation names a person the proposer found in
    /// Everyone: it is kept for the group it arrived in (`groupIdHex`; none
    /// given, it is dropped), its binding verified under the added account
    /// key, and lets a sweep of that group write that person's Welcome to
    /// that mailbox without a proof (``openWelcomeTarget(ofPerson:groupIdHex:)``).
    /// The newest envelope per (group, sender, added person) wins, whichever
    /// kind.
    public func keepDelegation(
        _ envelope: FfiCommitterDelegationEnvelope, fromAccountKeyB64: String?, groupIdHex: String? = nil
    ) {
        guard let sender = fromAccountKeyB64,
              let checked = try? checkCommitterDelegations(envelope: envelope),
              let wire = try? composeCommitterDelegationEnvelope(envelope: envelope) else { return }
        if envelope.delegations.isEmpty {
            guard groupIdHex != nil,
                  target(binding: envelope.addedMailboxBinding, accountKey: envelope.addedAccountKey) != nil
            else { return }
        }
        let held = HeldEnvelope(
            row: ReachRecord.KeptEnvelope(
                groupIdHex: groupIdHex?.lowercased() ?? "", senderKeyB64: sender,
                envelopeB64: wire.base64EncodedString(), storedAtMs: clockMs()),
            envelope: envelope,
            expiresAtMs: checked.map(\.expiresAtMs).max())
        heldEnvelopes.removeAll { $0.sameSlot(as: held) }
        heldEnvelopes.append(held)
        pruneHeldEnvelopes()
        storeHeldEnvelopes()
        onGrantKept?(envelope.addedAccountKey.base64EncodedString())
    }

    // MARK: - A person found in Everyone (ADR-0060 section 6)

    /// At most this many ``ReachRecord/openAdds`` are kept, the oldest
    /// dropped first. Chosen, not measured.
    static let maxOpenAdds = 256

    /// The proposer's side for a person found in Everyone: a
    /// committer-delegation envelope with no delegation, naming the person's
    /// verified mailbox `target`, ready to send in the group. `nil` when the
    /// core refuses it.
    public func composeOpenEnvelope(target: MailboxTarget) -> Data? {
        try? composeCommitterDelegationEnvelope(
            envelope: FfiCommitterDelegationEnvelope(
                addedAccountKey: target.accountKey,
                addedMailboxBinding: target.binding,
                delegations: []))
    }

    /// Record, durably, that this device proposed to add the person whose
    /// verified Everyone mailbox is `target` to `groupIdHex`: its own sweep
    /// reads this in place of the envelope it posted (a device never receives
    /// its own group messages). The binding is verified again here.
    public func recordOpenAdd(groupIdHex: String, target: MailboxTarget) {
        guard isActive,
              let verified = self.target(binding: target.binding, accountKey: target.accountKey) else { return }
        let group = groupIdHex.lowercased()
        let keyB64 = verified.accountKey.base64EncodedString()
        var adds = (record.openAdds ?? []).filter { !($0.groupIdHex == group && $0.accountKeyB64 == keyB64) }
        adds.append(ReachRecord.OpenAdd(
            groupIdHex: group, accountKeyB64: keyB64,
            bindingB64: verified.binding.base64EncodedString(), storedAtMs: Self.nowMs()))
        if adds.count > Self.maxOpenAdds { adds.removeFirst(adds.count - Self.maxOpenAdds) }
        record.openAdds = adds
        persist()
        onGrantKept?(keyB64)
    }

    /// The mailbox a sweep of `groupIdHex` may write the Welcome of `person`
    /// to WITHOUT a proof: the verified binding of a kept no-delegation
    /// envelope received in that group, or of this device's own record, that
    /// names this person (under any of their keys). `nil` otherwise, and
    /// always for this account's own key. The caller writes unsigned only
    /// when it holds no grant or delegation for the person.
    public func openWelcomeTarget(ofPerson person: Data, groupIdHex: String) -> MailboxTarget? {
        guard !keyResolver.isOwn(person), person != ownAccountKey else { return nil }
        let current = keyResolver.current(of: person)
        let group = groupIdHex.lowercased()
        for held in openEnvelopes(inGroup: group)
        where keyResolver.current(of: held.envelope.addedAccountKey) == current {
            let envelope = held.envelope
            if let verified = target(binding: envelope.addedMailboxBinding, accountKey: envelope.addedAccountKey) {
                return verified
            }
        }
        for add in (record.openAdds ?? []).reversed() where add.groupIdHex == group {
            guard let key = Data(base64Encoded: add.accountKeyB64),
                  keyResolver.current(of: key) == current,
                  let binding = Data(base64Encoded: add.bindingB64),
                  let verified = target(binding: binding, accountKey: key) else { continue }
            return verified
        }
        return nil
    }

    /// ``openWelcomeTarget(ofPerson:groupIdHex:)`` for a Welcome addressed
    /// by a mailbox: the open target whose address it is.
    public func openWelcomeTarget(forMailbox address: Data, groupIdHex: String) -> MailboxTarget? {
        let group = groupIdHex.lowercased()
        var people: [Data] = openEnvelopes(inGroup: group).map(\.envelope.addedAccountKey)
        people += (record.openAdds ?? []).filter { $0.groupIdHex == group }
            .compactMap { Data(base64Encoded: $0.accountKeyB64) }
        for person in people {
            if let open = openWelcomeTarget(ofPerson: person, groupIdHex: groupIdHex), open.address == address {
                return open
            }
        }
        return nil
    }

    /// The Welcome to `person` in `groupIdHex` was written: forget what let
    /// it go without a proof there.
    public func dropOpenAdd(groupIdHex: String, person: Data) {
        let current = keyResolver.current(of: person)
        let group = groupIdHex.lowercased()
        let heldBefore = heldEnvelopes.count
        heldEnvelopes.removeAll { held in
            held.isOpen && held.row.groupIdHex == group
                && keyResolver.current(of: held.envelope.addedAccountKey) == current
        }
        if heldEnvelopes.count != heldBefore { storeHeldEnvelopes() }
        guard let adds = record.openAdds else { return }
        let kept = adds.filter { add in
            guard add.groupIdHex == group, let key = Data(base64Encoded: add.accountKeyB64) else { return true }
            return keyResolver.current(of: key) != current
        }
        guard kept.count != adds.count else { return }
        record.openAdds = kept.isEmpty ? nil : kept
        persist()
    }

    /// ADR-0060 section 6 — keep the grant a search read for the person with
    /// account key `accountKey` (an introduction's `fof` grant) as a grant
    /// this device holds for them, so the committer delegations of an add are
    /// signed with it and this device can write their Welcome itself. Only a
    /// `fof` grant is read by a search without being held already; any other
    /// class is refused. Its binding must verify under one of the person's
    /// keys, as for every grant kept. Returns whether it was kept.
    @discardableResult
    public func keepFoundGrant(_ grant: FfiReachGrant, accountKey: Data) -> Bool {
        guard isActive, grant.class == .fof, grant.seed.count == 32,
              !keyResolver.isOwn(accountKey), accountKey != ownAccountKey,
              keyResolver.allKeys(of: accountKey).contains(where: { key in
                  (try? reachGrantMailboxPkVerified(grant: grant, accountPk: key)) != nil
              }) else {
            NSLog("ADR-0060: a grant found by search was not kept")
            return false
        }
        let key = keyResolver.current(of: accountKey).base64EncodedString()
        let held = ReachRules.HeldGrant(grant: grant, scopeHex: nil, storedAtMs: Self.nowMs())
        let merged = ReachRules.merging(held, into: record.held[key] ?? [])
        if merged != record.held[key] {
            record.held[key] = merged
            persist()
        }
        onGrantKept?(key)
        return true
    }

    /// The people (current keys) delegations are kept for.
    private func delegatedPeople() -> [Data] {
        var seen = Set<Data>()
        return signedEnvelopes.map { keyResolver.current(of: $0.envelope.addedAccountKey) }
            .filter { seen.insert($0).inserted }
    }

    /// The mailboxes the kept delegations for `person` name, each binding
    /// verified against the added account key the envelope carries.
    private func delegationTargets(ofPerson person: Data) -> [MailboxTarget] {
        var out: [MailboxTarget] = []
        for envelope in signedEnvelopes.map(\.envelope)
        where keyResolver.current(of: envelope.addedAccountKey) == person {
            guard let verified = target(
                binding: envelope.addedMailboxBinding, accountKey: envelope.addedAccountKey),
                  !out.contains(verified) else { continue }
            out.append(verified)
        }
        return out
    }

    /// The delegated write authorisation for a Welcome to `address`: a kept
    /// envelope whose added person's mailbox it is that names this account,
    /// signed with this account's own seed (the key the delegation names). The
    /// newest envelope first; an expired one is never offered (the DS would
    /// refuse it and the Welcome would wait on it).
    ///
    /// ADR-0074 section 3.5a — a proposer that has not observed this
    /// account's own rotation names the OLD account key: the delegation is
    /// then found under that key and the write signed with its seed
    /// (``ReachRules/delegationSigner(ownKeys:header:)``).
    private func delegatedAuth(toMailbox address: Data) -> DsMailboxWriteAuth? {
        guard let own = ownAccountKey, let seed = accountSeed, let accountIdB64 else { return nil }
        let ownKeys = [(key: own, seed: seed)] + ownPredecessorSigningSeeds(accountIdB64)
        let now = clockMs()
        for held in signedEnvelopes where (held.expiresAtMs ?? 0) > now {
            let envelope = held.envelope
            guard target(binding: envelope.addedMailboxBinding, accountKey: envelope.addedAccountKey)?
                .address == address else { continue }
            if let signer = ReachRules.delegationSigner(ownKeys: ownKeys, header: { key in
                try? committerDelegationFor(envelope: envelope, ownAccountKey: key)
            }) {
                return DsMailboxWriteAuth(reachSeed: signer.seed, delegationHeader: signer.header)
            }
        }
        return nil
    }

    /// The committer candidates one envelope can name, from `accountKeys`
    /// (the owner, then the admins): each by its current key, the key it signs
    /// the delegated write with, without repeats and at most
    /// `maxCommitterDelegations()` of them. What an envelope is composed for
    /// and what the re-send rule compares it with, so a candidate past the cap
    /// never counts as uncovered.
    public func delegationCandidates(_ accountKeys: [Data]) -> [Data] {
        var seen = Set<Data>()
        let unique = accountKeys.map { keyResolver.current(of: $0) }.filter { seen.insert($0).inserted }
        return Array(unique.prefix(Int(maxCommitterDelegations())))
    }

    /// ``composeDelegationEnvelope(addedAccountKey:candidateAccountKeys:groupIdHex:)``'s
    /// envelope alone.
    public func composeDelegations(
        addedAccountKey: Data, candidateAccountKeys: [Data], groupIdHex: String?
    ) -> Data? {
        composeDelegationEnvelope(
            addedAccountKey: addedAccountKey, candidateAccountKeys: candidateAccountKeys,
            groupIdHex: groupIdHex)?.wire
    }

    /// The proposer's side: one delegation per committer candidate (the owner
    /// and each admin, by account key, at most 16), each signed with the grant
    /// this device holds for the added person, as a committer-delegation
    /// envelope ready to send in the group, with the candidates it names.
    /// `nil` when no grant is held for them (nothing to delegate) or there is
    /// no candidate; nothing is signed then.
    public func composeDelegationEnvelope(
        addedAccountKey: Data, candidateAccountKeys: [Data], groupIdHex: String?
    ) -> (wire: Data, covered: [Data])? {
        // ADR-0074 section 3.5a rule 1 / ADR-0078 — the delegation is for
        // the mailbox the best grant held for the added person names; the
        // envelope carries that grant's binding and the account key it
        // verified under (the core refuses an envelope whose delegations name
        // another mailbox than its binding's); each committer candidate is
        // named by its current key, the key it signs the delegated write with.
        let person = keyResolver.current(of: addedAccountKey)
        guard let best = grantMailboxes(ofPerson: person, groupIdHex: groupIdHex).first,
              let issuer = best.seeds.first else { return nil }
        let expires = clockMs() + ReachRules.delegationLifetimeMs
        var blobs: [Data] = []
        var covered: [Data] = []
        for candidate in delegationCandidates(candidateAccountKeys) {
            var nonce = Data(count: 16)
            guard nonce.withUnsafeMutableBytes({
                SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
            }) == errSecSuccess else { return nil }
            guard let blob = try? signDelegationBlob(
                issuerSeed: issuer, mailbox: best.target.address, delegatePk: candidate,
                expiresAtMs: expires, nonce: nonce) else { continue }
            blobs.append(blob)
            covered.append(candidate)
        }
        guard !blobs.isEmpty,
              let wire = try? composeCommitterDelegationEnvelope(
                  envelope: FfiCommitterDelegationEnvelope(
                      addedAccountKey: best.target.accountKey,
                      addedMailboxBinding: best.target.binding,
                      delegations: blobs)) else { return nil }
        return (wire, covered)
    }

    /// The account keys (b64) this device holds any grant for.
    public func heldAccountKeys() -> [String] {
        heldPeople().map { $0.base64EncodedString() }
    }

    /// The seed of the best grant held for the person with account key `key`
    /// (b64): what a proposer signs committer delegations with.
    public func heldSeed(forAccountKeyB64 key: String, groupIdHex: String?) -> Data? {
        heldMailbox(forAccountKeyB64: key, groupIdHex: groupIdHex)?.seed
    }

    /// How an answer to a contact request reaches the person who sent it
    /// (``ReachRules/answerGrants(from:)``): one plan per verified mailbox, in
    /// the order the grants are tried. Empty when neither a `reply` nor a
    /// `contact` grant is held: nothing else may carry the answer, and nothing
    /// is written.
    public func answerPlans(toRequester accountKey: Data) -> [MailboxWritePlan] {
        let person = keyResolver.current(of: accountKey)
        let keys = keyResolver.allKeys(of: person)
        var plans: [MailboxWritePlan] = []
        for grant in ReachRules.answerGrants(from: heldForPerson(person)) {
            guard let seed = grant.seed, let target = target(of: grant, personKeys: keys) else { continue }
            let auth = DsMailboxWriteAuth(reachSeed: seed)
            if let index = plans.firstIndex(where: { $0.target == target }) {
                plans[index] = MailboxWritePlan(target: target, candidates: plans[index].candidates + [auth])
            } else {
                plans.append(MailboxWritePlan(target: target, candidates: [auth]))
            }
        }
        return plans
    }

    /// `docs/ux/who-can-find-me.md` section 6.1 — every grant a search may
    /// offer the person with account key `key` (b64) under, in the order they
    /// are tried (``ReachRules/searchGrants(from:)``), each with the VERIFIED
    /// mailbox it writes to. A grant whose binding does not verify is left
    /// out. SECRET.
    public func searchGrants(forAccountKeyB64 key: String) -> [KeptGrant] {
        guard let accountKey = Data(base64Encoded: key) else { return [] }
        let person = keyResolver.current(of: accountKey)
        let keys = keyResolver.allKeys(of: person)
        return ReachRules.searchGrants(from: heldForPerson(person)).compactMap { grant in
            guard let seed = grant.seed, let target = target(of: grant, personKeys: keys) else { return nil }
            return KeptGrant(seed: seed, mailbox: target)
        }
    }

    /// ADR-0078 — the best grant held for the person with account key `key`
    /// (b64) as a seed and the VERIFIED mailbox it writes to; `nil` when no
    /// held grant's binding verifies.
    public func heldMailbox(forAccountKeyB64 key: String, groupIdHex: String?) -> (seed: Data, target: MailboxTarget)? {
        guard let accountKey = Data(base64Encoded: key) else { return nil }
        let person = keyResolver.current(of: accountKey)
        guard let best = grantMailboxes(ofPerson: person, groupIdHex: groupIdHex).first,
              let seed = best.seeds.first else { return nil }
        return (seed, best.target)
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

    /// Keep a contact's offered record; returns whether the held set changed.
    @discardableResult
    func storeOffer(_ record: Data, fromAccountKeyB64 key: String) -> Bool {
        guard isActive else { return false }
        return storeOfferValue(key, record.base64EncodedString())
    }

    @discardableResult
    private func storeOfferValue(_ key: String, _ value: String) -> Bool {
        guard record.offers[key] != value else { return false }
        record.offers[key] = value
        persist()
        return true
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
        guard isActive, let token = sessionTokenProvider(), let accountIdB64 else { return }
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
                guard let identityPk = ownAccountKey, let binding = ownMailbox?.binding,
                      let accountId = own.accountId, accountId.count == 16,
                      !recordTermOutputs.isEmpty else {
                    NSLog("ADR-0060: Everyone not published: no identity, account id or search term")
                    return
                }
                let payload = FfiIntroductionPayload(
                    identityPk: identityPk, accountId: accountId,
                    displayName: name.isEmpty ? nil : name,
                    username: username, usernameCertificate: username == nil ? nil : cert,
                    mailboxBinding: binding, fof: nil)
                publicRecord = try composePublicRecord(
                    payload: payload, termOprfs: recordTermOutputs, issuedAtMs: Self.nowMs())
            }
            // The account changed while the terms were being evaluated: what
            // was composed is the old account's and is not published with
            // whatever token is live now.
            guard self.accountIdB64 == accountIdB64 else { return }
            try await client.putVisibility(
                public: wantPublic, nameTermLookupKeys: nameTermLookupKeys, publicRecord: publicRecord)
            // ...and the bookkeeping is not written into the next account's
            // record.
            guard self.accountIdB64 == accountIdB64 else { return }
            record.publishedEveryone = wantPublic
            publishedEveryoneFor = wantPublic ? publishKey : nil
            persist()
        } catch {
            guard self.accountIdB64 == accountIdB64 else { return }
            NSLog("ADR-0060: Everyone publish failed: \(error)")
            if status == .applied { status = Self.isOffline(error) ? .pendingOffline : .failed }
        }
    }

    // MARK: - Contacts of contacts: this account's own record (section 4)

    private static let termRateLimitWaitNs: UInt64 = 8_000_000_000

    /// This account's introduction record at the current `fof` version and
    /// name, or `nil` while contacts of contacts may not find it. Cached: every
    /// term costs one OPRF evaluation (one lookup token), so it is recomposed
    /// only when the name, the username or the `fof` version changed.
    public func ownIntroductionRecord(discovery: DiscoveryClient = DiscoveryClient()) async -> Data? {
        guard isActive, let fof = fofGrant(), let identityPk = ownAccountKey,
              let binding = ownMailbox?.binding, let accountIdB64 else { return nil }
        let own = ownNameProvider()
        guard let accountId = own.accountId, accountId.count == 16 else { return nil }
        // The binding is in the key: a rotation keeps the version and moves
        // the mailbox (ADR-0078), and the cached record names the old one.
        let cacheKey = "\(fof.version)|\(binding.base64EncodedString())|\(own.displayName ?? "")|\(own.username ?? "")|"
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
        // Each OPRF evaluation costs an anonymous token, so an output is kept
        // as soon as it is known: a failed build resumes where it stopped and
        // a changed input re-evaluates only its new terms (web and Android
        // keep theirs the same way). Without this every attempt spent a token
        // per term, and the retries ran through the account's daily quota.
        var evaluators: [(key: String, evaluate: () async throws -> Data)] = []
        if let username {
            evaluators.append(("u:\(username)", { try await discovery.usernameOprf(username).output }))
        }
        for term in nameSearchTerms(displayName: own.displayName ?? "") {
            evaluators.append(("n:\(term)", { try await discovery.nameTermOprf(term).output }))
        }
        let missing = ReachRules.termsToEvaluate(
            cached: Set((record.introductionTermOutputsB64 ?? [:]).keys), terms: evaluators.map(\.key))
        do {
            for entry in evaluators where missing.contains(entry.key) {
                let output: Data
                do {
                    output = try await entry.evaluate()
                } catch DiscoveryClientError.rateLimited {
                    // A long name has more terms than the server's burst, and
                    // the build runs only on an event, so it waits here once
                    // rather than until something next happens. Chosen, not
                    // measured: the bucket refills about one per second.
                    try await Task.sleep(nanoseconds: Self.termRateLimitWaitNs)
                    output = try await entry.evaluate()
                }
                // The account changed while the term was being evaluated:
                // its output is not kept in the next account's reach record.
                guard self.accountIdB64 == accountIdB64 else { return nil }
                var kept = record.introductionTermOutputsB64 ?? [:]
                kept[entry.key] = output.base64EncodedString()
                record.introductionTermOutputsB64 = kept
                persist()
            }
        } catch {
            NSLog("ADR-0060: introduction terms not evaluated: \(error)")
            return nil
        }
        // The account changed while the terms were being evaluated: its
        // record is neither cached in the next account's reach record nor
        // handed to a caller that would send it.
        guard self.accountIdB64 == accountIdB64 else { return nil }
        let termOutputs = evaluators.compactMap { entry in
            record.introductionTermOutputsB64?[entry.key].flatMap { Data(base64Encoded: $0) }
        }
        guard !termOutputs.isEmpty, termOutputs.count == evaluators.count else { return nil }
        let payload = FfiIntroductionPayload(
            identityPk: identityPk, accountId: accountId, displayName: own.displayName,
            username: username, usernameCertificate: cert, mailboxBinding: binding, fof: fof)
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

    /// ADR-0060 section 4, "A device that holds nothing asks" — what this
    /// device holds from the person `key` (b64), under any of their keys.
    public func heldIntroductions(fromAccountKeyB64 key: String) -> ReachRules.HeldIntroductions {
        let keys = personKeysB64(key)
        let contact = ReachRules.className(.contact)
        return ReachRules.HeldIntroductions(
            offer: keys.contains { record.offers[$0] != nil },
            bundle: keys.contains { record.bundles[$0] != nil },
            contactGrant: heldForPerson(b64: key).contains { $0.classRaw == contact })
    }

    /// An ask arrived in `groupIdHex`: forget that conversation, and only
    /// that one, in the two sent records, so the next publication run sends
    /// the offer and the bundle again. Returns whether it did
    /// (``ReachRules/resetsIntroductions(isOneToOneWithAsker:lastResetAtMs:nowMs:)``).
    @discardableResult
    public func noteIntroductionsAsked(
        groupIdHex: String, isOneToOneWithAsker: Bool, nowMs: UInt64
    ) -> Bool {
        guard isActive, ReachRules.resetsIntroductions(
            isOneToOneWithAsker: isOneToOneWithAsker,
            lastResetAtMs: record.introductionsResetAtMs?[groupIdHex], nowMs: nowMs) else { return false }
        record.introductionsSentDigest[groupIdHex] = nil
        record.bundleSentDigest?[groupIdHex] = nil
        var resets = record.introductionsResetAtMs ?? [:]
        resets[groupIdHex] = nowMs
        record.introductionsResetAtMs = resets
        persist()
        return true
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
