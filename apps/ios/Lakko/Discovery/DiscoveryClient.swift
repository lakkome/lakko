// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// URLSession-based client for `lakko-svc-discovery`.
///
/// Mirror of:
///
///   * `apps/web/src/discovery/client.ts` (1.17e3)
///   * `apps/android/.../discovery/DiscoveryClient.kt` (1.15e3)
///
/// **Phase 1.14e1 scope** — OPRF + register + lookup + identity
/// bundle (upload + fetch) + KeyPackage (upload + fetch). The
/// invite codec endpoints (upload / fetch / consume / revoke) land
/// with 1.14e6.
///
/// Two layers, same shape as ``AuthClient``:
///
///   1. **Low-level methods** map the HTTP endpoints 1:1. Take a
///      typed body, return a typed response (or `nil` on the
///      documented 404 path), do nothing else.
///   2. **High-level ``registerUsername(accountId:username:)`` /
///      ``lookup(username:)`` /
///      ``uploadIdentityBundle(accountId:signaturePubkey:x25519Pubkey:)``** —
///      orchestrate the multi-round-trip flows: OPRF blind → POST
///      `/oprf` → finalize → POST `/register` (or `/lookup`); and the
///      identity-bundle upload.
///
/// ## Anonymous tokens, not proof of work (ADR-0054 S3)
///
/// No request here carries a proof of work any more. The anonymous routes
/// spend one anonymous token each (``AnonTokenPool``, header
/// `x-lakko-anon-token`): `/discovery/oprf` (`discovery-lookup` - every
/// username or channel lookup, and the OPRF step of a registration),
/// `/discovery/channel/register` (`discovery-register`) and the three
/// consumes (`invite-consume`). Those routes, and the two token-free lookups
/// `/discovery/lookup` + `/discovery/channel/lookup`, NEVER carry the
/// session token (not even this client's ``sessionToken``), a cookie or any
/// account identifier, and travel on their own connection pool
/// (``AnonymousHTTP``): a lookup must not be linkable to the account that
/// made it (invariant #5). The bearer routes (register, identity,
/// keypackage, the invite uploads) need only the session and may answer
/// `429 {"code": "rate-limited"}`.
///
/// ## Default base URL
///
/// `<LakkoApiBaseUrl>/discovery` (``ApiEndpoints``); with no API base
/// URL, `http://localhost:8084` — the iOS simulator shares the host's
/// loopback (unlike Android emulator which needs `10.0.2.2`), port
/// `8084` matching the `LAKKO_BIND` dev setup for
/// `lakko-svc-discovery`.
public final class DiscoveryClient {

    // MARK: - Configuration

    /// Default base URL. See class-level docs.
    public static var defaultBaseURL: String { ApiEndpoints.baseURL(for: .discovery) }

    public let baseURL: String

    private let session: URLSession
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    /// ADR-0026 Phase 4 — when armed, every HTTP call routes through the
    /// obfuscated tunnel; disarmed by default, so the direct path is unchanged.
    /// DiscoveryClient is the first client to adopt the `perform` seam; the rest
    /// migrate to it in a follow-up.
    private let antiCensorship: AntiCensorshipConfig

    /// Phase 1.5b: bearer token threaded into the
    /// `Authorization: Bearer <token>` header on every protected
    /// `lakko-svc-discovery` write (register, identity upload,
    /// keypackage upload, invite upload, invite revoke). `nil`
    /// means "anonymous" — every protected endpoint will return
    /// 401 in that mode. The anonymous routes never carry it.
    ///
    /// `private(set)` so callers update via ``setSessionToken(_:)``;
    /// reads are non-blocking from any actor (the underlying
    /// `URLSession` is thread-safe).
    public private(set) var sessionToken: String?

    /// ADR-0054 S3 - the session the anonymous routes travel on (its own
    /// connection pool, no cookies), and where their tokens come from.
    private let anonymousSession: URLSession
    private let tokens: AnonTokenSource

    public init(
        baseURL: String = DiscoveryClient.defaultBaseURL,
        session: URLSession = .shared,
        sessionToken: String? = nil,
        antiCensorship: AntiCensorshipConfig = .current,
        tokens: AnonTokenSource = AnonTokenPool.shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.anonymousSession = AnonymousHTTP.session(pairedWith: session)
        self.sessionToken = sessionToken
        self.antiCensorship = antiCensorship
        self.tokens = tokens
        self.encoder = JSONEncoder()
        self.decoder = JSONDecoder()
    }

    /// Update the bearer token in-place. Used by
    /// ``ContactsRepository`` to switch tokens on a fresh
    /// login/recover without rebuilding the entire client (the
    /// `URLSession` connection pool is reused).
    public func setSessionToken(_ token: String?) {
        self.sessionToken = token
    }

    /// A copy of this client carrying `token` as its bearer, over the same
    /// base URL, session, transport and token source.
    ///
    /// For a caller that holds a long-lived ANONYMOUS client (its reads -
    /// KeyPackage fetches, invite fetch / consume - must stay unlinked from
    /// the account, invariant #5) and needs one bearer-gated write: the copy
    /// is used for that request only, so the shared client never starts
    /// attaching the token to every later read.
    public func withSessionToken(_ token: String) -> DiscoveryClient {
        DiscoveryClient(
            baseURL: baseURL,
            session: session,
            sessionToken: token,
            antiCensorship: antiCensorship,
            tokens: tokens
        )
    }

    // MARK: - Low-level: 1:1 HTTP endpoints

    /// `POST /discovery/oprf` - spends one `discovery-lookup` token; anonymous.
    public func oprfEvaluate(_ request: OprfRequest) async throws -> OprfResponse {
        try await postAnonymous(
            endpoint: "/discovery/oprf", body: request, context: .discoveryLookup)
    }

    /// `POST /discovery/register`.
    public func register(_ request: RegisterRequest) async throws -> RegisterResponse {
        try await post(endpoint: "/discovery/register", body: request)
    }

    /// `POST /discovery/lookup`. Returns the typed response on 200,
    /// `nil` on 404 (the typed not-found path), throws on any
    /// other non-2xx. Anonymous, no token (the per-IP limiter only).
    public func lookupRaw(_ request: LookupRequest) async throws -> LookupResponse? {
        try await postAnonymousNullableOn404(endpoint: "/discovery/lookup", body: request)
    }

    /// `POST /discovery/channel/register` (G4-5, ADR-0010) — bind an
    /// OPRF-hashed `@channelname` to a `channel_id`. Since ADR-0054 S3 an
    /// anonymous route spending one `discovery-register` token (ownership
    /// lives at the PublisherGroup layer); never carries the session.
    public func channelRegister(
        _ request: ChannelRegisterRequest
    ) async throws -> ChannelRegisterResponse {
        try await postAnonymous(
            endpoint: "/discovery/channel/register", body: request, context: .discoveryRegister)
    }

    /// `POST /discovery/channel/lookup` (G4-5, ADR-0010). Returns the
    /// typed response on 200, `nil` on 404 (the typed not-found path),
    /// throws on any other non-2xx. Mirror of ``lookupRaw(_:)`` for the
    /// channel namespace — anonymous, no token (the per-IP limiter only).
    public func channelLookupRaw(
        _ request: ChannelLookupRequest
    ) async throws -> ChannelLookupResponse? {
        try await postAnonymousNullableOn404(
            endpoint: "/discovery/channel/lookup", body: request)
    }

    /// `POST /discovery/identity` — upsert the identity bundle.
    public func identityUpload(
        _ request: IdentityUploadRequest
    ) async throws -> IdentityUploadResponse {
        try await post(endpoint: "/discovery/identity", body: request)
    }

