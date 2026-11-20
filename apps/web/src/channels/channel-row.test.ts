// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  channelRowMatches,
  channelRowModel,
  channelRowPostedAtMs,
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
  role: "owner" as const,
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
    expect(channelRowModel({ ...noHandle, role: "subscriber" }, NOW).subtitle).toBe("Subscribed");
  });

  it("names a publisher as a publisher, never as subscribed (UX 15.2)", () => {
    const noHandle = { ...base, localHandle: undefined };
    expect(channelRowModel({ ...noHandle, role: "publisher" }, NOW).subtitle).toBe("Publisher");
    // The @name still comes first when there is one.
    expect(channelRowModel({ ...base, role: "publisher" }, NOW).subtitle).toBe("@lakko-news");
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
    expect(channelRowPreview({ text: "Hello\n  world" }, "@news")).toBe("Hello world");
    expect(channelRowPreview(null, "@news")).toBe("@news");
    expect(channelRowPreview({ text: "   " }, "@news")).toBe("@news");
  });

  it("reads like a chat row for the three cases of media-messages section 11", async () => {
    const { channelRowPreview } = await import("./channel-row");
    const files = [{ mime: "application/pdf", kind: null }, { mime: "text/plain", kind: null }];
    expect(channelRowPreview({ text: "Weekend plans", media: files }, "@x")).toBe("2 files \u00b7 Weekend plans");
    expect(channelRowPreview({ text: "The trip", media: [{ mime: "image/jpeg", kind: null }] }, "@x")).toBe("The trip");
    expect(channelRowPreview({ text: "", media: files }, "@x")).toBe("2 files");
  });

  it("names a caption-less post's attachment the way a group row does, not the @name", async () => {
    const { channelRowPreview } = await import("./channel-row");
    const { attachmentsPreviewLabel } = await import("../chat/list-preview");
    const cases = [
      [{ mime: "image/jpeg", kind: null }],
      [{ mime: "text/plain", kind: null }],
      [{ mime: "image/gif", kind: "gif" }],
      [{ mime: "audio/mp4", kind: "voice" }],
      [{ mime: "video/mp4", kind: "video_note" }],
      [
        { mime: "image/jpeg", kind: null },
        { mime: "image/png", kind: null },
      ],
    ];
    for (const media of cases) {
      const shown = channelRowPreview({ text: "", media }, "@walk_22_channel");
      expect(shown).toBe(attachmentsPreviewLabel(media));
      expect(shown).not.toBe("@walk_22_channel");
    }
    expect(channelRowPreview({ text: "", media: [{ mime: "image/gif", kind: "gif" }] }, "@news")).toBe(
      attachmentsPreviewLabel([{ mime: "image/gif", kind: "gif" }]),
    );
    // A caption wins over the attachment's kind, as in a group's row.
    expect(channelRowPreview({ text: "Look", media: [{ mime: "image/jpeg", kind: null }] }, "@news")).toBe("Look");
    // No text and no attachments: the fallback.
    expect(channelRowPreview({ text: "", media: [] }, "@news")).toBe("@news");
  });
});

describe("channelRowTime", () => {
  it("is the newest post's time in the list form, or empty with no post", async () => {
    const { channelRowTime } = await import("./channel-row");
    const { formatListTime } = await import("../chat/list-time");
    const posted = new Date(2026, 8, 16, 14, 5).getTime();
    expect(channelRowTime(posted, NOW)).toBe(formatListTime(new Date(posted).toISOString(), NOW));
    expect(channelRowTime(posted, NOW)).not.toBe("");
    expect(channelRowTime(null, NOW)).toBe("");
    expect(channelRowTime(undefined, NOW)).toBe("");
  });
});

describe("channelRowPostedAtMs", () => {
  // Seen live 2026-11-12: a ban list posted at 22:48 stood as the row time of
  // a channel whose newest post was at 22:39. The time is the newest post
  // this device read, whatever newer record the server holds.
  it("is the newest read post's time, or nothing", () => {
    expect(channelRowPostedAtMs({ serverTsMs: 1_000 })).toBe(1_000);
    expect(channelRowPostedAtMs(null)).toBeNull();
    expect(channelRowPostedAtMs({ serverTsMs: Number.NaN })).toBeNull();
  });
});
