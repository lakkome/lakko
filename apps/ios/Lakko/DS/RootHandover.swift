// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0057 "What the S2 client work found", item 1 — an ownership transfer
// moves the journal root.
//
// ADR-0045 ownership transfer rewrites the role table, not the journal: the
// Delivery Service still knows the OLD owner's account-derived root, and the
// old owner cannot name the new owner's root (that key is derived from the new
// owner's account seed). So the old owner hands its root seed over:
//
//   * Sending (outgoing owner). When a device of this account applies a remote
//     commit or merges its own sweep, and the role table's owner was this
//     account BEFORE and another account AFTER, the device records the
//     handover as owed (group ids only, survives a restart) and seals
//     `dsJournalRootSeed(ownAccountSeed, gid)` to the new owner's account
//     X25519 key (`composeRootHandoverEnvelope`, the registrar-seed envelope at
//     rank root), posted in the group. It stops acting as root at the same
//     moment without anything here doing so: the repository resolves the
//     journal authority off the LIVE role table, so no appointment, root
//     registration, claim or reset is signed with the root afterwards. Every
//     device of the outgoing account may send it. The owed record is kept
//     until this account's own probe (a `root` statement naming its own root,
//     signed by it) answers 401, i.e. the DS no longer knows it as the root;
//     while the probe answers 204 the envelope is re-sent, at most every
//     ``RootHandover/resendInterval`` (second security pass, finding 4: a
//     one-shot send lost left the old root in place for good).
//   * Receiving (new owner). A rank-root envelope is kept only when it names
//     this journal, is addressed to this account and the live role table AT
//     RECEIPT names this account owner (C-68). No sender check: the DS is the
//     check, because a seed that is not the current root fails the `root`
//     statement with 401. The opened seed signs `root` naming this account's
//     own derived root; after a 204 the new root sends `registrars-set` with
//     the whole set the roster calls for (every old registrar not listed goes
//     with it) and the normal reconcile re-sends every member's seed. Until
//     the 204 (or the probe) confirms it, this account keeps signing with its
//     registrar seed (the "root held" marker). The opened seed is kept sealed
//     under the device key until the takeover lands or answers 401, so a
//     network failure or a restart retries it; it never enters the
//     registrar-seed rows and is never logged.
//
// Mirror of `apps/web/src/ds/root-handover.ts` and Android `RootHandover.kt`.
// Everything here takes its signers, the core codec and the network through
// closures, so the host tests drive every rule with fakes.

public enum RootHandover {

    // MARK: - Detecting an owner change at commit apply / sweep merge

    /// The pure sending rule: the new owner when the role table's owner was
    /// `own` `before` one commit apply / sweep merge and is ANOTHER account
    /// `after` it, else `nil` (a no-op commit, this account staying owner,
    /// becoming owner, a table-less group or an unreadable table).
    public static func ownerMovedAwayFrom(own: Data?, before: Data?, after: Data?) -> Data? {
        guard let own, let before, let after else { return nil }
        guard before == own, after != own else { return nil }
        return after
    }

    // MARK: - Sending (outgoing owner)

    /// What one handover attempt did.
    public enum SendOutcome: Equatable {
        /// Posted in the group.
        case sent
        /// Nothing to hand over any more: this account is owner again, the
        /// group has no owner, or the session ended.
        case cancelled
        /// The new owner's X25519 key is not resolvable yet: retried later.
        case pending
        /// Composing or sending failed: retried later.
        case failed

        /// `true` when the outcome ends the obligation (the owed record is
        /// cleared).
        public var settlesObligation: Bool { self == .sent || self == .cancelled }
    }