    /// `GET /discovery/identity/{accountId_b64url}`. Returns the
    /// bundle on 200, `nil` on 404, throws on any other non-2xx.
    ///
    /// The path uses **base64url-without-padding** because axum
    /// rejects `+` and `/` in path segments; the rest of the wire
    /// format uses standard-alphabet base64.
    public func identityFetch(
        accountId: Data
    ) async throws -> IdentityFetchResponse? {
        guard accountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "accountId must be \(Self.accountIdLength) bytes"
            )
        }
        let slug = base64UrlEncodeNoPad(accountId)
        return try await getNullableOn404(endpoint: "/discovery/identity/\(slug)")
    }

    /// `POST /discovery/keypackage` — upsert the user's MLS
    /// KeyPackage.
    public func keypackageUpload(
        _ request: KeyPackageUploadRequest
    ) async throws -> KeyPackageUploadResponse {
        try await post(endpoint: "/discovery/keypackage", body: request)
    }

    /// `GET /discovery/keypackage/{accountId_b64url}`. Returns the
    /// KeyPackage shape on 200, `nil` on 404, throws on any other
    /// non-2xx.
    public func keypackageFetch(
        accountId: Data
    ) async throws -> KeyPackageFetchResponse? {
        guard accountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "accountId must be \(Self.accountIdLength) bytes"
            )
        }
        let slug = base64UrlEncodeNoPad(accountId)
        return try await getNullableOn404(endpoint: "/discovery/keypackage/\(slug)")
    }

    // MARK: - High-level: full register / lookup / upload flows

    /// Bind `@username` to `accountId` server-side.
    ///
    /// Flow: OPRF blind → `POST /discovery/oprf` (anonymous, one
    /// `discovery-lookup` token) → finalize → `POST /discovery/register`
    /// (bearer).
    ///
    /// Username normalisation is handled inside `OprfBlinder.blind`
    /// (the UniFFI helper); the canonical form is surfaced back via
    /// ``RegisterResult/normalisedUsername`` so the UI can show what
    /// was actually claimed.
    ///
    /// Throws ``DiscoveryClientError/http(status:endpoint:body:)``
    /// with `status == 409` on username collision — usernames are
    /// first-come-first-served and immutable in Phase 1.14.
    public func registerUsername(
        accountId: Data,
        username: String
    ) async throws -> RegisterResult {
        guard accountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "accountId must be \(Self.accountIdLength) bytes"
            )
        }
        // ADR-0048 §4 — the blinder refuses a name outside the username rule;
        // checking first turns that refusal into the reason copy the form
        // shows (``UsernameRuleViolation``) instead of a raw FFI error.
        _ = try requireValidUsername(
            username, emptyError: DiscoveryClientError.argument("username must be non-empty"))
        let blinder = try OprfBlinder.blind(username: username)
        // Read the normalised form **before** finalize — after it
        // the UniFFI sentinel kicks in (1.15e1's
        // `DiscoveryAlreadyFinalized`).
        let normalisedUsername = try blinder.normalizedUsername()

        let oprfResp = try await oprfEvaluate(
            OprfRequest(blinded: blinder.blindedB64())
        )

        // ADR-0060 §5: only the lookup key goes to discovery; the OPRF
        // output never leaves the device.
        let lookupKey = try blinder.finalize(evaluatedB64: oprfResp.evaluated).lookupKey

        let resp = try await register(
            RegisterRequest(
                accountId: base64Encode(accountId),
                oprfHash: base64Encode(lookupKey)
            )
        )
        guard resp.ok else {
            throw DiscoveryClientError.http(
                status: 200,
                endpoint: "/discovery/register",
                body: "server reported ok=false"
            )
        }
        return RegisterResult(normalisedUsername: normalisedUsername)
    }

    /// Resolve `@username` to a raw 16-byte `accountId`, or `nil`
    /// when the username is not registered.
    ///
    /// Same flow as ``registerUsername(accountId:username:)`` modulo
    /// the second endpoint: OPRF blind → `POST /discovery/oprf` (one
    /// `discovery-lookup` token) → finalize → `POST /discovery/lookup`.
    /// Both requests anonymous; with no session there is no token, and the
    /// lookup fails with ``AnonTokenUnavailable/noSession``.
    public func lookup(username: String) async throws -> LookupResult {
        // ADR-0048 §4 — never send a lookup for a name that fails the rule.
        _ = try requireValidUsername(
            username, emptyError: DiscoveryClientError.argument("username must be non-empty"))
        let blinder = try OprfBlinder.blind(username: username)
        let normalisedUsername = try blinder.normalizedUsername()

        let oprfResp = try await oprfEvaluate(
            OprfRequest(blinded: blinder.blindedB64())
        )

        let lookupKey = try blinder.finalize(evaluatedB64: oprfResp.evaluated).lookupKey

        guard let resp = try await lookupRaw(
            LookupRequest(oprfHash: base64Encode(lookupKey))
        ) else {
            return LookupResult(
                normalisedUsername: normalisedUsername,
                account: nil
            )
        }
        return LookupResult(
            normalisedUsername: normalisedUsername,
            account: base64Decode(resp.accountId)
        )
    }

    // MARK: - ADR-0060 scoped discovery

    /// One OPRF round over `username` (one `discovery-lookup` token). The
    /// result carries both halves (ADR-0060 §5): `lookupKey` is what a
    /// username certificate binds and the only one that may go to discovery;
    /// `output` never leaves the device and is the key an introduction record
    /// is opened under. Throws on a name outside the rule.
    public func usernameOprf(_ username: String) async throws -> OprfResult {
        _ = try requireValidUsername(
            username, emptyError: DiscoveryClientError.argument("username must be non-empty"))
        let blinder = try OprfBlinder.blind(username: username)
        let evaluated = try await oprfEvaluate(OprfRequest(blinded: blinder.blindedB64()))
        return try blinder.finalize(evaluatedB64: evaluated.evaluated)
    }

    /// One OPRF round over a name search term, in the name domain
    /// (`OprfBlinder.blindNameTerm`, one `discovery-lookup` token). As with
    /// ``usernameOprf(_:)``: `lookupKey` for discovery, `output` for records.
    public func nameTermOprf(_ term: String) async throws -> OprfResult {
        let blinder = try OprfBlinder.blindNameTerm(term: term)
        let evaluated = try await oprfEvaluate(OprfRequest(blinded: blinder.blindedB64()))
        return try blinder.finalize(evaluatedB64: evaluated.evaluated)
    }

    /// `GET /discovery/username-certificate` (bearer) — this account's
    /// username certificate, signed now; `nil` when the account has no
    /// username (`404`).
    public func fetchUsernameCertificate() async throws -> Data? {
        let endpoint = "/discovery/username-certificate"
        var request = URLRequest(url: try buildURL(endpoint: endpoint))
        request.httpMethod = "GET"
        attachAuthorizationHeader(to: &request)
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(endpoint: endpoint, underlying: error)
        }
        guard let http = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(status: 0, endpoint: endpoint, body: "non-HTTP response")
        }
        if http.statusCode == 404 { return nil }
        guard (200..<300).contains(http.statusCode) else {
            throw DiscoveryClientError.http(
                status: http.statusCode, endpoint: endpoint,
                body: Self.truncate(String(data: raw.0, encoding: .utf8) ?? "<binary>"))
        }
        let parsed = try decoder.decode(UsernameCertificateWire.self, from: raw.0)
        return base64Decode(parsed.usernameCertificateB64)
    }

    /// `PUT /discovery/visibility` (bearer) — enter Everyone with the lookup
    /// keys (`OprfResult.lookupKey`, never the OPRF output) of every name
    /// search term and the public record (`composePublicRecord`, sealed under
    /// the OPRF outputs), or leave it (`public == false`, no terms, no record).
    public func putVisibility(
        public isPublic: Bool, nameTermLookupKeys: [Data], publicRecord: Data?
    ) async throws {
        let endpoint = "/discovery/visibility"
        var request = URLRequest(url: try buildURL(endpoint: endpoint))
        request.httpMethod = "PUT"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        attachAuthorizationHeader(to: &request)
        request.httpBody = try encoder.encode(VisibilityWireRequest(
            isPublic: isPublic, nameTermsB64: nameTermLookupKeys.map { base64Encode($0) },
            publicRecordB64: publicRecord.map { base64Encode($0) }))
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(endpoint: endpoint, underlying: error)
        }
        guard let http = raw.1 as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            let status = (raw.1 as? HTTPURLResponse)?.statusCode ?? 0
            throw DiscoveryClientError.http(
                status: status, endpoint: endpoint,
                body: Self.truncate(String(data: raw.0, encoding: .utf8) ?? "<binary>"))
        }
    }

    /// `POST /discovery/name/lookup` (anonymous) — the people (at most 20) in
    /// Everyone who published the name term whose lookup key
    /// (`OprfResult.lookupKey`, never the OPRF output) is `termLookupKey`,
    /// each with the public record the searcher opens with the term's OPRF
    /// output.
    public func nameLookup(termLookupKey: Data) async throws -> [PublicAccountHit] {
        let resp: NameLookupWireResponse = try await postAnonymous(
            endpoint: "/discovery/name/lookup",
            body: NameLookupWireRequest(oprfHashB64: base64Encode(termLookupKey)),
            context: nil)
        return resp.results.compactMap { row in
            guard let accountId = base64Decode(row.accountIdB64) else { return nil }
            return PublicAccountHit(
                accountId: accountId, publicRecord: base64Decode(row.publicRecordB64))
        }
    }

    /// `POST /discovery/lookup` (anonymous) for a username whose OPRF round
    /// the caller already ran: `lookupKey` is `OprfResult.lookupKey`. `nil`
    /// when the name is not registered or its owner is not in Everyone.
    public func lookupPublic(lookupKey: Data) async throws -> PublicAccountHit? {
        guard let resp = try await lookupRaw(LookupRequest(oprfHash: base64Encode(lookupKey))),
              let accountId = base64Decode(resp.accountId) else { return nil }
        return PublicAccountHit(
            accountId: accountId, publicRecord: resp.publicRecord.flatMap { base64Decode($0) })
    }

    /// `GET /.well-known/lakko-username-cert-key` — the username-certificate
    /// verifying key, used only when no key is pinned at build time.
    public func fetchUsernameCertKey() async throws -> Data? {
        let endpoint = "/.well-known/lakko-username-cert-key"
        guard let resp: CertKeyWire = try await getNullableOn404Public(endpoint: endpoint) else {
            return nil
        }
        return base64UrlDecodeNoPad(resp.keyB64url)
    }

    // MARK: - Channel discovery (G4-5, ADR-0010)
    //
    // The `@channelname` namespace reuses the SAME OPRF mechanism as
    // `@username` (the `/discovery/oprf` evaluate endpoint is
    // namespace-agnostic — one static key over the blinded handle). Only
    // the second endpoint differs: `/discovery/channel/{register,lookup}`
    // instead of `/discovery/{register,lookup}`, and the id is a 16-byte
    // channel_id pseudonym rather than a 16-byte account UUIDv7.
    // Mirror of `apps/web/src/discovery/client.ts`
    // `registerChannelName` / `lookupChannel`.

    /// Bind `@channelname` to a 16-byte `channel_id`.
    ///
    /// Flow: OPRF blind → `POST /discovery/oprf` (one `discovery-lookup`
    /// token) → finalize → `POST /discovery/channel/register` (one
    /// `discovery-register` token). Identical to
    /// ``registerUsername(accountId:username:)`` modulo the id type +
    /// the second endpoint. Handle normalisation (NFKC + lowercase +
    /// trim + leading-`@` strip) happens inside `OprfBlinder.blind`.
    ///
    /// The write is session-token-gated (a DoS rate-limit role, not an
    /// ownership proof). Throws
    /// ``DiscoveryClientError/http(status:endpoint:body:)`` with
    /// `status == 409` when the handle is already registered
    /// (first-come-first-served). Returns the server `ok` flag.
    public func registerChannelName(
        channelId: Data,
        handle: String
    ) async throws -> Bool {
        guard channelId.count == Self.channelIdLength else {
            throw DiscoveryClientError.argument(
                "channelId must be \(Self.channelIdLength) bytes"
            )
        }
        let blinder = try OprfBlinder.blindChannel(handle: handle)

        let oprfResp = try await oprfEvaluate(
            OprfRequest(blinded: blinder.blindedB64())
        )

        let lookupKey = try blinder.finalize(evaluatedB64: oprfResp.evaluated).lookupKey

        let resp = try await channelRegister(
            ChannelRegisterRequest(
                oprfHash: base64Encode(lookupKey),
                channelId: base64Encode(channelId)
            )
        )
        return resp.ok
    }

    /// Resolve `@channelname` to its 32-char hex `channel_id`, or `nil`
    /// when the handle is not registered.
    ///
    /// Same flow as ``registerChannelName(channelId:handle:)`` modulo the
    /// second endpoint: OPRF (one `discovery-lookup` token) → `POST /discovery/channel/lookup`.
    /// Exact-match only (the server does no prefix search). The 404
    /// not-found path is folded into a `nil` return so a not-found is
    /// visible in the type rather than an exception.
    public func lookupChannel(handle: String) async throws -> String? {
        let blinder = try OprfBlinder.blindChannel(handle: handle)

        let oprfResp = try await oprfEvaluate(
            OprfRequest(blinded: blinder.blindedB64())
        )

        let lookupKey = try blinder.finalize(evaluatedB64: oprfResp.evaluated).lookupKey

        guard let resp = try await channelLookupRaw(
            ChannelLookupRequest(oprfHash: base64Encode(lookupKey))
        ) else {
            return nil
        }
        guard let channelBytes = base64Decode(resp.channelId),
              channelBytes.count == Self.channelIdLength else {
            throw DiscoveryClientError.http(
                status: 200,
                endpoint: "/discovery/channel/lookup",
                body: "server returned malformed channel_id"
            )
        }
        return Self.hexEncode(channelBytes)
    }

    /// Upload (or replace) THIS device's MLS KeyPackage. Drives
    /// `POST /discovery/keypackage`.
    ///
    /// ADR-0050 — discovery keeps one row per `(account_id, device_id)`, where
    /// `deviceId` is the device leaf's 32-byte signature key, and caps distinct
    /// devices per account at 8 (409 beyond). The published package is a
    /// last-resort package, so one fetch opens a Welcome per group.
    public func uploadKeyPackage(
        accountId: Data,
        deviceId: Data,
        keyPackageBytes: Data
    ) async throws {
        guard accountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "accountId must be \(Self.accountIdLength) bytes"
            )
        }
        guard deviceId.count == 32 else {
            throw DiscoveryClientError.argument("deviceId must be 32 bytes")
        }
        guard !keyPackageBytes.isEmpty else {
            throw DiscoveryClientError.argument("keyPackageBytes must be non-empty")
        }
        guard keyPackageBytes.count <= Self.maxKeyPackageBytes else {
            throw DiscoveryClientError.argument(
                "keyPackageBytes exceeds the \(Self.maxKeyPackageBytes)-byte ceiling " +
                "(got \(keyPackageBytes.count) bytes)"
            )
        }

        let resp = try await keypackageUpload(
            KeyPackageUploadRequest(
                accountId: base64Encode(accountId),
                deviceId: base64Encode(deviceId),
                keypackage: base64Encode(keyPackageBytes)
            )
        )
        guard resp.ok else {
            throw DiscoveryClientError.http(
                status: 200,
                endpoint: "/discovery/keypackage",
                body: "server reported ok=false"
            )
        }
    }

    /// Resolve `accountId` to every device's KeyPackage (TLS-encoded MLS),
    /// oldest device first (ADR-0050). Returns `nil` when the account has
    /// published none. A caller adding the account adds ALL of them; the
    /// device binding inside each leaf is what the core verifies. Entries
    /// whose base64 does not decode are dropped.
    public func fetchKeyPackages(accountId: Data) async throws -> [DeviceKeyPackage]? {
        guard let raw = try await keypackageFetch(accountId: accountId) else {
            return nil
        }
        return raw.keypackages.compactMap { entry in
            guard let deviceId = base64Decode(entry.deviceId), deviceId.count == 32,
                  let keyPackage = base64Decode(entry.keypackage), !keyPackage.isEmpty
            else { return nil }
            return DeviceKeyPackage(deviceId: deviceId, keyPackage: keyPackage)
        }
    }

    /// `DELETE /discovery/keypackage/device/{device_id_b64url}` — remove one
    /// of the authenticated account's device KeyPackages (ADR-0050 device
    /// removal). Bearer-auth; idempotent (204).
    public func deleteKeyPackage(deviceId: Data) async throws {
        guard deviceId.count == 32 else {
            throw DiscoveryClientError.argument("deviceId must be 32 bytes")
        }
        let endpoint = "/discovery/keypackage/device/\(base64UrlEncodeNoPad(deviceId))"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        attachAuthorizationHeader(to: &request)
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(
                endpoint: "\(endpoint) (DELETE)",
                underlying: error
            )
        }
        guard let httpResponse = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: "\(endpoint) (DELETE)",
                body: "non-HTTP response"
            )
        }
        if (200..<300).contains(httpResponse.statusCode) {
            return
        }
        throw DiscoveryClientError.http(
            status: httpResponse.statusCode,
            endpoint: "\(endpoint) (DELETE)",
            body: Self.truncate(String(data: raw.0, encoding: .utf8) ?? "<binary>")
        )
    }

    /// Upload (or replace) the identity bundle for `accountId`.
    /// Drives `POST /discovery/identity` (bearer).
    ///
    /// Idempotent — the server upserts via
    /// `INSERT … ON DUPLICATE KEY UPDATE`. Calling this on every
    /// login refreshes the bundle, important for the
    /// deterministic-derivation property: the same password
    /// produces the same Ed25519 + X25519 public keys, so the
    /// server's row never changes content but the upsert path stays
    /// uniform.
    ///
    /// G-4 (ADR-0016): `supersessionProofB64`, when non-`nil`, is the
    /// base64 of an old-key-signed supersession proof bridging an
    /// AccountIdentity rotation (recover). It threads straight into
    /// `IdentityUploadRequest.supersessionProof` so the new keys +
    /// proof ship in the **same** request (ADR-0016 rule #1, atomic
    /// emit). Normal (non-rotation) callers pass `nil` — its absence
    /// clears any stale server-side proof. The server stores the
    /// bytes opaquely and never parses them (invariant #2).
    ///
    /// ADR-0036 (G-30 finding 2): `genesisAttest`, when non-`nil`, is the RAW
    /// bytes of an auth-attested genesis proof (`POST /auth/genesis/attest`).
    /// It ships alongside the genesis leaf-auth self-sig so the transparency
    /// genesis entry is account-authenticated (anti-squatting). Standard-base64
    /// encoded into `IdentityUploadRequest.genesisAttest`; `nil` on a
    /// non-genesis re-publish (absence is `COALESCE`-preserved server-side).
    ///
    /// ADR-0065 section 4.1: `keyResetAttest`, when non-`nil`, is the RAW
    /// 177-byte key-reset attestation of the reset that installs THIS key;
    /// standard-base64 encoded into `IdentityUploadRequest.keyResetAttest`.
    /// The refusals discovery answers are typed by
    /// ``IdentityUploadRefusal/of(_:)``.
    public func uploadIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        supersessionProofB64: String? = nil,
        leafAuth: Data? = nil,
        genesisAttest: Data? = nil,
        keyResetAttest: Data? = nil
    ) async throws {
        guard accountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "accountId must be \(Self.accountIdLength) bytes"
            )
        }
        guard signaturePubkey.count == Self.pubkeyLength else {
            throw DiscoveryClientError.argument(
                "signaturePubkey must be \(Self.pubkeyLength) bytes (Ed25519)"
            )
        }
        guard x25519Pubkey.count == Self.pubkeyLength else {
            throw DiscoveryClientError.argument(
                "x25519Pubkey must be \(Self.pubkeyLength) bytes (HPKE init_key)"
            )
        }

        let resp = try await identityUpload(
            IdentityUploadRequest(
                accountId: base64Encode(accountId),
                signaturePubkey: base64Encode(signaturePubkey),
                x25519Pubkey: base64Encode(x25519Pubkey),
                supersessionProof: supersessionProofB64,
                leafAuth: leafAuth.map { base64Encode($0) },
                genesisAttest: genesisAttest.map { base64Encode($0) },
                keyResetAttest: keyResetAttest.map { base64Encode($0) }
            )
        )
        guard resp.ok else {
            throw DiscoveryClientError.http(
                status: 200,
                endpoint: "/discovery/identity",
                body: "server reported ok=false"
            )
        }
    }

    // MARK: - Invite endpoints (1.14e6, parallel to Android 1.15e9)

    /// `POST /discovery/invite` — upload a verified
    /// `InviteCapability` blob, get the server-minted 6-byte
    /// `blob_id` back (8 base64url characters). Not idempotent:
    /// every upload gets a fresh random id.
    public func inviteUpload(
        _ request: InviteUploadRequest
    ) async throws -> InviteUploadResponse {
        try await post(endpoint: "/discovery/invite", body: request)
    }

    /// `GET /discovery/invite/{blob_id_b64url}`. Returns the
    /// capability wire shape on 200, `nil` on 404 (server folds
    /// expired blobs into the same status), throws on any other
    /// non-2xx.
    public func inviteFetch(blobId: Data) async throws -> InviteFetchResponse? {
        guard blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes"
            )
        }
        let slug = base64UrlEncodeNoPad(blobId)
        return try await getNullableOn404Public(
            endpoint: "/discovery/invite/\(slug)"
        )
    }

    // MARK: - High-level invite flows

    /// Upload a freshly-minted SEALED `/i/` link (ADR-0028 amendment
    /// 2026-09-28) and receive the server-minted `blobId` (6 random bytes).
    /// The ledger stores only `mint.sealed`; the clear fields carry what the
    /// server needs (revoke key, expiry, consume ceiling, the owner's label).
    /// A second upload of the same bytes gets a second id.
    public func uploadInvite(
        mint: InviteLinkMint,
        expiryMs: UInt64,
        maxUses: UInt32,
        label: String,
        bootstrapReferral: Bool
    ) async throws -> Data {
        try Self.checkSealed(mint.sealed, ceiling: Self.maxCapabilityBytes)
        let resp = try await inviteUpload(
            InviteUploadRequest(
                sealed: base64Encode(mint.sealed),
                issuerPubkey: base64Encode(mint.issuerPubkey),
                expiryMs: expiryMs,
                maxUses: maxUses,
                label: label,
                bootstrapReferral: bootstrapReferral
            )
        )
        return try Self.parseBlobId(resp.blobId, endpoint: "/discovery/invite")
    }

    /// Fetch a previously-uploaded SEALED `/i/` link by its ledger `blobId`.
    /// Returns `nil` on 404, the sealed bytes on 200; the caller opens them
    /// with the link's fragment secret (`InviteCapability.openLink`), which
    /// also re-verifies the signature, so a substituted blob opens nothing.
    public func fetchInvite(blobId: Data) async throws -> Data? {
        guard let raw = try await inviteFetch(blobId: blobId) else {
            return nil
        }
        return base64Decode(raw.sealed)
    }

    /// Atomically bump the consume counter for the invite at
    /// `blobId` with per-joiner dedup. Returns a discriminated
    /// ``ConsumeInviteResult`` so the joiner UI can branch on the
    /// four observable outcomes.
    public func consumeInvite(
        blobId: Data,
        capabilityBytes: Data,
        joinerAccountId: Data
    ) async throws -> ConsumeInviteResult {
        guard blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes"
            )
        }
        guard joinerAccountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "joinerAccountId must be \(Self.accountIdLength) bytes"
            )
        }

        let joinerTag = try computeJoinerInviteTag(
            capabilityBytes: capabilityBytes,
            joinerAccountId: joinerAccountId
        )
        let slug = base64UrlEncodeNoPad(blobId)

        let endpoint = "/discovery/invite/\(slug)/consume"
        // ADR-0054 S3 - one `invite-consume` anonymous token, no
        // `Authorization`, the anonymous session.
        let raw = try await postAnonymousRaw(
            endpoint: endpoint,
            body: InviteConsumeRequest(joinerTag: base64Encode(joinerTag)),
            context: .inviteConsume
        )
        let httpResponse = raw.1
        switch httpResponse.statusCode {
        case 404:
            return .notFound
        case 410:
            return .exhausted
        case 429:
            throw DiscoveryClientError.rateLimited(endpoint: endpoint)
        case 200..<300:
            let parsed: InviteConsumeResponse
            do {
                parsed = try decoder.decode(
                    InviteConsumeResponse.self,
                    from: raw.0
                )
            } catch {
                throw DiscoveryClientError.http(
                    status: httpResponse.statusCode,
                    endpoint: endpoint,
                    body: "decode failed: \(error)"
                )
            }
            if parsed.alreadyConsumed {
                return .alreadyConsumed(
                    usedCount: parsed.usedCount,
                    maxUses: parsed.maxUses
                )
            } else {
                return .consumed(
                    usedCount: parsed.usedCount,
                    maxUses: parsed.maxUses
                )
            }
        default:
            let bodyText = Self.truncate(
                String(data: raw.0, encoding: .utf8) ?? "<binary>"
            )
            throw DiscoveryClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: bodyText
            )
        }
    }

    /// Owner-signed revocation of an invite blob.
    ///
    /// Driven by the same Ed25519 key that signed the capability:
    /// `signInviteRevocation` builds the canonical bytes + signs.
    /// We POST `signedAtMs + signature` as the DELETE body; the
    /// server recovers the inviter's pubkey from the stored
    /// capability bytes and verifies the signature against the
    /// same canonical layout.
    ///
    /// Returns a discriminated ``RevokeInviteResult``: `.revoked`
    /// on 2xx, `.notFoundOrNotOwner` on 404 (server folds "no
    /// row" / "bad signature" / "stored bytes failed re-decode"
    /// into one status to deny status-code probing). Other non-2xx
    /// still throw.
    public func revokeInvite(
        blobId: Data,
        signingSeed: Data
    ) async throws -> RevokeInviteResult {
        guard blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes"
            )
        }
        guard signingSeed.count == Self.signingSeedLength else {
            throw DiscoveryClientError.argument(
                "signingSeed must be \(Self.signingSeedLength) bytes"
            )
        }

        let signedAtMs = Int64(Date().timeIntervalSince1970 * 1000)
        let signature = try signInviteRevocation(
            signingSeed: signingSeed,
            blobId: blobId,
            signedAtMs: UInt64(signedAtMs)
        )
        let slug = base64UrlEncodeNoPad(blobId)
        let endpoint = "/discovery/invite/\(slug)"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        attachAuthorizationHeader(to: &request)
        let body = InviteRevokeRequest(
            signedAtMs: signedAtMs,
            signature: base64Encode(signature)
        )
        do {
            request.httpBody = try encoder.encode(body)
        } catch {
            throw DiscoveryClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }

        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(
                endpoint: "\(endpoint) (DELETE)",
                underlying: error
            )
        }
        guard let httpResponse = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: "\(endpoint) (DELETE)",
                body: "non-HTTP response"
            )
        }
        if (200..<300).contains(httpResponse.statusCode) {
            return .revoked
        }
        if httpResponse.statusCode == 404 {
            return .notFoundOrNotOwner
        }
        let bodyText = Self.truncate(
            String(data: raw.0, encoding: .utf8) ?? "<binary>"
        )
        throw DiscoveryClientError.http(
            status: httpResponse.statusCode,
            endpoint: "\(endpoint) (DELETE)",
            body: bodyText
        )
    }

    /// A3 (GDPR erasure) — `DELETE /discovery/account`. Purges this account's
    /// discovery footprint (username binding, identity bundle, KeyPackage,
    /// owned invites). Bearer-auth; the server keys the purge on the verified
    /// session token's account_id. Idempotent (204).
    public func deleteAccount() async throws {
        let endpoint = "/discovery/account"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        attachAuthorizationHeader(to: &request)
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(
                endpoint: "\(endpoint) (DELETE)",
                underlying: error
            )
        }
        guard let httpResponse = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: "\(endpoint) (DELETE)",
                body: "non-HTTP response"
            )
        }
        if (200..<300).contains(httpResponse.statusCode) {
            return
        }
        let bodyText = Self.truncate(
            String(data: raw.0, encoding: .utf8) ?? "<binary>"
        )
        throw DiscoveryClientError.http(
            status: httpResponse.statusCode,
            endpoint: "\(endpoint) (DELETE)",
            body: bodyText
        )
    }

    /// `POST /discovery/call-invite` — upload a verified
    /// `CallInviteCapability` blob (Phase 3c3-server). Sibling
    /// of ``inviteUpload(_:)`` — wire shape
    /// ``InviteUploadRequest`` is shared; only the URL path
    /// differs. The high-level
    /// ``uploadCallInvite(capabilityBytes:)`` wrapper drives
    /// the token.
    public func callInviteUpload(
        _ request: InviteUploadRequest
    ) async throws -> InviteUploadResponse {
        try await post(endpoint: "/discovery/call-invite", body: request)
    }

    /// `GET /discovery/call-invite/{blob_id_b64url}`. Returns
    /// the capability wire shape on 200, `nil` on 404 (server
    /// folds expired blobs into the same status), throws on any
    /// other non-2xx.
    public func callInviteFetch(blobId: Data) async throws -> InviteFetchResponse? {
        guard blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes"
            )
        }
        let slug = base64UrlEncodeNoPad(blobId)
        return try await getNullableOn404Public(
            endpoint: "/discovery/call-invite/\(slug)"
        )
    }

    /// Upload a freshly-minted SEALED `/c/` link and receive the
    /// server-minted `blobId` (6 random bytes). Drives
    /// `POST /discovery/call-invite`. Every upload gets a fresh id.
    public func uploadCallInvite(
        mint: InviteLinkMint,
        expiryMs: UInt64,
        maxUses: UInt32
    ) async throws -> Data {
        try Self.checkSealed(mint.sealed, ceiling: Self.maxCallCapabilityBytes)
        let resp = try await callInviteUpload(
            InviteUploadRequest(
                sealed: base64Encode(mint.sealed),
                issuerPubkey: base64Encode(mint.issuerPubkey),
                expiryMs: expiryMs,
                maxUses: maxUses
            )
        )
        return try Self.parseBlobId(resp.blobId, endpoint: "/discovery/call-invite")
    }

    /// Fetch a previously-uploaded SEALED `/c/` link. `nil` on 404, the
    /// sealed bytes on 200 (opened with `CallInviteCapability.openLink`).
    public func fetchCallInvite(blobId: Data) async throws -> Data? {
        guard let raw = try await callInviteFetch(blobId: blobId) else {
            return nil
        }
        return base64Decode(raw.sealed)
    }

    /// Atomically bump the consume counter for the call-invite
    /// at `blobId` with per-joiner dedup. Returns a
    /// discriminated ``ConsumeInviteResult`` so the joiner UI
    /// can branch on the four observable outcomes.
    ///
    /// Reuses the same `computeJoinerInviteTag(...)` helper as
    /// the contact-invite flow — salt is per-blob, so
    /// contact-invite and call-invite tags for the same joiner
    /// don't collide on the server.
    public func consumeCallInvite(
        blobId: Data,
        capabilityBytes: Data,
        joinerAccountId: Data
    ) async throws -> ConsumeInviteResult {
        guard blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes"
            )
        }
        guard joinerAccountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "joinerAccountId must be \(Self.accountIdLength) bytes"
            )
        }

        let joinerTag = try computeJoinerInviteTag(
            capabilityBytes: capabilityBytes,
            joinerAccountId: joinerAccountId
        )
        let slug = base64UrlEncodeNoPad(blobId)

        let endpoint = "/discovery/call-invite/\(slug)/consume"
        // ADR-0054 S3 - one `invite-consume` anonymous token, no
        // `Authorization`, the anonymous session.
        let raw = try await postAnonymousRaw(
            endpoint: endpoint,
            body: InviteConsumeRequest(joinerTag: base64Encode(joinerTag)),
            context: .inviteConsume
        )
        let httpResponse = raw.1
        switch httpResponse.statusCode {
        case 404:
            return .notFound
        case 410:
            return .exhausted
        case 429:
            throw DiscoveryClientError.rateLimited(endpoint: endpoint)
        case 200..<300:
            let parsed: InviteConsumeResponse
            do {
                parsed = try decoder.decode(
                    InviteConsumeResponse.self,
                    from: raw.0
                )
            } catch {
                throw DiscoveryClientError.http(
                    status: httpResponse.statusCode,
                    endpoint: endpoint,
                    body: "decode failed: \(error)"
                )
            }
            if parsed.alreadyConsumed {
                return .alreadyConsumed(
                    usedCount: parsed.usedCount,
                    maxUses: parsed.maxUses
                )
            } else {
                return .consumed(
                    usedCount: parsed.usedCount,
                    maxUses: parsed.maxUses
                )
            }
        default:
            let bodyText = Self.truncate(
                String(data: raw.0, encoding: .utf8) ?? "<binary>"
            )
            throw DiscoveryClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: bodyText
            )
        }
    }

    // ------------------------------------------------------------------------
    // ADR-0028 (G-3) — group join-ticket blob ledger
    // ------------------------------------------------------------------------

    /// `POST /discovery/group-invite` — upload a verified
    /// `GroupInviteCapability` blob. Low-level helper driven by
    /// ``uploadGroupInvite(capabilityBytes:maxUses:)``.
    public func groupInviteUpload(
        _ request: InviteUploadRequest
    ) async throws -> InviteUploadResponse {
        try await post(endpoint: "/discovery/group-invite", body: request)
    }

    /// `GET /discovery/group-invite/{blob_id_b64url}`. Returns the capability
    /// wire shape on 200, `nil` on 404 (server folds expired blobs into the
    /// same status), throws on any other non-2xx.
    public func groupInviteFetch(blobId: Data) async throws -> InviteFetchResponse? {
        guard blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes"
            )
        }
        let slug = base64UrlEncodeNoPad(blobId)
        return try await getNullableOn404Public(
            endpoint: "/discovery/group-invite/\(slug)"
        )
    }

    /// Upload a freshly-minted SEALED `/g/` link and receive the
    /// server-minted `blobId` (6 random bytes). Drives
    /// `POST /discovery/group-invite`. Every upload gets a fresh id.
    ///
    /// `maxUses` is a SOFT anti-spam throttle (NOT part of the signed
    /// capability, NOT an access control — the admin approval gate is the
    /// security boundary): `0` = single-use, `0xFFFFFFFF` = unlimited.
    public func uploadGroupInvite(
        mint: InviteLinkMint,
        expiryMs: UInt64,
        maxUses: UInt32
    ) async throws -> Data {
        try Self.checkSealed(mint.sealed, ceiling: Self.maxGroupCapabilityBytes)
        let resp = try await groupInviteUpload(
            InviteUploadRequest(
                sealed: base64Encode(mint.sealed),
                issuerPubkey: base64Encode(mint.issuerPubkey),
                expiryMs: expiryMs,
                maxUses: maxUses
            )
        )
        return try Self.parseBlobId(resp.blobId, endpoint: "/discovery/group-invite")
    }

    /// Fetch a previously-uploaded SEALED `/g/` link. `nil` on 404, the
    /// sealed bytes on 200 (opened with `GroupInviteCapability.openLink`).
    public func fetchGroupInvite(blobId: Data) async throws -> Data? {
        guard let raw = try await groupInviteFetch(blobId: blobId) else {
            return nil
        }
        return base64Decode(raw.sealed)
    }

    /// Atomically bump the consume counter for the group-invite at `blobId`
    /// with per-joiner dedup. Returns a discriminated ``ConsumeInviteResult``.
    /// The consume is anti-spam defence-in-depth — the admin approval gate
    /// (ADR-0027) remains the security boundary.
    ///
    /// Reuses the same `computeJoinerInviteTag(...)` helper as the contact- and
    /// call-invite flows — salt is per-blob, so tags don't collide across
    /// invite kinds.
    public func consumeGroupInvite(
        blobId: Data,
        capabilityBytes: Data,
        joinerAccountId: Data
    ) async throws -> ConsumeInviteResult {
        guard blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes"
            )
        }
        guard joinerAccountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "joinerAccountId must be \(Self.accountIdLength) bytes"
            )
        }

        let joinerTag = try computeJoinerInviteTag(
            capabilityBytes: capabilityBytes,
            joinerAccountId: joinerAccountId
        )
        let slug = base64UrlEncodeNoPad(blobId)

        let endpoint = "/discovery/group-invite/\(slug)/consume"
        // ADR-0054 S3 - one `invite-consume` anonymous token, no
        // `Authorization`, the anonymous session.
        let raw = try await postAnonymousRaw(
            endpoint: endpoint,
            body: InviteConsumeRequest(joinerTag: base64Encode(joinerTag)),
            context: .inviteConsume
        )
        let httpResponse = raw.1
        switch httpResponse.statusCode {
        case 404:
            return .notFound
        case 410:
            return .exhausted
        case 429:
            throw DiscoveryClientError.rateLimited(endpoint: endpoint)
        case 200..<300:
            let parsed: InviteConsumeResponse
            do {
                parsed = try decoder.decode(
                    InviteConsumeResponse.self,
                    from: raw.0
                )
            } catch {
                throw DiscoveryClientError.http(
                    status: httpResponse.statusCode,
                    endpoint: endpoint,
                    body: "decode failed: \(error)"
                )
            }
            if parsed.alreadyConsumed {
                return .alreadyConsumed(
                    usedCount: parsed.usedCount,
                    maxUses: parsed.maxUses
                )
            } else {
                return .consumed(
                    usedCount: parsed.usedCount,
                    maxUses: parsed.maxUses
                )
            }
        default:
            let bodyText = Self.truncate(
                String(data: raw.0, encoding: .utf8) ?? "<binary>"
            )
            throw DiscoveryClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: bodyText
            )
        }
    }

    /// Owner-signed revocation of a `CallInviteCapability` blob
    /// (Phase 3c3-revoke).
    ///
    /// Sibling of ``revokeInvite(blobId:signingSeed:)`` for the
    /// call-invite ecosystem. Driven by the same Ed25519 key
    /// that signed the capability:
    /// `signCallInviteRevocation` builds the canonical bytes
    /// (`CALL_INVITE_REVOKE_DOMAIN_V1 || blob_id ||
    /// signed_at_ms_be`) under a distinct domain separator,
    /// so a captured contact-invite revoke cannot be replayed
    /// here (and vice versa). The wire body is the shared
    /// ``InviteRevokeRequest`` — only the URL path differs.
    ///
    /// Returns a discriminated ``RevokeInviteResult``:
    ///  - `.revoked` on 2xx
    ///  - `.notFoundOrNotOwner` on 404 (server folds "no row"
    ///    / "bad signature" / "stored bytes failed re-decode"
    ///    into one status to deny status-code probing)
    /// Other non-2xx still throw.
    public func revokeCallInvite(
        blobId: Data,
        signingSeed: Data
    ) async throws -> RevokeInviteResult {
        guard blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes"
            )
        }
        guard signingSeed.count == Self.signingSeedLength else {
            throw DiscoveryClientError.argument(
                "signingSeed must be \(Self.signingSeedLength) bytes"
            )
        }

        let signedAtMs = Int64(Date().timeIntervalSince1970 * 1000)
        let signature = try signCallInviteRevocation(
            signingSeed: signingSeed,
            blobId: blobId,
            signedAtMs: UInt64(signedAtMs)
        )
        let slug = base64UrlEncodeNoPad(blobId)
        let endpoint = "/discovery/call-invite/\(slug)"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        attachAuthorizationHeader(to: &request)
        let body = InviteRevokeRequest(
            signedAtMs: signedAtMs,
            signature: base64Encode(signature)
        )
        do {
            request.httpBody = try encoder.encode(body)
        } catch {
            throw DiscoveryClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }

        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(
                endpoint: "\(endpoint) (DELETE)",
                underlying: error
            )
        }
        guard let httpResponse = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: "\(endpoint) (DELETE)",
                body: "non-HTTP response"
            )
        }
        if (200..<300).contains(httpResponse.statusCode) {
            return .revoked
        }
        if httpResponse.statusCode == 404 {
            return .notFoundOrNotOwner
        }
        let bodyText = Self.truncate(
            String(data: raw.0, encoding: .utf8) ?? "<binary>"
        )
        throw DiscoveryClientError.http(
            status: httpResponse.statusCode,
            endpoint: "\(endpoint) (DELETE)",
            body: bodyText
        )
    }

    /// ADR-0028 revoke — issuer-signed delete of a group-invite blob. Sibling of
    /// ``revokeCallInvite(blobId:signingSeed:)``: signs the group-invite revoke
    /// canonical bytes with `signingSeed` (the issuer's ed25519 seed) and DELETEs
    /// the ledger row; the server verifies against the stored capability's issuer
    /// pubkey. 404 folds into `.notFoundOrNotOwner`.
    public func revokeGroupInvite(
        blobId: Data,
        signingSeed: Data
    ) async throws -> RevokeInviteResult {
        guard blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes"
            )
        }
        guard signingSeed.count == Self.signingSeedLength else {
            throw DiscoveryClientError.argument(
                "signingSeed must be \(Self.signingSeedLength) bytes"
            )
        }

        let signedAtMs = Int64(Date().timeIntervalSince1970 * 1000)
        let signature = try signGroupInviteRevocation(
            issuerSigningSeed: signingSeed,
            blobId: blobId,
            signedAtMs: UInt64(signedAtMs)
        )
        let slug = base64UrlEncodeNoPad(blobId)
        let endpoint = "/discovery/group-invite/\(slug)"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        attachAuthorizationHeader(to: &request)
        let body = InviteRevokeRequest(
            signedAtMs: signedAtMs,
            signature: base64Encode(signature)
        )
        do {
            request.httpBody = try encoder.encode(body)
        } catch {
            throw DiscoveryClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }

        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(
                endpoint: "\(endpoint) (DELETE)",
                underlying: error
            )
        }
        guard let httpResponse = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: "\(endpoint) (DELETE)",
                body: "non-HTTP response"
            )
        }
        if (200..<300).contains(httpResponse.statusCode) {
            return .revoked
        }
        if httpResponse.statusCode == 404 {
            return .notFoundOrNotOwner
        }
        let bodyText = Self.truncate(
            String(data: raw.0, encoding: .utf8) ?? "<binary>"
        )
        throw DiscoveryClientError.http(
            status: httpResponse.statusCode,
            endpoint: "\(endpoint) (DELETE)",
            body: bodyText
        )
    }

    /// Owner-signed list of every live invite this account owns
    /// (Phase 1.18d2b-list). Distinct Ed25519 domain separator
    /// from ``revokeInvite(blobId:signingSeed:)`` ensures a
    /// captured revoke signature cannot be replayed against this
    /// endpoint.
    ///
    /// Returns ``ListInvitesResult/list(_:)`` with the parsed
    /// rows on 200 (empty array when the owner has no live
    /// invites — distinct from a malformed-request failure), or
    /// ``ListInvitesResult/notFoundOrNotOwner`` on 404 (server
    /// folds "no identity_bundle for `accountId`" / "signature
    /// mismatch" into one status to deny status-code probing).
    /// Other non-2xx still throw.
    public func listInvitesByOwner(
        accountId: Data,
        signingSeed: Data
    ) async throws -> ListInvitesResult {
        guard accountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "accountId must be \(Self.accountIdLength) bytes"
            )
        }
        guard signingSeed.count == Self.signingSeedLength else {
            throw DiscoveryClientError.argument(
                "signingSeed must be \(Self.signingSeedLength) bytes"
            )
        }

        let signedAtMs = Int64(Date().timeIntervalSince1970 * 1000)
        let signature = try signOwnerListQuery(
            signingSeed: signingSeed,
            ownerAccountId: accountId,
            signedAtMs: UInt64(signedAtMs)
        )
        let endpoint = "/discovery/invite/by-owner"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        attachAuthorizationHeader(to: &request)
        let body = InviteOwnerListRequest(
            ownerAccountId: base64Encode(accountId),
            signedAtMs: signedAtMs,
            signature: base64Encode(signature)
        )
        do {
            request.httpBody = try encoder.encode(body)
        } catch {
            throw DiscoveryClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }

        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(
                endpoint: "\(endpoint) (POST)",
                underlying: error
            )
        }
        guard let httpResponse = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: "\(endpoint) (POST)",
                body: "non-HTTP response"
            )
        }
        if httpResponse.statusCode == 404 {
            return .notFoundOrNotOwner
        }
        if !(200..<300).contains(httpResponse.statusCode) {
            let bodyText = Self.truncate(
                String(data: raw.0, encoding: .utf8) ?? "<binary>"
            )
            throw DiscoveryClientError.http(
                status: httpResponse.statusCode,
                endpoint: "\(endpoint) (POST)",
                body: bodyText
            )
        }
        let decoded: InviteOwnerListResponse
        do {
            decoded = try decoder.decode(InviteOwnerListResponse.self, from: raw.0)
        } catch {
            throw DiscoveryClientError.http(
                status: httpResponse.statusCode,
                endpoint: "\(endpoint) (POST)",
                body: "failed to decode response: \(error)"
            )
        }
        let mapped = decoded.invites.map { entry in
            ActiveInvite(
                blobIdSlug: entry.blobIdSlug,
                expiryMs: entry.expiryMs,
                maxUses: entry.maxUses,
                usedCount: entry.usedCount,
                label: entry.label,
                bootstrapReferral: entry.bootstrapReferral,
                createdAt: entry.createdAt
            )
        }
        return .list(mapped)
    }

    /// Public-facing 404→nil GET fetch of a ledger blob. Carries no session and
    /// travels on the anonymous session like the consume that follows it.
    /// Pulled out so the invite-fetch implementation can reuse the
    /// existing helper without exposing it broadly.
    private func getNullableOn404Public<Res: Decodable>(
        endpoint: String
    ) async throws -> Res? {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: anonymousSession, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(
                endpoint: "\(endpoint) (GET)",
                underlying: error
            )
        }
        guard let httpResponse = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: "\(endpoint) (GET)",
                body: "non-HTTP response"
            )
        }
        if httpResponse.statusCode == 404 {
            return nil
        }
        if httpResponse.statusCode == 429 {
            throw DiscoveryClientError.rateLimited(endpoint: "\(endpoint) (GET)")
        }
        if !(200..<300).contains(httpResponse.statusCode) {
            let bodyText = Self.truncate(
                String(data: raw.0, encoding: .utf8) ?? "<binary>"
            )
            throw DiscoveryClientError.http(
                status: httpResponse.statusCode,
                endpoint: "\(endpoint) (GET)",
                body: bodyText
            )
        }
        return try decoder.decode(Res.self, from: raw.0)
    }

    // MARK: - Internal HTTP helpers

    /// One-shot bearer POST + decode. Used by every protected endpoint
    /// method that expects a non-nullable typed response.
    fileprivate func post<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req
    ) async throws -> Res {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        attachAuthorizationHeader(to: &request)
        do {
            request.httpBody = try encoder.encode(body)
        } catch {
            throw DiscoveryClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }
        return try await execute(endpoint: endpoint, request: request)
    }

    /// Attach the `Authorization: Bearer <token>` header when a session token
    /// is set. Only the protected (bearer) routes call this; the anonymous
    /// routes go through ``postAnonymousRaw(endpoint:body:context:)``, which
    /// never does.
    fileprivate func attachAuthorizationHeader(to request: inout URLRequest) {
        if let sessionToken {
            request.setValue("Bearer \(sessionToken)", forHTTPHeaderField: "Authorization")
        }
    }

    // MARK: - Anonymous routes (ADR-0054 S3)

    /// POST on an anonymous route: no `Authorization` (not even this client's
    /// own session), on ``anonymousSession``, spending one token of
    /// `context` when one is given (retried once with a second token on a
    /// token refusal). Returns the raw response whatever its status.
    fileprivate func postAnonymousRaw<Req: Encodable>(
        endpoint: String,
        body: Req,
        context: AnonTokenContext?
    ) async throws -> (Data, HTTPURLResponse) {
        let url = try buildURL(endpoint: endpoint)
        let encoded: Data
        do {
            encoded = try encoder.encode(body)
        } catch {
            throw DiscoveryClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }
        let perform: (String?) async throws -> (Data, HTTPURLResponse) = { tokenHeader in
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            if let tokenHeader {
                request.setValue(tokenHeader, forHTTPHeaderField: AnonTokenRoute.headerName)
            }
            request.httpBody = encoded
            let raw: (Data, URLResponse)
            do {
                raw = try await AntiCensorshipTransport.perform(
                    request, session: self.anonymousSession, config: self.antiCensorship)
            } catch {
                throw DiscoveryClientError.network(endpoint: endpoint, underlying: error)
            }
            guard let http = raw.1 as? HTTPURLResponse else {
                throw DiscoveryClientError.http(
                    status: 0, endpoint: endpoint, body: "non-HTTP response")
            }
            return (raw.0, http)
        }
        guard let context else { return try await perform(nil) }
        return try await AnonTokenRoute.send(context: context, tokens: tokens) { header in
            try await perform(header)
        }
    }

    /// ``postAnonymousRaw(endpoint:body:context:)`` + the typed decode.
    private func postAnonymous<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req,
        context: AnonTokenContext?
    ) async throws -> Res {
        let (data, http) = try await postAnonymousRaw(
            endpoint: endpoint, body: body, context: context)
        guard let decoded: Res = try decodeAnonymous(endpoint: endpoint, data: data, http: http) else {
            throw DiscoveryClientError.http(status: 404, endpoint: endpoint, body: "not found")
        }
        return decoded
    }

    /// The anonymous token-free lookups: 404 folds into `nil`.
    private func postAnonymousNullableOn404<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req
    ) async throws -> Res? {
        let (data, http) = try await postAnonymousRaw(endpoint: endpoint, body: body, context: nil)
        return try decodeAnonymous(endpoint: endpoint, data: data, http: http)
    }

    private func decodeAnonymous<Res: Decodable>(
        endpoint: String,
        data: Data,
        http: HTTPURLResponse
    ) throws -> Res? {
        if http.statusCode == 404 { return nil }
        if http.statusCode == 429 { throw DiscoveryClientError.rateLimited(endpoint: endpoint) }
        guard (200..<300).contains(http.statusCode) else {
            throw DiscoveryClientError.http(
                status: http.statusCode,
                endpoint: endpoint,
                body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>")
            )
        }
        do {
            return try decoder.decode(Res.self, from: data)
        } catch {
            throw DiscoveryClientError.http(
                status: http.statusCode,
                endpoint: endpoint,
                body: "decode failed: \(error)"
            )
        }
    }

    /// `GET` with a 404→nil fold, no request body — used by the
    /// `/discovery/identity/{slug}` and `/discovery/keypackage/{slug}`
    /// fetch paths.
    private func getNullableOn404<Res: Decodable>(
        endpoint: String
    ) async throws -> Res? {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        return try await executeNullableOn404(
            endpoint: "\(endpoint) (GET)",
            request: request
        )
    }

    fileprivate func buildURL(endpoint: String) throws -> URL {
        guard let url = URL(string: baseURL + endpoint) else {
            throw DiscoveryClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        return url
    }

    fileprivate func execute<Res: Decodable>(
        endpoint: String,
        request: URLRequest
    ) async throws -> Res {
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(
                endpoint: endpoint,
                underlying: error
            )
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            let bodyText = Self.truncate(
                String(data: data, encoding: .utf8) ?? "<binary>"
            )
            throw DiscoveryClientError.http(
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
            throw DiscoveryClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: "decode failed: \(error) [body=\(bodyText)]"
            )
        }
    }

    /// 404-tolerant variant of ``execute(endpoint:request:)``.
    private func executeNullableOn404<Res: Decodable>(
        endpoint: String,
        request: URLRequest
    ) async throws -> Res? {
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                request, session: session, config: antiCensorship)
        } catch {
            throw DiscoveryClientError.network(
                endpoint: endpoint,
                underlying: error
            )
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        if httpResponse.statusCode == 404 {
            return nil
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            let bodyText = Self.truncate(
                String(data: data, encoding: .utf8) ?? "<binary>"
            )
            throw DiscoveryClientError.http(
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
            throw DiscoveryClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: "decode failed: \(error) [body=\(bodyText)]"
            )
        }
    }

    fileprivate static func truncate(_ s: String, max: Int = 512) -> String {
        if s.count > max {
            return String(s.prefix(max)) + "…"
        }
        return s
    }

    /// Lowercase hex, no separators — the 32-char wire form the
    /// `services/channels` REST routes accept. Local to this client so
    /// ``lookupChannel(handle:)`` can return a channel-id hex without
    /// coupling the discovery module to `ChannelsComposer.bytesToHex`.
    fileprivate static func hexEncode(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }

    // MARK: - Constants

    /// Raw 16-byte UUIDv7.
    fileprivate static let accountIdLength: Int = 16

    /// Raw 16-byte channel_id pseudonym (ADR-0010) — same length as an
    /// account_id but a distinct namespace.
    fileprivate static let channelIdLength: Int = 16

    /// Ed25519 + X25519 both use 32-byte public keys.
    fileprivate static let pubkeyLength: Int = 32

    /// Mirror of `services/discovery`'s `MAX_KEYPACKAGE_BYTES`.
    /// Surfacing the ceiling here saves a round-trip on a
    /// misbehaving caller.
    fileprivate static let maxKeyPackageBytes: Int = 2048

    /// A sealed blob must be non-empty and within the ledger's ceiling.
    fileprivate static func checkSealed(_ sealed: Data, ceiling: Int) throws {
        guard !sealed.isEmpty else {
            throw DiscoveryClientError.argument("sealed link must be non-empty")
        }
        guard sealed.count <= ceiling else {
            throw DiscoveryClientError.argument(
                "sealed link exceeds the \(ceiling)-byte ceiling (got \(sealed.count) bytes)")
        }
    }

    /// Parse the server-minted ledger id.
    fileprivate static func parseBlobId(_ slug: String, endpoint: String) throws -> Data {
        guard let blobId = base64UrlDecodeNoPad(slug), blobId.count == blobIdLength else {
            throw DiscoveryClientError.http(
                status: 200, endpoint: endpoint, body: "server returned malformed blob_id")
        }
        return blobId
    }

    /// The `/i/` sealed-blob ceiling (`lakko_contacts::invite::MAX_SEALED_LEN`).
    fileprivate static let maxCapabilityBytes: Int = 254

    /// Mirror of `services/discovery`'s `MAX_CALL_CAPABILITY_BYTES`
    /// — the call-invite ledger uses an 8192-byte ceiling
    /// (vs. contact-invite's 2048) because `CallInviteCapability`
    /// embeds a one-time MLS KeyPackage (~4 KiB upper bound per
    /// `MAX_KEY_PACKAGE_LEN`).
    fileprivate static let maxCallCapabilityBytes: Int = 4338

    /// Mirror of `services/discovery`'s group-invite `MAX_CAPABILITY_BYTES`
    /// (ADR-0028) — the group join-ticket wire is leaner than a call-invite
    /// (no embedded KeyPackage); it tops out near 363 bytes, so the ledger
    /// column + this client-side ceiling are 512.
    fileprivate static let maxGroupCapabilityBytes: Int = 938

    /// The ledger id of an invite (`/i/`, `/c/`, `/g/`): a random 6-byte id
    /// the discovery service mints at upload (ADR-0028 amendment of
    /// 2026-09-27). Read from the core so it cannot drift from the server.
    static var blobIdLength: Int { Int(inviteBlobIdLen()) }

    /// Ed25519 signing seed length.
    fileprivate static let signingSeedLength: Int = 32
}

