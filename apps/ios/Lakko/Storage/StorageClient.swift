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
/// Content-addressable blob proxy with three verbs:
///
///   * `PUT  /blob/<sha256_hex>` — body = ciphertext. Idempotent:
///     the server verifies `SHA-256(body) == <sha256_hex>` and
///     stores the bytes at that key. 201 on first upload, 200 on
///     re-upload, 400 on hash mismatch.
///   * `GET  /blob/<sha256_hex>` — body = ciphertext, streamed.
///     200 on hit, 404 on miss.
///   * `HEAD /blob/<sha256_hex>` — same shape, headers only. The
///     `Content-Length` header sizes the download.
///
/// ## Auth posture
///
/// Phase-1 storage is unauthenticated — a "public CDN for E2EE
/// blobs". The server-side hash verification on PUT prevents
/// poisoning existing blobs (SHA-256 collisions are infeasible),
/// and nginx-layer rate limits curb churn. Owner tracking + quota
/// land in Phase 2; the client signature stays the same.
///
/// ## Hash boundary
///
/// The `blob_id` parameter is **always** the raw 32-byte SHA-256
/// of the *ciphertext* — exactly what `EncryptedAttachment.blobId`
/// returns from the UniFFI surface (`lakko-attachments` 1.19a +
/// FFI exposure 1.19b). Callers do not compute the hash
/// themselves; they hand the bytes from the encryption layer
/// straight through.
///
/// ## Default base URL
///
/// `http://localhost:8083` — iOS simulator loopback + port `8083`
/// matching `lakko-svc-storage`'s dev bind. Override for physical
/// devices (LAN IP) and production (Cloudflare tunnel URL).
public final class StorageClient {

    public static let defaultBaseURL: String = "http://localhost:8083"

    /// Raw SHA-256 blob id length.
    public static let blobIdLength: Int = 32

    /// Per-blob ceiling. Mirrors `MAX_BLOB_BYTES` in
    /// `services/storage/src/main.rs` and `MAX_ATTACHMENT_BYTES`
    /// in `lakko-attachments` — surface here so a misbehaving
    /// caller fails fast without spending an oversize-body
    /// round-trip.
    public static let maxBlobBytes: Int = 50 * 1024 * 1024

    public let baseURL: String
    private let session: URLSession
    /// Bearer token bound to the per-account `/backup` routes
    /// (Phase 2a1c). `nil` is fine for the content-addressed
    /// `/blob/...` routes — they're auth-free; a `nil` value on
    /// a backup-route call surfaces as a 401 from the server.
    private var sessionToken: String?

    public init(
        baseURL: String = StorageClient.defaultBaseURL,
        session: URLSession = .shared
    ) {
        self.baseURL = baseURL
        self.session = session
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

    // MARK: - PUT

    /// `PUT /blob/<hex>` — upload ciphertext. Idempotent: re-PUT
    /// of the same hash overwrites with byte-identical content
    /// (which is a no-op by the meaning of content addressing).
    ///
    /// The server hashes the body and compares against the URL
    /// slug; a mismatch surfaces as
    /// `StorageClientError.http(status: 400, …)`. This lets the
    /// caller detect a buggy local encryption path before any
    /// recipient sees a broken blob.
    ///
    /// - Parameters:
    ///   - blobId: 32-byte SHA-256 of the ciphertext (the
    ///     `blob_id` returned by `EncryptedAttachment.blobId`).
    ///   - ciphertext: The bytes to store. Must hash to `blobId`.
    public func putBlob(blobId: Data, ciphertext: Data) async throws {
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

        let hex = Self.blobIdHex(blobId)
        let endpoint = "/blob/\(hex)"
        var request = try buildRequest(endpoint: endpoint, method: "PUT")
        request.setValue(
            "application/octet-stream",
            forHTTPHeaderField: "Content-Type"
        )

        let raw: (Data, URLResponse)
        do {
            raw = try await session.upload(for: request, from: ciphertext)
        } catch {
            throw StorageClientError.network(
                endpoint: endpoint,
                underlying: error
            )
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw StorageClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        // 200 OK = idempotent re-upload, 201 Created = first
        // upload. Both are success.
        if httpResponse.statusCode == 200 || httpResponse.statusCode == 201 {
            return
        }
        let bodyText = Self.truncate(
            String(data: data, encoding: .utf8) ?? "<binary>"
        )
        throw StorageClientError.http(
            status: httpResponse.statusCode,
            endpoint: endpoint,
            body: bodyText
        )
    }

    // MARK: - GET

    /// `GET /blob/<hex>` — fetch ciphertext. Returns the raw
    /// bytes on 200, `nil` on 404 (typed not-found path so the UI
    /// can surface "attachment missing" without an exception).
    /// Throws on any other non-2xx.
    public func getBlob(blobId: Data) async throws -> Data? {
        guard blobId.count == Self.blobIdLength else {
            throw StorageClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes (got \(blobId.count))"
            )
        }
        let hex = Self.blobIdHex(blobId)
        let endpoint = "/blob/\(hex)"
        let request = try buildRequest(endpoint: endpoint, method: "GET")

        let raw: (Data, URLResponse)
        do {
            raw = try await session.data(for: request)
        } catch {
            throw StorageClientError.network(
                endpoint: endpoint,
                underlying: error
            )
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
            let bodyText = Self.truncate(
                String(data: data, encoding: .utf8) ?? "<binary>"
            )
            throw StorageClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: bodyText
            )
        }
        return data
    }

    // MARK: - HEAD

    /// `HEAD /blob/<hex>` — return the blob's `Content-Length` if
    /// it exists, `nil` if not. Used by the UI to size a download
    /// indicator before issuing the GET.
    ///
    /// Throws on any non-200 / non-404 — a 5xx here means the
    /// storage service is sick, not that the blob is missing.
    public func headBlob(blobId: Data) async throws -> Int? {
        guard blobId.count == Self.blobIdLength else {
            throw StorageClientError.argument(
                "blobId must be \(Self.blobIdLength) bytes (got \(blobId.count))"
            )
        }
        let hex = Self.blobIdHex(blobId)
        let endpoint = "/blob/\(hex)"
        let request = try buildRequest(endpoint: endpoint, method: "HEAD")

        let raw: (Data, URLResponse)
        do {
            raw = try await session.data(for: request)
        } catch {
            throw StorageClientError.network(
                endpoint: endpoint,
                underlying: error
            )
        }
        let (_, response) = raw
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
                body: "HEAD \(endpoint) returned \(httpResponse.statusCode)"
            )
        }
        let cl = httpResponse.value(forHTTPHeaderField: "Content-Length")
        return cl.flatMap(Int.init)
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
            raw = try await session.data(for: request)
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
            raw = try await session.data(for: request)
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
}

extension StorageClientError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let m): return m
        case .http(let s, let e, let b): return "storage \(e) → \(s): \(b)"
        case .network(let e, let u): return "storage \(e) network error: \(u)"
        }
    }
}
