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
///      orchestrate the multi-round-trip flows: PoW₁ → OPRF blind
///      → POST `/oprf` → finalize → PoW₂ → POST `/register` (or
///      `/lookup`); and the one-PoW identity-bundle upload.
///
/// ## Why a separate PoW challenge?
///
/// `services/discovery` keeps its own PoW challenge table; the auth
/// service's seeds are not accepted here. We call
/// `POST /pow/challenge` on the discovery base URL (not the auth
/// one) — the JSON shape happens to match, so we reuse
/// ``PowChallengeResponse`` from `apps/ios/Lakko/Auth/Wire.swift`.
///
/// ## Default base URL
///
/// `http://localhost:8084` — the iOS simulator shares the host's
/// loopback (unlike Android emulator which needs `10.0.2.2`), port
/// `8084` matching the `LAKKO_BIND` dev setup for
/// `lakko-svc-discovery`. Override via the constructor for physical
/// devices (LAN IP) and production (Cloudflare tunnel URL).
public final class DiscoveryClient {

    // MARK: - Configuration

    /// Default base URL. See class-level docs.
    public static let defaultBaseURL: String = "http://localhost:8084"

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
    /// 401 in that mode, but unauthenticated paths (`/pow/challenge`,
    /// `/discovery/oprf`, `/discovery/lookup`, all GETs, invite
    /// consume) still work.
    ///
    /// `private(set)` so callers update via ``setSessionToken(_:)``;
    /// reads are non-blocking from any actor (the underlying
    /// `URLSession` is thread-safe).
    public private(set) var sessionToken: String?

