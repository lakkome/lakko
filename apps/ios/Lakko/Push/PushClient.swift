// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// URLSession-based client for `lakko-svc-push-gw`.
///
/// Mirror of:
///
///   * `apps/web/src/push/client.ts` (1.20b)
///   * `apps/android/.../push/PushClient.kt` (1.20d)
///
/// The user-device side stores sealed group subscriptions; it never asks for
/// a wake. ADR-0054 decision 1 moved the group wake into the signed DS publish
/// (`x-lakko-wake`, see ``DsWake``): the DS forwards it to push-gw over the
/// internal network after appending the row. Since ADR-0078 section 3 push-gw
/// holds no device pseudonym and no token in the clear. Endpoints used here:
///
///   * `POST /push/subscribe` — store (or replace) one sealed subscription
///     `{sub_id, push_gid, wake_pk, sealed}`; `sealed` is this device's
///     tokens sealed under the group's unlock key
///     (`LakkoCore.sealPushSubscription`), `sub_id` 16 random bytes this
///     device keeps per subscription. Re-sending an id replaces its row,
///     which is how a new token reaches push-gw.
///   * `POST /push/unsubscribe` — drop one row by its `sub_id`.
///   * `GET /push/wake-key` — the X25519 key every wake is sealed to.
///
/// ## Auth posture (ADR-0054 S3)
///
/// Every route here spends one anonymous `push-register` token
/// (``AnonTokenPool``) in the `x-lakko-anon-token` header and carries
/// nothing else that names the account: no `Authorization`, no cookie, and
/// its own connection pool (``AnonymousHTTP``). There is no proof of work
/// and no `/pow/challenge` any more. A used-up allowance
/// (``AnonTokenUnavailable/quotaExhausted(retryAfterSecs:)``) is silent to
/// the user: registration is best-effort and runs again at the next launch.
///
/// ## Default base URL
///
/// `<LakkoApiBaseUrl>/push-gw` (``ApiEndpoints``); with no API
/// base URL, `http://localhost:8085` — iOS simulator loopback +
/// port `8085` matching `lakko-svc-push-gw`'s dev bind.
public final class PushClient {

    public static var defaultBaseURL: String { ApiEndpoints.baseURL(for: .pushGw) }

    /// Length of a subscription id (mirror of `PUSH_SUBSCRIPTION_ID_LEN`).
    public static let subscriptionIdLength: Int = 16

    /// Length of a `push_gid`, a wake public key and push-gw's wake key.
    public static let keyLength: Int = 32

    public let baseURL: String
    /// The anonymous session every push-gw route travels on.
    private let session: URLSession
    private let tokens: AnonTokenSource
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(
        baseURL: String = PushClient.defaultBaseURL,
        session: URLSession = .shared,
        tokens: AnonTokenSource = AnonTokenPool.shared
    ) {
        self.baseURL = baseURL
        self.session = AnonymousHTTP.session(pairedWith: session)
        self.tokens = tokens
        self.encoder = JSONEncoder()
        self.decoder = JSONDecoder()
    }

    // MARK: - Subscriptions (ADR-0078)

    /// Store (or replace) one sealed subscription.
    public func subscribe(
        subId: Data,
        pushGid: Data,
        wakePk: Data,
        sealed: Data
    ) async throws {
        guard subId.count == Self.subscriptionIdLength else {
            throw PushClientError.argument("subId must be \(Self.subscriptionIdLength) bytes")
        }
        guard pushGid.count == Self.keyLength, wakePk.count == Self.keyLength else {
            throw PushClientError.argument("pushGid and wakePk must be \(Self.keyLength) bytes")
        }
        guard !sealed.isEmpty else {
            throw PushClientError.argument("sealed must be non-empty")
        }
        let req = SubscribeRequest(
            sub_id: Self.base64UrlNoPad(subId),
            push_gid: Self.base64UrlNoPad(pushGid),
            wake_pk: Self.base64UrlNoPad(wakePk),
            sealed: Self.base64UrlNoPad(sealed)
        )
        let resp: OkResponse = try await post(endpoint: "/push/subscribe", body: req)
        guard resp.ok else {
            throw PushClientError.http(
                status: 200, endpoint: "/push/subscribe", body: "server reported ok=false")
        }
    }

    /// Drop one subscription by its id. Idempotent: `false` when it was
    /// already gone.
    ///
    /// `prepaidToken`: a `push-register` token taken before the session ended
    /// (logout clears the pool, so the caller takes one first).
    @discardableResult
    public func unsubscribe(subId: Data, prepaidToken: AnonToken? = nil) async throws -> Bool {
        guard subId.count == Self.subscriptionIdLength else {
            throw PushClientError.argument("subId must be \(Self.subscriptionIdLength) bytes")
        }
        let resp: UnsubscribeResponse = try await post(
            endpoint: "/push/unsubscribe",
            body: UnsubscribeRequest(sub_id: Self.base64UrlNoPad(subId)),
            tokens: prepaidToken.map { PrepaidAnonToken($0, then: tokens) } ?? tokens
        )
        return resp.removed
    }

