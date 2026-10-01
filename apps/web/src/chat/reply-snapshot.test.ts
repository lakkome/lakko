// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 / `docs/ux/message-actions.md` section 3.1 — the snapshot a reply
 * carries and the two lines a bubble draws from it.
 */
import { describe, expect, it } from "vitest";

import {
  labelForBubble,
  oneLine,
  persistedFromReplyRef,
  replyQuoteLabel,
  replyQuoteLines,
  replyRefFromPersisted,
  replySnapshotFor,
  REPLY_EXCERPT_FALLBACK,
} from "./reply-snapshot";
import type { MediaBubble } from "../media/inline-media";

type Att = { readonly bubble: MediaBubble };
const bubbleOf = (a: Att): MediaBubble => a.bubble;

describe("replySnapshotFor (the strip, taken when Reply is chosen)", () => {
  it("uses the message's text, trimmed to one line", () => {
    const s = replySnapshotFor(
      { text: "  first line\n\n  second   line \t", attachments: [] },
      "Anna",
      bubbleOf,
    );
    expect(s).toEqual({ author: "Anna", excerpt: "first line second line" });
  });

  it("keeps the author label exactly as the timeline names them", () => {
    expect(replySnapshotFor({ text: "hi", attachments: [] }, "You", bubbleOf).author).toBe("You");
  });

  it("names a message with no text by the kind of its FIRST attachment", () => {
    const kinds: Array<[MediaBubble, string]> = [
      ["voice", "Voice message"],
      ["video_note", "Video message"],
      ["photo", "Photo"],
      ["video", "Video"],
      ["gif", "GIF"],
      ["file", "File"],
    ];
    for (const [bubble, label] of kinds) {
      const s = replySnapshotFor(
        { text: "", attachments: [{ bubble }, { bubble: "file" }] },
        "Anna",
        bubbleOf,
      );
      expect(s.excerpt).toBe(label);
    }
  });

  it("prefers text over an attachment when the message has both", () => {
    const s = replySnapshotFor(
      { text: "caption", attachments: [{ bubble: "photo" }] },
      "Anna",
      bubbleOf,
    );
    expect(s.excerpt).toBe("caption");
  });

  it("a whitespace-only caption counts as no text", () => {
    const s = replySnapshotFor({ text: " \n ", attachments: [{ bubble: "gif" }] }, "A", bubbleOf);
    expect(s.excerpt).toBe("GIF");
  });

  it("a message with neither text nor attachments gets the fallback word", () => {
    expect(replySnapshotFor({ text: "", attachments: [] }, "A", bubbleOf).excerpt).toBe(
      REPLY_EXCERPT_FALLBACK(),
    );
  });

  it("does not cap the length: the core truncates on a character boundary", () => {
    const long = "x".repeat(2000);
    expect(replySnapshotFor({ text: long, attachments: [] }, "A", bubbleOf).excerpt).toBe(long);
  });

  it("labelForBubble is total over the bubble shapes", () => {
    const shapes: MediaBubble[] = ["voice", "video_note", "gif", "photo", "video", "file"];
    for (const b of shapes) expect(labelForBubble(b).length).toBeGreaterThan(0);
  });

  it("oneLine collapses every whitespace run and trims", () => {
    expect(oneLine("\n a \r\n\t b  ")).toBe("a b");
    expect(oneLine("")).toBe("");
  });
});

describe("replyQuoteLines (the block in the bubble)", () => {
  it("draws the carried snapshot", () => {
    expect(replyQuoteLines({ author: "Anna", excerpt: "see you at 5" })).toEqual({
      name: "Anna",
      excerpt: "see you at 5",
    });
  });

  it("a reply with no snapshot at all: name empty, excerpt 'Message'", () => {
    expect(replyQuoteLines({})).toEqual({ name: "", excerpt: "Message" });
  });

  it("a snapshot missing one half falls back on that half only", () => {
    expect(replyQuoteLines({ author: "Anna" })).toEqual({ name: "Anna", excerpt: "Message" });
    expect(replyQuoteLines({ excerpt: "hi" })).toEqual({ name: "", excerpt: "hi" });
  });

  it("the accessible name is 'Reply to <name>', or 'Reply' with no name", () => {
    expect(replyQuoteLabel({ name: "Anna", excerpt: "x" })).toBe("Reply to Anna");
    expect(replyQuoteLabel({ name: "", excerpt: "Message" })).toBe("Reply");
  });
});

describe("the persisted reply reference", () => {
  it("round-trips a full reference", () => {
    const ref = { target: 42, author: "Anna", excerpt: "hi" };
    const stored = persistedFromReplyRef(ref);
    expect(stored).toEqual({ target: "42", author: "Anna", excerpt: "hi" });
    expect(replyRefFromPersisted(stored)).toEqual(ref);
  });

  it("round-trips a reference with no snapshot without inventing fields", () => {
    const stored = persistedFromReplyRef({ target: 7 });
    expect(stored).toEqual({ target: "7" });
    expect(Object.keys(stored)).toEqual(["target"]);
    expect(replyRefFromPersisted(stored)).toEqual({ target: 7 });
    expect(Object.keys(replyRefFromPersisted(stored) ?? {})).toEqual(["target"]);
  });

  it("an absent row field reads as no reply", () => {
    expect(replyRefFromPersisted(undefined)).toBeUndefined();
  });

  it("an unparseable target reads as no reply rather than a reply to NaN", () => {
    expect(replyRefFromPersisted({ target: "not-a-number" })).toBeUndefined();
  });
});
