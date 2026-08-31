// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Durable pending-commit outbox (ADR-0040, G-35). iOS mirror of the web
/// reference `apps/web/src/chat/commit-outbox.ts` + `mls-store.ts`.
///
/// ## Why this exists
///
/// The four group-membership commit paths (add / remove / role-change /
/// external-join-approve) each run: core create-commit (which eagerly merges
/// epoch N -> N+1 in the in-memory OpenMLS provider) -> publish the commit (and,
/// for add / approve, a Welcome / rendezvous) to the Delivery Service -> persist
/// the N+1 snapshot best-effort. A crash — or a swallowed persist failure — in
/// the window between publish and persist left the commit DURABLE on the DS
/// (peers advance to N+1) while the local snapshot rolled back to N: a silent,
/// single-device membership brick (the device is stuck at N, cannot re-apply its
/// own commit, and its future N-epoch messages are rejected by peers at N+1).
///
/// A bare persist-before-publish reorder does NOT fix this — it flips the desync
/// to "device durably at N+1 with the commit never published", stranding the
/// whole group. The commit's *durability*, not its *ordering*, must change: the
/// commit (and its Welcome / rendezvous) has to survive the crash so it can be
/// RE-PUBLISHED to catch the group up. That is what this outbox holds.
///
/// ## Atomicity — entry-first-then-snapshot (ADR-0040 Open Question 1, iOS)
///
/// The web reference co-writes the outbox entry AND the N+1 snapshot in ONE
/// atomic IndexedDB transaction. iOS has NO cross-store atomic transaction — the
/// snapshot is one AES-GCM file (`IdentitySnapshotStore`) and the outbox is a
/// separate per-entry file here — so this platform takes the ADR's SECOND
/// sanctioned ordering: **entry-first, then snapshot**, both fail-closed, and the
/// restart sweep reconciles the one torn middle state.
///
///   - The commit path writes the outbox entry FIRST, then the N+1 snapshot,
///     both BEFORE any publish. If EITHER throws, the op ABORTS (no publish).
///   - The forbidden state "snapshot at N+1 with NO outbox entry" (whole-group
///     strand) is impossible by construction: the entry is written strictly
///     before the snapshot.
///   - The ONE torn state — entry present, snapshot still at N (the snapshot
///     write failed / crashed after the entry write) — is provably a case where
///     the commit was NEVER published (publish is strictly after BOTH writes), so
///     the restart sweep DISCARDS the orphan entry and stays at N. That is why
///     `targetEpoch` is stored: the sweep compares it against the group's
///     durably-restored `Group.epoch()`.
///
/// ## What an entry holds (invariants #1 / #2 preserved)
///
/// MLS commit ciphertext + a HPKE-sealed Welcome / rendezvous — opaque PUBLIC
/// artefacts the DS already stores verbatim. No plaintext, no PII, nothing that
/// does not already leave the device, so the entry is stored as plain base64 in
/// a JSON file (unlike the security-critical MLS snapshot, which stays
/// device-key-encrypted in `IdentitySnapshotStore`). The file is still excluded
/// from iCloud backup — a commit is MLS-epoch-bound + device-local, exactly like
/// the snapshot it is paired with.
///
/// ## Persistence layout — one file per in-flight commit
///
/// ```text
///   ApplicationSupport/lakko-commit-outbox/
///     <accountIdB64safe>/
///       <id>.json     one PendingCommitEntry (id is a 16-byte hex string)
/// ```
///
/// One file per entry (not one file per account) so a durable write of a new
/// entry never has to read-modify-write a shared file — each entry is
/// independently durable, and a corrupt / partial file for one entry cannot
/// clobber another. The store is tiny (one entry per in-flight commit; cleared
/// on ack).

/// The re-publish destination of a DIRECT-path entry's second artifact
/// (ADR-0040, G-35).
///
/// One case since ADR-0045 S7. The external-join approve used to write a
/// `.rendezvous` entry riding the group journal; it is a proposal now, so it
/// commits nothing, records nothing, and the sweep owns every rendezvous
/// (`Chat/CommitterSweep.swift`).
public enum SecondArtifactKind: String, Codable, Equatable {
    /// A Welcome envelope -> the new member's mailbox (`publishMailbox`), the
    /// add path. Addressed by the recipient pseudonym in
    /// `PendingCommitEntry/secondArtifactMailboxB64`.
    case welcome
}