// MARK: - Result types

/// Result of ``DiscoveryClient/registerUsername(accountId:username:)``.
public struct RegisterResult: Equatable {
    /// The canonical form actually claimed (after NFKC + lowercase +
    /// trim + leading-`@` strip). Surface to the UI so the user
    /// sees what handle is now bound to their account.
    public let normalisedUsername: String

    public init(normalisedUsername: String) {
        self.normalisedUsername = normalisedUsername
    }
}

/// Result of ``DiscoveryClient/lookup(username:)``.
/// A person in Everyone found by discovery (ADR-0060 section 5): the account id
/// and the public record, opened with the OPRF **output** of the username or
/// name term that found it (`openPublicRecord`). `publicRecord` is `nil` only
/// when the server sent none or it did not decode.
public struct PublicAccountHit: Equatable {
    public let accountId: Data
    public let publicRecord: Data?

    public init(accountId: Data, publicRecord: Data?) {
        self.accountId = accountId
        self.publicRecord = publicRecord
    }
}

public struct LookupResult: Equatable {
    public let normalisedUsername: String
    /// Raw 16-byte account_id, or `nil` if the username is not
    /// registered.
    public let account: Data?

    public init(normalisedUsername: String, account: Data?) {
        self.normalisedUsername = normalisedUsername
        self.account = account
    }
}

