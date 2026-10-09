// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which picture a tile shows and what this device asks the SFU for
 * (`docs/ux/group-call-grid.md` section 7.6, ADR-0070 step 5). The rule is
 * the real shared one, over wasm.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

import {
  askedQuality,
  coversHeavySwap,
  heavyStreamFor,
  higherQuality,
  isVideoQuality,
  physicalTileWidth,
  SettledWidth,
  sharedTileQualityRules,
  TILE_WIDTH_SETTLE_MS,
  tileQualityOf,
  VIDEO_WANTS_MAX,
  VIDEO_WANTS_MIN_INTERVAL_MS,
  VideoWantsBook,
  videoWantsFrame,
  VideoWantsSender,
  type TimerDeps,
  type VideoQuality,
  type VideoWantsFrame,
} from "./video-wants";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

/** A clock and timers a test moves by hand. */
function fakeTimers(): TimerDeps & { advance(ms: number): void; pending(): number } {
  let now = 1_000;
  let seq = 0;
  const timers = new Map<number, { at: number; fn: () => void }>();
  return {
    now: () => now,
    setTimeout: (fn, ms) => {
      seq += 1;
      timers.set(seq, { at: now + ms, fn });
      return seq;
    },
    clearTimeout: (handle) => {
      timers.delete(handle as number);
    },
    pending: () => timers.size,
    advance(ms: number): void {
      const until = now + ms;
      for (;;) {
        const due = [...timers].filter(([, t]) => t.at <= until).sort((a, b) => a[1].at - b[1].at)[0];
        if (due === undefined) break;
        timers.delete(due[0]);
        now = Math.max(now, due[1].at);
        due[1].fn();
      }
      now = until;
    },
  };
}

describe("the three qualities", () => {
  it("are the names the video-wants frame uses", () => {
    expect(isVideoQuality("main")).toBe(true);
    expect(isVideoQuality("middle")).toBe(true);
    expect(isVideoQuality("light")).toBe(true);
    expect(isVideoQuality("full")).toBe(false);
    expect(isVideoQuality(undefined)).toBe(false);
    // And the names the shared rule gives its three answers.
    expect(wasm.tileVideoQualityName(wasm.TileVideoQuality.Main)).toBe("main");
    expect(wasm.tileVideoQualityName(wasm.TileVideoQuality.Middle)).toBe("middle");
    expect(wasm.tileVideoQualityName(wasm.TileVideoQuality.Light)).toBe("light");
  });

  it("rank main over middle over light", () => {
    const all: VideoQuality[] = ["main", "middle", "light"];
    for (const q of all) expect(higherQuality(q, q)).toBe(q);
    expect(higherQuality("light", "middle")).toBe("middle");
    expect(higherQuality("middle", "light")).toBe("middle");
    expect(higherQuality("middle", "main")).toBe("main");
    expect(higherQuality("main", "light")).toBe("main");
  });
});

describe("which picture a tile shows", () => {
  it("is the shared rule's answer", () => {
    const main = wasm.tileMainMinWidthPx();
    const light = wasm.tileLightMaxWidthPx();
    // The thresholds the spec names (section 7.6).
    expect([main, light]).toEqual([960, 240]);
    // A call of two: the full picture at any size.
    expect(tileQualityOf(wasm, 2, 100, false)).toBe("main");
    expect(tileQualityOf(wasm, 2, 4000, false)).toBe("main");
    // Three or more: by width, and the full one only for a wide pinned or
    // stage tile.
    expect(tileQualityOf(wasm, 3, light - 1, false)).toBe("light");
    expect(tileQualityOf(wasm, 3, light, false)).toBe("middle");
    expect(tileQualityOf(wasm, 3, main, false)).toBe("middle");
    expect(tileQualityOf(wasm, 3, main, true)).toBe("main");
    expect(tileQualityOf(wasm, 3, main - 1, true)).toBe("middle");
    expect(tileQualityOf(wasm, 10, light - 1, true)).toBe("light");
    // Asked through the module's own copy of the rules too.
    expect(tileQualityOf(sharedTileQualityRules, 3, light, false)).toBe("middle");
  });

  it("is main while the rule cannot be asked, or answers something else", () => {
    const broken = {
      tileVideoQuality: (): never => {
        throw new Error("wasm not ready");
      },
      tileVideoQualityName: (): string => "main",
    };
    expect(tileQualityOf(broken as never, 5, 100, false)).toBe("main");
    const odd = { tileVideoQuality: (): number => 9, tileVideoQualityName: (): string => "tiny" };
    expect(tileQualityOf(odd as never, 5, 100, false)).toBe("main");
  });

  it("is asked in physical pixels: the CSS width times the pixel ratio", () => {
    expect(physicalTileWidth(200, 1)).toBe(200);
    expect(physicalTileWidth(200, 3)).toBe(600);
    expect(physicalTileWidth(199.6, 1.5)).toBe(299);
    // A ratio the browser does not give is 1; no width is no width.
    expect(physicalTileWidth(200, Number.NaN)).toBe(200);
    expect(physicalTileWidth(200, 0)).toBe(200);
    expect(physicalTileWidth(0, 2)).toBe(0);
    expect(physicalTileWidth(-5, 2)).toBe(0);
    // The same tile on a 1x laptop and on a 3x phone asks for different pictures.
    expect(tileQualityOf(wasm, 4, physicalTileWidth(200, 1), false)).toBe("light");
    expect(tileQualityOf(wasm, 4, physicalTileWidth(200, 3), false)).toBe("middle");
  });
});

