// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S4a — the pending-membership-request surface
 * ([`docs/ux/proposal-based-membership.md`]).
 *
 * `routes/Chat.tsx` cannot be tested at all — `vitest.config.ts` collects
 * `src/**\/*.test.ts` in a plain `node` environment with no jsdom and no Solid
 * JSX transform — so every UX rule that could otherwise hide in a component
 * lives in `pending-requests.ts` and is pinned here.
 *
 * ## Which of these actually discriminate (MEASURED — each probe was run)
 *
 * 1. **`a refused proposal is not a pending request`** — with the
 *    `if (!record.stored) continue;` guard removed from `foldPendingRequests`:
 *
 *        AssertionError: expected [ { key: 'remove:4', …(7) } ] to have a length
 *        of +0 but got 1
 *
 *    A dropped proposal will never be committed by any sweep, so rendering it
 *    as a pending request would tell the user a request is live when it is
 *    dead.
 *
 * 2. **`a plain member sees only their own request`** — with `visibleRequests`
 *    returning everything:
 *
 *        AssertionError: expected [ 'remove:4', …(1) ] to deeply equal [ Array(1) ]
 *          [
 *        +   "remove:4",
 *            "add:oaGhoaGhoaGhoaGh…",
 *          ]
 *
 *    The leaked row is the removal window ADR-0045 created: someone else's
 *    pending eviction, shown to a member who cannot act on it.
 *
 * 3. **`nothing renders inside the quiet window`** — with `requestState`
 *    returning `"waiting"` unconditionally, three tests went red:
 *
 *        AssertionError: expected 'waiting' to be 'pending' // Object.is equality
 *        AssertionError: expected { …(2) } to be null
 *        AssertionError: expected [ { …(8) } ] to have a length of +0 but got 1
 *
 * 4. **`the FIRST observation of a key wins`** — with `recordObservation`
 *    overwriting `atMs`:
 *
 *        AssertionError: expected 1060000 to be 1000000 // Object.is equality
 *
 *    A request that keeps being re-sent would never mature past the window and
 *    would stay invisible forever — precisely the wait the notice exists for.
 *
 * BOUNDARY GUARD, not a discriminator: the exact-copy assertions. They pass
 * with every rule above neutered; their job is to fail when the wording drifts
 * away from the spec, and in particular when the blameless single-line liveness
 * sentence acquires a second, escalated variant that the deleted `stalled`
 * state took with it.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";

/** The `localStorage` shim convention this repo already uses
 *  (`contacts/local-store.test.ts`). */
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

beforeEach(() => {
  (globalThis as unknown as { localStorage: unknown }).localStorage =
    new MemoryStorage();
});

afterEach(() => {
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
});

import {
  QUIET_WINDOW_MS,
  clearObservationsForAccount,
  foldPendingRequests,
  loadObservations,
  pendingNotice,
  proposalKey,
  pruneObservations,
  recordObservation,
  requestState,
  rowAnnotation,
  visibleRequests,
  waitingToBeAdded,
  type ProposalRecordView,
  type RequestObservation,
} from "./pending-requests";

const ACCOUNT = "acct-a";
const GROUP = "grp-1";
const ALICE = new Uint8Array(32).fill(0xa1);

function record(over: Partial<ProposalRecordView> = {}): ProposalRecordView {
  return {
    kind: "remove",
    senderLeaf: 1,
    stored: true,
    addSignatureKey: undefined,
    removeTargetLeaf: 4,
    ...over,
  };
}

const T0 = 1_000_000;

describe("ADR-0045 — the quiet window", () => {
  it("nothing renders inside the quiet window", () => {
    expect(requestState(T0, T0)).toBe("pending");
    expect(requestState(T0, T0 + QUIET_WINDOW_MS - 1)).toBe("pending");
  });

  it("renders once the window has elapsed", () => {
    expect(requestState(T0, T0 + QUIET_WINDOW_MS)).toBe("waiting");
    expect(requestState(T0, T0 + 10 * QUIET_WINDOW_MS)).toBe("waiting");
  });

  it("an UNOBSERVED request is `waiting` at once, not `pending` forever", () => {
    // Restored from a snapshot rather than seen to land, so it has necessarily
    // been pending since before this session — by definition past any window.
    // Treating it as `pending` would hide a genuinely old request behind a timer
    // that never fires.
    expect(requestState(undefined, T0)).toBe("waiting");
  });
});