    /// One handover attempt. The recipient is the owner the LIVE role table
    /// names now (not the one recorded when the change was seen), so a second
    /// transfer in between hands the root to whoever holds the group. Never
    /// sent while this account is owner. The seed `rootSeed` returns is wiped
    /// here on every path.
    ///
    /// - Parameters:
    ///   - liveOwner: the role table's owner as it stands now, `nil` for none
    ///     or an unreadable table.
    ///   - ownerX25519: the new owner's account X25519 key from its identity
    ///     bundle (verified to belong to that account key), or `nil` when it
    ///     does not resolve yet.
    ///   - compose: `composeRootHandoverEnvelope(gid, newOwner, x25519, seed)`.
    ///   - sendEnvelope: seal and publish into the group (an MLS application
    ///     message, under the group's MLS lock).
    @MainActor
    public static func send(
        ownAccountKey: () -> Data?,
        liveOwner: () -> Data?,
        ownerX25519: (Data) async -> Data?,
        rootSeed: () throws -> Data,
        compose: (_ newOwner: Data, _ x25519: Data, _ rootSeed: Data) throws -> Data,
        sendEnvelope: (Data) async throws -> Void
    ) async -> SendOutcome {
        guard let own = ownAccountKey() else { return .cancelled }
        guard let owner = liveOwner(), owner != own else { return .cancelled }
        guard let x25519 = await ownerX25519(owner) else { return .pending }
        // Re-read after the lookup: the table may have moved while the bundle
        // was fetched.
        guard let ownerNow = liveOwner(), ownerNow != own else { return .cancelled }
        guard ownerNow == owner else { return .pending }
        let envelope: Data
        do {
            var seed = try rootSeed()
            defer { wipe(&seed) }
            envelope = try compose(owner, x25519, seed)
        } catch {
            return .failed
        }
        do {
            try await sendEnvelope(envelope)
            return .sent
        } catch {
            return .failed
        }
    }

    // MARK: - The owed handover (outgoing owner, finding 4)

    /// The outgoing owner re-sends an owed handover at most this often while
    /// its probe says it is still the root. Chosen, not measured.
    public static let resendInterval: TimeInterval = 10 * 60

    /// What one step of an owed handover did.
    public enum OwedOutcome: Equatable {
        /// The probe answered 401: the root moved; the owed record goes.
        case done
        /// Sent this round; kept until the probe answers 401.
        case sent
        /// Still the root, but sent less than ``resendInterval`` ago.
        case throttled
        /// Nothing to hand over (this account is owner again, or the group has
        /// no owner): the owed record goes.
        case cancelled
        /// The probe or the send could not run: retried later.
        case retry

        /// `true` when the owed record is cleared.
        public var settlesObligation: Bool { self == .done || self == .cancelled }
    }

    /// One step of an owed handover: probe with this account's own root;
    /// while it is still the root, (re-)send the envelope unless it went out
    /// less than ``resendInterval`` ago. Never throws.
    @MainActor
    public static func runOwed(
        probe: () async throws -> JournalOwnership.RootProbe,
        lastSentAt: Date?,
        now: Date,
        send: () async -> SendOutcome
    ) async -> OwedOutcome {
        let answer: JournalOwnership.RootProbe
        do {
            answer = try await probe()
        } catch {
            NSLog("ADR-0057: root probe failed; the handover is retried later")
            return .retry
        }
        if answer == .notRoot {
            NSLog("ADR-0057: root probe answered 401; the handover landed")
            return .done
        }
        if let lastSentAt, now.timeIntervalSince(lastSentAt) < resendInterval { return .throttled }
        switch await send() {
        case .sent: return .sent
        case .cancelled: return .cancelled
        case .pending, .failed: return .retry
        }
    }

    // MARK: - Receiving (new owner)

    /// Why a rank-root envelope was dropped, or `accept`.
    public enum Verdict: Equatable {
        case accept
        /// The envelope names another journal.
        case wrongJournal
        /// Addressed to another account.
        case notAddressedToUs
        /// The live role table does not name this account owner.
        case notOwner
        /// This account's key could not be read: fail closed.
        case unresolved
    }

    /// The receive gate for a rank-root envelope. `owner` is the live role
    /// table's owner read at receipt. No sender check: the DS refuses a seed
    /// that is not the current root.
    public static func verdict(
        ownAccountKey: Data?,
        envelopeJournal: Data,
        envelopeAddressee: Data,
        groupId: Data,
        owner: Data?
    ) -> Verdict {
        guard let ownAccountKey else { return .unresolved }
        guard envelopeJournal == groupId else { return .wrongJournal }
        guard envelopeAddressee == ownAccountKey else { return .notAddressedToUs }
        guard let owner, owner == ownAccountKey else { return .notOwner }
        return .accept
    }

    /// What ``receiveSeedEnvelope(rank:envelopeJournal:envelopeAddressee:groupId:ownAccountKey:senderAccountKey:owner:open:storeRegistrarSeed:takeOver:onDrop:)``
    /// did with one envelope.
    public enum SeedEnvelopeOutcome: Equatable {
        /// A registrar seed, kept in the registrar-seed store.
        case registrarStored
        /// A root handover that passed its gate: the takeover was started.
        case handoverStarted
        /// Dropped (a gate refused it, or it did not open).
        case dropped
    }

