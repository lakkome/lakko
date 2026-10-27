// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** docs/ux/profile-screen.md — the screen's rules and the crop arithmetic. */
import { describe, expect, it } from "vitest";

import type { AvatarData } from "./avatar-model";
import {
  CLOSED_FIELD,
  isDirty,
  leaveAnswerEffect,
  leaveDecision,
  leaveField,
  normalizePending,
  openField,
  photoShown,
  revertField,
  runProfileSave,
  saveEnabled,
  typeIntoField,
  type PendingFields,
  type RenameResult,
  type SavedFields,
} from "./profile-edit";
import {
  INITIAL_CROP,
  clampCrop,
  clampVideoStart,
  dragCrop,
  encodeUnderLimit,
  keptVideoDurationMs,
  outputSide,
  sourceSquare,
  usableWithoutReencode,
  zoomCrop,
} from "./avatar-crop";

const AVATAR: AvatarData = {
  still: { blobIdB64: "AA==", keyB64: "AA==", nonceB64: "AA==", size: 1, side: 1 },
  video: null,
};
const SAVED: SavedFields = { name: "Alice", username: "alice", avatar: null };

describe("saved and pending", () => {
  it("a value changed back to its saved one is no change", () => {
    expect(isDirty(SAVED, { name: "Alice", username: "alice" })).toBe(false);
    expect(isDirty(SAVED, { name: "Bob" })).toBe(true);
    expect(isDirty(SAVED, { name: null })).toBe(true);
    // Removing a photo that is not there is nothing.
    expect(isDirty(SAVED, { photo: { kind: "remove" } })).toBe(false);
    expect(isDirty({ ...SAVED, avatar: AVATAR }, { photo: { kind: "remove" } })).toBe(true);
    expect(normalizePending(SAVED, { name: "Alice", username: "bob" })).toEqual({ username: "bob" });
  });

  it("Save needs a change, valid open fields, and no save running", () => {
    const pending: PendingFields<string> = { name: "Bob" };
    expect(saveEnabled({ saved: SAVED, pending, openFieldsValid: true, busy: false })).toBe(true);
    expect(saveEnabled({ saved: SAVED, pending, openFieldsValid: false, busy: false })).toBe(false);
    expect(saveEnabled({ saved: SAVED, pending, openFieldsValid: true, busy: true })).toBe(false);
    expect(saveEnabled({ saved: SAVED, pending: {}, openFieldsValid: true, busy: false })).toBe(false);
  });

  it("Remove photo shows while a photo is saved or pending", () => {
    expect(photoShown(SAVED, {})).toBe(false);
    expect(photoShown({ ...SAVED, avatar: AVATAR }, {})).toBe(true);
    expect(photoShown(SAVED, { photo: { kind: "set", media: "m" } })).toBe(true);
    expect(photoShown({ ...SAVED, avatar: AVATAR }, { photo: { kind: "remove" } })).toBe(false);
  });
});

describe("the in-place field", () => {
  it("opens with the current text and closes with the new one", () => {
    let f = openField("Alice");
    f = typeIntoField(f, "Alicia");
    const left = leaveField(f, true);
    expect(left).toEqual({ state: CLOSED_FIELD, committed: "Alicia" });
  });

  it("stays open while invalid", () => {
    const f = typeIntoField(openField("Alice"), "bad\nname");
    expect(leaveField(f, false)).toEqual({ state: f, committed: null });
  });

  it("Escape restores what was there before the edit", () => {
    const f = typeIntoField(openField("Alice"), "Zed");
    expect(revertField(f)).toEqual({ state: CLOSED_FIELD, restored: "Alice" });
    expect(revertField(CLOSED_FIELD)).toEqual({ state: CLOSED_FIELD, restored: null });
  });

  it("typing into a closed field does nothing", () => {
    expect(typeIntoField(CLOSED_FIELD, "x")).toBe(CLOSED_FIELD);
  });
});

describe("the save", () => {
  function ports(over: { upload?: () => Promise<AvatarData>; rename?: RenameResult; publish?: () => Promise<void> } = {}) {
    const calls: string[] = [];
    return {
      calls,
      ports: {
        uploadPhoto: async () => {
          calls.push("upload");
          return over.upload !== undefined ? over.upload() : AVATAR;
        },
        rename: async (u: string): Promise<RenameResult> => {
          calls.push(`rename:${u}`);
          return over.rename ?? { kind: "done", username: u };
        },
        publish: async (f: { name: string | null; avatar: AvatarData | null }) => {
          calls.push(`publish:${f.name}:${f.avatar === null ? "none" : "photo"}`);
          if (over.publish !== undefined) await over.publish();
        },
      },
    };
  }

  it("runs photo, then username, then profile", async () => {
    const t = ports();
    const out = await runProfileSave(SAVED, { name: "Bob", username: "bob", photo: { kind: "set", media: "m" } }, t.ports);
    expect(out).toEqual({ kind: "saved" });
    expect(t.calls).toEqual(["upload", "rename:bob", "publish:Bob:photo"]);
  });

  it("publishes nothing for a username-only change (the rename republishes)", async () => {
    const t = ports();
    await runProfileSave(SAVED, { username: "bob" }, t.ports);
    expect(t.calls).toEqual(["rename:bob"]);
  });

  it("a failed upload keeps everything pending", async () => {
    const t = ports({ upload: () => Promise.reject(new Error("net")) });
    const out = await runProfileSave(SAVED, { name: "Bob", photo: { kind: "set", media: "m" } }, t.ports);
    expect(out.kind).toBe("failed");
    if (out.kind === "failed") {
      expect(out.error).toEqual({ kind: "upload" });
      expect(out.pending).toEqual({ name: "Bob", photo: { kind: "set", media: "m" } });
    }
  });

  it("a taken username keeps it pending and the uploaded photo is not uploaded again", async () => {
    const t = ports({ rename: { kind: "taken" } });
    const out = await runProfileSave(SAVED, { username: "bob", photo: { kind: "set", media: "m" } }, t.ports);
    expect(out.kind === "failed" && out.error).toEqual({ kind: "taken", username: "bob" });
    if (out.kind !== "failed") throw new Error("expected failure");
    expect(out.pending.username).toBe("bob");
    expect(out.pending.photo?.kind).toBe("uploaded");
    const again = ports();
    await runProfileSave(SAVED, out.pending, again.ports);
    expect(again.calls).toEqual(["rename:bob", "publish:Alice:photo"]);
  });

  it("a cancelled proof stops the save there", async () => {
    const t = ports({ rename: { kind: "cancelled" } });
    const out = await runProfileSave(SAVED, { username: "bob", name: "Bob" }, t.ports);
    expect(out.kind === "failed" && out.error.kind).toBe("cancelled");
    expect(t.calls).toEqual(["rename:bob"]);
  });

  it("a failed publish keeps the name pending but not the done username", async () => {
    const t = ports({ publish: () => Promise.reject(new Error("x")) });
    const out = await runProfileSave(SAVED, { username: "bob", name: "Bob" }, t.ports);
    expect(out.kind === "failed" && out.pending).toEqual({ name: "Bob" });
  });

  it("removing the photo publishes a profile without one", async () => {
    const t = ports();
    await runProfileSave({ ...SAVED, avatar: AVATAR }, { photo: { kind: "remove" } }, t.ports);
    expect(t.calls).toEqual(["publish:Alice:none"]);
  });
});

