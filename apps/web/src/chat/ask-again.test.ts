// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { ASK_AGAIN_AFTER_MS, askAgainState, sendRequestAgain, type ResendClient } from "./ask-again";

const NOW = 1_800_000_000_000;
const DAY = 24 * 60 * 60 * 1000;

function b64(bytes: number[]): string {
  return btoa(String.fromCharCode(...bytes));
}

function fakeClient(): {
  client: ResendClient<string>;
  calls: Array<{ kind: string; args: unknown[] }>;
} {
  const calls: Array<{ kind: string; args: unknown[] }> = [];
  return {
    calls,
    client: {
      requestToJoinSealed: (...args) => {
        calls.push({ kind: "sealed", args });
        return Promise.resolve();
      },
      requestToJoinFromLink: (...args) => {
        calls.push({ kind: "link", args });
        return Promise.resolve({});
      },
    },
  };
}

describe("askAgainState (UX 6.2 / 15.2.2, amended 2026-11-12)", () => {
  it("is a day", () => {
    expect(ASK_AGAIN_AFTER_MS).toBe(DAY);
  });

  it("under a day waits with no control", () => {
    expect(askAgainState(NOW, NOW)).toBe("waiting");
    expect(askAgainState(NOW - DAY + 1, NOW)).toBe("waiting");
    // A clock that went backwards reads as just sent.
    expect(askAgainState(NOW + 5000, NOW)).toBe("waiting");
  });

  it("a day or more reads No answer yet with Ask again", () => {
    expect(askAgainState(NOW - DAY, NOW)).toBe("no-answer");
    expect(askAgainState(NOW - 7 * DAY, NOW)).toBe("no-answer");
  });
});

describe("sendRequestAgain", () => {
  it("a request from a link goes through the link again, with its ledger id", async () => {
    const { client, calls } = fakeClient();
    const account = new Uint8Array([9]);
    await sendRequestAgain(
      client,
      "me",
      { idB64: b64([1, 2]), knockKeyB64: b64([3]), knockVersion: 2, capabilityWireB64: b64([7, 8]), blobIdB64: b64([5]) },
      NOW,
      account,
    );
    expect(calls).toHaveLength(1);
    expect(calls[0]?.kind).toBe("link");
    const [identity, wire, nowMs, opts] = calls[0]!.args as [string, Uint8Array, number, { blobId: Uint8Array; joinerAccountId: Uint8Array }];
    expect(identity).toBe("me");
    expect([...wire]).toEqual([7, 8]);
    expect(nowMs).toBe(NOW);
    expect([...opts.blobId]).toEqual([5]);
    expect(opts.joinerAccountId).toBe(account);
  });

  it("a request from search is knocked sealed to the group's key", async () => {
    const { client, calls } = fakeClient();
    await sendRequestAgain(client, "me", { idB64: b64([1, 2]), knockKeyB64: b64([3, 4]), knockVersion: 6 }, NOW);
    expect(calls).toHaveLength(1);
    expect(calls[0]?.kind).toBe("sealed");
    const [identity, groupId, key, version] = calls[0]!.args as [string, Uint8Array, Uint8Array, number];
    expect(identity).toBe("me");
    expect([...groupId]).toEqual([1, 2]);
    expect([...key]).toEqual([3, 4]);
    expect(version).toBe(6);
  });

  it("a failure propagates, so the caller keeps the old record", async () => {
    const client: ResendClient<string> = {
      requestToJoinSealed: () => Promise.reject(new Error("offline")),
      requestToJoinFromLink: () => Promise.reject(new Error("offline")),
    };
    await expect(
      sendRequestAgain(client, "me", { idB64: b64([1]), knockKeyB64: b64([3]), knockVersion: 1 }, NOW),
    ).rejects.toThrow("offline");
  });
});
