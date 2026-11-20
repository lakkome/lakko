// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Draws a batch of anonymous tokens from `services/auth` (ADR-0054 S3).
public protocol AnonTokenIssuing: Sendable {
    /// Issue `count` tokens of `context` at `epoch` with `sessionToken`.
    ///
    /// Throws ``AnonTokenUnavailable``: `.epochNotIssuable` (the pool rebuilds
    /// once), `.quotaExhausted`, `.disabled`, `.verificationFailed` (the batch
    /// is discarded), `.issuanceFailed`.
    func issue(
        context: AnonTokenContext,
        epoch: UInt64,
        count: UInt32,
        sessionToken: String
    ) async throws -> [AnonToken]
}

/// The production issuer.
///
/// `POST /auth/tokens` (bearer, the authenticated session) evaluates the
/// blinded batch; the public key the proof is checked against comes from the
/// anonymous `GET /auth/tokens/key/{context}/{epoch}` - NEVER from the
/// `public_key` of the issuance response, since an issuer that served a
/// per-account key there could link issuance to spending. The key route is
/// fetched on the anonymous session and cached for 300 s (its
/// `Cache-Control`).
public final class AuthAnonTokenIssuer: AnonTokenIssuing, @unchecked Sendable {

    /// Unblind + verify. A seam so a test can observe which key the check ran
    /// against; production is `AnonTokenRequest.finalize`.
    public typealias Finalize = @Sendable (
        _ request: AnonTokenRequest,
        _ evaluations: [String],
        _ proof: String,
        _ publicKey: String
    ) throws -> [AnonToken]

    /// How long a key from the key route is reused.
    public static let keyCacheSeconds: TimeInterval = 300

    private let baseURL: String
    private let session: URLSession
    private let anonymousSession: URLSession
    private let finalize: Finalize
    private let now: @Sendable () -> Date

    private let lock = NSLock()
    private var keyCache: [String: (key: String, fetchedAt: Date)] = [:]

    public init(
        baseURL: String = AuthClient.defaultBaseURL,
        session: URLSession = .shared,
        anonymousSession: URLSession? = nil,
        finalize: @escaping Finalize = { request, evaluations, proof, key in
            try request.finalize(evaluationsB64url: evaluations, proofB64url: proof, publicKeyB64url: key)
        },
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.baseURL = baseURL
        self.session = session
        self.anonymousSession = anonymousSession ?? AnonymousHTTP.session(pairedWith: session)
        self.finalize = finalize
        self.now = now
    }

    public func issue(
        context: AnonTokenContext,
        epoch: UInt64,
        count: UInt32,
        sessionToken: String
    ) async throws -> [AnonToken] {
        do {
            return try await issueBatch(
                context: context, epoch: epoch, count: count, sessionToken: sessionToken)
        } catch PartialQuota.remaining(let remaining) {
            // The allowance has fewer tokens left than the batch asked for:
            // draw what is left, once.
            return try await issueBatch(
                context: context, epoch: epoch, count: remaining, sessionToken: sessionToken)
        }
    }

    private enum PartialQuota: Error {
        case remaining(UInt32)
    }

    private func issueBatch(
        context: AnonTokenContext,
        epoch: UInt64,
        count: UInt32,
        sessionToken: String
    ) async throws -> [AnonToken] {
        let request: AnonTokenRequest
        do {
            request = try AnonTokenRequest(context: context.rawValue, epoch: epoch, count: count)
        } catch {
            throw AnonTokenUnavailable.issuanceFailed("request: \(error)")
        }

        guard let url = URL(string: baseURL + "/auth/tokens") else {
            throw AnonTokenUnavailable.issuanceFailed("bad base URL")
        }
        var http = URLRequest(url: url)
        http.httpMethod = "POST"
        http.setValue("application/json", forHTTPHeaderField: "Content-Type")
        http.setValue("Bearer \(sessionToken)", forHTTPHeaderField: "Authorization")
        http.httpBody = Data(request.issueBodyJson().utf8)

        let (data, response) = try await perform(http, on: session)
        switch response.statusCode {
        case 200..<300:
            break
        case 400 where AnonTokenRoute.errorCode(in: data) == "epoch-not-issuable":
            guard let current = Self.number(in: data, key: "current_epoch") else {
                throw AnonTokenUnavailable.issuanceFailed("epoch-not-issuable without current_epoch")
            }
            throw AnonTokenUnavailable.epochNotIssuable(currentEpoch: current)
        case 429:
            if let left = Self.number(in: data, key: "remaining"), left > 0, left < UInt64(count) {
                throw PartialQuota.remaining(UInt32(left))
            }
            let retry = Self.number(in: data, key: "retry_after_secs").map { Int($0) }
                ?? response.value(forHTTPHeaderField: "Retry-After").flatMap { Int($0) }
            throw AnonTokenUnavailable.quotaExhausted(retryAfterSecs: retry)
        case 503 where AnonTokenRoute.errorCode(in: data) == "anon-tokens-disabled":
            throw AnonTokenUnavailable.disabled
        case 403 where AnonTokenRoute.errorCode(in: data) == accountSuspendedCode:
            // ADR-0098 step 3 — a suspended account draws `blob-read` only:
            // "not now" for this context, until the suspension ends.
            throw AnonTokenUnavailable.refused(code: accountSuspendedCode)
        default:
            throw AnonTokenUnavailable.issuanceFailed(
                "POST /auth/tokens -> \(response.statusCode) \(AnonTokenRoute.errorCode(in: data) ?? "")")
        }

        let issued: IssueResponse
        do {
            issued = try JSONDecoder().decode(IssueResponse.self, from: data)
        } catch {
            throw AnonTokenUnavailable.issuanceFailed("malformed issuance response")
        }
        // The issuer answers for the epoch that was asked for; anything else is
        // a response for some other request.
        guard issued.epoch == request.epoch(), issued.context == context.rawValue else {
            throw AnonTokenUnavailable.issuanceFailed("issuance for another context or epoch")
        }

        let key = try await publicKey(context: context, epoch: request.epoch())
        do {
            return try finalize(request, issued.evaluations, issued.proof, key)
        } catch {
            // `proof-verification` (or any other finalize failure): the batch
            // is discarded, no token of it is kept.
            NSLog("[anon-token] %@ batch discarded: %@", context.rawValue, String(describing: error))
            throw AnonTokenUnavailable.verificationFailed(String(describing: error))
        }
    }

