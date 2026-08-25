// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S4b — the committer liveness beacon: emit it, observe it, and let
 * the lowest-ranked *active* candidate take the sweep.
 *
 * ## What this closes
 *
 * S4a made admins propose and the **owner** sweep. Rank 0 acts without
 * consulting anything, so an admin-only group — an owner who has not opened
 * the app since the request landed — simply waits, with no deadline. That is
 * the designed behaviour, not a bug, but it is also the whole of the waiting:
 * every candidate is willing and none of them knows it is allowed.
 *
 * This slice supplies the missing fact. A candidate announces itself on the
 * group journal; every other candidate hears it, ranks its sender, and stands
 * down only for someone **strictly** better placed. Hearing nothing means
 * *act* — silence is the answer that nobody better placed is running, not a
 * failure to elect.
 *
 * ## The rules live in the core; this module supplies what the core cannot see
 *
 * `shouldEmitBeacon` and `actingCommitterVerdict` are pure, total and
 * clock-free, and both are consumed over the FFI rather than reimplemented
 * here — three clients each hand-rolling one rule is exactly the drift class
 * that produced ADR-0044's F-A replay. What this module owns is the three
 * things the core deliberately cannot supply:
 *
 * 1. **WHO** — the beacon envelope is field-less, so the sender comes from the
 *    MLS-authenticated sender leaf, resolved through `members()` to a
 *    signature key and ranked by `Group.committerRank`.
 * 2. **HOW OLD** — from the DS row's server-assigned `created_at`, which every
 *    member reads identically. The payload carries no timestamp precisely so a
 *    candidate cannot assert its own freshness: that would be a liveness claim
 *    no peer can check, i.e. a duty nobody could take away.
 * 3. **FOREGROUNDEDNESS** — a caller obligation the core cannot check. See
 *    {@link beaconEmitVerdict}.
 *
 * ## Rank at evaluation time, never at receipt
 *
 * A rank is a **position** in the role table's canonical order (`0` = owner,
 * `k + 1` = the k-th admin), not an identity. Promoting, demoting or removing
 * any admin renumbers every admin sorting after it, so a rank captured when a
 * beacon arrived and compared later against this device's own rank — which is
 * necessarily current — is not merely stale: it is a position in a **different
 * ordering**, and the comparison silently stops meaning anything.
 *
 * So {@link recordBeaconObservation} persists the sender's 32-byte signature
 * **key** and the row timestamp, and {@link projectObservations} ranks the key
 * at the moment the verdict is taken. The key rather than the leaf index for
 * the second half of the same reason: OpenMLS reuses a vacated leaf for the
 * next member added, so a persisted leaf later resolves to somebody else.
 *
 * ## This does NOT close the race
 *
 * Each candidate folds over its own poll-timed view, and a beacon is stale by
 * up to one interval even when its sender is perfectly healthy, so two
 * candidates can both reach `act`. ADR-0045's stand-down journal re-read and
 * its staged-commit rollback both remain required; `chat/sweep.ts` still runs
 * them unchanged, and reading this verdict as a licence to delete either would
 * be a mis-reading. A heartbeat removes the *wait*, not the *race*.
 *
 * ## Ports, not wasm
 *
 * Everything here is expressed over injected functions so the whole
 * orchestration is host-testable in the plain `node` vitest environment. The
 * tests wire the REAL wasm through those ports, so what is proven is the
 * composition against the core's actual rules rather than against a double
 * that happens to agree with them.
 */

/**
 * The FFI's `ActingCommitter`, as a discriminated union.
 *
 * `defer` and `not-a-candidate` stay distinct rather than collapsing to one
 * `false`, because one has something to re-evaluate on the next poll and the
 * other never will.
 */
export type ActingCommitter =
  | { readonly kind: "act" }
  | { readonly kind: "defer"; readonly toRank: number | undefined }
  | { readonly kind: "not-a-candidate" };

/** Why this device did or did not emit a beacon on this tick. */
export type BeaconEmitVerdict =
  /** Emit — this device is a candidate, foregrounded, and past its throttle. */
  | "emit"
  /** Not an owner or admin. A plain member's beacon can never make it the
   *  acting committer (every receiver's fold drops it), so emitting one is
   *  pure journal litter and a pure activity leak. */
  | "not-a-candidate"
  /** The tab is not visible. See {@link beaconEmitVerdict}. */
  | "not-foreground"
  /** A beacon went out less than one interval ago. */
  | "throttled";

/**
 * One projected observation, in the shape the FFI's `BeaconObservation`
 * constructor takes. Both fields are the OBSERVER's own re-derivation —
 * nothing in the beacon payload contributes to either.
 */
export interface ProjectedObservation {
  /** The rank re-derived for this beacon's sender AT VERDICT TIME, or
   *  `undefined` when that sender is not a candidate (a plain member, a
   *  demoted admin, or a key that no longer resolves). */
  readonly rank: number | undefined;
  /** `now - created_at` on the DS row, in milliseconds. Never negative. */
  readonly ageMs: bigint;
}

