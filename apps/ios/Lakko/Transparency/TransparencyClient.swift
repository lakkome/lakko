// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The three read endpoints ``TransparencyVerify`` consumes, abstracted
/// behind a protocol so a test can substitute a fake without a network
/// (the web analog is the `VerifyDeps.client` seam — there the concrete
/// `TransparencyClient` is duck-typed; Swift needs an explicit
/// protocol). ``TransparencyClient`` is the production conformer.
public protocol TransparencyFetching {
    /// `GET /transparency/root`.
    func fetchRoot() async throws -> SignedRoot
    /// `GET /transparency/latest/:account_key`; `nil` on 404.
    func fetchLatest(usernameHash: Data) async throws -> LatestEntry?
    /// `GET /transparency/proof/:idx`.
    func fetchProof(idx: UInt64) async throws -> InclusionProof
    /// `GET /transparency/announcement/:account_key/:generation` (ADR-0101
    /// section 10): the newest seal-announcement leaf of that generation;
    /// `nil` on 404.
    func fetchAnnouncement(usernameHash: Data, generation: UInt64) async throws -> AnnouncementLeaf?
}

public extension TransparencyFetching {
    /// A log that predates announcement leaves (a test fake) serves none.
    func fetchAnnouncement(usernameHash: Data, generation: UInt64) async throws -> AnnouncementLeaf? { nil }
}

/// One seal-announcement leaf of the key log (ADR-0101 section 10).
public struct AnnouncementLeaf: Equatable {
    public let idx: UInt64
    public let keyGeneration: UInt64
    public let seq: UInt64
    /// The announcement itself (the leaf commits it by hash).
    public let announcement: Data

    public init(idx: UInt64, keyGeneration: UInt64, seq: UInt64, announcement: Data) {
        self.idx = idx
        self.keyGeneration = keyGeneration
        self.seq = seq
        self.announcement = announcement
    }
}

/// URLSession-based client for `lakko-svc-transparency` (KT-log S3d).
///
/// Mirror of `apps/web/src/transparency/client.ts` (S3c).
///
/// Three anonymous GET endpoints back the inclusion check:
///
///   * `GET /transparency/root` → the operator-signed Merkle root
///     (`{ tree_root_b64, log_size, timestamp_unix, signature_b64 }`).
///   * `GET /transparency/latest/:account_key_b64url` → the latest
///     entry for an account-key (`{ idx, version, identity_pubkey_b64,
///     is_reset }`) or 404 when the account is not yet logged.
///   * `GET /transparency/proof/:idx` → an inclusion proof
///     (`{ idx, leaf_b64, audit_path: [b64...], log_size }`).
///
/// The client decodes the base64 fields to raw `Data` so the verify
/// layer hands them straight into the native `verifyLogInclusion`. It
/// is transport-only: it never verifies signatures (that is
/// ``TransparencyVerify``'s job, against the build-time pinned operator
/// key) and never consults the `/transparency/operator-key` endpoint
/// for trust (the operator key is pinned out-of-band via
/// ``TransparencyConfig/pinnedOperatorKey``).
///
/// ``forOptionalConfig()`` returns `nil` when the transparency
/// integration is OFF (URL or operator-key pin unset) so the caller can
/// skip the whole check; see ``TransparencyConfig`` for the contract.
public final class TransparencyClient: TransparencyFetching {

    public let baseURL: String

    private let session: URLSession
    private let decoder: JSONDecoder

    public init(baseURL: String, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
        self.decoder = JSONDecoder()
    }

    /// Build a client from the build-time config, or return `nil` when
    /// the transparency integration is OFF (URL or operator-key pin
    /// unset). The caller skips the whole log check on `nil`.
    public static func forOptionalConfig() -> TransparencyClient? {
        guard let base = TransparencyConfig.baseUrl,
              TransparencyConfig.pinnedOperatorKey != nil else {
            return nil
        }
        return TransparencyClient(baseURL: base)
    }

    // MARK: - Endpoints

