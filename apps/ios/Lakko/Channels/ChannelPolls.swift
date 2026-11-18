// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0090 section 4 — polls in channels: one account, one vote, through a
// keyed-verification anonymous credential auth issues per period (CMZ, the
// core's `lakko-anon-cred`), presented to the channel service with a tag per
// poll. The service learns counts per opaque option label, never the account.

/// The pure rules of a channel vote request.
public enum ChannelVoteRule {
    /// ADR-0090 section 4.3 — the service takes `|at_ms - server| <= 120 s`.
    public static let windowMs: Int64 = 120_000

    /// What to do after the service refused a vote.
    public enum Next: Equatable {
        /// Send again with this `at_ms`.
        case retry(atMs: Int64)
        /// Give up (the footer says `Your vote wasn't sent`).
        case fail
    }

    /// `409 stale-vote {stored_at_ms}`: retry with `stored_at_ms + 1` while
    /// that is inside the window around `nowMs`; `400
    /// vote-time-out-of-window {server_time_ms}`: retry once at the server's
    /// time. Anything else, or a second retry, fails.
    public static func next(
        code: String, storedAtMs: Int64?, serverTimeMs: Int64?, nowMs: Int64, attempt: Int
    ) -> Next {
        guard attempt == 0 else { return .fail }
        switch code {
        case "stale-vote":
            guard let stored = storedAtMs else { return .fail }
            let at = stored + 1
            return abs(at - (serverTimeMs ?? nowMs)) <= windowMs ? .retry(atMs: at) : .fail
        case "vote-time-out-of-window":
            guard let server = serverTimeMs else { return .fail }
            return .retry(atMs: server)
        default:
            return .fail
        }
    }

    /// Standard base64url without padding (the routes' binary fields).
    public static func b64url(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    public static func fromB64url(_ text: String) -> Data? {
        var s = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while s.count % 4 != 0 { s += "=" }
        return Data(base64Encoded: s)
    }

    public static func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }
}

/// Why a channel vote did not go out.
public enum ChannelVoteError: Error, Equatable {
    case noCredential
    case refused(String)
    case network
}

/// ADR-0090 section 4.2 — the account's credential for the current period,
/// asked for in the background the first time a vote needs it in a period.
@MainActor
public final class ChannelPollCredentials {
    public static let shared = ChannelPollCredentials()

    let baseURL: String
    private let session: URLSession

