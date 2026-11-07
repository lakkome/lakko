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
/// base URL, `http://localhost:8086` — iOS simulator loopback +
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
    let anonymousSession: URLSession
    let tokens: AnonTokenSource
    let encoder: JSONEncoder
    private let decoder: JSONDecoder
    /// How a request on the session reaches the server (the anti-censorship
    /// tunnel when armed). A seam so tests can see the exact request sent.
    let transport: ChannelsTransport
    /// ADR-0059 section 9 — the publisher role per (account, channel).
    let roles: ChannelPublisherRoleCache

    public convenience init(
        baseURL: String = ChannelsClient.defaultBaseURL,
        session: URLSession = .shared,
        tokens: AnonTokenSource = AnonTokenPool.shared
    ) {
        self.init(
            baseURL: baseURL,
            session: session,
            tokens: tokens,
            roles: .shared,
            transport: { request in
                try await AntiCensorshipTransport.perform(request, session: session, config: .current)
            }
        )
    }

    /// The full initializer: tests pass their own transport and role cache.
    init(
        baseURL: String,
        session: URLSession = .shared,
        tokens: AnonTokenSource = AnonTokenPool.shared,
        roles: ChannelPublisherRoleCache,
        transport: @escaping ChannelsTransport
    ) {
        self.baseURL = baseURL
        self.session = session
        self.anonymousSession = AnonymousHTTP.session(pairedWith: session)
        self.tokens = tokens
        self.encoder = JSONEncoder()
        self.decoder = JSONDecoder()
        self.roles = roles
        self.transport = transport
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

    /// Record the role this account holds for `channelIdHex` (on creation,
    /// or once a co-publisher join completed), so its next signed request
    /// goes out once under the right key (ADR-0059 section 9).
    public static func rememberPublisherRole(
        channelIdHex: String,
        accountIdentity: Identity,
        role: ChannelPublisherRole
    ) throws {
        let key = try IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
            .roleKey()
        ChannelPublisherRoleCache.shared.set(role, forKey: key)
    }

    static func hexToData(_ hex: String) -> Data? {
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
    public func createChannel(
        channelIdHex: String, accountIdentity: Identity, isPrivate: Bool = false
    ) async throws -> String {
        struct Req: Encodable { let channel_id: String; let owner_pubkey_hex: String; let `private`: Bool }
        struct Resp: Decodable { let channel_id: String }
        let signer = try Self.ownerSigner(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
        let ownerHex = signer.verifyingKey().map { String(format: "%02x", $0) }.joined()
        let resp: Resp = try await postJson(
            endpoint: "/v1/channels",
            body: Req(channel_id: channelIdHex, owner_pubkey_hex: ownerHex, private: isPrivate)
        )
        roles.set(.owner, forKey: ownerHex)
        return resp.channel_id
    }

    // MARK: - Posts

    /// `POST /v1/channels/:id/posts` — publish opaque
    /// ChannelPost bytes, signed with this account's publisher role for the
    /// channel (ADR-0059 section 9) over exactly those bytes.
    public func publishPost(
        channelIdHex: String,
        payload: Data,
        accountIdentity: Identity
    ) async throws -> PostView {
        struct Resp: Decodable {
            let cursor: UInt64
            let server_ts_ms: UInt64
            let payload_b64: String
        }
        let resp: Resp = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "POST",
            path: "/v1/channels/\(channelIdHex)/posts",
            query: "",
            body: payload,
            contentType: "application/octet-stream"
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
    /// to fan out HPKE-sealed seed envelopes. Signed (ADR-0059 sections 4
    /// and 9) with the role this account holds for the channel: the owner
    /// key when it created it, its co-publisher key (which the owner
    /// appointed after approving it) otherwise. With the role not yet
    /// learned this session, the owner proof goes first and a 401 is retried
    /// once with the co-publisher proof; the accepted role is remembered for
    /// every later signed request. Not appointed yet: the 401 stands.
    public func listSubscribers(
        channelIdHex: String,
        accountIdentity: Identity
    ) async throws -> [String] {
        struct Resp: Decodable { let subscribers: [String] }
        let resp: Resp = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "GET",
            path: "/v1/channels/\(channelIdHex)/subscribers",
            query: "",
            body: Data(),
            contentType: nil
        )
        return resp.subscribers
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

    /// `DELETE /v1/channels/:id` (ADR-0087 phase B) — the owner deletes the
    /// channel: owner proof over an empty body, `204` (also on a repeat).
    /// Afterwards the service answers `410 channel-deleted` for it.
    public func deleteChannel(channelIdHex: String, accountIdentity: Identity) async throws {
        let path = "/v1/channels/\(channelIdHex)"
        let signer = try Self.ownerSigner(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
        var request = URLRequest(url: try buildURL(endpoint: path))
        request.httpMethod = "DELETE"
        request.setValue(
            signer.sign(method: "DELETE", path: path, query: "", body: Data()),
            forHTTPHeaderField: channelOwnerProofHeaderName())
        let (data, response) = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        guard (200..<300).contains(http.statusCode) || http.statusCode == 410 else {
            struct ErrEnvelope: Decodable { let code: String; let detail: String }
            if let body = try? decoder.decode(ErrEnvelope.self, from: data) {
                throw ChannelsApiError(status: http.statusCode, code: body.code, detail: body.detail)
            }
            throw ChannelsApiError(
                status: http.statusCode, code: "unknown",
                detail: HTTPURLResponse.localizedString(forStatusCode: http.statusCode))
        }
    }

    /// `PUT /v1/channels/{id}/owner` (ADR-0074 section 3.5, "Channels") —
    /// replace the channel's owner key after a rotation of the account keys.
    /// The body is the NEW owner key (32 raw bytes: the verifying key the new
    /// account identity derives for the channel); the authorisation is the
    /// existing owner request proof, signed by `signingIdentity`'s owner key
    /// over the method, the path and the body.
    ///
    /// `204`: the key was replaced, or `signingIdentity` already owns the
    /// channel under the key named (which is how the new owner asks whether
    /// its handover landed). Throws ``ChannelsApiError``: `401
    /// owner-proof-required` (the signer is not the owner), `400
    /// bad-owner-pubkey`, `409 owner-changed`.
    public func replaceOwner(
        channelIdHex: String,
        signingIdentity: Identity,
        newOwnerIdentity: Identity
    ) async throws {
        guard let id = Self.hexToData(channelIdHex), id.count == 16 else {
            throw ChannelsApiError(status: -1, code: "bad-channel-id", detail: "channel id is not 32 hex chars")
        }
        let path = "/v1/channels/\(channelIdHex)/owner"
        let signer = try Self.ownerSigner(accountIdentity: signingIdentity, channelIdHex: channelIdHex)
        let body = try Self.ownerSigner(accountIdentity: newOwnerIdentity, channelIdHex: channelIdHex)
            .verifyingKey()
        var request = URLRequest(url: try buildURL(endpoint: path))
        request.httpMethod = "PUT"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.setValue(
            signer.sign(method: "PUT", path: path, query: "", body: body),
            forHTTPHeaderField: channelOwnerProofHeaderName())
        request.httpBody = body
        let (data, response) = try await transport(request)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        let _: EmptyResponse = try decode(data: data, http: http)
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
        epoch: UInt32? = nil,
        accountIdentity: Identity
    ) async throws {
        // Publisher-signed (ADR-0059 section 9). The query is part of what
        // the proof covers, so it is built once and both signed and sent.
        let _: EmptyResponse = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "POST",
            path: "/v1/channels/\(channelIdHex)/subscribers/\(subscriberPubkeyHex)/envelope",
            query: epoch.map { "epoch=\($0)" } ?? "",
            body: envelope,
            contentType: "application/octet-stream"
        )
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
        envelopes: [RotateBatchEntry],
        accountIdentity: Identity
    ) async throws -> UInt32 {
        struct Body: Encodable { let envelopes: [RotateBatchEntry] }
        struct EpochView: Decodable { let epoch: UInt32 }
        let view: EpochView = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "POST",
            path: "/v1/channels/\(channelIdHex)/rotate",
            query: "",
            body: try encoder.encode(Body(envelopes: envelopes)),
            contentType: "application/json"
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
        batch: [RotateBatchEntry],
        accountIdentity: Identity
    ) async throws -> Int {
        struct Body: Encodable {
            let target_epoch: UInt32
            let envelopes: [RotateBatchEntry]
        }
        struct StageView: Decodable {
            let target_epoch: UInt32
            let staged: Int
        }
        let view: StageView = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "POST",
            path: "/v1/channels/\(channelIdHex)/rotate/stage",
            query: "",
            body: try encoder.encode(Body(target_epoch: targetEpoch, envelopes: batch)),
            contentType: "application/json"
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
        targetEpoch: UInt32,
        accountIdentity: Identity
    ) async throws -> UInt32 {
        struct Body: Encodable { let target_epoch: UInt32 }
        struct EpochView: Decodable { let epoch: UInt32 }
        let view: EpochView = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "POST",
            path: "/v1/channels/\(channelIdHex)/rotate/commit",
            query: "",
            body: try encoder.encode(Body(target_epoch: targetEpoch)),
            contentType: "application/json"
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
        let roleKey = try? IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
            .roleKey()
        if let roleKey, let known = roles.role(forKey: roleKey) { return known == .owner }
        do {
            let signer = try Self.ownerSigner(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
            var request = URLRequest(url: try buildURL(endpoint: path))
            request.httpMethod = "GET"
            request.setValue(
                signer.sign(method: "GET", path: path, query: "", body: Data()),
                forHTTPHeaderField: channelOwnerProofHeaderName())
            let _: Resp = try await execute(request: request)
            if let roleKey { roles.set(.owner, forKey: roleKey) }
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

    // MARK: - ADR-0060 member cards

    /// `PUT /v1/channels/:id/members/:pubkey_hex` — upload this subscriber's
    /// member card, signed by its subscriber key (`sigHeader` is
    /// `memberCardRequestSignature(channelId:card:)`).
    public func putMemberCard(
        channelIdHex: String, subscriberPubHex: String, card: Data, sigHeader: String
    ) async throws {
        let url = try buildURL(endpoint: "/v1/channels/\(channelIdHex)/members/\(subscriberPubHex)")
        var request = URLRequest(url: url)
        request.httpMethod = "PUT"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.setValue(sigHeader, forHTTPHeaderField: Self.memberCardSigHeader)
        request.httpBody = card
        try await executeNoContent(request: request)
    }

    /// `DELETE /v1/channels/:id/members/:pubkey_hex` — remove the card on
    /// unsubscribe, signed over the empty card. Idempotent.
    public func deleteMemberCard(
        channelIdHex: String, subscriberPubHex: String, sigHeader: String
    ) async throws {
        let url = try buildURL(endpoint: "/v1/channels/\(channelIdHex)/members/\(subscriberPubHex)")
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        request.setValue(sigHeader, forHTTPHeaderField: Self.memberCardSigHeader)
        try await executeNoContent(request: request)
    }

    /// `GET /v1/channels/:id/members` — one page of `(subscriber key, card)`,
    /// and the `after` of the next page (`nil` on the last).
    public func listMemberCards(
        channelIdHex: String, after: String? = nil
    ) async throws -> (members: [(subscriberPk: Data, card: Data)], nextAfter: String?) {
        var endpoint = "/v1/channels/\(channelIdHex)/members"
        if let after { endpoint += "?after=\(after)" }
        let page: MemberCardsPage = try await getJson(endpoint: endpoint)
        let members = page.members.compactMap { m -> (Data, Data)? in
            guard let pk = base64UrlDecodeNoPad(m.subscriberPkB64),
                  let card = base64UrlDecodeNoPad(m.cardB64) else { return nil }
            return (pk, card)
        }
        return (members.map { (subscriberPk: $0.0, card: $0.1) }, page.nextAfter)
    }

    /// The member-card signature header (`x-lakko-member-card-sig`).
    static let memberCardSigHeader = "x-lakko-member-card-sig"

    /// The unsubscribe signature header (`x-lakko-unsubscribe-sig`), the
    /// core's `unsubscribeSigHeaderName()`; a unit test pins the two equal.
    static let unsubscribeSigHeader = "x-lakko-unsubscribe-sig"

    /// The `DELETE /v1/channels/:id/subscribers/:pubkey_hex` request that
    /// leaves a channel (ADR-0060 section 3): the server drops the subscriber
    /// row, its seed envelopes and its member card. `sigHeader` is the
    /// subscriber key's `unsubscribeRequestHeader(channelId:issuedAtMs:)`.
    func unsubscribeRequest(
        channelIdHex: String, subscriberPubHex: String, sigHeader: String
    ) throws -> URLRequest {
        let url = try buildURL(endpoint: "/v1/channels/\(channelIdHex)/subscribers/\(subscriberPubHex)")
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        request.setValue(sigHeader, forHTTPHeaderField: Self.unsubscribeSigHeader)
        return request
    }

    /// Leave a channel on the server. Idempotent: a `404` (the subscriber, or
    /// the channel, is already gone) counts as done.
    public func unsubscribe(
        channelIdHex: String, subscriberPubHex: String, sigHeader: String
    ) async throws {
        let request = try unsubscribeRequest(
            channelIdHex: channelIdHex, subscriberPubHex: subscriberPubHex, sigHeader: sigHeader)
        do {
            try await executeNoContent(request: request)
        } catch let error as ChannelsApiError where error.status == 404 {
            return
        }
    }

    private func executeNoContent(request: URLRequest) async throws {
        let (_, response) = try await AntiCensorshipTransport.perform(
            request, session: session, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        guard (200..<300).contains(http.statusCode) else {
            throw ChannelsApiError(
                status: http.statusCode, code: "member-card",
                detail: HTTPURLResponse.localizedString(forStatusCode: http.statusCode))
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

    /// One publisher-signed request (ADR-0059 section 9) as it goes on the
    /// wire: `path` is the service path (never the `/channels` prefix nginx
    /// strips on `api.lakko.me`), `query` the exact query without `?`, `body`
    /// the exact bytes. The proof is computed over those same three, so what
    /// is signed is what is sent.
    static func publisherRequest(
        baseURL: String,
        method: String,
        path: String,
        query: String,
        body: Data,
        contentType: String?,
        proof: (name: String, value: String)
    ) throws -> URLRequest {
        let endpoint = path + (query.isEmpty ? "" : "?" + query)
        guard let url = URL(string: baseURL + endpoint) else {
            throw ChannelsApiError(
                status: -1, code: "bad-url",
                detail: "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'")
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue(proof.value, forHTTPHeaderField: proof.name)
        if let contentType { request.setValue(contentType, forHTTPHeaderField: "Content-Type") }
        if method != "GET" && method != "HEAD" { request.httpBody = body }
        return request
    }

    /// Send one publisher-signed request and decode the success; a refusal
    /// throws the `ChannelsApiError`.
    ///
    /// With the role known, the request is signed once under it; a 401 then
    /// forgets the role, so the next call re-learns it, and surfaces. With
    /// the role unknown: the owner proof first and, on a 401, the
    /// co-publisher proof; whichever is accepted is remembered. Every attempt
    /// carries a fresh proof (each nonce is single use on the server).
    func sendAsPublisher<R: Decodable>(
        signing: ChannelPublisherSigning,
        method: String,
        path: String,
        query: String,
        body: Data,
        contentType: String?
    ) async throws -> R {
        let key = try signing.roleKey()
        func attempt(_ role: ChannelPublisherRole) async throws -> (Data, HTTPURLResponse) {
            let proof = try signing.proof(role: role, method: method, path: path, query: query, body: body)
            let request = try Self.publisherRequest(
                baseURL: baseURL, method: method, path: path, query: query,
                body: body, contentType: contentType, proof: proof)
            let (data, response) = try await transport(request)
            guard let http = response as? HTTPURLResponse else {
                throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
            }
            return (data, http)
        }
        if let known = roles.role(forKey: key) {
            let (data, http) = try await attempt(known)
            if http.statusCode == 401 { roles.clear(key: key) }
            return try decode(data: data, http: http)
        }
        let (ownerData, ownerHttp) = try await attempt(.owner)
        if (200..<300).contains(ownerHttp.statusCode) {
            roles.set(.owner, forKey: key)
            return try decode(data: ownerData, http: ownerHttp)
        }
        if ownerHttp.statusCode != 401 { return try decode(data: ownerData, http: ownerHttp) }
        let (data, http) = try await attempt(.copublisher)
        if (200..<300).contains(http.statusCode) { roles.set(.copublisher, forKey: key) }
        return try decode(data: data, http: http)
    }

    func getJson<R: Decodable>(endpoint: String) async throws -> R {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        return try await execute(request: request)
    }

    func execute<R: Decodable>(request: URLRequest) async throws -> R {
        let (data, response) = try await transport(request)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(
                status: -1,
                code: "no-http-response",
                detail: "non-HTTP response"
            )
        }
        return try decode(data: data, http: http)
    }

    func decode<R: Decodable>(data: Data, http: HTTPURLResponse) throws -> R {
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
        if R.self == EmptyResponse.self, let empty = EmptyResponse() as? R { return empty }
        return try decoder.decode(R.self, from: data)
    }

    func buildURL(endpoint: String) throws -> URL {
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

// MARK: - ADR-0059 section 9 — publisher-signed writes

/// How a channels request reaches the server; the client's seam for tests.
typealias ChannelsTransport = (URLRequest) async throws -> (Data, URLResponse)

/// A success with no body the caller reads (the envelope delivery).
struct EmptyResponse: Decodable {}

/// What this account is to a channel it publishes to: the owner (it created
/// the channel and derives the key registered for it) or a co-publisher the
/// owner appointed.
public enum ChannelPublisherRole: Equatable, Sendable {
    case owner
    case copublisher
}

/// The role per (account, channel), keyed on the owner verifying key the
/// account derives for the channel — unique to that pair, so another account
/// on the device never reuses this one's answer. In memory only: nothing
/// persisted says which role a device holds, so it is learned once per
/// session (from creation, a co-publisher join, or the first signed request)
/// and a large stage chunk is never uploaded twice to find out.
final class ChannelPublisherRoleCache: @unchecked Sendable {
    static let shared = ChannelPublisherRoleCache()

    private let lock = NSLock()
    private var roles: [String: ChannelPublisherRole] = [:]

    func role(forKey key: String) -> ChannelPublisherRole? {
        lock.lock(); defer { lock.unlock() }
        return roles[key]
    }

    func set(_ role: ChannelPublisherRole, forKey key: String) {
        lock.lock(); defer { lock.unlock() }
        roles[key] = role
    }

    func clear(key: String) {
        lock.lock(); defer { lock.unlock() }
        roles[key] = nil
    }

    func clearAll() {
        lock.lock(); defer { lock.unlock() }
        roles.removeAll()
    }
}

/// Produces the proof header for one publisher-signed request and the key
/// the role cache files this (account, channel) under.
protocol ChannelPublisherSigning {
    func roleKey() throws -> String
    func proof(
        role: ChannelPublisherRole, method: String, path: String, query: String, body: Data
    ) throws -> (name: String, value: String)
}

/// The production signing: the owner key or the co-publisher key the
/// account identity derives for the channel (ADR-0059 section 1).
struct IdentityPublisherSigning: ChannelPublisherSigning {
    let accountIdentity: Identity
    let channelIdHex: String

    func roleKey() throws -> String {
        let signer = try ChannelsClient.ownerSigner(accountIdentity: accountIdentity, channelIdHex: channelIdHex)
        return signer.verifyingKey().map { String(format: "%02x", $0) }.joined()
    }

    func proof(
        role: ChannelPublisherRole, method: String, path: String, query: String, body: Data
    ) throws -> (name: String, value: String) {
        switch role {
        case .owner:
            let signer = try ChannelsClient.ownerSigner(
                accountIdentity: accountIdentity, channelIdHex: channelIdHex)
            return (channelOwnerProofHeaderName(),
                    signer.sign(method: method, path: path, query: query, body: body))
        case .copublisher:
            guard let id = ChannelsClient.hexToData(channelIdHex), id.count == 16 else {
                throw ChannelsApiError(status: -1, code: "bad-channel-id", detail: "channel id is not 32 hex chars")
            }
            let signer = try channelCopublisherSigner(accountIdentity: accountIdentity, channelId: id)
            return (channelCopublisherProofHeaderName(),
                    signer.sign(method: method, path: path, query: query, body: body))
        }
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

    /// ADR-0087 phase B — the owner deleted the channel: every route answers
    /// `410 channel-deleted` for it.
    public static let channelDeletedCode = "channel-deleted"

    /// Whether `error` says the channel was deleted.
    public static func isChannelDeleted(_ error: Error) -> Bool {
        (error as? ChannelsApiError)?.code == channelDeletedCode
    }

    /// The one code the server answers every refused publisher proof with
    /// (`services/channels/src/owner_auth.rs`), whichever step failed.
    public static let publisherRefusedCode = "owner-proof-required"

    /// Whether `error` is the refused-publisher answer. The user-facing
    /// sentence is `channels.error.publisher_refused`, reached through
    /// ``ChannelsErrorCopy/key(forCode:)``.
    public static func isPublisherRefusal(_ error: Error) -> Bool {
        (error as? ChannelsApiError)?.code == publisherRefusedCode
    }
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

private struct MemberCardsPage: Decodable {
    struct Member: Decodable {
        let subscriberPkB64: String
        let cardB64: String
        enum CodingKeys: String, CodingKey {
            case subscriberPkB64 = "subscriber_pk_b64"
            case cardB64 = "card_b64"
        }
    }
    let members: [Member]
    let nextAfter: String?
    enum CodingKeys: String, CodingKey {
        case members
        case nextAfter = "next_after"
    }
}
