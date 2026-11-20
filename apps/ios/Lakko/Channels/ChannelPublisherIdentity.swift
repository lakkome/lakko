// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The two identities the channel publisher paths run on (ADR-0090).
///
/// Swift mirror of Android's `ChannelPublisherIdentity` and of the web
/// channel provider.
///
/// ## ADR-0090: a PublisherGroup leaf is this DEVICE
///
/// A channel's PublisherGroup is a group of devices: each device of the owner
/// and of every co-publisher holds its own leaf, the ADR-0050 bound device
/// identity that already holds the device's conversation leaves. So the MLS
/// identity here IS the chat repository's canonical (device) identity, and
/// the PublisherGroups live in the same OpenMLS provider as the
/// conversations. That is forced, not chosen: an owner device admits a
/// sibling by the KeyPackage the sibling published to discovery for its
/// conversations, whose private keys are in that provider and nowhere else,
/// so only that provider opens the Welcome.
///
/// The account-level work around the group (the journal root of ADR-0057,
/// the `/g/` capability the owner signs, the group id ADR-0078 section 6
/// derives, the co-publisher binding and the mailbox) runs on the account's
/// LINEAGE identity (the oldest own predecessor after an ADR-0074 rotation,
/// else the current seed), which is also the account the device binding
/// names. ``account(currentSeed:)`` returns it, and ``ownerAccountSeed`` its
/// 32-byte seed, the `ownerAccountSeed` of `PublisherGroup.create` /
/// `publisherGroupLoad`.
///
/// ## Discipline
///
///   * Everything runs on the main actor, so a snapshot never captures a
///     provider half-way through one FFI call.
///   * ``persistOrThrow()`` writes the device provider (the chat
///     repository's snapshot), fail closed: a PublisherGroup commit is
///     staged and published only with it on disk, and an application message
///     leaves only after its ratchet step is on disk. ``persist()`` is the
///     best-effort write after any other mutation.
@MainActor
public final class ChannelPublisherIdentity {

    /// The process-wide instance.
    public static let shared = ChannelPublisherIdentity()

    /// The record suffix of the pre-ADR-0090 channel provider snapshot,
    /// deleted on Forget device (flag day: its account-key leaves are not
    /// migrated).
    public nonisolated static let snapshotScope = ".channels"

    /// Thrown when no account is bound, the device identity is not
    /// installed, or the account's seed is unknown.
    public struct NotBound: Error {}

    /// The device identity of the bound account (the chat repository's
    /// canonical identity). Wired by the composition root.
    var deviceIdentityProvider: @MainActor () -> Identity? = { nil }
    /// Persist the device provider durably, throwing when it was not written.
    /// Wired by the composition root.
    var devicePersister: @MainActor () throws -> Void = { throw NotBound() }

    private let boundAccount: () -> String?
    private var account: Identity?
    private var boundAccountIdB64: String?
    private var boundSeed: Data?

    init(boundAccount: @escaping () -> String? = { AcceptedContactsStore.boundAccountIdB64 }) {
        self.boundAccount = boundAccount
    }

    /// The account the identity is bound to, or `nil`.
    public var boundAccountId: String? { boundAccountIdB64 }

    /// The DEVICE identity the PublisherGroups are opened, founded and joined
    /// with. `currentSeed` is the account's current phrase seed; the lineage
    /// seed it implies is noted (``ownerAccountSeed``) for the group id.
    public func identity(currentSeed: Data) throws -> Identity {
        _ = try account(currentSeed: currentSeed)
        guard let device = deviceIdentityProvider(), device.deviceBinding() != nil else {
            throw NotBound()
        }
        return device
    }

