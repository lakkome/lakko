// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one decision both one-to-one senders run (`one-to-one-send-gate.ts`):
 * the open chat view and the outbox runner. Real module; the directory check
 * is a recorded fake.
 */
import { afterEach, describe, expect, it } from "vitest";

import { HELD_SEND_GIVE_UP_MS } from "./held-send";
import {
  checkPeerIdentityIfDue,
  oneToOnePeerKey,
  oneToOneSendVerdict,
  oneToOneUnrepairableKey,
  resetPeerIdentityChecksForTest,
} from "./one-to-one-send-gate";

afterEach(() => resetPeerIdentityChecksForTest());

describe("oneToOneSendVerdict", () => {
  it("reads the route AFTER the identity check: a reset accepted by the check holds the message", async () => {
    let superseded = false;
    const order: string[] = [];
    const verdict = await oneToOneSendVerdict({
      check: async () => {
        order.push("check");
        superseded = true;
      },
      unrepairableKey: () => {
        order.push("route");
        return superseded ? "peer" : null;
      },
      heldAtMs: 1_000,
      now: () => 1_000,
    });
    expect(order).toEqual(["check", "route"]);
    expect(verdict).toBe("wait");
  });

  it("does not wait past the bound for a check that does not answer", async () => {
    const started = Date.now();
    const verdict = await oneToOneSendVerdict({
      check: () => new Promise<void>(() => {}),
      unrepairableKey: () => null,
      heldAtMs: started,
      waitMs: 20,
    });
    expect(verdict).toBe("send");
    expect(Date.now() - started).toBeLessThan(2_000);
  });

  it("gives up a message held past the give-up", async () => {
    const verdict = await oneToOneSendVerdict({
      check: async () => {},
      unrepairableKey: () => "peer",
      heldAtMs: 0,
      now: () => HELD_SEND_GIVE_UP_MS,
    });
    expect(verdict).toBe("give-up");
  });
});

describe("the peer and the unrepairable key", () => {
  it("the peer is the one roster person who is not this account", () => {
    expect(oneToOnePeerKey(["me", "you"], "me")).toBe("you");
    expect(oneToOnePeerKey(["me"], "me")).toBeNull();
    expect(oneToOnePeerKey(["me", "a", "b"], "me")).toBeNull();
  });

  it("a superseded peer makes the session unrepairable; this account never does", () => {
    expect(oneToOneUnrepairableKey({ rosterPersonKeysB64: ["me", "you"], ownPersonKeyB64: "me", supersededKeys: new Set(["you"]) })).toBe("you");
    expect(oneToOneUnrepairableKey({ rosterPersonKeysB64: ["me", "you"], ownPersonKeyB64: "me", supersededKeys: new Set(["me"]) })).toBeNull();
  });
});

describe("checkPeerIdentityIfDue", () => {
  it("is a no-op without a peer, and never throws on a failed check", async () => {
    const calls: string[] = [];
    await checkPeerIdentityIfDue({ conversationHex: "aa", peerKeyB64: null, check: async () => calls.push("x") });
    expect(calls).toEqual([]);
  });
});