describe("what the heavy element plays", () => {
  const mainStream = { id: "main" };
  const middleStream = { id: "middle" };

  it("is the main stream at main, the middle stream at middle, nothing at light", () => {
    expect(heavyStreamFor("main", mainStream, middleStream)).toBe(mainStream);
    expect(heavyStreamFor("middle", mainStream, middleStream)).toBe(middleStream);
    expect(heavyStreamFor("light", mainStream, middleStream)).toBeUndefined();
  });

  it("falls back to the main stream at middle when the person has no middle track", () => {
    expect(heavyStreamFor("middle", mainStream, undefined)).toBe(mainStream);
    // Never the other way round.
    expect(heavyStreamFor("main", mainStream, undefined)).toBe(mainStream);
    expect(heavyStreamFor("main", undefined, middleStream)).toBeUndefined();
  });

  it("is not left empty for a person who sends no light copy: such a tile asks for middle", () => {
    expect(askedQuality("light", true)).toBe("light");
    expect(askedQuality("light", false)).toBe("middle");
    expect(askedQuality("middle", false)).toBe("middle");
    expect(askedQuality("main", false)).toBe("main");
    // And with no middle track either, that is the main stream.
    expect(heavyStreamFor(askedQuality("light", false), mainStream, undefined)).toBe(mainStream);
  });

  it("is covered by the light copy from a swap until its first frame, where there is one", () => {
    expect(coversHeavySwap(true, true)).toBe(true);
    expect(coversHeavySwap(false, true)).toBe(false);
    expect(coversHeavySwap(true, false)).toBe(false);
  });
});

describe("VideoWantsBook", () => {
  it("wants light for a person no tile shows", () => {
    const book = new VideoWantsBook();
    book.setPeers(["a", "b"]);
    expect([...book.wants()]).toEqual([
      ["a", "light"],
      ["b", "light"],
    ]);
    expect(book.qualityOf("a")).toBe("light");
    expect(book.people()).toBe(3);
  });

  it("wants the highest of a person's mounted tiles", () => {
    const book = new VideoWantsBook();
    book.setPeers(["a", "b"]);
    const strip = {};
    const stage = {};
    expect(book.setTile(strip, "a", "light")).toBe(true);
    expect(book.setTile(stage, "a", "main")).toBe(true);
    expect(book.setTile({}, "b", "middle")).toBe(true);
    expect([...book.wants()]).toEqual([
      ["a", "main"],
      ["b", "middle"],
    ]);
    // The stage tile unmounts.
    expect(book.setTile(stage, "a", null)).toBe(true);
    expect(book.qualityOf("a")).toBe("light");
    expect(book.setTile(strip, "a", "middle")).toBe(true);
    expect(book.qualityOf("a")).toBe("middle");
  });

  it("says when a report changed nothing", () => {
    const book = new VideoWantsBook();
    const tile = {};
    expect(book.setTile(tile, "a", "main")).toBe(true);
    expect(book.setTile(tile, "a", "main")).toBe(false);
    expect(book.setTile(tile, "a", null)).toBe(true);
    expect(book.setTile(tile, "a", null)).toBe(false);
    expect(book.setTile({}, "a", null)).toBe(false);
  });

  it("names only the people of the call, joined and left", () => {
    const book = new VideoWantsBook();
    book.setPeers(["a"]);
    const tb = {};
    // A tile can report before its person is in the roster.
    book.setTile(tb, "b", "main");
    expect([...book.wants().keys()]).toEqual(["a"]);
    book.addPeer("b");
    expect([...book.wants()]).toEqual([
      ["a", "light"],
      ["b", "main"],
    ]);
    book.removePeer("a");
    expect([...book.wants()]).toEqual([["b", "main"]]);
    expect(book.people()).toBe(2);
    book.clear();
    expect(book.wants().size).toBe(0);
    expect(book.people()).toBe(1);
    expect(book.qualityOf("b")).toBe("light");
  });

  it("lets one tile move from one person to another", () => {
    const book = new VideoWantsBook();
    book.setPeers(["a", "b"]);
    const tile = {};
    book.setTile(tile, "a", "main");
    book.setTile(tile, "b", "main");
    expect([...book.wants()]).toEqual([
      ["a", "light"],
      ["b", "main"],
    ]);
  });
});

