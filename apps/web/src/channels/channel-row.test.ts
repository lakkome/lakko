// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  channelRowMatches,
  channelRowModel,
  channelTitle,
  displayHandle,
  subscriberCountLabel,
} from "./channel-row";

// Wednesday 16 September 2026, 15:30 local time.
const NOW = new Date(2026, 8, 16, 15, 30);

const base = {
  id: "00".repeat(16),
  localName: "Lakko news",
  localHandle: "lakko-news",
  isOwner: true,
  subscriberCount: null,
  lastPostMs: null,
};

describe("channelRowModel", () => {
  it("titles the row by the local name and shows the handle on the second line", () => {
    const m = channelRowModel(base, NOW);
    expect(m.title).toBe("Lakko news");
    expect(m.handle).toBe("@lakko-news");
    expect(m.subtitle).toBe("@lakko-news");
  });

  it("falls back to the handle, then to 'Channel', when there is no name", () => {
    expect(channelRowModel({ ...base, localName: "  " }, NOW).title).toBe("lakko-news");
    expect(channelRowModel({ ...base, localName: "", localHandle: undefined }, NOW).title).toBe(
      "Channel",
    );
  });

  it("the shared name wins over the local name and the handle (ADR-0051)", () => {
    expect(channelRowModel({ ...base, sharedName: "Lakko News" }, NOW).title).toBe("Lakko News");
    expect(channelRowModel({ ...base, sharedName: "  " }, NOW).title).toBe("Lakko news");
    expect(channelTitle(undefined, "", "lakko-news")).toBe("lakko-news");
    expect(channelTitle("Shared", "Local", undefined)).toBe("Shared");
  });

  it("adds the subscriber count when it is known", () => {
    expect(channelRowModel({ ...base, subscriberCount: 1 }, NOW).subtitle).toBe(
      "@lakko-news · 1 subscriber",
    );
    expect(channelRowModel({ ...base, subscriberCount: 40 }, NOW).subtitle).toBe(
      "@lakko-news · 40 subscribers",
    );
  });

  it("says the role when there is neither a handle nor a count", () => {
    const noHandle = { ...base, localHandle: undefined };
    expect(channelRowModel(noHandle, NOW).subtitle).toBe("Your channel");
    expect(channelRowModel({ ...noHandle, isOwner: false }, NOW).subtitle).toBe("Subscribed");
  });

  it("labels the last post time like a chat row, and stays empty when unknown", () => {
    const today = new Date(2026, 8, 16, 9, 5).getTime();
    expect(channelRowModel({ ...base, lastPostMs: today }, NOW).time).toMatch(/9:05/);
    expect(channelRowModel(base, NOW).time).toBe("");
  });
});

describe("displayHandle", () => {
  it("normalises to one at-sign and trims", () => {
    expect(displayHandle("@@news ")).toBe("@news");
    expect(displayHandle("news")).toBe("@news");
    expect(displayHandle("  ")).toBe("");
    expect(displayHandle(undefined)).toBe("");
  });
});

describe("subscriberCountLabel", () => {
  it("pluralises", () => {
    expect(subscriberCountLabel(0)).toBe("0 subscribers");
    expect(subscriberCountLabel(1)).toBe("1 subscriber");
  });
});

describe("channelRowMatches", () => {
  const m = channelRowModel(base, NOW);
  it("matches the title and the handle, ignoring case and a leading at-sign", () => {
    expect(channelRowMatches(m, "NEWS")).toBe(true);
    expect(channelRowMatches(m, "@lakko")).toBe(true);
    expect(channelRowMatches(m, "weather")).toBe(false);
  });
  it("matches everything on an empty query", () => {
    expect(channelRowMatches(m, "   ")).toBe(true);
  });
});

describe("channelRowPreview", () => {
  it("shows the last readable post, one line, else the fallback", async () => {
    const { channelRowPreview } = await import("./channel-row");
    expect(channelRowPreview("Hello\n  world", "@news")).toBe("Hello world");
    expect(channelRowPreview(null, "@news")).toBe("@news");
    expect(channelRowPreview("   ", "@news")).toBe("@news");
  });
});
