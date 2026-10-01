// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S4b — the committer liveness beacon, driven over its ports.
 *
 * ## The wasm is REAL
 *
 * `shouldEmitBeacon`, `actingCommitterVerdict`, `BeaconObservation` and the
 * four constants are the actual core, loaded through the Node `__wbindgen_init`
 * glue. Only `Group.committerRank` is doubled — and it is doubled by a plain
 * lookup table that RETURNS `undefined` for an unknown key rather than
 * throwing, because that is what the real method does for a non-candidate.
 * A double that throws where the real code degrades is the trap this suite has
 * fallen into twice: it makes the test pass whether or not the production code
 * handles the case.
 *
 * ## Which of these discriminate (MEASURED — every probe below was run)
 *
 * 1. **`a plain member never emits`** — with `beaconEmitVerdict` passing a
 *    hardcoded `0` to the throttle instead of `input.ownRank` (the realistic
 *    bug: a caller that assumes it is the owner):
 *
 *        FAIL … > the emit gate > a plain member never emits — its beacon is
 *        pure litter
 *        AssertionError: expected 'emit' to be 'not-a-candidate'
 *
 * 2. **`the throttle holds inside one interval`** — with
 *    `lastEmittedAtMs` always passed as `undefined` (the realistic bug: not
 *    persisting the last-emit stamp):
 *
 *        FAIL … > the emit gate > the throttle holds inside one interval
 *        AssertionError: expected 'emit' to be 'throttled'
 *
 * 3. **`a hidden tab never emits`** — with the `foreground` gate deleted:
 *
 *        FAIL … > the emit gate > a hidden tab never emits, even when the
 *        throttle would allow it
 *        AssertionError: expected 'emit' to be 'not-foreground'
 *
 * 4a. **`a demotion between receipt and verdict is reflected`** — with the
 *    rank MEMOIZED on first sight, which is exactly "ranked at receipt":
 *
 *        FAIL … > rank is derived at VERDICT time, never at receipt > a
 *        demotion between receipt and verdict is reflected in WHO we defer to
 *        AssertionError: expected { kind: 'defer', toRank: 1 } to deeply equal
 *        { kind: 'act' }
 *        -   "kind": "act",
 *        +   "kind": "defer",
 *        +   "toRank": 1,
 *
 *    i.e. the device stands down for a member who is no longer allowed to
 *    commit at all, and the group stops applying membership changes. NOTE the
 *    memoized probe leaves `a beacon from a since-demoted member no longer
 *    defers` GREEN — that test only ever evaluates once, post-demotion — which
 *    is why it has its own probe:
 *
 * 4b. **`a beacon from a since-demoted member no longer defers`** — with the
 *    classic fail-open default, `rankOf(key) ?? 0`. FOUR tests went red:
 *
 *        FAIL … > a beacon from a since-demoted member no longer defers
 *        AssertionError: expected { kind: 'defer', toRank: +0 } to deeply equal
 *        { kind: 'act' }
 *
 *    plus `a demotion between receipt and verdict …` (deferring to `0` instead
 *    of `1`) and both prune tests, since an unranked sender is never pruned.
 *
 * 5. **`a stale beacon is ignored — on the DS clock`** — with
 *    `projectObservations` sourcing the age from a local `Date.now()`-style
 *    "when we saw it" rather than the DS row (modelled by stamping every
 *    observation at `nowMs`):
 *
 *        FAIL … > the freshness window > a stale beacon is ignored — measured
 *        on the DS row, never a local clock
 *        AssertionError: expected { kind: 'defer', toRank: 0 } to deeply equal
 *        { kind: 'act' }
 *
 * 6. **`an older re-fetched row never overwrites a newer one`** — with the
 *    newest-wins guard in `recordBeaconObservation` removed:
 *
 *        FAIL … > the observation store > an older re-fetched row never
 *        overwrites a newer one
 *        AssertionError: expected 1000 to be 9000
 *
 *    A re-fetched journal batch would age a live candidate out and hand its
 *    duty away.
 *
 * BOUNDARY GUARD, not a discriminator: `hearing nothing makes a candidate
 * act`. It passes under every probe above. Its job is to catch the single most
 * likely future mis-reading of the fold — that silence should mean "wait".
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";

