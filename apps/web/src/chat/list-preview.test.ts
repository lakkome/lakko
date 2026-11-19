// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  attachmentPreviewLabel,
  attachmentsPreviewLabel,
  emptyPreviewLine,
  messagePreviewWords,
  previewLine,
  previewMediaOf,
  rowMatchesQuery,
  stackEdges,
} from "./list-preview";

describe("attachmentPreviewLabel", () => {
  it("names the recording flows by their kind", () => {
    expect(attachmentPreviewLabel({ mime: "audio/mp4", kind: "voice" })).toBe("Voice message");
    expect(attachmentPreviewLabel({ mime: "video/mp4", kind: "video_note" })).toBe("Video message");
    expect(attachmentPreviewLabel({ mime: "image/gif", kind: "gif" })).toBe("GIF");
  });

  it("tells a photo or a video from the MIME type when there is no kind", () => {
    expect(attachmentPreviewLabel({ mime: "image/jpeg", kind: null })).toBe("Photo");
    expect(attachmentPreviewLabel({ mime: "IMAGE/PNG" })).toBe("Photo");
    expect(attachmentPreviewLabel({ mime: "video/mp4" })).toBe("Video");
    expect(attachmentPreviewLabel({ mime: "audio/ogg" })).toBe("Voice message");
  });

  it("is a file otherwise", () => {
    expect(attachmentPreviewLabel({ mime: "application/pdf" })).toBe("File");
    expect(attachmentPreviewLabel({ mime: null })).toBe("File");
  });

  it("counts the rest beyond a recorded attachment", () => {
    expect(attachmentsPreviewLabel([])).toBe("");
    expect(
      attachmentsPreviewLabel([{ mime: "audio/mp4", kind: "voice" }, { mime: "application/zip" }]),
    ).toBe("Voice message and 1 more");
  });
});

describe("attachmentsPreviewLabel (media-messages section 11)", () => {
  it("names photos and videos by count", () => {
    expect(attachmentsPreviewLabel([{ mime: "image/png" }])).toBe("Photo");
    expect(attachmentsPreviewLabel([{ mime: "image/png" }, { mime: "image/jpeg" }, { mime: "image/webp" }])).toBe(
      "3 photos",
    );
    expect(attachmentsPreviewLabel([{ mime: "video/mp4" }])).toBe("Video");
    expect(attachmentsPreviewLabel([{ mime: "video/mp4" }, { mime: "video/mp4" }])).toBe("2 videos");
    expect(
      attachmentsPreviewLabel([{ mime: "image/png" }, { mime: "video/mp4" }, { mime: "image/png" }, { mime: "video/mp4" }]),
    ).toBe("4 photos and videos");
  });

  it("names the photos when files ride beside them", () => {
    expect(attachmentsPreviewLabel([{ mime: "image/png" }, { mime: "application/zip" }, { mime: "x" }])).toBe("Photo");
  });

  it("names files only by count", () => {
    expect(attachmentsPreviewLabel([{ mime: "application/pdf" }])).toBe("File");
    expect(attachmentsPreviewLabel([{ mime: "application/pdf" }, { mime: null }, { mime: "text/plain" }])).toBe(
      "3 files",
    );
  });
});