describe("the frame", () => {
  it("is the two parallel lists composeSfuVideoWants takes, and the helper takes it", () => {
    const frame = videoWantsFrame(
      new Map<string, VideoQuality>([
        ["a", "main"],
        ["b", "middle"],
        ["c", "light"],
      ]),
    );
    expect(frame).toEqual({ peers: ["a", "b", "c"], qualities: ["main", "middle", "light"] });
    expect(JSON.parse(wasm.composeSfuVideoWants(frame.peers, frame.qualities))).toEqual({
      kind: "video-wants",
      wants: [
        { peer: "a", quality: "main" },
        { peer: "b", quality: "middle" },
        { peer: "c", quality: "light" },
      ],
    });
  });

  it("stays within the protocol's bound: past 64 people the main entries are the ones left out", () => {
    expect(VIDEO_WANTS_MAX).toBe(64);
    const wants = new Map<string, VideoQuality>();
    for (let i = 0; i < 70; i += 1) wants.set(`p${i}`, i < 10 ? "main" : "light");
    const frame = videoWantsFrame(wants);
    expect(frame.peers).toHaveLength(60);
    expect(frame.qualities.every((q) => q === "light")).toBe(true);
    // The SFU reads a person the frame leaves out as main, which is what they were.
    expect(() => wasm.composeSfuVideoWants(frame.peers, frame.qualities)).not.toThrow();

    const many = new Map<string, VideoQuality>();
    for (let i = 0; i < 100; i += 1) many.set(`p${i}`, "light");
    const cut = videoWantsFrame(many);
    expect(cut.peers).toHaveLength(64);
    expect(cut.peers[0]).toBe("p0");
    expect(() => wasm.composeSfuVideoWants(cut.peers, cut.qualities)).not.toThrow();
    // One more than the bound is what the helper refuses.
    const over = [...many.keys()].slice(0, 65);
    expect(() => wasm.composeSfuVideoWants(over, over.map(() => "light"))).toThrow();
  });
});

