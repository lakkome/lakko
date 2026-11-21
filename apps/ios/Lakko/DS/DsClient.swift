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
/// for a group journal, the account's mailbox key for a mailbox read
/// (ADR-0078). The caller
/// passes a ``DsAuth``; the client signs the service path, the raw query
/// and the exact body, and retries a `401` once under the previous epoch
/// and once after the owner's lazy claim (ADR-0057; a member has none).
/// Epoch keys are registered by ownership statements
/// (``putStatement(_:)``), which carry their own signature. A write to
/// another account's mailbox is a sealed row signed with a reach key, or
/// unsigned with a `mailbox-write` anonymous token; a rendezvous write is
/// unsigned with a `rendezvous-write` anonymous token. No session token is ever
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

    /// `POST /ds/mailbox/{address_hex}/messages` — a write to someone's
    /// personal mailbox. `payload` is a sealed mailbox row (ADR-0078,
    /// ``MailboxTarget/seal(kind:body:)``); anything else is refused here
    /// before it leaves the device, exactly as the DS would refuse it.
    ///
    /// ADR-0060 section 2 — a claimed mailbox is CLOSED unless its owner chose
    /// Everyone, and a closed mailbox stores a write only when it is signed
    /// with a reach key the owner registered (`writeAuth`), or with a
    /// delegate key next to a delegation one of them signed. A write with no
    /// proof to a closed mailbox is answered `201` and DROPPED, exactly as an
    /// unclaimed mailbox answers, so this call cannot tell a delivered write
    /// from a dropped one; a proof that does not verify is `401`.
    ///
    /// ADR-0078 — a write with no proof spends one `mailbox-write` anonymous
    /// token and travels on the anonymous session; a write with a proof
    /// spends none.
    public func publishMailbox(
        pseudonym: Data,
        payload: Data,
        writeAuth: DsMailboxWriteAuth? = nil
    ) async throws -> PublishResult {
        guard pseudonym.count == Self.pseudonymLength else {
            throw DsClientError.argument(
                "pseudonym must be \(Self.pseudonymLength) bytes"
            )
        }
        guard isWellFormedMailboxRow(row: payload) else {
            throw DsClientError.argument("payload must be a sealed mailbox row")
        }
        let endpoint = "/ds/mailbox/\(Self.pseudonymHex(pseudonym))/messages"
        guard let writeAuth else {
            return try await publishWithToken(
                context: .mailboxWrite, endpoint: endpoint, payload: payload)
        }
        var headers: [String: String] = [:]
        let signer: DsProofSigner
        do {
            signer = try DsProofSigner.forReach(reachSeed: writeAuth.reachSeed)
        } catch {
            throw DsClientError.argument("reach seed must be 32 bytes")
        }
        if let delegation = writeAuth.delegationHeader {
            headers[Self.delegationHeaderName] = delegation
        }
        let parsed: PublishWireResponse = try await sendDecoding(
            method: "POST",
            endpoint: endpoint,
            body: payload,
            contentType: "application/octet-stream",
            extraHeaders: headers,
            auth: DsAuth(signer: signer)
        )
        return PublishResult(id: parsed.id, createdAt: parsed.createdAt)
    }

    // MARK: - ADR-0060 rendezvous

    /// `POST /ds/rendezvous/{id_hex}/messages` — pairing, recovery, the welcome
    /// courier, the state bundle, a rejoin presence note and a re-entry gap
    /// history. Never claimable and refusing any proof, delegation or wake
    /// header. Every write spends one `rendezvous-write` anonymous token and
    /// travels on the anonymous session, like a mailbox write with no proof;
    /// with no token to spend the write fails as a network failure does
    /// (``AnonTokenUnavailable``) and the caller's own retry applies.
    public func publishRendezvous(id: Data, payload: Data) async throws -> PublishResult {
        guard id.count == Self.pseudonymLength else {
            throw DsClientError.argument("rendezvous id must be \(Self.pseudonymLength) bytes")
        }
        guard !payload.isEmpty else {
            throw DsClientError.argument("payload must be non-empty")
        }
        return try await publishWithToken(
            context: .rendezvousWrite,
            endpoint: "/ds/rendezvous/\(Self.pseudonymHex(id))/messages",
            payload: payload
        )
    }

    /// An unsigned write that spends one anonymous token of `context`, on
    /// the anonymous session, retried once on a spent or expired token
    /// (``AnonTokenRoute/send(context:tokens:perform:)``).
    private func publishWithToken(
        context: AnonTokenContext,
        endpoint: String,
        payload: Data
    ) async throws -> PublishResult {
        let (data, http) = try await AnonTokenRoute.send(
            context: context, tokens: tokens
        ) { tokenHeader in
            try await send(
                method: "POST",
                endpoint: endpoint,
                body: payload,
                contentType: "application/octet-stream",
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
        let parsed: PublishWireResponse
        do {
            parsed = try decoder.decode(PublishWireResponse.self, from: data)
        } catch {
            throw DsClientError.http(
                status: http.statusCode, endpoint: endpoint, body: "decode failed: \(error)")
        }
        return PublishResult(id: parsed.id, createdAt: parsed.createdAt)
    }

    /// `GET /ds/rendezvous/{id_hex}/messages?...` — unsigned, see
    /// ``publishRendezvous(id:payload:)``.
    public func fetchRendezvous(
        id: Data,
        after: Int64? = nil,
        limit: Int? = nil,
        waitSecs: Int? = nil
    ) async throws -> FetchResult {
        guard id.count == Self.pseudonymLength else {
            throw DsClientError.argument("rendezvous id must be \(Self.pseudonymLength) bytes")
        }
        return try await doFetch(
            endpoint: "/ds/rendezvous/\(Self.pseudonymHex(id))/messages",
            after: after,
            limit: limit,
            waitSecs: waitSecs,
            auth: nil
        )
    }

    // MARK: - ADR-0060 reach keys

    /// `PUT /ds/mailbox/{hex}/reach` — register and unregister reach public
    /// keys on this account's own mailbox, and set the Everyone flag. Signed
    /// by the account's mailbox key (`dsMailboxSigner(accountIdentity:)`,
    /// ADR-0078).
    /// Idempotent: re-adding a key replaces its expiry, removing an absent key
    /// changes nothing.
    public func updateReach(
        pseudonym: Data,
        add: [DsReachKeyAdd],
        remove: [Data],
        open: Bool?,
        signer: DsRequestSigner
    ) async throws {
        guard pseudonym.count == Self.pseudonymLength else {
            throw DsClientError.argument("pseudonym must be \(Self.pseudonymLength) bytes")
        }
        let body = try encoder.encode(ReachWireRequest(
            add: add.map { ReachWireAdd(reachPkB64: base64Encode($0.publicKey), expiresAtMs: $0.expiresAtMs) },
            remove: remove.map { base64Encode($0) },
            open: open
        ))
        _ = try await send(
            method: "PUT",
            endpoint: "/ds/mailbox/\(Self.pseudonymHex(pseudonym))/reach",
            body: body,
            contentType: "application/json",
            auth: DsAuth(signer: signer)
        )
    }

    /// `GET /ds/mailbox/{hex}/reach` — the reach keys registered on this
    /// account's own mailbox and its Everyone flag (ADR-0060 section 1,
    /// "Versions after a restore"). Signed by the mailbox key (ADR-0078),
    /// exactly like the owner's message read; anything else is a
    /// uniform `401`, which throws. Public keys only (invariant #2).
    public func fetchReach(pseudonym: Data, signer: DsRequestSigner) async throws -> DsReachListing {
        guard pseudonym.count == Self.pseudonymLength else {
            throw DsClientError.argument("pseudonym must be \(Self.pseudonymLength) bytes")
        }
        let endpoint = "/ds/mailbox/\(Self.pseudonymHex(pseudonym))/reach"
        let parsed: ReachListingWire = try await sendDecoding(
            method: "GET", endpoint: endpoint, auth: DsAuth(signer: signer))
        var keys: [DsReachKeyAdd] = []
        for entry in parsed.keys {
            guard let key = Data(base64Encoded: entry.reachPkB64), key.count == 32 else {
                throw DsClientError.http(
                    status: 200, endpoint: endpoint, body: "a reach key that is not 32 bytes")
            }
            keys.append(DsReachKeyAdd(publicKey: key, expiresAtMs: entry.expiresAtMs))
        }
        return DsReachListing(keys: keys, open: parsed.open)
    }

    /// `POST /ds/mailbox/{hex}/reach/check` — is the key of `reachSeed` still
    /// registered, unexpired, on that mailbox? `true` on `204`, `false` on
    /// `401`; anything else throws (never read a network failure as "not
    /// found"). A searcher asks this before showing a result, so a narrowed
    /// setting takes effect on the next search.
    public func reachCheck(pseudonym: Data, reachSeed: Data) async throws -> Bool {
        guard pseudonym.count == Self.pseudonymLength else {
            throw DsClientError.argument("pseudonym must be \(Self.pseudonymLength) bytes")
        }
        let signer: DsProofSigner
        do {
            signer = try DsProofSigner.forReach(reachSeed: reachSeed)
        } catch {
            throw DsClientError.argument("reach seed must be 32 bytes")
        }
        let endpoint = "/ds/mailbox/\(Self.pseudonymHex(pseudonym))/reach/check"
        let (data, http) = try await send(
            method: "POST",
            endpoint: endpoint,
            body: Data(),
            auth: DsAuth(signer: signer),
            acceptAnyStatus: true
        )
        switch http.statusCode {
        case 200..<300: return true
        case 401: return false
        default:
            throw DsClientError.http(
                status: http.statusCode, endpoint: endpoint,
                body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>"))
        }
    }

    /// The delegation header's name, from the core (`x-lakko-ds-delegation`).
    static let delegationHeaderName: String = LakkoCore.delegationHeaderName()

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
    /// ADR-0053 / ADR-0078 — reading a claimed mailbox needs a proof under
    /// the account's mailbox key (`dsMailboxSigner(accountIdentity:)`). Only
    /// this account's own mailbox (and, for 30 days, its predecessor
    /// mailboxes) is ever read. Every row is a sealed mailbox row the caller
    /// opens with `openMailboxRow`.
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

    /// ADR-0087 section 5 — `DELETE /ds/group/{hex}/knocks?through=<id>`
    /// behind `Reject all`: the rows up to the last one this admin saw go, a
    /// request that arrived after stays. Write tier, signed like a read.
    public func deleteKnocks(groupId: Data, through: Int64, auth: DsAuth? = nil) async throws {
        guard !groupId.isEmpty else {
            throw DsClientError.argument("groupId must be non-empty")
        }
        let endpoint = "/ds/group/\(Self.pseudonymHex(groupId))/knocks"
        let (data, http) = try await send(
            method: "DELETE", endpoint: endpoint,
            queryItems: [URLQueryItem(name: "through", value: String(through))],
            auth: auth, acceptAnyStatus: true)
        guard (200..<300).contains(http.statusCode) else {
            throw DsClientError.http(
                status: http.statusCode, endpoint: endpoint,
                body: String(data: data, encoding: .utf8) ?? "")
        }
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
                createdAt: msg.createdAt,
                // Standard base64 or base64url: the key is 32 bytes either way.
                reachKey: msg.reachPkB64.flatMap { base64Decode($0) ?? base64UrlDecodeNoPad($0) }
                    .flatMap { $0.count == 32 ? $0 : nil }
            )
        }
        return FetchResult(
            messages: messages, hasMore: parsed.hasMore, latestEpoch: parsed.latestEpoch,
            registeredCommits: parsed.commits.map(FetchResult.registeredCommits(from:)))
    }

    // MARK: - ADR-0053 claim and knock

    /// ADR-0057 — send one ownership statement: `PUT <request.path>` with the
    /// statement body the FFI signer produced, no proof header (the statement
    /// carries its own signature). A `409` for a lost race on the group's lock
    /// row (body ``lockRaceBody``) is retried once with the same bytes: the
    /// transaction rolled back, so the statement's nonce was not spent. A
    /// DECIDED conflict (claimed already, an append past the head, a binding
    /// held elsewhere) is never re-sent: the DS spends the nonce before it
    /// decides, so the identical re-send would be refused `401` and a real
    /// `409` would surface as ``DsStatementError/unauthorized`` (live walk
    /// 2026-09-29). Every refusal surfaces as ``DsStatementError``.
    public func putStatement(_ request: DsStatementRequest) async throws {
        let body = Data(request.bodyJson.utf8)
        var attempt = 0
        while true {
            attempt += 1
            let (data, http) = try await send(
                method: "PUT",
                endpoint: request.path,
                body: body,
                contentType: "application/json",
                auth: nil,
                acceptAnyStatus: true
            )
            if (200..<300).contains(http.statusCode) { return }
            if http.statusCode == 409, attempt == 1,
               String(data: data, encoding: .utf8)?.contains(Self.lockRaceBody) == true {
                continue
            }
            throw DsStatementError.from(
                status: http.statusCode,
                body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>"))
        }
    }

    /// ADR-0081 decision 1 — `POST /ds/group/{hex}/commit`: the root's or a
    /// registrar's `RegisterEpoch` statement for the next epoch together with
    /// the commit row, in one request proven by `proof` (the key of the epoch
    /// the commit leaves) over the exact body. `201` is a new row, `200` the
    /// identical re-send of one that landed (its original id). Any refusal
    /// throws ``DsStatementError`` (`409`: another commit owns the epoch); a
    /// request that did not reach the DS throws
    /// ``DsClientError/network(endpoint:underlying:)``.
    public func postCommit(_ request: DsStatementRequest, proof: DsRequestSigner) async throws -> DsCommitAnswer {
        guard request.path.hasPrefix("/ds/group/"), request.path.hasSuffix("/commit") else {
            throw DsClientError.argument("a commit path must name a group journal's commit route")
        }
        let (data, http) = try await send(
            method: "POST",
            endpoint: request.path,
            body: Data(request.bodyJson.utf8),
            contentType: "application/json",
            auth: DsAuth(signer: proof),
            acceptAnyStatus: true
        )
        guard http.statusCode == 200 || http.statusCode == 201 else {
            throw DsStatementError.from(
                status: http.statusCode,
                body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>"))
        }
        let parsed: PublishWireResponse
        do {
            parsed = try decoder.decode(PublishWireResponse.self, from: data)
        } catch {
            throw DsClientError.http(
                status: http.statusCode, endpoint: request.path, body: "decode failed: \(error)")
        }
        return DsCommitAnswer(id: parsed.id, createdAt: parsed.createdAt, resent: http.statusCode == 200)
    }

    /// `PUT /ds/mailbox/{address_hex}/access` — claim a mailbox under the
    /// mailbox key it is named after (ADR-0078). `bodyJson` is the mailbox
    /// signer's `mailboxClaimBodyJson()` (`{"mailbox_pk_b64"}`). Unsigned:
    /// the key is public and the claim grants the claimant nothing.
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
        let skewed = http.statusCode == 401 && DsClockSkew.isSkewed(
            serverDateHeader: http.value(forHTTPHeaderField: "Date"), now: Date())
        if http.statusCode == 401, let auth, !skewed {
            auth.onRefusedAtHead?()
        }
        if acceptAnyStatus { return (data, http) }
        guard (200..<300).contains(http.statusCode) else {
            if http.statusCode == 401, auth != nil, skewed {
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
    /// The DS body of a `409` that lost a race for a group's lock row (the
    /// transaction rolled back, nothing was decided), as opposed to a decided
    /// conflict. `services/ds/src/access.rs` `registration_db_error`.
    static let lockRaceBody = "concurrent registration"

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
    let latestEpoch: UInt64?
    /// ADR-0091 — `[{epoch, id}]`, see ``FetchResult/registeredCommits``.
    let commits: [FetchWireCommit]?

    enum CodingKeys: String, CodingKey {
        case messages
        case hasMore = "has_more"
        case latestEpoch = "latest_epoch"
        case commits
    }
}

/// One entry of the DS `commits` field (ADR-0091).
struct FetchWireCommit: Decodable, Equatable {
    let epoch: UInt64
    let id: Int64
}

private struct FetchWireMessage: Decodable {
    let id: Int64
    let payloadB64: String
    let createdAt: String
    let reachPkB64: String?

    enum CodingKeys: String, CodingKey {
        case id
        case payloadB64 = "payload_b64"
        case createdAt = "created_at"
        case reachPkB64 = "reach_pk_b64"
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

private struct ReachWireAdd: Encodable {
    let reachPkB64: String
    let expiresAtMs: UInt64?

    enum CodingKeys: String, CodingKey {
        case reachPkB64 = "reach_pk_b64"
        case expiresAtMs = "expires_at_ms"
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(reachPkB64, forKey: .reachPkB64)
        try c.encodeIfPresent(expiresAtMs, forKey: .expiresAtMs)
    }
}

private struct ReachListingWire: Decodable {
    struct Key: Decodable {
        let reachPkB64: String
        let expiresAtMs: UInt64?

        enum CodingKeys: String, CodingKey {
            case reachPkB64 = "reach_pk_b64"
            case expiresAtMs = "expires_at_ms"
        }
    }

    let keys: [Key]
    let open: Bool
}

private struct ReachWireRequest: Encodable {
    let add: [ReachWireAdd]
    let remove: [String]
    let open: Bool?

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(add, forKey: .add)
        try c.encode(remove, forKey: .remove)
        try c.encodeIfPresent(open, forKey: .open)
    }

    enum CodingKeys: String, CodingKey { case add, remove, open }
}

// MARK: - ADR-0060 public types

/// How a write to someone else's closed mailbox is authorised (ADR-0060
/// section 2): the 32-byte reach seed the proof is signed with — a grant the
/// owner handed out, or a delegate's own seed — and, for a delegated write,
/// the `x-lakko-ds-delegation` header value the grant holder issued.
public struct DsMailboxWriteAuth: Equatable {
    public let reachSeed: Data
    public let delegationHeader: String?

    public init(reachSeed: Data, delegationHeader: String? = nil) {
        self.reachSeed = reachSeed
        self.delegationHeader = delegationHeader
    }
}

/// What ``DsClient/fetchReach(pseudonym:signer:)`` returns: the reach public
/// keys registered on the owner's mailbox (with their expiry) and whether it
/// is open (Everyone).
public struct DsReachListing: Equatable {
    public let keys: [DsReachKeyAdd]
    public let open: Bool

    public init(keys: [DsReachKeyAdd], open: Bool) {
        self.keys = keys
        self.open = open
    }
}

/// The two reach-key operations ``ReachCenter`` drives on the owner's mailbox,
/// as a protocol so a host test supplies a fake DS.
public protocol ReachDsTransport: AnyObject {
    func updateReach(
        pseudonym: Data, add: [DsReachKeyAdd], remove: [Data], open: Bool?, signer: DsRequestSigner
    ) async throws
    func fetchReach(pseudonym: Data, signer: DsRequestSigner) async throws -> DsReachListing
}

extension DsClient: ReachDsTransport {}

/// One key to register in ``DsClient/updateReach(pseudonym:add:remove:open:signer:)``.
public struct DsReachKeyAdd: Equatable, Hashable {
    public let publicKey: Data
    /// Unix ms after which the DS stops honouring the key; `nil` for a key
    /// that lives until removed.
    public let expiresAtMs: UInt64?

    public init(publicKey: Data, expiresAtMs: UInt64?) {
        self.publicKey = publicKey
        self.expiresAtMs = expiresAtMs
    }
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
    /// ADR-0060 section 1 (2026-11-06) — the reach key that admitted this
    /// row, on the owner's signed mailbox read; `nil` on a proofless row (and
    /// on group journals).
    public let reachKey: Data?

    public init(id: Int64, payload: Data, createdAt: String, reachKey: Data? = nil) {
        self.id = id
        self.payload = payload
        self.createdAt = createdAt
        self.reachKey = reachKey
    }
}

/// Reading a whole rendezvous, page after page. Anyone who learned a
/// rendezvous id can post rows to it (each costs a `rendezvous-write`
/// token), so a reader that wants one row among many reads every page rather
/// than the first: junk rows ahead of the real one cannot bury it.
public enum RendezvousRead {
    /// Rows asked for per read. The DS default page.
    public static let pageSize = 100
    /// Pages read at most (1000 rows). Chosen, not measured.
    public static let pageCap = 10

    public typealias FetchPage = (_ after: Int64, _ limit: Int) async throws -> FetchResult

    /// Every row of the rendezvous from the start, oldest first, at most
    /// ``pageCap`` pages of ``pageSize``: `after` is the last row id of the
    /// previous page, and the read follows `hasMore`. A failed page throws.
    public static func readAll(fetchPage: FetchPage) async throws -> [MailboxMessage] {
        var rows: [MailboxMessage] = []
        var after: Int64 = 0
        for _ in 0..<pageCap {
            let page = try await fetchPage(after, pageSize)
            rows.append(contentsOf: page.messages)
            guard page.hasMore, let last = page.messages.last, last.id > after else { break }
            after = last.id
        }
        return rows
    }
}

/// Result of ``DsClient/fetchMailbox(pseudonym:after:limit:waitSecs:)`` /
/// ``DsClient/fetchGroup(groupId:after:limit:waitSecs:)``.
public struct FetchResult: Equatable {
    public let messages: [MailboxMessage]
    /// `true` if there are more rows past the last returned id;
    /// caller should call again with `after = lastMessage.id`.
    public let hasMore: Bool
    /// The newest epoch registered on the DS for this journal. Present only
    /// on a signed read of a CLAIMED group journal; `nil` for an unclaimed
    /// journal and for mailboxes. The DeviceGroup sync runtime compares it
    /// with its own epoch to notice it is behind (``DeviceGroupRejoin``).
    public let latestEpoch: UInt64?
    /// ADR-0091 — present only where ``latestEpoch`` is: the commit row the DS
    /// registered for each epoch above the one the read was signed at, keyed
    /// by the epoch the row moves the journal TO. Row id of entry `n` is THE
    /// commit from `n - 1` to `n` (ADR-0081: registered with its row in one
    /// transaction); a commit-shaped row posted through the plain publish
    /// route is never named.
    public let registeredCommits: [UInt64: Int64]?

    public init(
        messages: [MailboxMessage], hasMore: Bool, latestEpoch: UInt64? = nil,
        registeredCommits: [UInt64: Int64]? = nil
    ) {
        self.messages = messages
        self.hasMore = hasMore
        self.latestEpoch = latestEpoch
        self.registeredCommits = registeredCommits
    }

    /// The `commits` entries keyed by epoch; a negative row id is dropped.
    static func registeredCommits(from wire: [FetchWireCommit]) -> [UInt64: Int64] {
        var out: [UInt64: Int64] = [:]
        for entry in wire where entry.id >= 0 { out[entry.epoch] = entry.id }
        return out
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
    /// The endpoint as it may appear in an error message: the route without its
    /// query and with every run of 16 or more hex digits (a journal, mailbox or
    /// rendezvous id) replaced by `:id`. Error messages reach logs, which carry
    /// no identifier (ADR-0078 section 11); the case keeps the full value.
    static func redacted(_ endpoint: String) -> String {
        let path = endpoint.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? endpoint
        return path.replacingOccurrences(of: "[0-9a-fA-F]{16,}", with: ":id", options: .regularExpression)
    }

    public var errorDescription: String? {
        switch self {
        case .argument(let m): return m
        case .http(let s, let e, let b): return "ds \(DsClientError.redacted(e)) → \(s): \(b)"
        case .network(let e, let u): return "ds \(DsClientError.redacted(e)) network error: \(u)"
        case .clockSkew: return DsClockSkew.copy
        }
    }
}

extension DsClient: DsStatementSending {}

extension DsClient: DsCommitSending {}
