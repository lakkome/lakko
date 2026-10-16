// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// DeviceGroup lifecycle on iOS (Phase 2a3b3, ADR-0007).
///
/// Mirror of `apps/web/src/devices/lifecycle.ts`. One entry
/// point — [`ensureDeviceGroup`] — wraps the "load existing or
/// create-founder" decision plus the post-mutation snapshot
/// persistence so the per-screen call sites see a single
/// function.
///
/// The DeviceIdentity wasm/UniFFI handle this module builds
/// from the cached `deviceIdentitySeed` has its own in-memory
/// OpenMLS provider, independent of the AccountIdentity
/// provider that ``ChatRepository`` holds for conversation
/// groups (ADR-0007). Snapshot/restore goes through this
/// DeviceIdentity provider's storage only.
public enum DeviceGroupLifecycle {

    /// Refused: this device holds no account seed yet, so it cannot name its
    /// DeviceGroup (ADR-0078 section 6: the id is a function of the seed).
    public struct NoAccountSeed: Error {}

    /// ADR-0078 section 6 — the 64-byte seed the account's DeviceGroup id is
    /// derived from: the seed the conversation leaves are bound to, i.e. the
    /// OLDEST own predecessor after a proven rotation (a rotation keeps the
    /// DeviceGroup, so its id must not move), else `currentSeed` when given,
    /// else the device's stored account seed. `nil` when none is held.
    public static func lineageSeed(
        accountIdB64: String,
        currentSeed: Data? = nil,
        predecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore()
    ) -> Data? {
        let held = predecessors.load(accountIdB64: accountIdB64)
        if let oldest = held.first { return oldest }
        if let currentSeed { return currentSeed }
        return try? AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: accountIdB64)
    }

    /// The account's DeviceGroup journal id ([`lineageSeed`]); `nil` when
    /// this device cannot derive it yet.
    public static func groupId(
        accountIdB64: String,
        currentSeed: Data? = nil,
        predecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore()
    ) -> Data? {
        guard let seed = lineageSeed(
            accountIdB64: accountIdB64, currentSeed: currentSeed, predecessors: predecessors),
              let account = try? Identity.fromSeed(seed: seed) else { return nil }
        return deviceGroupIdFor(accountIdentity: account)
    }

    /// Result of [`ensureDeviceGroup`].
    public struct Ensured {
        /// The Identity built from the DeviceIdentity seed,
        /// populated with the (loaded or freshly-bootstrapped)
        /// MLS provider state. Caller holds this for the
        /// duration of the operation.
        public let identity: Identity
        /// The live DeviceGroup handle.
        public let group: DeviceGroup
        /// `true` when this call bootstrapped a fresh
        /// DeviceGroup (this device is now the founder).
        public let bootstrapped: Bool
    }

    /// The account session each DeviceGroup identity handle was made in.
    ///
    /// A handle carries the whole MLS provider, and a snapshot write
    /// persists all of it. One made in an earlier session of the account (a
    /// pairing, a removal or a sync pass that was still running at Log out)
    /// would, resumed after the same account signed in again, write that
    /// session's state over what the new session persisted. Its writes are
    /// entered with the session it was made in and are refused once that has
    /// ended (``AccountStoreGate/Session``). Weak keys: an entry goes with
    /// its handle.
    private static let handleSessions = NSMapTable<Identity, SessionBox>.weakToStrongObjects()
    private static let handleSessionsLock = NSLock()

    private final class SessionBox {
        let session: AccountStoreGate.Session
        init(_ session: AccountStoreGate.Session) { self.session = session }
    }

    /// Record that `identity` was made now, in the current session of
    /// `accountIdB64`. Called where a DeviceGroup identity handle is created.
    public static func noteHandle(_ identity: Identity, accountIdB64: String) {
        let box = SessionBox(AccountStoreGate.session(accountIdB64))
        handleSessionsLock.lock()
        handleSessions.setObject(box, forKey: identity)
        handleSessionsLock.unlock()
    }

    /// The session `identity` was made in; the current one for a handle
    /// nobody recorded.
    static func session(of identity: Identity, accountIdB64: String) -> AccountStoreGate.Session {
        handleSessionsLock.lock()
        defer { handleSessionsLock.unlock() }
        return handleSessions.object(forKey: identity)?.session
            ?? AccountStoreGate.session(accountIdB64)
    }

    /// Resolve the local DeviceGroup state: either restore
    /// the cached snapshot or bootstrap a fresh founder leaf.
    ///
    /// Idempotent on the snapshot side — calling twice with
    /// the same `(accountIdB64, deviceIdentitySeed)` returns
    /// the same group (the bootstrap branch fires only when
    /// the snapshot is absent).
    ///
    /// The DeviceGroup id is derived from the account seed ([`lineageSeed`],
    /// ADR-0078 section 6); `accountSeed` is the current one when the caller
    /// holds it (a key reset passes the NEW seed). Throws ``NoAccountSeed``
    /// when the device holds none.
    public static func ensureDeviceGroup(
        accountIdBytes: Data,
        accountIdB64: String,
        deviceIdentitySeed: Data,
        accountSeed: Data? = nil,
        snapshotStore: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) throws -> Ensured {
        guard let seed = lineageSeed(accountIdB64: accountIdB64, currentSeed: accountSeed) else {
            throw NoAccountSeed()
        }
        let lineage = try Identity.fromSeed(seed: seed)
        // Build the per-device Identity from the cached seed.
        // This is the DeviceIdentity (ADR-0007), NOT the
        // AccountIdentity ChatRepository holds.
        let identity = try Identity.fromSeed(seed: deviceIdentitySeed)
        noteHandle(identity, accountIdB64: accountIdB64)

        // Try to restore prior MLS state.
        if let snapshot = snapshotStore.getSnapshot(accountIdB64: accountIdB64) {
            do {
                try identity.restoreStorage(snapshot: snapshot)
                if let group = try loadDeviceGroup(
                    identity: identity,
                    accountId: accountIdBytes,
                    accountIdentity: lineage
                ) {
                    return Ensured(
                        identity: identity,
                        group: group,
                        bootstrapped: false
                    )
                }
                // Snapshot existed but didn't carry the
                // DeviceGroup row — fall through to
                // bootstrap. Defensive: shouldn't happen in
                // normal flows because every snapshot we
                // write was captured immediately after a
                // DeviceGroup mutation.
            } catch {
                NSLog("DeviceGroup snapshot restore failed: \(error); bootstrapping")
                // Fall through.
            }
        }

        // Bootstrap. Local device becomes the founding leaf.
        let group = try DeviceGroup.createFounder(
            identity: identity,
            accountId: accountIdBytes,
            accountIdentity: lineage
        )
        let fresh = try identity.snapshotStorage()
        snapshotStore.putSnapshot(
            accountIdB64: accountIdB64, blob: fresh,
            session: session(of: identity, accountIdB64: accountIdB64))
        // ADR-0057 obligation 7 — founded HERE: the next claim may reset the
        // journal if the same root claimed this deterministic id before (a
        // DeviceGroup lost with the local state and founded again).
        if let groupId = try? group.groupId() {
            JournalFoundingMarks.shared.mark(
                journalIdHex: DsClient.pseudonymHex(groupId), accountIdB64: accountIdB64)
        }
        return Ensured(identity: identity, group: group, bootstrapped: true)
    }

    /// Re-capture the MLS provider snapshot after a
    /// DeviceGroup mutation (add-device, remove-device,
    /// application-message send). Best-effort — a snapshot
    /// persist failure logs and continues.
    public static func persistSnapshot(
        identity: Identity,
        accountIdB64: String,
        snapshotStore: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) {
        do {
            let snapshot = try identity.snapshotStorage()
            snapshotStore.putSnapshot(
                accountIdB64: accountIdB64, blob: snapshot,
                session: session(of: identity, accountIdB64: accountIdB64))
        } catch {
            NSLog("DeviceGroup snapshot persist failed: \(error)")
        }
    }

    /// Persist the MLS provider snapshot BEFORE a DeviceGroup message built
    /// from it is published, and throw when it did not land: a disk or
    /// Keychain failure, the account's session ended
    /// (``AccountStoreGate/Sealed``), or `identity` is a handle of an earlier
    /// session. The caller publishes nothing then.
    public static func persistSnapshotOrThrow(
        identity: Identity,
        accountIdB64: String,
        snapshotStore: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) throws {
        let snapshot = try identity.snapshotStorage()
        try snapshotStore.putSnapshotDurable(
            accountIdB64: accountIdB64, blob: snapshot,
            session: session(of: identity, accountIdB64: accountIdB64))
    }
}