import {
  BEACON_TICK_MS,
  beaconEmitVerdict,
  clearBeaconObservationsForAccount,
  decideActingCommitter,
  deviceCommitterRank,
  loadBeaconObservations,
  projectObservations,
  pruneBeaconObservations,
  recordBeaconObservation,
  wasmActingCommitterFold,
  type ActingCommitter,
  type ActingCommitterWasm,
} from "./committer-beacon";

/** The `localStorage` shim convention this repo already uses. */
class MemoryStorage {
  private readonly map = new Map<string, string>();
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return [...this.map.keys()][i] ?? null;
  }
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await initWasm({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  (globalThis as unknown as { localStorage: unknown }).localStorage =
    new MemoryStorage();
});

afterEach(() => {
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
});

const ACCOUNT = "acct-1";
const GROUP = "grp-1";

/** The REAL FFI throttle, behind the port shape. */
const realThrottle = (
  rank: number | undefined,
  last: bigint | undefined,
  now: bigint,
): boolean => wasm.shouldEmitBeacon(rank, last, now);

/** The REAL FFI fold, through the same adapter production uses. */
const realFold = wasmActingCommitterFold(wasm as unknown as ActingCommitterWasm);

/** A 32-byte signature key whose every byte is `byte`, as the store keys it
 *  (standard base64). Bytewise order is the byte order, so a lower `byte`
 *  is a lower key for the core's same-rank tie-break. */
function keyOf(byte: number): string {
  return btoa(String.fromCharCode(...new Array<number>(32).fill(byte)));
}
function bytesOf(b64: string): Uint8Array {
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}

const OWNER = keyOf(0x10);
const ADMIN = keyOf(0x20);
const EX_ADMIN = keyOf(0x30);
const GONE = keyOf(0x31);
const HERE = keyOf(0x32);
const KA = keyOf(0x33);
const KB = keyOf(0x34);
const KK = keyOf(0x35);
const KX = keyOf(0x36);
/** This device's own leaf key in every test unless one says otherwise. */
const SELF = keyOf(0x80);
/** Sibling devices of this device's own account, around SELF in key order. */
const SIBLING_LOWER = keyOf(0x40);
const SIBLING_HIGHER = keyOf(0xc0);

/**
 * A role table as a plain lookup, matching what the real `Group.committerRank`
 * returns: `0` for the owner, `k + 1` for the k-th admin, and `undefined` for
 * a plain member or a key that does not resolve. It DEGRADES rather than
 * throwing, exactly like the real method.
 */
function roleTable(entries: Record<string, number>) {
  return (senderKeyB64: string): number | undefined => entries[senderKeyB64];
}

function decide(input: {
  ownRank: number | undefined;
  rankOf: (k: string) => number | undefined;
  nowMs: number;
  windowMs?: bigint;
  ownKey?: string;
}): ActingCommitter {
  return decideActingCommitter({
    accountIdB64: ACCOUNT,
    groupIdB64: GROUP,
    ownRank: input.ownRank,
    ownSignatureKey: bytesOf(input.ownKey ?? SELF),
    rankOf: input.rankOf,
    nowMs: input.nowMs,
    freshnessWindowMs: input.windowMs ?? wasm.committerBeaconFreshnessWindowMs(),
    fold: realFold,
  });
}

// ---------------------------------------------------------------------------

