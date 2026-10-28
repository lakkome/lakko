// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0065 section 5 — the blob routes are gated by an hour's pass bought
// with one anonymous token, one pass per direction per device. Section 6 —
// every upload carries a delete public key derived from the account seed.

/// Which pass a blob request needs.
public enum BlobPassKind: String, Sendable, CaseIterable {
    /// `PUT /blob/{hex}`.
    case upload
    /// `GET` / `HEAD /blob/{hex}` and `POST /blob/keep`.
    case read

    var tokenContext: AnonTokenContext { self == .upload ? .blobUpload : .blobRead }
    var endpoint: String { "/blob/pass/\(rawValue)" }
}

/// A pass the storage service issued.
public struct BlobPass: Equatable, Sendable {
    public let value: String
    public let kind: BlobPassKind
    public let expiresAtUnixSecs: Int64
}

/// The passes this device holds, in memory (pure).
struct BlobPassCache: Equatable {
    /// A pass is taken again this long before it expires, so no request
    /// leaves with a pass that dies on the way.
    static let refreshMarginSecs: Int64 = 60

    private(set) var passes: [BlobPassKind: BlobPass] = [:]

    func usable(_ kind: BlobPassKind, nowSecs: Int64) -> BlobPass? {
        guard let pass = passes[kind], pass.expiresAtUnixSecs - Self.refreshMarginSecs > nowSecs else { return nil }
        return pass
    }

    mutating func store(_ pass: BlobPass) { passes[pass.kind] = pass }

    /// Drop `value` (refused by the server): the next request takes a new
    /// pass. A pass taken meanwhile by another request is kept.
    mutating func drop(_ kind: BlobPassKind, value: String) {
        if passes[kind]?.value == value { passes[kind] = nil }
    }

    mutating func clear() { passes = [:] }
}

/// What a blob route's answer means (pure, from the status and the
/// `{"error": code}` body).
enum BlobAnswer: Equatable {
    case ok
    /// 404 `blob-not-found`: the file is gone (UX 4.1, no retry).
    case gone
    /// The pass is not good any more (an expired or unknown pass, a budget
    /// used up): take a new pass once and send again.
    case renewPass
    /// Anything else: the generic failure.
    case failed(status: Int, code: String?)

    static let renewCodes: Set<String> = [
        "blob-pass-required", "blob-pass-invalid", "blob-pass-expired",
        "blob-upload-budget-exhausted", "blob-read-budget-exhausted",
    ]

    static func classify(status: Int, body: Data) -> BlobAnswer {
        if (200..<300).contains(status) { return .ok }
        let code = AnonTokenRoute.errorCode(in: body)
        if status == 404 { return .gone }
        if let code, renewCodes.contains(code), status == 401 || status == 429 { return .renewPass }
        return .failed(status: status, code: code)
    }

    /// A second refusal after a new pass: a used-up budget is the limit
    /// state (UX 4.2); an HTML 429 from nginx (no JSON code) is generic.
    static func isLimit(status: Int, code: String?) -> Bool {
        status == 429 && (code == "blob-upload-budget-exhausted" || code == "blob-read-budget-exhausted")
    }
}

