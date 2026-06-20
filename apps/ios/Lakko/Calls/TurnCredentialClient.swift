// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// Client for `POST /call/turn-credential` (G3-{1,2}-turn step 2).
///
/// Mirror of [`CallJwtClient`](CallJwtClient.swift). The auth
/// service mints a short-lived credential (10-minute TTL by
/// default) per the coturn `static-auth-secret` REST API
/// convention; we hand the result to libwebrtc as an
/// `RTCIceServer` row at call start.
///
/// ## Fail-soft posture
///
/// A 503 response (`turn_not_configured`) is the **expected**
/// state when the deployment is STUN-only — the caller
/// proceeds with the STUN-only ICE list returned by
/// `WebRTCPeerConnection.defaultIceServers()`. A genuine
/// network error (transport failure / 5xx other than 503) is
/// also collapsed to "no TURN this call" — the call attempt
/// continues, and ICE will negotiate over whichever paths
/// STUN candidates can find. This is the right trade-off
/// because the alternative is refusing to dial when the
/// auth service is briefly unreachable, which is a much worse
/// UX than a degraded-NAT-traversal call.
///
/// The single situation we **don't** fail-soft on is a server
/// 200 with a malformed body — that indicates a deployment
/// mismatch the caller should know about. Surfaced as
/// [`TurnCredentialClientError`].
public final class TurnCredentialClient: @unchecked Sendable {
    public static let defaultBaseURL = "http://127.0.0.1:8081"

    public let baseURL: String
    private let session: URLSession
    private let decoder: JSONDecoder

    private let stateLock = NSLock()
    private var sessionToken: String?

    public init(
        baseURL: String = TurnCredentialClient.defaultBaseURL,
        session: URLSession = .shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.decoder = JSONDecoder()
        self.decoder.keyDecodingStrategy = .convertFromSnakeCase
    }

    /// Rebind the bearer token. `nil` clears.
    public func setSessionToken(_ token: String?) {
        stateLock.lock()
        defer { stateLock.unlock() }
        sessionToken = token
    }

    private func currentSessionToken() -> String? {
        stateLock.lock()
        defer { stateLock.unlock() }
        return sessionToken
    }

    /// `POST /call/turn-credential` — returns one
    /// `RTCIceServer` row ready to drop into
    /// `RTCConfiguration.iceServers`, OR `nil` if the auth
    /// service reports the endpoint is not configured (a 503
    /// with `turn_not_configured`).
    ///
    /// Network / transport failures surface as `nil` too,
    /// per the fail-soft posture documented above. Inspect
    /// the underlying `URLSession` logs for the actual root
    /// cause when debugging — this client is intentionally
    /// quiet.
    public func fetchOrNil() async -> RTCIceServer? {
        do {
            return try await fetch()
        } catch {
            NSLog("[lakko.turn] fetch failed, proceeding STUN-only: %@", String(describing: error))
            return nil
        }
    }

    /// Stricter variant of `fetchOrNil` — surfaces every error
    /// to the caller. Use in tests; production code wants
    /// fail-soft so a brief auth-service hiccup doesn't
    /// abort the call.
    public func fetch() async throws -> RTCIceServer? {
        guard let token = currentSessionToken() else {
            throw TurnCredentialClientError.argument(
                "session token is not bound — call setSessionToken before fetch()"
            )
        }
        let endpoint = "/call/turn-credential"
        guard let url = URL(string: baseURL + endpoint) else {
            throw TurnCredentialClientError.argument("invalid baseURL: \(baseURL)")
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")

        let (data, response) = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw TurnCredentialClientError.network(endpoint: endpoint, status: -1)
        }
        if http.statusCode == 503 {
            // Auth service reports TURN is not configured.
            // The body is `{"error":"turn_not_configured",...}` —
            // we don't introspect it; the status code carries
            // the contract.
            return nil
        }
        guard (200...299).contains(http.statusCode) else {
            throw TurnCredentialClientError.network(endpoint: endpoint, status: http.statusCode)
        }
        let parsed: TurnCredentialResponse
        do {
            parsed = try decoder.decode(TurnCredentialResponse.self, from: data)
        } catch {
            throw TurnCredentialClientError.decode(reason: String(describing: error))
        }
        return RTCIceServer(
            urlStrings: parsed.urls,
            username: parsed.username,
            credential: parsed.password
        )
    }
}

/// JSON wire-shape the auth service returns. Mirror of the
/// Rust `TurnCredentialResponse` in `services/auth/src/turn.rs`.
struct TurnCredentialResponse: Decodable {
    let urls: [String]
    let username: String
    let password: String
    let ttlSecs: UInt64
    let expiryUnixSecs: UInt64
}

public enum TurnCredentialClientError: Error {
    /// Caller-side wiring bug (missing bearer / bad baseURL).
    case argument(String)
    /// Transport / HTTP failure.
    case network(endpoint: String, status: Int)
    /// 200 with malformed body — deployment mismatch.
    case decode(reason: String)
}