describe("ADR-0045 S4b — the emit gate", () => {
  it("a candidate that has never emitted emits at once", () => {
    // The core's own rule 2: the first beacon of a session is immediate,
    // because that is exactly when a stale view is likeliest to have handed
    // this device's duty elsewhere.
    expect(
      beaconEmitVerdict({
        ownRank: 1,
        foreground: true,
        lastEmittedAtMs: undefined,
        nowMs: 1_000_000,
        mayRegister: true,
        throttle: realThrottle,
      }),
    ).toBe("emit");
  });

  it("a plain member never emits — its beacon is pure litter", () => {
    // `ownRank === undefined`. A plain member's beacon can never make it the
    // acting committer (every receiver's fold drops it), so emitting one buys
    // nothing and publishes this device's activity schedule for free.
    expect(
      beaconEmitVerdict({
        ownRank: undefined,
        foreground: true,
        lastEmittedAtMs: undefined,
        nowMs: 1_000_000,
        mayRegister: true,
        throttle: realThrottle,
      }),
    ).toBe("not-a-candidate");
  });

  it("the throttle holds inside one interval", () => {
    const interval = Number(wasm.committerBeaconIntervalMs());
    const now = 5_000_000;
    expect(
      beaconEmitVerdict({
        ownRank: 0,
        foreground: true,
        lastEmittedAtMs: now - interval + 1,
        nowMs: now,
        mayRegister: true,
        throttle: realThrottle,
      }),
    ).toBe("throttled");
    // …and releases exactly one interval later.
    expect(
      beaconEmitVerdict({
        ownRank: 0,
        foreground: true,
        lastEmittedAtMs: now - interval,
        nowMs: now,
        mayRegister: true,
        throttle: realThrottle,
      }),
    ).toBe("emit");
  });

  it("ADR-0050 2.5a item 8 — a candidate with no root and no registrar seed never emits", () => {
    // The same check the sweep makes before staging: a device that cannot
    // register the epoch a sweep moves to never sweeps, so a beacon from it
    // would make every other candidate defer to nobody.
    expect(
      beaconEmitVerdict({
        ownRank: 2,
        foreground: true,
        lastEmittedAtMs: undefined,
        nowMs: 1_000_000,
        mayRegister: false,
        throttle: realThrottle,
      }),
    ).toBe("no-journal-authority");
  });

  it("a hidden tab never emits, even when the throttle would allow it", () => {
    // THE caller obligation the core cannot check. A beacon means "able to
    // sweep"; the ADR-0041 background decrypt is read-only-throwaway and
    // structurally cannot commit (C-59), so a beacon from a hidden tab is a
    // lie every other candidate would defer to — and would keep deferring to.
    expect(
      beaconEmitVerdict({
        ownRank: 0,
        foreground: false,
        lastEmittedAtMs: undefined,
        nowMs: 1_000_000,
        mayRegister: true,
        throttle: realThrottle,
      }),
    ).toBe("not-foreground");
  });
});

