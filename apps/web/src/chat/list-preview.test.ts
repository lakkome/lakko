// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  attachmentPreviewLabel,
  attachmentsPreviewLabel,
  emptyPreviewLine,
  previewLine,
  rowMatchesQuery,
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

  it("counts the rest beyond the first attachment", () => {
    expect(attachmentsPreviewLabel([])).toBe("");
    expect(attachmentsPreviewLabel([{ mime: "image/png" }])).toBe("Photo");
    expect(
      attachmentsPreviewLabel([{ mime: "image/png" }, { mime: "application/zip" }, { mime: "x" }]),
    ).toBe("Photo and 2 more");
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
