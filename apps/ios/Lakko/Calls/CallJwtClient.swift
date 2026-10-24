// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0078 section 2: `POST /call/jwt` takes no session. It spends one
// anonymous `call-join` token (``AnonTokenRoute``, on ``AnonymousHTTP``) and
// names the ROOM, computed here from the call id (`sfuRoomForCall`), so
// neither the issuer nor the SFU sees the call id or learns which account
// joins. The token names the room and a random peer id; a TURN credential
// with a random username tag comes back beside it.

/// Client-side HTTP transport for the Phase 3b4 call-admission JWT
/// pipeline — iOS mirror of the web `CallJwtClient` (apps/web/src/calls/
/// call-jwt-client.ts).
///
///   * ``mint(callId:role:)``             — `POST /call/jwt`, paid for by
///                                          one anonymous `call-join`
///                                          token. Returns the JWT, its
///                                          parsed (UNVERIFIED) claims and
///                                          the TURN credential.
///   * ``fetchJwks()``                    — `GET /.well-known/jwks.json`.
///                                          Anonymous; caller caches.
///   * ``parseUnverified(jwt:)``          — Wraps the UniFFI
///                                          `parseCallJwtUnverified`
///                                          free function.
///   * ``verifyAgainstJwks(jwt:jwksDocBytes:nowUnixSecs:)``
///                                        — Defensive self-verify via
///                                          UniFFI `verifyCallJwtWithJwks`.
///
/// The crypto + wire-format primitives live in `lakko-call-jwt` (Rust)
/// and are surfaced through UniFFI as the
/// `parseCallJwtUnverified` + `verifyCallJwtWithJwks` free functions
/// plus the `FfiCallJwtClaims` Record + `FfiCallRole` Enum. This file
/// is the **HTTP transport** layer on top of them.
///
/// See `docs/security/calls.md` C-15..C-23 for the invariants the
/// underlying primitives preserve.
public final class CallJwtClient: @unchecked Sendable {
    public static var defaultBaseURL: String { ApiEndpoints.baseURL(for: .auth) }

    /// Default JWKS cache TTL per ADR-0008 — 24 h.
    public static let defaultJwksTtl: TimeInterval = 24 * 60 * 60

    public let baseURL: String
    public let jwksTtl: TimeInterval

    /// `URLSession` is injectable so tests can use an ephemeral
    /// session with a stub `URLProtocol`.
    private let session: URLSession
    private let anonymousSession: URLSession
    private let tokens: AnonTokenSource

    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    private let stateLock = NSLock()
    private var jwksCache: JwksCacheEntry?

    public init(
        baseURL: String = CallJwtClient.defaultBaseURL,
        session: URLSession = .shared,
        jwksTtl: TimeInterval = CallJwtClient.defaultJwksTtl,
        tokens: AnonTokenSource = AnonTokenPool.shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.anonymousSession = AnonymousHTTP.session(pairedWith: session)
        self.tokens = tokens
        self.jwksTtl = max(jwksTtl, 0)
        let encoder = JSONEncoder()
        encoder.outputFormatting = []
        self.encoder = encoder
        self.decoder = JSONDecoder()
    }


    /// Drop the cached JWKS doc. Call after a verify failure
    /// that suggests a key rotation has happened (the
    /// convenience ``verifyJwt(jwt:nowUnixSecs:)`` does this
    /// for you).
    public func clearJwksCache() {
        stateLock.lock()
        defer { stateLock.unlock() }
        jwksCache = nil
    }


    private func cachedJwksIfFresh() -> Data? {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard let entry = jwksCache else { return nil }
        let age = Date().timeIntervalSince(entry.fetchedAt)
        return age < jwksTtl ? entry.bytes : nil
    }

    private func storeJwks(_ bytes: Data) {
        stateLock.lock()
        defer { stateLock.unlock() }
        jwksCache = JwksCacheEntry(bytes: bytes, fetchedAt: Date())
    }

    // MARK: - Mint

    /// ``mint(room:role:)`` for the room of the 16-byte `callId`.
    public func mint(callId: Data, role: CallRole = .participant) async throws -> MintCallJwtResult {
        let room: String
        do {
            room = try sfuRoomForCall(callId: callId)
        } catch {
            throw CallJwtClientError.argument("call id must be 16 bytes: \(error)")
        }
        return try await mint(room: room, role: role)
    }

