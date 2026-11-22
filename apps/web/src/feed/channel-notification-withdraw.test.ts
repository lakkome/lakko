// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeEach, describe, expect, it } from "vitest";

import type { ChannelPostSlot, PersistedChannelPost } from "./channel-post-log";
import {
  CHANNEL_POST_CURSOR_KEY,
  postsGoneIn,
  recordChannelNotification,
  resetChannelNotificationRecords,
  withdrawGoneChannelNotification,
} from "./channel-notification-withdraw";

const NOW = 1_800_000_000_000;
const DAY = 86_400_000;

function post(cursor: number, ageMs = 0): PersistedChannelPost {
  return { cursor, serverTsMs: NOW - ageMs, text: `post ${cursor}` } as unknown as PersistedChannelPost;
}

function slot(cursor: number, posts: PersistedChannelPost[]): ChannelPostSlot {
  return { cursor, posts } as unknown as ChannelPostSlot;
}

function swNote(tag: string, data: unknown): { tag: string; data: unknown; closed: boolean; close: () => void } {
  const n = {
    tag,
    data,
    closed: false,
    close: () => {
      n.closed = true;
    },
  };
  return n;
}

describe("postsGoneIn", () => {
  it("names the posts a read took out of the channel's record", () => {
    const gone = postsGoneIn({ slotBefore: slot(3, [post(1), post(2), post(3)]), slotAfter: slot(4, [post(1), post(3)]) }, NOW);
    expect([...gone]).toEqual([2]);
  });

  it("does not count a post that only aged past the retention window", () => {
    const gone = postsGoneIn({ slotBefore: slot(2, [post(1, 31 * DAY), post(2)]), slotAfter: slot(2, [post(2)]) }, NOW);
    expect(gone.size).toBe(0);
  });

  it("is empty on a first read", () => {
    expect(postsGoneIn({ slotBefore: undefined, slotAfter: slot(2, [post(2)]) }, NOW).size).toBe(0);
  });
});

describe("withdrawGoneChannelNotification", () => {
  beforeEach(() => resetChannelNotificationRecords());

  const tag = "channel:ab";

  it("closes the page's notification when the post it stands for is gone", async () => {
    let closed = false;
    recordChannelNotification(tag, 7, () => {
      closed = true;
    });
    const n = await withdrawGoneChannelNotification(tag, new Set([7]), {
      getRegistration: async () => null,
      withdrawHost: async () => {},
    });
    expect(n).toBe(1);
    expect(closed).toBe(true);
  });

  it("leaves a notification standing for a newer post alone", async () => {
    let closed = false;
    recordChannelNotification(tag, 8, () => {
      closed = true;
    });
    await withdrawGoneChannelNotification(tag, new Set([7]), {
      getRegistration: async () => null,
      withdrawHost: async () => {},
    });
    expect(closed).toBe(false);
  });

  it("takes the desktop host's notification back", async () => {
    const withdrawn: string[] = [];
    recordChannelNotification(tag, 5);
    await withdrawGoneChannelNotification(tag, new Set([5]), {
      getRegistration: async () => null,
      withdrawHost: async (t) => {
        withdrawn.push(t);
      },
    });
    expect(withdrawn).toEqual([tag]);
  });

  it("finds a service-worker notification by the cursor it carries, after a reload too", async () => {
    const forGone = swNote(tag, { lakkoOpenPath: "/channel/ab", [CHANNEL_POST_CURSOR_KEY]: 9 });
    const forKept = swNote(tag, { lakkoOpenPath: "/channel/ab", [CHANNEL_POST_CURSOR_KEY]: 10 });
    const older = swNote(tag, { lakkoOpenPath: "/channel/ab" });
    let askedTag: string | undefined;
    const n = await withdrawGoneChannelNotification(tag, new Set([9]), {
      getRegistration: async () => ({
        getNotifications: async (filter?: GetNotificationOptions) => {
          askedTag = filter?.tag;
          return [forGone, forKept, older] as unknown as Notification[];
        },
      }),
      withdrawHost: async () => {},
    });
    expect(askedTag).toBe(tag);
    expect(n).toBe(1);
    expect([forGone.closed, forKept.closed, older.closed]).toEqual([true, false, false]);
  });

  it("does nothing when nothing is gone, and never throws", async () => {
    recordChannelNotification(tag, 1, () => {
      throw new Error("boom");
    });
    expect(
      await withdrawGoneChannelNotification(tag, new Set(), {
        getRegistration: async () => {
          throw new Error("no sw");
        },
        withdrawHost: async () => {},
      }),
    ).toBe(0);
    expect(
      await withdrawGoneChannelNotification(tag, new Set([1]), {
        getRegistration: async () => {
          throw new Error("no sw");
        },
        withdrawHost: async () => {},
      }),
    ).toBe(0);
  });
});
