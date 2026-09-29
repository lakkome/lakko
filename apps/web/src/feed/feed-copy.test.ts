// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { PersistedAttachment, PersistedMessage } from "../chat/message-log-store";
import { attachmentLine, repliesLabel, whyLine } from "./feed-copy";
import type { FeedCandidateRef } from "./feed-model";

function message(extra: Partial<PersistedMessage> = {}): PersistedMessage {
  return {
    messageId: "1",
    direction: "incoming",
    text: "hi",
    attachments: [],
    createdAt: "2026-09-29T10:00:00Z",
    ...extra,
  };
}

function ref(extra: Partial<FeedCandidateRef> = {}, m: Partial<PersistedMessage> = {}): FeedCandidateRef {
  return {
    source: { kind: "group", idHex: "aa", slug: "s", title: "Book club", muted: false, audience: 4 },
    message: message(m),
    mentionsMe: false,
    repliesToMe: false,
    ...extra,
  };
}

const attachment = (a: Partial<PersistedAttachment>): PersistedAttachment =>
  ({ mime: null, sizeStr: "1", name: "f", ...a }) as PersistedAttachment;

describe("whyLine", () => {
  it("puts the reader first", () => {
    expect(whyLine(ref({ mentionsMe: true, repliesToMe: true }), "latest")).toBe("Mentions you");
    expect(whyLine(ref({ repliesToMe: true }), "popular")).toBe("Reply to your message");
  });

  it("names popularity only on the Popular feed and only with reactions", () => {
    const reacted = ref({}, { reactions: [{ reactorKeyB64: "x", emoji: "a" }] });
    expect(whyLine(reacted, "popular")).toBe("Popular in Book club");
    expect(whyLine(reacted, "latest")).toBeNull();
    expect(whyLine(ref(), "popular")).toBeNull();
  });
});

describe("attachmentLine", () => {
  it("names the one attachment by kind, then by type", () => {
    expect(attachmentLine(message())).toBeNull();
    expect(attachmentLine(message({ attachments: [attachment({ kind: "voice" })] }))).toBe(
      "Voice message",
    );
    expect(attachmentLine(message({ attachments: [attachment({ kind: "video_note" })] }))).toBe(
      "Video message",
    );
    expect(attachmentLine(message({ attachments: [attachment({ mime: "image/png" })] }))).toBe(
      "Photo",
    );
    expect(attachmentLine(message({ attachments: [attachment({ mime: "application/pdf" })] }))).toBe(
      "File",
    );
    expect(
      attachmentLine(message({ attachments: [attachment({}), attachment({})] })),
    ).toBe("2 attachments");
  });
});

describe("repliesLabel", () => {
  it("is singular for one", () => {
    expect(repliesLabel(1)).toBe("1 reply");
    expect(repliesLabel(3)).toBe("3 replies");
  });
});
