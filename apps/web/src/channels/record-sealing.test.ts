// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Seen live 2026-11-13: after a co-publisher approve (epoch 0 -> 1) and a
 * reload, the owner's corrected channel record rendered "This post isn't
 * available." on the owner and on a subscriber, while an ordinary post right
 * after it was readable. The record was sealed with the composer's seed
 * (epoch 1) under the channel's current epoch number, unknown after the
 * reload and so 0: no reader opens that. A record is sealed exactly as a post
 * is (`composerSeedFor`). Against the real wasm core: the published wire
 * names the epoch whose seed opens it, and the plaintext is a record.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import * as realWasm from "../../wasm/lakko_ffi_wasm";
import init from "../../wasm/lakko_ffi_wasm";

const published = vi.hoisted(() => ({ wires: [] as Uint8Array[] }));

vi.mock("../wasm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../wasm")>();
  return { ...actual, loadWasm: vi.fn(async () => realWasm) };
});
vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return {
    ...actual,
    publishPost: vi.fn(async (_id: string, wire: Uint8Array) => {
      published.wires.push(wire);
      return { cursor: 7, serverTs: "2026-11-13T10:15:00Z" };
    }),
  };
});

import { publishChannelMeta } from "./channel-meta";
import { channelsActions } from "../state/channels-state";
import type { WasmIdentity } from "./composer";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const ID = "1922757c76bb6d346b8255abfafe28fd";
const SEED0 = new Uint8Array(32).fill(0x10);
const SEED1 = new Uint8Array(32).fill(0x11);
const channelId = (): Uint8Array => Uint8Array.from(ID.match(/../g)!.map((h) => parseInt(h, 16)));

describe("a channel record is sealed as a post is", () => {
  it("after a commit and a reload (channel epoch not read yet), under the epoch its seed serves", async () => {
    published.wires.length = 0;
    // The reload: the composer's seed serves epoch 1, the epoch-0 seed is
    // kept, the channel's epoch is not known yet.
    channelsActions.setKeys(ID, { seed: SEED1.slice(), signingKeySeed: new Uint8Array(32).fill(2), seedEpoch: 1 });
    channelsActions.setSubscribedSeedForEpoch(ID, 0, SEED0.slice());
    channelsActions.updateAccess(ID, (r) => ({ ...r, visibility: "public" }));
    // The counter store wants a signed-in account; its value does not matter here.
    vi.spyOn(channelsActions, "nextCounter").mockReturnValue(1n);

    await publishChannelMeta(ID, { name: "Walk 28 channel", description: "", listed: true }, {} as WasmIdentity, 20n);

    expect(published.wires).toHaveLength(1);
    const post = realWasm.decodeChannelPost(published.wires[0]!);
    try {
      expect(post.epochId).toBe(1);
      // A reader holding the epoch-1 seed opens it, and it is a record.
      const plaintext = realWasm.verifyAndDecryptChannelPost(SEED1, channelId(), post);
      const c = realWasm.classifyChannelPostPlaintext(plaintext);
      try {
        expect(c.kind).toBe("meta");
        expect(c.listed).toBe(true);
      } finally {
        c.free();
      }
    } finally {
      post.free();
    }
  });
});