/// Result of ``DiscoveryClient/consumeInvite(blobId:capabilityBytes:joinerAccountId:)``.
/// Discriminated so the joiner UI can branch cleanly on the four
/// observable outcomes without `as?`-walking the underlying
/// `DiscoveryClientError` hierarchy.
public enum ConsumeInviteResult: Equatable {
    /// Server accepted the consume + bumped the counter + recorded
    /// this joiner in the dedup side table. Caller proceeds with
    /// `sendContactRequestToBundle(...)`.
    case consumed(usedCount: Int, maxUses: Int64)
    /// Server's `(blob_id, joiner_tag)` PK already had a row — the
    /// counter was NOT bumped. UI surfaces "you've already
    /// accepted this invite".
    case alreadyConsumed(usedCount: Int, maxUses: Int64)
    /// 410 Gone — the `max_uses` ceiling was hit by other joiners
    /// before this consume could land.
    case exhausted
    /// 404 — never-issued OR expired (server folds both into the
    /// same status to deny status-code probing).
    case notFound
}

/// Result of ``DiscoveryClient/revokeInvite(blobId:signingSeed:)``.
public enum RevokeInviteResult: Equatable {
    /// Server deleted the row (2xx).
    case revoked
    /// 404. Server folds "no row" / "signature did not verify" /
    /// "stored bytes failed re-decode" into the same status to
    /// deny status-code probing of the blob_id space.
    case notFoundOrNotOwner
}