describe("leaving", () => {
  it("asks only when something is pending", () => {
    expect(leaveDecision(false, false)).toBe("leave");
    expect(leaveDecision(true, false)).toBe("ask");
    expect(leaveDecision(true, true)).toBe("stay");
  });

  it("maps each answer", () => {
    expect(leaveAnswerEffect("save")).toEqual({ runSave: true, dropPending: false, continueNavigation: "after-save" });
    expect(leaveAnswerEffect("discard")).toEqual({ runSave: false, dropPending: true, continueNavigation: "now" });
    expect(leaveAnswerEffect("cancel")).toEqual({ runSave: false, dropPending: false, continueNavigation: "never" });
  });
});

describe("the crop", () => {
  it("at zoom 1 the shorter side fills the circle and the square is centred", () => {
    const sq = sourceSquare(2000, 1000, 300, INITIAL_CROP);
    expect(sq).toEqual({ sx: 500, sy: 0, side: 1000 });
  });

  it("panning is clamped so the circle stays covered", () => {
    // 2000x1000 at 300px: scale 0.3, picture 600x300 on screen, can move 150px.
    const c = dragCrop(2000, 1000, 300, INITIAL_CROP, 1000, 50);
    expect(c.panX).toBe(150);
    expect(c.panY).toBe(0);
    expect(sourceSquare(2000, 1000, 300, c).sx).toBe(0);
  });

  it("zoom is bounded to 1..4 and shrinks the kept square", () => {
    expect(zoomCrop(1000, 1000, 300, INITIAL_CROP, 10).zoom).toBe(4);
    expect(zoomCrop(1000, 1000, 300, INITIAL_CROP, 0.1).zoom).toBe(1);
    expect(sourceSquare(1000, 1000, 300, { zoom: 2, panX: 0, panY: 0 }).side).toBe(500);
    expect(clampCrop(1000, 1000, 300, { zoom: 2, panX: 9999, panY: -9999 })).toEqual({ zoom: 2, panX: 150, panY: -150 });
  });

  it("caps the output side", () => {
    expect(outputSide(1000, 640)).toBe(640);
    expect(outputSide(333, 480, true)).toBe(332);
    expect(outputSide(0.2, 640)).toBe(1);
  });

  it("the JPEG loop lowers quality, then the side, until it fits", async () => {
    const tried: string[] = [];
    const out = await encodeUnderLimit(640, async (side, q) => {
      tried.push(`${side}@${q}`);
      // Fits at 512 px and quality <= 0.7.
      return new Blob([new Uint8Array(side <= 512 && q <= 0.7 ? 100 : 1000)]);
    }, 500);
    expect(out?.side).toBe(512);
    expect(tried[0]).toBe("640@0.92");
    expect(tried.at(-1)).toBe("512@0.7");
  });

  it("the JPEG loop gives up below the smallest side", async () => {
    expect(await encodeUnderLimit(200, async () => new Blob([new Uint8Array(10)]), 5)).toBeNull();
  });

  it("chooses where the ten seconds start", () => {
    expect(clampVideoStart(5000, 8000)).toBe(0);
    expect(clampVideoStart(50_000, 30_000)).toBe(20_000);
    expect(keptVideoDurationMs(30_000, 25_000)).toBe(10_000);
    expect(keptVideoDurationMs(4_000, 0)).toBe(4_000);
    expect(keptVideoDurationMs(0, 0)).toBeNull();
  });

  it("accepts a video without re-encoding only when it already fits", () => {
    const ok = { mp4: true, width: 480, height: 480, bytes: 1000, durationMs: 9000, hasAudio: false, maxSide: 480, maxBytes: 4_194_304 };
    expect(usableWithoutReencode(ok)).toBe(true);
    expect(usableWithoutReencode({ ...ok, height: 470 })).toBe(false);
    expect(usableWithoutReencode({ ...ok, durationMs: 12_000 })).toBe(false);
    expect(usableWithoutReencode({ ...ok, hasAudio: null })).toBe(false);
    expect(usableWithoutReencode({ ...ok, mp4: false })).toBe(false);
  });
});
