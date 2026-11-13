// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Mark all read in Chats reads the tab's channels too (follow-up to base
 * scenario 29, 2026-11-13): each is read through the newest post this device
 * holds, so its count clears; a marker never moves backwards.
 */
import { describe, expect, it } from "vitest";

import { channelUnreadCount, emptyFeedMarks, markChannelsRead } from "./feed-marks";

const a = "aa".repeat(16);
const b = "bb".repeat(16);

describe("mark all read on channels", () => {
  it("reads each channel through its newest held post, never backwards", () => {
    const marks = { ...emptyFeedMarks(), channelRead: { [a]: 3, [b]: 9 } };
    const slots = {
      [a]: { cursor: 4, posts: [{ cursor: 5 }, { cursor: 7 }] },
      [b]: { cursor: 2, posts: [{ cursor: 5 }] },
    };
    const next = markChannelsRead(marks, [a, b], slots);
    expect(next.channelRead[a]).toBe(7);
    expect(next.channelRead[b]).toBe(9);
    expect(channelUnreadCount([5, 7], next.channelRead[a], false)).toBe(0);
  });

  it("leaves a channel with no slot as it is", () => {
    const marks = { ...emptyFeedMarks(), channelRead: { [a]: 3 } };
    expect(markChannelsRead(marks, [a], {})).toEqual(marks);
  });
});
