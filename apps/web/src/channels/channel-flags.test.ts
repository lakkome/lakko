// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { channelFlagsKey, channelFlagsKeyHex, channelFlagsOf } from "./channel-flags";

const id = "AB".repeat(16);

describe("a channel's synced flags", () => {
  it("are keyed on the channel id's 16 bytes, never a group id's 32", () => {
    const key = channelFlagsKey(id);
    expect(key).not.toBeNull();
    expect(key?.length).toBe(16);
    expect(channelFlagsKey("ab".repeat(32))).toBeNull();
    expect(channelFlagsKey("not hex")).toBeNull();
  });

  it("are read from the flags map under the lower-case id", () => {
    expect(channelFlagsKeyHex(id)).toBe("ab".repeat(16));
    const flags = { ["ab".repeat(16)]: { pinned: true, muted: true, archived: false } };
    expect(channelFlagsOf(flags, id)).toEqual({ pinned: true, muted: true, archived: false });
    expect(channelFlagsOf({}, id)).toEqual({ pinned: false, muted: false, archived: false });
  });
});
