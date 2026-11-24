// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Every picture of the call screen (`routes/Call.tsx`) reaches the screen in
 * the desktop app on macOS, whose WebKit plays video without putting it on
 * screen (`calls/video-canvas.ts`, `media/video-overlay.ts`).
 *
 * Seen on 2026-11-24: in the desktop app the sharer's own share tile stayed
 * black for a whole share while the others saw the picture. The app's log
 * read `view=picture` (the mirror rule had said to show it); the tile's
 * `<video>` was the one picture element of the call screen with no canvas,
 * and the only one given its stream inside its `ref`, before it was in the
 * document. The call screen is not loaded by this test runner (no DOM, no
 * Solid transform), so the rule is pinned on the source: each `<video>` there
 * is drawn through a canvas (a canvas with `lk-video-canvas` right after it,
 * or `paintVideoBehind` in its `ref`), and no `ref` binds a stream.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const source = readFileSync(resolve(process.cwd(), "src/routes/Call.tsx"), "utf8");

/** Each `<video ... />` JSX element: its tag and the text up to the next one. */
function videoElements(): Array<{ line: number; tag: string; after: string }> {
  const out: Array<{ line: number; tag: string; after: string }> = [];
  const re = /^\s*<video\b/gm;
  let m: RegExpExecArray | null;
  while ((m = re.exec(source)) !== null) {
    const start = m.index;
    const end = source.indexOf("/>", start);
    const tag = source.slice(start, end + 2);
    const nextVideo = source.slice(end).search(/^\s*<video\b/m);
    const after = source.slice(end, nextVideo < 0 ? end + 1500 : end + Math.min(nextVideo, 1500));
    out.push({ line: source.slice(0, start).split("\n").length, tag, after });
  }
  return out;
}

describe("the call screen's pictures", () => {
  const videos = videoElements();

  it("are found", () => {
    // The camera, the remote layers, the received share and the own share.
    expect(videos.length).toBeGreaterThanOrEqual(4);
  });

  it("each go through a canvas where the engine needs one", () => {
    for (const v of videos) {
      const throughCanvas = v.tag.includes("paintVideoBehind(") || v.after.includes("lk-video-canvas");
      expect(throughCanvas, `<video> at Call.tsx:${v.line} has no canvas`).toBe(true);
    }
  });

  it("are never given a stream in their ref, before they are in the document", () => {
    for (const v of videos) {
      expect(v.tag.includes("srcObject"), `<video> at Call.tsx:${v.line} binds in its ref`).toBe(false);
    }
  });

  it("include the own share's picture, bound like the others", () => {
    const own = /const OwnShareVideo: Component[\s\S]*?\n};/.exec(source)?.[0] ?? "";
    expect(own, "no OwnShareVideo").not.toBe("");
    expect(own).toContain("LocalPreviewBinder");
    expect(own).toContain("paintVideoBehind(");
    const tile = /const OwnShareTile: Component[\s\S]*?\n};/.exec(source)?.[0] ?? "";
    expect(tile).toContain("<OwnShareVideo");
  });
});
