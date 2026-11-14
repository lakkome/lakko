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
        let lineage = Self.ownerSeed(
            lineage: DeviceGroupLifecycle.lineageSeed(accountIdB64: accountId, currentSeed: currentSeed)
                ?? currentSeed,
            currentSeed: currentSeed,
            boundAccountKey: deviceIdentityProvider()?.accountPublicKey())
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

    /// The account identity of ``ownerAccountSeed``, or throws.
    public nonisolated static func ownerAccountIdentity() throws -> Identity {
        guard let seed = ownerAccountSeed else { throw NotBound() }
        return try Identity.fromSeed(seed: seed)
    }
}