    /// The account's lineage identity: journal roots, link signing, the
    /// co-publisher binding, the mailbox. Idempotent for the same account
    /// and lineage seed.
    public func account(currentSeed: Data) throws -> Identity {
        guard let accountId = boundAccount() else { throw NotBound() }
        let bindingKey = deviceIdentityProvider()?.accountPublicKey()
            ?? Self.storedDeviceBindingKey(accountIdB64: accountId)
        let lineage = Self.ownerSeed(
            lineage: DeviceGroupLifecycle.lineageSeed(accountIdB64: accountId, currentSeed: currentSeed)
                ?? currentSeed,
            currentSeed: currentSeed,
            boundAccountKey: bindingKey)
        Self.noteSessionSeeds(
            accountIdB64: accountId, current: currentSeed, owner: lineage, fromBinding: bindingKey != nil)
        if let account, boundAccountIdB64 == accountId, boundSeed == lineage {
            Self.noteOwnerSeed(lineage)
            return account
        }
        let fresh = try Identity.fromSeed(seed: lineage)
        account = fresh
        boundAccountIdB64 = accountId
        boundSeed = lineage
        Self.noteOwnerSeed(lineage)
        return fresh
    }

    /// The seed the PublisherGroup's owner account key derives from: the
    /// lineage seed, unless the device binding names the current key (an
    /// ADR-0074 device that kept its binding under the current key), in
    /// which case the current seed, since `create` / `load` refuse a device
    /// bound to another account than the seed's.
    nonisolated static func ownerSeed(lineage: Data, currentSeed: Data, boundAccountKey: Data?) -> Data {
        guard let boundAccountKey, lineage != currentSeed,
              let lineageKey = try? Identity.fromSeed(seed: lineage).signaturePublicKey(),
              lineageKey != boundAccountKey,
              let currentKey = try? Identity.fromSeed(seed: currentSeed).signaturePublicKey(),
              currentKey == boundAccountKey else { return lineage }
        return currentSeed
    }

    /// Snapshot the device provider now, best effort.
    public func persist() {
        do {
            try persistOrThrow()
        } catch {
            NSLog("Channels: the device provider snapshot was not persisted")
        }
    }

    /// Snapshot the device provider now, throwing when it was not written:
    /// no account bound, another account bound since, no device identity, or
    /// a refused or failed store write.
    public func persistOrThrow() throws {
        guard let accountId = boundAccount(), boundAccountIdB64 == nil || boundAccountIdB64 == accountId,
              deviceIdentityProvider() != nil else { throw NotBound() }
        try devicePersister()
    }

    /// Drop the cached account identity. Log out.
    public func unbind() {
        account = nil
        boundAccountIdB64 = nil
        boundSeed = nil
        Self.noteOwnerSeed(nil)
        Self.noteCurrentSeed(nil)
    }

    /// Forget device: delete `accountIdB64`'s pre-ADR-0090 channel provider
    /// snapshot (the PublisherGroups now live in the chat provider, wiped
    /// with it).
    public nonisolated static func deleteOnDeviceSnapshot(accountIdB64: String) {
        IdentitySnapshotStore(scope: snapshotScope).deleteSnapshot(accountIdB64: accountIdB64)
    }

    // MARK: - The owner seed, readable off the main actor

    private nonisolated static let seedLock = NSLock()
    private nonisolated(unsafe) static var currentOwnerSeed: Data?

    private nonisolated(unsafe) static var currentAccountSeed: Data?
    /// The account the noted seeds belong to.
    private nonisolated(unsafe) static var notedAccountIdB64: String?

    nonisolated static func noteCurrentSeed(_ seed: Data?) {
        seedLock.lock()
        currentAccountSeed = seed
        if seed == nil {
            notedAccountIdB64 = nil
            notedLineageSeed = nil
        }
        seedLock.unlock()
    }

    /// Note both seeds of `accountIdB64` at once (the session landing, the
    /// channel identity's binding, or the first proof of a session).
    nonisolated static func noteSessionSeeds(
        accountIdB64: String, current: Data, owner: Data, fromBinding: Bool = true
    ) {
        seedLock.lock()
        if notedAccountIdB64 != accountIdB64 || currentAccountSeed != current { notedLineageSeed = nil }
        notedAccountIdB64 = accountIdB64
        currentAccountSeed = current
        currentOwnerSeed = owner
        ownerFromBinding = fromBinding
        seedLock.unlock()
    }

