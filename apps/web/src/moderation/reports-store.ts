// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX `reporting-and-moderation.md` section 2.3 - "Your reports": the reports
 * sent from THIS device. Never synced (syncing would tie the reports
 * together) and never in the backup: one sealed row in `lakko-account-kv`,
 * which a Forget device deletes with the rest of the account's rows.
 */

import { loadAccountJson, saveAccountJson } from "../storage/account-kv";

/** One sent report, as the list shows it. */
export interface SentReport {
  /** The receipt the status is fetched with (22 base64url characters). */
  readonly receiptId: string;
  /** The target's name as the reporter saw it. */
  readonly targetName: string;
  /** The reason code (1..8). */
  readonly reason: number;
  /** When it was sent, wall-clock milliseconds. */
  readonly sentAtMs: number;
}

const NAME = "moderation-reports";
/** Kept on this device at most (chosen): the oldest go first. */
export const SENT_REPORTS_MAX = 200;

/** Read a stored list, dropping anything malformed. Pure. */
export function decodeSentReports(value: unknown): SentReport[] {
  if (!Array.isArray(value)) return [];
  const out: SentReport[] = [];
  for (const v of value) {
    if (v === null || typeof v !== "object") continue;
    const r = v as Record<string, unknown>;
    if (
      typeof r.receiptId === "string" &&
      /^[A-Za-z0-9_-]{22}$/.test(r.receiptId) &&
      typeof r.targetName === "string" &&
      typeof r.reason === "number" &&
      Number.isInteger(r.reason) &&
      typeof r.sentAtMs === "number"
    ) {
      out.push({ receiptId: r.receiptId, targetName: r.targetName, reason: r.reason, sentAtMs: r.sentAtMs });
    }
  }
  return out;
}

/** The list with `report` added, newest first, bounded. Pure. */
export function withSentReport(list: readonly SentReport[], report: SentReport): SentReport[] {
  return [report, ...list.filter((r) => r.receiptId !== report.receiptId)].slice(0, SENT_REPORTS_MAX);
}

/** The reports sent from this device, newest first. */
export async function loadSentReports(accountId: string): Promise<SentReport[]> {
  try {
    return decodeSentReports(await loadAccountJson(accountId, NAME));
  } catch {
    return [];
  }
}

/** Record one sent report. A failed write loses only this line of the list. */
export async function recordSentReport(accountId: string, report: SentReport): Promise<void> {
  try {
    const held = await loadSentReports(accountId);
    await saveAccountJson(accountId, NAME, withSentReport(held, report));
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("sent report not recorded", err instanceof Error ? err.name : "unknown");
  }
}
