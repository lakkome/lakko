// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import { channelIdForHandle, clearClaimedHandles, forgetClaimedHandle } from "./claimed-handles";

describe("the channel id kept for a claimed handle (D5)", () => {
  it("a retry reuses the id of the attempt that claimed the handle", () => {
    const s = new MemoryStorage();
    expect(channelIdForHandle("a", "news", "id1", s)).toBe("id1");
    expect(channelIdForHandle("a", "@News ", "id2", s)).toBe("id1");
  });

  it("is per account and per handle", () => {
    const s = new MemoryStorage();
    channelIdForHandle("a", "news", "id1", s);
    expect(channelIdForHandle("b", "news", "id2", s)).toBe("id2");
    expect(channelIdForHandle("a", "other", "id3", s)).toBe("id3");
  });

  it("goes when the channel exists or the handle was someone else's", () => {
    const s = new MemoryStorage();
    channelIdForHandle("a", "news", "id1", s);
    forgetClaimedHandle("a", "news", s);
    expect(channelIdForHandle("a", "news", "id2", s)).toBe("id2");
  });

  it("goes with the account", () => {
    const s = new MemoryStorage();
    channelIdForHandle("a", "news", "id1", s);
    clearClaimedHandles("a", s);
    expect(channelIdForHandle("a", "news", "id2", s)).toBe("id2");
  });

  it("an unreadable row reads as none", () => {
    const s = new MemoryStorage();
    s.setItem("lakko.channels.claimed-handles.v1.a", "not json");
    expect(channelIdForHandle("a", "news", "id1", s)).toBe("id1");
  });
});