/// One durable pending-commit record (ADR-0040, G-35). Mirror of the web
/// `PendingCommitEntry`. The commit + Welcome / rendezvous bytes are opaque MLS
/// public artefacts (invariants #1 / #2), stored as plain base64.
public struct PendingCommitEntry: Codable, Equatable {
    /// Random 16-byte hex id — the entry file name.
    public var id: String
    /// base64(account_id) — set by the store on write so a logout / account
    /// switch can purge stale entries and the sweep never crosses accounts.
    public var accountId: String
    /// base64(group_id) — the sweep filters entries on this.
    public var groupId: String
    /// Decimal string of the post-commit epoch (N+1). The sweep parses it and
    /// compares against the group's durably-restored `Group.epoch()` to
    /// reconcile a torn entry-then-snapshot write (torn -> discard).
    public var targetEpoch: String
    /// base64 of the EXACT commit wire-bytes the core create-commit produced.
    public var commitB64: String
    /// base64 of the second artifact's EXACT publishable wire-bytes — the
    /// `wrapEnvelope(.welcome, welcome)` mailbox envelope. `nil` for remove /
    /// role (they publish only the commit).
    public var secondArtifactB64: String?
    /// Discriminates the second artifact's re-publish destination.
    public var secondArtifactKind: SecondArtifactKind?
    /// base64 of the recipient mailbox pseudonym — the `publishMailbox` target
    /// for the second artifact. `nil` when there is no second artifact.
    public var secondArtifactMailboxB64: String?
    /// ADR-0045 — `true` while this entry belongs to a STAGED sweep whose
    /// journal verdict has not been read yet.
    ///
    /// Such an entry is NOT re-publishable by the ADR-0040 restart loop, and
    /// either reason alone is decisive: its device is durably at N, not N+1, so
    /// the unconditional-re-publish argument that loop rests on does not hold
    /// for it; and its Welcome must not reach the wire until the verdict says
    /// the commit won (ADR-0045 obligation 2). `resolveOwedVerdict` owns it.
    /// Optional so an entry written before ADR-0045 decodes as `nil` = a plain
    /// direct-path commit, which is what it is.
    public var sweepVerdictPending: Bool?
    /// ADR-0045 — this sweep's verdict is SETTLED (its commit won and merged)
    /// and the only thing it still owes is the Welcome fan-out.
    ///
    /// Set in one durable write at the moment of the merge, which is what keeps
    /// obligation 2 intact: the flag IS the durable statement "the verdict said
    /// WON", so an entry that has not reached a verdict never carries it and a
    /// pre-verdict restart therefore cannot be talked into publishing a Welcome.
    ///
    /// ``sweepVerdictPending`` stays set alongside it, deliberately — that flag
    /// is what marks the entry as a SWEEP entry, and the ADR-0040 legacy
    /// re-publish loop skips on it. Clearing it would hand a welcome-owed entry
    /// to a loop that re-publishes the commit, finds no singular
    /// ``secondArtifactMailboxB64``, publishes no Welcome at all, and then ACKS
    /// — destroying the record. The two are read together:
    /// `sweepVerdictPending && !sweepWelcomesOwed` is verdict-owed,
    /// `sweepWelcomesOwed` is fan-out-owed.
    public var sweepWelcomesOwed: Bool?
    /// ADR-0045 — base64 mailbox pseudonyms for EVERY member a staged sweep
    /// adds. One sweep commit can carry several adds under a single Welcome,
    /// where the direct add path has exactly one destination
    /// (``secondArtifactMailboxB64``, which stays untouched for that path).
    public var welcomeMailboxesB64: [String]?
    /// ADR-0045 S7 — the sweep's group-journal `WelcomeRendezvous` envelopes,
    /// one per member the sweep adds.
    ///
    /// Whole envelopes rather than addresses, because a rendezvous binds the
    /// added member's signature key into its own bytes — so unlike the single
    /// Welcome envelope fanned out to N mailboxes above, every recipient gets
    /// different bytes. Recorded before any publish so a restart re-publishes
    /// exactly what this sweep computed. Empty when the sweep carried no add.
    public var welcomeRendezvousB64: [String]?
    /// `false` until BOTH artifacts have (re)published. An un-acked entry is
    /// swept on the first `pollGroup` of a session.
    public var acked: Bool
    /// DS ids once known — recorded on ack for diagnostics.
    public var commitMessageId: Int64?
    public var secondMessageId: Int64?

