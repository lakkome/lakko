// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S7 residual 1 — a request that left the pending set without landing.
 *
 * `routes/Chat.tsx` cannot be tested (`vitest.config.ts` runs plain `node`, no
 * jsdom, no Solid transform), so the whole rule lives in
 * `unapplied-requests.ts` and is pinned here — the same split
 * `pending-requests.ts` already uses.
 *
 * ## Which of these discriminate (MEASURED — each probe was RUN and reverted)
 *
 * Recorded beside each test.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { RequestObservation } from "./pending-requests";
import {
  clearUnappliedForAccount,
  dismissUnapplied,
  loadUnapplied,
  recordUnapplied,
  unappliedAddRequests,
  unappliedLine,
  JOIN_REQUEST_AGAIN,
  JOIN_WAIT_PRIMARY,
  JOIN_WAIT_SECONDARY,
  UNAPPLIED_EXPLANATION,
} from "./unapplied-requests";

/** The `localStorage` shim convention this repo already uses
 *  (`chat/pending-requests.test.ts`). */
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

const ACCOUNT = "acct";
const GROUP = "grp";
const JOINER = "am9pbmVyLWtleQ=="; // arbitrary base64
const CONTACT = "Y29udGFjdC1rZXk=";

function own(extra?: Partial<RequestObservation>): RequestObservation {
  return { atMs: 1_000, own: true, ...extra };
}

describe("detecting a request that was not carried", () => {
  /**
   * The whole point. The approving admin's own `add:` observation is gone from
   * the core's live pending set — the sweep emptied it — and the joiner is not
   * in the roster, so the request died without a trace anywhere else.
   *
   * DISCRIMINATION (RUN, then reverted): with the roster check inverted
   * (`roster.has` -> `!roster.has`) two tests went red:
   *
   *     AssertionError: expected [] to have a length of 1 but got +0
   *     AssertionError: expected [ { …(2) } ] to deeply equal []
   */
  it("reports an own add whose key left the pending set without reaching the roster", () => {
    const out = unappliedAddRequests(
      new Map([[`add:${JOINER}`, own({ joinRequest: true })]]),
      [],
      [],
    );
    expect(out).toHaveLength(1);
    expect(out[0]).toEqual({ addSignatureKeyB64: JOINER, viaJoinRequest: true });
  });

  /**
   * The single most important false positive to exclude: the request WORKED.
   *
   * DISCRIMINATION (RUN, then reverted): dropping the roster membership test
   * entirely made this red:
   *
   *     AssertionError: expected [ { …(2) } ] to deeply equal []
   */
  it("says nothing when the add landed — the key is in the roster", () => {
    expect(
      unappliedAddRequests(
        new Map([[`add:${JOINER}`, own({ joinRequest: true })]]),
        [],
        [JOINER],
      ),
    ).toEqual([]);
  });

  /** Still pending is not "not carried" — the sweep has not run yet. */
  it("says nothing while the request is still in the core's pending set", () => {
    expect(
      unappliedAddRequests(
        new Map([[`add:${JOINER}`, own()]]),
        [`add:${JOINER}`],
        [],
      ),
    ).toEqual([]);
  });

  /**
   * UX §10 — a member is never told about someone else's request. A foreign
   * proposal is observed with `own: false` (the receive loop records it for the
   * quiet window), and its disappearance is not this device's business.
   *
   * DISCRIMINATION (RUN, then reverted): with the `own` guard removed:
   *
   *     AssertionError: expected [ { …(2) } ] to deeply equal []
   */
  it("ignores a request this device did not make", () => {
    expect(
      unappliedAddRequests(
        new Map([[`add:${JOINER}`, { atMs: 1_000, own: false }]]),
        [],
        [],
      ),
    ).toEqual([]);
  });

  /**
   * The deliberate scope limit, pinned so nobody widens it by accident.
   *
   * `remove:` and `leave:` keys name a LEAF, and OpenMLS hands a vacated leaf to
   * the next `Add` — so a roster test on a leaf can be wrong in both directions.
   * A `role-change` key carries no payload at all, so there is nothing to test.
   * All three are also VISIBLE when they fail: the person you tried to remove is
   * still in the roster, and a group you failed to leave is still in your list.
   * The add is the only request whose failure leaves no trace, and the only one
   * whose effect test is keyed on a signature key rather than a leaf.
   */
  it("claims nothing about a remove, a leave or a role change", () => {
    expect(
      unappliedAddRequests(
        new Map([
          ["remove:4", own()],
          ["leave:2", own()],
          ["role-change", own()],
          ["update:3", own()],
        ]),
        [],
        [],
      ),
    ).toEqual([]);
  });

  /** A key with no signature key behind it (`add:?`) names nobody, so there is
   *  nothing honest to say about it. */
  it("skips an add whose signature key was never resolved", () => {
    expect(unappliedAddRequests(new Map([["add:?", own()]]), [], [])).toEqual([]);
  });

  /**
   * A torn roster read is UNKNOWN, never "nobody is in the group" — the same
   * fail-closed direction `membershipRotationDecision` takes with the identical
   * signal.
   *
   * DISCRIMINATION (RUN, then reverted): with the `null` guard removed and
   * `null` coerced to `[]`:
   *
   *     AssertionError: expected [ { …(2) } ] to deeply equal []
   */
  it("claims nothing when the roster could not be read", () => {
    expect(
      unappliedAddRequests(
        new Map([[`add:${JOINER}`, own({ joinRequest: true })]]),
        [],
        null,
      ),
    ).toEqual([]);
  });
});