/**
 * How often this client's emitter WAKES.
 *
 * **Not the beacon interval** — that is the core's
 * `committerBeaconIntervalMs`, enforced by `shouldEmitBeacon`, and is never
 * hand-copied. This only bounds how long after a tab becomes visible (or a
 * chat is opened) the first beacon waits, so it has to be comfortably inside
 * the interval and is otherwise free to be coarse: each tick derives this
 * device's own rank, which costs an Ed25519 key derivation.
 */
export const BEACON_TICK_MS = 15_000;

/**
 * The send-side gate, with the one rule the core cannot check bolted on.
 *
 * **The foreground gate is not cosmetic.** A beacon means *able to sweep*, and
 * on this client a sweep runs only at the tail of a poll driven by a mounted,
 * visible `Chat.tsx`. The ADR-0041 background decrypt is read-only-throwaway
 * and structurally cannot commit (**C-59**), so a beacon emitted from a hidden
 * tab would be a lie that costs the group a takeover: every other candidate
 * would defer to a device that is not going to sweep, and would keep doing so
 * for as long as the hidden tab kept beaconing.
 *
 * The gate runs FIRST and the throttle decides everything else, so the
 * candidate rule and the interval rule stay the core's — the `not-a-candidate`
 * / `throttled` split below is only a label for the log line, derived after
 * the core has already said no.
 */
export function beaconEmitVerdict(input: {
  /** This device's ADR-0045 committer rank. `0` is the OWNER, not "no rank" —
   *  `if (rank)` is wrong in both directions. */
  readonly ownRank: number | undefined;
  /** `document.visibilityState === "visible"`, or the platform equivalent. */
  readonly foreground: boolean;
  /** When this device last emitted, from its OWN local clock — legitimate
   *  here and nowhere else in the beacon design, because it governs only this
   *  device's send cadence and feeds no judgement about anybody else. */
  readonly lastEmittedAtMs: number | undefined;
  readonly nowMs: number;
  /** The FFI `shouldEmitBeacon`. Injected so the rule is the core's. */
  readonly throttle: (
    ownRank: number | undefined,
    lastEmittedAtMs: bigint | undefined,
    nowMs: bigint,
  ) => boolean;
}): BeaconEmitVerdict {
  if (!input.foreground) return "not-foreground";
  const allowed = input.throttle(
    input.ownRank,
    input.lastEmittedAtMs === undefined
      ? undefined
      : BigInt(Math.trunc(input.lastEmittedAtMs)),
    BigInt(Math.trunc(input.nowMs)),
  );
  if (allowed) return "emit";
  return input.ownRank === undefined ? "not-a-candidate" : "throttled";
}

/**
 * Project the observation store into the array the fold consumes, ranking
 * every sender AGAINST THE CURRENT ROLE TABLE.
 *
 * Returns the live keys alongside, so the caller can prune in the same pass:
 * an entry whose sender no longer ranks is a member who was demoted, removed,
 * or whose key no longer resolves, and it will never contribute again.
 *
 * A row that appears to come from the FUTURE is clamped to age `0` rather than
 * wrapping. That is the safe direction as the FFI states it — an age that
 * reads too fresh makes this device defer (it waits), while one that reads too
 * stale makes it act (it may double-sweep). `ageMs` is a `bigint` on the ABI,
 * and a negative one would not merely be wrong but would sit *below* every
 * freshness window forever.
 */
export function projectObservations(
  observations: ReadonlyMap<string, number>,
  rankOf: (senderKeyB64: string) => number | undefined,
  nowMs: number,
): { readonly projected: ProjectedObservation[]; readonly liveKeys: string[] } {
  const projected: ProjectedObservation[] = [];
  const liveKeys: string[] = [];
  for (const [senderKeyB64, atMs] of observations) {
    // RULE 1: ranked HERE, at verdict time, never at receipt.
    const rank = rankOf(senderKeyB64);
    if (rank === undefined) continue;
    liveKeys.push(senderKeyB64);
    const age = Math.trunc(nowMs) - Math.trunc(atMs);
    projected.push({ rank, ageMs: BigInt(age < 0 ? 0 : age) });
  }
  return { projected, liveKeys };
}

// ---------------------------------------------------------------------------
// The observation store
// ---------------------------------------------------------------------------

const STORE_PREFIX = "lakko.committer-beacons.v1";

function storeKey(accountIdB64: string, groupIdB64: string): string {
  return `${STORE_PREFIX}.${accountIdB64}.${groupIdB64}`;
}

/**
 * Read this group's observations as `senderSignatureKeyB64 -> DS row ms`.
 *
 * Never throws: a corrupt or absent row reads as empty, which degrades to
 * "heard nothing" — and hearing nothing means *act*. That is the correct
 * direction for a store failure: the sweep's stand-down and rollback already
 * handle two candidates acting, whereas a store failure that made this device
 * defer would make it wait on evidence it does not have.
 */
