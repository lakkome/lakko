// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The verdict of a log-inclusion check for one fetched identity
/// (KT-log S3d). Mirror of `apps/web/src/transparency/verify.ts`'s
/// `LogVerifyStatus`.
public enum LogVerifyStatus: String, Equatable {
    /// Root is operator-signed (pinned key), no rollback, and the
    /// served pubkey is proven included at the latest version.
    case confirmed
    /// `/latest` 404s: the account is not yet in the log. Benign under
    /// S2.5 eventual consistency OR an off-log equivocation — surfaced,
    /// not auto-trusted.
    case absent
    /// The signed root's `log_size` is SMALLER than the highest we have
    /// seen. An append-only log cannot shrink → an unambiguous attack.
    /// HARD.
    case rollback
    /// The log's latest entry pubkey differs from what discovery
    /// served, OR the inclusion proof failed to verify. Suspicious.
    case mismatch
    /// Any network error fetching root/latest/proof, or a non-404 HTTP
    /// / malformed-body fault.
    case unreachable
    /// The integration is unconfigured (no URL or no pinned operator
    /// key). The whole check is skipped.
    case off
}

/// The outcome of ``TransparencyVerify/verifyIdentityInLog(accountId:publishedSignaturePubkey:)``
/// (KT-log S4). Mirror of the web `LogVerifyResult`.
///
/// Adds the `is_reset` marker the log serves on its `/latest` entry to
/// the bare status. ``isReset`` lets a contact DISTINGUISH an
/// owner-initiated identity RESET (the lost-everything fresh-identity
/// mint — logged with `is_reset=true`, ADR-0017) from an unexplained key
/// change (a possible compromise — `is_reset` false/absent).
///
/// ``isReset`` is the `/latest` entry's marker WHENEVER a latest entry
/// was observed — i.e. on `.confirmed` AND on `.mismatch` (both imply a
/// latest exists). It is `nil` when there was no latest entry to read it
/// from: `.absent` (account not yet logged), `.rollback` (short-circuited
/// before the latest lookup), `.unreachable` (network/log fault), and
/// `.off` (the integration is unconfigured).
public struct LogVerifyResult: Equatable {
    /// The S3d state-machine verdict.
    public let status: LogVerifyStatus
    /// The `/latest` entry's `is_reset` marker when a latest entry was
    /// read (`.confirmed` / `.mismatch`); `nil` otherwise.
    public let isReset: Bool?

    public init(status: LogVerifyStatus, isReset: Bool?) {
        self.status = status
        self.isReset = isReset
    }
}

/// The rotation context handed to
/// ``TransparencyVerify/verifyRotationChain(accountId:servedPubkey:ctx:)``
/// (G-30 #2, ADR-0019). Mirror of the web `verifyRotationChain`'s `ctx`
/// argument — the bundle's supersession proof bytes, the locally-pinned
/// OLD pubkey, and the pinned log-version floor.
public struct RotationContext: Equatable {
    /// The bundle's `supersession_proof` bytes (already base64-decoded)
    /// — the chain's OLD-key authorisation.
    public let supersessionProof: Data
    /// The 32-byte AccountIdentity pubkey pinned for this account BEFORE
    /// the rotation re-pinned (the chain's trusted old key).
    public let pinnedOldPubkey: Data
    /// The transparency-log version the pinned key was last
    /// chain-confirmed at (the downgrade floor; `0` when never
    /// chain-confirmed). The chain proof must reference a STRICTLY-NEWER
    /// logged version.
    public let pinnedLogVersion: UInt64

    public init(supersessionProof: Data, pinnedOldPubkey: Data, pinnedLogVersion: UInt64) {
        self.supersessionProof = supersessionProof
        self.pinnedOldPubkey = pinnedOldPubkey
        self.pinnedLogVersion = pinnedLogVersion
    }
}