/// One row in the
/// ``DiscoveryClient/listInvitesByOwner(accountId:signingSeed:)``
/// response — normalised TS-parity view of the wire shape
/// (Phase 1.18d2b-list). `blobIdSlug` is the URL-safe base64
/// the owner uses to reconstruct the share URL or feed back
/// into ``DiscoveryClient/revokeInvite(blobId:signingSeed:)``.
public struct ActiveInvite: Equatable {
    /// URL-safe base64 (no padding) of the 6-byte blob_id —
    /// same `/i/<slug>` segment that appears in the share URL.
    public let blobIdSlug: String
    /// UNIX millis the capability expires at. `0` means never
    /// expires (the `NEVER_EXPIRES` sentinel).
    public let expiryMs: UInt64
    /// Wire-format `max_uses` sentinel: `0` = SINGLE_USE,
    /// `0xFFFFFFFF` = unlimited, any other N = N consumes allowed.
    public let maxUses: UInt32
    /// Server-counted consumes.
    public let usedCount: UInt32
    /// Owner-supplied free-text label; empty string when not set.
    public let label: String
    /// `true` if the capability flags the link as a referral
    /// signup-deep-link (Phase 1.9b4).
    public let bootstrapReferral: Bool
    /// RFC 3339 timestamp the server recorded on first upload.
    public let createdAt: String

