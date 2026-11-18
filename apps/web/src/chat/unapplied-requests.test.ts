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
 * Since ADR-0050 section 2.5a a row is written only when the automatic
 * re-request of an own request STOPS (`chat/membership-retry.ts`); the
 * detection of a superseded request lives there and is tested in
 * `membership-retry.test.ts`. This file pins the rows, their copy and the store.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  clearUnappliedForAccount,
  dismissUnapplied,
  loadUnapplied,
  recordUnapplied,
  unappliedFromRequest,
  unappliedLine,
  type UnappliedRequest,
  JOIN_WAIT_PRIMARY,
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

const joinRow: UnappliedRequest = unappliedFromRequest({
  kind: "approve-join",
  subjectKeyB64: JOINER,
});
const addRow: UnappliedRequest = unappliedFromRequest({
  kind: "add",
  subjectKeyB64: CONTACT,
  subjectAccountIdB64: "YWNjdA==",
});

describe("a row per own request kind (ADR-0050 2.5a point 4)", () => {
  /**
   * The row is keyed on `kind:subject`, so an add and an approve of the same
   * person, or a promote and a demote, are separate rows rather than one
   * overwriting the other; the subject is always a KEY, never a leaf index.
   */
  it("maps every own request kind to its row", () => {
    const S = CONTACT;
    expect(unappliedFromRequest({ kind: "remove", subjectKeyB64: S })).toEqual({
      key: `remove:${S}`,
      kind: "remove",
      subjectKeyB64: S,
    });
    expect(unappliedFromRequest({ kind: "leave", subjectKeyB64: S }).kind).toBe("leave");
    expect(
      unappliedFromRequest({ kind: "set-admin", subjectKeyB64: S, makeAdmin: true }).kind,
    ).toBe("make-admin");
    expect(
      unappliedFromRequest({ kind: "set-admin", subjectKeyB64: S, makeAdmin: false }).kind,
    ).toBe("remove-admin");
    expect(
      unappliedFromRequest({ kind: "transfer-ownership", subjectKeyB64: S, outgoing: "admin" })
        .kind,
    ).toBe("transfer-ownership");
    expect(
      unappliedFromRequest({ kind: "admit-device", subjectKeyB64: S, subjectAccountIdB64: "eA==" })
        .key,
    ).toBe(`admit-device:${S}`);
    expect(unappliedFromRequest({ kind: "retire-device", subjectKeyB64: S }).kind).toBe(
      "retire-device",
    );
    expect(joinRow.key).toBe(`approve-join:${JOINER}`);
    expect(addRow.key).toBe(`add:${CONTACT}`);
  });
});

describe("the copy", () => {
  /**
   * A literal pin on every string, so a re-wording is a deliberate edit rather
   * than a drift. The strings match the Android client's.
   */
  it("is exactly these strings", () => {
    const line = (r: Parameters<typeof unappliedFromRequest>[0]): string =>
      unappliedLine(unappliedFromRequest(r));
    expect(unappliedLine(joinRow)).toBe("Not applied. Ask them to request to join again.");
    expect(unappliedLine(addRow)).toBe("Not added.");
    expect(line({ kind: "remove", subjectKeyB64: CONTACT })).toBe("Not removed.");
    expect(line({ kind: "leave", subjectKeyB64: CONTACT })).toBe(
      "Not applied. You are still a member.",
    );
    expect(line({ kind: "set-admin", subjectKeyB64: CONTACT, makeAdmin: true })).toBe(
      "Not made an admin.",
    );
    expect(line({ kind: "set-admin", subjectKeyB64: CONTACT, makeAdmin: false })).toBe(
      "Still an admin.",
    );
    expect(
      line({ kind: "transfer-ownership", subjectKeyB64: CONTACT, outgoing: "member" }),
    ).toBe("Ownership was not transferred.");
    expect(
      line({ kind: "admit-device", subjectKeyB64: CONTACT, subjectAccountIdB64: "eA==" }),
    ).toBe("Device not added to this group.");
    expect(line({ kind: "retire-device", subjectKeyB64: CONTACT })).toBe(
      "Device not removed from this group.",
    );
  });

  /**
   * The rule, asserted independently of the literals above so it survives a
   * re-wording. The core reports no reason for an exclusion, so no string here
   * may claim one — and a row exists because a re-request STOPPED, usually for
   * lost authority, so no string may offer "you can do it again" either
   * (except the join row, whose action belongs to the joiner).
   */
  it("never states a reason and never promises a retry", () => {
    const rows: UnappliedRequest[] = [
      joinRow,
      addRow,
      unappliedFromRequest({ kind: "remove", subjectKeyB64: CONTACT }),
      unappliedFromRequest({ kind: "leave", subjectKeyB64: CONTACT }),
      unappliedFromRequest({ kind: "set-admin", subjectKeyB64: CONTACT, makeAdmin: true }),
      unappliedFromRequest({ kind: "transfer-ownership", subjectKeyB64: CONTACT, outgoing: "admin" }),
      unappliedFromRequest({ kind: "retire-device", subjectKeyB64: CONTACT }),
    ];
    for (const s of [...rows.map(unappliedLine), UNAPPLIED_EXPLANATION()]) {
      expect(s.toLowerCase()).not.toMatch(
        /expired|refused|rejected|invalid|failed|error|because/,
      );
    }
    for (const r of rows.filter((r) => r.kind !== "approve-join")) {
      expect(unappliedLine(r)).not.toMatch(/again/);
    }
  });

  /** A join approval is terminal on the admin side: a successful approve
   *  consumes the retained request, and only the joiner holds the key
   *  material. So the copy asks THEM to request again. */
  it("directs a join approval back to the joiner", () => {
    expect(unappliedLine(joinRow)).toContain("request to join again");
  });
});