    /// The one dispatch for a received registrar-seed envelope, so the rule
    /// that a ROOT-rank envelope never enters the registrar-seed store is
    /// structural: the root branch has no path to `storeRegistrarSeed`.
    ///
    /// - Parameters:
    ///   - open: opens the envelope with the ACCOUNT identity; throws when it
    ///     does not open.
    ///   - storeRegistrarSeed: keeps a member / admin registrar seed.
    ///   - takeOver: starts the new owner's takeover with the opened OLD root
    ///     seed, detached; it owns the seed and wipes it.
    ///   - onDrop: told why an envelope was dropped (logging; never the seed).
    public static func receiveSeedEnvelope(
        rank: DsSeedEnvelopeRank,
        envelopeJournal: Data,
        envelopeAddressee: Data,
        groupId: Data,
        ownAccountKey: Data?,
        senderAccountKey: Data?,
        owner: Data?,
        open: () throws -> Data,
        storeRegistrarSeed: (Data, DsRegistrarRank) -> Void,
        takeOver: (Data) -> Void,
        onDrop: (String) -> Void = { _ in }
    ) -> SeedEnvelopeOutcome {
        let registrarRank: DsRegistrarRank
        switch rank {
        case .root:
            let gate = verdict(
                ownAccountKey: ownAccountKey, envelopeJournal: envelopeJournal,
                envelopeAddressee: envelopeAddressee, groupId: groupId, owner: owner)
            guard gate == .accept else {
                if gate != .notAddressedToUs { onDrop("root handover refused: \(gate)") }
                return .dropped
            }
            guard var seed = try? open() else {
                onDrop("root handover did not open")
                return .dropped
            }
            guard seed.count == registrarSeedLength else {
                wipe(&seed)
                onDrop("root handover seed has the wrong width")
                return .dropped
            }
            takeOver(seed)
            wipe(&seed)
            return .handoverStarted
        case .member:
            registrarRank = .member
        case .admin:
            registrarRank = .admin
        }
        guard let ownAccountKey else { return .dropped }
        let gate = JournalOwnership.seedVerdict(
            envelopeJournal: envelopeJournal,
            envelopeAddressee: envelopeAddressee,
            groupId: groupId,
            ownAccountKey: ownAccountKey,
            senderAccountKey: senderAccountKey,
            owner: owner)
        guard gate == .accept else {
            if gate != .notAddressedToUs { onDrop("registrar seed refused: \(gate)") }
            return .dropped
        }
        guard var seed = try? open() else {
            onDrop("registrar seed did not open")
            return .dropped
        }
        defer { wipe(&seed) }
        storeRegistrarSeed(seed, registrarRank)
        return .registrarStored
    }

    /// The width of a registrar / root seed.
    public static let registrarSeedLength = 32

    // MARK: - The new owner's takeover

    /// What the new owner's takeover did.
    public enum TakeOverOutcome: Equatable {
        /// The root moved to this account's root (204); the registrar set was
        /// reset (or its reset given up on); the reconcile was scheduled.
        case tookOver
        /// 401, and this account's own probe says it holds the root: another
        /// device of this account took over already. Marker set, seed dropped.
        case heldElsewhere
        /// 401: the seed is not the current root (the envelope is stale or
        /// forged). Dropped, never retried with it.
        case stale
        /// The role table no longer names this account owner.
        case notOwner
        /// The root statement kept failing (transiently, or with another 4xx).
        /// The caller keeps the seed and retries later.
        case failed

        /// `true` when the kept seed is wiped: everything but `failed`.
        public var settlesSeed: Bool { self != .failed }
    }

    /// Delays between takeover attempts on a transient failure (network,
    /// 5xx): two retries. Chosen, not measured. Mirrors web and Android.
    public static let retryDelaysMs: [UInt64] = [5_000, 30_000]

    /// A takeover failure worth retrying: anything but a DS answer below 500
    /// (a network error, a timeout, a 5xx).
    public static func isTransient(_ error: Error) -> Bool {
        guard let refusal = error as? DsStatementError else { return true }
        if case .http(let status, _) = refusal { return status >= 500 }
        return false
    }

    private struct NotOwnerAnyMore: Error {}

