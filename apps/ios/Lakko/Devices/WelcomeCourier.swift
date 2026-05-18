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
/// `POST/GET /ds/mailbox/{pseudonym_hex}/messages` route is
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
        case received(Data)
        case timeout
        case cancelled
    }

    /// Scanner side: PUT a Welcome to the rendezvous mailbox
    /// slot the joining device chose.
    public static func publishWelcome(
        rendezvousPseudonym: Data,
        welcomeBytes: Data,
        ds: DsClient = DsClient()
    ) async throws {
        _ = try await ds.publishMailbox(
            pseudonym: rendezvousPseudonym,
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
    public static func pollForWelcome(
        rendezvousPseudonym: Data,
        timeoutSeconds: TimeInterval = defaultTimeoutSeconds,
        ds: DsClient = DsClient()
    ) async throws -> WelcomeOutcome {
        let deadline = Date(timeIntervalSinceNow: timeoutSeconds)
        // `after=0` is the canonical "give me everything from
        // the beginning of this slot" cursor for the DS
        // mailbox. The slot is fresh per pairing, so the
        // first published id is 1.
        var cursor: Int64 = 0

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
                result = try await ds.fetchMailbox(
                    pseudonym: rendezvousPseudonym,
                    after: cursor,
                    limit: 1,
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
            if let first = result.messages.first {
                return .received(first.payload)
            }
            // Advance cursor by the highest id seen so the
            // next poll doesn't re-fetch the same empty
            // range. Happy path returns above; this branch
            // is defensive.
            cursor = result.messages.reduce(cursor) { max($0, $1.id) }
        }
    }
}
