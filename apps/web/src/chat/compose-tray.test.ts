// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11 — the composer tray's rules: the
 * shape an item takes, the two limits, when Send runs, Backspace.
 */
import { describe, expect, it } from "vitest";

import {
  backspaceRemovesLast,
  fitIntoTray,
  TRAY_MAX_FILES,
  TRAY_MAX_MEDIA,
  trayDurationLabel,
  trayKindOf,
  traySendable,
  type TrayKind,
} from "./compose-tray";
import { MAX_ATTACHMENT_BYTES } from "./attachments";

describe("trayKindOf", () => {
  it("makes an inline photo or video a thumbnail when it came in as media", () => {
    expect(trayKindOf({ type: "image/jpeg", size: 10 }, "media", "image")).toBe("photo");
    expect(trayKindOf({ type: "video/mp4", size: 10 }, "media", "video")).toBe("video");
  });

  it("makes anything the receivers cannot show inline a file chip, never a refusal", () => {
    expect(trayKindOf({ type: "image/heic", size: 10 }, "media", null)).toBe("file");
    expect(trayKindOf({ type: "video/quicktime", size: 10 }, "media", null)).toBe("file");
    expect(trayKindOf({ type: "application/pdf", size: 10 }, "media", null)).toBe("file");
  });

  it("sends what came through File or Audio as a file whatever it is", () => {
    expect(trayKindOf({ type: "image/jpeg", size: 10 }, "file", "image")).toBe("file");
    expect(trayKindOf({ type: "audio/mpeg", size: 10 }, "audio", "audio")).toBe("file");
  });

  it("refuses only what is over the cap", () => {
    expect(trayKindOf({ type: "video/mp4", size: MAX_ATTACHMENT_BYTES + 1 }, "media", "video")).toBe("too-big");
    expect(trayKindOf({ type: "application/pdf", size: MAX_ATTACHMENT_BYTES }, "file", null)).toBe("file");
  });
});

describe("fitIntoTray", () => {
  const many = (kind: TrayKind, n: number): TrayKind[] => Array.from({ length: n }, () => kind);

  it("takes everything that fits, in order", () => {
    expect(fitIntoTray([], ["photo", "file", "video"])).toEqual({ accepted: [0, 1, 2], notice: null });
  });

  it("adds what fits past the photos-and-videos limit and says so", () => {
    const r = fitIntoTray(many("photo", TRAY_MAX_MEDIA - 2), ["video", "photo", "photo", "file"]);
    expect(r.accepted).toEqual([0, 1, 3]);
    expect(r.notice).toBe("media");
  });

  it("counts files apart from photos and videos", () => {
    const r = fitIntoTray([...many("photo", TRAY_MAX_MEDIA), ...many("file", TRAY_MAX_FILES - 1)], ["file", "file"]);
    expect(r.accepted).toEqual([0]);
    expect(r.notice).toBe("files");
  });

  it("names the photos-and-videos limit when both dropped something", () => {
    const r = fitIntoTray([...many("video", TRAY_MAX_MEDIA), ...many("file", TRAY_MAX_FILES)], ["file", "photo"]);
    expect(r.accepted).toEqual([]);
    expect(r.notice).toBe("media");
  });
});

describe("traySendable", () => {
  it("needs text or an item", () => {
    expect(traySendable("", [])).toBe(false);
    expect(traySendable("hi", [])).toBe(true);
    expect(traySendable("", ["ready"])).toBe(true);
  });

  it("waits for every upload and refuses while one failed", () => {
    expect(traySendable("hi", ["ready", "uploading"])).toBe(false);
    expect(traySendable("hi", ["preparing"])).toBe(false);
    expect(traySendable("", ["ready", "failed"])).toBe(false);
  });
});

describe("backspaceRemovesLast", () => {
  const base = { key: "Backspace", fieldEmpty: true, modified: false, composing: false, items: 2 };
  it("removes the last item from an empty field", () => {
    expect(backspaceRemovesLast(base)).toBe(true);
  });
  it("leaves text editing alone", () => {
    expect(backspaceRemovesLast({ ...base, fieldEmpty: false })).toBe(false);
    expect(backspaceRemovesLast({ ...base, modified: true })).toBe(false);
    expect(backspaceRemovesLast({ ...base, composing: true })).toBe(false);
    expect(backspaceRemovesLast({ ...base, items: 0 })).toBe(false);
    expect(backspaceRemovesLast({ ...base, key: "Delete" })).toBe(false);
  });
});

describe("trayDurationLabel", () => {
  it("writes minutes and seconds", () => {
    expect(trayDurationLabel(7_400)).toBe("0:07");
    expect(trayDurationLabel(65_000)).toBe("1:05");
    expect(trayDurationLabel(undefined)).toBeNull();
    expect(trayDurationLabel(0)).toBeNull();
  });
});
