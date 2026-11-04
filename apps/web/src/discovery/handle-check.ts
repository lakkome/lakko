// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `@name` field's questions to discovery, asked as seldom as search asks
 * (`docs/ux/who-can-find-me.md` section 6.1): 500 ms after typing stops, and
 * never again for the same text within 60 s. Every OPRF round spends a
 * `discovery-lookup` token and passes the per-address limit, so:
 *
 * - an OPRF answer (the name's or a search word's) is kept for the session:
 *   the server's evaluation is deterministic, and Create reuses the pair the
 *   taken check already paid for instead of asking again;
 * - the taken answer is kept 60 s per name;
 * - a rate-limited OPRF round waits once and is asked again, as the public
 *   record publisher does (`reach/publication-model.ts`), so a long name's
 *   search words do not fail a Create on the server's burst.
 *
 * A refusal the caller must show is recognised by {@link isRateLimited}, which
 * the forms map to the rate-limit line, never to "could not claim".
 */
import { base64Encode } from "../auth/account-id";
import { DiscoveryClient, DiscoveryHttpError, DiscoveryRateLimitedError } from "./client";
import { HandleWriteError, oprfForName } from "./handle-lookup";
import type { OprfPair } from "./oprf-pair";
import { TERM_RATE_LIMIT_WAIT_MS } from "../reach/publication-model";

/** Wait after typing stops before asking (UX 6.1). */
export const HANDLE_CHECK_DEBOUNCE_MS = 500;
/** The same text is not asked again within this window (UX 6.1). */
export const HANDLE_CHECK_REPEAT_MS = 60_000;

type NamePair = OprfPair & { readonly normalized: string };

const namePairs = new Map<string, Promise<NamePair>>();
const termPairs = new Map<string, Promise<OprfPair>>();
const takenAnswers = new Map<string, { readonly taken: boolean; readonly atMs: number }>();

/** Whether `err` is discovery (or a handle write) refusing for the rate. */
export function isRateLimited(err: unknown): boolean {
  if (err instanceof DiscoveryRateLimitedError) return true;
  if (err instanceof DiscoveryHttpError) return err.status === 429;
  if (err instanceof HandleWriteError) return err.status === 429;
  return false;
}

async function oncePastRateLimit<T>(ask: () => Promise<T>, wait: (ms: number) => Promise<void>): Promise<T> {
  try {
    return await ask();
  } catch (err) {
    if (!isRateLimited(err)) throw err;
    await wait(TERM_RATE_LIMIT_WAIT_MS);
    return ask();
  }
}

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

function remembered<T>(map: Map<string, Promise<T>>, key: string, ask: () => Promise<T>): Promise<T> {
  const held = map.get(key);
  if (held !== undefined) return held;
  const p = ask();
  map.set(key, p);
  // A failure is not kept: the next ask tries again.
  p.catch(() => {
    if (map.get(key) === p) map.delete(key);
  });
  return p;
}

/** The OPRF pair of an `@name`, asked once per session. */
export function namePair(name: string, wait: (ms: number) => Promise<void> = sleep): Promise<NamePair> {
  const key = name.trim().replace(/^@+/, "").toLowerCase();
  return remembered(namePairs, key, () => oncePastRateLimit(() => oprfForName(key), wait));
}

/**
 * The OPRF pairs of a record's own search words, in order: the words not
 * yet kept are asked in ONE batch (`askBatch`, one register token per 66),
 * once more after a rate limit, and kept for the session.
 */
export async function termPairsInBatch(
  terms: readonly string[],
  askBatch: (missing: readonly string[]) => Promise<OprfPair[]>,
  wait: (ms: number) => Promise<void> = sleep,
): Promise<OprfPair[]> {
  const missing = [...new Set(terms)].filter((t) => !termPairs.has(t));
  if (missing.length > 0) {
    const pairs = await oncePastRateLimit(() => askBatch(missing), wait);
    missing.forEach((t, i) => {
      const p = pairs[i];
      if (p !== undefined) termPairs.set(t, Promise.resolve(p));
    });
  }
  return Promise.all(
    terms.map((t) => {
      const held = termPairs.get(t);
      if (held === undefined) throw new Error("an OPRF batch answered fewer words than asked");
      return held;
    }),
  );
}

/** Whether `name` is held: the answer of the last 60 s, else one lookup.
 *  `null` when discovery answered nothing usable (say nothing). */
export async function handleTaken(name: string, nowMs: () => number = Date.now): Promise<boolean | null> {
  const key = name.trim().replace(/^@+/, "").toLowerCase();
  const held = takenAnswers.get(key);
  if (held !== undefined && nowMs() - held.atMs < HANDLE_CHECK_REPEAT_MS) return held.taken;
  try {
    const pair = await namePair(key);
    const res = await new DiscoveryClient().lookupRaw({ oprf_hash: base64Encode(pair.lookupKey) });
    const taken = res !== null;
    takenAnswers.set(key, { taken, atMs: nowMs() });
    return taken;
  } catch {
    return null;
  }
}

/** After a claim: the name's answer is no longer "free". */
export function noteHandleHeld(name: string, nowMs: number = Date.now()): void {
  takenAnswers.set(name.trim().replace(/^@+/, "").toLowerCase(), { taken: true, atMs: nowMs });
}

/** Test hook: forget every kept answer. */
export function resetHandleChecks(): void {
  namePairs.clear();
  termPairs.clear();
  takenAnswers.clear();
}
