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
 * ## The policy (ADR-0037 §6 table)
 *
 * | Case | Condition | Verdict |
 * |---|---|---|
 * | **Absent, no floor**  | byte 0 != `0xB1` **and** no persisted floor — a true zero-anchor fresh install | [RestoreVerdict.FailOpen] — warn, restore proceeds. |
 * | **Absent, floor, hash==** | headless but `BLAKE3(envelope) == floor.envelopeHash` — byte-identical to the last known head, stripped benignly in transit | [RestoreVerdict.Proceed]. |
 * | **Absent, floor, hash!=** | a *different* headless envelope while we hold head-protected history — a head-stripping downgrade | [RestoreVerdict.StaleOverride] — user-acknowledged, fail-closed. |
 * | **Corrupt floor** | the local anchor exists but is undecryptable / tampered (F5a) | [RestoreVerdict.StaleOverride] — the anchor is untrustworthy; never silently fall to the weaker fresh path. |
 * | **Invalid** | framed but sig / `account_id` / `envelope_hash` bad, or a truncated framed blob | [RestoreVerdict.HardFail] — refuse + alarm, no override. |
 * | **Stale**   | framed + valid but `version < floor` (returning) or `signed_at` too old (fresh) | [RestoreVerdict.StaleOverride] — user-acknowledged override, never silent. |
 *
 * The fail-open case is narrowed to **exactly** the true zero-anchor fresh
 * install (no floor). A **returning** client (any persisted floor) is
 * hard-protected: an attacker who strips the `0xB1` head to bypass the monotone
 * floor no longer wins — a headless envelope that does not byte-match the floor
 * routes to [RestoreVerdict.StaleOverride], not [RestoreVerdict.FailOpen]. The
 * fail-open that remains is the deliberate availability exception for the legacy
 * rollout: the envelope stays AEAD-sealed under the recovery phrase regardless
 * of the head, so invariant #4 (no silent downgrade to non-E2EE) is untouched —
 * only rollback-detection is skipped, and only for a client with no anchor to
 * defend.
 */

/**
 * Why a restore verdict was reached. [diagnostic] is what the log records;
 * [staleKey] is the catalog phrase the stale-backup dialog puts in its
 * `{reason}` (ADR-0058: a screen never shows the English diagnostic). Only
 * the reasons that can reach [RestoreVerdict.StaleOverride] carry one: a hard
 * refusal's dialog says no reason (`backup.rollback_refused.*`), and a
 * fail-open shows nothing.
 */
public enum class RestoreReason(public val diagnostic: String, public val staleKey: String? = null) {
    HEAD_CORRUPT("backup head is present but truncated or corrupt"),
    ANCHOR_UNREADABLE(
        "the local rollback anchor is unreadable or was tampered",
        "backup.stale_restore.reason.anchor_unreadable",
    ),
    NO_HEAD("no backup head (legacy blob or head stripped), no anchor"),
    HEAD_STRIPPED(
        "backup head stripped and the envelope differs from the last known one",
        "backup.stale_restore.reason.head_stripped",
    ),
    BINDING_INVALID("backup head signature or account binding failed"),
    HASH_MISMATCH("backup head does not commit the served envelope"),
    BELOW_FLOOR(
        "backup is older than the last one restored on this device",
        "backup.stale_restore.reason.below_floor",
    ),
    FRESH_INVALID("backup head signature, account binding, or envelope hash failed"),
    FRESH_STALE("backup is older than the freshness window", "backup.stale_restore.reason.too_old"),
    ATTEST_KEY_UNAVAILABLE("attest key unavailable - cannot verify the backup head"),
}

/** The four terminal restore verdicts (ADR-0037 §6). Mirror of the web
 *  `RestoreVerdict` discriminated union. */
public sealed interface RestoreVerdict {
    /** Framed + valid + within floor/freshness — open the envelope. */
    public data object Proceed : RestoreVerdict

    /** No head present (legacy / stripped) — restore proceeds, warn. */
    public data class FailOpen(val reason: RestoreReason) : RestoreVerdict

