// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/channel-notifications.md` section 2 against the real wasm rule:
 * what a channel read the post sync made notifies, and the cursor kept.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { ChannelPostSlot, PersistedChannelPost } from "./channel-post-log";
import { channelReadKind, decideChannelRead, type ChannelNotifyFacts, type ChannelRead } from "./channel-notify";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const NOW = 1_800_000_000_000;
const ID = "ab".repeat(16);

function post(cursor: number, ageMs = 1_000, text = `post ${cursor}`): PersistedChannelPost {
  return { cursor, serverTsMs: NOW - ageMs, text };
}

function slot(cursor: number, posts: PersistedChannelPost[]): ChannelPostSlot {
  return { cursor, posts };
}

function read(before: ChannelPostSlot | undefined, after: ChannelPostSlot, readMarker: number | undefined): ChannelRead {
  return { channelIdHex: ID, slotBefore: before, slotAfter: after, readMarker };
}

const facts = (over: Partial<ChannelNotifyFacts> = {}): ChannelNotifyFacts => ({
  muted: false,
  openAndVisible: false,
  own: false,
  lastNotified: 0,
  nowMs: NOW,
  ...over,
});

describe("channelReadKind", () => {
  it("tells an ordinary read, a first read after subscribing and a refetch apart", () => {
    expect(channelReadKind(slot(3, []), 3)).toBe("ongoing");
    expect(channelReadKind(undefined, undefined)).toBe("first-after-subscribe");
    expect(channelReadKind(undefined, 7)).toBe("history");
  });
});

describe("decideChannelRead", () => {
  const before = slot(4, [post(3), post(4)]);

  it("notifies the newest new post and keeps the cursor of the read", () => {
    const d = decideChannelRead(wasm, read(before, slot(6, [post(3), post(4), post(5), post(6)]), 4), facts({ lastNotified: 4 }));
    expect(d.post?.cursor).toBe(6);
    expect(d.lastNotified).toBe(6);
  });

  it("is quiet for a muted channel, an open one and the person's own channel, and still moves the cursor", () => {
    const after = slot(6, [post(3), post(4), post(5), post(6)]);
    for (const f of [facts({ muted: true }), facts({ openAndVisible: true }), facts({ own: true })]) {
      const d = decideChannelRead(wasm, read(before, after, 4), { ...f, lastNotified: 4 });
      expect(d.post).toBeNull();
      expect(d.lastNotified).toBe(6);
    }
  });

  it("never notifies a post at or below the read marker or already notified", () => {
    const after = slot(6, [post(5), post(6)]);
    expect(decideChannelRead(wasm, read(before, after, 6), facts({ lastNotified: 4 })).post).toBeNull();
    expect(decideChannelRead(wasm, read(before, after, 4), facts({ lastNotified: 6 })).post).toBeNull();
  });

  it("on the first read after subscribing notifies only a newest post under an hour old", () => {
    const fresh = slot(9, [post(7, 5_000_000), post(8, 4_000_000), post(9, 60_000)]);
    expect(decideChannelRead(wasm, read(undefined, fresh, undefined), facts()).post?.cursor).toBe(9);
    const stale = slot(9, [post(8), post(9, 3_600_000)]);
    const d = decideChannelRead(wasm, read(undefined, stale, undefined), facts());
    expect(d.post).toBeNull();
    expect(d.lastNotified).toBe(9);
  });

  it("notifies nothing of a history refetch", () => {
    const after = slot(9, [post(8), post(9)]);
    const d = decideChannelRead(wasm, read(undefined, after, 2), facts());
    expect(d.post).toBeNull();
    expect(d.lastNotified).toBe(9);
  });

  it("moves the cursor past records that are not posts", () => {
    // The read passed a metadata record at 7 after the post at 5.
    const d = decideChannelRead(wasm, read(before, slot(7, [post(3), post(4), post(5)]), 4), facts({ lastNotified: 4 }));
    expect(d.post?.cursor).toBe(5);
    expect(d.lastNotified).toBe(7);
  });
});
