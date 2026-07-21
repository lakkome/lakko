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
 * ## The policy (ADR-0037 §6 table)
 *
 * | Case | Condition | Verdict |
 * |---|---|---|
 * | **Absent, no floor**    | byte 0 != `0xB1` **and** no persisted floor — a true zero-anchor fresh install | `fail-open` — warn, restore proceeds. |
 * | **Absent, floor, hash==**   | headless but `BLAKE3(envelope) == floor.envelopeHash` — byte-identical to the last known head, stripped benignly in transit | `proceed`. |
 * | **Absent, floor, hash!=**   | a *different* headless envelope while we hold head-protected history — a head-stripping downgrade | `stale-override` — user-acknowledged, fail-closed. |
 * | **Corrupt floor**       | the local anchor exists but is undecryptable / tampered (F5a) | `stale-override` — the anchor is untrustworthy; never silently fall to the weaker fresh path. |
 * | **Invalid** | framed but sig / `account_id` / `envelope_hash` bad, or a truncated framed blob | `hard-fail` — refuse + alarm, no override. |
 * | **Stale**   | framed + valid but `version < floor` (returning) or `signed_at` too old (fresh) | `stale-override` — user-acknowledged override, never silent. |
 *
 * The `fail-open` case is narrowed to **exactly** the true zero-anchor fresh
 * install (no floor). A **returning** client (any persisted floor) is
 * hard-protected: an attacker who strips the `0xB1` head to bypass the monotone
 * floor no longer wins — a headless envelope that does not byte-match the floor
 * routes to `stale-override`, not `fail-open`. The `fail-open` that remains is
 * the deliberate availability exception for the legacy rollout: the envelope
 * stays AEAD-sealed under the recovery phrase regardless of the head, so
 * invariant #4 (no silent downgrade to non-E2EE) is untouched — only
 * rollback-detection is skipped, and only for a client with no anchor to defend.
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
  /** A version floor was persisted AND read cleanly for this account (returning
   *  client). When `false` the client is a zero-anchor fresh install OR the
   *  floor was unreadable (see `floorCorrupt`). */
  readonly hasFloor: boolean;
  /** F5a: the local anchor exists in IDB but could not be decrypted / parsed
   *  (device-key rotation or active tampering). Distinct from a genuinely
   *  absent floor — the anchor is untrustworthy, so we never silently demote to
   *  the weaker fresh-install path. Only meaningful with `hasFloor == false`. */
  readonly floorCorrupt: boolean;
  /** F4: `!framed && hasFloor` and `BLAKE3(served envelope) == floor.envelopeHash`
   *  — a headless blob byte-identical to the last known (head stripped benignly
   *  in transit). Only meaningful when `!framed && hasFloor`. */
  readonly absentHashMatchesFloor: boolean;
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
  floorCorrupt: false,
  absentHashMatchesFloor: false,
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
 *   2. a CORRUPT floor (F5a) — the local anchor exists but is untrustworthy —
 *      routes to `stale-override` before anything else, so we never demote to
 *      the weaker fresh path on a tampered anchor;
 *   3. an absent head splits on the floor (F4): no floor → `fail-open` (true
 *      zero-anchor fresh install, unchanged); floor + byte-identical envelope →
 *      `proceed`; floor + a DIFFERENT headless envelope → `stale-override` (a
 *      head-stripping downgrade against head-protected history);
 *   4. a returning client (has floor) uses the hard monotonic floor — no
 *      window — refusing an invalid signature / hash (hard) or a below-floor
 *      version (stale-override);
 *   5. a fresh install (no floor) falls back to the freshness window —
 *      refusing an invalid head (hard) or an over-window one (stale-override).
 */
export function restoreVerdict(facts: RestoreGateFacts): RestoreVerdict {
  if (facts.parseError) {
    return {
      kind: "hard-fail",
      reason: "backup head is present but truncated or corrupt",
    };
  }
  if (facts.floorCorrupt) {
    // F5a: the persisted anchor exists but could not be decrypted / parsed. It
    // once anchored this device, so a genuine floor-below rollback and a
    // maliciously-corrupted floor are indistinguishable here — never silently
    // fall to the weaker fresh path; require an explicit acknowledgement.
    return {
      kind: "stale-override",
      reason: "the local rollback anchor is unreadable or was tampered",
    };
  }
  if (!facts.framed) {
    // F4: an absent head is only a benign legacy / stripped-in-transit case for
    // a TRUE zero-anchor fresh install. A returning client (has floor) refuses a
    // head-stripping downgrade unless the headless envelope is byte-identical to
    // the last known one.
    if (!facts.hasFloor) {
      return {
        kind: "fail-open",
        reason: "no backup head (legacy blob or head stripped), no anchor",
      };
    }
    if (facts.absentHashMatchesFloor) {
      return { kind: "proceed" };
    }
    return {
      kind: "stale-override",
      reason:
        "backup head stripped and the envelope differs from the last known one",
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