    /// `GET /transparency/root`.
    public func fetchRoot() async throws -> SignedRoot {
        let wire: RootResWire = try await getJson(endpoint: "/transparency/root")
        guard let treeRoot = base64Decode(wire.treeRootB64),
              let signature = base64Decode(wire.signatureB64) else {
            throw TransparencyClientError.http(
                status: 200,
                endpoint: "/transparency/root",
                detail: "tree_root_b64 / signature_b64 not valid base64"
            )
        }
        // ADR-0039: the witness co-signatures are a LIST on the wire (one
        // per co-signing witness), OMITTED when the operator runs without a
        // witness. Each entry is decoded independently; a blank / undecodable
        // entry is dropped (the K-of-N quorum gate ignores it — and refuses
        // the whole root when fewer than `threshold` distinct pinned witnesses
        // remain). Replaces the pre-ADR-0039 singular `witness_cosig_b64`.
        let witnessCosigs: [Data] = (wire.witnessCosigsB64 ?? []).compactMap { b64 in
            b64.isEmpty ? nil : base64Decode(b64)
        }
        return SignedRoot(
            treeRoot: treeRoot,
            logSize: wire.logSize,
            timestampUnix: wire.timestampUnix,
            signature: signature,
            witnessCosigs: witnessCosigs
        )
    }

    /// `GET /transparency/latest/:account_key`. The 32-byte account-key
    /// is encoded URL-safe-no-pad for the path segment (the server
    /// accepts either alphabet). Returns `nil` on 404 — the account is
    /// not yet in the log.
    public func fetchLatest(usernameHash: Data) async throws -> LatestEntry? {
        let slug = base64UrlEncodeNoPad(usernameHash)
        let endpoint = "/transparency/latest/\(slug)"
        guard let wire: LatestResWire = try await getJsonOr404(endpoint: endpoint) else {
            return nil
        }
        guard let identityPubkey = base64Decode(wire.identityPubkeyB64) else {
            throw TransparencyClientError.http(
                status: 200,
                endpoint: endpoint,
                detail: "identity_pubkey_b64 not valid base64"
            )
        }
        return LatestEntry(
            idx: wire.idx,
            version: wire.version,
            identityPubkey: identityPubkey,
            isReset: wire.isReset,
            leafAuth: wire.leafAuthB64.flatMap { base64Decode($0) },
            genesisAttest: wire.genesisAttestB64.flatMap { base64Decode($0) }
        )
    }

    /// `GET /transparency/announcement/:account_key/:generation`; `nil` on
    /// 404 (no announcement of that generation is logged).
    public func fetchAnnouncement(usernameHash: Data, generation: UInt64) async throws -> AnnouncementLeaf? {
        let endpoint = "/transparency/announcement/\(base64UrlEncodeNoPad(usernameHash))/\(generation)"
        guard let wire: AnnouncementResWire = try await getJsonOr404(endpoint: endpoint) else { return nil }
        guard let announcement = base64Decode(wire.announcementB64) else {
            throw TransparencyClientError.http(
                status: 200, endpoint: "/transparency/announcement", detail: "announcement_b64 not valid base64")
        }
        return AnnouncementLeaf(
            idx: wire.idx, keyGeneration: wire.keyGeneration, seq: wire.seq, announcement: announcement)
    }

    /// `GET /transparency/proof/:idx`.
    public func fetchProof(idx: UInt64) async throws -> InclusionProof {
        let endpoint = "/transparency/proof/\(idx)"
        let wire: ProofResWire = try await getJson(endpoint: endpoint)
        guard let leaf = base64Decode(wire.leafB64) else {
            throw TransparencyClientError.http(
                status: 200,
                endpoint: endpoint,
                detail: "leaf_b64 not valid base64"
            )
        }
        // Flatten the audit path: each entry is a base64 32-byte
        // sibling hash; concatenate them so the native verifier reads
        // a single `Data` whose length is a multiple of 32. (Mirror of
        // the web client's flatten step.)
        var flat = Data()
        for hashB64 in wire.auditPath {
            guard let h = base64Decode(hashB64) else {
                throw TransparencyClientError.http(
                    status: 200,
                    endpoint: endpoint,
                    detail: "audit_path entry not valid base64"
                )
            }
            flat.append(h)
        }
        return InclusionProof(
            idx: wire.idx,
            leaf: leaf,
            auditPath: flat,
            logSize: wire.logSize
        )
    }

    // MARK: - Internal HTTP helpers

    /// GET + decode, treating any non-2xx (incl. the well-known 404) as
    /// an error. Used by `/root` + `/proof`, which have no documented
    /// 404 path.
    private func getJson<Res: Decodable>(endpoint: String) async throws -> Res {
        guard let result: Res = try await getJsonOr404(endpoint: endpoint) else {
            throw TransparencyClientError.http(
                status: 404,
                endpoint: endpoint,
                detail: "unexpected 404"
            )
        }
        return result
    }