    public init(
        blobIdSlug: String,
        expiryMs: UInt64,
        maxUses: UInt32,
        usedCount: UInt32,
        label: String,
        bootstrapReferral: Bool,
        createdAt: String
    ) {
        self.blobIdSlug = blobIdSlug
        self.expiryMs = expiryMs
        self.maxUses = maxUses
        self.usedCount = usedCount
        self.label = label
        self.bootstrapReferral = bootstrapReferral
        self.createdAt = createdAt
    }
}

/// Result of
/// ``DiscoveryClient/listInvitesByOwner(accountId:signingSeed:)``.
public enum ListInvitesResult: Equatable {
    /// Server returned the live-invite list (possibly empty).
    case list([ActiveInvite])
    /// 404. Server folds "no identity_bundle for `accountId`" /
    /// "signature mismatch" / "stored pubkey malformed" into
    /// one status to deny status-code probing.
    case notFoundOrNotOwner
}

// MARK: - Errors

/// Error vocabulary for ``DiscoveryClient``.
public enum DiscoveryClientError: Error {
    /// Programmer error — empty / invalid inputs caught before any
    /// network round-trip. Distinct from HTTP errors so the UI can
    /// surface a form-validation message.
    case argument(String)
    /// Server returned a non-2xx status. Carries the original status
    /// + endpoint so the UI can branch on (e.g.) 409 = taken,
    /// 404 = unknown.
    case http(status: Int, endpoint: String, body: String)
    /// Network-level failure (DNS, TLS, no route, …). Distinct from
    /// `http` so the UI can suggest "check connection".
    case network(endpoint: String, underlying: Error)
    /// `429` from one of the rate-limited invite reads (fetch / consume).
    /// The link may be perfectly good: the server is asking this network
    /// address to slow down. Never to be shown as "link invalid".
    case rateLimited(endpoint: String)

