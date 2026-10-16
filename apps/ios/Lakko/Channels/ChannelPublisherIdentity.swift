// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The ONE long-lived MLS identity the channel publisher paths run on, with
/// its OpenMLS provider snapshotted to disk and restored on first use.
///
/// Swift mirror of Android's `ChannelPublisherIdentity` and of the web
/// channel provider snapshot (`apps/web/src/state/channels-state.ts`).
///
/// ## Why this exists
///
/// A channel's PublisherGroup (ADR-0010 Tier 1) lives in the in-memory OpenMLS
/// provider of whichever `Identity` it was created in. Every iOS channel
/// screen used to build a throwaway `Identity.fromSeed` and never snapshot
/// it, so the group lived exactly as long as that object: the next screen
/// founded a FRESH group (a different content seed, a different epoch), a
/// co-publisher the owner approved was in a group the owner no longer had,
/// and a rotation resealed a seed no co-publisher could derive. Only the
/// content seed survived, in the Keychain (``ChannelPublisherKeyStore``).
///
/// ## Discipline
///
///   * The identity is derived from the account's LINEAGE seed (the oldest
///     own predecessor after an ADR-0074 rotation, else the current seed,
///     ``DeviceGroupLifecycle/lineageSeed(accountIdB64:currentSeed:predecessors:)``),
///     the same identity web (`conversationAccountIdentity`) and Android
///     run their channel provider on. ADR-0078 section 6 derives a
///     PublisherGroup id from this identity's seed, so it must not change
///     with a rotation. Owner and co-publisher REQUESTS to the channels
///     service are signed by the current seed's identity, which the
///     rotation handed the channels to; callers build that one themselves.
///   * Everything runs on the main actor, so a snapshot never captures a
///     provider half-way through one FFI call.
///   * ``persistOrThrow()`` is the fail-closed write the G-50 outbox needs
///     before a PublisherGroup commit is published; ``persist()`` is the
///     best-effort write after any other mutation (a knock processed, a
///     rejection, a commit applied).
///   * The snapshot is a separate record (`<account>.channels`) in
///     ``IdentitySnapshotStore``, never the chat provider's, so neither
///     snapshot can roll the other's groups back.
@MainActor
public final class ChannelPublisherIdentity {

    /// The process-wide instance.
    public static let shared = ChannelPublisherIdentity()

    /// The record suffix of the channel provider snapshot.
    public nonisolated static let snapshotScope = ".channels"

    /// Thrown when no account is bound, or the account's seed is unknown.
    public struct NotBound: Error {}

    private let store: IdentitySnapshotStore
    private let boundAccount: () -> String?
    private var identity: Identity?
    private var boundAccountIdB64: String?
    private var boundSeed: Data?

    init(
        store: IdentitySnapshotStore = IdentitySnapshotStore(scope: ChannelPublisherIdentity.snapshotScope),
        boundAccount: @escaping () -> String? = { AcceptedContactsStore.boundAccountIdB64 }
    ) {
        self.store = store
        self.boundAccount = boundAccount
    }

    /// The account the identity is bound to, or `nil`.
    public var boundAccountId: String? { boundAccountIdB64 }

    /// The channel identity of the bound account. `currentSeed` is the
    /// account's current phrase seed (what the screens hold); the identity
    /// is built from the lineage seed and restored from the snapshot on the
    /// first call of the account's session. Idempotent for the same account
    /// and lineage seed; another account replaces the identity.
    public func identity(currentSeed: Data) throws -> Identity {
        guard let account = boundAccount() else { throw NotBound() }
        let lineage = DeviceGroupLifecycle.lineageSeed(
            accountIdB64: account, currentSeed: currentSeed) ?? currentSeed
        if let identity, boundAccountIdB64 == account, boundSeed == lineage {
            return identity
        }
        let fresh = try Identity.fromSeed(seed: lineage)
        if let snapshot = store.getSnapshot(accountIdB64: account), !snapshot.isEmpty {
            do {
                try fresh.restoreStorage(snapshot: snapshot)
            } catch {
                // An unreadable snapshot reads as "none": the publisher groups
                // then re-found on the next open (the behaviour before this
                // provider was durable) rather than the whole Channels screen
                // failing.
                NSLog("Channels: the channel provider snapshot did not restore; starting empty")
            }
        }
        identity = fresh
        boundAccountIdB64 = account
        boundSeed = lineage
        return fresh
    }

    /// Snapshot the provider now, best effort. A no-op when nothing is
    /// bound or another account has been bound since.
    public func persist() {
        do {
            try persistOrThrow()
        } catch {
            NSLog("Channels: the channel provider snapshot was not persisted")
        }
    }

    /// Snapshot the provider now, throwing when it was not written: no
    /// identity bound, the bound account is no longer the session's, an
    /// empty snapshot, or a refused or failed store write. Called before a
    /// PublisherGroup commit is published (G-50), so nothing goes out with
    /// nothing on disk behind it.
    public func persistOrThrow() throws {
        guard let identity, let account = boundAccountIdB64,
              account == boundAccount() else { throw NotBound() }
        let bytes = try identity.snapshotStorage()
        guard !bytes.isEmpty else { throw NotBound() }
        try store.putSnapshotDurable(accountIdB64: account, blob: bytes)
    }

    /// Drop the in-memory identity; the snapshot on disk stays. Log out.
    public func unbind() {
        identity = nil
        boundAccountIdB64 = nil
        boundSeed = nil
    }

    /// Forget device: delete `accountIdB64`'s channel provider snapshot.
    public nonisolated static func deleteOnDeviceSnapshot(accountIdB64: String) {
        IdentitySnapshotStore(scope: snapshotScope).deleteSnapshot(accountIdB64: accountIdB64)
    }
}
