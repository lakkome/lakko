// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Where the DeviceGroup receive loop resumes after a restart.
///
/// ## What it fixes
///
/// The loop started every process at cursor 0, so each launch fetched every
/// row the account's DeviceGroup journal had ever carried and handed it to
/// OpenMLS again. Every one failed: this device's own rows as
/// `CannotDecryptOwnMessage` (a sender cannot open what it sent), every other
/// device's rows because their generation was consumed the first time round,
/// and each failure logged a line. Mirror of Android's
/// `DeviceGroupJournalCursor.kt` and web's `devices/sync/journal-batch.ts`.
///
/// ## The rule (C-67)
///
/// The persisted cursor may LAG the durable DeviceGroup snapshot, never LEAD
/// it. So it is written only after the snapshot that reflects every row up to
/// it was written successfully in the same run, and it never moves past a row
/// that might still take this device forward if read again: a foreign commit
/// that failed while sealed at or above the handle's epoch. A row that can
/// never apply (an own echo, an older epoch's commit, an application message
/// whose generation is consumed, bytes that are not MLS) is passed, exactly as
/// the in-session loop always passed it.
///
/// The cursor is bound to the journal it was read on: a key reset founds a new
/// DeviceGroup journal (ADR-0078 phase 6), and a position on the old one must
/// never skip rows of the new one. It is stored beside the snapshot under the
/// same wrap key (``DeviceGroupSnapshotStore/PendingKind/journalCursor``) and
/// deleted with it on Forget device.
enum DeviceGroupJournalCursor {

    private static let cursorLength = 8

    /// `group_id || cursor (8 bytes, big-endian)`.
    static func encode(groupId: Data, cursor: Int64) -> Data {
        var out = Data(groupId)
        withUnsafeBytes(of: UInt64(bitPattern: cursor).bigEndian) { out.append(contentsOf: $0) }
        return out
    }

    /// The cursor stored for `groupId`, or 0 when `stored` is absent, names
    /// another journal, or is not a usable position.
    static func decode(_ stored: Data?, groupId: Data) -> Int64 {
        guard let stored, stored.count == groupId.count + cursorLength,
              stored.prefix(groupId.count) == groupId else { return 0 }
        var value: UInt64 = 0
        for byte in stored.suffix(cursorLength) { value = (value << 8) | UInt64(byte) }
        let signed = Int64(bitPattern: value)
        return signed < 0 ? 0 : signed
    }

    /// The stored cursor of `accountIdB64`'s DeviceGroup journal `groupId`;
    /// 0 when none can be read (re-reading is the safe direction).
    static func read(
        accountIdB64: String, groupId: Data, store: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) -> Int64 {
        decode(store.getPending(accountIdB64: accountIdB64, kind: .journalCursor), groupId: groupId)
    }

    /// Store `cursor`. Throws on a failed write or an ended account session.
    static func write(
        accountIdB64: String, groupId: Data, cursor: Int64,
        store: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) throws {
        try store.putPending(
            accountIdB64: accountIdB64, kind: .journalCursor, blob: encode(groupId: groupId, cursor: cursor))
    }

    /// Whether a row's decrypt failure is this device's own row coming back:
    /// openmls's `ValidationError::CannotDecryptOwnMessage` reaches the client
    /// inside the error text. Its effect is already in the snapshot (an own
    /// commit is merged before it is published, ADR-0081; an own application
    /// message changes nothing here).
    static func isOwnMessageEcho(_ error: Error) -> Bool {
        String(describing: error).contains("CannotDecryptOwnMessage")
    }
}

/// What a row that failed to apply means for the persisted cursor.
enum FailedRowVerdict: Equatable {
    /// The row can never apply: the cursor may move past it.
    case pass
    /// The row is a commit that might still apply if read again: the persisted
    /// cursor stays before it.
    case hold

    /// The verdict for a row `processIncomingVerifyingRemovals` refused.
    ///
    /// `sealedEpoch` / `epochAfter`: the row's framed epoch and the epoch after
    /// it (`epochOfMessage` / `epochAfterMessage`; `after > sealed` only for a
    /// commit), `nil` when the bytes are not an MLS message. `localEpoch`: the
    /// handle's epoch when the row was read, `nil` when unreadable.
    static func of(isOwnEcho: Bool, sealedEpoch: UInt64?, epochAfter: UInt64?, localEpoch: UInt64?) -> Self {
        if isOwnEcho { return .pass }
        // Not MLS framing: no later read can process it either.
        guard let sealedEpoch, let epochAfter else { return .pass }
        // Not a commit: an application message or a proposal cannot move this
        // device's epoch, and a consumed generation never opens again.
        if epochAfter <= sealedEpoch { return .pass }
        // A commit of an epoch this handle is already past can never apply.
        if let localEpoch, sealedEpoch < localEpoch { return .pass }
        return .hold
    }
}