describe("the copy", () => {
  /**
   * A literal pin on the two strings, so a re-wording is a deliberate edit
   * rather than a drift.
   *
   * DISCRIMINATION (RUN, then reverted): with the join line replaced by the UX
   * spec §7 draft:
   *
   *     AssertionError: expected 'This request expired before it could …' to be
   *     'Not applied. Ask them to request to j…' // Object.is equality
   */
  it("is exactly these two strings", () => {
    expect(unappliedLine({ addSignatureKeyB64: JOINER, viaJoinRequest: true })).toBe(
      "Not applied. Ask them to request to join again.",
    );
    expect(unappliedLine({ addSignatureKeyB64: CONTACT, viaJoinRequest: false })).toBe(
      "Not added. You can add them again.",
    );
  });

  /**
   * The rule, asserted independently of the literals above so it survives a
   * re-wording. The core reports no reason for an exclusion, so no string here
   * may claim one.
   *
   * DISCRIMINATION (RUN, then reverted): with the join line replaced by the UX
   * spec §7 draft `This request expired before it could be applied. Ask them to
   * request to join again.` this went red — which is the finding, because
   * "expired" is a REASON this client cannot establish:
   *
   *     AssertionError: expected 'this request expired before it could …' not to
   *     match /expired|refused|rejected|invalid|fail…/
   */
  it("never states a reason", () => {
    for (const s of [
      unappliedLine({ addSignatureKeyB64: JOINER, viaJoinRequest: true }),
      unappliedLine({ addSignatureKeyB64: CONTACT, viaJoinRequest: false }),
      UNAPPLIED_EXPLANATION(),
    ]) {
      expect(s.toLowerCase()).not.toMatch(
        /expired|refused|rejected|invalid|failed|error|because/,
      );
    }
  });

  /** A join approval is terminal on the admin side: a successful approve
   *  consumes the retained request (`Group::approve_external_join` drops it once
   *  the proposal is on its way), and only the joiner holds the key material. So
   *  the copy asks THEM to request again rather than offering a retry. */
  it("directs a join approval back to the joiner, and an ordinary add to us", () => {
    expect(unappliedLine({ addSignatureKeyB64: JOINER, viaJoinRequest: true })).toContain(
      "request to join again",
    );
    expect(unappliedLine({ addSignatureKeyB64: CONTACT, viaJoinRequest: false })).toContain(
      "You can add them again",
    );
  });
});

describe("the joiner's half", () => {
  /**
   * The finding, pinned: the joiner cannot be TOLD anything, so what it gets is
   * the truth about the wait plus the one repair available to it. Both lines
   * must state the unbounded wait; neither may claim a cause.
   *
   * DISCRIMINATION (RUN, then reverted): with `JOIN_WAIT_SECONDARY` reverted to
   * the pre-change copy `Request sent. Waiting for an admin to approve, then
   * for the change to be applied to the group.` this went red on the
   * time-limit assertion:
   *
   *     AssertionError: expected 'Request sent. Waiting for an admin to …' to
   *     contain 'time limit'
   */
  it("states that neither step is bounded, and offers the one repair", () => {
    expect(JOIN_WAIT_PRIMARY()).toBe(
      "Request sent. An admin approves it, then the change is applied to the group.",
    );
    expect(JOIN_WAIT_SECONDARY()).toContain("time limit");
    expect(JOIN_WAIT_SECONDARY()).toContain("send the request again");
    expect(JOIN_REQUEST_AGAIN()).toBe("Request again");
  });

  /** The same no-reason rule as the group side. The joiner knows even less than
   *  the admin does, so it may claim even less. */
  it("never states a reason either", () => {
    for (const s of [JOIN_WAIT_PRIMARY(), JOIN_WAIT_SECONDARY(), JOIN_REQUEST_AGAIN()]) {
      expect(s.toLowerCase()).not.toMatch(
        /expired|refused|rejected|invalid|failed|error|because/,
      );
    }
  });
});