describe("ADR-0045 S4b — the fold", () => {
  it("hearing nothing makes a candidate act", () => {
    // The rule most likely to be got wrong. Silence is not "wait": it is the
    // answer that nobody better placed is running, which makes this device the
    // one that should.
    expect(decide({ ownRank: 3, rankOf: roleTable({}), nowMs: 1_000 })).toEqual({
      kind: "act",
    });
  });

  it("a fresh beacon from a strictly lower rank makes a candidate defer", () => {
    recordBeaconObservation(ACCOUNT, GROUP, OWNER, 1_000_000);
    expect(
      decide({
        ownRank: 2,
        rankOf: roleTable({ [OWNER]: 0 }),
        nowMs: 1_000_000 + 5_000,
      }),
    ).toMatchObject({ kind: "defer", toRank: 0, toSignatureKey: bytesOf(OWNER) });
  });

  it("a device's own echoed-back beacon is ignored, whatever it ranks", () => {
    // The journal echoes this device's own beacon back through its own poll,
    // and it lands in the store like anyone else's. The core ignores an
    // observation under this device's own key, so it cannot deadlock itself.
    recordBeaconObservation(ACCOUNT, GROUP, SELF, 1_000_000);
    expect(
      decide({
        ownRank: 2,
        rankOf: roleTable({ [SELF]: 2 }),
        nowMs: 1_000_000 + 5_000,
      }),
    ).toEqual({ kind: "act" });
  });

  it("ADR-0050 2.5a item 6 — a sibling device with a LOWER key defers us", () => {
    // Every device of a ranked account holds the account's rank: the sibling
    // ranks equal, and the lower key acts.
    recordBeaconObservation(ACCOUNT, GROUP, SIBLING_LOWER, 1_000_000);
    expect(
      decide({
        ownRank: 2,
        rankOf: roleTable({ [SIBLING_LOWER]: 2, [SELF]: 2 }),
        nowMs: 1_000_000 + 5_000,
      }),
    ).toMatchObject({ kind: "defer", toRank: 2, toSignatureKey: bytesOf(SIBLING_LOWER) });
  });

  it("ADR-0050 2.5a item 6 — a sibling device with a HIGHER key does not", () => {
    recordBeaconObservation(ACCOUNT, GROUP, SIBLING_HIGHER, 1_000_000);
    expect(
      decide({
        ownRank: 2,
        rankOf: roleTable({ [SIBLING_HIGHER]: 2, [SELF]: 2 }),
        nowMs: 1_000_000 + 5_000,
      }),
    ).toEqual({ kind: "act" });
  });

  it("ADR-0050 2.5a item 6 — the owner's lower-keyed sibling device defers it too", () => {
    // Rank 0 never waits for a higher rank, but two devices of the owner's
    // account rank 0 both, and only one of them acts.
    recordBeaconObservation(ACCOUNT, GROUP, SIBLING_LOWER, 1_000_000);
    expect(
      decide({
        ownRank: 0,
        rankOf: roleTable({ [SIBLING_LOWER]: 0 }),
        nowMs: 1_000_000 + 5_000,
      }),
    ).toMatchObject({ kind: "defer", toRank: 0 });
  });

  it("a lower-keyed sibling whose beacon went stale no longer holds the duty", () => {
    // A lost device stays in the roster; once its beacon ages out the live
    // device of the same account acts.
    const window = Number(wasm.committerBeaconFreshnessWindowMs());
    recordBeaconObservation(ACCOUNT, GROUP, SIBLING_LOWER, 1_000_000);
    expect(
      decide({
        ownRank: 2,
        rankOf: roleTable({ [SIBLING_LOWER]: 2 }),
        nowMs: 1_000_000 + window + 1,
      }),
    ).toEqual({ kind: "act" });
  });

  it("a higher rank never defers a better-placed candidate", () => {
    recordBeaconObservation(ACCOUNT, GROUP, ADMIN, 1_000_000);
    expect(
      decide({
        ownRank: 0,
        rankOf: roleTable({ [ADMIN]: 4 }),
        nowMs: 1_000_000 + 5_000,
      }),
    ).toEqual({ kind: "act" });
  });

  it("a plain member is never a candidate, whatever it has heard", () => {
    recordBeaconObservation(ACCOUNT, GROUP, OWNER, 1_000_000);
    expect(
      decide({
        ownRank: undefined,
        rankOf: roleTable({ [OWNER]: 0 }),
        nowMs: 1_000_000 + 5_000,
      }),
    ).toEqual({ kind: "not-a-candidate" });
  });
});

describe("ADR-0045 S4b — the freshness window", () => {
  it("a stale beacon is ignored — measured on the DS row, never a local clock", () => {
    const window = Number(wasm.committerBeaconFreshnessWindowMs());
    // The DS stamped this row one millisecond past the window. The device may
    // have SEEN it a moment ago; that is exactly the distinction the beacon's
    // field-less payload forces, and the age must come from the row.
    recordBeaconObservation(ACCOUNT, GROUP, OWNER, 1_000_000);
    expect(
      decide({
        ownRank: 2,
        rankOf: roleTable({ [OWNER]: 0 }),
        nowMs: 1_000_000 + window + 1,
      }),
    ).toEqual({ kind: "act" });
    // One millisecond inside the window it still defers, so the boundary is
    // where it is claimed to be and not merely somewhere nearby.
    expect(
      decide({
        ownRank: 2,
        rankOf: roleTable({ [OWNER]: 0 }),
        nowMs: 1_000_000 + window - 1,
      }),
    ).toMatchObject({ kind: "defer", toRank: 0, toSignatureKey: bytesOf(OWNER) });
  });

  it("a row from the future is clamped to fresh, never wrapped", () => {
    // `ageMs` is an unsigned bigint on the ABI: a negative age would sit BELOW
    // every window forever. Clamping to 0 also errs the safe way — too fresh
    // makes this device wait, too stale makes it double-sweep.
    const { projected } = projectObservations(
      new Map([[OWNER, 2_000_000]]),
      roleTable({ [OWNER]: 0 }),
      1_000_000,
    );
    expect(projected).toEqual([{ rank: 0, ageMs: 0n, signatureKey: bytesOf(OWNER) }]);
  });
});

