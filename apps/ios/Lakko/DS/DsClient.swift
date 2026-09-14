// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// URLSession-based client for `lakko-svc-ds`.
///
/// Mirror of:
///
///   * `apps/web/src/ds/client.ts` (1.17e4)
///   * `apps/android/.../ds/DsClient.kt` (1.15e4)
///
/// The Delivery Service is a thin append-only relay: every message
/// goes into either a per-group journal (`/ds/group/{group_hex}`)
/// or a per-mailbox journal (`/ds/mailbox/{device_hex}`). Both
/// paths key the journal by a 32-byte pseudonym encoded as
/// **lowercase hex** in the URL.
///
/// Two operations per journal kind:
///
///   * `POST /ds/{kind}/{pseudonym_hex}/messages` — body is raw
///     bytes (`application/octet-stream`), server caps size at its
///     `MAX_MESSAGE_BYTES`. Response is the assigned monotonic
///     `id` + ISO-8601 `createdAt` — the `id` becomes the `after=`
///     cursor for the next GET.
///
///   * `GET /ds/{kind}/{pseudonym_hex}/messages?after=&limit=&wait=`
///     — pull all messages newer than `after`. `wait` is the
///     long-poll budget in seconds (0 returns immediately whatever's
///     there; non-zero holds the connection until at least one row
///     arrives or the server's `MAX_WAIT_SECS` clamp kicks in).
///     Response is a JSON envelope carrying the rows as
///     `payload_b64` strings; we decode to raw bytes before handing
///     back.
///
/// ## Auth posture
///
/// The DS does NOT require PoW or auth tokens in Phase 1 — every
/// write is open to anyone who can reach the URL. Production
/// deployments gate writes on auth-issued tokens (Phase 1.5b+);
/// the client signature stays the same.
///
/// ## Default base URL
///
/// `<LakkoApiBaseUrl>/ds` (``ApiEndpoints``); with no API base URL,
/// `http://localhost:8082` — iOS simulator loopback + port `8082`
/// matching `lakko-svc-ds`'s dev bind.
public final class DsClient {

    public static var defaultBaseURL: String { ApiEndpoints.baseURL(for: .ds) }

    public let baseURL: String
    private let session: URLSession
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(
        baseURL: String = DsClient.defaultBaseURL,
        session: URLSession = .shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.encoder = JSONEncoder()
        self.decoder = JSONDecoder()
    }

    // MARK: - Publish

    /// `POST /ds/mailbox/{pseudonym_hex}/messages`.
    public func publishMailbox(
        pseudonym: Data,
        payload: Data
    ) async throws -> PublishResult {
        guard pseudonym.count == Self.pseudonymLength else {
            throw DsClientError.argument(
                "pseudonym must be \(Self.pseudonymLength) bytes"
            )
        }
        guard !payload.isEmpty else {
            throw DsClientError.argument("payload must be non-empty")
        }
        return try await doPublish(
            endpoint: "/ds/mailbox/\(Self.pseudonymHex(pseudonym))/messages",
            payload: payload
        )
    }

    /// `POST /ds/group/{group_id_hex}/messages`. `groupId` is not
    /// constrained to 32 bytes (MLS group ids can be longer).
    public func publishGroup(
        groupId: Data,
        payload: Data
    ) async throws -> PublishResult {
        guard !groupId.isEmpty else {
            throw DsClientError.argument("groupId must be non-empty")
        }
        guard !payload.isEmpty else {
            throw DsClientError.argument("payload must be non-empty")
        }
        return try await doPublish(
            endpoint: "/ds/group/\(Self.pseudonymHex(groupId))/messages",
            payload: payload
        )
    }

    private func doPublish(
        endpoint: String,
        payload: Data
    ) async throws -> PublishResult {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.httpBody = payload
        let parsed: PublishWireResponse = try await execute(
            endpoint: endpoint,
            request: request
        )
        return PublishResult(id: parsed.id, createdAt: parsed.createdAt)
    }

    // MARK: - Fetch

    /// `GET /ds/mailbox/{pseudonym_hex}/messages?...`.
    public func fetchMailbox(
        pseudonym: Data,
        after: Int64? = nil,
        limit: Int? = nil,
        waitSecs: Int? = nil
    ) async throws -> FetchResult {
        guard pseudonym.count == Self.pseudonymLength else {
            throw DsClientError.argument(
                "pseudonym must be \(Self.pseudonymLength) bytes"
            )
        }
        return try await doFetch(
            endpoint: "/ds/mailbox/\(Self.pseudonymHex(pseudonym))/messages",
            after: after,
            limit: limit,
            waitSecs: waitSecs
        )
    }

    /// `GET /ds/group/{group_id_hex}/messages?...`.
    public func fetchGroup(
        groupId: Data,
        after: Int64? = nil,
        limit: Int? = nil,
        waitSecs: Int? = nil
    ) async throws -> FetchResult {
        guard !groupId.isEmpty else {
            throw DsClientError.argument("groupId must be non-empty")
        }
        return try await doFetch(
            endpoint: "/ds/group/\(Self.pseudonymHex(groupId))/messages",
            after: after,
            limit: limit,
            waitSecs: waitSecs
        )
    }

