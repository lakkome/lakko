// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Where an owned channel's vault head lives: read, written at a version,
/// claimed. The live one is ``LiveChannelHeadStore``; tests fake it.
@MainActor
protocol ChannelHeadStore {
    /// The head and its version, or `nil` when there is none.
    func read() async throws -> (version: UInt64, head: VaultConversationHead)?
    /// Write `head` at `version` (current + 1). Throws ``VaultClientError``.
    func write(version: UInt64, head: VaultConversationHead) async throws
    /// Claim the vault under its write key.
    func claim() async throws
    /// Delete the vault (ADR-0064 section 1: the client deletes its vaults).
    func delete() async throws
}

/// The vault of one owned channel on the storage service, under the OWNER
/// account's LINEAGE identity (``ChannelPublisherIdentity/ownerAccountIdentity()``:
/// the seed PublisherGroup create / load take and the journal root derives
/// from), not the current account identity, so the head stays usable across
/// an ADR-0074 key rotation.
@MainActor
struct LiveChannelHeadStore: ChannelHeadStore {
    let vault: VaultRef
    let keys: VaultKeys
    let client: VaultClient

    init?(channelIdHex: String, client: VaultClient = VaultClient(), lineage: Identity? = nil) {
        guard let channelBytes = ChannelsClient.hexToData(channelIdHex),
              let account = lineage ?? (try? ChannelPublisherIdentity.ownerAccountIdentity()) else { return nil }
        self.vault = ChannelVaultHead.vaultRef(channelId: channelBytes)
        self.keys = vaultKeys(accountIdentity: account)
        self.client = client
    }

    func delete() async throws {
        let hex = try idHex()
        let proof = try keys.signRequest(vault: vault, method: "DELETE", path: VaultClient.vaultPath(hex), body: Data())
        try await client.deleteVault(vaultIdHex: hex, proof: proof)
    }

    private func idHex() throws -> String { hexEncode(try keys.vaultId(vault: vault)) }

    func read() async throws -> (version: UInt64, head: VaultConversationHead)? {
        let hex = try idHex()
        let proof = try keys.signRequest(vault: vault, method: "GET", path: VaultClient.headPath(hex), body: Data())
        guard let object = try await client.getHead(vaultIdHex: hex, proof: proof) else { return nil }
        let plain = try keys.openHead(vault: vault, version: object.version, sealed: object.sealed)
        return (object.version, try decodeConversationHead(bytes: plain))
    }

    func write(version: UInt64, head: VaultConversationHead) async throws {
        let hex = try idHex()
        let sealed = try keys.sealHead(vault: vault, version: version, plaintext: try encodeConversationHead(head: head))
        let body = VaultClient.objectBody(version: version, sealed: sealed)
        let proof = try keys.signRequest(vault: vault, method: "PUT", path: VaultClient.headPath(hex), body: body)
        try await client.putHead(vaultIdHex: hex, body: body, proof: proof)
    }

    func claim() async throws {
        try await client.claim(vaultIdHex: try idHex(), writePublicKey: try keys.writePublicKey(vault: vault))
    }
}

/// ADR-0064 section 7, amendment of 2026-11-14 — an owned channel's vault
/// head: a `ConversationHead` whose `reentry` holds the two newest
/// PublisherGroup GroupInfos (each with its epoch's Delivery Service access
/// seed), no registrar seed. Every owner device that holds the group writes
/// it before it sends a PublisherGroup commit (the epoch the staged commit
/// moves to) and after it applied a commit another owner device made (the
/// current epoch); an owner device that holds no group enters from it
/// (``ChannelEntry``). A co-publisher's device never writes it.
///
/// A failed write never stops a commit: one attempt, with one read-merge-
/// write after a `409`, then the caller goes on.
@MainActor
enum ChannelVaultHead {

    // MARK: - Pure rules

    /// The epochs a head keeps.
    nonisolated static var keptEpochs: Int { max(1, min(2, Int(vaultMaxReentryEpochs()))) }

