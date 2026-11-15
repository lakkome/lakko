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
    ///
    /// G-56 (ADR-0074 "Joining after a rotation", contract 2.4) — when this
    /// device recorded that its account's chats use a conversation key `C`
    /// and no seed it holds derives `C`, the answer is `nil`: the fallback to
    /// the current seed would name another journal than the siblings', and
    /// every caller treats `nil` as "do not open, found or knock". With no
    /// record (never read, or no link) the fallback is unchanged.
    /// `conversationKeys` `nil` skips the guard (the local wipe, which must
    /// name what it deletes).
    public static func lineageSeed(
        accountIdB64: String,
        currentSeed: Data? = nil,
        predecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore(),
        conversationKeys: OwnConversationKeyStoring? = KeychainOwnConversationKeyStore()
    ) -> Data? {
        let held = predecessors.load(accountIdB64: accountIdB64)
        let current = currentSeed
            ?? ((try? AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: accountIdB64)) ?? nil)
        let lineage = held.first ?? current
        guard let conversationKey = conversationKeys?.load(accountIdB64: accountIdB64)?.conversationKey else {
            return lineage
        }
        let holdsIt = (held + [current].compactMap { $0 }).contains {
            (try? Identity.fromSeed(seed: $0).signaturePublicKey()) == conversationKey
        }
        guard holdsIt else {
            NSLog("[g56] the DeviceGroup lineage key is not on this device; no DeviceGroup is opened")
            return nil
        }
        return lineage
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
        /// ADR-0081 decision 3 — the snapshot generation this handle was
        /// read at (or written at, for a founding). A later write of this
        /// handle names it and is refused once another handle wrote.
        public let generation: Int
    }

    /// ADR-0081 decision 4 — this device holds no usable DeviceGroup state
    /// and the call was not allowed to found one.
    public struct NotJoined: Error, Equatable {
        public enum Reason: Equatable {
            /// The caller passes no founding decision (a one-shot path:
            /// pairing, removal, the policy, the device list): nothing is
            /// founded there.
            case noState
        }
        public let reason: Reason
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

    /// Resolve the local DeviceGroup state: restore the cached snapshot, or —
    /// only when `found` is `true` — found a fresh DeviceGroup with this
    /// device as its first leaf.
    ///
    /// ADR-0081 decision 4: founding is reserved for a key reset and for an
    /// account whose directory lists no other own device; the caller decides
    /// that (``DeviceGroupSyncRepository`` asks the directory) and passes
    /// `found`. Every other caller passes nothing and gets ``NotJoined`` when
    /// this device holds no usable state: before ADR-0081 such a call
    /// founded a new DeviceGroup, and the next claim reset the journal,
    /// cutting off every live sibling. A snapshot that does not decrypt or
    /// does not restore is DELETED first (it can never become usable), so
    /// the decision is taken on "no state".
    ///
    /// The DeviceGroup id is derived from the account seed ([`lineageSeed`],
    /// ADR-0078 section 6); `accountSeed` is the current one when the caller
    /// holds it (a key reset passes the NEW seed). Throws ``NoAccountSeed``
    /// when the device holds none.
    ///
    /// The founding's snapshot write FAILS CLOSED (decision 3).
    public static func ensureDeviceGroup(
        accountIdBytes: Data,
        accountIdB64: String,
        deviceIdentitySeed: Data,
        accountSeed: Data? = nil,
        found: Bool = false,
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

        // Try to restore prior MLS state. The generation is read BEFORE the
        // snapshot, so a write that lands in between is caught by the next
        // conditional write of this handle rather than overwritten by it.
        let generation = snapshotStore.generation(accountIdB64: accountIdB64)
        if let snapshot = snapshotStore.getSnapshot(accountIdB64: accountIdB64) {
            var loaded: DeviceGroup?
            do {
                try identity.restoreStorage(snapshot: snapshot)
                loaded = try loadDeviceGroup(
                    identity: identity,
                    accountId: accountIdBytes,
                    accountIdentity: lineage
                )
            } catch {
                NSLog("DeviceGroup snapshot restore failed: %@", String(describing: error))
            }
            if let loaded {
                return Ensured(
                    identity: identity, group: loaded, bootstrapped: false, generation: generation)
            }
            // A snapshot that does not restore, or carries no DeviceGroup,
            // can never become usable: dropped before the decision.
            NSLog("DeviceGroup snapshot unusable; deleted")
            snapshotStore.dropUnusableSnapshot(accountIdB64: accountIdB64)
        } else {
            // No snapshot, or one that does not decrypt (`getSnapshot` reads
            // both as nil): an undecryptable file is dropped, the pending
            // rows (a rejoin's knock) stay.
            snapshotStore.dropUnusableSnapshot(accountIdB64: accountIdB64)
        }
        guard found else {
            throw NotJoined(reason: .noState)
        }
        return try foundDeviceGroup(
            identity: identity, lineage: lineage, accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64, snapshotStore: snapshotStore)
    }

    /// Found a fresh DeviceGroup with `identity` as its first leaf: the
    /// pending rows of a previous DeviceGroup are dropped (nothing of it can
    /// be finished), the snapshot is written FAIL-CLOSED, and the founding is
    /// marked so the next claim may reset (ADR-0057 obligation 7).
    private static func foundDeviceGroup(
        identity: Identity,
        lineage: Identity,
        accountIdBytes: Data,
        accountIdB64: String,
        snapshotStore: DeviceGroupSnapshotStore
    ) throws -> Ensured {
        let group = try DeviceGroup.createFounder(
            identity: identity,
            accountId: accountIdBytes,
            accountIdentity: lineage
        )
        for kind in DeviceGroupSnapshotStore.PendingKind.allCases {
            snapshotStore.deletePending(accountIdB64: accountIdB64, kind: kind)
        }
        let fresh = try identity.snapshotStorage()
        let written = try snapshotStore.putSnapshotDurable(
            accountIdB64: accountIdB64, blob: fresh,
            session: session(of: identity, accountIdB64: accountIdB64))
        if let groupId = try? group.groupId() {
            JournalFoundingMarks.shared.mark(
                journalIdHex: DsClient.pseudonymHex(groupId), accountIdB64: accountIdB64)
        }
        return Ensured(identity: identity, group: group, bootstrapped: true, generation: written)
    }

    /// A DeviceGroup this device JOINED from a Welcome is another device's
    /// chain: a founding mark left from an earlier founding here (whose claim
    /// never landed) must not make claim-on-open reset it
    /// (``DeviceGroupDsAccess/claim(group:identity:accountIdB64:ds:marks:)``).
    public static func clearFoundingMark(
        joined group: DeviceGroup, marks: JournalFoundingMarks = .shared
    ) {
        guard let groupId = try? group.groupId() else { return }
        marks.clear(journalIdHex: DsClient.pseudonymHex(groupId))
    }

    /// Persist the MLS provider snapshot of `identity` and THROW when it did
    /// not land (ADR-0081 decision 3: every DeviceGroup snapshot write fails
    /// closed): a disk or Keychain failure, the account's session ended
    /// (``AccountStoreGate/Sealed``), `identity` is a handle of an earlier
    /// session, or — with `expectedGeneration` — another handle wrote since
    /// (``DeviceGroupSnapshotStore/SnapshotChanged``). Returns the generation
    /// the write made.
    @discardableResult
    public static func persistSnapshotOrThrow(
        identity: Identity,
        accountIdB64: String,
        expectedGeneration: Int? = nil,
        snapshotStore: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) throws -> Int {
        let snapshot = try identity.snapshotStorage()
        return try snapshotStore.putSnapshotDurable(
            accountIdB64: accountIdB64, blob: snapshot,
            session: session(of: identity, accountIdB64: accountIdB64),
            expectedGeneration: expectedGeneration)
    }
}
