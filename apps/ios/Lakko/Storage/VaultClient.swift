// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// What a vault route answered, when it was not a success.
public enum VaultClientError: Error, Equatable {
    /// A write to a vault that is not claimed (the service answers every
    /// unverifiable proof `401 vault-proof-refused`, an unclaimed vault
    /// included): claim it and write again.
    case unclaimed
    /// `409 vault-version-conflict`: the version is not current + 1.
    case conflict
    /// `409 vault-claimed`: the vault is claimed under another write key.
    case claimedElsewhere
    /// Any other answer.
    case http(status: Int, code: String?)
    /// The request did not reach the service.
    case network
}

/// The storage service's vault routes (ADR-0064 section 1), the head and the
/// claim only. Mirror of the routes in `services/storage/src/vault.rs`.
///
/// No session and no account: a claim spends one `vault-claim` anonymous
/// token; every other request carries the storage proof the caller signs
/// over the exact body with the vault's write key (`VaultKeys.signRequest`,
/// over the SERVICE path `/vault/<hex>/head`, never the reverse-proxy
/// prefix). Requests ride the anonymous connection pool, through the
/// anti-censorship tunnel when it is armed.
///
/// A head write's body and a head read's answer are
/// `version (u64 big-endian) || sealed object`.
public struct VaultClient {
    public let baseURL: String
    private let tokens: AnonTokenSource
    private let perform: (URLRequest) async throws -> (Data, HTTPURLResponse)

    /// A request that waits longer gives up (a head write must never hold a
    /// commit for long). Chosen, not measured.
    public static let timeoutSeconds: TimeInterval = 15

    public init(
        baseURL: String = StorageClient.defaultBaseURL,
        tokens: AnonTokenSource = AnonTokenPool.shared,
        perform: ((URLRequest) async throws -> (Data, HTTPURLResponse))? = nil
    ) {
        self.baseURL = baseURL
        self.tokens = tokens
        self.perform = perform ?? { request in
            let (data, response) = try await AntiCensorshipTransport.perform(
                request, session: AnonymousHTTP.session, config: .current)
            guard let http = response as? HTTPURLResponse else { throw VaultClientError.network }
            return (data, http)
        }
    }

    /// The service path of a vault's head (what the proof signs).
    public static func headPath(_ vaultIdHex: String) -> String { "/vault/\(vaultIdHex)/head" }

    /// The service path of a vault (the claim).
    public static func vaultPath(_ vaultIdHex: String) -> String { "/vault/\(vaultIdHex)" }

    /// `version || sealed`, the body of a head write.
    public static func objectBody(version: UInt64, sealed: Data) -> Data {
        var body = Data(count: 8)
        for i in 0..<8 { body[i] = UInt8(truncatingIfNeeded: version >> (56 - 8 * UInt64(i))) }
        body.append(sealed)
        return body
    }

    /// The version and the sealed object of a head read's answer; `nil` when
    /// shorter than a version.
    public static func splitObject(_ body: Data) -> (version: UInt64, sealed: Data)? {
        guard body.count > 8 else { return nil }
        let bytes = [UInt8](body.prefix(8))
        let version = bytes.reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
        return (version, Data(body.dropFirst(8)))
    }

    private func request(_ method: String, _ path: String, body: Data?) throws -> URLRequest {
        guard let url = URL(string: baseURL + path) else { throw VaultClientError.http(status: 0, code: "bad-url") }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = Self.timeoutSeconds
        if let body {
            request.httpBody = body
            request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        }
        return request
    }

    private func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        do {
            return try await perform(request)
        } catch let error as VaultClientError {
            throw error
        } catch is AnonTokenUnavailable {
            throw VaultClientError.http(status: 0, code: "anon-token-unavailable")
        } catch {
            throw VaultClientError.network
        }
    }

    /// `PUT /vault/{id}` with one `vault-claim` token. `201` and `200` (the
    /// same key) succeed.
    public func claim(vaultIdHex: String, writePublicKey: Data) async throws {
        let json = try JSONSerialization.data(withJSONObject: ["write_pk": base64UrlEncodeNoPad(writePublicKey)])
        var request = try request("PUT", Self.vaultPath(vaultIdHex), body: json)
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let (data, http): (Data, HTTPURLResponse)
        do {
            (data, http) = try await AnonTokenRoute.send(context: .vaultClaim, tokens: tokens) { header in
                var signed = request
                signed.setValue(header, forHTTPHeaderField: AnonTokenRoute.headerName)
                return try await self.send(signed)
            }
        } catch let error as VaultClientError {
            throw error
        } catch {
            throw VaultClientError.http(status: 0, code: "anon-token-unavailable")
        }
        switch http.statusCode {
        case 200, 201: return
        case 409 where AnonTokenRoute.errorCode(in: data) == "vault-claimed": throw VaultClientError.claimedElsewhere
        default: throw VaultClientError.http(status: http.statusCode, code: AnonTokenRoute.errorCode(in: data))
        }
    }

    /// `PUT /vault/{id}/head`. `proof` is the storage proof over `body`.
    public func putHead(vaultIdHex: String, body: Data, proof: String) async throws {
        var request = try request("PUT", Self.headPath(vaultIdHex), body: body)
        request.setValue(proof, forHTTPHeaderField: Self.proofHeader)
        let (data, http) = try await send(request)
        let code = AnonTokenRoute.errorCode(in: data)
        switch http.statusCode {
        case 200, 201: return
        case 401: throw VaultClientError.unclaimed
        case 409 where code == "vault-version-conflict": throw VaultClientError.conflict
        default: throw VaultClientError.http(status: http.statusCode, code: code)
        }
    }

    /// `GET /vault/{id}/head`; `nil` when there is none the proof opens
    /// (`401` for a vault never claimed, `404` for one with no head yet).
    public func getHead(vaultIdHex: String, proof: String) async throws -> (version: UInt64, sealed: Data)? {
        var request = try request("GET", Self.headPath(vaultIdHex), body: nil)
        request.setValue(proof, forHTTPHeaderField: Self.proofHeader)
        let (data, http) = try await send(request)
        switch http.statusCode {
        case 200:
            guard let object = Self.splitObject(data) else {
                throw VaultClientError.http(status: 200, code: "vault-body-malformed")
            }
            return object
        case 401, 404: return nil
        default: throw VaultClientError.http(status: http.statusCode, code: AnonTokenRoute.errorCode(in: data))
        }
    }

    /// `DELETE /vault/{id}` with the proof over an empty body. `204`
    /// succeeds; `401` / `404` (no such vault, or never claimed) leave
    /// nothing to delete and succeed too.
    public func deleteVault(vaultIdHex: String, proof: String) async throws {
        var request = try request("DELETE", Self.vaultPath(vaultIdHex), body: nil)
        request.setValue(proof, forHTTPHeaderField: Self.proofHeader)
        let (data, http) = try await send(request)
        switch http.statusCode {
        case 200, 204, 401, 404: return
        default: throw VaultClientError.http(status: http.statusCode, code: AnonTokenRoute.errorCode(in: data))
        }
    }

    /// The storage proof header name, from the core.
    public static var proofHeader: String { storageProofHeaderName() }
}