    /// GET + decode, folding the well-known 404 into `nil`. Any other
    /// non-2xx (or a JSON-parse failure) throws
    /// ``TransparencyClientError/http(status:endpoint:detail:)``; a
    /// transport failure throws
    /// ``TransparencyClientError/network(endpoint:underlying:)`` so the
    /// verify layer can map both to `"unreachable"`.
    private func getJsonOr404<Res: Decodable>(endpoint: String) async throws -> Res? {
        guard let url = URL(string: baseURL + endpoint) else {
            throw TransparencyClientError.http(
                status: 0,
                endpoint: endpoint,
                detail: "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"

        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        } catch {
            throw TransparencyClientError.network(endpoint: endpoint, underlying: error)
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw TransparencyClientError.http(
                status: 0,
                endpoint: endpoint,
                detail: "non-HTTP response"
            )
        }
        if httpResponse.statusCode == 404 {
            return nil
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            throw TransparencyClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                detail: Self.truncate(String(data: data, encoding: .utf8) ?? "<binary>")
            )
        }
        do {
            return try decoder.decode(Res.self, from: data)
        } catch {
            throw TransparencyClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                detail: "body is not valid JSON: \(error)"
            )
        }
    }

    private static func truncate(_ s: String, max: Int = 256) -> String {
        s.count > max ? String(s.prefix(max)) + "…" : s
    }
}

// MARK: - Decoded response types (bytes, not base64)

/// Decoded `GET /transparency/root` response.
public struct SignedRoot: Equatable {
    /// 32-byte Merkle root over the first `logSize` entries.
    public let treeRoot: Data
    /// Number of entries the root commits to.
    public let logSize: UInt64
    /// Operator-chosen timestamp (seconds since UNIX epoch).
    public let timestampUnix: UInt64
    /// 64-byte Ed25519 signature over the canonical root pre-image.
    public let signature: Data
    /// ADR-0039 (G-30 finding-1): the witness co-signature wire blobs over
    /// `(treeRoot, logSize)` — a LIST, one per co-signing witness (empty `[]`
    /// when the server serves none). A client that pins a witness key SET
    /// requires a K-of-N quorum of valid ones (the split-view defence).
    /// Replaces the pre-ADR-0039 singular `witnessCosig`.
    public let witnessCosigs: [Data]

    public init(
        treeRoot: Data,
        logSize: UInt64,
        timestampUnix: UInt64,
        signature: Data,
        witnessCosigs: [Data] = []
    ) {
        self.treeRoot = treeRoot
        self.logSize = logSize
        self.timestampUnix = timestampUnix
        self.signature = signature
        self.witnessCosigs = witnessCosigs
    }
}

/// Decoded `GET /transparency/latest/:account_key` response.
public struct LatestEntry: Equatable {
    /// 0-based leaf index of the latest entry (== the `/proof` idx).
    public let idx: UInt64
    /// Server-assigned monotonic per-account-key version (>= 1).
    public let version: UInt64
    /// The account's current identity public key (raw bytes).
    public let identityPubkey: Data
    /// ADR-0017 fresh-start marker — `true` if the latest entry was a
    /// no-supersession-proof owner reset.
    public let isReset: Bool
    /// G-30 #2 (ADR-0019): the entry's opaque leaf authorisation (a 64-byte
    /// genesis self-sig or a 145-byte supersession proof), or `nil` for a
    /// pre-Sprint-2 / unauthenticated row. A client verifies a genesis
    /// self-sig against the served key on a first observation.
    public let leafAuth: Data?
    /// ADR-0036 (G-30 finding 2): the entry's opaque auth-attested genesis
    /// proof (a 153-byte `GenesisAttestation`), or `nil` for a rotation / a
    /// pre-ADR-0036 / un-attested genesis row. When present on a first
    /// observation the client verifies it against the pinned auth attest key
    /// (anti-squatting) BESIDE the self-sig — fail-closed on a present-but-
    /// invalid value.
    public let genesisAttest: Data?