describe("ADR-0045 — the fold", () => {
  it("a refused proposal is not a pending request", () => {
    const { requests, liveKeys } = foldPendingRequests(
      [record({ stored: false })],
      new Map(),
      T0,
    );
    expect(requests).toHaveLength(0);
    expect(liveKeys).toHaveLength(0);
  });

  it("keys an add on the signature key and a remove on the leaf", () => {
    expect(proposalKey(record({ kind: "add", addSignatureKey: ALICE }))).toBe(
      `add:${Buffer.from(ALICE).toString("base64")}`,
    );
    expect(proposalKey(record({ kind: "remove", removeTargetLeaf: 4 }))).toBe(
      "remove:4",
    );
    // A role change collapses to ONE key: `classify` discards the extensions,
    // so two of them are genuinely indistinguishable to a receiver.
    expect(proposalKey(record({ kind: "role-change" }))).toBe("role-change");
  });

  it("joins the observation store for the age and the own-ness", () => {
    const obs = new Map<string, RequestObservation>([
      ["remove:4", { atMs: T0, own: true }],
    ]);
    const { requests } = foldPendingRequests([record()], obs, T0 + QUIET_WINDOW_MS);
    expect(requests[0]).toMatchObject({
      key: "remove:4",
      kind: "remove",
      targetLeaf: 4,
      own: true,
      state: "waiting",
    });
  });
});

describe("ADR-0045 UX §10 — visibility", () => {
  const OTHERS_REMOVAL = record({ kind: "remove", removeTargetLeaf: 4 });
  const MY_ADD = record({ kind: "add", addSignatureKey: ALICE });

  function fold(canManage: boolean): string[] {
    const obs = new Map<string, RequestObservation>([
      [proposalKey(MY_ADD), { atMs: T0, own: true }],
      [proposalKey(OTHERS_REMOVAL), { atMs: T0, own: false }],
    ]);
    const { requests } = foldPendingRequests(
      [OTHERS_REMOVAL, MY_ADD],
      obs,
      T0 + QUIET_WINDOW_MS,
    );
    return visibleRequests(requests, canManage).map((r) => r.key);
  }

  it("an owner or admin sees the whole pending set", () => {
    expect(fold(true)).toEqual(["remove:4", proposalKey(MY_ADD)]);
  });

  it("a plain member sees only their own request", () => {
    // Not concealment from the TARGET — the receipt gate stores the proposal on
    // every member's device including theirs. The choice is only whether to
    // build a surface that broadcasts someone else's pending eviction to people
    // who cannot act on it.
    expect(fold(false)).toEqual([proposalKey(MY_ADD)]);
  });

  it("the own-request exception is what makes `Leave group` have feedback", () => {
    const mine = record({ kind: "remove", removeTargetLeaf: 9, senderLeaf: 9 });
    const obs = new Map<string, RequestObservation>([
      ["remove:9", { atMs: T0, own: true }],
    ]);
    const { requests } = foldPendingRequests([mine], obs, T0 + QUIET_WINDOW_MS);
    expect(visibleRequests(requests, false)).toHaveLength(1);
  });
});

describe("ADR-0045 UX §5 — the exact copy", () => {
  function waiting(kind: string, extra: Partial<ProposalRecordView> = {}) {
    const r = record({ kind, ...extra });
    const obs = new Map<string, RequestObservation>([
      [proposalKey(r), { atMs: T0, own: false }],
    ]);
    return foldPendingRequests([r], obs, T0 + QUIET_WINDOW_MS).requests;
  }

  it("renders no notice while nothing is past the window", () => {
    const r = record();
    const obs = new Map<string, RequestObservation>([
      [proposalKey(r), { atMs: T0, own: false }],
    ]);
    const { requests } = foldPendingRequests([r], obs, T0);
    expect(pendingNotice(requests)).toBeNull();
  });

  it("uses the singular and plural count lines", () => {
    expect(pendingNotice(waiting("remove"))?.count).toBe(
      "1 membership change is waiting to apply.",
    );
    const two = [
      ...waiting("remove"),
      ...waiting("add", { addSignatureKey: ALICE }),
    ];
    expect(pendingNotice(two)?.count).toBe(
      "2 membership changes are waiting to apply.",
    );
  });

  it("has ONE liveness line — blameless, personless, and true at any age", () => {
    // The escalated second variant went with the `stalled` state: ADR-0045
    // decision 2 gives a waiting request no deadline, so there is no moment at
    // which the client learns something new to say. It must also not name a
    // person: even under the beacon variant, a beacon licenses "someone is
    // active", never "this person is online".
    const line = pendingNotice(waiting("remove"))?.liveness;
    expect(line).toBe(
      "Membership changes are applied by the group owner or an admin. " +
        "They will apply automatically when one does.",
    );
    expect(line).not.toMatch(/still|stalled|no admin|offline|nobody/i);
  });

  it("annotates a member's own row `Waiting to leave.` and another's `Waiting to be removed.`", () => {
    const reqs = waiting("remove", { removeTargetLeaf: 4 });
    expect(rowAnnotation(reqs, 4, 9)).toBe("Waiting to be removed.");
    expect(rowAnnotation(reqs, 4, 4)).toBe("Waiting to leave.");
    expect(rowAnnotation(reqs, 5, 9)).toBeNull();
  });

  it("gives a pending ROLE CHANGE no row annotation — it has no target", () => {
    const reqs = waiting("role-change");
    expect(reqs).toHaveLength(1);
    for (let leaf = 0; leaf < 5; leaf++) {
      expect(rowAnnotation(reqs, leaf, 0)).toBeNull();
    }
    // It still contributes to the panel-level count.
    expect(pendingNotice(reqs)).not.toBeNull();
  });

  it("puts a pending ADD in its own section, never in the roster", () => {
    const reqs = waiting("add", { addSignatureKey: ALICE });
    expect(waitingToBeAdded(reqs)).toHaveLength(1);
    // …and it must not annotate any member row, because a requested person is
    // not a member.
    expect(rowAnnotation(reqs, 0, 0)).toBeNull();
  });

  it("hides a pending ADD until the quiet window elapses", () => {
    const r = record({ kind: "add", addSignatureKey: ALICE });
    const obs = new Map<string, RequestObservation>([
      [proposalKey(r), { atMs: T0, own: true }],
    ]);
    const { requests } = foldPendingRequests([r], obs, T0);
    expect(waitingToBeAdded(requests)).toHaveLength(0);
  });
});

