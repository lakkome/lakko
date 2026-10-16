// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0054 decision 1 — the group wake rides the signed DS publish.
 *
 * Pins:
 *   - a chat message passes `{ wake: { headerValue } }` to `publishGroup`,
 *     the sealed wake the sealer built for kind `message` (ADR-0078: the
 *     `push_gid` and the kind are inside the seal, never the DS group_id);
 *   - the wake is sealed BEFORE the message is sealed;
 *   - a control envelope passes no wake; a call frame opted in passes `call`;
 *   - no sealer, a `null` result, a failing sealer and a wrong-length header
 *     all publish WITHOUT a wake, and never fail the send (invariant #5:
 *     the group_id never stands in for the push_gid);
 *   - the client never makes a network request of its own to wake anybody —
 *     the DS is mocked, so any `fetch` at all would be a push-gw call.
 *
 * The group and DS are mocked so the test needs no wasm.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ChatClient } from "./client";
import type { DsClient, DsGroupPublishOptions } from "../ds/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const GROUP_ID = new Uint8Array([9, 9, 9, 9]);
/** A stand-in for a sealed wake: 312 characters naming its kind. */
const SEALED = (kind: string): string => (kind === "call" ? "c" : "m").repeat(312);
const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

function mockGroup(order: string[] = []): WasmGroup {
  return {
    createApplicationMessage: (_id: unknown, _pt: Uint8Array) => {
      order.push("create");
      return new Uint8Array([1, 2, 3]);
    },
    get groupId(): Uint8Array {
      return GROUP_ID;
    },
  } as unknown as WasmGroup;
}

type PublishGroup = (
  groupId: Uint8Array,
  payload: Uint8Array,
  options?: DsGroupPublishOptions,
) => Promise<{ id: number; createdAt: string }>;

function mockDs(order: string[] = []): {
  ds: DsClient;
  publishGroup: ReturnType<typeof vi.fn<PublishGroup>>;
} {
  const publishGroup = vi.fn<PublishGroup>(async () => {
    order.push("publish");
    return { id: 7, createdAt: "2026-09-27T00:00:00Z" };
  });
  return { ds: { publishGroup } as unknown as DsClient, publishGroup };
}

function wakeOf(
  publishGroup: ReturnType<typeof vi.fn<PublishGroup>>,
): DsGroupPublishOptions["wake"] {
  expect(publishGroup).toHaveBeenCalledTimes(1);
  return publishGroup.mock.calls[0]?.[2]?.wake;
}

let fetchSpy: ReturnType<typeof vi.fn>;
beforeEach(() => {
  fetchSpy = vi.fn(async () => new Response("{}", { status: 200 }));
  vi.stubGlobal("fetch", fetchSpy);
});
afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ChatClient — the wake is a header of the publish (ADR-0054)", () => {
  it("a message carries a sealed message wake", async () => {
    const { ds, publishGroup } = mockDs();
    const client = new ChatClient(ds, {
      identityProvider: () => IDENTITY_TOKEN,
      wakeSealer: async (_g: Uint8Array, kind: string) => SEALED(kind),
    });

    await client.sendMessage(null, mockGroup(), new Uint8Array([7]));

    const wake = wakeOf(publishGroup);
    expect(wake?.headerValue).toBe(SEALED("message"));
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("seals the wake before the message, and publishes last", async () => {
    const order: string[] = [];
    const { ds } = mockDs(order);
    const client = new ChatClient(ds, {
      identityProvider: () => IDENTITY_TOKEN,
      onPersist: async () => {
        order.push("persist");
      },
      wakeSealer: async (_g: Uint8Array, kind: string) => {
        order.push("resolve");
        return SEALED(kind);
      },
    });

    await client.sendMessage(null, mockGroup(order), new Uint8Array([7]));

    expect(order).toEqual(["resolve", "create", "persist", "publish"]);
  });

  it("a control envelope carries no wake and never consults the resolver", async () => {
    const { ds, publishGroup } = mockDs();
    const resolver = vi.fn(async () => SEALED("message"));
    const client = new ChatClient(ds, {
      identityProvider: () => IDENTITY_TOKEN,
      wakeSealer: resolver,
    });

    await client.broadcastControl(null, mockGroup(), new Uint8Array([7]));

    expect(wakeOf(publishGroup)).toBeNull();
    expect(resolver).not.toHaveBeenCalled();
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("a control frame opted in as a call carries a call wake", async () => {
    const { ds, publishGroup } = mockDs();
    const client = new ChatClient(ds, {
      identityProvider: () => IDENTITY_TOKEN,
      wakeSealer: async (_g: Uint8Array, kind: string) => SEALED(kind),
    });

    await client.broadcastControl(null, mockGroup(), new Uint8Array([7]), { wake: "call" });

    const wake = wakeOf(publishGroup);
    expect(wake?.headerValue).toBe(SEALED("call"));
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("no resolver wired: no wake, and never the group_id in its place", async () => {
    const { ds, publishGroup } = mockDs();
    const client = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN });

    const res = await client.sendMessage(null, mockGroup(), new Uint8Array([7]));

    expect(wakeOf(publishGroup)).toBeNull();
    expect(res.messageId).toBe(7);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("the resolver yields null (no secret held yet): no wake", async () => {
    const { ds, publishGroup } = mockDs();
    const client = new ChatClient(ds, {
      identityProvider: () => IDENTITY_TOKEN,
      wakeSealer: async () => null,
    });

    await client.sendMessage(null, mockGroup(), new Uint8Array([7]));

    expect(wakeOf(publishGroup)).toBeNull();
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("a failing resolver never fails the send; it publishes without a wake", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { ds, publishGroup } = mockDs();
    const client = new ChatClient(ds, {
      identityProvider: () => IDENTITY_TOKEN,
      wakeSealer: async () => {
        throw new Error("IndexedDB unavailable");
      },
    });

    const res = await client.sendMessage(null, mockGroup(), new Uint8Array([7]));

    expect(res.messageId).toBe(7);
    expect(wakeOf(publishGroup)).toBeNull();
    warn.mockRestore();
  });

  it("a wrong-length sealed wake is dropped rather than failing the publish", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { ds, publishGroup } = mockDs();
    const client = new ChatClient(ds, {
      identityProvider: () => IDENTITY_TOKEN,
      wakeSealer: async () => "x".repeat(16),
    });

    await client.sendMessage(null, mockGroup(), new Uint8Array([7]));

    expect(wakeOf(publishGroup)).toBeNull();
    warn.mockRestore();
  });
});
