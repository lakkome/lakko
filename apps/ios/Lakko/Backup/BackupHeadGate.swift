// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Backup-head restore gate — the pure rollback-detection policy (ADR-0037 §6,
/// G-11).
///
/// Swift mirror of `apps/web/src/backup/head-gate.ts`.
///
/// A restoring client peeks byte 0 of the storage blob and, for a framed blob
/// (`0xB1` container tag), verifies the auth-signed `BackupHead` prefix against
/// the pinned attest key before opening the phrase-AEAD envelope. This file
/// holds the **pure** classifier that turns the resolved verify facts into one
/// of four verdicts; the IO that resolves those facts (parse + hash + attest-key
/// fetch + floor read + UniFFI verify) lives in ``evaluateRestoreBlob(_:)`` in
/// `BackupHeadRestore.swift`.
///
/// ## The three-case policy (ADR-0037 §6 table)
///
/// | Case | Condition | Verdict |
/// |---|---|---|
/// | **Absent**  | byte 0 != `0xB1` (legacy blob / head stripped) | `failOpen` — warn, restore proceeds. |
/// | **Invalid** | framed but sig / `account_id` / `envelope_hash` bad, or a truncated framed blob | `hardFail` — refuse + alarm, no override. |
/// | **Stale**   | framed + valid but `version < floor` (returning) or `signed_at` too old (fresh) | `staleOverride` — user-acknowledged override, never silent. |
///
/// The `failOpen` on an **absent** head is the deliberate availability
/// exception: the envelope stays AEAD-sealed under the recovery phrase
/// regardless of the head, so invariant #4 (no silent downgrade to non-E2EE) is
/// untouched — only rollback-detection is skipped, and a head-stripping attacker
/// gains only the pre-ADR-0037 status-quo rollback surface.

/// The four terminal restore verdicts (ADR-0037 §6).
public enum RestoreVerdict: Equatable {
    /// Framed + valid + within floor/freshness — open the envelope.
    case proceed
    /// No head present (legacy / stripped) — restore proceeds, warn.
    case failOpen(reason: String)
    /// Active tamper / corruption — refuse with no escape hatch.
    case hardFail(reason: String)
    /// Indistinguishable from a rollback replay — refuse unless the user
    /// explicitly acknowledges a deliberate downgrade.
    case staleOverride(reason: String)
}

/// The facts a restore attempt resolves before the verdict. Every field is a
/// boolean the IO layer computes once; the pure ``restoreVerdict(_:)`` switch
/// turns them into a verdict. Keeping this a plain value makes the whole policy
/// unit-testable without Keychain / network / UniFFI.
public struct RestoreGateFacts: Equatable {
    /// `parseBackupBlob` threw — a truncated / corrupt framed blob.
    public var parseError: Bool
    /// byte 0 == `0xB1` — the blob carried a container tag + head.
    public var framed: Bool
    /// A version floor was persisted for this account (returning client). When
    /// `false` the client is a zero-anchor fresh install → freshness window.
    public var hasFloor: Bool
    /// Returning path: `verifyBackupHeadBinding` threw (bad sig / account bind).
    public var bindingInvalid: Bool
    /// Returning path: `verified.envelopeHash != backupEnvelopeHash(envelope)`.
    public var hashMismatch: Bool
    /// Returning path: `verified.backupVersion < floor.version`.
    public var versionBelowFloor: Bool
    /// Fresh path: `verifyBackupHead` threw a non-stale (hard) `FfiError`.
    public var freshInvalid: Bool
    /// Fresh path: `verifyBackupHead` threw `FfiError.BackupHeadStale`.
    public var freshStale: Bool

    public init(
        parseError: Bool = false,
        framed: Bool = false,
        hasFloor: Bool = false,
        bindingInvalid: Bool = false,
        hashMismatch: Bool = false,
        versionBelowFloor: Bool = false,
        freshInvalid: Bool = false,
        freshStale: Bool = false
    ) {
        self.parseError = parseError
        self.framed = framed
        self.hasFloor = hasFloor
        self.bindingInvalid = bindingInvalid
        self.hashMismatch = hashMismatch
        self.versionBelowFloor = versionBelowFloor
        self.freshInvalid = freshInvalid
        self.freshStale = freshStale
    }
}

/// The pure rollback-detection classifier (ADR-0037 §6). Total over
/// ``RestoreGateFacts``; no IO, no UniFFI — the whole security policy in one
/// auditable switch. Byte-for-byte behavioural mirror of the web
/// `restoreVerdict`.
///
/// Branch order is load-bearing:
///   1. a parse error (truncated framed blob) is a hard tamper regardless;
///   2. an absent head fails open (availability exception);
///   3. a returning client (has floor) uses the hard monotonic floor — no
///      window — refusing an invalid signature / hash (hard) or a below-floor
///      version (stale-override);
///   4. a fresh install (no floor) falls back to the freshness window —
///      refusing an invalid head (hard) or an over-window one (stale-override).
public func restoreVerdict(_ facts: RestoreGateFacts) -> RestoreVerdict {
    if facts.parseError {
        return .hardFail(reason: "backup head is present but truncated or corrupt")
    }
    if !facts.framed {
        return .failOpen(reason: "no backup head (legacy blob or head stripped)")
    }
    if facts.hasFloor {
        // Returning client — hard monotonic floor, no freshness window involved.
        if facts.bindingInvalid {
            return .hardFail(reason: "backup head signature or account binding failed")
        }
        if facts.hashMismatch {
            return .hardFail(reason: "backup head does not commit the served envelope")
        }
        if facts.versionBelowFloor {
            return .staleOverride(
                reason: "backup is older than the last one restored on this device"
            )
        }
        return .proceed
    }
    // Fresh install — zero anchor, freshness window applies.
    if facts.freshInvalid {
        return .hardFail(
            reason: "backup head signature, account binding, or envelope hash failed"
        )
    }
    if facts.freshStale {
        return .staleOverride(reason: "backup is older than the freshness window")
    }
    return .proceed
}

// MARK: - Staleness window config (ADR-0037 § Freshness)

/// Default staleness window — 30 days in seconds. A rollout tuning knob: it must
/// comfortably exceed the maximum gap between a recently-active user's last
/// upload and their restore, so a legitimately-current head is never flagged
/// stale. Too tight → false "stale" refusals for inactive users (availability,
/// mitigated by the override); too loose → a wider within-window fresh-install
/// replay residual (integrity). Tune to the observed auto-backup cadence.
public let backupHeadStalenessSecsDefault: Int64 = 30 * 24 * 60 * 60

/// The configured staleness window in seconds — the `Info.plist`
/// `LakkoBackupHeadStalenessSecs` value when set to a positive integer, else
/// ``backupHeadStalenessSecsDefault``. Mirror of the web env knob
/// `VITE_LAKKO_BACKUP_HEAD_STALENESS_SECS`.
public func backupHeadStalenessSecs() -> Int64 {
    guard
        let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoBackupHeadStalenessSecs")
    else {
        return backupHeadStalenessSecsDefault
    }
    // The Info.plist value may arrive as an Int (native number) or a String.
    if let n = raw as? Int, n > 0 {
        return Int64(n)
    }
    if let s = raw as? String {
        let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
        if let parsed = Int64(trimmed), parsed > 0 {
            return parsed
        }
    }
    return backupHeadStalenessSecsDefault
}

/// The configured staleness window in **milliseconds**, ready to hand to the
/// UniFFI `verifyBackupHead(..., maxStalenessMillis:)` — the head works in
/// milliseconds natively.
public func backupHeadStalenessMillis() -> Int64 {
    backupHeadStalenessSecs() * 1000
}
