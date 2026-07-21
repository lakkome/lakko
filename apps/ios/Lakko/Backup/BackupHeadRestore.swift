// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Backup-head restore evaluation — the IO that resolves the
/// ``RestoreGateFacts`` (ADR-0037 §6, G-11) and applies the pure
/// ``restoreVerdict(_:)`` policy.
///
/// Swift mirror of `apps/web/src/backup/head-restore.ts`.
///
/// This is the impure half of the restore gate: it peeks the storage blob,
/// parses the framing, computes the envelope hash, fetches the pinned attest
/// key, reads the persisted floor, and runs the UniFFI verify — then hands the
/// resolved facts to ``restoreVerdict(_:)``. Kept separate from the pure
/// classifier so the security policy stays unit-testable without Keychain /
/// network / UniFFI.

/// The resolved outcome of evaluating a storage blob against the restore gate.
public struct RestoreEvaluation {
    /// The terminal verdict from ``restoreVerdict(_:)``.
    public let verdict: RestoreVerdict
    /// The envelope portion to open with the phrase (the whole blob for a legacy
    /// blob; the tail after `TAG ‖ head` for a framed one).
    public let envelope: Data
    /// `backupEnvelopeHash(envelope)` — carried so a clean `proceed` can persist
    /// the floor without recomputing. `nil` when the blob failed to parse or was
    /// unframed.
    public let envelopeHash: Data?
    /// The verified `backup_version` on a `proceed`, else `nil`.
    public let verifiedVersion: UInt64?
}

/// Inputs the restore seam threads in. Mirror of the web
/// `RestoreEvaluationInput`.
public struct RestoreEvaluationInput {
    /// The raw bytes fetched from `GET /backup`.
    public let blob: Data
    /// The 16-byte account_id the client authenticated as (head binding).
    public let accountId: Data
    /// Base64 account id — the floor-store key.
    public let accountIdB64: String
    /// Auth base for the attest-key fetch (defaults to the DeviceAttestKey base).
    public let authBaseUrl: String
    /// Clock in ms (defaults to now).
    public let nowMillis: Int64
    /// Staleness window in ms (defaults to the Info.plist knob).
    public let stalenessMillis: Int64

    public init(
        blob: Data,
        accountId: Data,
        accountIdB64: String,
        authBaseUrl: String,
        nowMillis: Int64,
        stalenessMillis: Int64
    ) {
        self.blob = blob
        self.accountId = accountId
        self.accountIdB64 = accountIdB64
        self.authBaseUrl = authBaseUrl
        self.nowMillis = nowMillis
        self.stalenessMillis = stalenessMillis
    }
}

/// Evaluate a storage blob against the ADR-0037 §6 restore gate.
///
/// Resolves each fact via IO then returns the ``restoreVerdict(_:)``. The caller
/// (``BackupClient/restore(phraseBytes:gate:)``) acts on the verdict:
/// `hardFail` → refuse + alarm; `staleOverride` → ask the user; `failOpen` /
/// `proceed` → open the envelope (and, on a clean `proceed`, persist the floor
/// from `verifiedVersion` + `envelopeHash`).
public func evaluateRestoreBlob(_ input: RestoreEvaluationInput) async -> RestoreEvaluation {
    // 1. Parse the framing. A truncated / corrupt FRAMED blob throws → hard-fail
    //    (active tamper). A legacy / empty blob parses as unframed.
    let parsed: ParsedBackupBlob
    do {
        parsed = try parseBackupBlob(blob: input.blob)
    } catch {
        return RestoreEvaluation(
            verdict: restoreVerdict(RestoreGateFacts(parseError: true)),
            envelope: input.blob,
            envelopeHash: nil,
            verifiedVersion: nil
        )
    }

    let envelope = parsed.envelope

    // 2. Absent head → fail-open (availability exception; the phrase-AEAD still
    //    protects the content — invariant #4 untouched).
    guard parsed.framed, let head = parsed.head else {
        return RestoreEvaluation(
            verdict: restoreVerdict(RestoreGateFacts(framed: false)),
            envelope: envelope,
            envelopeHash: nil,
            verifiedVersion: nil
        )
    }

    let envelopeHash = backupEnvelopeHash(envelopeBytes: envelope)

    // 3. A framed head we cannot verify (no pinned + no reachable JWKS attest
    //    key) is a misconfig, NOT an absent head — fail-closed (hard), never
    //    silently accept an unverified framed head (ADR-0037 §6).
    let attestKey: Data
    do {
        attestKey = try await DeviceAttestKey.fetch(baseURL: input.authBaseUrl)
    } catch {
        return RestoreEvaluation(
            verdict: .hardFail(reason: "attest key unavailable — cannot verify the backup head"),
            envelope: envelope,
            envelopeHash: envelopeHash,
            verifiedVersion: nil
        )
    }

    // 4. Read the persisted floor. Present → returning client (hard monotonic
    //    floor); absent → zero-anchor fresh install (freshness window).
    if let floor = BackupFloorStore.getFloor(accountIdB64: input.accountIdB64) {
        // Returning client: signature + account binding only, then hash + floor.
        do {
            let verified = try verifyBackupHeadBinding(
                head: head,
                attestVerifyingKey: attestKey,
                expectedAccountId: input.accountId
            )
            let hashMismatch = verified.envelopeHash != envelopeHash
            let versionBelowFloor = verified.backupVersion < floor.version
            let verdict = restoreVerdict(
                RestoreGateFacts(
                    framed: true,
                    hasFloor: true,
                    hashMismatch: hashMismatch,
                    versionBelowFloor: versionBelowFloor
                )
            )
            let verifiedVersion: UInt64? = (verdict == .proceed) ? verified.backupVersion : nil
            return RestoreEvaluation(
                verdict: verdict,
                envelope: envelope,
                envelopeHash: envelopeHash,
                verifiedVersion: verifiedVersion
            )
        } catch {
            return RestoreEvaluation(
                verdict: restoreVerdict(
                    RestoreGateFacts(framed: true, hasFloor: true, bindingInvalid: true)
                ),
                envelope: envelope,
                envelopeHash: envelopeHash,
                verifiedVersion: nil
            )
        }
    }

    // Fresh install: fold signature + account + hash + freshness into one call.
    do {
        let verified = try verifyBackupHead(
            head: head,
            attestVerifyingKey: attestKey,
            expectedAccountId: input.accountId,
            expectedEnvelopeHash: envelopeHash,
            nowMillis: input.nowMillis,
            maxStalenessMillis: input.stalenessMillis
        )
        return RestoreEvaluation(
            verdict: restoreVerdict(RestoreGateFacts(framed: true, hasFloor: false)),
            envelope: envelope,
            envelopeHash: envelopeHash,
            verifiedVersion: verified.backupVersion
        )
    } catch {
        // On iOS the error kind is the typed `FfiError.BackupHeadStale` variant
        // (stale-override) vs ANY other FfiError (hard invalid tamper).
        let stale: Bool
        if let ffi = error as? FfiError, case .BackupHeadStale = ffi {
            stale = true
        } else {
            stale = false
        }
        return RestoreEvaluation(
            verdict: restoreVerdict(
                RestoreGateFacts(
                    framed: true,
                    hasFloor: false,
                    // Any non-stale failure (including a bad sig / account /
                    // hash) is a hard tamper.
                    freshInvalid: !stale,
                    freshStale: stale
                )
            ),
            envelope: envelope,
            envelopeHash: envelopeHash,
            verifiedVersion: nil
        )
    }
}
