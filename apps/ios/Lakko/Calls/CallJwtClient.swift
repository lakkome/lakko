// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Client-side HTTP transport for the Phase 3b4 call-admission JWT
/// pipeline — iOS mirror of the web `CallJwtClient` (apps/web/src/calls/
/// call-jwt-client.ts).
///
///   * ``mint(callId:role:)``             — `POST /call/jwt` with a
///                                          bearer session token.
///                                          Returns the JWT + parsed
///                                          (UNVERIFIED) claims.
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
    public static let defaultBaseURL = "http://127.0.0.1:8081"

    /// Default JWKS cache TTL per ADR-0008 — 24 h.
    public static let defaultJwksTtl: TimeInterval = 24 * 60 * 60

    public let baseURL: String
    public let jwksTtl: TimeInterval

    /// `URLSession` is injectable so tests can use an ephemeral
    /// session with a stub `URLProtocol`.
    private let session: URLSession

    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    private let stateLock = NSLock()
    private var sessionToken: String?
    private var jwksCache: JwksCacheEntry?

    public init(
        baseURL: String = CallJwtClient.defaultBaseURL,
        session: URLSession = .shared,
        jwksTtl: TimeInterval = CallJwtClient.defaultJwksTtl
    ) {
        self.baseURL = baseURL
        self.session = session
        self.jwksTtl = max(jwksTtl, 0)
        let encoder = JSONEncoder()
        encoder.outputFormatting = []
        self.encoder = encoder
        self.decoder = JSONDecoder()
    }

    /// Rebind the bearer token. `nil` clears (sign-out).
    public func setSessionToken(_ token: String?) {
        stateLock.lock()
        defer { stateLock.unlock() }
        sessionToken = token
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

    private func currentSessionToken() -> String? {
        stateLock.lock()
        defer { stateLock.unlock() }
        return sessionToken
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

    /// `POST /call/jwt` — mints a JWT scoped to
    /// `(account_id-from-bearer, callId, role)`. Returns the
    /// raw JWS Compact representation along with the
    /// **unverified** parsed claims for UI convenience.
    ///
    /// - Throws ``CallJwtClientError/argument(_:)`` when `callId`
    ///   doesn't match `^[0-9a-f]{32}$` or no bearer token is set.
    /// - Throws ``CallJwtClientError/http(status:endpoint:body:)``
    ///   on a non-2xx response (401 on missing / bad bearer,
    ///   400 on the server's own `call_id` validation, 500 on
    ///   a mint failure).
    /// - Throws ``CallJwtClientError/network(endpoint:underlying:)``
    ///   on transport errors.
    public func mint(callId: String, role: CallRole = .participant) async throws -> MintCallJwtResult {
        guard CallJwtClient.isValidCallId(callId) else {
            throw CallJwtClientError.argument(
                "call_id must be 32 lower-case hex chars, got \(callId.count) chars"
            )
        }
        guard let token = currentSessionToken() else {
            throw CallJwtClientError.argument(
                "session token is not bound — call setSessionToken before mint()"
            )
        }
        let endpoint = "/call/jwt"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        do {
            request.httpBody = try encoder.encode(IssueRequest(callId: callId, role: role))
        } catch {
            throw CallJwtClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }
        let response: IssueResponse = try await execute(endpoint: endpoint, request: request)
        let claims = try parseUnverified(jwt: response.jwt)
        return MintCallJwtResult(
            jwt: response.jwt,
            expiresAtUnixSecs: response.expiresAtUnixSecs,
            claims: claims
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

    private static let callIdPattern: NSRegularExpression = {
        do {
            return try NSRegularExpression(pattern: "^[0-9a-f]{32}$", options: [])
        } catch {
            preconditionFailure("call_id regex compile failed: \(error)")
        }
    }()

    private static func isValidCallId(_ callId: String) -> Bool {
        let range = NSRange(callId.startIndex..., in: callId)
        return callIdPattern.firstMatch(in: callId, options: [], range: range) != nil
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
    public let sub: String
    public let callId: String
    public let role: CallRole
    public let iat: UInt64
    public let exp: UInt64
    public let kid: String

    init(ffi: FfiCallJwtClaims) {
        self.iss = ffi.iss
        self.aud = ffi.aud
        self.sub = ffi.sub
        self.callId = ffi.callId
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

/// `POST /call/jwt` request shape — snake_case keys match
/// the server. Kept private; callers use ``CallJwtClient/mint(callId:role:)``.
private struct IssueRequest: Encodable {
    let callId: String
    let role: CallRole

    enum CodingKeys: String, CodingKey {
        case callId = "call_id"
        case role
    }
}

/// `POST /call/jwt` response shape.
private struct IssueResponse: Decodable {
    let jwt: String
    let expiresAtUnixSecs: UInt64

    enum CodingKeys: String, CodingKey {
        case jwt
        case expiresAtUnixSecs = "expires_at_unix_secs"
    }
}
