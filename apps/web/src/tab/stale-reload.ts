// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a page does when the MLS store refuses its write as stale
 * (docs/ux/one-tab.md section 6): reload once, never in a loop.
 *
 * A stale refusal means something other than this page wrote the account's
 * row since this page read it. A tab of this build cannot (it holds the
 * one-tab lock), so it is a page that does not take part: seen live on
 * test.lakko.me 2026-11-21, an older build of Lakko open elsewhere in the
 * same browser profile wrote its older provider over the row about every 30
 * seconds, and the page that reloaded on every refusal reloaded forever,
 * wiping whatever the person was typing.
 *
 * So the first refusal reloads (it picks up the newer row), and a refusal
 * within {@link STALE_RELOAD_WINDOW_MS} of a reload made for the same reason
 * does not: the page stops writing and shows the other-tab notice, whose
 * button reloads only when the person asks. The mark lives in
 * `sessionStorage` (per tab, survives the reload).
 */

/** How long after a stale reload a second refusal is taken as a writer that
 *  keeps going. Chosen, not measured. */
export const STALE_RELOAD_WINDOW_MS = 120_000;

/** The `sessionStorage` key of the last stale reload's time. */
export const STALE_RELOAD_KEY = "lakko-stale-reload-at";

/** The part of `Storage` this needs. */
export interface MarkStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/**
 * `"reload"` the first time (and records it), `"stay"` when this tab already
 * reloaded for a stale write within the window. A storage that throws counts
 * as no mark for the read and is ignored for the write, except that a failed
 * write answers `"stay"`: without the mark the next page could not tell, and
 * staying is the side that cannot loop.
 */
export function staleReaction(storage: MarkStorage | null, nowMs: number): "reload" | "stay" {
  let last: number | null = null;
  try {
    const raw = storage?.getItem(STALE_RELOAD_KEY) ?? null;
    const parsed = raw === null ? NaN : Number(raw);
    last = Number.isFinite(parsed) ? parsed : null;
  } catch {
    last = null;
  }
  if (last !== null && nowMs >= last && nowMs - last < STALE_RELOAD_WINDOW_MS) return "stay";
  try {
    if (storage === null) return "stay";
    storage.setItem(STALE_RELOAD_KEY, String(nowMs));
  } catch {
    return "stay";
  }
  return "reload";
}