    init(baseURL: String = AuthClient.defaultBaseURL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    static func storeKey(_ account: String, _ period: UInt64) -> String {
        "lakko.anon-credential.v1.\(account).\(period)"
    }

    /// The credential (secret, credential bytes, issuer key) for now.
    func current(writer: Identity, accountIdB64: String) async throws
        -> (secret: AnonCredentialSecret, credential: Data, key: AnonIssuerPublicKey) {
        let secret = try AnonCredentialSecret.fromLineageSeed(lineageSeed: writer.ed25519Seed())
        var period = anonCredPeriodAt(unixSecs: UInt64(Date().timeIntervalSince1970))
        if let held = UserDefaults.standard.data(forKey: Self.storeKey(accountIdB64, period)),
           let key = try? await issuerKey(period: period) {
            return (secret, held, key)
        }
        for _ in 0..<2 {
            let key = try await issuerKey(period: period)
            let request = try secret.issuanceRequest(key: key)
            let (status, body) = try await send(
                "POST", "/auth/anon-credential", json: ["request_b64": ChannelVoteRule.b64url(request)])
            if status == 409, Self.code(body) == "commitment-not-registered" {
                let registration = try secret.registration()
                let (put, _) = try await send(
                    "PUT", "/auth/anon-credential/commitment",
                    json: ["registration_b64": ChannelVoteRule.b64url(registration)])
                guard put == 204 || put == 409 else { throw ChannelVoteError.noCredential }
                continue
            }
            if status == 400, Self.code(body) == "period-not-issuable",
               let current = (try? JSONSerialization.jsonObject(with: body) as? [String: Any])?["current_period"] as? NSNumber {
                period = current.uint64Value
                continue
            }
            guard (200..<300).contains(status),
                  let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
                  let responseB64 = object["response_b64"] as? String,
                  let response = ChannelVoteRule.fromB64url(responseB64) else {
                throw ChannelVoteError.noCredential
            }
            // Checked against the key the anonymous key route publishes,
            // never the one the issuance echoes.
            let credential = try secret.finalizeIssuance(key: key, response: response)
            let issued = (try? anonCredCredentialPeriod(credential: credential)) ?? period
            UserDefaults.standard.set(credential, forKey: Self.storeKey(accountIdB64, issued))
            return (secret, credential, try await issuerKey(period: issued))
        }
        throw ChannelVoteError.noCredential
    }

    private func issuerKey(period: UInt64) async throws -> AnonIssuerPublicKey {
        let (status, body) = try await send("GET", "/auth/anon-credential/key/\(period)", json: nil, bearer: false)
        guard status == 200, let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let keyB64 = object["public_key_b64"] as? String, let bytes = ChannelVoteRule.fromB64url(keyB64) else {
            throw ChannelVoteError.noCredential
        }
        return try AnonIssuerPublicKey.decode(period: period, bytes: bytes)
    }

    private func send(_ method: String, _ path: String, json: [String: String]?, bearer: Bool = true)
        async throws -> (Int, Data) {
        guard let url = URL(string: baseURL + path) else { throw ChannelVoteError.network }
        var request = URLRequest(url: url)
        request.httpMethod = method
        if let json {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: json)
        }
        if bearer {
            guard case .token(let token) = await SessionTokenProvider.shared.accessToken() else {
                throw ChannelVoteError.noCredential
            }
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        let session = session
        let (data, response) = try await SessionTokenProvider.shared.perform(request) {
            try await AntiCensorshipTransport.perform($0, session: session, config: .current)
        }
        return ((response as? HTTPURLResponse)?.statusCode ?? -1, data)
    }

    static func code(_ body: Data) -> String? {
        (try? JSONSerialization.jsonObject(with: body) as? [String: Any])?["code"] as? String
            ?? (try? JSONSerialization.jsonObject(with: body) as? [String: Any])?["error"] as? String
    }
}

extension ChannelsClient {
    /// ADR-0090 section 4.3 — `PUT` (vote, `labels` 1..=10) or `DELETE`
    /// (retract, no labels) `/v1/channels/{id}/posts/{post}/votes/{tag_hex}`.
    func sendPollVote(
        channelIdHex: String, channelId: Data, postCursor: UInt64, labels: [Data],
        credential: (secret: AnonCredentialSecret, credential: Data, key: AnonIssuerPublicKey)
    ) async throws {
        let sorted = labels.sorted { $0.lexicographicallyPrecedes($1) }
        let action: ChannelPollVoteAction = sorted.isEmpty ? .delete : .put
        var atMs = Int64(Date().timeIntervalSince1970 * 1000)
        for attempt in 0..<2 {
            let message = try channelPollVoteMessage(action: action, atMs: UInt64(max(0, atMs)), labels: sorted)
            let presentation = try credential.secret.present(
                credential: credential.credential, key: credential.key, channelId: channelId,
                postCursor: postCursor, message: message)
            var body: [String: Any] = [
                "presentation_b64": ChannelVoteRule.b64url(presentation.bytes), "at_ms": atMs,
            ]
            if action == .put { body["labels_hex"] = sorted.map(ChannelVoteRule.hex) }
            var request = URLRequest(url: try buildURL(
                endpoint: "/v1/channels/\(channelIdHex)/posts/\(postCursor)/votes/\(ChannelVoteRule.hex(presentation.tag))"))
            request.httpMethod = action == .put ? "PUT" : "DELETE"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
            let (data, response) = try await AntiCensorshipTransport.perform(
                request, session: anonymousSession, config: .current)
            guard let http = response as? HTTPURLResponse else { throw ChannelVoteError.network }
            if (200..<300).contains(http.statusCode) { return }
            let object = (try? JSONSerialization.jsonObject(with: data) as? [String: Any]) ?? [:]
            let code = object["code"] as? String ?? "\(http.statusCode)"
            let next = ChannelVoteRule.next(
                code: code, storedAtMs: (object["stored_at_ms"] as? NSNumber)?.int64Value,
                serverTimeMs: (object["server_time_ms"] as? NSNumber)?.int64Value,
                nowMs: Int64(Date().timeIntervalSince1970 * 1000), attempt: attempt)
            switch next {
            case .retry(let at): atMs = at
            case .fail: throw ChannelVoteError.refused(code)
            }
        }
        throw ChannelVoteError.refused("retry")
    }
}

/// A channel poll's numbers, read through the core from the service's
/// per-label counts (or a close's frozen tally).
public struct ChannelPollCounts: Equatable {
    public var counts: [UInt32]
    public var voters: UInt32
    public var frozen: Bool
}

extension PollView {
    /// A channel poll: counts from the service (mapped through the option
    /// labels), closed by a close op (its frozen tally) or by its time.
    static func channel(
        poll: SignedPoll, counts: ChannelPollCounts?, closedByOp: Bool, own: [UInt8]?, nowMs: Int64
    ) -> PollView {
        let n = poll.body.options.count
        let closed = closedByOp || (poll.body.closeAtMs.map { nowMs >= $0 } ?? false)
        return PollView(
            poll: poll, counts: counts?.counts ?? Array(repeating: 0, count: n), voters: counts?.voters ?? 0,
            closed: closed, closedBy: nil, ownChoices: own.flatMap { $0.isEmpty ? nil : $0 }, unverifiable: false,
            choicesByVoter: [], frozen: counts?.frozen ?? false)
    }
}