describe("messagePreviewWords (media-messages section 11, the three cases)", () => {
  it("files only, with text: the count words, a middle dot, the text", () => {
    expect(messagePreviewWords("Weekend plans", [{ mime: "application/pdf" }, { mime: "text/plain" }])).toBe(
      "2 files \u00b7 Weekend plans",
    );
    expect(messagePreviewWords("Notes", [{ mime: null }])).toBe("File \u00b7 Notes");
  });

  it("photos or videos, with text: the text alone (the row draws the thumbnail)", () => {
    expect(messagePreviewWords("The trip", [{ mime: "image/jpeg" }, { mime: "video/mp4" }])).toBe("The trip");
    expect(messagePreviewWords("The trip", [{ mime: "image/jpeg" }, { mime: "application/pdf" }])).toBe("The trip");
  });

  it("no text: the count words alone", () => {
    expect(messagePreviewWords("", [{ mime: "image/jpeg" }, { mime: "image/png" }, { mime: "image/webp" }])).toBe(
      "3 photos",
    );
    expect(messagePreviewWords("", [{ mime: "application/pdf" }, { mime: "application/zip" }])).toBe("2 files");
  });

  it("leaves a plain text message and a recording with a caption as their text", () => {
    expect(messagePreviewWords("hi", [])).toBe("hi");
    expect(messagePreviewWords("listen", [{ mime: "audio/mp4", kind: "voice" }])).toBe("listen");
  });
});

describe("previewMediaOf and the stack", () => {
  const att = (mime: string, extra: Record<string, unknown> = {}) =>
    ({ blobIdB64: "", keyB64: "", nonceB64: "", mime, sizeStr: "1", name: null, ...extra }) as never;

  it("takes the first photo or video and counts them", () => {
    const thumb = { blobIdB64: "t", keyB64: "k", nonceB64: "n", mime: "image/jpeg", sizeStr: "9", width: 4, height: 3 };
    expect(previewMediaOf([att("application/pdf"), att("video/mp4", { thumb }), att("image/png")])).toEqual({
      thumb,
      video: true,
      count: 2,
    });
    expect(previewMediaOf([att("application/pdf")])).toBeNull();
    expect(previewMediaOf([att("audio/mp4", { kind: "voice" })])).toBeNull();
  });

  it("draws one edge for two items and two for three or more", () => {
    expect([1, 2, 3, 10].map(stackEdges)).toEqual([0, 1, 2, 2]);
  });
});

describe("previewLine", () => {
  it("prefixes the reader's own message with You", () => {
    expect(previewLine({ preview: "hi", isOwn: true, senderName: null, isGroup: true })).toBe(
      "You: hi",
    );
    expect(previewLine({ preview: "hi", isOwn: true, senderName: "Anna", isGroup: false })).toBe(
      "You: hi",
    );
  });

  it("prefixes the sender's name in a group only", () => {
    expect(previewLine({ preview: "hi", isOwn: false, senderName: "Anna", isGroup: true })).toBe(
      "Anna: hi",
    );
    expect(previewLine({ preview: "hi", isOwn: false, senderName: "Anna", isGroup: false })).toBe(
      "hi",
    );
  });

  it("shows the bare preview when the sender is unknown", () => {
    expect(previewLine({ preview: "hi", isOwn: false, senderName: null, isGroup: true })).toBe(
      "hi",
    );
    expect(previewLine({ preview: "hi", isOwn: false, senderName: "", isGroup: true })).toBe("hi");
  });

  it("is empty for an empty conversation", () => {
    expect(previewLine({ preview: null, isOwn: false, senderName: null, isGroup: true })).toBe("");
    expect(previewLine({ preview: "", isOwn: true, senderName: null, isGroup: false })).toBe("");
  });
});

describe("rowMatchesQuery", () => {
  it("matches the name or the preview, case-insensitively", () => {
    expect(rowMatchesQuery("ann", "Anna", "hello")).toBe(true);
    expect(rowMatchesQuery("HELLO", "Anna", "Anna: hello there")).toBe(true);
    expect(rowMatchesQuery("ivan", "Anna", "hello")).toBe(false);
  });

  it("an empty or blank query matches every row", () => {
    expect(rowMatchesQuery("", "Anna", "")).toBe(true);
    expect(rowMatchesQuery("   ", "Anna", "")).toBe(true);
  });
});

describe("emptyPreviewLine", () => {
  it("says there are no messages only once the log was read", () => {
    expect(emptyPreviewLine(true)).toBe("No messages yet");
  });

  it("says nothing while the log is still being read", () => {
    expect(emptyPreviewLine(false)).toBe("");
  });
});