/// The per-device holder of the two passes and the delete-key source.
public actor BlobAccess {
    public static let shared = BlobAccess()

    private var cache = BlobPassCache()
    private var inflight: [BlobPassKind: Task<BlobPass, Error>] = [:]
    private let tokens: AnonTokenSource
    private let session: URLSession
    private let baseURL: () -> String
    private let now: @Sendable () -> Int64

    /// The seed the blob delete keys are derived from (64 bytes): the
    /// account's LINEAGE seed, so a file uploaded before a key rotation
    /// (ADR-0074) can still be deleted after it. Set by the app shell;
    /// `nil` while signed out.
    nonisolated(unsafe) public static var deleteKeySeed: @MainActor () -> Data? = { nil }

    public init(
        tokens: AnonTokenSource = AnonTokenPool.shared,
        session: URLSession = AnonymousHTTP.session,
        baseURL: @escaping () -> String = { StorageClient.defaultBaseURL },
        now: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970) }
    ) {
        self.tokens = tokens
        self.session = session
        self.baseURL = baseURL
        self.now = now
    }

    /// A pass of `kind` good for at least a minute: the cached one, or one
    /// bought now with a token. Concurrent callers share one purchase.
    public func pass(_ kind: BlobPassKind) async throws -> BlobPass {
        if let held = cache.usable(kind, nowSecs: now()) { return held }
        if let running = inflight[kind] { return try await running.value }
        let task = Task { try await self.buy(kind) }
        inflight[kind] = task
        defer { inflight[kind] = nil }
        let pass = try await task.value
        cache.store(pass)
        return pass
    }

    /// The server refused `value`: forget it.
    public func refused(_ kind: BlobPassKind, value: String) {
        cache.drop(kind, value: value)
    }

    /// Sign-out / forget device.
    public func clear() { cache.clear() }

    private func buy(_ kind: BlobPassKind) async throws -> BlobPass {
        guard let url = URL(string: baseURL() + kind.endpoint) else {
            throw StorageClientError.argument("invalid storage base URL")
        }
        let session = self.session
        let (data, http) = try await AnonTokenRoute.send(context: kind.tokenContext, tokens: tokens) { header in
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue(header, forHTTPHeaderField: AnonTokenRoute.headerName)
            let raw: (Data, URLResponse)
            do {
                raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
            } catch {
                throw StorageClientError.network(endpoint: kind.endpoint, underlying: error)
            }
            guard let http = raw.1 as? HTTPURLResponse else {
                throw StorageClientError.http(status: 0, endpoint: kind.endpoint, body: "non-HTTP response")
            }
            return (raw.0, http)
        }
        guard (200..<300).contains(http.statusCode),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let value = object["pass"] as? String, !value.isEmpty,
              let expires = (object["expires_at_unix_secs"] as? NSNumber)?.int64Value else {
            throw StorageClientError.http(
                status: http.statusCode, endpoint: kind.endpoint,
                body: AnonTokenRoute.errorCode(in: data) ?? "pass refused")
        }
        return BlobPass(value: value, kind: kind, expiresAtUnixSecs: expires)
    }
}

/// ADR-0065 section 6 — the delete key of a blob and the signed delete,
/// from the account's vault keys.
enum BlobDeleteKeys {
    @MainActor private static var cached: (seed: Data, keys: VaultKeys)?

    /// The seed blob delete keys come from: the oldest own predecessor after
    /// a proven rotation, else the current account seed (the DeviceGroup's
    /// lineage seed, ``DeviceGroupLifecycle/lineageSeed(accountIdB64:currentSeed:predecessors:)``).
    /// Only the blob delete keys use it; ADR-0064 vault ids and keys do not.
    static func seed(
        accountIdB64: String, currentSeed: Data?,
        predecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore()
    ) -> Data? {
        DeviceGroupLifecycle.lineageSeed(
            accountIdB64: accountIdB64, currentSeed: currentSeed, predecessors: predecessors)
    }

    /// The delete public key of `blobId` under `seed` (tests compare these
    /// across a rotation).
    static func publicKey(seed: Data, blobId: Data) -> Data? {
        guard let identity = try? Identity.fromSeed(seed: seed) else { return nil }
        return try? LakkoCore.vaultKeys(accountIdentity: identity).blobDeletePublicKey(blobId: blobId)
    }

    @MainActor
    static func vaultKeys() -> VaultKeys? {
        guard let seed = BlobAccess.deleteKeySeed() else { return nil }
        if let cached, cached.seed == seed { return cached.keys }
        guard let identity = try? Identity.fromSeed(seed: seed) else { return nil }
        let keys = LakkoCore.vaultKeys(accountIdentity: identity)
        cached = (seed, keys)
        return keys
    }

    /// The `x-lakko-blob-delete-key` value for `blobId`, or `nil` without a
    /// signed-in account.
    @MainActor
    static func headerValue(blobId: Data) -> String? {
        guard let key = try? vaultKeys()?.blobDeletePublicKey(blobId: blobId) else { return nil }
        return base64UrlEncodeNoPad(key)
    }

    @MainActor
    static func proof(blobId: Data, path: String) -> String? {
        try? vaultKeys()?.signBlobDelete(blobId: blobId, method: "DELETE", path: path, body: Data())
    }

    static let deleteKeyHeader = "x-lakko-blob-delete-key"
    static let passHeader = "x-lakko-blob-pass"
    static var proofHeader: String { storageProofHeaderName() }
}
