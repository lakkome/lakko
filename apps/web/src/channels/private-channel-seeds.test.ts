// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * lakko-security F1 and F5 (2026-11-15).
 *
 * F1: subscriber rows the channels service lists are marked as readers only
 * when this device switches a channel it knew was public to private. A
 * channel created private starts with the mark at 0 whatever rows the
 * service lists, and an owner device that holds no admission seed for a
 * private channel neither mints one nor marks rows: it waits for the
 * account's admission-seed register ("needs-owner").
 *
 * F5: New channel never founds under a kept id that names a channel the
 * account knows (a create that finished): the id is forgotten and a fresh one
 * is taken.
 */
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { ChannelAccessRecord } from "./channel-access-record";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm, wasmSync: () => wasm }));

const h = vi.hoisted(() => ({
  access: new Map<string, unknown>(),
  keys: new Map<string, unknown>(),
  listed: [] as { id: string }[],
  registerIds: new Set<string>(),
}));

vi.mock("../state/channels-state", async (importOriginal) => {
  const real = await importOriginal<typeof import("../state/channels-state")>();
  const { emptyAccessRecord } = await import("./channel-access-record");
  const get = (id: string): ChannelAccessRecord => (h.access.get(id) as ChannelAccessRecord | undefined) ?? emptyAccessRecord();
  return {
    ...real,
    awaitChannelsHydration: async () => undefined,
    channelsState: {
      ...real.channelsState,
      keysByChannel: () => h.keys,
      channels: () => h.listed,
      accessByChannel: () => h.access,
      metaByChannel: () => new Map(),
    },
    channelsActions: {
      ...real.channelsActions,
      accessOf: get,
      addChannel: vi.fn(),
      setKeys: vi.fn(),
      setEpoch: vi.fn(),
      persistPublisherGroupDurable: vi.fn(async () => undefined),
      updateAccess: (id: string, f: (r: ChannelAccessRecord) => ChannelAccessRecord) => h.access.set(id, f(get(id))),
      updateAccessDurable: async (id: string, f: (r: ChannelAccessRecord) => ChannelAccessRecord) => {
        h.access.set(id, f(get(id)));
      },
      coPublisherGroupIdOf: () => undefined,
    },
  };
});

vi.mock("./channel-subscription-register", async (importOriginal) => {
  const real = await importOriginal<typeof import("./channel-subscription-register")>();
  return {
    ...real,
    channelSubscriptionOf: (id: string) =>
      h.registerIds.has(id)
        ? { channelIdHex: id, subscribed: true, role: "owner", publisherGroupIdHex: "", ownerAccountKey: new Uint8Array() }
        : undefined,
  };
});

const api = vi.hoisted(() => ({
  createChannel: vi.fn(async () => undefined),
  listSubscriberPage: vi.fn(async () => ({
    entries: [
      { seq: 3, subscriberPubkeyHex: "aa".repeat(32) },
      { seq: 9, subscriberPubkeyHex: "bb".repeat(32) },
    ],
    nextAfter: 9,
  })),
  putChannelAccess: vi.fn(async () => undefined),
}));
vi.mock("./api", async (importOriginal) => {
  const real = await importOriginal<typeof import("./api")>();
  return { ...real, ...api };
});

const KEYS = { seed: new Uint8Array(32).fill(1), signingKeySeed: new Uint8Array(32).fill(2), seedEpoch: 0 };
const founded = vi.hoisted(() => ({ ids: [] as string[] }));
vi.mock("./composer", async (importOriginal) => {
  const real = await importOriginal<typeof import("./composer")>();
  return {
    ...real,
    freshPublisherKeys: vi.fn(async (_identity: unknown, idHex: string) => {
      founded.ids.push(idHex);
      return KEYS;
    }),
  };
});
vi.mock("../chat/group-handle", async (importOriginal) => {
  const real = await importOriginal<typeof import("../chat/group-handle")>();
  return {
    ...real,
    claimChannelHandle: vi.fn(async () => ({ handle: "pixel_pub", isPublic: false, leaseUntilMs: 0, certificate: null })),
  };
});
vi.mock("../chat/handle-owed", async (importOriginal) => {
  const real = await importOriginal<typeof import("../chat/handle-owed")>();
  return { ...real, noteHandleWrite: vi.fn(async () => undefined) };
});
vi.mock("./channel-meta", async (importOriginal) => {
  const real = await importOriginal<typeof import("./channel-meta")>();
  return { ...real, publishChannelMeta: vi.fn(async () => undefined) };
});

