// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// URLSession-based client for `lakko-svc-channels`
/// (Phase 4 G4-2 iOS).
///
/// Mirror of the web side in
/// `apps/web/src/channels/api.ts` — same endpoints, same
/// stable error codes, JS-camelCase ↔ Swift-camelCase
/// names line up trivially.
///
/// ## Default base URL
///
/// `http://localhost:8090` — iOS simulator loopback + the
/// `services/channels` dev bind. Override on physical
/// devices (LAN IP) and in production (Cloudflare tunnel).
public final class ChannelsClient {

    public static let defaultBaseURL: String = "http://localhost:8090"

    public let baseURL: String
    private let session: URLSession
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(
        baseURL: String = ChannelsClient.defaultBaseURL,
        session: URLSession = .shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.encoder = JSONEncoder()
        self.decoder = JSONDecoder()
    }

    // MARK: - Channels

    /// `POST /v1/channels` — register a fresh channel.
    public func createChannel(channelIdHex: String) async throws -> String {
        struct Req: Encodable { let channel_id: String }
        struct Resp: Decodable { let channel_id: String }
        let resp: Resp = try await postJson(
            endpoint: "/v1/channels",
            body: Req(channel_id: channelIdHex)
        )
        return resp.channel_id
    }

    // MARK: - Posts

    /// `POST /v1/channels/:id/posts` — publish opaque
    /// ChannelPost bytes.
    public func publishPost(
        channelIdHex: String,
        payload: Data
    ) async throws -> PostView {
        struct Resp: Decodable {
            let cursor: UInt64
            let server_ts_ms: UInt64
            let payload_b64: String
        }
        let resp: Resp = try await postOctet(
            endpoint: "/v1/channels/\(channelIdHex)/posts",
            payload: payload
        )
        return PostView(
            cursor: resp.cursor,
            serverTsMs: resp.server_ts_ms,
            payloadB64: resp.payload_b64
        )
    }

    /// `GET /v1/channels/:id/posts?after=…&limit=…` —
    /// cursor-paginated fetch.
    public func fetchPosts(
        channelIdHex: String,
        after: UInt64,
        limit: Int? = nil
    ) async throws -> FetchPostsResult {
        struct Resp: Decodable {
            struct P: Decodable {
                let cursor: UInt64
                let server_ts_ms: UInt64
                let payload_b64: String
            }
            let posts: [P]
            let next_after: UInt64
        }
        var query = "?after=\(after)"
        if let limit { query += "&limit=\(limit)" }
        let resp: Resp = try await getJson(
            endpoint: "/v1/channels/\(channelIdHex)/posts" + query
        )
        return FetchPostsResult(
            posts: resp.posts.map {
                PostView(
                    cursor: $0.cursor,
                    serverTsMs: $0.server_ts_ms,
                    payloadB64: $0.payload_b64
                )
            },
            nextAfter: resp.next_after
        )
    }

    /// `GET /v1/channels/:id/epoch`.
    public func fetchEpoch(channelIdHex: String) async throws -> UInt32 {
        struct Resp: Decodable { let epoch: UInt32 }
        let resp: Resp = try await getJson(
            endpoint: "/v1/channels/\(channelIdHex)/epoch"
        )
        return resp.epoch
    }

    // MARK: - Subscribers (G4-3c)

    /// `POST /v1/channels/:id/subscribers` — register a
    /// subscriber's X25519 public key.
    @discardableResult
    public func subscribe(
        channelIdHex: String,
        subscriberPubkeyHex: String
    ) async throws -> String {
        struct Req: Encodable { let subscriber_pubkey_hex: String }
        struct Resp: Decodable { let subscriber_id: String }
        let resp: Resp = try await postJson(
            endpoint: "/v1/channels/\(channelIdHex)/subscribers",
            body: Req(subscriber_pubkey_hex: subscriberPubkeyHex)
        )
        return resp.subscriber_id
    }

    /// `GET /v1/channels/:id/subscribers` — list registered
    /// subscriber pubkey-hex strings. Used by the publisher
    /// to fan out HPKE-sealed seed envelopes.
    public func listSubscribers(
        channelIdHex: String
    ) async throws -> [String] {
        struct Resp: Decodable { let subscribers: [String] }
        let resp: Resp = try await getJson(
            endpoint: "/v1/channels/\(channelIdHex)/subscribers"
        )
        return resp.subscribers
    }

    /// `GET /v1/channels/:id/subscribers/:pk/envelope` —
    /// fetch the latest seed envelope delivered to this
    /// subscriber. Returns `nil` when registered but no
    /// envelope yet (server returns 404
    /// `envelope-not-ready` which we map to nil so the
    /// caller can branch cleanly).
    public func fetchEnvelope(
        channelIdHex: String,
        subscriberPubkeyHex: String
    ) async throws -> String? {
        struct Resp: Decodable { let envelope_b64: String }
        do {
            let resp: Resp = try await getJson(
                endpoint: "/v1/channels/\(channelIdHex)/subscribers/\(subscriberPubkeyHex)/envelope"
            )
            return resp.envelope_b64
        } catch let err as ChannelsApiError where err.code == "envelope-not-ready" {
            return nil
        }
    }