describe("the store", () => {
  it("round-trips, is idempotent, and survives a re-record", () => {
    recordUnapplied(ACCOUNT, GROUP, [
      { addSignatureKeyB64: JOINER, viaJoinRequest: true },
    ]);
    recordUnapplied(ACCOUNT, GROUP, [
      { addSignatureKeyB64: JOINER, viaJoinRequest: true },
    ]);
    const loaded = loadUnapplied(ACCOUNT, GROUP);
    expect([...loaded.values()]).toEqual([
      { addSignatureKeyB64: JOINER, viaJoinRequest: true },
    ]);
  });

  /**
   * DISCRIMINATION (RUN, then reverted): with `dismissUnapplied` a no-op, this
   * and the resurrection test both went red:
   *
   *     AssertionError: expected 2 to be 1 // Object.is equality
   *     AssertionError: expected 1 to be +0 // Object.is equality
   */
  it("dismisses one row without touching the other", () => {
    recordUnapplied(ACCOUNT, GROUP, [
      { addSignatureKeyB64: JOINER, viaJoinRequest: true },
      { addSignatureKeyB64: CONTACT, viaJoinRequest: false },
    ]);
    dismissUnapplied(ACCOUNT, GROUP, JOINER);
    const loaded = loadUnapplied(ACCOUNT, GROUP);
    expect(loaded.size).toBe(1);
    expect(loaded.get(CONTACT)).toBeDefined();
  });

  /**
   * A dismissed row must STAY dismissed: the detection re-runs on every fold,
   * and re-recording what the user has already read would make the notice
   * un-dismissable.
   *
   * DISCRIMINATION (RUN, then reverted): with `recordUnapplied` overwriting a
   * dismissed row instead of honouring the tombstone:
   *
   *     AssertionError: expected 1 to be +0 // Object.is equality
   */
  it("does not resurrect a dismissed row on the next detection pass", () => {
    recordUnapplied(ACCOUNT, GROUP, [
      { addSignatureKeyB64: JOINER, viaJoinRequest: true },
    ]);
    dismissUnapplied(ACCOUNT, GROUP, JOINER);
    recordUnapplied(ACCOUNT, GROUP, [
      { addSignatureKeyB64: JOINER, viaJoinRequest: true },
    ]);
    expect(loadUnapplied(ACCOUNT, GROUP).size).toBe(0);
  });

  it("is scoped per account and per group", () => {
    recordUnapplied(ACCOUNT, GROUP, [
      { addSignatureKeyB64: JOINER, viaJoinRequest: true },
    ]);
    expect(loadUnapplied(ACCOUNT, "other").size).toBe(0);
    expect(loadUnapplied("other", GROUP).size).toBe(0);
  });

  /** `recover` / `forgetDevice` hygiene — the same discipline the observation
   *  store and the commit outbox follow. */
  it("clears every group for one account and leaves other accounts alone", () => {
    recordUnapplied(ACCOUNT, GROUP, [
      { addSignatureKeyB64: JOINER, viaJoinRequest: true },
    ]);
    recordUnapplied(ACCOUNT, "g2", [
      { addSignatureKeyB64: CONTACT, viaJoinRequest: false },
    ]);
    recordUnapplied("other", GROUP, [
      { addSignatureKeyB64: CONTACT, viaJoinRequest: false },
    ]);
    clearUnappliedForAccount(ACCOUNT);
    expect(loadUnapplied(ACCOUNT, GROUP).size).toBe(0);
    expect(loadUnapplied(ACCOUNT, "g2").size).toBe(0);
    expect(loadUnapplied("other", GROUP).size).toBe(1);
  });

  it("reads a corrupt row as empty rather than throwing", () => {
    globalThis.localStorage.setItem(
      `lakko.unapplied-requests.v1.${ACCOUNT}.${GROUP}`,
      "{not json",
    );
    expect(loadUnapplied(ACCOUNT, GROUP).size).toBe(0);
  });
});
