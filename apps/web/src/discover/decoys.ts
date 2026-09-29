// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which directory buckets a Discover refresh fetches (ADR-0059 section 5,
 * open question 1 as decided on 2026-09-29).
 *
 * A bucket is one (topic, language) pair. The person's picked topics are
 * fetched in rounds of {@link ROUND_SIZE}; unless "Mix in random topics" is
 * off, every round is padded with decoy topics in the same language and
 * shuffled, so the server sees eight buckets from an address and cannot tell
 * which were picked.
 *
 * The decoys are a **stable random set per device per month**: one random
 * order of every topic is drawn when the month (or the language) changes and
 * reused on every refresh of that month. Drawing fresh decoys each day would
 * let an observer separate the picked buckets (present every day) from the
 * decoys (different every day); with a stable set both repeat. A change of
 * the picked topics stays visible, as the ADR says.
 *
 * Pure: the caller supplies the clock and the random source.
 */

/** Buckets per round (ADR-0059 section 5, a chosen number). */
export const ROUND_SIZE = 8;

/** A refresh at most this often (ADR-0059 section 5: once a day). */
export const REFRESH_INTERVAL_MS = 24 * 60 * 60 * 1000;

/** The decoy order this device uses for one month and one language. */
export interface DecoyDraw {
  /** `YYYY-MM`, UTC. */
  readonly month: string;
  readonly language: string;
  /** Every topic id, in a random order drawn once for the month. */
  readonly order: readonly number[];
}

/** `YYYY-MM` of `nowMs`, UTC. */
export function monthKey(nowMs: number): string {
  const d = new Date(nowMs);
  const m = d.getUTCMonth() + 1;
  return `${d.getUTCFullYear()}-${m < 10 ? "0" : ""}${m}`;
}

/** Fisher-Yates over a copy; `random` returns values in [0, 1). */
export function shuffled<T>(items: readonly T[], random: () => number): T[] {
  const out = [...items];
  for (let i = out.length - 1; i > 0; i -= 1) {
    const j = Math.min(i, Math.floor(random() * (i + 1)));
    const a = out[i] as T;
    out[i] = out[j] as T;
    out[j] = a;
  }
  return out;
}

/**
 * The decoy draw to use now: `held` when it is for this month and language,
 * else a fresh random order of `allTopics`. The second value says whether a
 * new draw was made (the caller persists it).
 */
export function currentDecoys(
  held: DecoyDraw | null,
  allTopics: readonly number[],
  language: string,
  nowMs: number,
  random: () => number,
): { readonly draw: DecoyDraw; readonly fresh: boolean } {
  const month = monthKey(nowMs);
  if (held !== null && held.month === month && held.language === language) {
    return { draw: held, fresh: false };
  }
  return { draw: { month, language, order: shuffled(allTopics, random) }, fresh: true };
}

/**
 * The rounds of topic ids to fetch. Picked topics are split into rounds of
 * {@link ROUND_SIZE}; with `mixRandom`, each round is filled up to the size
 * with the next decoys of the month's order (never a picked topic; the order
 * wraps when a day needs more decoys than there are unpicked topics) and
 * shuffled. Without `mixRandom` a round holds only picked topics. No picked
 * topics, no rounds: the shortlist is not downloaded at all.
 */
export function planBucketRounds(
  picked: readonly number[],
  decoys: DecoyDraw,
  mixRandom: boolean,
  random: () => number,
): number[][] {
  const unique = [...new Set(picked)];
  if (unique.length === 0) return [];
  const pickedSet = new Set(unique);
  const pool = decoys.order.filter((t) => !pickedSet.has(t) && Number.isInteger(t));
  const rounds: number[][] = [];
  let next = 0;
  for (let i = 0; i < unique.length; i += ROUND_SIZE) {
    const round = unique.slice(i, i + ROUND_SIZE);
    if (mixRandom && pool.length > 0) {
      const want = Math.min(ROUND_SIZE - round.length, pool.length);
      const chosen = new Set<number>();
      while (chosen.size < want) {
        chosen.add(pool[next % pool.length] as number);
        next += 1;
      }
      round.push(...chosen);
    }
    rounds.push(shuffled(round, random));
  }
  return rounds;
}

/** Whether the shortlist is due: never fetched, a day old, or a different
 *  set of picked buckets than the one last fetched. */
export function refreshDue(
  lastRefreshMs: number | null,
  lastFetchedKey: string | null,
  pickedKey: string,
  nowMs: number,
): boolean {
  if (lastRefreshMs === null || lastFetchedKey !== pickedKey) return true;
  return nowMs - lastRefreshMs >= REFRESH_INTERVAL_MS || nowMs < lastRefreshMs;
}

/** A stable key for a set of picked topics in a language. */
export function pickedKey(picked: readonly number[], language: string): string {
  return `${language}:${[...new Set(picked)].sort((a, b) => a - b).join(",")}`;
}