    /// The vault ref of a channel: the channel's conversation ref.
    nonisolated static func vaultRef(channelId: Data) -> VaultRef {
        .conversation(conversation: FolderConversationRef(kind: .channel, id: channelId))
    }

    /// `held` and `added` merged: one entry per epoch (`added` wins an
    /// epoch both hold), newest first, at most `keep`.
    nonisolated static func mergeReentry(
        _ held: [VaultReentryEpoch], _ added: [VaultReentryEpoch], keep: Int = 2
    ) -> [VaultReentryEpoch] {
        var byEpoch: [UInt64: VaultReentryEpoch] = [:]
        for entry in held { byEpoch[entry.epoch] = entry }
        for entry in added { byEpoch[entry.epoch] = entry }
        return Array(byEpoch.values.sorted { $0.epoch > $1.epoch }.prefix(keep))
    }

    /// Whether `head` already carries `entry` exactly (nothing to write).
    nonisolated static func carries(_ head: VaultConversationHead, _ entry: VaultReentryEpoch) -> Bool {
        head.reentry.contains(entry)
    }

    /// A channel's head with nothing but `reentry`.
    nonisolated static func head(
        channelId: Data, reentry: [VaultReentryEpoch], nowMs: UInt64
    ) -> VaultConversationHead {
        VaultConversationHead(
            conversation: FolderConversationRef(kind: .channel, id: channelId), kind: .unknown, title: "",
            people: [], membership: .in, sharedPins: [], personalPins: [], lastActivityMs: nowMs,
            chunks: [], tail: [], reentry: reentry, registrarSeed: nil)
    }

    /// The head rebuilt with `base`'s re-entry list and `entry` merged into
    /// it. Only `reentry` is carried over (every platform reads only it);
    /// the rest is the empty channel head.
    nonisolated static func merged(
        _ base: VaultConversationHead, _ entry: VaultReentryEpoch, nowMs: UInt64
    ) -> VaultConversationHead {
        head(channelId: base.conversation.id,
             reentry: mergeReentry(base.reentry, [entry], keep: keptEpochs), nowMs: nowMs)
    }

    // MARK: - The writer

    /// The head this device last wrote or read, by channel, with its version
    /// (in memory: a restart reads it back on the first `409`).
    private static var known: [String: (version: UInt64, head: VaultConversationHead)] = [:]

    /// The store of a channel's head (a seam for tests).
    static var storeFor: @MainActor (_ channelIdHex: String) -> ChannelHeadStore? = { channelIdHex in
        LiveChannelHeadStore(channelIdHex: channelIdHex)
    }

    /// Sign-out / account switch.
    static func reset() { known = [:] }

    private static func nowMs() -> UInt64 { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }

