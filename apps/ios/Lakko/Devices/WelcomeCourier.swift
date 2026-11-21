// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// DS-mailbox round-trip for the add-device Welcome delivery
/// (Phase 2a3b3 iOS infrastructure).
///
/// Mirror of `apps/web/src/devices/welcome-courier.ts`. The
/// scanner-side device PUTs the Welcome via
/// [`publishWelcome`]; the joining-side device long-polls
/// via [`pollForWelcome`] until the Welcome arrives, the
/// caller's cancellation `Task.isCancelled` fires, or the
/// soft timeout elapses.
///
/// No new server endpoints — the existing anonymous
/// `POST/GET /ds/rendezvous/{id_hex}/messages` route (ADR-0060) is
/// exactly the rendezvous-mailbox contract.
public enum WelcomeCourier {

    /// Default soft timeout for [`pollForWelcome`]. The UI
    /// can override via the `timeoutSeconds` argument.
    public static let defaultTimeoutSeconds: TimeInterval = 60

    /// Server-side long-poll budget per request. The DS
    /// service clamps to its own MAX_WAIT_SECS; we ask
    /// comfortably below that so a misclock between client
    /// and server doesn't waste a full poll.
    private static let perRequestLongPollSeconds: Int = 15

    /// Discriminated outcome of [`pollForWelcome`].
    public enum WelcomeOutcome {
        /// `welcomeCursor` is the DS journal id the Welcome row
        /// landed on; the history-bundle poll picks up from
        /// `after = welcomeCursor`.
        case received(Data, welcomeCursor: Int64)
        case timeout
        case cancelled
    }

    /// Discriminated outcome of
    /// [`pollForHistoryBundle`] (Phase 2a3d8-ui).
    public enum HistoryBundleOutcome {
        case received(Data)
        case timeout
        case cancelled
    }

    /// Soft timeout for the joiner-side history poll (Phase
    /// 2a3d8-ui). Short — bundle is supposed to arrive within
    /// seconds of the Welcome; a longer wait stalls the UX.
    public static let historyBundleTimeoutSeconds: TimeInterval = 10

    /// Scanner side: PUT a Welcome to the rendezvous mailbox
    /// slot the joining device chose.
    public static func publishWelcome(
        rendezvousPseudonym: Data,
        welcomeBytes: Data,
        ds: DsClient = DsClient()
    ) async throws {
        _ = try await ds.publishRendezvous(id: rendezvousPseudonym,
            payload: welcomeBytes
        )
    }

    /// Joining side: poll the rendezvous mailbox until the
    /// Welcome arrives, the calling Task is cancelled, or
    /// the soft timeout elapses.
    ///
    /// Returns:
    ///   * `.received(welcome)` — hand to
    ///     `DeviceGroup.joinFromWelcome`.
    ///   * `.timeout` — UI surfaces "no response yet" with a
    ///     retry affordance.
    ///   * `.cancelled` — calling task was cancelled (user
    ///     closed the pairing screen).
    ///
    /// Throws on transport / server failures (404 on a fresh
    /// mailbox is treated as "no rows yet, keep polling" —
    /// not an error).
    ///
    /// `after` is the last row already tried as the Welcome that did not
    /// join (``PairingOrchestrator/WelcomeNotOpened``): the poll reads past
    /// it. A recovery or state row is never the Welcome and is skipped.
    public static func pollForWelcome(
        rendezvousPseudonym: Data,
        after: Int64 = 0,
        timeoutSeconds: TimeInterval = defaultTimeoutSeconds,
        ds: DsClient = DsClient()
    ) async throws -> WelcomeOutcome {
        let deadline = Date(timeIntervalSinceNow: timeoutSeconds)
        // `after=0` is the canonical "give me everything from
        // the beginning of this slot" cursor for the DS
        // mailbox. The slot is fresh per pairing, so the
        // first published id is 1.
        var cursor: Int64 = max(0, after)

        while true {
            if Task.isCancelled { return .cancelled }
            let remaining = deadline.timeIntervalSinceNow
            if remaining <= 0 { return .timeout }

            let waitSecs = min(
                Self.perRequestLongPollSeconds,
                max(1, Int(remaining))
            )
            let result: FetchResult
            do {
                result = try await ds.fetchRendezvous(id: rendezvousPseudonym,
                    after: cursor,
                    limit: RendezvousRead.pageSize,
                    waitSecs: waitSecs
                )
            } catch DsClientError.http(let status, _, _) where status == 404 {
                // No rows yet — keep polling within the
                // overall deadline.
                continue
            } catch let err as DsClientError {
                if case .network = err {
                    // Transient network blip — try again
                    // until the deadline elapses.
                    continue
                }
                throw err
            }
            if let row = result.messages.first(where: { $0.id > cursor && !isTaggedRow($0.payload) }) {
                return .received(row.payload, welcomeCursor: row.id)
            }
            // Advance cursor by the highest id seen so the
            // next poll doesn't re-fetch the same range.
            cursor = result.messages.reduce(cursor) { max($0, $1.id) }
        }
    }

