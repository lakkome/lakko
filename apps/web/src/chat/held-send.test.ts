// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0065 R.7 case 1 - the held send (`held-send.ts`). */
import { describe, expect, it } from "vitest";

import {
  HELD_SEND_GIVE_UP_MS,
  HeldSendStore,
  heldVerdict,
  identityCheckDue,
  sendRoute,
  flushVerdict,
  isDeadSession,
  opRoute,
} from "./held-send";

describe("sendRoute", () => {
  it("holds a one-to-one whose other person was superseded", () => {
    expect(sendRoute({ kind: "chat", unrepairableKeyB64: "old-anna" })).toBe("hold");
  });
  it("sends in a one-to-one that is fine, and on the successor", () => {
    expect(sendRoute({ kind: "chat", unrepairableKeyB64: null })).toBe("send");
  });
  it("never holds a group: a member sends where its device is", () => {
    expect(sendRoute({ kind: "group", unrepairableKeyB64: "old-owner" })).toBe("send");
    expect(sendRoute({ kind: "group", unrepairableKeyB64: null })).toBe("send");
  });
});

describe("heldVerdict", () => {
  it("goes out as soon as the route allows", () => {
    expect(heldVerdict({ route: "send", heldAtMs: 0, nowMs: 10 * HELD_SEND_GIVE_UP_MS })).toBe("send");
  });
  it("waits, then gives up to the ordinary failed state", () => {
    expect(heldVerdict({ route: "hold", heldAtMs: 1000, nowMs: 1000 + HELD_SEND_GIVE_UP_MS - 1 })).toBe("wait");
    expect(heldVerdict({ route: "hold", heldAtMs: 1000, nowMs: 1000 + HELD_SEND_GIVE_UP_MS })).toBe("give-up");
  });
});

describe("HeldSendStore", () => {
  it("keeps messages per conversation in send order, and hands them over once", () => {
    const store = new HeldSendStore<string>();
    store.hold("c1", "first", 1);
    store.hold("c2", "other", 2);
    store.hold("c1", "second", 3);
    expect(store.count("c1")).toBe(2);
    expect(store.take("c1").map((h) => h.payload)).toEqual(["first", "second"]);
    expect(store.take("c1")).toEqual([]);
    expect(store.count("c2")).toBe(1);
    store.clear();
    expect(store.take("c2")).toEqual([]);
  });
});

describe("identityCheckDue", () => {
  it("runs when never checked or older than the interval", () => {
    expect(identityCheckDue(undefined, 5, 100)).toBe(true);
    expect(identityCheckDue(0, 100, 100)).toBe(true);
    expect(identityCheckDue(1, 100, 100)).toBe(false);
  });
});

describe("opRoute (an edit, a delete, a reaction in a superseded one-to-one)", () => {
  it("is applied locally and never sent where a message would be held", () => {
    expect(opRoute({ kind: "chat", unrepairableKeyB64: "old-key" })).toBe("local-only");
  });
  it("is sent everywhere else", () => {
    expect(opRoute({ kind: "chat", unrepairableKeyB64: null })).toBe("send");
    expect(opRoute({ kind: "group", unrepairableKeyB64: "owner-key" })).toBe("send");
    expect(opRoute({ kind: "group", unrepairableKeyB64: null })).toBe("send");
  });
});

describe("isDeadSession (the publish guard's predicate)", () => {
  it("a one-to-one whose other person's reset was accepted is dead", () => {
    expect(isDeadSession({ kind: "chat", unrepairableKeyB64: "old-key" })).toBe(true);
  });
  it("a live one-to-one and every group are not", () => {
    expect(isDeadSession({ kind: "chat", unrepairableKeyB64: null })).toBe(false);
    expect(isDeadSession({ kind: "group", unrepairableKeyB64: "owner-key" })).toBe(false);
  });
});

describe("flushVerdict (the background flush of held messages)", () => {
  const base = { held: 2, viewMounted: false, headActive: true, headRoute: "send" as const };
  it("flushes once the conversation's current group can take a message", () => {
    expect(flushVerdict(base)).toBe("flush");
  });
  it("waits while the current group is still the superseded session", () => {
    expect(flushVerdict({ ...base, headRoute: "hold" })).toBe("wait");
  });
  it("leaves them to a mounted view of the conversation", () => {
    expect(flushVerdict({ ...base, viewMounted: true })).toBe("wait");
  });
  it("waits when this device is not live in the current group, and with nothing held", () => {
    expect(flushVerdict({ ...base, headActive: false })).toBe("wait");
    expect(flushVerdict({ ...base, held: 0 })).toBe("wait");
  });
});

describe("HeldSendStore: what the flush and the reload need", () => {
  it("puts unsent messages back at the front, in order", () => {
    const store = new HeldSendStore<string>();
    store.hold("c", "a", 1);
    store.hold("c", "b", 2);
    const taken = store.take("c");
    store.hold("c", "typed-meanwhile", 3);
    store.putBack("c", taken.slice(1));
    expect(store.take("c").map((h) => h.payload)).toEqual(["b", "typed-meanwhile"]);
  });
  it("lists and replaces everything", () => {
    const store = new HeldSendStore<string>();
    store.replaceAll([
      ["c1", { heldAtMs: 1, payload: "x" }],
      ["c2", { heldAtMs: 2, payload: "y" }],
      ["c1", { heldAtMs: 3, payload: "z" }],
    ]);
    expect(store.all()).toEqual([
      ["c1", [{ heldAtMs: 1, payload: "x" }, { heldAtMs: 3, payload: "z" }]],
      ["c2", [{ heldAtMs: 2, payload: "y" }]],
    ]);
  });
});
