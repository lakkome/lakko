// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0074 "T5 as built" items 17, 24 and 25 — two rules of the routes that
// take an account statement, free of state.
//
//   * `StatementStale`: `401 {"error":"statement-stale","server_time_ms":T}`
//     is answered ONLY to a statement whose signature verified and whose time
//     is outside the server's 120 s. The device holds the right key and a
//     wrong clock: it signs the same claims again at `T` and repeats the
//     request ONCE. Nothing was counted, and nothing may be destroyed on it
//     (a recall record in particular).
//   * `RecallFinishRetry`: `POST /account/recall/finish` answers a
//     byte-identical request again for ten minutes, so a lost answer is asked
//     for again with the SAME bytes instead of abandoning the recall.

/// The stale-statement answer and its one retry.
public enum StatementStale {

    public static let code = "statement-stale"

    /// The server's time carried by a `statement-stale` answer, or `nil` when
    /// `status` / `body` is not that answer (or carries no usable time).
    public static func serverTimeMs(status: Int, body: String) -> Int64? {
        guard status == 401,
              let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any],
              json["error"] as? String == code,
              let time = json["server_time_ms"] as? NSNumber, time.int64Value > 0 else { return nil }
        return time.int64Value
    }

    public static func serverTimeMs(of error: Error) -> Int64? {
        if case .http(let status, _, let body) = error as? AuthClientError {
            return serverTimeMs(status: status, body: body)
        }
        return nil
    }

    /// Whether `error` is the stale answer at all (with or without a time).
    public static func isStale(_ error: Error) -> Bool {
        guard case .http(401, _, let body) = error as? AuthClientError,
              let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        else { return false }
        return json["error"] as? String == code
    }

    /// Run `attempt(nil)` (the statement signed at the device's own time). On
    /// the stale answer, run `attempt(T)` exactly once, where the statement
    /// is signed at the server's time `T`. A second stale answer is thrown as
    /// it is: the caller treats it as "nothing is known", never as a refusal.
    public static func retrying<Value>(
        _ attempt: (_ serverTimeMs: Int64?) async throws -> Value
    ) async throws -> Value {
        do {
            return try await attempt(nil)
        } catch {
            guard let serverTime = serverTimeMs(of: error) else { throw error }
            NSLog("[auth] a statement was refused as stale; signing again at the server's time")
            return try await attempt(serverTime)
        }
    }
}

/// The retry of a recall's `finish` whose answer did not arrive.
public enum RecallFinishRetry {

    /// How many times the SAME request bytes are sent at most (the first
    /// send included). Chosen, not measured: the server keeps the outcome for
    /// the attempt's own ten minutes.
    public static let maxSends = 3

    /// Whether the request is sent again with the same bytes: only when no
    /// answer arrived. Any HTTP answer is the server's verdict.
    public static func resends(after error: Error) -> Bool {
        if case .network = error as? AuthClientError { return true }
        return false
    }

    /// Send `body` through `send`, again with the identical bytes while no
    /// answer arrives, at most ``maxSends`` times.
    public static func sendIdentical<Value>(
        body: Data,
        send: (_ body: Data) async throws -> Value
    ) async throws -> Value {
        var sends = 0
        while true {
            sends += 1
            do {
                return try await send(body)
            } catch {
                guard resends(after: error), sends < maxSends else { throw error }
                NSLog("[auth] the recall's finish got no answer; asking again with the same request")
            }
        }
    }
}