    /// push-gw's X25519 wake key. No token: it is public.
    public func wakeKey() async throws -> Data {
        let url = try buildURL(endpoint: "/push/wake-key")
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(
                URLRequest(url: url), session: session, config: .current)
        } catch {
            throw PushClientError.network(endpoint: "/push/wake-key", underlying: error)
        }
        guard let http = raw.1 as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw PushClientError.http(
                status: (raw.1 as? HTTPURLResponse)?.statusCode ?? 0,
                endpoint: "/push/wake-key", body: "no wake key")
        }
        guard let body = try? decoder.decode(WakeKeyResponse.self, from: raw.0),
              let key = base64UrlDecodeNoPad(body.key), key.count == Self.keyLength else {
            throw PushClientError.http(status: http.statusCode, endpoint: "/push/wake-key", body: "malformed key")
        }
        return key
    }

    // MARK: - Internal HTTP

    /// One token route: spend a `push-register` token (retried once with a
    /// second token on a token refusal), no `Authorization`, the anonymous
    /// session.
    private func post<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req,
        tokens: AnonTokenSource? = nil
    ) async throws -> Res {
        let url = try buildURL(endpoint: endpoint)
        let encoded: Data
        do {
            encoded = try encoder.encode(body)
        } catch {
            throw PushClientError.argument(
                "JSON encode failed for \(endpoint): \(error)"
            )
        }
        let (data, httpResponse) = try await AnonTokenRoute.send(
            context: .pushRegister,
            tokens: tokens ?? self.tokens
        ) { tokenHeader in
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue(tokenHeader, forHTTPHeaderField: AnonTokenRoute.headerName)
            request.httpBody = encoded
            let raw: (Data, URLResponse)
            do {
                raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
            } catch {
                throw PushClientError.network(
                    endpoint: endpoint,
                    underlying: error
                )
            }
            guard let httpResponse = raw.1 as? HTTPURLResponse else {
                throw PushClientError.http(
                    status: 0,
                    endpoint: endpoint,
                    body: "non-HTTP response"
                )
            }
            return (raw.0, httpResponse)
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            let body = Self.truncate(
                String(data: data, encoding: .utf8) ?? "<binary>"
            )
            throw PushClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: body
            )
        }
        do {
            return try decoder.decode(Res.self, from: data)
        } catch {
            let body = Self.truncate(
                String(data: data, encoding: .utf8) ?? "<binary>"
            )
            throw PushClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: "decode failed: \(error) [body=\(body)]"
            )
        }
    }

    private func buildURL(endpoint: String) throws -> URL {
        guard let url = URL(string: baseURL + endpoint) else {
            throw PushClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        return url
    }

    private static func truncate(_ s: String, max: Int = 512) -> String {
        if s.count > max {
            return String(s.prefix(max)) + "…"
        }
        return s
    }

    /// RFC 4648 section 5 base64url, padding stripped (the push-gw wire form).
    static func base64UrlNoPad(_ bytes: Data) -> String {
        var s = bytes.base64EncodedString()
        s = s.replacingOccurrences(of: "+", with: "-")
        s = s.replacingOccurrences(of: "/", with: "_")
        while s.hasSuffix("=") { s.removeLast() }
        return s
    }
}

// MARK: - Wire shapes

// swiftlint:disable identifier_name
// Snake-case stored properties so the default `JSONEncoder`
// serialises the field names exactly as the server expects
// (no `convertToSnakeCase` round-trip ambiguity).
private struct SubscribeRequest: Encodable {
    let sub_id: String
    let push_gid: String
    let wake_pk: String
    let sealed: String
}

private struct OkResponse: Decodable {
    let ok: Bool
}

private struct UnsubscribeRequest: Encodable {
    let sub_id: String
}

private struct UnsubscribeResponse: Decodable {
    let ok: Bool
    let removed: Bool
}

private struct WakeKeyResponse: Decodable {
    let key: String
}

// swiftlint:enable identifier_name

// MARK: - Errors

public enum PushClientError: Error {
    /// Local input validation (wrong length, empty, malformed
    /// base URL). Caught before any network round-trip.
    case argument(String)
    /// Server returned a non-2xx status.
    case http(status: Int, endpoint: String, body: String)
    /// DNS / TLS / transport failure.
    case network(endpoint: String, underlying: Error)
}

extension PushClientError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let m): return m
        case .http(let s, let e, let b): return "push \(e) → \(s): \(b)"
        case .network(let e, let u): return "push \(e) network error: \(u)"
        }
    }
}
