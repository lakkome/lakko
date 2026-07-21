// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import me.lakko.app.BuildConfig

/**
 * Backup-head restore gate — the pure rollback-detection policy (ADR-0037 §6,
 * G-11).
 *
 * Mirror of `apps/web/src/backup/head-gate.ts`.
 *
 * A restoring client peeks byte 0 of the storage blob and, for a framed blob
 * (`0xB1` container tag), verifies the auth-signed `BackupHead` prefix against
 * the pinned attest key before opening the phrase-AEAD envelope. This file holds
 * the **pure** classifier that turns the resolved verify facts into one of four
 * verdicts; the IO that resolves those facts (parse + hash + attest-key fetch +
 * floor read + FFI verify) lives in [evaluateRestoreBlob] (`HeadRestore.kt`).
 *
 * ## The three-case policy (ADR-0037 §6 table)
 *
 * | Case | Condition | Verdict |
 * |---|---|---|
 * | **Absent**  | byte 0 != `0xB1` (legacy blob / head stripped) | [RestoreVerdict.FailOpen] — warn, restore proceeds. |
 * | **Invalid** | framed but sig / `account_id` / `envelope_hash` bad, or a truncated framed blob | [RestoreVerdict.HardFail] — refuse + alarm, no override. |
 * | **Stale**   | framed + valid but `version < floor` (returning) or `signed_at` too old (fresh) | [RestoreVerdict.StaleOverride] — user-acknowledged override, never silent. |
 *
 * The fail-open on an **absent** head is the deliberate availability exception:
 * the envelope stays AEAD-sealed under the recovery phrase regardless of the
 * head, so invariant #4 (no silent downgrade to non-E2EE) is untouched — only
 * rollback-detection is skipped, and a head-stripping attacker gains only the
 * pre-ADR-0037 status-quo rollback surface.
 */

/** The four terminal restore verdicts (ADR-0037 §6). Mirror of the web
 *  `RestoreVerdict` discriminated union. */
public sealed interface RestoreVerdict {
    /** Framed + valid + within floor/freshness — open the envelope. */
    public data object Proceed : RestoreVerdict

    /** No head present (legacy / stripped) — restore proceeds, warn. */
    public data class FailOpen(val reason: String) : RestoreVerdict

    /** Active tamper / corruption — refuse with no escape hatch. */
    public data class HardFail(val reason: String) : RestoreVerdict

    /** Indistinguishable from a rollback replay — refuse unless the user
     *  explicitly acknowledges a deliberate downgrade. */
    public data class StaleOverride(val reason: String) : RestoreVerdict
}

/**
 * The facts a restore attempt resolves before the verdict. Every field is a
 * boolean the IO layer computes once; the pure [restoreVerdict] switch turns
 * them into a verdict. Keeping this a plain data class makes the whole policy
 * unit-testable without FFI / EncryptedSharedPreferences / network.
 */
public data class RestoreGateFacts(
    /** [parseBackupBlob][uniffi.lakko_ffi_uniffi.parseBackupBlob] threw
     *  `FfiException.Backup` — a truncated / corrupt framed blob. */
    val parseError: Boolean = false,
    /** byte 0 == `0xB1` — the blob carried a container tag + head. */
    val framed: Boolean = false,
    /** A version floor was persisted for this account (returning client). When
     *  `false` the client is a zero-anchor fresh install → freshness window. */
    val hasFloor: Boolean = false,
    /** Returning path: `verifyBackupHeadBinding` threw `FfiException.Backup`. */
    val bindingInvalid: Boolean = false,
    /** Returning path: `verified.envelopeHash != backupEnvelopeHash(envelope)`. */
    val hashMismatch: Boolean = false,
    /** Returning path: `verified.backupVersion < floor.version`. */
    val versionBelowFloor: Boolean = false,
    /** Fresh path: `verifyBackupHead` threw `FfiException.Backup` (hard). */
    val freshInvalid: Boolean = false,
    /** Fresh path: `verifyBackupHead` threw `FfiException.BackupHeadStale`. */
    val freshStale: Boolean = false,
)

