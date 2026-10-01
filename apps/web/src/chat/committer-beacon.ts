// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S4b — the committer liveness beacon: emit it, observe it, and let
 * the lowest-ranked *active* candidate take the sweep.
 *
 * ## Same rank, lower key (ADR-0050 section 2.5a item 6)
 *
 * Every device (leaf) of a ranked account holds the account's rank, so two
 * devices of one person rank equal. The core breaks that tie by signature
 * key: a device defers to a fresh observation of a LOWER rank, or of the SAME
 * rank with a LOWER key, and ignores observations of its own key. The acting
 * device of an account is therefore its lowest-keyed device whose beacon is
 * fresh, not its lowest leaf in the roster: a lost device stays in the roster
 * but stops beaconing, and its account's live device acts in its place. This
 * module hands the core both keys (this device's own, and each observation's
 * sender) and never decides the tie itself.
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
 * down only for someone better placed (a strictly lower rank, or the same
 * rank and a lower signature key). Hearing nothing means
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
  | {
      readonly kind: "defer";
      readonly toRank: number | undefined;
      /** The deferred-to device's 32-byte signature key, for the log line;
       *  absent on the fail-closed defers that name nobody. */
      readonly toSignatureKey?: Uint8Array;
    }
  | { readonly kind: "not-a-candidate" };

/** Why this device did or did not emit a beacon on this tick. */
export type BeaconEmitVerdict =
  /** Emit — this device is a candidate, foregrounded, and past its throttle. */
  | "emit"
  /** No committer rank: a plain member with no device admission to sweep, or
   *  a group with no role table. Its beacon could never make it the acting
   *  committer (every receiver's fold drops it), so emitting one is pure
   *  journal litter and a pure activity leak. */
  | "not-a-candidate"
  /** This device holds neither the root nor a registrar seed for the group
   *  (ADR-0050 section 2.5a item 8): it could not register the epoch a sweep
   *  moves to, so it never sweeps, and a beacon from it would make every
   *  other candidate defer to a device that is not going to sweep. */
  | "no-journal-authority"
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
   *  `undefined` when that sender is not a candidate (a key that no longer
   *  resolves, or a group with no role table). Since ADR-0050 section 2.5a a
   *  plain member is ranked too, after the owner and admins, and beacons only
   *  while it holds a device admission it could sweep. */
  readonly rank: number | undefined;
  /** `now - created_at` on the DS row, in milliseconds. Never negative. */
  readonly ageMs: bigint;
  /** The sender's 32-byte leaf signature key — the tie-break between devices
   *  of one account (ADR-0050 section 2.5a item 6). */
  readonly signatureKey: Uint8Array;
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
  /** Whether this device holds the root or a registrar seed for the group
   *  (`dsAccess.mayRegister`, the same check the sweep makes before staging). */
  readonly mayRegister: boolean;
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
  // Checked before the throttle, so the throttle's first-of-session beacon is
  // not spent by a device that may not announce itself at all.
  if (input.ownRank !== undefined && !input.mayRegister) return "no-journal-authority";
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
 * This device's committer rank for the beacon emitter (ADR-0050 section 2.5a
 * item 8): asked with the identity the GROUP runs on — this device's leaf, the
 * one the sweep uses — never with an identity rebuilt from the account seed.
 * On a bound device the account key holds no leaf, so the account identity
 * has no rank at all and the device would never beacon. `undefined` when no
 * device identity is loaded (fail closed: a device that cannot establish it
 * is a candidate must not claim to be one).
 */
export function deviceCommitterRank<I>(
  group: { ownCommitterRank(identity: I): number | undefined },
  deviceIdentity: I | null,
): number | undefined {
  if (deviceIdentity === null) return undefined;
  return group.ownCommitterRank(deviceIdentity);
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
    const signatureKey = decodeKey(senderKeyB64);
    // A stored key that is not 32 bytes names nobody the core can compare
    // against; it is pruned like a sender that no longer ranks.
    if (signatureKey === null) continue;
    liveKeys.push(senderKeyB64);
    const age = Math.trunc(nowMs) - Math.trunc(atMs);
    projected.push({ rank, ageMs: BigInt(age < 0 ? 0 : age), signatureKey });
  }
  return { projected, liveKeys };
}

function decodeKey(b64: string): Uint8Array | null {
  try {
    const bin = atob(b64);
    if (bin.length !== 32) return null;
    const out = new Uint8Array(32);
    for (let i = 0; i < 32; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
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
  /** This device's own leaf signature key: the key of the identity the group
   *  runs on (the one `ownCommitterRank` and the sweep use). The core ignores
   *  observations of it, so a device's own echoed beacon never defers it. */
  readonly ownSignatureKey: Uint8Array;
  readonly rankOf: (senderKeyB64: string) => number | undefined;
  readonly nowMs: number;
  readonly freshnessWindowMs: bigint;
  readonly fold: ActingCommitterFold;
}): ActingCommitter {
  const observations = loadBeaconObservations(input.accountIdB64, input.groupIdB64);
  const { projected, liveKeys } = projectObservations(
    observations,
    input.rankOf,
    input.nowMs,
  );
  pruneBeaconObservations(input.accountIdB64, input.groupIdB64, liveKeys);
  return input.fold(input.ownRank, input.ownSignatureKey, projected, input.freshnessWindowMs);
}

/** The fold port: the FFI `actingCommitterVerdict` behind an adapter. */
export type ActingCommitterFold = (
  ownRank: number | undefined,
  ownSignatureKey: Uint8Array,
  observed: readonly ProjectedObservation[],
  freshnessWindowMs: bigint,
) => ActingCommitter;

/** The part of the wasm namespace {@link wasmActingCommitterFold} calls. */
export interface ActingCommitterWasm {
  actingCommitterVerdict(
    ownRank: number | null | undefined,
    ownSignatureKey: Uint8Array,
    observed: unknown[],
    freshnessWindowMs: bigint,
  ): {
    readonly kind: string;
    readonly deferToRank: number | undefined;
    readonly deferToSignatureKey: Uint8Array | undefined;
    free(): void;
  };
  BeaconObservation: new (
    rank: number | null | undefined,
    ageMs: bigint,
    signatureKey: Uint8Array,
  ) => unknown;
}

/**
 * The FFI fold behind the {@link ActingCommitterFold} port, shared by the
 * chat view and the background sweeper so the two cannot drift.
 *
 * wasm-bindgen CONSUMES each observation, which is why they are built fresh on
 * every evaluation rather than cached — and which is also what
 * ranking-at-verdict-time requires anyway. The verdict is a wasm-owned object
 * freed here: this runs on every poll of every open group.
 *
 * The FFI throws on a malformed key (own or observed); that surfaces as a
 * throw, which every caller reads as a defer (fail closed).
 */
export function wasmActingCommitterFold(wasm: ActingCommitterWasm): ActingCommitterFold {
  return (rank, ownSignatureKey, observed, windowMs) => {
    const verdict = wasm.actingCommitterVerdict(
      rank,
      ownSignatureKey,
      observed.map((o) => new wasm.BeaconObservation(o.rank, o.ageMs, o.signatureKey)),
      windowMs,
    );
    try {
      if (verdict.kind === "act") return { kind: "act" };
      if (verdict.kind === "defer") {
        const key = verdict.deferToSignatureKey;
        return {
          kind: "defer",
          toRank: verdict.deferToRank,
          ...(key === undefined ? {} : { toSignatureKey: key }),
        };
      }
      return { kind: "not-a-candidate" };
    } finally {
      verdict.free();
    }
  };
}