    /// Whether `error` is, or wraps, a ``rateLimited(endpoint:)`` refusal.
    /// Looks through the repository wrappers (`ChatError.wrapped`,
    /// `ContactsError.wrapped`) so a screen can pick the "try again later"
    /// copy without knowing which layer threw.
    public static func isRateLimited(_ error: Error) -> Bool {
        if let discovery = error as? DiscoveryClientError, case .rateLimited = discovery {
            return true
        }
        if let chat = error as? ChatError, case .wrapped(_, let underlying) = chat {
            return isRateLimited(underlying)
        }
        if let contacts = error as? ContactsError, case .wrapped(let underlying) = contacts {
            return isRateLimited(underlying)
        }
        return false
    }
}

extension DiscoveryClientError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .http(let status, let endpoint, let body):
            return "discovery \(endpoint) → \(status): \(body)"
        case .network(let endpoint, let underlying):
            return "discovery \(endpoint) network error: \(underlying)"
        case .rateLimited(let endpoint):
            return "discovery \(endpoint) → 429: rate limited"
        }
    }
}

/// One device of an account, as the directory serves it (ADR-0050).
public struct DeviceKeyPackage: Equatable {
    /// The device leaf's 32-byte Ed25519 signature key.
    public let deviceId: Data
    /// TLS-encoded MLS KeyPackage bytes.
    public let keyPackage: Data