/// The outcome of
/// ``TransparencyVerify/verifyRotationChain(accountId:servedPubkey:ctx:)``
/// (G-30 #2, ADR-0019). Mirror of the web `RotationChainResult`.
///
/// Extends the ``LogVerifyResult`` shape with the chain-confirmed
/// per-account-key ``version``: the transparency-log version the served
/// (rotated-to) key was proven included at, so the caller can persist it
/// as the next rotation's downgrade floor. ``version`` is non-`nil` ONLY
/// on a `.confirmed` verdict (a latest entry was read AND the chain gate
/// passed); `nil` on every other status (no chain confirmation to anchor
/// a version on).
public struct RotationChainResult: Equatable {
    /// The verdict — same enum as ``LogVerifyResult``.
    public let status: LogVerifyStatus
    /// The `/latest` entry's `is_reset` marker when a latest entry was
    /// read (`.confirmed` / `.mismatch`); `nil` otherwise.
    public let isReset: Bool?
    /// The chain-confirmed log version on `.confirmed`; `nil` otherwise.
    public let version: UInt64?

    public init(status: LogVerifyStatus, isReset: Bool?, version: UInt64?) {
        self.status = status
        self.isReset = isReset
        self.version = version
    }
}

/// Transparency-log inclusion verification (KT-log S3d) — the
/// equivocation-detection / gossip layer on top of the ADR-0016
/// supersession/pin gate.
///
/// Mirror of `apps/web/src/transparency/verify.ts` (S3c).
///
/// ``verifyIdentityInLog(accountId:publishedSignaturePubkey:)`` consults
/// the operator-signed, append-only transparency log to confirm that
/// the identity discovery just served for a contact is the SAME one the
/// public log commits to. A divergence (the log holds a different key,
/// or the log shrank) is surfaced so the caller can act.
///
/// The function is BEST-EFFORT and never throws on a network/log fault:
/// it maps faults to a status. The CALLER decides the posture (see
/// ``ContactsClient/checkTransparencyLog(accountId:accountIdB64:servedPubkey:)``)
/// — for S3d, only `.rollback` is a hard failure.
public struct TransparencyVerify {

    /// The operator key + signed-root + proof bundle handed to the
    /// inclusion-gate seam. Lets tests drive the `confirmed` /
    /// proof-fail `mismatch` branches without minting a real Merkle
    /// proof (the log builder is Rust-internal, not FFI-exposed).
    public struct InclusionArgs {
        public let operatorKey: Data
        public let root: SignedRoot
        public let accountKey: Data
        public let servedPubkey: Data
        public let proof: InclusionProof
        public let version: UInt64
    }

    /// The operator key + signed-root + proof + supersession bundle
    /// handed to the rotation-chain-gate seam (G-30 #2, ADR-0019). Lets
    /// tests drive the `confirmed` / chain-fail `mismatch` branches
    /// without minting a real Merkle proof AND a real supersession proof
    /// (both are Rust-internal, not FFI-exposed).
    public struct ChainArgs {
        public let operatorKey: Data
        public let root: SignedRoot
        public let accountKey: Data
        public let servedPubkey: Data
        public let proof: InclusionProof
        public let version: UInt64
        public let supersessionProof: Data
        public let pinnedOldPubkey: Data
        public let pinnedLogVersion: UInt64
    }

    private let client: TransparencyFetching?
    private let operatorKey: Data?
    private let lastSeenRoot: LastSeenRoot
    /// Inclusion-gate override (TEST SEAM ONLY). When supplied, this
    /// replaces the real `verifyLogInclusion` FFI call: it THROWS to
    /// simulate an inclusion/root-signature failure and RETURNS to
    /// simulate success — exactly the FFI contract. Production callers
    /// pass `nil`; building a valid Merkle proof requires the Rust log
    /// builder (not FFI-exposed), so tests stub this seam to cover the
    /// `confirmed` / proof-fail `mismatch` branches.
    private let verifyInclusion: ((InclusionArgs) throws -> Void)?
    /// Rotation-chain-gate override (TEST SEAM ONLY) — G-30 #2
    /// (ADR-0019). When supplied, this REPLACES the real
    /// `verifySupersessionChain` FFI call in
    /// ``verifyRotationChain(accountId:servedPubkey:ctx:)``: it RETURNS
    /// the authorised new pubkey to simulate a passing chain and THROWS
    /// to simulate a chain-gate failure — exactly the FFI contract.
    /// Production callers pass `nil`; building a real Merkle proof + a
    /// real supersession proof needs the Rust log builder (not
    /// FFI-exposed), so tests stub this seam to cover the `confirmed` /
    /// chain-fail `mismatch` branches.
    private let verifyChain: ((ChainArgs) throws -> Data)?