describe("VideoWantsSender", () => {
  function setup(sendOk: () => boolean = () => true) {
    const timers = fakeTimers();
    const book = new VideoWantsBook();
    const sent: VideoWantsFrame[] = [];
    const sender = new VideoWantsSender(
      book,
      (frame) => {
        if (!sendOk()) return false;
        sent.push(frame);
        return true;
      },
      timers,
    );
    return { timers, book, sent, sender };
  }

  it("sends the whole set when it changed", () => {
    const { timers, book, sent, sender } = setup();
    book.setPeers(["a", "b"]);
    book.setTile({}, "a", "middle");
    sender.changed();
    expect(sent).toEqual([]);
    timers.advance(0);
    expect(sent).toEqual([{ peers: ["a", "b"], qualities: ["middle", "light"] }]);
  });

  it("makes one frame of the reports of one moment", () => {
    const { timers, book, sent, sender } = setup();
    book.setPeers(["a", "b", "c"]);
    for (const peer of ["a", "b", "c"]) {
      book.setTile({}, peer, "middle");
      sender.changed();
    }
    timers.advance(0);
    expect(sent).toHaveLength(1);
    expect(sent[0]?.qualities).toEqual(["middle", "middle", "middle"]);
  });

  it("sends at most one frame per 200 ms, the later one carrying the latest set", () => {
    expect(VIDEO_WANTS_MIN_INTERVAL_MS).toBe(200);
    const { timers, book, sent, sender } = setup();
    book.setPeers(["a"]);
    const tile = {};
    book.setTile(tile, "a", "middle");
    sender.changed();
    timers.advance(0);
    expect(sent).toHaveLength(1);

    book.setTile(tile, "a", "main");
    sender.changed();
    timers.advance(100);
    book.setTile(tile, "a", "light");
    sender.changed();
    timers.advance(99);
    expect(sent).toHaveLength(1);
    timers.advance(1);
    expect(sent).toHaveLength(2);
    expect(sent[1]).toEqual({ peers: ["a"], qualities: ["light"] });
    expect(timers.pending()).toBe(0);
  });

  it("sends nothing when the set is the one last sent", () => {
    const { timers, book, sent, sender } = setup();
    book.setPeers(["a"]);
    const tile = {};
    book.setTile(tile, "a", "middle");
    sender.changed();
    timers.advance(0);
    book.setTile(tile, "a", "main");
    book.setTile(tile, "a", "middle");
    sender.changed();
    timers.advance(1_000);
    expect(sent).toHaveLength(1);
  });

  it("resends the set whatever it is, still no closer than 200 ms", () => {
    const { timers, book, sent, sender } = setup();
    book.setPeers(["a"]);
    sender.resend();
    timers.advance(0);
    expect(sent).toEqual([{ peers: ["a"], qualities: ["light"] }]);
    sender.resend();
    sender.resend();
    timers.advance(199);
    expect(sent).toHaveLength(1);
    timers.advance(1);
    expect(sent).toHaveLength(2);
    expect(sent[1]).toEqual(sent[0]);
    // Once: the resend was used up.
    sender.changed();
    timers.advance(1_000);
    expect(sent).toHaveLength(2);
  });

  it("never sends an empty set", () => {
    const { timers, sent, sender } = setup();
    sender.resend();
    timers.advance(1_000);
    expect(sent).toEqual([]);
  });

  it("tries again later with a frame that could not go", () => {
    let open = false;
    const { timers, book, sent, sender } = setup(() => open);
    book.setPeers(["a"]);
    sender.resend();
    timers.advance(0);
    expect(sent).toEqual([]);
    // Nothing spins while it cannot go.
    expect(timers.pending()).toBe(0);
    open = true;
    sender.changed();
    timers.advance(0);
    expect(sent).toEqual([{ peers: ["a"], qualities: ["light"] }]);
  });

  it("sends nothing after a reset, and starts afresh", () => {
    const { timers, book, sent, sender } = setup();
    book.setPeers(["a"]);
    sender.resend();
    sender.reset();
    timers.advance(1_000);
    expect(sent).toEqual([]);
    expect(timers.pending()).toBe(0);
    sender.changed();
    timers.advance(0);
    expect(sent).toHaveLength(1);
  });
});

describe("SettledWidth", () => {
  it("uses a tile's first width at once", () => {
    const timers = fakeTimers();
    const applied: number[] = [];
    const settled = new SettledWidth((w) => applied.push(w), timers);
    settled.observe(480);
    expect(applied).toEqual([480]);
    expect(timers.pending()).toBe(0);
  });

  it("uses a later width only once it has stood for 300 ms, so a drag asks once", () => {
    expect(TILE_WIDTH_SETTLE_MS).toBe(300);
    const timers = fakeTimers();
    const applied: number[] = [];
    const settled = new SettledWidth((w) => applied.push(w), timers);
    settled.observe(480);
    for (const w of [470, 400, 300, 250, 230]) {
      settled.observe(w);
      timers.advance(50);
    }
    expect(applied).toEqual([480]);
    timers.advance(249);
    expect(applied).toEqual([480]);
    timers.advance(1);
    expect(applied).toEqual([480, 230]);
  });

  it("applies nothing when the width came back to where it was", () => {
    const timers = fakeTimers();
    const applied: number[] = [];
    const settled = new SettledWidth((w) => applied.push(w), timers);
    settled.observe(480);
    settled.observe(300);
    timers.advance(100);
    settled.observe(480);
    timers.advance(1_000);
    expect(applied).toEqual([480]);
    expect(timers.pending()).toBe(0);
  });

  it("applies nothing after it is disposed", () => {
    const timers = fakeTimers();
    const applied: number[] = [];
    const settled = new SettledWidth((w) => applied.push(w), timers);
    settled.observe(480);
    settled.observe(200);
    settled.dispose();
    timers.advance(1_000);
    expect(applied).toEqual([480]);
  });
});
