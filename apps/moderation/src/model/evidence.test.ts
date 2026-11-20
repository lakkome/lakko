// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  attachmentGate,
  attachmentLabel,
  checksNeedPosting,
  countChecks,
  evidenceMark,
  isGreyed,
  markLabel,
  usernameLine,
} from "./evidence";

describe("evidenceMark", () => {
  it("marks a message by whether it verified as the target's", () => {
    expect(evidenceMark("message", true)).toBe("by-target");
    expect(evidenceMark("message", false)).toBe("not-verified");
    expect(markLabel("by-target")).toBe("Written by this account");
    expect(markLabel("not-verified")).toBe("Not verified");
  });

  it("never marks another kind as written by the target", () => {
    for (const kind of ["contact-request", "channel-post", "listing", undefined, "something"]) {
      expect(evidenceMark(kind, true)).toBe("not-checked");
    }
  });

  it("greys everything that is not written by the target", () => {
    expect(isGreyed("by-target")).toBe(false);
    expect(isGreyed("not-verified")).toBe(true);
    expect(isGreyed("not-checked")).toBe(true);
  });
});

describe("countChecks", () => {
  it("counts messages only, verified and not", () => {
    expect(
      countChecks([
        { kind: "message", isByTarget: true },
        { kind: "message", isByTarget: false },
        { kind: "message", isByTarget: true },
        { kind: "listing", isByTarget: true },
        { kind: "contact-request", isByTarget: false },
      ]),
    ).toEqual({ checked: 2, unchecked: 1 });
    expect(countChecks([])).toEqual({ checked: 0, unchecked: 0 });
  });

  it("stays within the API's 0..255", () => {
    const many = Array.from({ length: 300 }, () => ({ kind: "message", isByTarget: true }));
    expect(countChecks(many).checked).toBe(255);
  });

  it("posts only when the stored checks are absent or different", () => {
    expect(checksNeedPosting(null, { checked: 0, unchecked: 0 })).toBe(true);
    expect(checksNeedPosting({ checked: 1, unchecked: 0 }, { checked: 1, unchecked: 0 })).toBe(false);
    expect(checksNeedPosting({ checked: 1, unchecked: 0 }, { checked: 1, unchecked: 1 })).toBe(true);
  });
});

describe("attachments", () => {
  it("blurs photos and videos, and opens audio and files only on request", () => {
    expect(attachmentGate("image")).toBe("blurred");
    expect(attachmentGate("video")).toBe("blurred");
    expect(attachmentGate("audio")).toBe("on-request");
    expect(attachmentGate(null)).toBe("on-request");
  });

  it("describes the attachment", () => {
    expect(attachmentLabel({ kind: undefined, inline: "image", name: undefined, size: 2048n })).toBe("Photo (2.0 KB)");
    expect(attachmentLabel({ kind: "voice", inline: "audio", name: undefined, size: 10n })).toBe("Voice message (10 B)");
    expect(attachmentLabel({ kind: undefined, inline: null, name: "a.pdf", size: 3n * 1024n * 1024n })).toBe("File: a.pdf (3.0 MB)");
  });
});

describe("usernameLine", () => {
  it("shows a username only when the evidence names one", () => {
    expect(usernameLine("alice", null)).toBe("@alice");
    expect(usernameLine(null, { has_username: true, in_everyone: false, name_lookup_keys_b64: [] })).toBe("Has a username");
    expect(usernameLine(null, { has_username: false, in_everyone: false, name_lookup_keys_b64: [] })).toBe("No username");
    expect(usernameLine("", null)).toContain("unknown");
  });
});

describe("evidence files", () => {
  it("keeps the MIME type only for inline media", async () => {
    const { blobContentType, saveName, fileProblem } = await import("./evidence");
    expect(blobContentType("image", "Image/JPEG; q=1")).toBe("image/jpeg");
    expect(blobContentType(null, "text/html")).toBe("application/octet-stream");
    expect(blobContentType("video", undefined)).toBe("application/octet-stream");
    expect(saveName(" offer.pdf ")).toBe("offer.pdf");
    expect(saveName(undefined)).toBe("evidence-file");
    expect(fileProblem("mismatch")).toContain("different file");
    expect(fileProblem("decrypt")).toContain("couldn't be decrypted");
  });
});