    /// Build a verifier from the build-time config (production path) or
    /// from injected dependencies (tests).
    ///
    /// - Parameters:
    ///   - client: a pre-built ``TransparencyFetching`` (production:
    ///     ``TransparencyClient``), or `nil` to mean "integration off".
    ///     Defaults to ``TransparencyClient/forOptionalConfig()``.
    ///   - operatorKey: the pinned operator verifying key (32 bytes),
    ///     or `nil` for "off". Defaults to
    ///     ``TransparencyConfig/pinnedOperatorKey``.
    ///   - lastSeenRoot: the no-rollback high-water store. Defaults to
    ///     a `.standard`-backed ``LastSeenRoot``.
    ///   - verifyInclusion: the test-only inclusion seam (see above).
    ///   - verifyChain: the test-only rotation-chain seam (G-30 #2; see
    ///     above).
    public init(
        client: TransparencyFetching? = TransparencyClient.forOptionalConfig(),
        operatorKey: Data? = TransparencyConfig.pinnedOperatorKey,
        lastSeenRoot: LastSeenRoot = LastSeenRoot(),
        verifyInclusion: ((InclusionArgs) throws -> Void)? = nil,
        verifyChain: ((ChainArgs) throws -> Data)? = nil
    ) {
        self.client = client
        self.operatorKey = operatorKey
        self.lastSeenRoot = lastSeenRoot
        self.verifyInclusion = verifyInclusion
        self.verifyChain = verifyChain
    }

