// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0026 Phase 4 — the native HTTP routing seam.
///
/// When anti-censorship is **armed**, a Lakko REST call is routed through the
/// best obfuscated transport (the Rust `tunneledHttpRequest` FFI: verify the
/// signed bootstrap config against a pinned trust anchor, dial the transports
/// fail-closed, run one HTTP/1.1 request over the winning tunnel). When
/// **disarmed** (the default), the call takes the direct `URLSession` path
/// unchanged — there is no behaviour change until a deployment arms it.
///
/// This is the foundation a single client (`DiscoveryClient`) adopts as the
/// pattern; the remaining clients migrate to the same `perform` seam in a
/// follow-up. The on-the-wire tunnel only does anything against a deployed
/// `services/obfs-proxy` (ADR-0026 Phase 5).
public struct AntiCensorshipConfig: Sendable {
    /// Whether to route through the obfuscated tunnel.
    public let armed: Bool
    /// The signed bootstrap-config JSON bytes (opaque; verified inside the FFI).
    public let bootstrapConfigJson: Data
    /// The 32-byte Ed25519 verifying key the config's signature is checked
    /// against (build-time pinned).
    public let trustAnchor: Data
    /// Dial deadline in milliseconds.
    public let deadlineMs: UInt64

    public init(
        armed: Bool,
        bootstrapConfigJson: Data,
        trustAnchor: Data,
        deadlineMs: UInt64 = 30_000
    ) {
        self.armed = armed
        self.bootstrapConfigJson = bootstrapConfigJson
        self.trustAnchor = trustAnchor
        self.deadlineMs = deadlineMs
    }

    /// The default: direct path, no tunnel. Safe — identical to today.
    public static let disarmed = AntiCensorshipConfig(
        armed: false,
        bootstrapConfigJson: Data(),
        trustAnchor: Data()
    )
}

/// Errors specific to the tunnel routing path.
public enum AntiCensorshipError: Error {
    /// The request URL lacked a host the tunnel could target.
    case missingHost
    /// The FFI returned a response that could not be reconstructed.
    case badResponse
}

public enum AntiCensorshipTransport {
    /// Perform `request` through the obfuscated tunnel when `config.armed`,
    /// otherwise via `session` directly. Returns the same `(Data, URLResponse)`
    /// shape `URLSession.data(for:)` does, so call sites are unchanged.
    public static func perform(
        _ request: URLRequest,
        session: URLSession,
        config: AntiCensorshipConfig
    ) async throws -> (Data, URLResponse) {
        guard config.armed else {
            return try await session.data(for: request)
        }
        guard let url = request.url, let host = url.host else {
            throw AntiCensorshipError.missingHost
        }
        let port = UInt16(url.port ?? (url.scheme == "http" ? 80 : 443))
        let method = request.httpMethod ?? "GET"
        var path = url.path.isEmpty ? "/" : url.path
        if let query = url.query, !query.isEmpty {
            path += "?\(query)"
        }
        let headers = (request.allHTTPHeaderFields ?? [:]).map {
            FfiHeader(name: $0.key, value: $0.value)
        }
        let body = request.httpBody ?? Data()
        let now = UInt64(Date().timeIntervalSince1970)

        // `tunneledHttpRequest` is synchronous (it drives a private tokio
        // runtime to completion), so run it off the cooperative pool.
        let cfg = config
        let resp = try await Task.detached(priority: .userInitiated) {
            try tunneledHttpRequest(
                configJson: cfg.bootstrapConfigJson,
                trustAnchor: cfg.trustAnchor,
                nowSecs: now,
                endpointHost: host,
                endpointPort: port,
                deadlineMs: cfg.deadlineMs,
                method: method,
                path: path,
                host: host,
                headers: headers,
                body: body
            )
        }.value

        let headerFields = Dictionary(
            resp.headers.map { ($0.name, $0.value) },
            uniquingKeysWith: { first, _ in first }
        )
        guard
            let httpResponse = HTTPURLResponse(
                url: url,
                statusCode: Int(resp.status),
                httpVersion: "HTTP/1.1",
                headerFields: headerFields
            )
        else {
            throw AntiCensorshipError.badResponse
        }
        return (resp.body, httpResponse)
    }
}
