// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// G4-2-rotation-timer-ios — one entry in the rotation
/// batch the publisher posts to
/// `POST /v1/channels/:id/rotate`. Snake-case field names
/// match the server-side `RotateBatchEntry` wire shape so
/// the default JSONEncoder produces the right body bytes.
public struct RotateBatchEntry: Encodable {
    /// Hex-encoded subscriber public key.
    public let subscriber_pubkey_hex: String
    /// Base64-encoded sealed HPKE envelope carrying the
    /// new content seed.
    public let envelope_b64: String

    public init(subscriberPubkeyHex: String, envelopeB64: String) {
        self.subscriber_pubkey_hex = subscriberPubkeyHex
        self.envelope_b64 = envelopeB64
    }
}

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
/// `<LakkoApiBaseUrl>/channels` (``ApiEndpoints``); with no API
/// base URL, `http://localhost:8090` — iOS simulator loopback +
/// the `services/channels` dev bind.
public final class ChannelsClient {

    public static var defaultBaseURL: String { ApiEndpoints.baseURL(for: .channels) }

    /// ADR-0030 — maximum entries a single `POST /rotate/stage`
    /// chunk may carry, mirroring the server-side
    /// `MAX_STAGE_CHUNK_ENTRIES`. The rotation fanout slices its
    /// sealed envelope batch into chunks of at most this many
    /// entries; a larger chunk is refused server-side with 413
    /// `chunk-too-large`.
    public static let maxStageChunkEntries: Int = 5000

    public let baseURL: String
    private let session: URLSession
    /// The anonymous session the token route (subscribe) travels on.
    private let anonymousSession: URLSession
    private let tokens: AnonTokenSource
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(
        baseURL: String = ChannelsClient.defaultBaseURL,
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

    /// The owner signer of `channelIdHex` for the account identity
    /// (ADR-0059 section 1). Only the account that created the channel
    /// derives the key the server registered for it.
    public static func ownerSigner(
        accountIdentity: Identity,
        channelIdHex: String
    ) throws -> ChannelOwnerSigner {
        guard let id = hexToData(channelIdHex), id.count == 16 else {
            throw ChannelsApiError(status: -1, code: "bad-channel-id", detail: "channel id is not 32 hex chars")
        }
        return try channelOwnerSigner(accountIdentity: accountIdentity, channelId: id)
    }

    private static func hexToData(_ hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var out = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            out.append(byte)
            index = next
        }
        return out
    }

    // MARK: - Channels

