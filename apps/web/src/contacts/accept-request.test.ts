// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Accepting a contact request leaves one of three things, never a contact
 * with no chat. The group and the network are stood in for; the address book
 * is the real one.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { acceptContactRequest, acceptKeepsRequest } from "./accept-request";
import type { IncomingContactRequest } from "./client";
import { bindContactsAccount, loadAccepted } from "./local-store";

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, String(value));
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return Array.from(this.map.keys())[i] ?? null;
  }
}

const ACCOUNT = "Y2FyYS1hY2NvdW50";

const request = (over: Partial<IncomingContactRequest> = {}): IncomingContactRequest =>
  ({
    messageId: 7,
    fromPubkey: new Uint8Array(32).fill(0xa1),
    fromAccountId: new Uint8Array(16).fill(0xa2),
    fromPseudonym: new Uint8Array(16).fill(0xa3),
    replyGrantB64: "REPLY",
    ...over,
  }) as IncomingContactRequest;

function setup(over: Record<string, unknown> = {}) {
  const free = vi.fn();
  const group = { free, groupId: new Uint8Array(32).fill(1) };
  const client = {
    prepareGroupWith: vi.fn(async () => ({ group, welcome: new Uint8Array([1]), targets: [] })),
    recordOwedAnswer: vi.fn(async () => ({ id: "entry" })),
    settleOwedAnswer: vi.fn(async () => "settled"),
    sendContactAck: vi.fn(async () => ({ messageId: 1, createdAt: "" })),
    ...over,
  };
  const chats = {
    add: vi.fn(() => ({ groupIdB64: "GROUP", group })),
    persistHeld: vi.fn(async () => undefined),
  };
  const profiles = { groupJoined: vi.fn() };
  return {
    free,
    client,
    chats,
    profiles,
    deps: {
      client,
      identity: {},
      accountIdB64: ACCOUNT,
      chats,
      profiles,
    } as unknown as Parameters<typeof acceptContactRequest>[0],
  };
}

const offline = (): Promise<never> => Promise.reject(new Error("offline"));

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(ACCOUNT);
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
  vi.restoreAllMocks();
});

describe("acceptContactRequest", () => {
  it("UX 16.11: the person's Accept adopts the new account BEFORE the chat is founded", async () => {
    const adoptStartedOverRequest = vi.fn(() => true);
    const s = setup({ adoptStartedOverRequest });
    const req = request();
    const outcome = await acceptContactRequest(s.deps, req);
    expect(outcome.status).toBe("done");
    expect(adoptStartedOverRequest).toHaveBeenCalledTimes(1);
    expect(adoptStartedOverRequest).toHaveBeenCalledWith(req.fromAccountId, req.fromPubkey);
    // The identity gate inside the founding must see the moved pin.
    expect(adoptStartedOverRequest.mock.invocationCallOrder[0]).toBeLessThan(
      s.client.prepareGroupWith.mock.invocationCallOrder[0]!,
    );
  });

  it("a request with no account id adopts nothing", async () => {
    const adoptStartedOverRequest = vi.fn(() => true);
    const s = setup({ adoptStartedOverRequest });
    await acceptContactRequest(s.deps, request({ fromAccountId: null }));
    expect(adoptStartedOverRequest).not.toHaveBeenCalled();
  });

  it("keeps the answer owed before the group is shown, and is done when it landed", async () => {
    const s = setup();
    const outcome = await acceptContactRequest(s.deps, request());
    expect(outcome.status).toBe("done");
    expect(outcome.groupIdB64).toBe("GROUP");
    expect(loadAccepted()[0]?.groupIdB64).toBe("GROUP");
    expect(s.client.recordOwedAnswer.mock.invocationCallOrder[0]).toBeLessThan(
      s.chats.add.mock.invocationCallOrder[0]!,
    );
    expect(s.chats.persistHeld.mock.invocationCallOrder[0]).toBeLessThan(
      s.client.settleOwedAnswer.mock.invocationCallOrder[0]!,
    );
    expect(acceptKeepsRequest(outcome)).toBe(false);
  });

  it("a group that could not be founded leaves the request listed and no contact", async () => {
    const s = setup({ prepareGroupWith: offline });
    const outcome = await acceptContactRequest(s.deps, request());
    expect(outcome).toMatchObject({ status: "failed", groupIdB64: null });
    expect(acceptKeepsRequest(outcome)).toBe(true);
    expect(loadAccepted()).toEqual([]);
    expect(s.chats.add).not.toHaveBeenCalled();
    expect(s.client.sendContactAck).not.toHaveBeenCalled();
  });

  it("an answer that could not be kept founds nothing either", async () => {
    const s = setup({ recordOwedAnswer: offline });
    const outcome = await acceptContactRequest(s.deps, request());
    expect(acceptKeepsRequest(outcome)).toBe(true);
    expect(s.free).toHaveBeenCalledTimes(1);
    expect(s.chats.add).not.toHaveBeenCalled();
    expect(loadAccepted()).toEqual([]);
  });

  it("a Welcome that failed stays owed: a contact with the chat, the request gone", async () => {
    const s = setup({ settleOwedAnswer: async () => "owed" });
    const outcome = await acceptContactRequest(s.deps, request());
    expect(outcome).toMatchObject({ status: "owed", groupIdB64: "GROUP" });
    expect(acceptKeepsRequest(outcome)).toBe(false);
    expect(loadAccepted()[0]?.groupIdB64).toBe("GROUP");
    // The ack is the settling step's, after the Welcome.
    expect(s.client.sendContactAck).not.toHaveBeenCalled();
  });

  it("Accept works again after a failure", async () => {
    const prepareGroupWith = vi
      .fn()
      .mockImplementationOnce(offline)
      .mockResolvedValue({ group: { free: () => {} }, welcome: new Uint8Array(), targets: [] });
    const s = setup({ prepareGroupWith });
    expect((await acceptContactRequest(s.deps, request())).status).toBe("failed");
    expect((await acceptContactRequest(s.deps, request())).status).toBe("done");
  });

  it("a legacy request (no account id) can found no one-to-one, so it never makes a contact", async () => {
    const failed = setup({ sendContactAck: offline });
    expect((await acceptContactRequest(failed.deps, request({ fromAccountId: null }))).status).toBe("failed");
    expect(loadAccepted()).toEqual([]);

    const ok = setup();
    const outcome = await acceptContactRequest(ok.deps, request({ fromAccountId: null }));
    expect(outcome).toMatchObject({ status: "legacy", groupIdB64: null });
    expect(loadAccepted()).toEqual([]);
  });

  it("an Accept that cannot found the one-to-one makes no contact and keeps the request; Try again founds it", async () => {
    let calls = 0;
    const prepareGroupWith = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw new Error("ds down");
      return { group: { free: () => {} }, welcome: new Uint8Array(), targets: [] };
    });
    const s = setup({ prepareGroupWith } as never);
    const first = await acceptContactRequest(s.deps, request());
    expect(first.status).toBe("failed");
    expect(acceptKeepsRequest(first)).toBe(true);
    expect(loadAccepted()).toEqual([]);
    const second = await acceptContactRequest(s.deps, request());
    expect(acceptKeepsRequest(second)).toBe(false);
    expect(loadAccepted()).toHaveLength(1);
    expect(loadAccepted()[0]?.groupIdB64).toBeDefined();
  });
});
