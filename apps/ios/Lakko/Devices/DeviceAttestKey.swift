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
/// The strongest posture PINS this key out-of-band so the client never trusts
/// the auth origin to serve it. A build-time pin is supported via the
/// `Info.plist` key `LakkoDeviceAttestKey` (base64url of the 32-byte key); when
/// set, the JWKS endpoint is never consulted. This defends the case where the
/// JWKS-serving infra is compromised but the (isolated) signing key is not.
/// When unset, the JWKS fetch is the discovery path. Either way the key is only
/// ever handed to `DeviceGroup.processIncomingVerifyingRemovals`, which performs
/// the Ed25519 verification in the audited Rust core.
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

    /// The build-time pinned key (`Info.plist` `LakkoDeviceAttestKey`,
    /// base64url of 32 bytes), or `nil` when unset / malformed.
    ///
    /// Internal (not `private`) so a caller can tell whether ``fetch(baseURL:now:)``
    /// returned the build-time pin (strong posture) or fell back to the JWKS
    /// endpoint (weaker — the auth origin is trusted to serve the key). The pin
    /// wins outright in ``fetch(baseURL:now:)``, so `pinnedKey() == nil` is an
    /// exact, race-free indicator that a fetched key came from the JWKS endpoint.
    /// Mirror of the web `pinnedDeviceAttestKey`.
    static func pinnedKey() -> Data? {
        guard
            let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoDeviceAttestKey") as? String
        else {
            return nil
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let key = base64UrlDecodeNoPad(trimmed), key.count == 32 else {
            return nil
        }
        return key
    }

    /// Fetch (and cache for 24 h) the 32-byte device-attestation verifying key.
    static func fetch(
        baseURL: String = WebauthnClient.defaultBaseURL,
        now: Date = Date()
    ) async throws -> Data {
        // A build-time pinned key wins outright — never consult the JWKS endpoint.
        if let pinned = pinnedKey() {
            return pinned
        }
        if let key = cachedKey, let at = cachedAt, now.timeIntervalSince(at) < ttl {
            return key
        }
        guard let url = URL(string: baseURL + "/.well-known/device-attest-jwks.json") else {
            throw KeyError.badUrl
        }
        // ADR-0026 — route through the obfuscated tunnel when armed (the last
        // native HTTP surface); disarmed = the direct URLSession path.
        let (data, response) = try await AntiCensorshipTransport.perform(
            URLRequest(url: url), session: .shared, config: .current)
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