    /// `POST /v1/channels` — register a fresh channel under the owner key
    /// `accountIdentity` derives for it (ADR-0059 section 1). The same key
    /// again is idempotent; another key for an existing id is refused with
    /// 409 `channel-exists`.
    public func createChannel(channelIdHex: String, accountIdentity: Identity) async throws -> String {
        struct Req: Encodable { let channel_id: String; let owner_pubkey_hex: String }
        struct Resp: Decodable { let channel_id: String }
        let signer = try Self.ownerSigner(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
        let ownerHex = signer.verifyingKey().map { String(format: "%02x", $0) }.joined()
        let resp: Resp = try await postJson(
            endpoint: "/v1/channels",
            body: Req(channel_id: channelIdHex, owner_pubkey_hex: ownerHex)
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
    /// subscriber's X25519 public key. Spends one `channel-subscribe`
    /// anonymous token (ADR-0059 section 4) on the anonymous session; the
    /// request carries no session token.
    @discardableResult
    public func subscribe(
        channelIdHex: String,
        subscriberPubkeyHex: String
    ) async throws -> String {
        struct Req: Encodable { let subscriber_pubkey_hex: String }
        struct Resp: Decodable { let subscriber_id: String }
        let url = try buildURL(endpoint: "/v1/channels/\(channelIdHex)/subscribers")
        let body = try encoder.encode(Req(subscriber_pubkey_hex: subscriberPubkeyHex))
        let anonymousSession = self.anonymousSession
        let (data, http) = try await AnonTokenRoute.send(
            context: .channelSubscribe,
            tokens: tokens
        ) { tokenHeader in
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue(tokenHeader, forHTTPHeaderField: AnonTokenRoute.headerName)
            request.httpBody = body
            let (data, response) = try await AntiCensorshipTransport.perform(
                request, session: anonymousSession, config: .current
            )
            guard let http = response as? HTTPURLResponse else {
                throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
            }
            return (data, http)
        }
        let resp: Resp = try decode(data: data, http: http)
        return resp.subscriber_id
    }

    /// `GET /v1/channels/:id/subscribers` — list registered
    /// subscriber pubkey-hex strings. Used by the publisher
    /// to fan out HPKE-sealed seed envelopes. Owner-signed (ADR-0059
    /// section 4) with the key `accountIdentity` derives for the channel,
    /// over the service path (never the `/channels` prefix nginx strips).
    ///
    /// When the server refuses the owner proof (this account is a
    /// co-publisher, not the owner), the read is retried once with this
    /// account's co-publisher key, which the owner appointed after approving
    /// it (ADR-0059 section 1, co-publisher keys). Not appointed yet: the 401
    /// stands.
    public func listSubscribers(
        channelIdHex: String,
        accountIdentity: Identity
    ) async throws -> [String] {
        struct Resp: Decodable { let subscribers: [String] }
        let path = "/v1/channels/\(channelIdHex)/subscribers"
        let signer = try Self.ownerSigner(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
        let proof = signer.sign(method: "GET", path: path, query: "", body: Data())
        var request = URLRequest(url: try buildURL(endpoint: path))
        request.httpMethod = "GET"
        request.setValue(proof, forHTTPHeaderField: channelOwnerProofHeaderName())
        do {
            let resp: Resp = try await execute(request: request)
            return resp.subscribers
        } catch let err as ChannelsApiError where err.status == 401 {
            guard let id = Self.hexToData(channelIdHex), id.count == 16 else { throw err }
            let copub = try channelCopublisherSigner(accountIdentity: accountIdentity, channelId: id)
            var retry = URLRequest(url: try buildURL(endpoint: path))
            retry.httpMethod = "GET"
            retry.setValue(
                copub.sign(method: "GET", path: path, query: "", body: Data()),
                forHTTPHeaderField: channelCopublisherProofHeaderName())
            let resp: Resp = try await execute(request: retry)
            return resp.subscribers
        }
    }

    /// `PUT` (appoint) / `DELETE` (revoke)
    /// `/v1/channels/:id/copublishers/:key` — owner-signed (ADR-0059
    /// section 1). Idempotent both ways; `409 too-many-copublishers` past the
    /// cap. `copublisherKey` must come from a binding verified against the
    /// joiner key the owner approved, never from the server.
    public func setCoPublisherAppointment(
        appoint: Bool,
        channelIdHex: String,
        accountIdentity: Identity,
        copublisherKey: Data
    ) async throws {
        guard let id = Self.hexToData(channelIdHex), id.count == 16 else {
            throw ChannelsApiError(status: -1, code: "bad-channel-id", detail: "channel id is not 32 hex chars")
        }
        let method = appoint ? "PUT" : "DELETE"
        let path = try channelCopublisherPath(channelId: id, copublisherKey: copublisherKey)
        let signer = try Self.ownerSigner(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
        var request = URLRequest(url: try buildURL(endpoint: path))
        request.httpMethod = method
        request.setValue(
            signer.sign(method: method, path: path, query: "", body: Data()),
            forHTTPHeaderField: channelOwnerProofHeaderName())
        let (data, response) = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        if !(200..<300).contains(http.statusCode) {
            struct ErrEnvelope: Decodable { let code: String; let detail: String }
            if let body = try? decoder.decode(ErrEnvelope.self, from: data) {
                throw ChannelsApiError(status: http.statusCode, code: body.code, detail: body.detail)
            }
            throw ChannelsApiError(
                status: http.statusCode,
                code: "unknown",
                detail: HTTPURLResponse.localizedString(forStatusCode: http.statusCode))
        }
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

    /// `GET /v1/channels/:id/subscribers/:pk/envelopes` —
    /// fetch the whole RETAINED SET of seed envelopes for
    /// this subscriber (ADR-0031), ascending by epoch. A
    /// catching-up subscriber opens each `(epoch, envelope)`
    /// into its per-channel `epoch -> seed` map and decrypts
    /// every post with the seed matching its `epoch_id`.
    ///
    /// The server retains the most-recent N epochs (default
    /// N = 4); a subscriber offline for longer re-syncs to the
    /// current seed and accepts older posts as unreadable.
    ///
    /// Each entry's `envelope_b64` is base64url (no padding),
    /// decoded here to raw wire bytes. Returns an empty array
    /// when the subscriber is registered but has no envelope
    /// yet (server maps that to an empty retained set, not a
    /// 404).
    public func fetchEnvelopes(
        channelIdHex: String,
        subscriberPubkeyHex: String
    ) async throws -> [(epoch: UInt32, envelope: Data)] {
        struct Entry: Decodable {
            let epoch: UInt32
            let envelope_b64: String
        }
        struct Resp: Decodable { let envelopes: [Entry] }
        do {
            let resp: Resp = try await getJson(
                endpoint: "/v1/channels/\(channelIdHex)/subscribers/\(subscriberPubkeyHex)/envelopes"
            )
            return resp.envelopes.compactMap { entry in
                guard let wire = Data(base64URLNoPadChannels: entry.envelope_b64)
                else { return nil }
                return (epoch: entry.epoch, envelope: wire)
            }
        } catch let err as ChannelsApiError where err.code == "envelope-not-ready" {
            // Registered but nothing fanned out yet — treat as
            // an empty retained set so the caller can branch on
            // `.isEmpty` cleanly (mirrors `fetchEnvelope` → nil).
            return []
        }
    }

    /// `POST /v1/channels/:id/subscribers/:pk/envelope` —
    /// publisher delivers a per-subscriber HPKE-sealed seed
    /// envelope. The server takes raw `Bytes` — same opacity
    /// contract as the publish endpoint.
    ///
    /// `epoch` (ADR-0031) is the concrete epoch the publisher
    /// sealed this envelope for; when supplied it rides as the
    /// `?epoch=<Int>` query so the server files the envelope
    /// under that epoch (a future epoch is refused with 409
    /// `envelope-epoch-in-future`). Omitted → the server files
    /// it under the channel's current epoch.
    ///
    /// Throws `ChannelsApiError`:
    /// - `unknown-subscriber`        (404) — pubkey not registered
    /// - `envelope-epoch-in-future`  (409) — epoch > current
    /// - `bad-envelope-len`          (400) — wrong wire length
    /// - `bad-channel-id`            (400) — wrong channel-id shape
    /// - `bad-subscriber-id`         (400) — wrong pubkey-hex shape
    public func deliverEnvelope(
        channelIdHex: String,
        subscriberPubkeyHex: String,
        envelope: Data,
        epoch: UInt32? = nil
    ) async throws {
        var endpoint =
            "/v1/channels/\(channelIdHex)/subscribers/\(subscriberPubkeyHex)/envelope"
        if let epoch {
            endpoint += "?epoch=\(epoch)"
        }
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.httpBody = envelope
        let (data, response) = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
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

    /// G4-2-rotation-timer-ios — atomic batch rotation
    /// fanout. The publisher walks `listSubscribers(...)`,
    /// reseals the new MLS-exporter seed once per
    /// subscriber via the wasm-equivalent UniFFI
    /// `sealSubscriberSeedEnvelope`, then posts the batch
    /// here. The server commits or rejects the whole batch
    /// atomically (G4-4 / C-42); on success the response
    /// carries the new epoch id, which the caller records
    /// as part of the rotation state.
    ///
    /// Throws `ChannelsApiError`:
    /// - `unknown-subscriber` (404) — one of the pubkeys is
    ///   not registered (batch aborted; no envelopes
    ///   landed)
    /// - `bad-envelope-len`   (400) — wrong wire length
    /// - `bad-channel-id`     (400) — wrong channel-id shape
    /// - `bad-subscriber-id`  (400) — wrong pubkey-hex shape
    public func rotateChannel(
        channelIdHex: String,
        envelopes: [RotateBatchEntry]
    ) async throws -> UInt32 {
        struct Body: Encodable { let envelopes: [RotateBatchEntry] }
        struct EpochView: Decodable { let epoch: UInt32 }
        let body = Body(envelopes: envelopes)
        let view: EpochView = try await postJson(
            endpoint: "/v1/channels/\(channelIdHex)/rotate",
            body: body
        )
        return view.epoch
    }

    // MARK: - Staged rotation (ADR-0030)

    /// ADR-0030 (Sprint D2) — prepare phase of the chunked
    /// staged-rotation fanout. Uploads a bounded chunk of the
    /// next epoch's per-subscriber envelopes into the staging
    /// area WITHOUT bumping the live epoch or serving them to
    /// subscribers. Idempotent: re-posting a chunk is an upsert
    /// keyed on `(channel, target_epoch, subscriber)`, so a
    /// client that dropped mid-fanout simply re-sends the
    /// missing chunks (resumability).
    ///
    /// A chunk MUST hold at most
    /// ``ChannelsClient/maxStageChunkEntries`` entries — the
    /// server rejects a larger chunk with 413 `chunk-too-large`.
    /// Returns the number of envelopes accepted into the staging
    /// area by this chunk (for progress tracking).
    ///
    /// Throws `ChannelsApiError`:
    /// - `chunk-too-large`    (413) — over the entry cap
    /// - `stale-target-epoch` (409) — target_epoch != current + 1
    /// - `unknown-subscriber` (409) — a pubkey is not registered
    /// - `unknown-channel`    (404) — no such channel
    /// - `bad-envelope-len`   (400) — wrong wire length
    /// - `bad-channel-id`     (400) — wrong channel-id shape
    @discardableResult
    public func stageRotation(
        channelIdHex: String,
        targetEpoch: UInt32,
        batch: [RotateBatchEntry]
    ) async throws -> Int {
        struct Body: Encodable {
            let target_epoch: UInt32
            let envelopes: [RotateBatchEntry]
        }
        struct StageView: Decodable {
            let target_epoch: UInt32
            let staged: Int
        }
        let view: StageView = try await postJson(
            endpoint: "/v1/channels/\(channelIdHex)/rotate/stage",
            body: Body(target_epoch: targetEpoch, envelopes: batch)
        )
        return view.staged
    }

    /// ADR-0030 (Sprint D2) — commit phase, the single atomic
    /// point. Promotes every envelope staged for `targetEpoch`
    /// into the live set + bumps the epoch in ONE server
    /// transaction (C-42 preserved: a subscriber sees the old
    /// epoch until commit runs, then the new epoch with every
    /// staged envelope already live — never a torn rotation).
    /// The server re-verifies `targetEpoch == current + 1`, so a
    /// replayed / out-of-order commit is refused. Returns the new
    /// live epoch after the promotion.
    ///
    /// Throws `ChannelsApiError`:
    /// - `stale-target-epoch` (409) — target_epoch != current + 1
    /// - `unknown-channel`    (404) — no such channel
    /// - `bad-channel-id`     (400) — wrong channel-id shape
    public func commitRotation(
        channelIdHex: String,
        targetEpoch: UInt32
    ) async throws -> UInt32 {
        struct Body: Encodable { let target_epoch: UInt32 }
        struct EpochView: Decodable { let epoch: UInt32 }
        let view: EpochView = try await postJson(
            endpoint: "/v1/channels/\(channelIdHex)/rotate/commit",
            body: Body(target_epoch: targetEpoch)
        )
        return view.epoch
    }

    // MARK: - ADR-0059 — the directory (Discover)

    /// Whether this account holds the OWNER key of `channelIdHex` (not a
    /// co-publisher's): an owner-signed read of the subscriber list is
    /// accepted only under the key registered at creation. `nil` when the
    /// server could not answer (network, a 5xx).
    public func probeChannelOwner(channelIdHex: String, accountIdentity: Identity) async -> Bool? {
        struct Resp: Decodable { let subscribers: [String] }
        let path = "/v1/channels/\(channelIdHex)/subscribers"
        do {
            let signer = try Self.ownerSigner(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
            var request = URLRequest(url: try buildURL(endpoint: path))
            request.httpMethod = "GET"
            request.setValue(
                signer.sign(method: "GET", path: path, query: "", body: Data()),
                forHTTPHeaderField: channelOwnerProofHeaderName())
            let _: Resp = try await execute(request: request)
            return true
        } catch let err as ChannelsApiError where err.status == 401 {
            return false
        } catch {
            return nil
        }
    }

    /// `PUT /v1/channels/:id/listing` — store or replace the channel's
    /// listing (owner-signed). `listing` is the canonical
    /// `composeChannelListing` bytes.
    public func putListing(channelIdHex: String, accountIdentity: Identity, listing: Data) async throws {
        try await ownerSignedWrite(
            method: "PUT", path: "/v1/channels/\(channelIdHex)/listing",
            channelIdHex: channelIdHex, accountIdentity: accountIdentity, body: listing)
    }

    /// `DELETE /v1/channels/:id/listing` — withdraw the listing
    /// (owner-signed, idempotent).
    public func deleteListing(channelIdHex: String, accountIdentity: Identity) async throws {
        try await ownerSignedWrite(
            method: "DELETE", path: "/v1/channels/\(channelIdHex)/listing",
            channelIdHex: channelIdHex, accountIdentity: accountIdentity, body: Data())
    }

    private func ownerSignedWrite(
        method: String, path: String, channelIdHex: String, accountIdentity: Identity, body: Data
    ) async throws {
        let signer = try Self.ownerSigner(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
        var request = URLRequest(url: try buildURL(endpoint: path))
        request.httpMethod = method
        request.setValue(
            signer.sign(method: method, path: path, query: "", body: body),
            forHTTPHeaderField: channelOwnerProofHeaderName())
        if !body.isEmpty {
            request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
            request.httpBody = body
        }
        let (data, response) = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        try Self.throwIfFailed(data: data, http: http, decoder: decoder)
    }

    private static func throwIfFailed(data: Data, http: HTTPURLResponse, decoder: JSONDecoder) throws {
        guard !(200..<300).contains(http.statusCode) else { return }
        if let body = try? decoder.decode(ChannelsErrEnvelope.self, from: data) {
            throw ChannelsApiError(status: http.statusCode, code: body.code, detail: body.detail)
        }
        throw ChannelsApiError(
            status: http.statusCode, code: "unknown",
            detail: HTTPURLResponse.localizedString(forStatusCode: http.statusCode))
    }

    /// An anonymous, cacheable directory read: no session, no token, on the
    /// anonymous connection pool (the server answers every caller the same,
    /// ADR-0059 section 5).
    private func anonymousGet<R: Decodable>(endpoint: String) async throws -> R {
        var request = URLRequest(url: try buildURL(endpoint: endpoint))
        request.httpMethod = "GET"
        let (data, response) = try await AntiCensorshipTransport.perform(
            request, session: anonymousSession, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        return try decode(data: data, http: http)
    }

    /// `GET /v1/directory/listing/:id` — one channel's listing, or `nil`
    /// when it is not listed (404). Tells the server someone looked at this
    /// channel, as any card view does.
    func fetchListing(channelIdHex: String) async throws -> DirectoryItem? {
        do {
            let item: DirectoryItem = try await anonymousGet(endpoint: "/v1/directory/listing/\(channelIdHex)")
            return item
        } catch let err as ChannelsApiError where err.status == 404 {
            return nil
        }
    }

    /// `GET /v1/directory/bucket/:topic/:language` — the top listings of one
    /// bucket. No session, no token.
    func fetchBucket(topic: Int, language: String) async throws -> [DirectoryItem] {
        struct Resp: Decodable { let items: [DirectoryItem]? }
        let resp: Resp = try await anonymousGet(endpoint: "/v1/directory/bucket/\(topic)/\(language)")
        return resp.items ?? []
    }

    /// `GET /v1/directory/search?q=` — one `directory-search` token; the
    /// query is sent to the server (the UI says so).
    func searchDirectory(query: String) async throws -> [DirectoryItem] {
        struct Resp: Decodable { let items: [DirectoryItem]? }
        var components = URLComponents()
        components.queryItems = [URLQueryItem(name: "q", value: query)]
        let encoded = (components.percentEncodedQuery ?? "").replacingOccurrences(of: "+", with: "%2B")
        let url = try buildURL(endpoint: "/v1/directory/search?\(encoded)")
        let anonymousSession = self.anonymousSession
        let (data, http) = try await AnonTokenRoute.send(context: .directorySearch, tokens: tokens) { header in
            var request = URLRequest(url: url)
            request.httpMethod = "GET"
            request.setValue(header, forHTTPHeaderField: AnonTokenRoute.headerName)
            let (data, response) = try await AntiCensorshipTransport.perform(
                request, session: anonymousSession, config: .current)
            guard let http = response as? HTTPURLResponse else {
                throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
            }
            return (data, http)
        }
        let resp: Resp = try decode(data: data, http: http)
        return resp.items ?? []
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
        let (data, response) = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(
                status: -1,
                code: "no-http-response",
                detail: "non-HTTP response"
            )
        }
        return try decode(data: data, http: http)
    }

    private func decode<R: Decodable>(data: Data, http: HTTPURLResponse) throws -> R {
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

private extension Data {
    /// Decode a base64url-no-pad string (the
    /// `envelope_b64` wire form the plural `/envelopes`
    /// endpoint ships) into raw `Data`. Returns `nil` on any
    /// parse failure so the caller can skip a malformed entry
    /// rather than fail the whole retained set.
    init?(base64URLNoPadChannels s: String) {
        var std = s
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let pad = (4 - (std.count % 4)) % 4
        std.append(String(repeating: "=", count: pad))
        guard let data = Data(base64Encoded: std) else { return nil }
        self = data
    }
}
