// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// URLSession-based client for `lakko-svc-storage`.
///
/// Mirror of:
///
///   * `apps/web/src/storage/client.ts` (1.19c1)
///   * `apps/android/.../storage/StorageClient.kt` (1.19c1)
///
/// Content-addressable blob store (ADR-0065):
///
///   * `PUT  /blob/<sha256_hex>` — body = ciphertext, with an upload pass
///     and the blob's delete public key. 201 new, 200 existed.
///   * `GET` / `HEAD /blob/<sha256_hex>` — with a read pass. 404 = the file
///     is gone (no retry helps).
///   * `POST /blob/keep` — mark files accessed today (read pass).
///   * `DELETE /blob/<sha256_hex>` — signed under the delete key, no pass.
///
/// ## Auth posture
///
/// The blob routes carry no session and no account: an hour's pass bought
/// with one anonymous token per direction (``BlobAccess``), sent on the
/// anonymous connection pool, through the anti-censorship tunnel when it is
/// armed (uploads included).
///
/// ## Hash boundary
///
/// The `blob_id` parameter is **always** the raw 32-byte SHA-256
/// of the *ciphertext* — exactly what `EncryptedAttachment.blobId`
/// returns from the UniFFI surface. Callers do not compute the hash
/// themselves; they hand the bytes from the encryption layer
/// straight through.
///
/// ## Default base URL
///
/// `<LakkoApiBaseUrl>/storage` (``ApiEndpoints``); with no API base
/// URL, `http://localhost:8083` — iOS simulator loopback + port `8083`
/// matching `lakko-svc-storage`'s dev bind.
public final class StorageClient {

    public static var defaultBaseURL: String { ApiEndpoints.baseURL(for: .storage) }

    /// Raw SHA-256 blob id length.
    public static let blobIdLength: Int = 32

    /// Per-blob ceiling. Mirrors `MAX_BLOB_BYTES` in
    /// `services/storage/src/main.rs` and `MAX_ATTACHMENT_BYTES`
    /// in `lakko-attachments` — surface here so a misbehaving
    /// caller fails fast without spending an oversize-body
    /// round-trip.
    public static let maxBlobBytes: Int = 50 * 1024 * 1024

    /// The most ids one keep request carries (the server's bound).
    public static let maxKeepIds = 256

    public let baseURL: String
    private let session: URLSession
    /// The anonymous pool the blob routes travel on.
    private let blobSession: URLSession
    private let access: BlobAccess
    /// The delete-key header value for a blob, and the signed delete (the
    /// account's vault keys in production; seams for tests).
    private let deleteKey: @Sendable (Data) async -> String?
    private let deleteProof: @Sendable (Data, String) async -> String?
    /// Bearer token bound to the per-account `/backup` routes
    /// (Phase 2a1c). Never sent on a `/blob/...` route.
    private var sessionToken: String?

    public init(
        baseURL: String = StorageClient.defaultBaseURL,
        session: URLSession = .shared,
        access: BlobAccess? = nil,
        deleteKey: (@Sendable (Data) async -> String?)? = nil,
        deleteProof: (@Sendable (Data, String) async -> String?)? = nil
    ) {
        self.baseURL = baseURL
        self.session = session
        let blobSession = AnonymousHTTP.session(pairedWith: session)
        self.blobSession = blobSession
        if let access {
            self.access = access
        } else if session === URLSession.shared, baseURL == StorageClient.defaultBaseURL {
            self.access = .shared
        } else {
            self.access = BlobAccess(session: blobSession, baseURL: { baseURL })
        }
        self.deleteKey = deleteKey ?? { blobId in await BlobDeleteKeys.headerValue(blobId: blobId) }
        self.deleteProof = deleteProof ?? { blobId, path in await BlobDeleteKeys.proof(blobId: blobId, path: path) }
        self.sessionToken = nil
    }

    /// Bind / rebind the bearer token used for the per-account
    /// backup routes. Mirror of `apps/web`'s
    /// `StorageClient.setSessionToken`. Caller drives this from
    /// the same `AuthRepository.identitySecrets?.sessionToken`
    /// signal the discovery / ds clients already consume.
    public func setSessionToken(_ token: String?) {
        self.sessionToken = token
    }

    // MARK: - The pass