    /// `POST /call/jwt` for `room` (64 lower-case hex), paid for by one
    /// `call-join` token. Throws ``AnonTokenUnavailable`` when no token can
    /// be had, ``CallJwtClientError/http(status:endpoint:body:)`` on any other
    /// non-2xx (`429 room-admission-limit` included).
    public func mint(room: String, role: CallRole = .participant) async throws -> MintCallJwtResult {
        guard CallJwtClient.isValidRoom(room) else {
            throw CallJwtClientError.argument(
                "room must be 64 lower-case hex chars, got \(room.count) chars"
            )
        }
        let endpoint = "/call/jwt"
        let url = try buildURL(endpoint: endpoint)
        let body: Data
        do {
            body = try encoder.encode(IssueRequest(room: room, role: role))
        } catch {
            throw CallJwtClientError.argument("failed to encode \(endpoint) body: \(error)")
        }
        let (data, http) = try await AnonTokenRoute.send(context: .callJoin, tokens: tokens) { header in
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue(header, forHTTPHeaderField: AnonTokenRoute.headerName)
            request.httpBody = body
            let raw: (Data, URLResponse)
            do {
                raw = try await AntiCensorshipTransport.perform(
                    request, session: self.anonymousSession, config: .current)
            } catch {
                throw CallJwtClientError.network(endpoint: endpoint, underlying: error)
            }
            guard let http = raw.1 as? HTTPURLResponse else {
                throw CallJwtClientError.http(status: 0, endpoint: endpoint, body: "non-HTTP response")
            }
            return (raw.0, http)
        }
        guard (200..<300).contains(http.statusCode) else {
            throw CallJwtClientError.http(
                status: http.statusCode,
                endpoint: endpoint,
                body: truncate(String(data: data, encoding: .utf8) ?? "<binary>")
            )
        }
        let response: IssueResponse
        do {
            response = try decoder.decode(IssueResponse.self, from: data)
        } catch {
            throw CallJwtClientError.http(status: 200, endpoint: endpoint, body: "decode failed: \(error)")
        }
        let claims = try parseUnverified(jwt: response.jwt)
        return MintCallJwtResult(
            jwt: response.jwt,
            expiresAtUnixSecs: response.expiresAtUnixSecs,
            claims: claims,
            turn: response.turn.flatMap(TurnCredential.init(wire:))
        )
    }

    // MARK: - JWKS fetch

    /// `GET /.well-known/jwks.json` — fetches the auth service's
    /// published JWK Set. The returned `Data` is the raw JSON
    /// document; pass it into ``verifyAgainstJwks(jwt:jwksDocBytes:nowUnixSecs:)``
    /// or hold it.
    ///
    /// Anonymous endpoint — no bearer required.
    ///
    /// **Caches in-memory for ``jwksTtl`` seconds** (default
    /// 24 h per ADR-0008). Pass `forceRefresh = true` to
    /// bypass the cache; on a `kid` mismatch during verify
    /// the convenience ``verifyJwt(jwt:nowUnixSecs:)`` does
    /// this for you. Direct invalidation via
    /// ``clearJwksCache()``.
    public func fetchJwks(forceRefresh: Bool = false) async throws -> Data {
        if !forceRefresh, let cached = cachedJwksIfFresh() {
            return cached
        }
        let endpoint = "/.well-known/jwks.json"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let bytes = try await executeRaw(endpoint: endpoint, request: request)
        storeJwks(bytes)
        return bytes
    }

    // MARK: - Parse / Verify

    /// Parse the JWT WITHOUT verifying the signature. Useful
    /// for UI surfaces reading role / expiry off a JWT the
    /// caller already has. The result should NOT drive any
    /// authorisation decision; the SFU does the load-bearing
    /// verification.
    public func parseUnverified(jwt: String) throws -> ParsedCallJwt {
        let claims: FfiCallJwtClaims
        do {
            claims = try parseCallJwtUnverified(jwt: jwt)
        } catch {
            throw CallJwtClientError.verify(reason: "parseCallJwtUnverified", underlying: error)
        }
        return ParsedCallJwt(ffi: claims)
    }