describe("ADR-0045 S4b — rank is derived at VERDICT time, never at receipt", () => {
  it("a beacon from a since-demoted member no longer defers", () => {
    // Rank 1 when the beacon landed; demoted out of the admin set since. Under
    // the current table its sender is not a candidate at all, so the beacon is
    // no evidence about anybody and this device acts.
    recordBeaconObservation(ACCOUNT, GROUP, EX_ADMIN, 1_000_000);
    expect(
      decide({
        ownRank: 2,
        rankOf: roleTable({}), // demoted: no longer resolves to a rank
        nowMs: 1_000_000 + 5_000,
      }),
    ).toEqual({ kind: "act" });
  });

  it("a demotion between receipt and verdict is reflected in WHO we defer to", () => {
    // Two beacons. Under the table as it stood at receipt, `a` was rank 1 and
    // `b` rank 3. `a` is then demoted and `b` promoted into its slot. Ranking
    // at verdict time is what makes the answer `1` (b's CURRENT position)
    // rather than `1` attributed to a member who can no longer commit — and
    // what stops a promoted admin being compared as though it were still 3.
    recordBeaconObservation(ACCOUNT, GROUP, KA, 1_000_000);
    recordBeaconObservation(ACCOUNT, GROUP, KB, 1_000_000);
    const atReceipt = roleTable({ [KA]: 1, [KB]: 3 });
    const atVerdict = roleTable({ [KB]: 1 });

    expect(
      decide({ ownRank: 2, rankOf: atReceipt, nowMs: 1_000_000 + 5_000 }),
    ).toMatchObject({ kind: "defer", toRank: 1 });

    // Same stored bytes, different table: `a` contributes nothing and `b` is
    // now the one strictly better placed.
    expect(
      decide({ ownRank: 2, rankOf: atVerdict, nowMs: 1_000_000 + 5_000 }),
    ).toMatchObject({ kind: "defer", toRank: 1 });

    // …and once `b` is demoted below this device too, nothing defers it.
    expect(
      decide({
        ownRank: 2,
        rankOf: roleTable({ [KB]: 5 }),
        nowMs: 1_000_000 + 5_000,
      }),
    ).toEqual({ kind: "act" });
  });

  it("the projection prunes a sender that no longer ranks", () => {
    recordBeaconObservation(ACCOUNT, GROUP, GONE, 1_000_000);
    recordBeaconObservation(ACCOUNT, GROUP, HERE, 1_000_000);
    const { liveKeys } = projectObservations(
      loadBeaconObservations(ACCOUNT, GROUP),
      roleTable({ [HERE]: 1 }),
      1_000_000,
    );
    expect(liveKeys).toEqual([HERE]);
    pruneBeaconObservations(ACCOUNT, GROUP, liveKeys);
    expect([...loadBeaconObservations(ACCOUNT, GROUP).keys()]).toEqual([HERE]);
  });

  it("decideActingCommitter prunes in the same pass it folds", () => {
    recordBeaconObservation(ACCOUNT, GROUP, GONE, 1_000_000);
    decide({ ownRank: 1, rankOf: roleTable({}), nowMs: 1_000_000 });
    expect(loadBeaconObservations(ACCOUNT, GROUP).size).toBe(0);
  });
});