    public init(
        id: String,
        accountId: String,
        groupId: String,
        targetEpoch: String,
        commitB64: String,
        secondArtifactB64: String? = nil,
        secondArtifactKind: SecondArtifactKind? = nil,
        secondArtifactMailboxB64: String? = nil,
        sweepVerdictPending: Bool? = nil,
        sweepWelcomesOwed: Bool? = nil,
        welcomeMailboxesB64: [String]? = nil,
        welcomeRendezvousB64: [String]? = nil,
        acked: Bool = false,
        commitMessageId: Int64? = nil,
        secondMessageId: Int64? = nil
    ) {
        self.id = id
        self.accountId = accountId
        self.groupId = groupId
        self.targetEpoch = targetEpoch
        self.commitB64 = commitB64
        self.secondArtifactB64 = secondArtifactB64
        self.secondArtifactKind = secondArtifactKind
        self.secondArtifactMailboxB64 = secondArtifactMailboxB64
        self.sweepVerdictPending = sweepVerdictPending
        self.sweepWelcomesOwed = sweepWelcomesOwed
        self.welcomeMailboxesB64 = welcomeMailboxesB64
        self.welcomeRendezvousB64 = welcomeRendezvousB64
        self.acked = acked
        self.commitMessageId = commitMessageId
        self.secondMessageId = secondMessageId
    }
}

/// The subset a commit path supplies (ADR-0040, G-35). The store fills the
/// account scope + the `acked: false` flag, so the caller never has to know the
/// account id or the durability bookkeeping.
public struct NewCommitEntry: Equatable {
    public var id: String
    public var groupId: String
    public var targetEpoch: String
    public var commitB64: String
    public var secondArtifactB64: String?
    public var secondArtifactKind: SecondArtifactKind?
    public var secondArtifactMailboxB64: String?
    /// ADR-0045 — see ``PendingCommitEntry/sweepVerdictPending``.
    public var sweepVerdictPending: Bool?
    /// ADR-0045 — see ``PendingCommitEntry/welcomeMailboxesB64``.
    public var welcomeMailboxesB64: [String]?
    /// ADR-0045 S7 — the sweep's group-journal `WelcomeRendezvous` envelopes,
    /// one per member the sweep adds.
    ///
    /// Whole envelopes rather than addresses, because a rendezvous binds the
    /// added member's signature key into its own bytes — so unlike the single
    /// Welcome envelope fanned out to N mailboxes above, every recipient gets
    /// different bytes. Recorded before any publish so a restart re-publishes
    /// exactly what this sweep computed. Empty when the sweep carried no add.
    public var welcomeRendezvousB64: [String]?

    public init(
        id: String,
        groupId: String,
        targetEpoch: String,
        commitB64: String,
        secondArtifactB64: String? = nil,
        secondArtifactKind: SecondArtifactKind? = nil,
        secondArtifactMailboxB64: String? = nil,
        sweepVerdictPending: Bool? = nil,
        welcomeMailboxesB64: [String]? = nil,
        welcomeRendezvousB64: [String]? = nil
    ) {
        self.id = id
        self.groupId = groupId
        self.targetEpoch = targetEpoch
        self.commitB64 = commitB64
        self.secondArtifactB64 = secondArtifactB64
        self.secondArtifactKind = secondArtifactKind
        self.secondArtifactMailboxB64 = secondArtifactMailboxB64
        self.sweepVerdictPending = sweepVerdictPending
        self.welcomeMailboxesB64 = welcomeMailboxesB64
        self.welcomeRendezvousB64 = welcomeRendezvousB64
    }
}

/// A single sweep decision the pure `ChatRepository.planSweep` produces
/// (ADR-0040 torn-write reconciliation). Split out from the executing sweep so
/// the load-bearing decision is directly unit-testable.
public enum CommitSweepAction: Equatable {
    /// The group durably reached the entry's `targetEpoch` (the commit WAS
    /// applied) -> re-publish it idempotently + re-arm the committer's own-echo
    /// filter.
    case republish(PendingCommitEntry)
    /// The group did NOT reach `targetEpoch` (a torn entry-then-snapshot write
    /// whose snapshot was lost) -> the commit was NEVER published -> discard the
    /// orphan entry, stay at N (clean).
    case discardTornWrite(PendingCommitEntry)
}