    /** Active tamper / corruption — refuse with no escape hatch. */
    public data class HardFail(val reason: RestoreReason) : RestoreVerdict

    /** Indistinguishable from a rollback replay — refuse unless the user
     *  explicitly acknowledges a deliberate downgrade. */
    public data class StaleOverride(val reason: RestoreReason) : RestoreVerdict
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
    /** A version floor was persisted AND read cleanly for this account (returning
     *  client). When `false` the client is a zero-anchor fresh install OR the
     *  floor was unreadable (see [floorCorrupt]). */
    val hasFloor: Boolean = false,
    /** F5a: the local anchor exists in the store but could not be decrypted /
     *  parsed (device-key rotation or active tampering). Distinct from a
     *  genuinely absent floor — the anchor is untrustworthy, so we never silently
     *  demote to the weaker fresh-install path. Only meaningful with
     *  `hasFloor == false`. */
    val floorCorrupt: Boolean = false,
    /** F4: `!framed && hasFloor` and `BLAKE3(served envelope) == floor.envelopeHash`
     *  — a headless blob byte-identical to the last known one (head stripped
     *  benignly in transit). Only meaningful when `!framed && hasFloor`. */
    val absentHashMatchesFloor: Boolean = false,
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
 *   2. a CORRUPT floor (F5a) — the local anchor exists but is untrustworthy —
 *      routes to stale-override before anything else, so we never demote to the
 *      weaker fresh path on a tampered anchor;
 *   3. an absent head splits on the floor (F4): no floor → fail-open (true
 *      zero-anchor fresh install, unchanged); floor + byte-identical envelope →
 *      proceed; floor + a DIFFERENT headless envelope → stale-override (a
 *      head-stripping downgrade against head-protected history);
 *   4. a returning client (has floor) uses the hard monotonic floor — no
 *      window — refusing an invalid signature / hash (hard) or a below-floor
 *      version (stale-override);
 *   5. a fresh install (no floor) falls back to the freshness window —
 *      refusing an invalid head (hard) or an over-window one (stale-override).
 */
public fun restoreVerdict(facts: RestoreGateFacts): RestoreVerdict {
    if (facts.parseError) {
        return RestoreVerdict.HardFail(RestoreReason.HEAD_CORRUPT)
    }
    if (facts.floorCorrupt) {
        // F5a: the persisted anchor exists but could not be decrypted / parsed.
        // It once anchored this device, so a genuine floor-below rollback and a
        // maliciously-corrupted floor are indistinguishable here — never silently
        // fall to the weaker fresh path; require an explicit acknowledgement.
        return RestoreVerdict.StaleOverride(RestoreReason.ANCHOR_UNREADABLE)
    }
    if (!facts.framed) {
        // F4: an absent head is only a benign legacy / stripped-in-transit case
        // for a TRUE zero-anchor fresh install. A returning client (has floor)
        // refuses a head-stripping downgrade unless the headless envelope is
        // byte-identical to the last known one.
        if (!facts.hasFloor) {
            return RestoreVerdict.FailOpen(RestoreReason.NO_HEAD)
        }
        if (facts.absentHashMatchesFloor) {
            return RestoreVerdict.Proceed
        }
        return RestoreVerdict.StaleOverride(RestoreReason.HEAD_STRIPPED)
    }
    if (facts.hasFloor) {
        // Returning client — hard monotonic floor, no freshness window involved.
        if (facts.bindingInvalid) {
            return RestoreVerdict.HardFail(RestoreReason.BINDING_INVALID)
        }
        if (facts.hashMismatch) {
            return RestoreVerdict.HardFail(RestoreReason.HASH_MISMATCH)
        }
        if (facts.versionBelowFloor) {
            return RestoreVerdict.StaleOverride(RestoreReason.BELOW_FLOOR)
        }
        return RestoreVerdict.Proceed
    }
    // Fresh install — zero anchor, freshness window applies.
    if (facts.freshInvalid) {
        return RestoreVerdict.HardFail(RestoreReason.FRESH_INVALID)
    }
    if (facts.freshStale) {
        return RestoreVerdict.StaleOverride(RestoreReason.FRESH_STALE)
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