    /// Write `entry` into the channel's head: the head last known here (or
    /// an empty one) merged, at its version + 1; a `409` reads the head,
    /// merges and writes once more; a write to an unclaimed vault claims it
    /// and writes once more. Whether the head now carries `entry`. Never
    /// throws.
    @discardableResult
    ///
    /// `replace`: the head is rebuilt with `entry` alone, nothing merged
    /// (decision A's re-founded group starts at a low epoch, and keeping the
    /// two newest by number would keep the dead group's GroupInfo). The
    /// version rule is the same.
    static func write(
        channelIdHex: String, entry: VaultReentryEpoch, store: ChannelHeadStore, replace: Bool = false
    ) async -> Bool {
        let channelId = channelIdHex.lowercased()
        guard let channelBytes = ChannelsClient.hexToData(channelId) else { return false }
        let empty = head(channelId: channelBytes, reentry: [], nowMs: nowMs())
        let base = known[channelId] ?? (0, empty)
        if base.version > 0, replace ? base.head.reentry == [entry] : carries(base.head, entry) { return true }
        func next(_ held: VaultConversationHead) -> VaultConversationHead {
            merged(replace ? empty : held, entry, nowMs: nowMs())
        }
        var attempt = (version: base.version &+ 1, head: next(base.head))
        var claimed = false
        var reread = false
        while true {
            // An epoch older than both saved ones is dropped by the merge:
            // nothing of it would be written, so nothing is sent and the
            // write reports false.
            guard carries(attempt.head, entry) else {
                NSLog("ADR-0064: a channel head entry older than the saved ones was not written")
                return false
            }
            do {
                try await store.write(version: attempt.version, head: attempt.head)
                known[channelId] = attempt
                return true
            } catch VaultClientError.unclaimed where !claimed {
                claimed = true
                do {
                    try await store.claim()
                } catch {
                    known[channelId] = nil
                    NSLog("ADR-0064: a channel vault was not claimed; the head is not written")
                    return false
                }
            } catch VaultClientError.conflict where !reread {
                reread = true
                do {
                    if let current = try await store.read() {
                        known[channelId] = current
                        if replace ? current.head.reentry == [entry] : carries(current.head, entry) {
                            return true
                        }
                        attempt = (current.version &+ 1, next(current.head))
                    } else {
                        attempt = (1, next(empty))
                    }
                } catch {
                    known[channelId] = nil
                    NSLog("ADR-0064: a channel head was not read after a conflict; not written")
                    return false
                }
            } catch {
                known[channelId] = nil
                NSLog("ADR-0064: a channel head was not written (%@)", String(describing: type(of: error)))
                return false
            }
        }
    }

    /// Whether this device writes the head of `channelIdHex`: it holds the
    /// group as a device of the OWNER's account, and the channel is not one
    /// this account co-publishes.
    static func writes(channelIdHex: String, group: PublisherGroup, identity: Identity) -> Bool {
        CoPublishedJournals.shared.journalIdHex(channelIdHex: channelIdHex) == nil
            && (try? group.isOwnerDevice(identity: identity)) == true
    }

    /// Before a staged commit is sent: the epoch it moves to.
    @discardableResult
    static func writeStaged(channelIdHex: String, group: PublisherGroup, identity: Identity) async -> Bool {
        guard writes(channelIdHex: channelIdHex, group: group, identity: identity),
              let info = (try? group.stagedGroupInfo(identity: identity)) ?? nil,
              let seed = (try? group.stagedDsAccessSeed(identity: identity)) ?? nil,
              let epoch = try? group.epoch(),
              let store = storeFor(channelIdHex) else { return false }
        return await write(
            channelIdHex: channelIdHex,
            entry: VaultReentryEpoch(epoch: epoch &+ 1, dsAccessSeed: seed, groupInfo: info), store: store)
    }

    /// After a commit landed here (another owner device's, an entry, a
    /// founding): the current epoch.
    @discardableResult
    static func writeCurrent(
        channelIdHex: String, group: PublisherGroup, identity: Identity, replace: Bool = false
    ) async -> Bool {
        guard writes(channelIdHex: channelIdHex, group: group, identity: identity),
              (try? group.hasStagedCommit()) == false,
              let info = try? group.groupInfo(identity: identity),
              let seed = try? group.dsAccessSeed(identity: identity),
              let epoch = try? group.epoch(),
              let store = storeFor(channelIdHex) else { return false }
        return await write(
            channelIdHex: channelIdHex,
            entry: VaultReentryEpoch(epoch: epoch, dsAccessSeed: seed, groupInfo: info), store: store,
            replace: replace)
    }

    // MARK: - Deleting the vault

    /// Delete one owned channel's vault after the channel was deleted. Best
    /// effort: a failure is logged and changes nothing else. `currentSeed`
    /// notes the account's lineage seed first, the one the vault is named by.
    @discardableResult
    static func deleteVault(channelIdHex: String, currentSeed: Data?) async -> Bool {
        if let currentSeed { _ = try? ChannelPublisherIdentity.shared.account(currentSeed: currentSeed) }
        known[channelIdHex.lowercased()] = nil
        guard let store = storeFor(channelIdHex) else { return false }
        do {
            try await store.delete()
            return true
        } catch {
            NSLog("ADR-0064: a channel vault was not deleted (%@)", String(describing: type(of: error)))
            return false
        }
    }

