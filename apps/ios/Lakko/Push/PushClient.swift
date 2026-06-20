// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// URLSession-based client for `lakko-svc-push-gw`.
///
/// Mirror of:
///
///   * `apps/web/src/push/client.ts` (1.20b)
///   * `apps/android/.../push/PushClient.kt` (1.20d)
///
/// Two endpoints from the user-device side (the third,
/// `/push/notify`, is driven by the DS fan-out — clients
/// themselves never notify):
///
///   * `POST /push/register` — bind a
///     `(device_pseudonym, provider)` pair to a platform-specific
///     token blob. Idempotent: re-register overwrites the prior
///     token (clients rotate after APNs rebuild / FCM token
///     refresh / Web Push subscription change).
///   * `POST /push/unregister` — drop a token. Called on
///     logout + forgetDevice.
///
/// Plus `/pow/challenge` for the per-call PoW seed, same shape
/// as the auth + discovery PoW path.
///
/// ## Auth posture
///
/// The endpoints don't require a session token in Phase 1 —
/// the PoW gate is the per-call anti-spam. A misbehaving caller
/// fails the verify on the server side; valid PoWs that hit
/// rate limits get 429.
///
/// ## Default base URL
///
/// `http://localhost:8085` — iOS simulator loopback + port
/// `8085` matching `lakko-svc-push-gw`'s dev bind. Override
/// for physical devices (LAN IP) and production (Cloudflare
/// tunnel URL).
public final class PushClient {

    public static let defaultBaseURL: String = "http://localhost:8085"

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
    }

    public let baseURL: String
    private let session: URLSession
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(
        baseURL: String = PushClient.defaultBaseURL,
        session: URLSession = .shared
    ) {
        self.baseURL = baseURL
        self.session = session
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
        let challenge = try await fetchChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )
        let req = PushRegisterRequest(
            pow: pow,
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
        let challenge = try await fetchChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )
        let req = GroupSubscribeRequest(
            pow: pow,
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
        let challenge = try await fetchChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )
        let req = GroupUnsubscribeRequest(
            pow: pow,
            device_pseudonym: Self.pseudonymHex(devicePseudonym),
            group_pseudonym: Self.pseudonymHex(groupPseudonym)
        )
        let resp: GroupUnsubscribeResponse = try await post(
            endpoint: "/push/group/unsubscribe",
            body: req
        )
        return resp.removed
    }

    /// Fan a wake out to every device subscribed to
    /// `groupPseudonym`. Phase 1.20e: the publisher's client
    /// fires this immediately after a successful
    /// `DsClient.publishGroup` so peers' devices wake.
    public func notifyGroup(
        groupPseudonym: Data,
        envelopePointer: Int64? = nil
    ) async throws -> Int {
        guard groupPseudonym.count == Self.devicePseudonymLength else {
            throw PushClientError.argument(
                "groupPseudonym must be \(Self.devicePseudonymLength) bytes"
            )
        }
        let challenge = try await fetchChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )
        let req = NotifyGroupRequest(
            pow: pow,
            group_pseudonym: Self.pseudonymHex(groupPseudonym),
            envelope_pointer: envelopePointer.map(UInt64.init)
        )
        let resp: NotifyGroupResponse = try await post(
            endpoint: "/push/notify-group",
            body: req
        )
        return resp.devices_woken
    }

    // MARK: - Unregister

    /// Drop a previously-registered token. Idempotent: returns
    /// `removed: false` if the row was already gone.
    public func unregister(
        devicePseudonym: Data,
        provider: Provider
    ) async throws -> Bool {
        guard devicePseudonym.count == Self.devicePseudonymLength else {
            throw PushClientError.argument(
                "devicePseudonym must be \(Self.devicePseudonymLength) bytes"
            )
        }
        let challenge = try await fetchChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )
        let req = UnregisterRequest(
            pow: pow,
            device_pseudonym: Self.pseudonymHex(devicePseudonym),
            provider: provider
        )
        let resp: UnregisterResponse = try await post(
            endpoint: "/push/unregister",
            body: req
        )
        return resp.removed
    }

    // MARK: - Internal HTTP

    private func fetchChallenge() async throws -> PowChallenge {
        let url = try buildURL(endpoint: "/pow/challenge")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        return try await execute(
            endpoint: "/pow/challenge",
            request: request
        )
    }

    private func post<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req
    ) async throws -> Res {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        do {
            request.httpBody = try encoder.encode(body)
        } catch {
            throw PushClientError.argument(
                "JSON encode failed for \(endpoint): \(error)"
            )
        }
        return try await execute(endpoint: endpoint, request: request)
    }

    private func execute<Res: Decodable>(
        endpoint: String,
        request: URLRequest
    ) async throws -> Res {
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        } catch {
            throw PushClientError.network(
                endpoint: endpoint,
                underlying: error
            )
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw PushClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
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

private struct PowChallenge: Decodable {
    let seed: String
    let effortBits: Int

    enum CodingKeys: String, CodingKey {
        case seed
        case effortBits = "effort_bits"
    }
}

// swiftlint:disable identifier_name
// Snake-case stored properties so the default `JSONEncoder`
// serialises the field names exactly as the server expects
// (no `convertToSnakeCase` round-trip ambiguity).
private struct PushRegisterRequest: Encodable {
    let pow: PowProofPayload
    let device_pseudonym: String
    let provider: PushClient.Provider
    let token: String
}

private struct PushRegisterResponse: Decodable {
    let ok: Bool
}

private struct UnregisterRequest: Encodable {
    let pow: PowProofPayload
    let device_pseudonym: String
    let provider: PushClient.Provider
}

private struct UnregisterResponse: Decodable {
    let ok: Bool
    let removed: Bool
}

// 1.20e — group fan-out wire shapes.
private struct GroupSubscribeRequest: Encodable {
    let pow: PowProofPayload
    let device_pseudonym: String
    let group_pseudonym: String
}

private struct GroupSubscribeResponse: Decodable {
    let ok: Bool
    let inserted: Bool
}

private struct GroupUnsubscribeRequest: Encodable {
    let pow: PowProofPayload
    let device_pseudonym: String
    let group_pseudonym: String
}

private struct GroupUnsubscribeResponse: Decodable {
    let ok: Bool
    let removed: Bool
}

private struct NotifyGroupRequest: Encodable {
    let pow: PowProofPayload
    let group_pseudonym: String
    let envelope_pointer: UInt64?
}

private struct NotifyGroupResponse: Decodable {
    let devices_woken: Int
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
