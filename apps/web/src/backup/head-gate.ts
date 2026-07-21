// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Backup-head restore gate — the pure rollback-detection policy (ADR-0037 §6,
 * G-11).
 *
 * A restoring client peeks byte 0 of the storage blob and, for a framed blob
 * (`0xB1` container tag), verifies the auth-signed `BackupHead` prefix against
 * the pinned attest key before opening the phrase-AEAD envelope. This module
 * holds the **pure** classifier that turns the resolved verify facts into one
 * of four verdicts; the IO that resolves those facts (parse + hash + attest-key
 * fetch + floor read + wasm verify) lives in `head-restore.ts`.
 *
 * ## The three-case policy (ADR-0037 §6 table)
 *
 * | Case | Condition | Verdict |
 * |---|---|---|
 * | **Absent**  | byte 0 != `0xB1` (legacy blob / head stripped) | `fail-open` — warn, restore proceeds. |
 * | **Invalid** | framed but sig / `account_id` / `envelope_hash` bad, or a truncated framed blob | `hard-fail` — refuse + alarm, no override. |
 * | **Stale**   | framed + valid but `version < floor` (returning) or `signed_at` too old (fresh) | `stale-override` — user-acknowledged override, never silent. |
 *
 * The `fail-open` on an **absent** head is the deliberate availability
 * exception: the envelope stays AEAD-sealed under the recovery phrase
 * regardless of the head, so invariant #4 (no silent downgrade to non-E2EE) is
 * untouched — only rollback-detection is skipped, and a head-stripping attacker
 * gains only the pre-ADR-0037 status-quo rollback surface.
 */

/** The four terminal restore verdicts (ADR-0037 §6). */
export type RestoreVerdict =
  /** Framed + valid + within floor/freshness — open the envelope. */
  | { readonly kind: "proceed" }
  /** No head present (legacy / stripped) — restore proceeds, warn. */
  | { readonly kind: "fail-open"; readonly reason: string }
  /** Active tamper / corruption — refuse with no escape hatch. */
  | { readonly kind: "hard-fail"; readonly reason: string }
  /** Indistinguishable from a rollback replay — refuse unless the user
   *  explicitly acknowledges a deliberate downgrade. */
  | { readonly kind: "stale-override"; readonly reason: string };

/**
 * The facts a restore attempt resolves before the verdict. Every field is a
 * boolean the IO layer computes once; the pure [`restoreVerdict`] switch turns
 * them into a verdict. Keeping this a plain record makes the whole policy
 * unit-testable without wasm / IDB / network.
 */
export interface RestoreGateFacts {
  /** `parseBackupBlob` threw `invalid:` — a truncated / corrupt framed blob. */
  readonly parseError: boolean;
  /** byte 0 == `0xB1` — the blob carried a container tag + head. */
  readonly framed: boolean;
  /** A version floor was persisted for this account (returning client). When
   *  `false` the client is a zero-anchor fresh install → freshness window. */
  readonly hasFloor: boolean;
  /** Returning path: `verifyBackupHeadBinding` threw `invalid:`. */
  readonly bindingInvalid: boolean;
  /** Returning path: `verified.envelopeHash != BLAKE3(served envelope)`. */
  readonly hashMismatch: boolean;
  /** Returning path: `verified.backupVersion < floor`. */
  readonly versionBelowFloor: boolean;
  /** Fresh path: `verifyBackupHead` threw `invalid:`. */
  readonly freshInvalid: boolean;
  /** Fresh path: `verifyBackupHead` threw `stale:`. */
  readonly freshStale: boolean;
}

/** All-false facts — the base a caller spreads over. */
export const NO_FACTS: RestoreGateFacts = {
  parseError: false,
  framed: false,
  hasFloor: false,
  bindingInvalid: false,
  hashMismatch: false,
  versionBelowFloor: false,
  freshInvalid: false,
  freshStale: false,
};

/**
 * The pure rollback-detection classifier (ADR-0037 §6). Total over
 * [`RestoreGateFacts`]; no IO, no wasm — the whole security policy in one
 * auditable switch.
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
export function restoreVerdict(facts: RestoreGateFacts): RestoreVerdict {
  if (facts.parseError) {
    return {
      kind: "hard-fail",
      reason: "backup head is present but truncated or corrupt",
    };
  }
  if (!facts.framed) {
    return {
      kind: "fail-open",
      reason: "no backup head (legacy blob or head stripped)",
    };
  }
  if (facts.hasFloor) {
    // Returning client — hard monotonic floor, no freshness window involved.
    if (facts.bindingInvalid) {
      return {
        kind: "hard-fail",
        reason: "backup head signature or account binding failed",
      };
    }
    if (facts.hashMismatch) {
      return {
        kind: "hard-fail",
        reason: "backup head does not commit the served envelope",
      };
    }
    if (facts.versionBelowFloor) {
      return {
        kind: "stale-override",
        reason: "backup is older than the last one restored on this device",
      };
    }
    return { kind: "proceed" };
  }
  // Fresh install — zero anchor, freshness window applies.
  if (facts.freshInvalid) {
    return {
      kind: "hard-fail",
      reason: "backup head signature, account binding, or envelope hash failed",
    };
  }
  if (facts.freshStale) {
    return {
      kind: "stale-override",
      reason: "backup is older than the freshness window",
    };
  }
  return { kind: "proceed" };
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
export const BACKUP_HEAD_STALENESS_SECS_DEFAULT = 30 * 24 * 60 * 60;

/**
 * The configured staleness window in seconds — `VITE_LAKKO_BACKUP_HEAD_STALENESS_SECS`
 * when set to a positive integer, else [`BACKUP_HEAD_STALENESS_SECS_DEFAULT`].
 */
export function backupHeadStalenessSecs(): number {
  const env = import.meta.env as Record<string, string | undefined>;
  const raw = env["VITE_LAKKO_BACKUP_HEAD_STALENESS_SECS"];
  if (typeof raw === "string" && raw.trim().length > 0) {
    const parsed = Number.parseInt(raw.trim(), 10);
    if (Number.isFinite(parsed) && parsed > 0) {
      return parsed;
    }
  }
  return BACKUP_HEAD_STALENESS_SECS_DEFAULT;
}

/**
 * The configured staleness window in **milliseconds** as a `bigint`, ready to
 * hand to the wasm `verifyBackupHead(..., maxStalenessMillis)` — the head works
 * in milliseconds natively.
 */
export function backupHeadStalenessMillis(): bigint {
  return BigInt(backupHeadStalenessSecs()) * 1000n;
}
