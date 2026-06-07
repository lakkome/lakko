// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Device-attestation verifying key (G-13, ADR-0015 sprint 5).
///
/// Peer devices verifying a removal attestation need the 32-byte Ed25519
/// public key the auth service signs attestations with, published as an
/// OKP / Ed25519 JWK at `GET /.well-known/device-attest-jwks.json`. Mirror of
/// `apps/web/src/auth/device-attest-key.ts`.
///
/// In the zero-trust posture this key should ideally be PINNED via the signed
/// bootstrap config rather than fetched from the protected origin; this is the
/// discovery path for the current single-origin deployment. The fetched key is
/// only ever handed to `DeviceGroup.processIncomingVerifyingRemovals`, which
/// performs the Ed25519 verification in the audited Rust core.
enum DeviceAttestKey {
    private struct Jwks: Decodable { let keys: [Jwk]? }
    private struct Jwk: Decodable { let x: String? }

    private static var cachedKey: Data?
    private static var cachedAt: Date?
    private static let ttl: TimeInterval = 24 * 60 * 60

    enum KeyError: Error {
        case http(Int)
        case empty
        case badLength(Int)
        case badUrl
    }

    /// Fetch (and cache for 24 h) the 32-byte device-attestation verifying key.
    static func fetch(
        baseURL: String = WebauthnClient.defaultBaseURL,
        now: Date = Date()
    ) async throws -> Data {
        if let key = cachedKey, let at = cachedAt, now.timeIntervalSince(at) < ttl {
            return key
        }
        guard let url = URL(string: baseURL + "/.well-known/device-attest-jwks.json") else {
            throw KeyError.badUrl
        }
        let (data, response) = try await URLSession.shared.data(from: url)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw KeyError.http(http.statusCode)
        }
        let jwks = try JSONDecoder().decode(Jwks.self, from: data)
        guard let x = jwks.keys?.first?.x else { throw KeyError.empty }
        let key = try base64UrlDecode(x)
        guard key.count == 32 else { throw KeyError.badLength(key.count) }
        cachedKey = key
        cachedAt = now
        return key
    }

    /// Drop the cached key (e.g. after a rotation signal).
    static func clearCache() {
        cachedKey = nil
        cachedAt = nil
    }
}