    /// Verify that `publishedSignaturePubkey` (the AccountIdentity
    /// Ed25519 key discovery served for `accountId`) is committed by
    /// the operator-signed transparency log.
    ///
    /// - Parameters:
    ///   - accountId: the peer's raw 16-byte account_id.
    ///   - publishedSignaturePubkey: the 32-byte Ed25519 key discovery
    ///     returned for that account.
    /// - Returns: a ``LogVerifyResult`` carrying the ``LogVerifyStatus``
    ///   verdict and the `/latest` entry's `is_reset` marker (KT-log
    ///   S4); the marker is non-`nil` only when a latest entry was read
    ///   (`.confirmed` / `.mismatch`). Never throws.
    public func verifyIdentityInLog(
        accountId: Data,
        publishedSignaturePubkey: Data
    ) async -> LogVerifyResult {
        // off — the integration is unconfigured. Skip entirely. No
        // latest entry was read, so `isReset` is nil.
        guard let client = client, let operatorKey = operatorKey else {
            return LogVerifyResult(status: .off, isReset: nil)
        }

        do {
            // 1. Fetch the operator-signed root and run the no-rollback
            //    gate against our persisted high-water mark BEFORE
            //    trusting it. (We do NOT verify the root signature
            //    first: a shrink is suspicious regardless of who signed
            //    it, and the rollback verdict is the strongest.) We
            //    short-circuit BEFORE the latest lookup, so no `isReset`.
            let root = try await client.fetchRoot()
            if lastSeenRoot.checkAndAdvance(root.logSize) == .rollback {
                return LogVerifyResult(status: .rollback, isReset: nil)
            }

            // 2. Look up the account's latest logged entry. The
            //    account-key derivation is done in the audited Rust
            //    core via `transparencyAccountKey` — NOT a hand-rolled
            //    BLAKE3 — so it cannot drift from the server's
            //    `account_key(...)`.
            let accountKey = transparencyAccountKey(accountId: accountId)
            guard let latest = try await client.fetchLatest(usernameHash: accountKey) else {
                // Not yet logged. Benign under eventual consistency OR
                // an off-log equivocation — surfaced, not auto-trusted.
                // No latest entry, so no `isReset`.
                return LogVerifyResult(status: .absent, isReset: nil)
            }

            // From here a latest entry EXISTS — capture its `is_reset`
            // marker (KT-log S4). It rides out on both the `.mismatch`
            // and `.confirmed` verdicts below; the caller surfaces it to
            // distinguish an owner-initiated reset from an unexplained
            // key change.
            let isReset = latest.isReset

            // 3. The log's latest pubkey must equal what discovery
            //    served. A divergence is the equivocation signal.
            if latest.identityPubkey != publishedSignaturePubkey {
                return LogVerifyResult(status: .mismatch, isReset: isReset)
            }

            // 4. Prove the exact (account_key, pubkey, version) leaf is
            //    included under the operator-signed root. A tampered
            //    proof, forged root, or substituted identity fails here.
            let proof = try await client.fetchProof(idx: latest.idx)
            do {
                let args = InclusionArgs(
                    operatorKey: operatorKey,
                    root: root,
                    accountKey: accountKey,
                    servedPubkey: publishedSignaturePubkey,
                    proof: proof,
                    version: latest.version
                )
                if let seam = verifyInclusion {
                    // Test seam — stubbed inclusion gate (throws to
                    // fail).
                    try seam(args)
                } else {
                    // Production — the audited Rust core does the
                    // root-signature + Merkle-inclusion check.
                    try verifyLogInclusion(
                        operatorPubkey: operatorKey,
                        treeRoot: root.treeRoot,
                        logSize: root.logSize,
                        timestampUnix: root.timestampUnix,
                        rootSignature: root.signature,
                        leafIndex: latest.idx,
                        auditPath: proof.auditPath,
                        entryUsernameHash: accountKey,
                        entryIdentityPubkey: publishedSignaturePubkey,
                        entryVersion: latest.version
                    )
                }
            } catch {
                // The inclusion / root-signature gate failed: the served
                // key is not provably committed by the pinned operator →
                // treat as a mismatch (a log/server divergence from what
                // we were told). A latest entry still existed, so the
                // marker rides out.
                return LogVerifyResult(status: .mismatch, isReset: isReset)
            }
            return LogVerifyResult(status: .confirmed, isReset: isReset)
        } catch {
            // A network fault, a non-404 HTTP fault, or a malformed body
            // — the log is effectively unusable for this check.
            // Fail-open to `.unreachable` (the caller does not block on
            // it); the supersession/pin gate is the primary trust.
            return LogVerifyResult(status: .unreachable, isReset: nil)
        }
    }