    /// Send one blob request under a pass of `kind`. A refused pass (expired,
    /// unknown after a server restart, its budget used up) earns one new pass
    /// and one more send; a second used-up budget is the limit state.
    private func sendWithPass(
        _ kind: BlobPassKind,
        endpoint: String,
        _ perform: (String) async throws -> (Data, HTTPURLResponse)
    ) async throws -> (Data, HTTPURLResponse, BlobAnswer) {
        var renewed = false
        while true {
            let pass: BlobPass
            do {
                pass = try await access.pass(kind)
            } catch let unavailable as AnonTokenUnavailable {
                if case .quotaExhausted = unavailable { throw StorageClientError.limited(kind) }
                throw unavailable
            }
            let (data, http) = try await perform(pass.value)
            let answer = BlobAnswer.classify(status: http.statusCode, body: data)
            guard answer == .renewPass else { return (data, http, answer) }
            await access.refused(kind, value: pass.value)
            let code = AnonTokenRoute.errorCode(in: data)
            if renewed {
                if BlobAnswer.isLimit(status: http.statusCode, code: code) {
                    throw StorageClientError.limited(kind)
                }
                return (data, http, .failed(status: http.statusCode, code: code))
            }
            renewed = true
        }
    }

    private func transportError(_ endpoint: String, _ error: Error) -> Error {
        if error is StorageClientError || error is AnonTokenUnavailable { return error }
        return StorageClientError.network(endpoint: endpoint, underlying: error)
    }

    // MARK: - PUT

    /// `PUT /blob/<hex>` — upload ciphertext under an upload pass, with the
    /// blob's delete public key (ADR-0065 section 6). Idempotent.
    ///
    /// Throws ``StorageClientError/limited(_:)`` when the upload allowance
    /// is used up (UX 4.2).
    ///
    /// - Parameters:
    ///   - blobId: 32-byte SHA-256 of the ciphertext (the
    ///     `blob_id` returned by `EncryptedAttachment.blobId`).
    ///   - ciphertext: The bytes to store. Must hash to `blobId`.
    ///   - progress: ADR-0047 — optional fraction-sent callback (0...1) for
    ///     the timeline's determinate upload ring. Called from the session's
    ///     delegate queue; callers hop to the main actor themselves. On the
    ///     anti-censorship tunnel the body leaves in one piece, so the ring
    ///     jumps from 0 to 1.
    public func putBlob(
        blobId: Data,
        ciphertext: Data,
        idleDays: UInt8? = nil,
        progress: (@Sendable (Double) -> Void)? = nil
    ) async throws {
        guard blobId.count == Self.blobIdLength else {
            throw StorageClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes (got \(blobId.count))"
            )
        }
        guard !ciphertext.isEmpty else {
            throw StorageClientError.argument("ciphertext must be non-empty")
        }
        guard ciphertext.count <= Self.maxBlobBytes else {
            throw StorageClientError.argument(
                "ciphertext exceeds the \(Self.maxBlobBytes)-byte ceiling "
                    + "(got \(ciphertext.count) bytes)"
            )
        }
        guard let deleteKeyValue = await deleteKey(blobId) else {
            throw StorageClientError.argument("no account to derive the blob delete key from")
        }

        let hex = Self.blobIdHex(blobId)
        let endpoint = "/blob/\(hex)"
        let base = try buildRequest(endpoint: endpoint, method: "PUT")
        let blobSession = self.blobSession
        let (data, http, answer) = try await sendWithPass(.upload, endpoint: endpoint) { pass in
            var request = base
            request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
            request.setValue(String(ciphertext.count), forHTTPHeaderField: "Content-Length")
            request.setValue(pass, forHTTPHeaderField: BlobDeleteKeys.passHeader)
            request.setValue(deleteKeyValue, forHTTPHeaderField: BlobDeleteKeys.deleteKeyHeader)
            // ADR-0087 section 13 — a shorter idle life (a channel file).
            if let idleDays, idleDays > 0 {
                request.setValue(String(idleDays), forHTTPHeaderField: "x-lakko-blob-idle-days")
            }
            let raw: (Data, URLResponse)
            do {
                let config = AntiCensorshipConfig.current
                if config.armed {
                    // ADR-0065 residual 5 — an armed device uploads through
                    // the tunnel like every other request.
                    request.httpBody = ciphertext
                    raw = try await AntiCensorshipTransport.perform(request, session: blobSession, config: config)
                    progress?(1)
                } else if let progress {
                    raw = try await blobSession.upload(
                        for: request, from: ciphertext,
                        delegate: UploadProgressDelegate(onProgress: progress))
                } else {
                    raw = try await blobSession.upload(for: request, from: ciphertext)
                }
            } catch {
                throw self.transportError(endpoint, error)
            }
            guard let http = raw.1 as? HTTPURLResponse else {
                throw StorageClientError.http(status: 0, endpoint: endpoint, body: "non-HTTP response")
            }
            return (raw.0, http)
        }
        if answer == .ok { return }
        throw StorageClientError.http(
            status: http.statusCode, endpoint: endpoint,
            body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>"))
    }

    // MARK: - GET