    /// `POST /v1/channels/:id/subscribers/:pk/envelope` —
    /// publisher delivers a per-subscriber HPKE-sealed seed
    /// envelope. The server takes raw `Bytes` — same opacity
    /// contract as the publish endpoint.
    ///
    /// Throws `ChannelsApiError`:
    /// - `unknown-subscriber` (404) — pubkey not registered
    /// - `bad-envelope-len`   (400) — wrong wire length
    /// - `bad-channel-id`     (400) — wrong channel-id shape
    /// - `bad-subscriber-id`  (400) — wrong pubkey-hex shape
    public func deliverEnvelope(
        channelIdHex: String,
        subscriberPubkeyHex: String,
        envelope: Data
    ) async throws {
        let url = try buildURL(
            endpoint: "/v1/channels/\(channelIdHex)/subscribers/\(subscriberPubkeyHex)/envelope"
        )
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.httpBody = envelope
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(
                status: -1,
                code: "no-http-response",
                detail: "non-HTTP response"
            )
        }
        if !(200..<300).contains(http.statusCode) {
            struct ErrEnvelope: Decodable { let code: String; let detail: String }
            if let body = try? decoder.decode(ErrEnvelope.self, from: data) {
                throw ChannelsApiError(
                    status: http.statusCode,
                    code: body.code,
                    detail: body.detail
                )
            }
            throw ChannelsApiError(
                status: http.statusCode,
                code: "unknown",
                detail: HTTPURLResponse.localizedString(forStatusCode: http.statusCode)
            )
        }
    }

    // MARK: - Internals

    private func postJson<Body: Encodable, R: Decodable>(
        endpoint: String,
        body: Body
    ) async throws -> R {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try encoder.encode(body)
        return try await execute(request: request)
    }

    private func postOctet<R: Decodable>(
        endpoint: String,
        payload: Data
    ) async throws -> R {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.httpBody = payload
        return try await execute(request: request)
    }

    private func getJson<R: Decodable>(endpoint: String) async throws -> R {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        return try await execute(request: request)
    }

    private func execute<R: Decodable>(request: URLRequest) async throws -> R {
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(
                status: -1,
                code: "no-http-response",
                detail: "non-HTTP response"
            )
        }
        if !(200..<300).contains(http.statusCode) {
            // Server promises {code, detail} on every 4xx/5xx.
            // The decoded shape lives on the type
            // (`ChannelsErrEnvelope` below the class) rather
            // than nested here — Swift refuses to nest a
            // type inside a generic function.
            if let body = try? decoder.decode(ChannelsErrEnvelope.self, from: data) {
                throw ChannelsApiError(
                    status: http.statusCode,
                    code: body.code,
                    detail: body.detail
                )
            }
            throw ChannelsApiError(
                status: http.statusCode,
                code: "unknown",
                detail: HTTPURLResponse.localizedString(forStatusCode: http.statusCode)
            )
        }
        return try decoder.decode(R.self, from: data)
    }

    private func buildURL(endpoint: String) throws -> URL {
        guard let url = URL(string: baseURL + endpoint) else {
            throw ChannelsApiError(
                status: -1,
                code: "bad-url",
                detail: "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        return url
    }
}

// MARK: - Wire types

/// One post in the cursor-paginated fetch response.
public struct PostView: Equatable, Sendable {
    public let cursor: UInt64
    public let serverTsMs: UInt64
    /// Opaque ChannelPost bytes, base64url-encoded.
    public let payloadB64: String
}

/// Response shape from
/// ``ChannelsClient/fetchPosts(channelIdHex:after:limit:)``.
public struct FetchPostsResult: Sendable {
    public let posts: [PostView]
    public let nextAfter: UInt64
}

/// Server-supplied error envelope — mirrors the
/// `{code, detail}` JSON the channels service returns on
/// every 4xx/5xx. The `code` field is stable across
/// releases; UI branches on it, not on prose.
public struct ChannelsApiError: Error, Equatable, Sendable {
    public let status: Int
    public let code: String
    public let detail: String
}

/// Internal decode target for the `{code, detail}` JSON the
/// channels service ships on every 4xx/5xx. Lives at file
/// scope rather than nested inside the generic `execute`
/// method — Swift disallows nesting types inside generic
/// functions, and pulling it here is the canonical fix.
fileprivate struct ChannelsErrEnvelope: Decodable {
    let code: String
    let detail: String
}