    /// 2026-11-20 — the session landing notes the account's seeds before any
    /// channel request, so the proof mapping holds from the first request
    /// (``proofIdentity(role:passed:)``); the device binding, when the chat
    /// provider holds it already, decides the owner seed exactly as
    /// ``account(currentSeed:)`` does.
    static func installSession(accountIdB64: String, currentSeed: Data) {
        let lineage = DeviceGroupLifecycle.lineageSeed(accountIdB64: accountIdB64, currentSeed: currentSeed)
            ?? currentSeed
        let bindingKey = shared.deviceIdentityProvider()?.accountPublicKey()
            ?? storedDeviceBindingKey(accountIdB64: accountIdB64)
        let owner = ownerSeed(lineage: lineage, currentSeed: currentSeed, boundAccountKey: bindingKey)
        noteSessionSeeds(accountIdB64: accountIdB64, current: currentSeed, owner: owner, fromBinding: bindingKey != nil)
    }

    /// Whether the noted owner seed was decided with the device binding
    /// known (landing with the chat provider up, the channel identity's
    /// binding, or a stored binding read by the fallback). When not, the
    /// fallback reads the stored binding again on the next proof.
    private nonisolated(unsafe) static var ownerFromBinding = false
    /// The oldest own predecessor seed of the noted account (or its current
    /// seed when it never rotated); cleared with the other notes.
    private nonisolated(unsafe) static var notedLineageSeed: Data?

    private nonisolated static func noteLineage(_ seed: Data) {
        seedLock.lock()
        notedLineageSeed = seed
        seedLock.unlock()
    }

    /// The account seeds a proof is decided with: the current one, the owner
    /// (binding-aware lineage) one, and the oldest own predecessor.
    struct ProofSeeds: Equatable {
        let current: Data
        let owner: Data
        let lineage: Data
    }