    private func doFetch(
        endpoint: String,
        after: Int64?,
        limit: Int?,
        waitSecs: Int?
    ) async throws -> FetchResult {
        guard var components = URLComponents(string: baseURL + endpoint) else {
            throw DsClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        var query: [URLQueryItem] = []
        if let after = after {
            query.append(URLQueryItem(name: "after", value: String(after)))
        }
        if let limit = limit {
            query.append(URLQueryItem(name: "limit", value: String(limit)))
        }
        if let waitSecs = waitSecs {
            query.append(URLQueryItem(name: "wait", value: String(waitSecs)))
        }
        if !query.isEmpty {
            components.queryItems = query
        }
        guard let url = components.url else {
            throw DsClientError.argument("failed to build URL for \(endpoint)")
        }

        var request = URLRequest(url: url)
        request.httpMethod = "GET"

        // Long-poll budget translates to a generous URLSession
        // timeout: the server clamps to its `MAX_WAIT_SECS`; on
        // the client side we add a few seconds of slack so the
        // round-trip + server's clamp can complete without
        // URLSession timing out first.
        if let waitSecs = waitSecs, waitSecs > 0 {
            request.timeoutInterval = TimeInterval(waitSecs + 10)
        }

        let parsed: FetchWireResponse = try await execute(
            endpoint: endpoint,
            request: request
        )
        let messages: [MailboxMessage] = parsed.messages.compactMap { msg in
            guard let payload = base64Decode(msg.payloadB64) else {
                return nil
            }
            return MailboxMessage(
                id: msg.id,
                payload: payload,
                createdAt: msg.createdAt
            )
        }
        return FetchResult(messages: messages, hasMore: parsed.hasMore)
    }

    // MARK: - Internal HTTP

    private func buildURL(endpoint: String) throws -> URL {
        guard let url = URL(string: baseURL + endpoint) else {
            throw DsClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        return url
    }

    private func execute<Res: Decodable>(
        endpoint: String,
        request: URLRequest
    ) async throws -> Res {
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        } catch {
            throw DsClientError.network(endpoint: endpoint, underlying: error)
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw DsClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            let bodyText = Self.truncate(
                String(data: data, encoding: .utf8) ?? "<binary>"
            )
            throw DsClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: bodyText
            )
        }
        do {
            return try decoder.decode(Res.self, from: data)
        } catch {
            let bodyText = Self.truncate(
                String(data: data, encoding: .utf8) ?? "<binary>"
            )
            throw DsClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: "decode failed: \(error) [body=\(bodyText)]"
            )
        }
    }

    private static func truncate(_ s: String, max: Int = 512) -> String {
        if s.count > max {
            return String(s.prefix(max)) + "…"
        }
        return s
    }

    // MARK: - Constants + helpers

    /// 32-byte pseudonym (BLAKE3 output length used by every
    /// variant in `apps/ios/Lakko/Contacts/Pseudonyms.swift`).
    public static let pseudonymLength: Int = 32

    /// Lowercase-hex encode a byte buffer (no `0x` prefix). Public
    /// because it's a useful helper for tests + for any caller
    /// assembling DS URLs directly without going through this
    /// client.
    public static func pseudonymHex(_ bytes: Data) -> String {
        var out = String()
        out.reserveCapacity(bytes.count * 2)
        for b in bytes {
            let hi = Int(b >> 4) & 0x0F
            let lo = Int(b) & 0x0F
            out.append(Self.hexAlphabet[hi])
            out.append(Self.hexAlphabet[lo])
        }
        return out
    }

    private static let hexAlphabet: [Character] = Array("0123456789abcdef")
}

// MARK: - Wire shapes (internal)

private struct PublishWireResponse: Decodable {
    let id: Int64
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case createdAt = "created_at"
    }
}

private struct FetchWireResponse: Decodable {
    let messages: [FetchWireMessage]
    let hasMore: Bool

    enum CodingKeys: String, CodingKey {
        case messages
        case hasMore = "has_more"
    }
}

private struct FetchWireMessage: Decodable {
    let id: Int64
    let payloadB64: String
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case payloadB64 = "payload_b64"
        case createdAt = "created_at"
    }
}

// MARK: - Public result types

/// Result of ``DsClient/publishMailbox(pseudonym:payload:)`` /
/// ``DsClient/publishGroup(groupId:payload:)``.
public struct PublishResult: Equatable {
    /// Server-assigned monotonic id. Use as the `after` cursor on
    /// subsequent fetches.
    public let id: Int64
    /// ISO-8601 timestamp from the server.
    public let createdAt: String

    public init(id: Int64, createdAt: String) {
        self.id = id
        self.createdAt = createdAt
    }
}

/// Decoded message from a fetch cursor.
public struct MailboxMessage: Equatable {
    public let id: Int64
    /// Raw payload bytes (decoded from the server's `payload_b64`).
    public let payload: Data
    public let createdAt: String

    public init(id: Int64, payload: Data, createdAt: String) {
        self.id = id
        self.payload = payload
        self.createdAt = createdAt
    }
}

/// Result of ``DsClient/fetchMailbox(pseudonym:after:limit:waitSecs:)`` /
/// ``DsClient/fetchGroup(groupId:after:limit:waitSecs:)``.
public struct FetchResult: Equatable {
    public let messages: [MailboxMessage]
    /// `true` if there are more rows past the last returned id;
    /// caller should call again with `after = lastMessage.id`.
    public let hasMore: Bool

    public init(messages: [MailboxMessage], hasMore: Bool) {
        self.messages = messages
        self.hasMore = hasMore
    }
}

// MARK: - Errors

public enum DsClientError: Error {
    case argument(String)
    case http(status: Int, endpoint: String, body: String)
    case network(endpoint: String, underlying: Error)
}

extension DsClientError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let m): return m
        case .http(let s, let e, let b): return "ds \(e) → \(s): \(b)"
        case .network(let e, let u): return "ds \(e) network error: \(u)"
        }
    }
}