    /// Verify a JWT against a cached JWKS doc. Returns the
    /// cryptographically-validated claim set. `nowUnixSecs`
    /// defaults to the current wall clock; tests inject a
    /// fixed value to drive the `exp` check.
    public func verifyAgainstJwks(
        jwt: String,
        jwksDocBytes: Data,
        nowUnixSecs: UInt64? = nil
    ) throws -> ParsedCallJwt {
        let now = nowUnixSecs ?? UInt64(Date().timeIntervalSince1970)
        let claims: FfiCallJwtClaims
        do {
            claims = try verifyCallJwtWithJwks(
                jwksDocBytes: jwksDocBytes,
                jwt: jwt,
                nowUnixSecs: now
            )
        } catch {
            throw CallJwtClientError.verify(reason: "verifyCallJwtWithJwks", underlying: error)
        }
        return ParsedCallJwt(ffi: claims)
    }

    /// Convenience: fetch JWKS (cache-aware) + verify in one
    /// call. On a `no JWK matches header.kid` failure — the
    /// canonical key-rotation signal — invalidates the
    /// cache, refetches once, retries the verify. Any second
    /// failure surfaces verbatim.
    public func verifyJwt(
        jwt: String,
        nowUnixSecs: UInt64? = nil
    ) async throws -> ParsedCallJwt {
        let cached = try await fetchJwks()
        do {
            return try verifyAgainstJwks(jwt: jwt, jwksDocBytes: cached, nowUnixSecs: nowUnixSecs)
        } catch let firstErr {
            guard CallJwtClient.isKidMismatch(firstErr) else {
                throw firstErr
            }
            clearJwksCache()
            let fresh = try await fetchJwks(forceRefresh: true)
            return try verifyAgainstJwks(jwt: jwt, jwksDocBytes: fresh, nowUnixSecs: nowUnixSecs)
        }
    }

    // MARK: - Helpers

    /// Mint a 16-byte random `callId` and render it as 32
    /// lower-case hex chars — the shape the server expects.
    public static func mintCallId() -> String {
        var bytes = [UInt8](repeating: 0, count: 16)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        precondition(status == errSecSuccess, "SecRandomCopyBytes failed: \(status)")
        return bytes.map { String(format: "%02x", $0) }.joined()
    }

    /// Discriminate the `no JWK matches header.kid` shape
    /// from unrelated verify failures. UniFFI surfaces the
    /// Rust `JwtError::Format(...)` message via the `verify`
    /// case's underlying error; we string-match on the
    /// substring `lakko-call-jwt` emits.
    private static func isKidMismatch(_ err: Error) -> Bool {
        if case let CallJwtClientError.verify(_, underlying) = err {
            let message = String(describing: underlying)
            return message.contains("no JWK matches header.kid") || message.contains("kid")
        }
        return false
    }

    /// Whether `room` is 64 lower-case hex characters.
    static func isValidRoom(_ room: String) -> Bool {
        room.utf8.count == 64 && room.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }

    private func buildURL(endpoint: String) throws -> URL {
        guard let url = URL(string: baseURL + endpoint) else {
            throw CallJwtClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        return url
    }

    private func execute<Res: Decodable>(
        endpoint: String,
        request: URLRequest
    ) async throws -> Res {
        let data = try await executeRaw(endpoint: endpoint, request: request)
        do {
            return try decoder.decode(Res.self, from: data)
        } catch {
            let bodyText = truncate(String(data: data, encoding: .utf8) ?? "<binary>")
            throw CallJwtClientError.http(
                status: 200,
                endpoint: endpoint,
                body: "decode failed: \(error) [body=\(bodyText)]"
            )
        }
    }

    private func executeRaw(
        endpoint: String,
        request: URLRequest
    ) async throws -> Data {
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        } catch {
            throw CallJwtClientError.network(endpoint: endpoint, underlying: error)
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw CallJwtClientError.http(status: 0, endpoint: endpoint, body: "non-HTTP response")
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            let bodyText = truncate(String(data: data, encoding: .utf8) ?? "<binary>")
            throw CallJwtClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: bodyText
            )
        }
        return data
    }

    private func truncate(_ s: String, max: Int = 512) -> String {
        if s.count > max {
            return String(s.prefix(max)) + "…"
        }
        return s
    }
}

// MARK: - Public types