describe("the joiner's half", () => {
  /**
   * The joiner cannot be TOLD anything (no honest channel), so the page says
   * only what is true of the wait: the request was sent and an admin sees it
   * when they next open Lakko. Amended 2026-11-12: no second line and no
   * `Request again`; `Ask again` comes after a day (`./ask-again.ts`).
   */
  it("says what the wait is, in one line", () => {
    expect(JOIN_WAIT_PRIMARY()).toBe("Request sent. An admin will see it when they next open Lakko.");
  });

  /** The same no-reason rule as the group side. The joiner knows even less than
   *  the admin does, so it may claim even less. */
  it("never states a reason either", () => {
    expect(JOIN_WAIT_PRIMARY().toLowerCase()).not.toMatch(
      /expired|refused|rejected|invalid|failed|error|because/,
    );
  });
});

describe("the store", () => {
  it("round-trips, is idempotent, and survives a re-record", () => {
    recordUnapplied(ACCOUNT, GROUP, [joinRow]);
    recordUnapplied(ACCOUNT, GROUP, [joinRow]);
    const loaded = loadUnapplied(ACCOUNT, GROUP);
    expect([...loaded.values()]).toEqual([joinRow]);
  });

  it("dismisses one row without touching the other", () => {
    recordUnapplied(ACCOUNT, GROUP, [joinRow, addRow]);
    dismissUnapplied(ACCOUNT, GROUP, joinRow.key);
    const loaded = loadUnapplied(ACCOUNT, GROUP);
    expect(loaded.size).toBe(1);
    expect(loaded.get(addRow.key)).toEqual(addRow);
  });

  /**
   * A dismissed row must STAY dismissed: the same request can stop again after
   * a later re-request, and re-recording what the user has already read would
   * make the notice un-dismissable.
   */
  it("does not resurrect a dismissed row when the same request stops again", () => {
    recordUnapplied(ACCOUNT, GROUP, [joinRow]);
    dismissUnapplied(ACCOUNT, GROUP, joinRow.key);
    recordUnapplied(ACCOUNT, GROUP, [joinRow]);
    expect(loadUnapplied(ACCOUNT, GROUP).size).toBe(0);
  });

  it("is scoped per account and per group", () => {
    recordUnapplied(ACCOUNT, GROUP, [joinRow]);
    expect(loadUnapplied(ACCOUNT, "other").size).toBe(0);
    expect(loadUnapplied("other", GROUP).size).toBe(0);
  });

  /** `recover` / `forgetDevice` hygiene — the same discipline the observation
   *  store and the commit outbox follow. */
  it("clears every group for one account and leaves other accounts alone", () => {
    recordUnapplied(ACCOUNT, GROUP, [joinRow]);
    recordUnapplied(ACCOUNT, "g2", [addRow]);
    recordUnapplied("other", GROUP, [addRow]);
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

  /** A row whose key does not name its own kind and subject is a forged or
   *  stale row and is dropped rather than rendered against the wrong person. */
  it("drops a row whose key disagrees with its content", () => {
    globalThis.localStorage.setItem(
      `lakko.unapplied-requests.v1.${ACCOUNT}.${GROUP}`,
      JSON.stringify({ [`remove:${JOINER}`]: { kind: "remove", subjectKeyB64: CONTACT } }),
    );
    expect(loadUnapplied(ACCOUNT, GROUP).size).toBe(0);
  });
});