    private static func withTransientRetry(
        delaysMs: [UInt64],
        sleep: (UInt64) async -> Void,
        _ run: () async throws -> Void
    ) async throws {
        var attempt = 0
        while true {
            do {
                try await run()
                return
            } catch let error as NotOwnerAnyMore {
                throw error
            } catch {
                guard attempt < delaysMs.count, isTransient(error) else { throw error }
                await sleep(delaysMs[attempt])
                attempt += 1
            }
        }
    }

    /// The new owner's takeover with the handed-over OLD root seed: `root`
    /// signed by the old root naming this account's derived root; after a 204
    /// the "root held" marker is set and the NEW root sends `registrars-set`
    /// with the whole set the roster calls for; then the reconcile re-sends
    /// every member's seed. Never throws.
    ///
    /// The caller owns `oldRootSeed` and wipes it after this returns (a Swift
    /// `Data` is a value: a wipe here would clear only this copy).
    ///
    /// - Parameters:
    ///   - ownRootPublicKey: `dsJournalRootSigner(ownAccountIdentity, gid).publicKey`.
    ///   - stillOwner: the live role table still names this account owner;
    ///     checked immediately before every `root` PUT attempt.
    ///   - oldRootTransfer: `dsStatementSigner(oldRootSeed).rootTransfer(gid, newRoot)`.
    ///   - probeOwnRoot: the probe with this account's own root, run after a
    ///     401 (another device of this account may have taken over already).
    ///   - markRootHeld: set this account's "root held" marker.
    ///   - newRoot: this account's root signer (the NEW root), or `nil`.
    ///   - desiredRegistrarEntries: the whole registrar set the live roster
    ///     calls for, as derived under the NEW root, or `nil` when unreadable.
    ///   - resetAppointments: forget which seeds were delivered (every seed
    ///     changes with the root) and drop this account's own old registrar
    ///     seed for the group.
    ///   - reconcile: schedule the normal reconcile under the new root.
    @MainActor
    public static func takeOver(
        journalId: Data,
        oldRootSeed: Data,
        ownRootPublicKey: () -> Data?,
        stillOwner: () -> Bool,
        oldRootTransfer: (_ oldRootSeed: Data, _ newRoot: Data) throws -> DsStatementRequest,
        probeOwnRoot: () async throws -> JournalOwnership.RootProbe,
        markRootHeld: () -> Void,
        newRoot: () -> DsStatementSignerProtocol?,
        desiredRegistrarEntries: () -> [DsRegistrarEntry]?,
        ds: DsStatementSending,
        resetAppointments: () -> Void,
        reconcile: () -> Void,
        retryDelaysMs: [UInt64] = RootHandover.retryDelaysMs,
        sleep: (UInt64) async -> Void = { try? await Task.sleep(nanoseconds: $0 * 1_000_000) },
        log: (String) -> Void = { _ in }
    ) async -> TakeOverOutcome {
        guard let newRootKey = ownRootPublicKey() else { return .failed }

        // 1. The root statement, signed by the OLD root, naming this account's.
        do {
            try await withTransientRetry(delaysMs: retryDelaysMs, sleep: sleep) {
                guard stillOwner() else { throw NotOwnerAnyMore() }
                try await ds.putStatement(try oldRootTransfer(oldRootSeed, newRootKey))
            }
        } catch is NotOwnerAnyMore {
            return .notOwner
        } catch DsStatementError.unauthorized {
            if (try? await probeOwnRoot()) == .root {
                markRootHeld()
                log("root handover already applied by another device of this account")
                reconcile()
                return .heldElsewhere
            }
            log("root handover not applied (401): the seed is not the current root; dropped")
            return .stale
        } catch {
            log("root handover failed")
            return .failed
        }

        // The root is this account's from here: sign with it from now on.
        markRootHeld()

        // 2. One `registrars-set` under the new root: the whole set the roster
        //    calls for; every old root's registrar not listed goes with it.
        if let root = newRoot(), let entries = desiredRegistrarEntries() {
            do {
                try await withTransientRetry(delaysMs: retryDelaysMs, sleep: sleep) {
                    try await ds.putStatement(try root.registrarsSet(journalId: journalId, entries: entries))
                }
            } catch {
                // The root moved; the reconcile below sends the set again.
                log("registrar set not reset after root handover")
            }
        }

        // 3. Re-send every member's seed under the new root.
        resetAppointments()
        reconcile()
        return .tookOver
    }

    /// Overwrite a secret in place.
    static func wipe(_ bytes: inout Data) {
        bytes.resetBytes(in: 0..<bytes.count)
    }
}