/// The persisted position of one run of the receive loop, under the rule in
/// ``DeviceGroupJournalCursor``. One loop owns it.
struct JournalCursorTracker {
    /// The highest row id such that every row up to it is reflected in the
    /// in-memory state or can never apply.
    private var safe: Int64
    /// A held row was met (or the snapshot moved under this handle): `safe`
    /// moves no further this run.
    private var holding = false
    /// A snapshot write failed: the in-memory state is ahead of the disk
    /// until a later snapshot write succeeds.
    private var dirty = false
    private var written: Int64

    init(start: Int64) {
        safe = start
        written = start
    }

    /// A row applied, or refused in a way that can never apply.
    mutating func passed(_ rowId: Int64) {
        if !holding, rowId > safe { safe = rowId }
    }

    /// A row refused; see ``FailedRowVerdict/of(isOwnEcho:sealedEpoch:epochAfter:localEpoch:)``.
    mutating func refused(_ rowId: Int64, verdict: FailedRowVerdict) {
        switch verdict {
        case .pass: passed(rowId)
        case .hold: holding = true
        }
    }

    mutating func snapshotPersisted() { dirty = false }

    mutating func snapshotPersistFailed() { dirty = true }

    /// Another handle wrote the snapshot past this one and the loop reloads
    /// from disk: what this run applied in memory since the last stored
    /// position may not be on disk, so the position goes back to the stored
    /// one and nothing more is stored this run (the next start reads again
    /// from there; rows already applied fail as consumed and are passed).
    mutating func snapshotMovedUnderHandle() {
        safe = written
        holding = true
    }

    /// The cursor to store now, or `nil` when nothing new may be stored.
    func cursorToWrite() -> Int64? { !dirty && safe > written ? safe : nil }

    mutating func cursorWritten(_ value: Int64) {
        if value > written { written = value }
    }
}

// MARK: - The one read of a superseded device (ADR-0074 section 3.3)

/// What reading the account's DeviceGroup journal to its head came to, for a
/// device whose session the server refused as superseded and that holds no
/// key-rotation envelope.
enum JournalHeadRead: Equatable {
    /// Read to the head; every service envelope on the way was handed to the
    /// key-rotation intake.
    case reached
    /// This device holds no DeviceGroup state it can read the journal with.
    case noState
    /// The DS refused the read (`401`, `403`, `404`): this device's epoch
    /// key is no longer the journal's.
    case refused
    /// No answer within the bound (offline, a timeout, a `5xx`): nothing is
    /// decided; the read is tried again later, and nothing is wiped.
    case noAnswer
}

/// ADR-0074 section 3.3 — a superseded device with no stored rotation
/// envelope READS its DeviceGroup journal once before it treats the refusal
/// as a reset; the DS needs no session token. When the sync runtime is not
/// running (a launch whose refresh was refused before any session landed),
/// waiting for it would read nothing: this opens the persisted DeviceGroup
/// state on a handle of its own, fetches from the persisted cursor to the
/// head under the epoch-key DS proof, hands each service envelope over, and
/// persists NOTHING (no snapshot, no cursor): the sync runtime reads the
/// same rows again when it runs.
enum DeviceGroupJournalOneShot {

    /// The whole read is bounded by this; chosen, not measured.
    static let deadlineMs: Int64 = 40_000

    struct Page: Equatable {
        let rows: [Row]
        let hasMore: Bool
    }

    struct Row: Equatable {
        let id: Int64
        let payload: Data
    }

    enum FetchFailure: Error, Equatable {
        case refused
        case noAnswer
    }

    /// One open DeviceGroup handle: how to fetch a page after a cursor, and
    /// how to apply one row to the in-memory handle, returning the service
    /// envelope it carried (or `nil`).
    struct Reader {
        let startCursor: Int64
        let fetch: (_ after: Int64) async throws -> Page
        let serviceEnvelope: (_ payload: Data) -> Data?
    }