    public init(
        idx: UInt64,
        version: UInt64,
        identityPubkey: Data,
        isReset: Bool,
        leafAuth: Data? = nil,
        genesisAttest: Data? = nil
    ) {
        self.idx = idx
        self.version = version
        self.identityPubkey = identityPubkey
        self.isReset = isReset
        self.leafAuth = leafAuth
        self.genesisAttest = genesisAttest
    }
}

/// Decoded `GET /transparency/proof/:idx` response. The `auditPath` is
/// FLATTENED (each 32-byte sibling hash concatenated leaf→root) so it
/// feeds straight into `verifyLogInclusion`.
public struct InclusionProof: Equatable {
    /// 0-based leaf index of the proven entry.
    public let idx: UInt64
    /// The entry's canonical leaf bytes (informational; the verify path
    /// binds the decomposed `(account_key, pubkey, version)` instead).
    public let leaf: Data
    /// Flattened audit path: sibling node-hashes concatenated
    /// leaf→root, length always a multiple of 32.
    public let auditPath: Data
    /// Tree size the proof was generated against.
    public let logSize: UInt64

    public init(idx: UInt64, leaf: Data, auditPath: Data, logSize: UInt64) {
        self.idx = idx
        self.leaf = leaf
        self.auditPath = auditPath
        self.logSize = logSize
    }
}

// MARK: - Wire shapes (snake_case JSON ↔ camelCase Swift)

private struct RootResWire: Decodable {
    let treeRootB64: String
    let logSize: UInt64
    let timestampUnix: UInt64
    let signatureB64: String
    /// ADR-0039: base64 witness co-signatures, a JSON array — absent when the
    /// server has no cosigs. Replaces the pre-ADR-0039 singular
    /// `witness_cosig_b64`.
    let witnessCosigsB64: [String]?

    enum CodingKeys: String, CodingKey {
        case treeRootB64 = "tree_root_b64"
        case logSize = "log_size"
        case timestampUnix = "timestamp_unix"
        case signatureB64 = "signature_b64"
        case witnessCosigsB64 = "witness_cosigs_b64"
    }
}

private struct LatestResWire: Decodable {
    let idx: UInt64
    let version: UInt64
    let identityPubkeyB64: String
    let isReset: Bool
    /// G-30 #2 (ADR-0019): base64 of the entry's leaf authorisation, or
    /// absent for a pre-Sprint-2 / unauthenticated row.
    let leafAuthB64: String?
    /// ADR-0036 (G-30 finding 2): base64 of the entry's auth-attested genesis
    /// proof, or absent for a rotation / pre-ADR-0036 / un-attested row.
    let genesisAttestB64: String?

    enum CodingKeys: String, CodingKey {
        case idx
        case version
        case identityPubkeyB64 = "identity_pubkey_b64"
        case isReset = "is_reset"
        case leafAuthB64 = "leaf_auth_b64"
        case genesisAttestB64 = "genesis_attest_b64"
    }
}

private struct AnnouncementResWire: Decodable {
    let idx: UInt64
    let keyGeneration: UInt64
    let seq: UInt64
    let announcementB64: String

    enum CodingKeys: String, CodingKey {
        case idx, seq
        case keyGeneration = "key_generation"
        case announcementB64 = "announcement_b64"
    }
}

private struct ProofResWire: Decodable {
    let idx: UInt64
    let leafB64: String
    let auditPath: [String]
    let logSize: UInt64

    enum CodingKeys: String, CodingKey {
        case idx
        case leafB64 = "leaf_b64"
        case auditPath = "audit_path"
        case logSize = "log_size"
    }
}

// MARK: - Errors

/// Error vocabulary for ``TransparencyClient``. ``TransparencyVerify``
/// maps both variants to `"unreachable"` (a best-effort, fail-open
/// verdict — the supersession/pin gate is the primary trust).
public enum TransparencyClientError: Error {
    /// Network-level failure (DNS, TLS, no route, …). Distinct from a
    /// parse failure so the verify layer can map both to
    /// `"unreachable"`.
    case network(endpoint: String, underlying: Error)
    /// Server returned a non-2xx (and not the well-known 404) status,
    /// or a body that did not decode to the expected shape.
    case http(status: Int, endpoint: String, detail: String)
}

extension TransparencyClientError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .network(let endpoint, let underlying):
            return "transparency \(endpoint) network error: \(underlying)"
        case .http(let status, let endpoint, let detail):
            return "transparency \(endpoint) → \(status): \(detail)"
        }
    }
}