export function loadBeaconObservations(
  accountIdB64: string,
  groupIdB64: string,
): Map<string, number> {
  try {
    const raw = globalThis.localStorage?.getItem(storeKey(accountIdB64, groupIdB64));
    if (raw === null || raw === undefined) return new Map();
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out = new Map<string, number>();
    for (const [k, v] of Object.entries(parsed)) {
      if (typeof v === "number" && Number.isFinite(v)) out.set(k, v);
    }
    return out;
  } catch {
    return new Map();
  }
}

/** Write this group's observations. Never throws — losing the store costs a
 *  takeover's promptness, never its correctness. */
function saveBeaconObservations(
  accountIdB64: string,
  groupIdB64: string,
  observations: ReadonlyMap<string, number>,
): void {
  try {
    const obj: Record<string, number> = {};
    for (const [k, v] of observations) obj[k] = v;
    globalThis.localStorage?.setItem(
      storeKey(accountIdB64, groupIdB64),
      JSON.stringify(obj),
    );
  } catch {
    /* best-effort */
  }
}

/**
 * Record that a beacon landed.
 *
 * **The NEWEST timestamp wins** — the opposite of `pending-requests.ts`, where
 * the first observation of a request wins so a re-broadcast cannot reset its
 * age. Here recency IS the signal: the whole question is *how long since this
 * candidate last proved it was running*, so an older stamp must never
 * overwrite a newer one (a re-fetched journal batch would otherwise age a live
 * candidate out and hand its duty away).
 *
 * A row whose timestamp does not parse is DROPPED rather than stored. There is
 * no age to measure, so it is no evidence of liveness; storing it as "fresh"
 * would defer this device forever with nothing to age out, and storing it as
 * "stale" is what dropping it already achieves.
 *
 * The store is naturally bounded by the candidate count (one entry per
 * signature key, and `projectObservations` prunes anything that no longer
 * ranks), so it needs no age-based GC of its own.
 */
export function recordBeaconObservation(
  accountIdB64: string,
  groupIdB64: string,
  senderKeyB64: string,
  atMs: number,
): void {
  if (!Number.isFinite(atMs)) return;
  const observations = loadBeaconObservations(accountIdB64, groupIdB64);
  const existing = observations.get(senderKeyB64);
  if (existing !== undefined && existing >= atMs) return;
  observations.set(senderKeyB64, atMs);
  saveBeaconObservations(accountIdB64, groupIdB64, observations);
}

/** Drop every observation whose sender no longer ranks — see
 *  {@link projectObservations}, which supplies `liveKeys` from the same pass. */
export function pruneBeaconObservations(
  accountIdB64: string,
  groupIdB64: string,
  liveKeys: readonly string[],
): void {
  const observations = loadBeaconObservations(accountIdB64, groupIdB64);
  const live = new Set(liveKeys);
  let changed = false;
  for (const key of [...observations.keys()]) {
    if (!live.has(key)) {
      observations.delete(key);
      changed = true;
    }
  }
  if (changed) saveBeaconObservations(accountIdB64, groupIdB64, observations);
}

/** Drop every observation for one account — `recover` / `forgetDevice`
 *  hygiene, the same discipline the pending-request store follows. */
export function clearBeaconObservationsForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined || store === null) return;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i++) {
      const k = store.key(i);
      if (k !== null && k.startsWith(`${STORE_PREFIX}.${accountIdB64}.`)) victims.push(k);
    }
    for (const k of victims) store.removeItem(k);
  } catch {
    /* best-effort */
  }
}

// ---------------------------------------------------------------------------
// The composed decision
// ---------------------------------------------------------------------------

/**
 * The whole receive-side decision, composed: read the store, rank every sender
 * against the CURRENT role table, prune what no longer ranks, and hand the
 * projection to the core's fold.
 *
 * `fold` is the FFI `actingCommitterVerdict` behind a thin adapter (it takes
 * `BeaconObservation` instances, which wasm-bindgen CONSUMES, so they must be
 * built fresh on every evaluation — which is what ranking-at-verdict-time
 * requires anyway).
 */
export function decideActingCommitter(input: {
  readonly accountIdB64: string;
  readonly groupIdB64: string;
  readonly ownRank: number | undefined;
  readonly rankOf: (senderKeyB64: string) => number | undefined;
  readonly nowMs: number;
  readonly freshnessWindowMs: bigint;
  readonly fold: (
    ownRank: number | undefined,
    observed: readonly ProjectedObservation[],
    freshnessWindowMs: bigint,
  ) => ActingCommitter;
}): ActingCommitter {
  const observations = loadBeaconObservations(input.accountIdB64, input.groupIdB64);
  const { projected, liveKeys } = projectObservations(
    observations,
    input.rankOf,
    input.nowMs,
  );
  pruneBeaconObservations(input.accountIdB64, input.groupIdB64, liveKeys);
  return input.fold(input.ownRank, projected, input.freshnessWindowMs);
}
