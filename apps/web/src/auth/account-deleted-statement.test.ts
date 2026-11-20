// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 1 — the statement is sent after the auth deletion and
 * before the wipe, never before the auth deletion, and a failure never stops
 * the wipe.
 */
import { describe, expect, it } from "vitest";

import { runAccountDeletion } from "./account-deletion";
import {
  ACCOUNT_DELETED_CLEANUP_LABEL,
  bindingSigner,
  buildStatementCleanups,
  isStatementConversation,
  type StatementTarget,
} from "./account-deleted-statement";

const SEED = new Uint8Array(32).fill(7);
const compose = (seed: Uint8Array, groupId: Uint8Array): Uint8Array => new Uint8Array([seed[0] ?? 0, groupId[0] ?? 0]);

function target(id: number, events: string[], fail = false): StatementTarget {
  return {
    groupId: new Uint8Array([id]),
    publish: async (plaintext) => {
      events.push(`statement:${plaintext[1]}`);
      if (fail) throw new Error("publish failed");
    },
  };
}

const quiet = { log: () => undefined, sleep: async () => undefined };

describe("sending the statement during an account deletion", () => {
  it("goes after the auth deletion and before the wipe, to every conversation", async () => {
    const events: string[] = [];
    await runAccountDeletion({
      ...quiet,
      deleteAtAuth: async () => {
        events.push("auth");
      },
      cleanups: async () => buildStatementCleanups([target(1, events), target(2, events)], SEED, compose),
      wipe: async () => {
        events.push("wipe");
      },
    });
    expect(events[0]).toBe("auth");
    expect(events.at(-1)).toBe("wipe");
    expect(events.filter((e) => e.startsWith("statement:")).sort()).toEqual(["statement:1", "statement:2"]);
  });

  it("is never sent when auth refuses the deletion", async () => {
    const events: string[] = [];
    await expect(
      runAccountDeletion({
        ...quiet,
        deleteAtAuth: async () => {
          throw new Error("recall window");
        },
        cleanups: async () => buildStatementCleanups([target(1, events)], SEED, compose),
        wipe: async () => {
          events.push("wipe");
        },
      }),
    ).rejects.toThrow("recall window");
    expect(events).toEqual([]);
  });

  it("a failed post is logged by its label only and the wipe still runs", async () => {
    const events: string[] = [];
    const logged: string[] = [];
    await runAccountDeletion({
      sleep: async () => undefined,
      log: (_m, label) => {
        logged.push(label);
      },
      deleteAtAuth: async () => undefined,
      cleanups: async () => buildStatementCleanups([target(1, events, true)], SEED, compose),
      wipe: async () => {
        events.push("wipe");
      },
    });
    expect(events.at(-1)).toBe("wipe");
    expect(logged).toContain(ACCOUNT_DELETED_CLEANUP_LABEL);
  });

  it("sends nothing without a 32-byte signing seed", () => {
    expect(buildStatementCleanups([target(1, [])], new Uint8Array(16), compose)).toEqual([]);
  });
});

describe("what the statement is signed with and where it goes", () => {
  const key = (b: number): Uint8Array => new Uint8Array(32).fill(b);
  it("the identity whose key the conversation leaves are bound under", () => {
    const a = { signaturePublicKey: key(1), name: "current" };
    const b = { signaturePublicKey: key(2), name: "conversation" };
    expect(bindingSigner(key(2), [a, b])?.name).toBe("conversation");
    expect(bindingSigner(key(3), [a, b])).toBeNull();
    expect(bindingSigner(undefined, [a])).toBeNull();
  });

  it("one-to-ones and groups that are still active; not a group without a kind", () => {
    expect(isStatementConversation({ groupKind: () => 1, isActive: true })).toBe(true);
    expect(isStatementConversation({ groupKind: () => 2, isActive: true })).toBe(true);
    expect(isStatementConversation({ groupKind: () => undefined, isActive: true })).toBe(false);
    expect(isStatementConversation({ groupKind: () => 1, isActive: false })).toBe(false);
  });
});
