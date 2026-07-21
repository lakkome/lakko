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
/// ## The policy (ADR-0037 §6 table)
///
/// | Case | Condition | Verdict |
/// |---|---|---|
/// | **Absent, no floor**    | byte 0 != `0xB1` **and** no persisted floor — a true zero-anchor fresh install | `failOpen` — warn, restore proceeds. |
/// | **Absent, floor, hash==**   | headless but `BLAKE3(envelope) == floor.envelopeHash` — byte-identical to the last known head, stripped benignly in transit | `proceed`. |
/// | **Absent, floor, hash!=**   | a *different* headless envelope while we hold head-protected history — a head-stripping downgrade | `staleOverride` — user-acknowledged, fail-closed. |
/// | **Corrupt floor**       | the local anchor exists but is undecodable / tampered (F5a) | `staleOverride` — the anchor is untrustworthy; never silently fall to the weaker fresh path. |
/// | **Invalid** | framed but sig / `account_id` / `envelope_hash` bad, or a truncated framed blob | `hardFail` — refuse + alarm, no override. |
/// | **Stale**   | framed + valid but `version < floor` (returning) or `signed_at` too old (fresh) | `staleOverride` — user-acknowledged override, never silent. |
///
/// The `failOpen` case is narrowed to **exactly** the true zero-anchor fresh
/// install (no floor). A **returning** client (any persisted floor) is
/// hard-protected: an attacker who strips the `0xB1` head to bypass the monotone
/// floor no longer wins — a headless envelope that does not byte-match the floor
/// routes to `staleOverride`, not `failOpen`. The `failOpen` that remains is the
/// deliberate availability exception for the legacy rollout: the envelope stays
/// AEAD-sealed under the recovery phrase regardless of the head, so invariant #4
/// (no silent downgrade to non-E2EE) is untouched — only rollback-detection is
/// skipped, and only for a client with no anchor to defend.

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
    /// A version floor was persisted AND read cleanly for this account (returning
    /// client). When `false` the client is a zero-anchor fresh install OR the
    /// floor was unreadable (see `floorCorrupt`).
    public var hasFloor: Bool
    /// F5a: the local anchor exists in the Keychain but could not be decoded /
    /// parsed (present-but-wrong-length row or an active tamper). Distinct from a
    /// genuinely absent floor — the anchor is untrustworthy, so we never silently
    /// demote to the weaker fresh-install path. Only meaningful with
    /// `hasFloor == false`.
    public var floorCorrupt: Bool
    /// F4: `!framed && hasFloor` and `backupEnvelopeHash(served envelope) ==
    /// floor.envelopeHash` — a headless blob byte-identical to the last known
    /// (head stripped benignly in transit). Only meaningful when
    /// `!framed && hasFloor`.
    public var absentHashMatchesFloor: Bool
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
        floorCorrupt: Bool = false,
        absentHashMatchesFloor: Bool = false,
        bindingInvalid: Bool = false,
        hashMismatch: Bool = false,
        versionBelowFloor: Bool = false,
        freshInvalid: Bool = false,
        freshStale: Bool = false
    ) {
        self.parseError = parseError
        self.framed = framed
        self.hasFloor = hasFloor
        self.floorCorrupt = floorCorrupt
        self.absentHashMatchesFloor = absentHashMatchesFloor
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
///   2. a CORRUPT floor (F5a) — the local anchor exists but is untrustworthy —
///      routes to `staleOverride` before anything else, so we never demote to
///      the weaker fresh path on a tampered anchor;
///   3. an absent head splits on the floor (F4): no floor → `failOpen` (true
///      zero-anchor fresh install, unchanged); floor + byte-identical envelope →
///      `proceed`; floor + a DIFFERENT headless envelope → `staleOverride` (a
///      head-stripping downgrade against head-protected history);
///   4. a returning client (has floor) uses the hard monotonic floor — no
///      window — refusing an invalid signature / hash (hard) or a below-floor
///      version (stale-override);
///   5. a fresh install (no floor) falls back to the freshness window —
///      refusing an invalid head (hard) or an over-window one (stale-override).
public func restoreVerdict(_ facts: RestoreGateFacts) -> RestoreVerdict {
    if facts.parseError {
        return .hardFail(reason: "backup head is present but truncated or corrupt")
    }
    if facts.floorCorrupt {
        // F5a: the persisted anchor exists but could not be decoded / parsed. It
        // once anchored this device, so a genuine floor-below rollback and a
        // maliciously-corrupted floor are indistinguishable here — never silently
        // fall to the weaker fresh path; require an explicit acknowledgement.
        return .staleOverride(
            reason: "the local rollback anchor is unreadable or was tampered"
        )
    }
    if !facts.framed {
        // F4: an absent head is only a benign legacy / stripped-in-transit case
        // for a TRUE zero-anchor fresh install. A returning client (has floor)
        // refuses a head-stripping downgrade unless the headless envelope is
        // byte-identical to the last known one.
        if !facts.hasFloor {
            return .failOpen(
                reason: "no backup head (legacy blob or head stripped), no anchor"
            )
        }
        if facts.absentHashMatchesFloor {
            return .proceed
        }
        return .staleOverride(
            reason: "backup head stripped and the envelope differs from the last known one"
        )
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