    public init(deviceId: Data, keyPackage: Data) {
        self.deviceId = deviceId
        self.keyPackage = keyPackage
    }
}

// MARK: - ADR-0060 wire shapes

private struct UsernameCertificateWire: Decodable {
    let usernameCertificateB64: String
    enum CodingKeys: String, CodingKey { case usernameCertificateB64 = "username_certificate_b64" }
}

private struct VisibilityWireRequest: Encodable {
    let isPublic: Bool
    let nameTermsB64: [String]
    let publicRecordB64: String?
    enum CodingKeys: String, CodingKey {
        case isPublic = "public"
        case nameTermsB64 = "name_terms_b64"
        case publicRecordB64 = "public_record_b64"
    }

    // The server refuses the field outside Everyone, so a nil record is
    // omitted rather than sent as null.
    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(isPublic, forKey: .isPublic)
        try c.encode(nameTermsB64, forKey: .nameTermsB64)
        try c.encodeIfPresent(publicRecordB64, forKey: .publicRecordB64)
    }
}

private struct NameLookupWireRequest: Encodable {
    let oprfHashB64: String
    enum CodingKeys: String, CodingKey { case oprfHashB64 = "oprf_hash_b64" }
}

private struct NameLookupWireResponse: Decodable {
    struct Row: Decodable {
        let accountIdB64: String
        let publicRecordB64: String
        enum CodingKeys: String, CodingKey {
            case accountIdB64 = "account_id_b64"
            case publicRecordB64 = "public_record_b64"
        }
    }
    let results: [Row]
}

private struct CertKeyWire: Decodable {
    let keyB64url: String
    enum CodingKeys: String, CodingKey { case keyB64url = "key_b64url" }
}
