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
/// The user-device side registers tokens and group subscriptions; it never
/// asks for a wake. ADR-0054 decision 1 moved the group wake into the signed
/// DS publish (`x-lakko-wake`, see ``DsWake``): the DS forwards it to push-gw
/// over the internal network after appending the row, so no client calls
/// `/push/notify-group` and `/push/notify` is gone. Endpoints used here:
///
///   * `POST /push/register` — bind a
///     `(device_pseudonym, provider)` pair to a platform-specific
///     token blob. Idempotent: re-register overwrites the prior
///     token (clients rotate after APNs rebuild / FCM token
///     refresh / Web Push subscription change).
///   * `POST /push/unregister` — drop a token. Called on
///     logout + forgetDevice.
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

    /// Raw device pseudonym length — 32 bytes, 64 lowercase hex
    /// chars on the wire (mirror of `services/push-gw`'s
    /// `DEVICE_LEN`).
    public static let devicePseudonymLength: Int = 32

    /// Provider names as the server serialises them
    /// (snake_case via `#[serde(rename_all = "snake_case")]`
    /// on `ProviderKind` in `services/push-gw/src/provider.rs`).
    public enum Provider: String, Codable {
        case apns
        case fcm
        case webPush = "web_push"
        case unifiedPush = "unified_push"
        /// PushKit VoIP tokens (`docs/ux/calls.md` section 2.1).
        ///
        /// A SECOND, separate token for the same device: a VoIP push must be
        /// sent to the PushKit token with `apns-push-type: voip` and a VoIP
        /// services certificate, and APNs refuses it for the alert token. So it
        /// is its own provider row rather than an overwrite of ``apns``.
        ///
        /// **The server does not accept this value yet.** `ProviderKind` in
        /// `services/push-gw/src/provider.rs` has four variants and no
        /// `apns_voip`, so `POST /push/register` rejects the body outright.
        /// The client is deliberately complete anyway — the registration is
        /// best-effort and its failure is logged, never fatal — so that the
        /// server work is a server change with no client change. What it needs:
        /// a fifth `ProviderKind` (db value 5, never renumbering the four),
        /// an `ApnsVoipProvider` (or a flag on the existing one) that sends
        /// `apns-push-type: voip` to `<bundle id>.voip` as the topic with a
        /// VoIP certificate, and a call wake fanning out to the VoIP row
        /// *instead of* the alert row when one exists for the device.
        case apnsVoip = "apns_voip"
    }

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

    // MARK: - Register

    /// Bind `(devicePseudonym, provider) → tokenBytes`.
    /// Idempotent: re-register with a fresh `tokenBytes`
    /// overwrites the prior bytes.
    ///
    /// - Parameters:
    ///   - devicePseudonym: 32-byte stable per-device handle.
    ///     Derive once per device + persist locally; see
    ///     ``DevicePushId``.
    ///   - provider: Which delivery path this token is for —
    ///     ``Provider/apns`` for iOS / iPadOS / macOS.
    ///   - tokenBytes: Provider-opaque token blob. For APNs
    ///     this is the raw 32-byte device token the OS handed
    ///     us in
    ///     `application(_:didRegisterForRemoteNotificationsWithDeviceToken:)`.
    public func register(
        devicePseudonym: Data,
        provider: Provider,
        tokenBytes: Data
    ) async throws {
        guard devicePseudonym.count == Self.devicePseudonymLength else {
            throw PushClientError.argument(
                "devicePseudonym must be \(Self.devicePseudonymLength) bytes"
            )
        }
        guard !tokenBytes.isEmpty else {
            throw PushClientError.argument("tokenBytes must be non-empty")
        }
        let req = PushRegisterRequest(
            device_pseudonym: Self.pseudonymHex(devicePseudonym),
            provider: provider,
            token: tokenBytes.base64EncodedString()
        )
        let resp: PushRegisterResponse = try await post(
            endpoint: "/push/register",
            body: req
        )
        guard resp.ok else {
            throw PushClientError.http(
                status: 200,
                endpoint: "/push/register",
                body: "server reported ok=false"
            )
        }
    }

    // MARK: - Group fan-out (Phase 1.20e)

    /// Subscribe this device to push wakes for `groupPseudonym`.
    /// Idempotent: a re-subscribe with the same `(device, group)`
    /// pair returns `inserted = false`.
    public func subscribeGroup(
        devicePseudonym: Data,
        groupPseudonym: Data
    ) async throws -> Bool {
        guard devicePseudonym.count == Self.devicePseudonymLength else {
            throw PushClientError.argument(
                "devicePseudonym must be \(Self.devicePseudonymLength) bytes"
            )
        }
        guard groupPseudonym.count == Self.devicePseudonymLength else {
            throw PushClientError.argument(
                "groupPseudonym must be \(Self.devicePseudonymLength) bytes"
            )
        }
        let req = GroupSubscribeRequest(
            device_pseudonym: Self.pseudonymHex(devicePseudonym),
            group_pseudonym: Self.pseudonymHex(groupPseudonym)
        )
        let resp: GroupSubscribeResponse = try await post(
            endpoint: "/push/group/subscribe",
            body: req
        )
        return resp.inserted
    }

    /// Drop a `(device, group)` subscription. Idempotent.
    public func unsubscribeGroup(
        devicePseudonym: Data,
        groupPseudonym: Data
    ) async throws -> Bool {
        guard devicePseudonym.count == Self.devicePseudonymLength,
              groupPseudonym.count == Self.devicePseudonymLength
        else {
            throw PushClientError.argument("pseudonyms must be 32 bytes")
        }
        let req = GroupUnsubscribeRequest(
            device_pseudonym: Self.pseudonymHex(devicePseudonym),
            group_pseudonym: Self.pseudonymHex(groupPseudonym)
        )
        let resp: GroupUnsubscribeResponse = try await post(
            endpoint: "/push/group/unsubscribe",
            body: req
        )
        return resp.removed
    }

    // MARK: - Unregister

    /// Drop a previously-registered token. Idempotent: returns
    /// `removed: false` if the row was already gone.
    ///
    /// `prepaidToken`: a `push-register` token taken before the session ended
    /// (logout clears the pool, so the caller takes one first).
    public func unregister(
        devicePseudonym: Data,
        provider: Provider,
        prepaidToken: AnonToken? = nil
    ) async throws -> Bool {
        guard devicePseudonym.count == Self.devicePseudonymLength else {
            throw PushClientError.argument(
                "devicePseudonym must be \(Self.devicePseudonymLength) bytes"
            )
        }
        let req = UnregisterRequest(
            device_pseudonym: Self.pseudonymHex(devicePseudonym),
            provider: provider
        )
        let resp: UnregisterResponse = try await post(
            endpoint: "/push/unregister",
            body: req,
            tokens: prepaidToken.map { PrepaidAnonToken($0, then: tokens) } ?? tokens
        )
        return resp.removed
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

    /// Lowercase-hex encode the 32-byte device pseudonym for the
    /// wire form `services/push-gw` expects.
    public static func pseudonymHex(_ bytes: Data) -> String {
        var out = String()
        out.reserveCapacity(bytes.count * 2)
        for b in bytes {
            let hi = Int(b >> 4) & 0x0F
            let lo = Int(b) & 0x0F
            out.append(hexAlphabet[hi])
            out.append(hexAlphabet[lo])
        }
        return out
    }

    private static let hexAlphabet: [Character] = Array("0123456789abcdef")
}

// MARK: - Wire shapes

// swiftlint:disable identifier_name
// Snake-case stored properties so the default `JSONEncoder`
// serialises the field names exactly as the server expects
// (no `convertToSnakeCase` round-trip ambiguity).
private struct PushRegisterRequest: Encodable {
    let device_pseudonym: String
    let provider: PushClient.Provider
    let token: String
}

private struct PushRegisterResponse: Decodable {
    let ok: Bool
}

private struct UnregisterRequest: Encodable {
    let device_pseudonym: String
    let provider: PushClient.Provider
}

private struct UnregisterResponse: Decodable {
    let ok: Bool
    let removed: Bool
}

// 1.20e — group fan-out wire shapes.
private struct GroupSubscribeRequest: Encodable {
    let device_pseudonym: String
    let group_pseudonym: String
}

private struct GroupSubscribeResponse: Decodable {
    let ok: Bool
    let inserted: Bool
}

private struct GroupUnsubscribeRequest: Encodable {
    let device_pseudonym: String
    let group_pseudonym: String
}

private struct GroupUnsubscribeResponse: Decodable {
    let ok: Bool
    let removed: Bool
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