    /// The channels the account owns: a live subscription register of role
    /// Owner, or a channel this device keeps as the account's own (its
    /// extras say owner). Sorted, each once.
    nonisolated static func ownedChannelIds(
        registers: [(channelId: Data, subscribed: Bool, owner: Bool)], extras: [String: ChannelExtras]
    ) -> [String] {
        var ids = Set(registers.filter { $0.subscribed && $0.owner }.map { hexEncode($0.channelId).lowercased() })
        for (id, value) in extras where value.accountRole == ChannelAccountRole.owner { ids.insert(id.lowercased()) }
        return ids.sorted()
    }

    /// The wait an account deletion gives the vault deletes, all of them
    /// together. Chosen, not measured.
    nonisolated static let accountDeletionBudgetSeconds: Double = 10

    /// Account deletion: delete the vault of every channel the account owns,
    /// before the account goes. Best effort and bounded: returns after
    /// ``accountDeletionBudgetSeconds`` at the latest, whatever the storage
    /// service does. The number of vaults deleted.
    @discardableResult
    static func deleteOwnedVaults(
        accountIdB64: String, currentSeed: Data?, budgetSeconds: Double = accountDeletionBudgetSeconds,
        registers: [(channelId: Data, subscribed: Bool, owner: Bool)]? = nil
    ) async -> Int {
        let held = registers ?? (ChannelDeviceAdmission.seedRegister?.channelSubscriptions ?? []).map {
            (channelId: $0.channelId, subscribed: $0.subscribed, owner: $0.role == .owner)
        }
        let ids = ownedChannelIds(registers: held, extras: ChannelExtrasStore.load(accountIdB64))
        guard !ids.isEmpty else { return 0 }
        let deleted = DeletedCount()
        await bounded(seconds: budgetSeconds) {
            for id in ids {
                if Task.isCancelled { return }
                if await deleteVault(channelIdHex: id, currentSeed: currentSeed) { deleted.add() }
            }
        }
        if deleted.value < ids.count {
            NSLog("ADR-0064: %d of %d channel vaults deleted before the account deletion", deleted.value, ids.count)
        }
        return deleted.value
    }

    /// Run `work`, returning when it finishes or after `seconds`, whichever
    /// comes first (the work is then cancelled and left to end on its own).
    static func bounded(seconds: Double, _ work: @escaping @MainActor () async -> Void) async {
        let once = ResumeOnce()
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            let worker = Task { @MainActor in
                await work()
                if once.take() { continuation.resume() }
            }
            Task {
                try? await Task.sleep(nanoseconds: UInt64(max(0, seconds) * 1_000_000_000))
                if once.take() {
                    worker.cancel()
                    continuation.resume()
                }
            }
        }
    }

    /// The saved re-entry epochs of a channel, newest first; `nil` when the
    /// head could not be read (try again later), `[]` when there is none.
    static func readReentry(channelIdHex: String) async -> [VaultReentryEpoch]? {
        guard let store = storeFor(channelIdHex) else { return nil }
        do {
            guard let current = try await store.read() else { return [] }
            known[channelIdHex.lowercased()] = current
            return current.head.reentry.sorted { $0.epoch > $1.epoch }
        } catch {
            NSLog("ADR-0064: a channel head was not read (%@)", String(describing: type(of: error)))
            return nil
        }
    }
}

/// A flag that is taken once, across tasks.
private final class ResumeOnce: @unchecked Sendable {
    private let lock = NSLock()
    private var taken = false

    func take() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !taken else { return false }
        taken = true
        return true
    }
}

/// A counter shared with a bounded task.
private final class DeletedCount: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0

    var value: Int { lock.lock(); defer { lock.unlock() }; return count }

    func add() { lock.lock(); count += 1; lock.unlock() }
}