    /// Scanner side: PUT the sealed history bundle to the
    /// rendezvous slot AFTER the Welcome has been published.
    /// FIFO journal assigns it the next id (id=2 if Welcome
    /// was the first publish).
    public static func publishHistoryBundle(
        rendezvousPseudonym: Data,
        sealedBytes: Data,
        ds: DsClient = DsClient()
    ) async throws {
        _ = try await ds.publishRendezvous(id: rendezvousPseudonym,
            payload: sealedBytes
        )
    }

    /// Joining side: poll the rendezvous mailbox past the
    /// Welcome cursor for the sealed history bundle. Soft
    /// timeout (default 10 s) — the scanner side either
    /// publishes within that window or never (e.g. it had no
    /// prior history; nothing to send).
    public static func pollForHistoryBundle(
        rendezvousPseudonym: Data,
        welcomeCursor: Int64,
        timeoutSeconds: TimeInterval = historyBundleTimeoutSeconds,
        opens: ((Data) -> Bool)? = nil,
        ds: DsClient = DsClient()
    ) async throws -> HistoryBundleOutcome {
        let deadline = Date(timeIntervalSinceNow: timeoutSeconds)
        var cursor = welcomeCursor

        while true {
            if Task.isCancelled { return .cancelled }
            let remaining = deadline.timeIntervalSinceNow
            if remaining <= 0 { return .timeout }

            let waitSecs = min(
                Self.perRequestLongPollSeconds,
                max(1, Int(remaining))
            )
            let result: FetchResult
            do {
                result = try await ds.fetchRendezvous(id: rendezvousPseudonym,
                    after: cursor,
                    limit: RendezvousRead.pageSize,
                    waitSecs: waitSecs
                )
            } catch DsClientError.http(let status, _, _) where status == 404 {
                continue
            } catch let err as DsClientError {
                if case .network = err {
                    continue
                }
                throw err
            }
            if let row = firstHistoryRow(result.messages, after: cursor, opens: opens) {
                return .received(row)
            }
            cursor = result.messages.reduce(cursor) { max($0, $1.id) }
        }
    }

    /// A recovery or state row: tagged, never the Welcome or the history
    /// bundle.
    static func isTaggedRow(_ row: Data) -> Bool {
        RecoveryPayload.isRecoveryRow(row) || StateBundle.isStateRow(row)
    }

    /// The history bundle among `rows`, in order: past `after`, not a tagged
    /// row (ADR-0018 factor (a): the rendezvous also carries the recovery
    /// and state rows), and, when `opens` is given, one that opens. A row
    /// that does not open is passed over: anyone who learned the rendezvous
    /// can post to it.
    static func firstHistoryRow(
        _ rows: [MailboxMessage], after: Int64, opens: ((Data) -> Bool)?
    ) -> Data? {
        for row in rows where row.id > after && !isTaggedRow(row.payload) {
            if let opens, !opens(row.payload) { continue }
            return row.payload
        }
        return nil
    }
}