    /// G-30 #2 (ADR-0019) — verify that a peer's KEY ROTATION is
    /// cryptographically anchored to the operator-signed transparency
    /// log.
    ///
    /// Where ``verifyIdentityInLog(accountId:publishedSignaturePubkey:)``
    /// proves a SINGLE served key is in the log, this proves the
    /// TRANSITION old→new: the served (rotated-to) key is the log's
    /// current head for the account AND a supersession chain — root
    /// signature, no-rollback, Merkle inclusion, the embedded
    /// supersession proof from the OLD pinned key, a strictly-newer
    /// logged version than the pinned floor, and freshness — all hold.
    /// The heavy lifting is one `verifySupersessionChain` FFI call (which
    /// THROWS on any chain-gate failure and returns the authorised new
    /// pubkey on success).
    ///
    /// Called by
    /// ``ContactsClient/checkTransparencyLog(accountId:accountIdB64:servedPubkey:oldPin:supersessionProofB64:)``
    /// ONLY on the rotation branch (a pin exists, the served key differs
    /// from it, and the bundle carried a supersession proof). It is
    /// BEST-EFFORT with the SAME posture contract as
    /// ``verifyIdentityInLog(accountId:publishedSignaturePubkey:)``: it
    /// never throws — every fault maps to a status, and the caller (not
    /// this function) decides the posture. Only `.rollback` is surfaced
    /// as a hard verdict; a chain contradiction is surfaced as
    /// `.mismatch` (an equivocation signal), NOT a block.
    ///
    /// Status mapping (mirror of the web `verifyRotationChain`):
    ///   * `.off`         — the integration is unconfigured (no client /
    ///                      no pinned operator key). `version` nil.
    ///   * `.rollback`    — the signed root's `log_size` shrank below our
    ///                      high-water mark. `version` nil.
    ///   * `.absent`      — `/latest` 404s (the rotated key is not yet
    ///                      logged), OR the log's latest pubkey is NOT the
    ///                      served key (a not-yet-logged rotation or a
    ///                      third key — we cannot chain-prove the served
    ///                      key, so we do NOT block; the standalone gate-1
    ///                      result stands). `version` nil.
    ///   * `.confirmed`   — the chain gate passed. `version` is the
    ///                      latest entry's logged version.
    ///   * `.mismatch`    — the chain gate THREW (the log contradicts the
    ///                      proof — an equivocation signal). `version`
    ///                      nil.
    ///   * `.unreachable` — a network/log fault. `version` nil.
    ///
    /// - Parameters:
    ///   - accountId: the peer's raw 16-byte account_id.
    ///   - servedPubkey: the 32-byte Ed25519 key discovery served (the
    ///     rotated-TO key).
    ///   - ctx: the rotation context — the bundle's supersession proof
    ///     bytes, the locally-pinned OLD pubkey, and the pinned
    ///     log-version floor.
    /// - Returns: a ``RotationChainResult``. Never throws.
    public func verifyRotationChain(
        accountId: Data,
        servedPubkey: Data,
        ctx: RotationContext
    ) async -> RotationChainResult {
        // off — the integration is unconfigured. Skip entirely.
        guard let client = client, let operatorKey = operatorKey else {
            return RotationChainResult(status: .off, isReset: nil, version: nil)
        }

        do {
            // 1. Fetch the operator-signed root and run the no-rollback
            //    gate against our high-water mark BEFORE trusting it.
            let root = try await client.fetchRoot()
            if lastSeenRoot.checkAndAdvance(root.logSize) == .rollback {
                return RotationChainResult(status: .rollback, isReset: nil, version: nil)
            }

            // 2. Look up the account's latest logged entry. The
            //    account-key derivation runs in the audited Rust core via
            //    `transparencyAccountKey`, so it cannot drift from the
            //    server's `account_key(...)`.
            let accountKey = transparencyAccountKey(accountId: accountId)
            guard let latest = try await client.fetchLatest(usernameHash: accountKey) else {
                // The rotated key is not yet logged. Benign under
                // eventual consistency — we cannot chain-prove it, so we
                // do NOT block.
                return RotationChainResult(status: .absent, isReset: nil, version: nil)
            }

            // 3. The log's latest pubkey must BE the served key for the
            //    chain to prove the served key. If it is a not-yet-logged
            //    rotation or a third key, we cannot chain-prove the served
            //    key here; the standalone gate-1 (supersession/pin) result
            //    stands. Surface `.absent` (no chain confirmation) — do
            //    NOT block on it.
            if latest.identityPubkey != servedPubkey {
                return RotationChainResult(
                    status: .absent, isReset: latest.isReset, version: nil
                )
            }

            // 4. Fetch the inclusion proof and run the full chain gate.
            //    The FFI verifier throws on any sub-gate failure (root
            //    sig / rollback / inclusion / supersession proof /
            //    non-newer version / freshness).
            let proof = try await client.fetchProof(idx: latest.idx)
            do {
                let args = ChainArgs(
                    operatorKey: operatorKey,
                    root: root,
                    accountKey: accountKey,
                    servedPubkey: servedPubkey,
                    proof: proof,
                    version: latest.version,
                    supersessionProof: ctx.supersessionProof,
                    pinnedOldPubkey: ctx.pinnedOldPubkey,
                    pinnedLogVersion: ctx.pinnedLogVersion
                )
                if let seam = verifyChain {
                    // Test seam — stubbed chain gate (throws to fail).
                    _ = try seam(args)
                } else {
                    // Production — the audited Rust core does the
                    // root-signature + Merkle-inclusion + supersession +
                    // no-downgrade + freshness check.
                    _ = try verifySupersessionChain(
                        operatorPubkey: operatorKey,
                        newTreeRoot: root.treeRoot,
                        newLogSize: root.logSize,
                        newTimestampUnix: root.timestampUnix,
                        newRootSignature: root.signature,
                        entryUsernameHash: accountKey,
                        entryIdentityPubkey: servedPubkey,
                        entryVersion: latest.version,
                        inclusionLeafIndex: proof.idx,
                        inclusionAuditPath: proof.auditPath,
                        supersessionProof: ctx.supersessionProof,
                        pinnedOldPubkey: ctx.pinnedOldPubkey,
                        pinnedVersion: ctx.pinnedLogVersion,
                        lastSeenLogSize: lastSeenRoot.loadLastSeenLogSize(),
                        nowUnixSecs: Int64(Date().timeIntervalSince1970)
                    )
                }
            } catch {
                // The chain gate failed: the log CONTRADICTS the
                // supersession proof (an equivocation signal). Surface
                // `.mismatch` — the caller records + warns but does NOT
                // block (S3d posture). A latest entry existed, so the
                // marker rides out.
                return RotationChainResult(
                    status: .mismatch, isReset: latest.isReset, version: nil
                )
            }
            // The chain is proven — anchor the confirmed version for the
            // next rotation's downgrade floor.
            return RotationChainResult(
                status: .confirmed, isReset: latest.isReset, version: latest.version
            )
        } catch {
            // A network fault, a non-404 HTTP fault, or a malformed body
            // — the chain is unusable for this check. Fail-open to
            // `.unreachable` (the caller does not block on it); the
            // standalone supersession/pin gate is primary.
            return RotationChainResult(status: .unreachable, isReset: nil, version: nil)
        }
    }