    public init(
        baseURL: String = DiscoveryClient.defaultBaseURL,
        session: URLSession = .shared,
        sessionToken: String? = nil,
        antiCensorship: AntiCensorshipConfig = .current
    ) {
        self.baseURL = baseURL
        self.session = session
        self.sessionToken = sessionToken
        self.antiCensorship = antiCensorship
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

    // MARK: - Low-level: 1:1 HTTP endpoints

    /// `POST /pow/challenge`.
    public func powChallenge() async throws -> PowChallengeResponse {
        try await postNoBody(endpoint: "/pow/challenge")
    }

    /// `POST /discovery/oprf`.
    public func oprfEvaluate(_ request: OprfRequest) async throws -> OprfResponse {
        try await post(endpoint: "/discovery/oprf", body: request)
    }

    /// `POST /discovery/register`.
    public func register(_ request: RegisterRequest) async throws -> RegisterResponse {
        try await post(endpoint: "/discovery/register", body: request)
    }

    /// `POST /discovery/lookup`. Returns the typed response on 200,
    /// `nil` on 404 (the typed not-found path), throws on any
    /// other non-2xx.
    public func lookupRaw(_ request: LookupRequest) async throws -> LookupResponse? {
        try await postNullableOn404(endpoint: "/discovery/lookup", body: request)
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
    /// Flow: PoW₁ → OPRF blind → `POST /discovery/oprf` → finalize
    /// → PoW₂ → `POST /discovery/register`.
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
        let blinder = try OprfBlinder.blind(username: username)
        // Read the normalised form **before** finalize — after it
        // the UniFFI sentinel kicks in (1.15e1's
        // `DiscoveryAlreadyFinalized`).
        let normalisedUsername = try blinder.normalizedUsername()

        let challenge1 = try await powChallenge()
        let pow1 = try await solvePow(
            seedB64: challenge1.seed,
            effortBits: challenge1.effortBits
        )
        let oprfResp = try await oprfEvaluate(
            OprfRequest(pow: pow1, blinded: blinder.blindedB64())
        )

        let oprfHashBytes = try blinder.finalize(evaluatedB64: oprfResp.evaluated)
        let oprfHashB64 = base64Encode(oprfHashBytes)

        let challenge2 = try await powChallenge()
        let pow2 = try await solvePow(
            seedB64: challenge2.seed,
            effortBits: challenge2.effortBits
        )

        let resp = try await register(
            RegisterRequest(
                pow: pow2,
                accountId: base64Encode(accountId),
                oprfHash: oprfHashB64
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
    /// the second endpoint: PoW₁ → OPRF blind → `POST /discovery/oprf`
    /// → finalize → PoW₂ → `POST /discovery/lookup`.
    public func lookup(username: String) async throws -> LookupResult {
        let blinder = try OprfBlinder.blind(username: username)
        let normalisedUsername = try blinder.normalizedUsername()

        let challenge1 = try await powChallenge()
        let pow1 = try await solvePow(
            seedB64: challenge1.seed,
            effortBits: challenge1.effortBits
        )
        let oprfResp = try await oprfEvaluate(
            OprfRequest(pow: pow1, blinded: blinder.blindedB64())
        )

        let oprfHashBytes = try blinder.finalize(evaluatedB64: oprfResp.evaluated)
        let oprfHashB64 = base64Encode(oprfHashBytes)

        let challenge2 = try await powChallenge()
        let pow2 = try await solvePow(
            seedB64: challenge2.seed,
            effortBits: challenge2.effortBits
        )

        guard let resp = try await lookupRaw(
            LookupRequest(pow: pow2, oprfHash: oprfHashB64)
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

    /// Upload (or replace) the user's MLS KeyPackage. Drives one
    /// PoW solve + `POST /discovery/keypackage`.
    ///
    /// Unlike the identity bundle, KeyPackages are **not**
    /// deterministic — `Identity.createKeyPackage()` generates
    /// fresh randomness on every call (RFC 9420 forward-secrecy
    /// property). Calling this on every login refreshes the
    /// `init_key` on the server, so a fresh sender starting a new
    /// 1×1 group gets bytes the recipient hasn't already used.
    ///
    /// The Phase-1 directory only stores ONE KeyPackage per account
    /// at a time (consume-on-use is post-1.14 work), so a sender
    /// racing two sessions can in theory seed two groups with the
    /// same `init_key`. Documented MVP trade-off matching the
    /// Android + web sides.
    public func uploadKeyPackage(
        accountId: Data,
        keyPackageBytes: Data
    ) async throws {
        guard accountId.count == Self.accountIdLength else {
            throw DiscoveryClientError.argument(
                "accountId must be \(Self.accountIdLength) bytes"
            )
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

        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )

        let resp = try await keypackageUpload(
            KeyPackageUploadRequest(
                pow: pow,
                accountId: base64Encode(accountId),
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

    /// Resolve `accountId → KeyPackage bytes` (TLS-encoded MLS).
    /// Returns `nil` when the recipient hasn't published one yet.
    ///
    /// Convenience wrapper around
    /// ``keypackageFetch(accountId:)`` that decodes the base64 wire
    /// field to a `Data` so the caller can hand it straight to
    /// `Group.addMemberFromWire(...)`.
    public func fetchKeyPackage(accountId: Data) async throws -> Data? {
        guard let raw = try await keypackageFetch(accountId: accountId) else {
            return nil
        }
        return base64Decode(raw.keypackage)
    }

    /// Upload (or replace) the identity bundle for `accountId`.
    /// Drives one PoW solve + `POST /discovery/identity`.
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
    public func uploadIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        supersessionProofB64: String? = nil,
        leafAuth: Data? = nil
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

        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )

        let resp = try await identityUpload(
            IdentityUploadRequest(
                pow: pow,
                accountId: base64Encode(accountId),
                signaturePubkey: base64Encode(signaturePubkey),
                x25519Pubkey: base64Encode(x25519Pubkey),
                supersessionProof: supersessionProofB64,
                leafAuth: leafAuth.map { base64Encode($0) }
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
    /// `InviteCapability` blob, get the content-addressed
    /// `blob_id` back. Idempotent — same bytes yield the same id,
    /// server upserts.
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

    /// `POST /discovery/invite/{blob_id_b64url}/consume`. The
    /// high-level ``consumeInvite(blobId:capabilityBytes:joinerAccountId:)``
    /// wrapper drives the PoW solve + the local
    /// `computeJoinerInviteTag` call + folds the 200 / 404 / 410
    /// status trifecta into a discriminated ``ConsumeInviteResult``.
    public func inviteConsume(
        blobIdSlug: String,
        request: InviteConsumeRequest
    ) async throws -> InviteConsumeResponse {
        try await post(
            endpoint: "/discovery/invite/\(blobIdSlug)/consume",
            body: request
        )
    }

    // MARK: - High-level invite flows

    /// Upload a freshly-composed `InviteCapability` blob and
    /// receive the server-assigned content-addressed `blobId`
    /// (raw 32-byte SHA-256). Drives one PoW solve + `POST
    /// /discovery/invite`. Idempotent.
    public func uploadInvite(capabilityBytes: Data) async throws -> Data {
        guard !capabilityBytes.isEmpty else {
            throw DiscoveryClientError.argument(
                "capabilityBytes must be non-empty"
            )
        }
        guard capabilityBytes.count <= Self.maxCapabilityBytes else {
            throw DiscoveryClientError.argument(
                "capabilityBytes exceeds the \(Self.maxCapabilityBytes)-byte ceiling " +
                "(got \(capabilityBytes.count) bytes)"
            )
        }

        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )

        let resp = try await inviteUpload(
            InviteUploadRequest(
                pow: pow,
                capability: base64Encode(capabilityBytes)
            )
        )
        guard let blobId = base64UrlDecodeNoPad(resp.blobId),
              blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.http(
                status: 200,
                endpoint: "/discovery/invite",
                body: "server returned malformed blob_id"
            )
        }
        return blobId
    }

    /// Fetch a previously-uploaded `InviteCapability` by its
    /// content-addressed `blobId`. Returns `nil` on 404, raw
    /// capability bytes on 200 (the caller drives
    /// `InviteCapability.decodeAndVerify` to re-verify locally so
    /// a malicious server cannot serve substituted bytes).
    public func fetchInvite(blobId: Data) async throws -> Data? {
        guard let raw = try await inviteFetch(blobId: blobId) else {
            return nil
        }
        return base64Decode(raw.capability)
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

        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )

        let endpoint = "/discovery/invite/\(slug)/consume"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        attachAuthorizationHeader(to: &request)
        let body = InviteConsumeRequest(
            pow: pow,
            joinerTag: base64Encode(joinerTag)
        )
        do {
            request.httpBody = try encoder.encode(body)
        } catch {
            throw DiscoveryClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }

        // Bypass `execute(...)` so we can branch on the 404/410
        // status codes without taking the error path.
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
        guard let httpResponse = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        switch httpResponse.statusCode {
        case 404:
            return .notFound
        case 410:
            return .exhausted
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
    /// the PoW solve.
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

    /// `POST /discovery/call-invite/{blob_id_b64url}/consume`.
    /// The high-level ``consumeCallInvite(blobId:capabilityBytes:joinerAccountId:)``
    /// wrapper drives the PoW solve + the local
    /// `computeJoinerInviteTag` call + folds the 200 / 404 /
    /// 410 trifecta into a discriminated ``ConsumeInviteResult``.
    public func callInviteConsume(
        blobIdSlug: String,
        request: InviteConsumeRequest
    ) async throws -> InviteConsumeResponse {
        try await post(
            endpoint: "/discovery/call-invite/\(blobIdSlug)/consume",
            body: request
        )
    }

    /// Upload a freshly-composed `CallInviteCapability` blob and
    /// receive the server-assigned content-addressed `blobId`
    /// (raw 32-byte SHA-256). Drives one PoW solve + `POST
    /// /discovery/call-invite`. Idempotent — same bytes yield
    /// the same id, server upserts.
    ///
    /// Sibling of ``uploadInvite(capabilityBytes:)``; the
    /// client-side ceiling is `maxCallCapabilityBytes` (8 KiB,
    /// 4× contact-invite) because the call-invite capability
    /// embeds a one-time MLS KeyPackage.
    public func uploadCallInvite(capabilityBytes: Data) async throws -> Data {
        guard !capabilityBytes.isEmpty else {
            throw DiscoveryClientError.argument(
                "capabilityBytes must be non-empty"
            )
        }
        guard capabilityBytes.count <= Self.maxCallCapabilityBytes else {
            throw DiscoveryClientError.argument(
                "capabilityBytes exceeds the \(Self.maxCallCapabilityBytes)-byte ceiling " +
                "(got \(capabilityBytes.count) bytes)"
            )
        }

        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )

        let resp = try await callInviteUpload(
            InviteUploadRequest(
                pow: pow,
                capability: base64Encode(capabilityBytes)
            )
        )
        guard let blobId = base64UrlDecodeNoPad(resp.blobId),
              blobId.count == Self.blobIdLength else {
            throw DiscoveryClientError.http(
                status: 200,
                endpoint: "/discovery/call-invite",
                body: "server returned malformed blob_id"
            )
        }
        return blobId
    }

    /// Fetch a previously-uploaded `CallInviteCapability` by
    /// its content-addressed `blobId`. Returns `nil` on 404,
    /// raw capability bytes on 200 (the caller drives
    /// `CallInviteCapability.decodeAndVerify` to re-verify
    /// locally so a malicious server cannot serve substituted
    /// bytes).
    public func fetchCallInvite(blobId: Data) async throws -> Data? {
        guard let raw = try await callInviteFetch(blobId: blobId) else {
            return nil
        }
        return base64Decode(raw.capability)
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

        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )

        let endpoint = "/discovery/call-invite/\(slug)/consume"
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        attachAuthorizationHeader(to: &request)
        let body = InviteConsumeRequest(
            pow: pow,
            joinerTag: base64Encode(joinerTag)
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
                endpoint: endpoint,
                underlying: error
            )
        }
        guard let httpResponse = raw.1 as? HTTPURLResponse else {
            throw DiscoveryClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        switch httpResponse.statusCode {
        case 404:
            return .notFound
        case 410:
            return .exhausted
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

    /// Public-facing 404→nil GET fetch, the only difference from
    /// the private ``getNullableOn404(endpoint:)`` is callability.
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
                request, session: session, config: antiCensorship)
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

    /// One-shot POST + decode. Used by every endpoint method that
    /// expects a non-nullable typed response.
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

    /// `POST` with no body — used by `/pow/challenge`.
    fileprivate func postNoBody<Res: Decodable>(
        endpoint: String
    ) async throws -> Res {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        attachAuthorizationHeader(to: &request)
        return try await execute(endpoint: endpoint, request: request)
    }

    /// Attach the `Authorization: Bearer <token>` header when a
    /// session token is set. Unauthenticated POSTs (`/pow/challenge`,
    /// `/discovery/oprf`, `/discovery/lookup`,
    /// `invite/{}/consume`) silently ignore the header server-side
    /// — adding it unconditionally simplifies the call sites at
    /// the cost of a few bytes per request.
    fileprivate func attachAuthorizationHeader(to request: inout URLRequest) {
        if let token = sessionToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
    }

    /// Variant of ``post(endpoint:body:)`` that folds a documented
    /// 404 into a `nil` return. Used by `/discovery/lookup`.
    private func postNullableOn404<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req
    ) async throws -> Res? {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        do {
            request.httpBody = try encoder.encode(body)
        } catch {
            throw DiscoveryClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }
        return try await executeNullableOn404(endpoint: endpoint, request: request)
    }

    /// `GET` variant of ``postNullableOn404(endpoint:body:)``. Same
    /// 404→nil fold, no request body — used by the
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

    // MARK: - Constants

    /// Raw 16-byte UUIDv7.
    fileprivate static let accountIdLength: Int = 16

    /// Ed25519 + X25519 both use 32-byte public keys.
    fileprivate static let pubkeyLength: Int = 32

    /// Mirror of `services/discovery`'s `MAX_KEYPACKAGE_BYTES`.
    /// Surfacing the ceiling here saves a round-trip on a
    /// misbehaving caller.
    fileprivate static let maxKeyPackageBytes: Int = 2048

    /// Mirror of `services/discovery`'s `MAX_CAPABILITY_BYTES`.
    fileprivate static let maxCapabilityBytes: Int = 2048

    /// Mirror of `services/discovery`'s `MAX_CALL_CAPABILITY_BYTES`
    /// — the call-invite ledger uses an 8192-byte ceiling
    /// (vs. contact-invite's 2048) because `CallInviteCapability`
    /// embeds a one-time MLS KeyPackage (~4 KiB upper bound per
    /// `MAX_KEY_PACKAGE_LEN`).
    fileprivate static let maxCallCapabilityBytes: Int = 8192

    /// SHA-256 → 32-byte content-addressed invite id.
    fileprivate static let blobIdLength: Int = 32

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
    /// URL-safe base64 (no padding) of the 32-byte blob_id —
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
        }
    }
}