describe("ADR-0045 — the observation store", () => {
  it("the FIRST observation of a key wins, so a re-send cannot reset the age", () => {
    recordObservation(ACCOUNT, GROUP, "remove:4", T0, false);
    recordObservation(ACCOUNT, GROUP, "remove:4", T0 + 60_000, false);
    const obs = loadObservations(ACCOUNT, GROUP);
    expect(obs.get("remove:4")?.atMs).toBe(T0);
    // Which is what keeps a repeatedly re-sent request from staying invisible.
    expect(requestState(obs.get("remove:4")?.atMs, T0 + QUIET_WINDOW_MS)).toBe(
      "waiting",
    );
  });

  it("upgrades `own` to true regardless of arrival order", () => {
    // The peer's echo of our own proposal can land before our local note of it.
    recordObservation(ACCOUNT, GROUP, "remove:4", T0, false);
    recordObservation(ACCOUNT, GROUP, "remove:4", T0 + 1, true);
    expect(loadObservations(ACCOUNT, GROUP).get("remove:4")?.own).toBe(true);
    // …and never downgrades it.
    recordObservation(ACCOUNT, GROUP, "remove:4", T0 + 2, false);
    expect(loadObservations(ACCOUNT, GROUP).get("remove:4")?.own).toBe(true);
  });

  it("prunes observations whose request the core no longer holds", () => {
    recordObservation(ACCOUNT, GROUP, "remove:4", T0, false);
    recordObservation(ACCOUNT, GROUP, "role-change", T0, true);
    pruneObservations(ACCOUNT, GROUP, ["role-change"]);
    const obs = loadObservations(ACCOUNT, GROUP);
    expect([...obs.keys()]).toEqual(["role-change"]);
  });

  it("is scoped per group, and purged whole per account", () => {
    recordObservation(ACCOUNT, GROUP, "remove:4", T0, false);
    recordObservation(ACCOUNT, "grp-2", "remove:9", T0, false);
    recordObservation("acct-b", GROUP, "remove:1", T0, false);

    expect(loadObservations(ACCOUNT, "grp-2").size).toBe(1);
    clearObservationsForAccount(ACCOUNT);
    expect(loadObservations(ACCOUNT, GROUP).size).toBe(0);
    expect(loadObservations(ACCOUNT, "grp-2").size).toBe(0);
    // Another account is untouched — `forgetDevice` hygiene must not reach it.
    expect(loadObservations("acct-b", GROUP).size).toBe(1);
  });

  it("a corrupt row reads as empty rather than throwing", () => {
    globalThis.localStorage.setItem(
      `lakko.pending-requests.v1.${ACCOUNT}.${GROUP}`,
      "{not json",
    );
    expect(loadObservations(ACCOUNT, GROUP).size).toBe(0);
    // Degrades to "render immediately", never to losing the request — the
    // request itself lives in the core's pending set, not here.
    expect(requestState(undefined, T0)).toBe("waiting");
  });
});