    /// The seeds of the bound account; derived from storage when nothing was
    /// noted for it yet, or when its owner seed was noted without the device
    /// binding: the account seed, its own predecessors
    /// (``DeviceGroupLifecycle/lineageSeed(accountIdB64:currentSeed:predecessors:conversationKeys:)``)
    /// and the stored device binding (`DeviceIdentityStore`, read as the chat
    /// repository reads it at landing), so no launch order leaves a proof
    /// decided by a guess.
    nonisolated static func sessionSeeds(
        boundAccountIdB64: String? = AcceptedContactsStore.boundAccountIdB64,
        storedCurrentSeed: (String) -> Data? = { (try? AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: $0)) ?? nil },
        lineageSeed: (String, Data) -> Data? = { DeviceGroupLifecycle.lineageSeed(accountIdB64: $0, currentSeed: $1) },
        storedBindingKey: (String) -> Data? = { storedDeviceBindingKey(accountIdB64: $0) }
    ) -> ProofSeeds? {
        seedLock.lock()
        let noted = (notedAccountIdB64, currentAccountSeed, currentOwnerSeed, ownerFromBinding)
        let notedLineage = notedLineageSeed
        seedLock.unlock()
        let account: String
        if let notedAccount = noted.0, boundAccountIdB64 == nil || boundAccountIdB64 == notedAccount {
            account = notedAccount
        } else if let bound = boundAccountIdB64 {
            account = bound
        } else {
            return nil
        }
        let sameAccount = noted.0 == account
        guard let current = (sameAccount ? noted.1 : nil) ?? storedCurrentSeed(account) else { return nil }
        let lineage = (sameAccount ? notedLineage : nil) ?? lineageSeed(account, current) ?? current
        if sameAccount, let owner = noted.2, noted.3 {
            noteLineage(lineage)
            return ProofSeeds(current: current, owner: owner, lineage: lineage)
        }
        let bindingKey = storedBindingKey(account)
        let owner = ownerSeed(lineage: lineage, currentSeed: current, boundAccountKey: bindingKey)
        // A binding not readable now: the owner seed is the plain lineage
        // until it is; owner proofs take the current key either way.
        noteSessionSeeds(accountIdB64: account, current: current, owner: owner, fromBinding: bindingKey != nil)
        noteLineage(lineage)
        return ProofSeeds(current: current, owner: owner, lineage: lineage)
    }

    /// The account key the stored device binding names, or `nil` when none is
    /// stored or it does not open (read as the chat repository reads it).
    nonisolated static func storedDeviceBindingKey(accountIdB64: String) -> Data? {
        guard let seed = (try? DeviceIdentityStore.getDeviceIdentitySeed(accountIdB64: accountIdB64)) ?? nil,
              let binding = DeviceIdentityStore.getDeviceBinding(accountIdB64: accountIdB64),
              let device = try? Identity.fromSeedWithBinding(seed: seed, binding: binding) else { return nil }
        return device.accountPublicKey()
    }

    /// 2026-11-20 (ADR-0074) — the identity a channel publisher proof of
    /// `role` is signed with, whatever account identity the caller passed:
    ///
    ///   * owner: the CURRENT seed's, the key the owner key moves to on a
    ///     rotation (``ChannelOwnerHandover``, ``ChannelOwnerRepair``), for
    ///     the current identity and any own earlier one (the lineage, the
    ///     owner seed), whatever the device binding says or whether it could
    ///     be read;
    ///   * co-publisher: the owner-account seed's, the identity the binding
    ///     the channel's owner appointed was composed with (the lineage,
    ///     unless the device binding names the current key). A co-publisher
    ///     cannot move its own appointment.
    ///
    /// Web and Android sign by the same rule. An identity that is none of the
    /// bound account's (a test, another account) is used as passed.
    nonisolated static func proofIdentity(role: ChannelPublisherRole, passed: Identity) -> Identity {
        proofIdentity(role: role, passed: passed, seeds: sessionSeeds())
    }

    nonisolated static func proofIdentity(
        role: ChannelPublisherRole, passed: Identity, seeds: ProofSeeds?
    ) -> Identity {
        guard let seeds else { return passed }
        // No rotation: one identity, nothing to choose (and no derivation).
        if seeds.current == seeds.owner, seeds.current == seeds.lineage { return passed }
        guard let currentId = try? Identity.fromSeed(seed: seeds.current) else { return passed }
        var accountKeys: Set<Data> = [currentId.signaturePublicKey()]
        var ownerId: Identity?
        for seed in Set([seeds.owner, seeds.lineage]) {
            guard let id = try? Identity.fromSeed(seed: seed) else { continue }
            accountKeys.insert(id.signaturePublicKey())
            if seed == seeds.owner { ownerId = id }
        }
        guard accountKeys.contains(passed.signaturePublicKey()) else { return passed }
        switch role {
        case .owner: return currentId
        case .copublisher: return ownerId ?? passed
        }
    }

    nonisolated static func noteOwnerSeed(_ seed: Data?) {
        seedLock.lock()
        currentOwnerSeed = seed
        seedLock.unlock()
    }

    /// The owner account seed of the bound account, as last computed by
    /// ``account(currentSeed:)``; `nil` before the first call of a session.
    public nonisolated static var ownerAccountSeed: Data? {
        seedLock.lock()
        defer { seedLock.unlock() }
        return currentOwnerSeed
    }

    /// The 32-byte Ed25519 seed of the owner account (the account identity
    /// of ``ownerAccountSeed``): what the core's PublisherGroup calls take
    /// as `ownerAccountSeed` (create, load, enter, the group id). The
    /// identity seed above is 64 bytes and the core refuses it.
    public nonisolated static var ownerAccountEd25519Seed: Data? {
        guard let seed = ownerAccountSeed, let identity = try? Identity.fromSeed(seed: seed) else { return nil }
        return identity.ed25519Seed()
    }

    /// The account identity of ``ownerAccountSeed``, or throws.
    public nonisolated static func ownerAccountIdentity() throws -> Identity {
        guard let seed = ownerAccountSeed else { throw NotBound() }
        return try Identity.fromSeed(seed: seed)
    }
}