    /// The key from the anonymous key route, cached for ``keyCacheSeconds``.
    func publicKey(context: AnonTokenContext, epoch: UInt64) async throws -> String {
        try await publicKey(contextName: context.rawValue, epoch: epoch)
    }

    /// The key route for a context named on the wire: also the `beta-place`
    /// context of a supporter code (ADR-0097), which is no pool context and
    /// so has no ``AnonTokenContext`` case.
    func publicKey(contextName: String, epoch: UInt64) async throws -> String {
        let cacheKey = "\(contextName)/\(epoch)"
        if let cached = cachedKey(cacheKey) { return cached }

        guard let url = URL(string: baseURL + "/auth/tokens/key/\(contextName)/\(epoch)") else {
            throw AnonTokenUnavailable.issuanceFailed("bad base URL")
        }
        var http = URLRequest(url: url)
        http.httpMethod = "GET"
        let (data, response) = try await perform(http, on: anonymousSession)
        guard (200..<300).contains(response.statusCode) else {
            if response.statusCode == 503, AnonTokenRoute.errorCode(in: data) == "anon-tokens-disabled" {
                throw AnonTokenUnavailable.disabled
            }
            throw AnonTokenUnavailable.issuanceFailed("GET key -> \(response.statusCode)")
        }
        guard let parsed = try? JSONDecoder().decode(KeyResponse.self, from: data),
              parsed.context == contextName, parsed.epoch == epoch
        else {
            throw AnonTokenUnavailable.issuanceFailed("malformed key response")
        }
        storeKey(parsed.publicKey, for: cacheKey)
        return parsed.publicKey
    }

    private func cachedKey(_ cacheKey: String) -> String? {
        lock.withLock {
            guard let cached = keyCache[cacheKey],
                  now().timeIntervalSince(cached.fetchedAt) < Self.keyCacheSeconds
            else { return nil }
            return cached.key
        }
    }

    private func storeKey(_ key: String, for cacheKey: String) {
        lock.withLock { keyCache[cacheKey] = (key, now()) }
    }

    private func perform(_ request: URLRequest, on session: URLSession) async throws -> (Data, HTTPURLResponse) {
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        } catch {
            throw AnonTokenUnavailable.issuanceFailed("network: \(error)")
        }
        guard let http = raw.1 as? HTTPURLResponse else {
            throw AnonTokenUnavailable.issuanceFailed("non-HTTP response")
        }
        return (raw.0, http)
    }

    private static func number(in data: Data, key: String) -> UInt64? {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let value = object[key] as? NSNumber
        else { return nil }
        return value.uint64Value
    }
}

/// `POST /auth/tokens` 200 body.
private struct IssueResponse: Decodable {
    let context: String
    let epoch: UInt64
    let evaluations: [String]
    let proof: String
    /// Present on the wire and deliberately unused: the proof is checked
    /// against the key route's key only.
    let publicKey: String?
    let remaining: UInt32?
    let epochEndsAt: UInt64?

    enum CodingKeys: String, CodingKey {
        case context, epoch, evaluations, proof, remaining
        case publicKey = "public_key"
        case epochEndsAt = "epoch_ends_at"
    }
}

/// `GET /auth/tokens/key/{context}/{epoch}` body.
private struct KeyResponse: Decodable {
    let context: String
    let epoch: UInt64
    let publicKey: String

    enum CodingKeys: String, CodingKey {
        case context, epoch
        case publicKey = "public_key"
    }
}