    /// Read to the head with `reader` (`nil`: no state), handing every
    /// service envelope to `deliver`. Pure over its ports.
    static func drain(
        reader: Reader?,
        deliver: (Data) -> Void,
        nowMs: () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) },
        deadlineMs: Int64 = DeviceGroupJournalOneShot.deadlineMs
    ) async -> JournalHeadRead {
        guard let reader else { return .noState }
        let end = nowMs() + deadlineMs
        var cursor = reader.startCursor
        while true {
            let left = end - nowMs()
            guard left > 0 else { return .noAnswer }
            let page: Page
            do {
                page = try await withDeadline(ms: left) { try await reader.fetch(cursor) }
            } catch FetchFailure.refused {
                return .refused
            } catch {
                return .noAnswer
            }
            for row in page.rows {
                cursor = max(cursor, row.id)
                if let envelope = reader.serviceEnvelope(row.payload) { deliver(envelope) }
            }
            if !page.hasMore { return .reached }
            if page.rows.isEmpty { return .noAnswer }
        }
    }

    /// `body`, or ``FetchFailure/noAnswer`` once `ms` ran out.
    private static func withDeadline<T: Sendable>(
        ms: Int64, _ body: @escaping @Sendable () async throws -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await body() }
            group.addTask {
                try await Task.sleep(nanoseconds: UInt64(ms) * 1_000_000)
                throw FetchFailure.noAnswer
            }
            defer { group.cancelAll() }
            guard let first = try await group.next() else { throw FetchFailure.noAnswer }
            return first
        }
    }

    /// The reader over this device's persisted DeviceGroup state, or `nil`
    /// when it holds none it can read with. Writes nothing: the snapshot is
    /// restored into a handle of its own that is never persisted.
    static func live(
        accountIdBytes: Data,
        accountIdB64: String,
        snapshotStore: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore(),
        ds: DsClient = DsClient()
    ) async -> Reader? {
        guard let seed = DeviceGroupLifecycle.lineageSeed(accountIdB64: accountIdB64),
              let lineage = try? Identity.fromSeed(seed: seed),
              let deviceSeed = try? DeviceIdentityStore.getDeviceIdentitySeed(accountIdB64: accountIdB64),
              let snapshot = snapshotStore.getSnapshot(accountIdB64: accountIdB64),
              let identity = try? Identity.fromSeed(seed: deviceSeed),
              (try? identity.restoreStorage(snapshot: snapshot)) != nil,
              let group = try? loadDeviceGroup(identity: identity, accountId: accountIdBytes, accountIdentity: lineage),
              let groupId = try? group.groupId() else {
            return nil
        }
        // Removals under the attested-removal policy are verified as the
        // sync runtime verifies them; without the key the batch could not be
        // applied faithfully, so the read gets no answer.
        let attestVk: Data
        do {
            attestVk = try await DeviceAttestKey.fetch()
        } catch {
            return Reader(
                startCursor: 0,
                fetch: { _ in throw FetchFailure.noAnswer },
                serviceEnvelope: { _ in nil })
        }
        return Reader(
            startCursor: DeviceGroupJournalCursor.read(
                accountIdB64: accountIdB64, groupId: groupId, store: snapshotStore),
            fetch: { after in
                // Signed under the handle's epoch at each page: a commit on
                // the way moves it. No reclaim: this read claims nothing.
                guard let signer = try? group.dsAccessSigner(identity: identity) else {
                    throw FetchFailure.refused
                }
                do {
                    let result = try await ds.fetchGroup(
                        groupId: groupId, after: after, limit: nil, waitSecs: 0,
                        auth: DsAuth(signer: signer))
                    return Page(
                        rows: result.messages.map { Row(id: $0.id, payload: $0.payload) },
                        hasMore: result.hasMore)
                } catch DsClientError.http(let status, _, _) where [401, 403, 404].contains(status) {
                    throw FetchFailure.refused
                } catch {
                    throw FetchFailure.noAnswer
                }
            },
            serviceEnvelope: { payload in
                guard let entry = try? DeviceGroupJournalEnvelope.decode(payload) else { return nil }
                let localEpoch = try? group.epoch()
                let processed: ProcessedMessage
                do {
                    processed = try group.processIncomingVerifyingRemovals(
                        identity: identity,
                        messageBytes: entry.message,
                        attestations: entry.attestation.map { [$0] } ?? [],
                        attestVerifyingKey: attestVk,
                        nowUnixSecs: Int64(Date().timeIntervalSince1970))
                } catch {
                    // ADR-0091 phase 1 — logged, no identifier; behaviour
                    // unchanged (this read persists nothing).
                    RegisteredCommitLog.skipped(
                        error: error, payload: entry.message, localEpoch: localEpoch, latestEpoch: nil)
                    return nil
                }
                guard case .application(let data, _, _) = processed,
                      let parsed = try? parseDevicegroupPayload(plaintext: data),
                      case .serviceEnvelope(let envelope) = parsed.body else {
                    return nil
                }
                return envelope
            })
    }
}
