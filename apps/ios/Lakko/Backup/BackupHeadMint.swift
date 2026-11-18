// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Backup-head mint client — `POST /auth/backup/head` (ADR-0037 §2, G-11).
///
/// Swift mirror of `apps/web/src/backup/head-mint.ts`.
///
/// The auth service holds the per-account monotone `backup_version` counter and
/// signs a `BackupHead` committing the envelope the client is about to store.
/// This is the ONE round-trip the upload path adds: after building the AEAD
/// envelope the client computes its `BLAKE3` (the UniFFI `backupEnvelopeHash`
/// helper), mints the head here, and frames `TAG ‖ head ‖ envelope` into the
/// single storage `PUT`.
///
/// Bearer-authenticated: holding a valid session token IS proof the caller
/// OPAQUE-authenticated as `account_id`, which is the security basis — auth binds
/// THAT account to the client-presented envelope hash. No PoW (mirrors the
/// genesis-attest mint).
///
/// A mint failure is **soft** on the upload side (ADR-0037 §6): the caller falls
/// back to writing the unframed legacy envelope rather than failing the whole
/// backup. So this helper simply throws on any failure and lets the seam decide.

/// The minted head + its auth-assigned monotone version. Mirror of the web
/// `MintedBackupHead`.
public struct MintedBackupHead {
    /// The raw signed `BackupHead` bytes (decoded from `backup_head_b64`).
    public let head: Data
    /// The auth-assigned monotone `backup_version` — the floor a future restore
    /// on this device lineage compares against.
    public let version: UInt64
}

/// Error vocabulary for the mint round-trip.
public enum BackupHeadMintError: Error, LocalizedError, Equatable {
    case noSessionToken
    case badUrl
    case http(Int)
    case malformedResponse
    /// Auth answered `key-generation-superseded` or `device-revoked` (or the
    /// session provider already knows it): this device is being signed out.
    /// NOT soft: the upload stops, no legacy envelope is written, since
    /// storage checks the token without auth and would accept a stale
    /// device's blob over the account's backup.
    case signedOut

    public var errorDescription: String? {
        switch self {
        case .noSessionToken: return "mintBackupHead: no session token"
        case .badUrl: return "mintBackupHead: invalid base URL"
        case .http(let s): return "mintBackupHead: /auth/backup/head -> \(s)"
        case .malformedResponse: return "mintBackupHead: malformed response"
        case .signedOut: return "mintBackupHead: this device's session was superseded or revoked"
        }
    }
}

/// `POST /auth/backup/head` request wire shape. `camelCase` in Swift, remapped
/// to the service's snake_case JSON via `CodingKeys` (the app-wide wire
/// convention — see `AuthWire.swift`).
private struct BackupHeadRequest: Encodable {
    let envelopeHashB64: String

    enum CodingKeys: String, CodingKey {
        case envelopeHashB64 = "envelope_hash_b64"
    }
}

/// `POST /auth/backup/head` response wire shape.
private struct BackupHeadResponse: Decodable {
    let backupHeadB64: String
    let backupVersion: UInt64

    enum CodingKeys: String, CodingKey {
        case backupHeadB64 = "backup_head_b64"
        case backupVersion = "backup_version"
    }
}

/// Mint an auth-signed backup head for `envelopeHash`.
///
/// - Parameters:
///   - sessionToken: bearer token proving the OPAQUE auth (`nil` / empty →
///     throws, so the soft-fail path writes the legacy envelope).
///   - envelopeHash: the 32-byte `BLAKE3` of the envelope about to be stored.
///   - baseURL: auth-service base (defaults to ``AuthClient/defaultBaseURL``).
/// - Throws: ``BackupHeadMintError`` on a null token, bad URL, non-2xx, or a
///   malformed response.
public func mintBackupHead(
    sessionToken: String?,
    envelopeHash: Data,
    baseURL: String = AuthClient.defaultBaseURL,
    session: URLSession = .shared
) async throws -> MintedBackupHead {
    guard let token = sessionToken, !token.isEmpty else {
        throw BackupHeadMintError.noSessionToken
    }
    guard let url = URL(string: baseURL + "/auth/backup/head") else {
        throw BackupHeadMintError.badUrl
    }

    var request = URLRequest(url: url)
    request.httpMethod = "POST"
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
    request.httpBody = try JSONEncoder().encode(
        BackupHeadRequest(envelopeHashB64: base64UrlEncodeNoPad(envelopeHash))
    )

    // ADR-0026 — route through the obfuscated tunnel when armed (an auth
    // surface, like the DeviceAttestKey JWKS fetch); disarmed = the direct
    // URLSession path.
    let data: Data
    let response: URLResponse
    do {
        (data, response) = try await AntiCensorshipTransport.perform(
            request, session: session, config: .current)
    } catch is SessionSupersededError {
        throw BackupHeadMintError.signedOut
    } catch is SessionDeviceRevokedError {
        throw BackupHeadMintError.signedOut
    }
    guard let http = response as? HTTPURLResponse else {
        throw BackupHeadMintError.http(0)
    }
    guard (200..<300).contains(http.statusCode) else {
        throw backupHeadMintRefusal(status: http.statusCode, body: data)
    }
    let wire: BackupHeadResponse
    do {
        wire = try JSONDecoder().decode(BackupHeadResponse.self, from: data)
    } catch {
        throw BackupHeadMintError.malformedResponse
    }
    guard let head = base64UrlDecodeNoPad(wire.backupHeadB64) else {
        throw BackupHeadMintError.malformedResponse
    }
    return MintedBackupHead(head: head, version: wire.backupVersion)
}

/// The error a non-2xx mint answer is read as: a sign-out signal
/// (``BackupHeadMintError/signedOut``) or the plain status.
func backupHeadMintRefusal(status: Int, body: Data) -> BackupHeadMintError {
    if status == 401 {
        switch SessionTokenRule.classify401(body: body) {
        case .keyGenerationSuperseded, .deviceRevoked: return .signedOut
        case .refreshInvalid, .other: break
        }
    }
    return .http(status)
}