    /// KT-log S4 — a LIGHTER, best-effort lookup of the `is_reset`
    /// marker for a SERVED key, used to ENRICH a fail-closed
    /// ``SupersessionRefused`` (an unproven changed key — the moment the
    /// reset/compromise distinction matters most).
    ///
    /// Unlike ``verifyIdentityInLog(accountId:publishedSignaturePubkey:)``,
    /// this does NOT run the inclusion proof or the no-rollback gate: it
    /// is a context-only enrichment that can never change a trust
    /// decision (gate 1 throws regardless). It only answers: "for this
    /// account, does the log's latest entry COMMIT TO EXACTLY the served
    /// key, and was that entry an owner reset?"
    ///
    /// Returns:
    ///   * `true`  — the log's latest pubkey matches `servedPubkey` AND
    ///     its `is_reset` marker is set (owner-initiated fresh-start).
    ///   * `false` — the log's latest pubkey matches `servedPubkey` AND
    ///     its `is_reset` marker is NOT set (an unexplained change —
    ///     possible compromise).
    ///   * `nil`   — the integration is off, the log is unreachable, the
    ///     account is absent from the log, OR the log's latest pubkey
    ///     does NOT match the served key (the marker is then about a
    ///     different key and tells us nothing about the served one).
    ///
    /// Best-effort: ANY fault maps to `nil`. This helper NEVER throws.
    ///
    /// Mirror of the web `lookupServedKeyReset`.
    ///
    /// - Parameters:
    ///   - accountId: the peer's raw 16-byte account_id.
    ///   - servedPubkey: the 32-byte Ed25519 key discovery served.
    public func lookupServedKeyReset(
        accountId: Data,
        servedPubkey: Data
    ) async -> Bool? {
        // off — no client, nothing to look up.
        guard let client = client else {
            return nil
        }
        do {
            let accountKey = transparencyAccountKey(accountId: accountId)
            guard let latest = try await client.fetchLatest(usernameHash: accountKey) else {
                // Account not yet logged — no marker to read.
                return nil
            }
            // The marker is only meaningful for the served key if the
            // log's latest entry actually commits to it. A divergence
            // means the marker describes some other key — return nil
            // (unknown).
            if latest.identityPubkey != servedPubkey {
                return nil
            }
            return latest.isReset
        } catch {
            // Best-effort enrichment — any fault is "unknown".
            return nil
        }
    }
}
