// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `ChatClient.sendMessage` — persist-before-send ordering + the
 * fail-closed persist gate (AEAD nonce-reuse-on-crash fix).
 *
 * The bug: `createApplicationMessage` advances the in-memory MLS
 * sender ratchet (consumes generation N, derives the AEAD
 * key+nonce from it, moves to N+1). If the gen-N ciphertext is
 * published to the DS BEFORE the N+1 ratchet state is durably
 * persisted, a crash between the two reloads the ratchet at N and
 * the next send re-consumes generation N — an identical
 * (key, nonce) on a different plaintext.
 *
 * The fix: create → persist(durable, awaited, FAIL-CLOSED) →
 * publish. These tests pin (a) that the persist is awaited and
 * happens BEFORE the publish on the success path, and (b) that a
 * persist FAILURE aborts the send — `publishGroup` is NOT called
 * — and surfaces an error to the caller.
 */
import { describe, it, expect, vi } from "vitest";
import { ChatClient, ChatError } from "./client";
import type { DsClient } from "../ds/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

/** A group whose `createApplicationMessage` returns fixed wire
 *  bytes and records into the shared call-order log. Standing in
 *  for the real MLS group so the test needs no wasm. */
function mockGroup(order: string[]): WasmGroup {
  return {
    createApplicationMessage: vi.fn((_id: unknown, _pt: Uint8Array) => {
      order.push("create");
      return new Uint8Array([1, 2, 3]);
    }),
    get groupId(): Uint8Array {
      return new Uint8Array([9, 9, 9, 9]);
    },
  } as unknown as WasmGroup;
}

/** Canonical identity is only ever handed to the (mocked) group,
 *  so an opaque token is enough. */
const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

describe("ChatClient.sendMessage — persist-before-send", () => {
  it("awaits the durable persist BEFORE publishing (success path)", async () => {
    const order: string[] = [];
    const publishGroup = vi.fn(async () => {
      order.push("publish");
      return { id: 42, createdAt: "2026-07-23T00:00:00Z" };
    });
    const onPersist = vi.fn(async () => {
      order.push("persist");
    });
    const ds = { publishGroup } as unknown as DsClient;

    const client = new ChatClient(
      ds,
      { identityProvider: () => IDENTITY_TOKEN, onPersist },
    );

    const res = await client.sendMessage(
      null,
      mockGroup(order),
      new Uint8Array([7]),
    );

    // Ordering: ratchet advance → durable persist → publish.
    expect(order).toEqual(["create", "persist", "publish"]);
    expect(onPersist).toHaveBeenCalledTimes(1);
    expect(publishGroup).toHaveBeenCalledTimes(1);
    expect(res.messageId).toBe(42);
  });

  it("ABORTS the send when the persist fails — publishGroup is NOT called", async () => {
    const order: string[] = [];
    const publishGroup = vi.fn(async () => {
      order.push("publish");
      return { id: 1, createdAt: "2026-07-23T00:00:00Z" };
    });
    const persistError = new Error("IndexedDB write failed");
    const onPersist = vi.fn(async () => {
      order.push("persist-fail");
      throw persistError;
    });
    const ds = { publishGroup } as unknown as DsClient;
    const group = mockGroup(order);

    const client = new ChatClient(
      ds,
      { identityProvider: () => IDENTITY_TOKEN, onPersist },
    );

    await expect(
      client.sendMessage(null, group, new Uint8Array([7])),
    ).rejects.toBeInstanceOf(ChatError);

    // The gen-N ciphertext NEVER reached the DS — this is the
    // whole point of the fail-closed gate.
    expect(publishGroup).not.toHaveBeenCalled();
    // The ratchet did advance (create ran) and the persist was
    // attempted before the abort; publish was never reached.
    expect(order).toEqual(["create", "persist-fail"]);
    expect(onPersist).toHaveBeenCalledTimes(1);
  });

  it("surfaces the persist failure as a ChatError wrapping the cause", async () => {
    const publishGroup = vi.fn(async () => ({
      id: 1,
      createdAt: "2026-07-23T00:00:00Z",
    }));
    const persistError = new Error("disk full");
    const onPersist = vi.fn(async () => {
      throw persistError;
    });
    const ds = { publishGroup } as unknown as DsClient;

    const client = new ChatClient(
      ds,
      { identityProvider: () => IDENTITY_TOKEN, onPersist },
    );

    let caught: unknown;
    try {
      await client.sendMessage(null, mockGroup([]), new Uint8Array([7]));
    } catch (err) {
      caught = err;
    }
    expect(caught).toBeInstanceOf(ChatError);
    expect((caught as ChatError).message).toMatch(/nonce reuse/i);
    expect((caught as ChatError).cause).toBe(persistError);
    expect(publishGroup).not.toHaveBeenCalled();
  });

  it("still publishes when no onPersist is wired (degenerate/test path is a no-op persist)", async () => {
    const order: string[] = [];
    const publishGroup = vi.fn(async () => {
      order.push("publish");
      return { id: 5, createdAt: "2026-07-23T00:00:00Z" };
    });
    const ds = { publishGroup } as unknown as DsClient;

    // No onPersist: persistDurableOrThrow is a no-op that resolves,
    // so the send proceeds. Such callers persist nothing and have
    // no reload-from-IDB path, so they are outside the bug's blast
    // radius.
    const client = new ChatClient(
      ds,
      { identityProvider: () => IDENTITY_TOKEN },
    );

    const res = await client.sendMessage(
      null,
      mockGroup(order),
      new Uint8Array([7]),
    );
    expect(order).toEqual(["create", "publish"]);
    expect(res.messageId).toBe(5);
  });
});