/// Mint a random 16-byte hex id for a new outbox entry (mirror of the web
/// `newOutboxEntryId`).
public func newOutboxEntryId() -> String {
    var bytes = [UInt8](repeating: 0, count: 16)
    _ = SecRandomCopyBytes(kSecRandomDefault, 16, &bytes)
    return bytes.map { String(format: "%02x", $0) }.joined()
}

/// File-backed durable pending-commit outbox store (ADR-0040, G-35). See the
/// file-level doc for the layout + the entry-first-then-snapshot atomicity
/// argument. The write path is FAIL-CLOSED (throws); ack / delete are
/// best-effort (a lost ack merely re-publishes idempotently on the next sweep).
public final class CommitOutboxStore {

    /// Root directory for every account's outbox. `nil` when Application Support
    /// could not be resolved — every durable write then throws (fail-closed), so
    /// a commit that cannot be durably recorded is never published. Injectable
    /// so tests point it at a temp directory (or `nil` to exercise fail-closed).
    private let rootDir: URL?

    public init(rootDir: URL? = CommitOutboxStore.defaultRootDir()) {
        self.rootDir = rootDir
    }

    // MARK: - Durable write (FAIL-CLOSED)

    /// Persist a FULL entry durably, awaited, and THROW on failure — fail-closed.
    /// Writes one `<id>.json` file atomically. Building block for
    /// `putNewDurable` + the store round-trip test.
    public func putEntryDurable(accountIdB64: String, entry: PendingCommitEntry) throws {
        guard let dir = accountDir(accountIdB64) else {
            throw CommitOutboxStoreError.unavailable(
                "no application-support directory to persist the commit outbox"
            )
        }
        try FileManager.default.createDirectory(
            at: dir,
            withIntermediateDirectories: true
        )
        try? Self.excludeFromBackup(dir)
        let data = try JSONEncoder().encode(entry)
        let url = dir.appendingPathComponent("\(entry.id).json")
        // `.atomic` stages a temp file + renames it into place, so a reader
        // never observes a torn entry file.
        try data.write(to: url, options: .atomic)
        try? Self.excludeFromBackup(url)
    }

    /// Persist a `NewCommitEntry` durably, filling the account scope +
    /// `acked: false`. THROWS on failure (fail-closed) — this is the write a
    /// commit path runs (entry-first) BEFORE the snapshot + before any publish.
    public func putNewDurable(accountIdB64: String, entry: NewCommitEntry) throws {
        let full = PendingCommitEntry(
            id: entry.id,
            accountId: accountIdB64,
            groupId: entry.groupId,
            targetEpoch: entry.targetEpoch,
            commitB64: entry.commitB64,
            secondArtifactB64: entry.secondArtifactB64,
            secondArtifactKind: entry.secondArtifactKind,
            secondArtifactMailboxB64: entry.secondArtifactMailboxB64,
            sweepVerdictPending: entry.sweepVerdictPending,
            welcomeMailboxesB64: entry.welcomeMailboxesB64,
            welcomeRendezvousB64: entry.welcomeRendezvousB64,
            acked: false
        )
        try putEntryDurable(accountIdB64: accountIdB64, entry: full)
    }

    // MARK: - Read

    /// Load every un-acked entry for `accountIdB64`, optionally filtered to one
    /// group's base64 id. Missing account directory -> `[]`. An unreadable /
    /// undecodable single entry file is skipped (logged), never a hard failure —
    /// the sweep should re-publish whatever entries it CAN read.
    public func loadUnacked(
        accountIdB64: String,
        groupIdB64: String? = nil
    ) -> [PendingCommitEntry] {
        guard let dir = accountDir(accountIdB64),
              let urls = try? FileManager.default.contentsOfDirectory(
                at: dir,
                includingPropertiesForKeys: nil
              )
        else {
            return []
        }
        var out: [PendingCommitEntry] = []
        for url in urls where url.pathExtension == "json" {
            guard let data = try? Data(contentsOf: url),
                  let entry = try? JSONDecoder().decode(PendingCommitEntry.self, from: data)
            else {
                NSLog("commit-outbox: skipping unreadable entry \(url.lastPathComponent)")
                continue
            }
            if entry.acked { continue }
            if let groupIdB64 = groupIdB64, entry.groupId != groupIdB64 { continue }
            out.append(entry)
        }
        return out
    }

