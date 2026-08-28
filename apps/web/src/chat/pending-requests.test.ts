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
 * 5. **`resolves a leave's target to the SENDER's own leaf`** — this one pins a
 *    defect that WAS live: `foldPendingRequests` read `record.removeTargetLeaf`
 *    straight into `targetLeaf`, and the core populates that field for a
 *    `Remove` only (`ProposalKind::Leave => ("leave", None, None)` in
 *    `crates/lakko-ffi-wasm/src/mls.rs`), so a leave's `targetLeaf` was always
 *    `undefined` and `rowAnnotation`'s `r.targetLeaf === leafIndex` could never
 *    match one. The `Waiting to leave.` copy the UX spec §5.2 requires was
 *    unreachable dead code. With the old expression restored, three tests go
 *    red:
 *
 *        FAIL … > a `leave` carries no target on the wire > resolves a leave's
 *        target to the SENDER's own leaf
 *        AssertionError: expected undefined to be 4 // Object.is equality
 *
 *        FAIL … > annotates `Waiting to leave.` for self and `Waiting to be
 *        removed.` for another — for a LEAVE
 *        AssertionError: expected null to be 'Waiting to leave.'
 *
 *        FAIL … > keys a leave on the sender leaf, which is the same leaf it
 *        now targets
 *        AssertionError: expected undefined to be 4 // Object.is equality
 *
 *    and with the over-eager fix `record.senderLeaf ?? 0` instead, the fourth:
 *
 *        FAIL … > a leave with an unresolvable sender targets nobody — not
 *        leaf 0, not anybody
 *        AssertionError: expected +0 to be undefined
 *
 *    i.e. `Waiting to leave.` on the OWNER's row for a request nobody made.
 *
 * BOUNDARY GUARD, not a discriminator: the exact-copy assertions, and the
 * `…— for a REMOVE` twin of the leave annotation case (it passed before the fix
 * and after it; its job is to prove the two kinds now behave identically rather
 * than that one of them changed). They pass
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

  // ADR-0045 UX §6.1.10 — the successor's key on OUR OWN record. It is the only
  // thing that tells `role-change` the transfer from `role-change` the promote,
  // because `ProposalKind::RoleChange` carries no payload and `proposalKey`
  // collapses both onto the one key.
  it("carries the successor key on our own transfer record, and prunes it with the request", () => {
    recordObservation(ACCOUNT, GROUP, "role-change", T0, true, "c3VjYw==");
    expect(loadObservations(ACCOUNT, GROUP).get("role-change")?.transferToB64).toBe(
      "c3VjYw==",
    );
    const { requests } = foldPendingRequests(
      [record({ kind: "role-change", removeTargetLeaf: undefined })],
      loadObservations(ACCOUNT, GROUP),
      T0 + QUIET_WINDOW_MS,
    );
    expect(requests[0]?.transferToB64).toBe("c3VjYw==");
    // A swept transfer takes its successor key with it — a stale key would make
    // an unrelated LATER role change name somebody nobody proposed.
    pruneObservations(ACCOUNT, GROUP, []);
    expect(loadObservations(ACCOUNT, GROUP).get("role-change")).toBeUndefined();
  });

  it("a later observation of the same key does not erase the successor", () => {
    // The receive loop observes our own request landing back off the journal,
    // and that observation carries no successor. First-wins, like `atMs`.
    recordObservation(ACCOUNT, GROUP, "role-change", T0, true, "c3VjYw==");
    recordObservation(ACCOUNT, GROUP, "role-change", T0 + 60_000, false);
    const seen = loadObservations(ACCOUNT, GROUP).get("role-change");
    expect(seen?.transferToB64).toBe("c3VjYw==");
    expect(seen?.own).toBe(true);
  });

  it("first-wins, so a second successor for the same key never overwrites", () => {
    // C-65 permits exactly ONE pending role change per group and `proposalKey`
    // collapses every role change onto the one key, so the FIRST record is the
    // one that matches what the core actually holds. This pins the `??` order
    // explicitly: the previous test cannot, because its second call supplies no
    // successor and both orders agree there.
    //
    // Reaching this at all needs a torn `pendingProposals()` read to have let a
    // second transfer past the client's C-65 pre-check, which is contrived —
    // but the label must never name somebody the user did not pick for the
    // request that is actually pending, and first-wins is what `atMs` already
    // does for the same reason.
    recordObservation(ACCOUNT, GROUP, "role-change", T0, true, "Zmlyc3Q=");
    recordObservation(ACCOUNT, GROUP, "role-change", T0 + 60_000, true, "c2Vjb25k");
    expect(loadObservations(ACCOUNT, GROUP).get("role-change")?.transferToB64).toBe(
      "Zmlyc3Q=",
    );
  });

  it("an ordinary promote records NO successor, so it is not read as a transfer", () => {
    recordObservation(ACCOUNT, GROUP, "role-change", T0, true);
    expect(
      loadObservations(ACCOUNT, GROUP).get("role-change")?.transferToB64,
    ).toBeUndefined();
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

describe("ADR-0045 — a `leave` carries no target on the wire", () => {
  /**
   * The core's `ProposalKind::Leave` is RFC 9420 `SelfRemove`, which names no
   * target at all: `crates/lakko-ffi-wasm/src/mls.rs` maps it to
   * `("leave", None, None)`, and `commit_effects` in
   * `crates/lakko-mls/src/group_proposals.rs` says why — "`SelfRemove` names no
   * target: the removed leaf is the sender's own."
   *
   * So `removeTargetLeaf` is `undefined` for every leave, and reading it
   * straight into `PendingRequest.targetLeaf` made `rowAnnotation`'s
   * `r.targetLeaf === leafIndex` test unsatisfiable for a leave — the
   * `Waiting to leave.` copy the UX spec §5.2 requires was dead code.
   */
  function leave(senderLeaf: number | undefined): ProposalRecordView {
    return record({ kind: "leave", senderLeaf, removeTargetLeaf: undefined });
  }

  function waitingReqs(records: ProposalRecordView[]) {
    const obs = new Map<string, RequestObservation>(
      records.map((r) => [proposalKey(r), { atMs: T0, own: false }] as const),
    );
    return foldPendingRequests(records, obs, T0 + QUIET_WINDOW_MS).requests;
  }

  it("resolves a leave's target to the SENDER's own leaf", () => {
    const [req] = waitingReqs([leave(4)]);
    expect(req?.kind).toBe("leave");
    expect(req?.senderLeaf).toBe(4);
    expect(req?.targetLeaf).toBe(4);
  });

  it("annotates `Waiting to leave.` for self and `Waiting to be removed.` for another — for a REMOVE", () => {
    const reqs = waitingReqs([record({ kind: "remove", removeTargetLeaf: 4 })]);
    expect(rowAnnotation(reqs, 4, 4)).toBe("Waiting to leave.");
    expect(rowAnnotation(reqs, 4, 9)).toBe("Waiting to be removed.");
  });

  it("annotates `Waiting to leave.` for self and `Waiting to be removed.` for another — for a LEAVE", () => {
    // This is the row the UX spec asks for and that never rendered: a member
    // who pressed `Leave group` must see the status of their own request.
    const reqs = waitingReqs([leave(4)]);
    expect(rowAnnotation(reqs, 4, 4)).toBe("Waiting to leave.");
    expect(rowAnnotation(reqs, 4, 9)).toBe("Waiting to be removed.");
    expect(rowAnnotation(reqs, 5, 5)).toBeNull();
  });

  it("a leave with an unresolvable sender targets nobody — not leaf 0, not anybody", () => {
    // `undefined` must not collapse onto a real leaf. An unresolvable sender is
    // always unauthorised in the core anyway, so annotating leaf 0 would put
    // `Waiting to leave.` on the OWNER's row for a request nobody made.
    const [req] = waitingReqs([leave(undefined)]);
    expect(req?.targetLeaf).toBeUndefined();
    const reqs = waitingReqs([leave(undefined)]);
    for (let leaf = 0; leaf < 5; leaf++) {
      expect(rowAnnotation(reqs, leaf, leaf)).toBeNull();
      expect(rowAnnotation(reqs, leaf, 9)).toBeNull();
    }
  });

  it("keys a leave on the sender leaf, which is the same leaf it now targets", () => {
    // `proposalKey` already keyed a leave on its sender; resolving `targetLeaf`
    // to that same leaf makes the two consistent rather than introducing a
    // second notion of "which leaf does this leave concern".
    const r = leave(4);
    expect(proposalKey(r)).toBe("leave:4");
    expect(waitingReqs([r])[0]?.targetLeaf).toBe(4);
  });
});