    /// `GET /blob/<hex>` — fetch ciphertext under a read pass. Returns the
    /// raw bytes on 200, `nil` when the file is gone (404, UX 4.1). Throws
    /// ``StorageClientError/limited(_:)`` when the read allowance is used up
    /// (UX 4.2), and on any other failure.
    public func getBlob(blobId: Data) async throws -> Data? {
        guard blobId.count == Self.blobIdLength else {
            throw StorageClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes (got \(blobId.count))"
            )
        }
        let hex = Self.blobIdHex(blobId)
        let endpoint = "/blob/\(hex)"
        let base = try buildRequest(endpoint: endpoint, method: "GET")
        let (data, http, answer) = try await sendWithPass(.read, endpoint: endpoint) { pass in
            try await self.performBlob(base, pass: pass, endpoint: endpoint)
        }
        switch answer {
        case .ok: return data
        case .gone: return nil
        case .renewPass, .failed:
            throw StorageClientError.http(
                status: http.statusCode, endpoint: endpoint,
                body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>"))
        }
    }

    // MARK: - HEAD

    /// `HEAD /blob/<hex>` — the blob's `Content-Length` if it exists, `nil`
    /// if it is gone. Costs one download of the read pass.
    public func headBlob(blobId: Data) async throws -> Int? {
        guard blobId.count == Self.blobIdLength else {
            throw StorageClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes (got \(blobId.count))"
            )
        }
        let hex = Self.blobIdHex(blobId)
        let endpoint = "/blob/\(hex)"
        let base = try buildRequest(endpoint: endpoint, method: "HEAD")
        let (_, http, answer) = try await sendWithPass(.read, endpoint: endpoint) { pass in
            try await self.performBlob(base, pass: pass, endpoint: endpoint)
        }
        switch answer {
        case .ok:
            return http.value(forHTTPHeaderField: "Content-Length").flatMap(Int.init)
        case .gone:
            return nil
        case .renewPass, .failed:
            throw StorageClientError.http(
                status: http.statusCode, endpoint: endpoint,
                body: "HEAD \(endpoint) returned \(http.statusCode)")
        }
    }

    // MARK: - Keep and delete (ADR-0065 sections 3 and 6)

    /// `POST /blob/keep` — mark up to ``maxKeepIds`` files accessed today.
    /// The server answers 204 whether the ids exist or not.
    public func keepBlobs(_ blobIds: [Data]) async throws {
        let ids = blobIds.filter { $0.count == Self.blobIdLength }.prefix(Self.maxKeepIds)
        guard !ids.isEmpty else { return }
        let endpoint = "/blob/keep"
        var base = try buildRequest(endpoint: endpoint, method: "POST")
        base.setValue("application/json", forHTTPHeaderField: "Content-Type")
        base.httpBody = try JSONSerialization.data(withJSONObject: ["ids": ids.map(Self.blobIdHex)])
        let (data, http, answer) = try await sendWithPass(.read, endpoint: endpoint) { pass in
            try await self.performBlob(base, pass: pass, endpoint: endpoint)
        }
        guard answer == .ok else {
            throw StorageClientError.http(
                status: http.statusCode, endpoint: endpoint,
                body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>"))
        }
    }

    /// `DELETE /blob/<hex>` — signed under the blob's delete key, no pass.
    /// The server answers 204, or one uniform 403 `blob-delete-refused` for
    /// every refusal (not this account's, gone already), which is final and
    /// treated as done. Anything else is retried later.
    public func deleteBlob(blobId: Data) async throws {
        guard blobId.count == Self.blobIdLength else {
            throw StorageClientError.argument("blobId must be \(Self.blobIdLength) bytes")
        }
        let hex = Self.blobIdHex(blobId)
        let endpoint = "/blob/\(hex)"
        guard let proof = await deleteProof(blobId, endpoint) else {
            throw StorageClientError.argument("no account to sign the blob delete with")
        }
        var request = try buildRequest(endpoint: endpoint, method: "DELETE")
        request.setValue(proof, forHTTPHeaderField: BlobDeleteKeys.proofHeader)
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: blobSession, config: .current)
        } catch {
            throw transportError(endpoint, error)
        }
        guard let http = raw.1 as? HTTPURLResponse else {
            throw StorageClientError.http(status: 0, endpoint: endpoint, body: "non-HTTP response")
        }
        if (200..<300).contains(http.statusCode) || http.statusCode == 403 { return }
        throw StorageClientError.http(
            status: http.statusCode, endpoint: endpoint,
            body: Self.truncate(String(data: raw.0, encoding: .utf8) ?? "<binary>"))
    }

    private func performBlob(_ base: URLRequest, pass: String, endpoint: String) async throws -> (Data, HTTPURLResponse) {
        var request = base
        request.setValue(pass, forHTTPHeaderField: BlobDeleteKeys.passHeader)
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: blobSession, config: .current)
        } catch {
            throw transportError(endpoint, error)
        }
        guard let http = raw.1 as? HTTPURLResponse else {
            throw StorageClientError.http(status: 0, endpoint: endpoint, body: "non-HTTP response")
        }
        return (raw.0, http)
    }

    // MARK: - Per-account backup (Phase 2a1c)

    /// `PUT /backup` — overwrite this session's backup blob. Body
    /// is the wire bytes from `sealBackup` (UniFFI). The server
    /// identifies the owning account via the bearer session token
    /// — there is no account_id in the URL or body, so a stolen
    /// token cannot scribble over another account.
    public func putBackup(envelopeBytes: Data) async throws {
        let endpoint = "/backup"
        var request = try buildRequest(endpoint: endpoint, method: "PUT")
        request.setValue(
            "application/octet-stream",
            forHTTPHeaderField: "Content-Type"
        )
        attachAuth(to: &request)

        let raw: (Data, URLResponse)
        do {
            raw = try await session.upload(for: request, from: envelopeBytes)
        } catch {
            throw StorageClientError.network(endpoint: endpoint, underlying: error)
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw StorageClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        if (200..<300).contains(httpResponse.statusCode) {
            return
        }
        throw StorageClientError.http(
            status: httpResponse.statusCode,
            endpoint: endpoint,
            body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>")
        )
    }

    /// `GET /backup` — fetch this session's backup blob, or `nil`
    /// when none has been uploaded yet.
    public func getBackup() async throws -> Data? {
        let endpoint = "/backup"
        var request = try buildRequest(endpoint: endpoint, method: "GET")
        attachAuth(to: &request)

        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        } catch {
            throw StorageClientError.network(endpoint: endpoint, underlying: error)
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw StorageClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        if httpResponse.statusCode == 404 {
            return nil
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            throw StorageClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>")
            )
        }
        return data
    }

    /// `DELETE /backup` — drop this session's backup blob.
    /// Idempotent — server returns 204 regardless of prior state.
    public func deleteBackup() async throws {
        let endpoint = "/backup"
        var request = try buildRequest(endpoint: endpoint, method: "DELETE")
        attachAuth(to: &request)

        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        } catch {
            throw StorageClientError.network(endpoint: endpoint, underlying: error)
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw StorageClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        if (200..<300).contains(httpResponse.statusCode) {
            return
        }
        throw StorageClientError.http(
            status: httpResponse.statusCode,
            endpoint: endpoint,
            body: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>")
        )
    }

    // MARK: - Internal HTTP

    private func buildRequest(
        endpoint: String,
        method: String
    ) throws -> URLRequest {
        guard let url = URL(string: baseURL + endpoint) else {
            throw StorageClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        return request
    }

    private func attachAuth(to request: inout URLRequest) {
        if let token = sessionToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
    }

    // MARK: - Helpers

    /// Lowercase-hex encode the 32-byte blob_id, matching the URL
    /// form `services/storage/src/blob.rs::validate_hex` accepts.
    public static func blobIdHex(_ bytes: Data) -> String {
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

    private static func truncate(_ s: String, max: Int = 512) -> String {
        if s.count > max {
            return String(s.prefix(max)) + "…"
        }
        return s
    }
}

// MARK: - Errors

public enum StorageClientError: Error {
    /// Local input validation (wrong length, oversize, malformed
    /// base URL). Caught before any network round-trip.
    case argument(String)
    /// Server returned a non-2xx, non-404 status.
    case http(status: Int, endpoint: String, body: String)
    /// DNS / TLS / transport failure.
    case network(endpoint: String, underlying: Error)
    /// ADR-0065 / UX 4.2 — the sending (`.upload`) or opening (`.read`)
    /// allowance is used up for now.
    case limited(BlobPassKind)
}

extension StorageClientError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let m): return m
        case .http(let s, let e, let b): return "storage \(e) → \(s): \(b)"
        case .network(let e, let u): return "storage \(e) network error: \(u)"
        case .limited(let kind): return "storage \(kind.rawValue) allowance used up"
        }
    }
}

/// ADR-0047 — forwards `didSendBodyData` to a fraction-sent callback so a
/// media upload can drive a determinate ring. Per-task, so concurrent uploads
/// never share a counter.
private final class UploadProgressDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    private let onProgress: @Sendable (Double) -> Void

    init(onProgress: @escaping @Sendable (Double) -> Void) {
        self.onProgress = onProgress
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didSendBodyData bytesSent: Int64,
        totalBytesSent: Int64,
        totalBytesExpectedToSend: Int64
    ) {
        guard totalBytesExpectedToSend > 0 else { return }
        onProgress(min(1, max(0, Double(totalBytesSent) / Double(totalBytesExpectedToSend))))
    }
}