import {
  channelCreateFinished,
  ChannelError,
  createChannelFull,
  privateChannelAdmissionSeed,
  privateSwitchMarksRows,
} from "./channel-access";
import { channelIdForHandle } from "./claimed-handles";
import { MemoryStorage } from "../test/fake-storage";
import type { WasmIdentity } from "./composer";

beforeAll(async () => {
  const p = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(p) });
});

beforeEach(() => {
  vi.stubGlobal("localStorage", new MemoryStorage());
  h.access.clear();
  h.keys.clear();
  h.listed = [];
  h.registerIds.clear();
  founded.ids = [];
  api.listSubscriberPage.mockClear();
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

const ACCOUNT = "AAAAAAAAAAAAAAAAAAAAAA==";
const identity = {} as WasmIdentity;

describe("F1: a private channel's rows", () => {
  it("creation marks nothing, even with rows listed at the service", async () => {
    const id = await createChannelFull(identity, identity, ACCOUNT, {
      name: "Pixel Pub",
      description: "",
      handle: "pixel_pub",
      visibility: "private",
      topic: 0,
      language: "",
      photo: null,
    });
    const r = h.access.get(id) as ChannelAccessRecord;
    expect(api.listSubscriberPage).not.toHaveBeenCalled();
    expect(r.admittedThroughSeq).toBe(0);
    expect(r.admissionSeeds).toHaveLength(1);
    expect(r.visibility).toBe("private");
  });

  it("an owner device without seeds for an existing private channel neither mints nor marks", () => {
    const id = "5c".repeat(16);
    h.keys.set(id, KEYS);
    h.registerIds.add(id);
    h.access.set(id, { ...(h.access.get(id) ?? {}), ...emptyPrivate() });
    let caught: unknown;
    try {
      privateChannelAdmissionSeed(id);
    } catch (err) {
      caught = err;
    }
    expect(caught).toBeInstanceOf(ChannelError);
    expect((caught as ChannelError).code).toBe("needs-owner");
    expect(api.listSubscriberPage).not.toHaveBeenCalled();
    const r = h.access.get(id) as ChannelAccessRecord;
    expect(r.admittedThroughSeq).toBe(0);
    expect(r.admissionSeeds).toEqual([]);
    expect(r.admissionMarkSet).toBeUndefined();
  });

  it("a held seed is the one links are made under", () => {
    const id = "5d".repeat(16);
    const seedB64 = btoa(String.fromCharCode(...new Uint8Array(32).fill(4)));
    h.access.set(id, { ...emptyPrivate(), admissionSeeds: [{ version: 2, seedB64 }] });
    expect(privateChannelAdmissionSeed(id)).toEqual({ version: 2, seedB64 });
  });

  it("only a known public channel switched to private marks today's rows", () => {
    expect(privateSwitchMarksRows("public")).toBe(true);
    expect(privateSwitchMarksRows(null)).toBe(false);
    expect(privateSwitchMarksRows("private")).toBe(false);
  });
});

describe("F5: New channel never founds under a finished channel's id", () => {
  it("a kept id the account's register names is forgotten and a fresh id is taken", async () => {
    // A create that finished kept this id (a crash before it was forgotten);
    // the channel now lives in the account's register.
    const kept = channelIdForHandle(ACCOUNT, "pixel_pub", "77".repeat(16));
    h.registerIds.add(kept);
    expect(channelCreateFinished(kept)).toBe(true);
    const id = await createChannelFull(identity, identity, ACCOUNT, {
      name: "Pixel Pub",
      description: "",
      handle: "pixel_pub",
      visibility: "private",
      topic: 0,
      language: "",
      photo: null,
    });
    expect(id).not.toBe(kept);
    expect(founded.ids).toEqual([id]);
  });

  it("a kept id of a create that never finished is used again", async () => {
    const kept = channelIdForHandle(ACCOUNT, "pixel_pub", "78".repeat(16));
    expect(channelCreateFinished(kept)).toBe(false);
    const id = await createChannelFull(identity, identity, ACCOUNT, {
      name: "Pixel Pub",
      description: "",
      handle: "pixel_pub",
      visibility: "private",
      topic: 0,
      language: "",
      photo: null,
    });
    expect(id).toBe(kept);
  });
});

function emptyPrivate(): ChannelAccessRecord {
  return {
    visibility: "private",
    handle: null,
    handleCertB64: null,
    handleEnvelopeB64: null,
    topic: null,
    language: null,
    avatar: null,
    readKeyB64: null,
    waiting: false,
    admissionSeeds: [],
    inviteLinks: [],
    namedPublishers: [],
    publishers: [],
    owedType: null,
    admittedThroughSeq: 0,
    publisherKeys: [],
  };
}