describe("ADR-0045 S4b — the observation store", () => {
  it("the NEWEST row wins — the opposite of the pending-request store", () => {
    recordBeaconObservation(ACCOUNT, GROUP, KK, 1_000);
    recordBeaconObservation(ACCOUNT, GROUP, KK, 9_000);
    expect(loadBeaconObservations(ACCOUNT, GROUP).get(KK)).toBe(9_000);
  });

  it("an older re-fetched row never overwrites a newer one", () => {
    // A re-fetched journal batch replays old rows. Letting one win would age a
    // live candidate out and hand its duty away for no reason.
    recordBeaconObservation(ACCOUNT, GROUP, KK, 9_000);
    recordBeaconObservation(ACCOUNT, GROUP, KK, 1_000);
    expect(loadBeaconObservations(ACCOUNT, GROUP).get(KK)).toBe(9_000);
  });

  it("an unparseable DS timestamp is dropped, not stored as fresh", () => {
    // `Date.parse` of a malformed timestamp is NaN. There is no age to
    // measure, so it is no evidence of liveness — and storing it as "fresh"
    // would defer this device forever with nothing that could ever age out.
    recordBeaconObservation(ACCOUNT, GROUP, KK, Number.NaN);
    expect(loadBeaconObservations(ACCOUNT, GROUP).size).toBe(0);
    expect(decide({ ownRank: 2, rankOf: roleTable({ [KK]: 0 }), nowMs: 1 })).toEqual(
      { kind: "act" },
    );
  });

  it("a corrupt store reads as empty — which means ACT, not wait", () => {
    globalThis.localStorage.setItem(
      "lakko.committer-beacons.v1.acct-1.grp-1",
      "{ not json",
    );
    expect(loadBeaconObservations(ACCOUNT, GROUP).size).toBe(0);
    expect(
      decide({ ownRank: 1, rankOf: roleTable({ [KX]: 0 }), nowMs: 1_000 }),
    ).toEqual({ kind: "act" });
  });

  it("observations are per-group and cleared per-account", () => {
    recordBeaconObservation(ACCOUNT, GROUP, KK, 1_000);
    recordBeaconObservation(ACCOUNT, "grp-2", KK, 2_000);
    recordBeaconObservation("acct-2", GROUP, KK, 3_000);
    expect(loadBeaconObservations(ACCOUNT, GROUP).get(KK)).toBe(1_000);
    expect(loadBeaconObservations(ACCOUNT, "grp-2").get(KK)).toBe(2_000);

    clearBeaconObservationsForAccount(ACCOUNT);
    expect(loadBeaconObservations(ACCOUNT, GROUP).size).toBe(0);
    expect(loadBeaconObservations(ACCOUNT, "grp-2").size).toBe(0);
    // Another account's rows are untouched.
    expect(loadBeaconObservations("acct-2", GROUP).get(KK)).toBe(3_000);
  });
});

describe("ADR-0045 S4b — the constants come from the core", () => {
  it("the freshness window is the documented construction", () => {
    // Pinned against a literal on purpose: the interval and the window are two
    // halves of one budget, and a client that hand-copied either would drift
    // silently — the class ADR-0044 exists about. If this fails, the core
    // moved and this client must be re-read, not patched.
    expect(wasm.committerBeaconFreshnessWindowMs()).toBe(
      2n * wasm.committerBeaconIntervalMs() +
        wasm.committerBeaconWorstCasePollMs(),
    );
  });

  it("the emitter's tick is well inside the beacon interval", () => {
    // Not a core rule — a sanity bound on this client's own driver. A tick
    // coarser than the interval would silently stretch the cadence.
    expect(BEACON_TICK_MS).toBeLessThan(
      Number(wasm.committerBeaconIntervalMs()),
    );
  });
});

describe("ADR-0050 section 2.5a item 8 — the beacon ranks with the DEVICE identity", () => {
  it("a bound device ranks with its own leaf identity; the account identity holds no leaf", () => {
    // Web used to rank with an identity rebuilt from the ACCOUNT seed, which
    // holds no leaf on a bound device, so it never beaconed. The group is run
    // by the device identity (the one sweeps use), and only that ranks.
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x41));
    const device = wasm.Identity.fromSeedBound(
      new Uint8Array(32).fill(0x42),
      account.ed25519Seed,
      1_700_000_000n,
    );
    const group = new wasm.Group(device);
    try {
      expect(deviceCommitterRank(group, device)).toBe(0);
      expect(group.ownCommitterRank(account)).toBeUndefined();
      expect(deviceCommitterRank(group, null)).toBeUndefined();
    } finally {
      group.free();
      device.free();
      account.free();
    }
  });
});