    // MARK: - Ack / delete (best-effort)

    /// Mark an entry acked + record the published DS ids. Best-effort: a failure
    /// leaves the entry un-acked, so the next sweep re-publishes it idempotently
    /// (peers drop the wrong-epoch duplicate; the committer's echo filter is
    /// re-armed). No-op when the entry file is already gone.
    public func markAckedBestEffort(
        accountIdB64: String,
        id: String,
        commitMessageId: Int64? = nil,
        secondMessageId: Int64? = nil
    ) {
        guard let dir = accountDir(accountIdB64) else { return }
        let url = dir.appendingPathComponent("\(id).json")
        guard let data = try? Data(contentsOf: url),
              var entry = try? JSONDecoder().decode(PendingCommitEntry.self, from: data)
        else {
            return
        }
        entry.acked = true
        if let commitMessageId = commitMessageId { entry.commitMessageId = commitMessageId }
        if let secondMessageId = secondMessageId { entry.secondMessageId = secondMessageId }
        guard let encoded = try? JSONEncoder().encode(entry) else { return }
        try? encoded.write(to: url, options: .atomic)
    }

    /// ADR-0045 — flip a sweep entry from *verdict owed* to *Welcome fan-out
    /// owed*, recording the DS id its commit published under, in ONE atomic
    /// file write so it is never half-applied.
    ///
    /// THROWS on failure. The caller treats that as non-fatal: the marker only
    /// buys a retry, and the Welcome it guards is correct to publish either way.
    ///
    /// A no-op when the entry is already gone (a concurrent ack / discard raced
    /// us).
    public func markWelcomesOwed(
        accountIdB64: String,
        id: String,
        commitMessageId: Int64
    ) throws {
        guard let dir = accountDir(accountIdB64) else {
            throw CommitOutboxStoreError.unavailable(
                "no application-support directory to record the owed Welcome fan-out"
            )
        }
        let url = dir.appendingPathComponent("\(id).json")
        guard let data = try? Data(contentsOf: url),
              var entry = try? JSONDecoder().decode(PendingCommitEntry.self, from: data)
        else {
            return
        }
        entry.sweepWelcomesOwed = true
        entry.commitMessageId = commitMessageId
        let encoded = try JSONEncoder().encode(entry)
        try encoded.write(to: url, options: .atomic)
    }

    /// Delete an entry by id. Best-effort, no-op when absent.
    public func deleteEntryBestEffort(accountIdB64: String, id: String) {
        guard let dir = accountDir(accountIdB64) else { return }
        let url = dir.appendingPathComponent("\(id).json")
        try? FileManager.default.removeItem(at: url)
    }

    /// Purge every entry for an account — recover / forget-device hygiene, so a
    /// prior session's pending commits never get swept into a later one. No-op
    /// when there is nothing to purge.
    public func deleteOutboxForAccount(accountIdB64: String) {
        guard let dir = accountDir(accountIdB64) else { return }
        try? FileManager.default.removeItem(at: dir)
    }

    // MARK: - Layout

    private func accountDir(_ accountIdB64: String) -> URL? {
        guard let rootDir = rootDir else { return nil }
        // Replace any `/` in the base64 account id with `_` so the segment is
        // filesystem-safe — same posture as `IdentitySnapshotStore`.
        let safe = accountIdB64.replacingOccurrences(of: "/", with: "_")
        return rootDir.appendingPathComponent(safe, isDirectory: true)
    }

    /// Default outbox root under Application Support. `public` so the `public`
    /// initializer's default-argument value may reference it.
    public static func defaultRootDir() -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return nil
        }
        return appSupport.appendingPathComponent("lakko-commit-outbox")
    }

    private static func excludeFromBackup(_ url: URL) throws {
        var url = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try url.setResourceValues(values)
    }
}

public enum CommitOutboxStoreError: Error {
    /// The durable-persist location could not be resolved — there is no safe
    /// place to record the pending commit, so the fail-closed commit path must
    /// abort before publishing.
    case unavailable(String)
}