/// Participation role baked into the minted JWT. Mirror of
/// the Rust `CallRole` enum; the wire form is lower-case.
public enum CallRole: String, Codable, Sendable, Equatable {
    case participant
    case publisher
    case subscriber
}

/// Flat claim set returned by ``CallJwtClient/parseUnverified(jwt:)``
/// and ``CallJwtClient/verifyAgainstJwks(jwt:jwksDocBytes:nowUnixSecs:)``.
public struct ParsedCallJwt: Equatable, Sendable {
    public let iss: String
    public let aud: String
    /// The room, 64 lower-case hex characters (ADR-0078 section 2: no
    /// account, no call id in the token).
    public let room: String
    /// The SFU peer id, 32 lower-case hex characters, random per token.
    public let peerId: String
    public let role: CallRole
    public let iat: UInt64
    public let exp: UInt64
    public let kid: String

    init(ffi: FfiCallJwtClaims) {
        self.iss = ffi.iss
        self.aud = ffi.aud
        self.room = ffi.room
        self.peerId = ffi.peerId
        self.role = ParsedCallJwt.role(from: ffi.role)
        self.iat = ffi.iat
        self.exp = ffi.exp
        self.kid = ffi.kid
    }

    private static func role(from ffi: FfiCallRole) -> CallRole {
        switch ffi {
        case .participant: return .participant
        case .publisher: return .publisher
        case .subscriber: return .subscriber
        }
    }
}

/// Result of ``CallJwtClient/mint(callId:role:)``. The
/// `claims` field is the **unverified** parse of the JWT
/// body — fine for UI surfaces; production callers that
/// need a verifying parse should call
/// ``CallJwtClient/verifyAgainstJwks(jwt:jwksDocBytes:nowUnixSecs:)``
/// and read THAT result instead.
public struct MintCallJwtResult: Equatable, Sendable {
    public let jwt: String
    public let expiresAtUnixSecs: UInt64
    public let claims: ParsedCallJwt
    /// The TURN credential minted beside the token, or `nil` when the
    /// deployment has no relay (the call proceeds on host candidates alone).
    public let turn: TurnCredential?
}

/// A TURN credential in the coturn REST shape (`services/auth/src/turn.rs`):
/// the username's tag is random per credential (ADR-0078 section 2).
public struct TurnCredential: Equatable, Sendable {
    public let urls: [String]
    public let username: String
    public let password: String

    init?(wire: TurnCredentialWire) {
        let urls = wire.urls.filter { !$0.isEmpty }
        guard !urls.isEmpty, !wire.username.isEmpty, !wire.password.isEmpty else { return nil }
        self.urls = urls
        self.username = wire.username
        self.password = wire.password
    }
}

/// The `turn` object of a `POST /call/jwt` answer.
struct TurnCredentialWire: Decodable {
    let urls: [String]
    let username: String
    let password: String
}

/// Typed errors surfaced by ``CallJwtClient``.
public enum CallJwtClientError: Error, CustomStringConvertible {
    /// Caller misuse — bad arg shape before any I/O.
    case argument(String)
    /// Server returned a non-2xx response.
    case http(status: Int, endpoint: String, body: String)
    /// Network transport failure — DNS, TLS, abort, etc.
    case network(endpoint: String, underlying: Error)
    /// UniFFI verify path rejected.
    case verify(reason: String, underlying: Error)

    public var description: String {
        switch self {
        case .argument(let m): return "argument: \(m)"
        case .http(let s, let e, let b): return "http \(s) on \(e): \(b)"
        case .network(let e, let u): return "network on \(e): \(u)"
        case .verify(let r, let u): return "verify \(r): \(u)"
        }
    }
}

// MARK: - Wire shape (private)

/// JWKS in-memory cache entry. Lifetime is the
/// `CallJwtClient` instance.
private struct JwksCacheEntry {
    let bytes: Data
    let fetchedAt: Date
}

/// `POST /call/jwt` request shape: the room and the role, nothing else.
private struct IssueRequest: Encodable {
    let room: String
    let role: CallRole
}

/// `POST /call/jwt` response shape.
private struct IssueResponse: Decodable {
    let jwt: String
    let expiresAtUnixSecs: UInt64
    let turn: TurnCredentialWire?

    enum CodingKeys: String, CodingKey {
        case jwt
        case expiresAtUnixSecs = "expires_at_unix_secs"
        case turn
    }
}