/**
 * The pure rollback-detection classifier (ADR-0037 §6). Total over
 * [RestoreGateFacts]; no IO, no FFI — the whole security policy in one auditable
 * `when`. Ported verbatim from the web `restoreVerdict`.
 *
 * Branch order is load-bearing:
 *   1. a parse error (truncated framed blob) is a hard tamper regardless;
 *   2. an absent head fails open (availability exception);
 *   3. a returning client (has floor) uses the hard monotonic floor — no
 *      window — refusing an invalid signature / hash (hard) or a below-floor
 *      version (stale-override);
 *   4. a fresh install (no floor) falls back to the freshness window —
 *      refusing an invalid head (hard) or an over-window one (stale-override).
 */
public fun restoreVerdict(facts: RestoreGateFacts): RestoreVerdict {
    if (facts.parseError) {
        return RestoreVerdict.HardFail(
            "backup head is present but truncated or corrupt",
        )
    }
    if (!facts.framed) {
        return RestoreVerdict.FailOpen(
            "no backup head (legacy blob or head stripped)",
        )
    }
    if (facts.hasFloor) {
        // Returning client — hard monotonic floor, no freshness window involved.
        if (facts.bindingInvalid) {
            return RestoreVerdict.HardFail(
                "backup head signature or account binding failed",
            )
        }
        if (facts.hashMismatch) {
            return RestoreVerdict.HardFail(
                "backup head does not commit the served envelope",
            )
        }
        if (facts.versionBelowFloor) {
            return RestoreVerdict.StaleOverride(
                "backup is older than the last one restored on this device",
            )
        }
        return RestoreVerdict.Proceed
    }
    // Fresh install — zero anchor, freshness window applies.
    if (facts.freshInvalid) {
        return RestoreVerdict.HardFail(
            "backup head signature, account binding, or envelope hash failed",
        )
    }
    if (facts.freshStale) {
        return RestoreVerdict.StaleOverride(
            "backup is older than the freshness window",
        )
    }
    return RestoreVerdict.Proceed
}

/**
 * Default staleness window — 30 days in seconds. A rollout tuning knob
 * (ADR-0037 § Freshness): it must comfortably exceed the maximum gap between a
 * recently-active user's last upload and their restore, so a legitimately-
 * current head is never flagged stale. Too tight → false "stale" refusals for
 * inactive users (availability, mitigated by the override); too loose → a wider
 * within-window fresh-install replay residual (integrity). Tune to the observed
 * auto-backup cadence.
 */
public const val BACKUP_HEAD_STALENESS_SECS_DEFAULT: Long = 30L * 24 * 60 * 60

/**
 * The configured staleness window in seconds —
 * `BuildConfig.LAKKO_BACKUP_HEAD_STALENESS_SECS` (gradle property
 * `lakko.backup.head.staleness.secs`) when set to a positive integer, else
 * [BACKUP_HEAD_STALENESS_SECS_DEFAULT]. Mirror of the web `backupHeadStalenessSecs`.
 */
public fun backupHeadStalenessSecs(): Long {
    val raw = BuildConfig.LAKKO_BACKUP_HEAD_STALENESS_SECS.trim()
    if (raw.isNotEmpty()) {
        val parsed = raw.toLongOrNull()
        if (parsed != null && parsed > 0L) {
            return parsed
        }
    }
    return BACKUP_HEAD_STALENESS_SECS_DEFAULT
}

/**
 * The configured staleness window in **milliseconds**, ready to hand to the FFI
 * `verifyBackupHead(..., maxStalenessMillis)` — the head works in milliseconds
 * natively. Mirror of the web `backupHeadStalenessMillis`.
 */
public fun backupHeadStalenessMillis(): Long = backupHeadStalenessSecs() * 1000L
