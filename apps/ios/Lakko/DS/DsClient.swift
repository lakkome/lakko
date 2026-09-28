// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

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
/// ## Auth posture (ADR-0053)
///
/// A CLAIMED journal answers only a request carrying an
/// `x-lakko-ds-proof` header signed with a key the journal was claimed
/// under: a per-epoch access key every member derives from the MLS group
/// for a group journal, the identity key for a mailbox read. The caller
/// passes a ``DsAuth``; the client signs the service path, the raw query
/// and the exact body, and retries a `401` once under the previous epoch
/// and once after a claim-on-open. Writing to another account's mailbox
/// and every rendezvous mailbox stay unsigned. No session token is ever
/// sent here (invariant #5).
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
    /// ADR-0054 S3 - the knock travels on its own connection pool, unlinked
    /// from this device's signed journal and mailbox requests.
    private let anonymousSession: URLSession
    private let tokens: AnonTokenSource
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(
        baseURL: String = DsClient.defaultBaseURL,
        session: URLSession = .shared,
        tokens: AnonTokenSource = AnonTokenPool.shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.anonymousSession = AnonymousHTTP.session(pairedWith: session)
        self.tokens = tokens
        self.encoder = JSONEncoder()
        self.decoder = JSONDecoder()
    }

    // MARK: - Publish

    /// `POST /ds/mailbox/{pseudonym_hex}/messages`. Unsigned: a mailbox is
    /// open for writes (the cold inbox of ADR-0005), claimed or not.
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
            payload: payload,
            auth: nil
        )
    }

    /// `POST /ds/group/{group_id_hex}/messages`. `groupId` is not
    /// constrained to 32 bytes (MLS group ids can be longer).
    ///
    /// ADR-0053 — `auth` signs the request for a claimed journal. `nil` sends
    /// it unsigned, which only an unclaimed journal accepts.
    ///
    /// ADR-0054 decision 1 — `wake` adds the `x-lakko-wake` header, asking the
    /// DS to wake the group's subscribed devices once the row is appended.
    /// `nil` (every control envelope, and any group whose `push_gid` this
    /// device does not hold yet) sends no header and wakes nobody. The header
    /// is not part of the signed proof inputs; the DS forwards it only after
    /// the proof gate passed.
    public func publishGroup(
        groupId: Data,
        payload: Data,
        auth: DsAuth? = nil,
        wake: DsWake? = nil
    ) async throws -> PublishResult {
        guard !groupId.isEmpty else {
            throw DsClientError.argument("groupId must be non-empty")
        }
        guard !payload.isEmpty else {
            throw DsClientError.argument("payload must be non-empty")
        }
        return try await doPublish(
            endpoint: "/ds/group/\(Self.pseudonymHex(groupId))/messages",
            payload: payload,
            auth: auth,
            wake: wake
        )
    }

    private func doPublish(
        endpoint: String,
        payload: Data,
        auth: DsAuth?,
        wake: DsWake? = nil
    ) async throws -> PublishResult {
        var headers: [String: String] = [:]
        if let wake {
            headers[DsWake.headerName] = wake.headerValue
        }
        let parsed: PublishWireResponse = try await sendDecoding(
            method: "POST",
            endpoint: endpoint,
            body: payload,
            contentType: "application/octet-stream",
            extraHeaders: headers,
            auth: auth
        )
        return PublishResult(id: parsed.id, createdAt: parsed.createdAt)
    }

    // MARK: - Fetch

    /// `GET /ds/mailbox/{pseudonym_hex}/messages?...`.
    ///
    /// ADR-0053 — reading a claimed mailbox needs a proof under the identity
    /// key the mailbox is named after (`dsMailboxSigner(identity:)`). Only
    /// this device's own mailboxes are ever read signed; a rendezvous mailbox
    /// stays unclaimed and is read with `auth == nil`.
    public func fetchMailbox(
        pseudonym: Data,
        after: Int64? = nil,
        limit: Int? = nil,
        waitSecs: Int? = nil,
        auth: DsAuth? = nil
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
            waitSecs: waitSecs,
            auth: auth
        )
    }

    /// `GET /ds/group/{group_id_hex}/messages?...`.
    public func fetchGroup(
        groupId: Data,
        after: Int64? = nil,
        limit: Int? = nil,
        waitSecs: Int? = nil,
        auth: DsAuth? = nil
    ) async throws -> FetchResult {
        guard !groupId.isEmpty else {
            throw DsClientError.argument("groupId must be non-empty")
        }
        return try await doFetch(
            endpoint: "/ds/group/\(Self.pseudonymHex(groupId))/messages",
            after: after,
            limit: limit,
            waitSecs: waitSecs,
            auth: auth
        )
    }

    /// ADR-0053 — `GET /ds/group/{group_id_hex}/knocks?...`: what non-members
    /// knocked on this group. Signed exactly like a group read; the response
    /// has the shape of a journal fetch with its own id space.
    public func fetchKnocks(
        groupId: Data,
        after: Int64? = nil,
        limit: Int? = nil,
        waitSecs: Int? = nil,
        auth: DsAuth? = nil
    ) async throws -> FetchResult {
        guard !groupId.isEmpty else {
            throw DsClientError.argument("groupId must be non-empty")
        }
        return try await doFetch(
            endpoint: "/ds/group/\(Self.pseudonymHex(groupId))/knocks",
            after: after,
            limit: limit,
            waitSecs: waitSecs,
            auth: auth
        )
    }

    private func doFetch(
        endpoint: String,
        after: Int64?,
        limit: Int?,
        waitSecs: Int?,
        auth: DsAuth?
    ) async throws -> FetchResult {
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
        // Long-poll budget translates to a generous URLSession
        // timeout: the server clamps to its `MAX_WAIT_SECS`; on
        // the client side we add a few seconds of slack so the
        // round-trip + server's clamp can complete without
        // URLSession timing out first.
        var timeout: TimeInterval?
        if let waitSecs = waitSecs, waitSecs > 0 {
            timeout = TimeInterval(waitSecs + 10)
        }

        let parsed: FetchWireResponse = try await sendDecoding(
            method: "GET",
            endpoint: endpoint,
            queryItems: query,
            timeout: timeout,
            auth: auth
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

    // MARK: - ADR-0053 claim and knock

    /// `PUT /ds/group/{group_id_hex}/access` — register the access key of one
    /// epoch. `bodyJson` is the signer's `groupClaimBodyJson()` verbatim.
    ///
    /// `provenBy` signs the registration under an already-registered epoch
    /// (register-before-publish, heal-on-apply, a claim with an older signer
    /// in hand). `nil` sends it unsigned, which the server accepts for the
    /// first registration of an unclaimed journal and for re-sending a key
    /// already registered at that epoch.
    public func claimGroup(
        groupId: Data,
        bodyJson: String,
        provenBy: DsRequestSigner?
    ) async throws {
        guard !groupId.isEmpty else {
            throw DsClientError.argument("groupId must be non-empty")
        }
        _ = try await send(
            method: "PUT",
            endpoint: "/ds/group/\(Self.pseudonymHex(groupId))/access",
            body: Data(bodyJson.utf8),
            contentType: "application/json",
            auth: provenBy.map { DsAuth(signer: $0) }
        )
    }

    /// `PUT /ds/mailbox/{pseudonym_hex}/access` — claim a mailbox under the
    /// identity key it is named after. `bodyJson` is the mailbox signer's
    /// `mailboxClaimBodyJson()`. Unsigned: the key is public and the claim
    /// grants the claimant nothing.
    public func claimMailbox(pseudonym: Data, bodyJson: String) async throws {
        guard pseudonym.count == Self.pseudonymLength else {
            throw DsClientError.argument(
                "pseudonym must be \(Self.pseudonymLength) bytes"
            )
        }
        _ = try await send(
            method: "PUT",
            endpoint: "/ds/mailbox/\(Self.pseudonymHex(pseudonym))/access",
            body: Data(bodyJson.utf8),
            contentType: "application/json",
            auth: nil
        )
    }

    /// `POST /ds/group/{group_id_hex}/knock` — a non-member's request. Spends
    /// one `ds-knock` anonymous token (ADR-0054 S3), retried once with a
    /// second token on a token refusal, on the anonymous session. An empty
    /// `payloads` list is a PROBE: nothing is stored and the response carries
    /// the group's latest registered epoch; it costs a token too.
    public func knock(
        groupId: Data,
        payloads: [Data]
    ) async throws -> KnockResult {
        guard !groupId.isEmpty else {
            throw DsClientError.argument("groupId must be non-empty")
        }
        let body = try encoder.encode(
            KnockWireRequest(payloadsB64: payloads.map { base64Encode($0) })
        )
        let endpoint = "/ds/group/\(Self.pseudonymHex(groupId))/knock"
        let (data, http) = try await AnonTokenRoute.send(
            context: .dsKnock, tokens: tokens
        ) { tokenHeader in
            try await send(
                method: "POST",
                endpoint: endpoint,
                body: body,
                contentType: "application/json",
                extraHeaders: [AnonTokenRoute.headerName: tokenHeader],
                auth: nil,
                over: anonymousSession,
                acceptAnyStatus: true
            )
        }
        guard (200..<300).contains(http.statusCode) else {
            throw DsClientError.http(
                status: http.statusCode,
                endpoint: endpoint,
                body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>")
            )
        }
        let parsed: KnockWireResponse
        do {
            parsed = try decoder.decode(KnockWireResponse.self, from: data)
        } catch {
            throw DsClientError.http(
                status: http.statusCode, endpoint: endpoint, body: "decode failed: \(error)")
        }
        return KnockResult(ids: parsed.ids, epoch: parsed.epoch)
    }

    // MARK: - Internal HTTP

    /// Build the request URL and the exact `(path, query)` a proof signs.
    ///
    /// The proof covers the SERVICE path — `endpoint`, never the nginx prefix
    /// `baseURL` carries — and the query string exactly as it goes on the
    /// wire, which is `URLComponents.percentEncodedQuery` because that is what
    /// `components.url` sends. Static and pure so a test pins that the two
    /// agree byte for byte.
    static func requestTarget(
        baseURL: String,
        endpoint: String,
        queryItems: [URLQueryItem]
    ) throws -> (url: URL, path: String, query: String) {
        guard var components = URLComponents(string: baseURL + endpoint) else {
            throw DsClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        if !queryItems.isEmpty {
            components.queryItems = queryItems
        }
        guard let url = components.url else {
            throw DsClientError.argument("failed to build URL for \(endpoint)")
        }
        return (url, endpoint, components.percentEncodedQuery ?? "")
    }

    /// One request, signed when `auth` is present, with the contract-point-5
    /// retries on a `401`: once under the previous epoch's signer, once after
    /// a claim-on-open; a refusal that survives both surfaces, as the clock
    /// copy when the server's `Date` is more than a minute off.
    private func send(
        method: String,
        endpoint: String,
        queryItems: [URLQueryItem] = [],
        body: Data = Data(),
        contentType: String? = nil,
        extraHeaders: [String: String] = [:],
        timeout: TimeInterval? = nil,
        auth: DsAuth?,
        over sessionOverride: URLSession? = nil,
        acceptAnyStatus: Bool = false
    ) async throws -> (Data, HTTPURLResponse) {
        let target = try Self.requestTarget(
            baseURL: baseURL, endpoint: endpoint, queryItems: queryItems)

        func attempt(_ signer: DsRequestSigner?) async throws -> (Data, HTTPURLResponse) {
            var request = URLRequest(url: target.url)
            request.httpMethod = method
            if let contentType {
                request.setValue(contentType, forHTTPHeaderField: "Content-Type")
            }
            for (name, value) in extraHeaders {
                request.setValue(value, forHTTPHeaderField: name)
            }
            if method != "GET" {
                request.httpBody = body
            }
            if let timeout {
                request.timeoutInterval = timeout
            }
            if let signer {
                let header = signer.sign(
                    method: method,
                    path: target.path,
                    query: target.query,
                    body: method == "GET" ? Data() : body
                )
                request.setValue(header, forHTTPHeaderField: Self.proofHeaderName)
            }
            let raw: (Data, URLResponse)
            do {
                raw = try await AntiCensorshipTransport.perform(
                    request, session: sessionOverride ?? session, config: .current)
            } catch {
                throw DsClientError.network(endpoint: endpoint, underlying: error)
            }
            guard let http = raw.1 as? HTTPURLResponse else {
                throw DsClientError.http(
                    status: 0,
                    endpoint: endpoint,
                    body: "non-HTTP response"
                )
            }
            return (raw.0, http)
        }

        var result = try await attempt(auth?.signer)
        if result.1.statusCode == 401, let auth {
            for step in DsUnauthorizedRetry.plan(
                hasPrevious: auth.previous != nil, hasReclaim: auth.reclaim != nil
            ) {
                let retrySigner: DsRequestSigner?
                switch step {
                case .previousEpoch:
                    retrySigner = auth.previous
                case .reclaim:
                    retrySigner = await auth.reclaim?()
                }
                guard let retrySigner else { continue }
                result = try await attempt(retrySigner)
                if result.1.statusCode != 401 { break }
            }
        }

        let (data, http) = result
        if acceptAnyStatus { return (data, http) }
        guard (200..<300).contains(http.statusCode) else {
            if http.statusCode == 401, auth != nil,
               DsClockSkew.isSkewed(
                   serverDateHeader: http.value(forHTTPHeaderField: "Date"), now: Date()) {
                throw DsClientError.clockSkew(endpoint: endpoint)
            }
            let bodyText = Self.truncate(
                String(data: data, encoding: .utf8) ?? "<binary>"
            )
            throw DsClientError.http(
                status: http.statusCode,
                endpoint: endpoint,
                body: bodyText
            )
        }
        return (data, http)
    }

    private func sendDecoding<Res: Decodable>(
        method: String,
        endpoint: String,
        queryItems: [URLQueryItem] = [],
        body: Data = Data(),
        contentType: String? = nil,
        extraHeaders: [String: String] = [:],
        timeout: TimeInterval? = nil,
        auth: DsAuth?
    ) async throws -> Res {
        let (data, http) = try await send(
            method: method,
            endpoint: endpoint,
            queryItems: queryItems,
            body: body,
            contentType: contentType,
            extraHeaders: extraHeaders,
            timeout: timeout,
            auth: auth
        )
        do {
            return try decoder.decode(Res.self, from: data)
        } catch {
            let bodyText = Self.truncate(
                String(data: data, encoding: .utf8) ?? "<binary>"
            )
            throw DsClientError.http(
                status: http.statusCode,
                endpoint: endpoint,
                body: "decode failed: \(error) [body=\(bodyText)]"
            )
        }
    }

    /// The proof header's name, from the core (`x-lakko-ds-proof`).
    static let proofHeaderName: String = dsProofHeaderName()

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

private struct KnockWireRequest: Encodable {
    let payloadsB64: [String]

    enum CodingKeys: String, CodingKey {
        case payloadsB64 = "payloads_b64"
    }
}

private struct KnockWireResponse: Decodable {
    let ids: [Int64]
    let epoch: UInt64?
}

// MARK: - Public result types

/// Result of ``DsClient/knock(groupId:payloads:)``.
public struct KnockResult: Equatable {
    /// The ids of the stored knock rows (empty for a probe).
    public let ids: [Int64]
    /// The group's latest registered access epoch, or `nil` for an unclaimed
    /// group.
    public let epoch: UInt64?

    public init(ids: [Int64], epoch: UInt64?) {
        self.ids = ids
        self.epoch = epoch
    }
}

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
    /// ADR-0053 — a signed request was refused (`401`) after every retry, and
    /// the server's clock is more than a minute from this device's: the proof
    /// window is two minutes, so the device clock is the likely cause.
    case clockSkew(endpoint: String)

    /// The clock copy when `error` is, or wraps, ``clockSkew(endpoint:)``.
    public static func clockSkewCopy(in error: Error) -> String? {
        if let ds = error as? DsClientError, case .clockSkew = ds { return DsClockSkew.copy }
        if let chat = error as? ChatError, case .wrapped(_, let underlying) = chat {
            return clockSkewCopy(in: underlying)
        }
        return nil
    }
}

extension DsClientError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let m): return m
        case .http(let s, let e, let b): return "ds \(e) → \(s): \(b)"
        case .network(let e, let u): return "ds \(e) network error: \(u)"
        case .clockSkew: return DsClockSkew.copy
        }
    }
}
