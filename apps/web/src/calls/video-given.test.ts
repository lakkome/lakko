// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the SFU gives and what a tile therefore shows (ADR-0070 step 6,
 * `video-given.ts`). The frame parser is the real shared one, over wasm.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

import {
  claimsVideoGiven,
  effectiveQuality,
  heavyStreamForGiven,
  readVideoGivenFrame,
  shownQuality,
  VideoGivenBook,
  videoGivenChanges,
  videoGivenFromPairs,
} from "./video-given";
import {
  lowerQuality,
  VideoWantsBook,
  VideoWantsSender,
  type TimerDeps,
  type VideoQuality,
  type VideoWantsFrame,
} from "./video-wants";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const ALL: readonly VideoQuality[] = ["main", "middle", "light"];

function frame(given: unknown): string {
  return JSON.stringify({ kind: "video-given", given });
}

describe("reading a video-given frame", () => {
  it("reads the whole set, in the frame's order, through the shared helper", () => {
    const raw = frame([
      { peer: "p1", quality: "main" },
      { peer: "p2", quality: "middle" },
      { peer: "p3", quality: "light" },
    ]);
    expect(wasm.validateSfuServerFrame(raw)).toBe("video-given");
    expect(readVideoGivenFrame(wasm, raw)).toEqual([
      { peer: "p1", quality: "main" },
      { peer: "p2", quality: "middle" },
      { peer: "p3", quality: "light" },
    ]);
  });

  it("reads an empty set", () => {
    expect(readVideoGivenFrame(wasm, frame([]))).toEqual([]);
  });

  it("refuses an unknown quality, as a whole", () => {
    const raw = frame([
      { peer: "p1", quality: "main" },
      { peer: "p2", quality: "full" },
    ]);
    expect(() => wasm.validateSfuServerFrame(raw)).toThrow();
    expect(readVideoGivenFrame(wasm, raw)).toBeNull();
  });

  it("refuses a malformed frame without throwing", () => {
    const malformed = [
      "",
      "not json",
      "{",
      frame("main"),
      frame([{ peer: "p1" }]),
      frame([{ quality: "main" }]),
      frame([{ peer: "", quality: "main" }]),
      frame([{ peer: 7, quality: "main" }]),
      JSON.stringify({ kind: "video-given" }),
      // Another kind of frame is not a video-given one.
      JSON.stringify({ kind: "pong", nonce: 1 }),
      // More entries than the protocol allows (64).
      frame(Array.from({ length: 65 }, (_, i) => ({ peer: `p${i}`, quality: "main" }))),
    ];
    for (const raw of malformed) {
      expect(readVideoGivenFrame(wasm, raw), raw.slice(0, 40)).toBeNull();
    }
  });

  it("reads as many entries as the protocol allows", () => {
    const raw = frame(Array.from({ length: 64 }, (_, i) => ({ peer: `p${i}`, quality: "light" })));
    expect(readVideoGivenFrame(wasm, raw)).toHaveLength(64);
  });

  it("refuses a helper's answer that is not pairs of a peer and a known quality", () => {
    expect(videoGivenFromPairs([])).toEqual([]);
    expect(videoGivenFromPairs(["p1", "main"])).toEqual([{ peer: "p1", quality: "main" }]);
    expect(videoGivenFromPairs(["p1"])).toBeNull();
    expect(videoGivenFromPairs(["p1", "main", "p2"])).toBeNull();
    expect(videoGivenFromPairs(["p1", "best"])).toBeNull();
    expect(videoGivenFromPairs(["", "main"])).toBeNull();
    expect(readVideoGivenFrame({ parseSfuVideoGiven: () => ["p1", "huge"] }, "{}")).toBeNull();
    expect(
      readVideoGivenFrame(
        {
          parseSfuVideoGiven: () => {
            throw new Error("refused");
          },
        },
        "{}",
      ),
    ).toBeNull();
  });

  it("tells a refused frame that says it is a video-given one from any other", () => {
    expect(claimsVideoGiven(frame([{ peer: "p", quality: "full" }]))).toBe(true);
    expect(claimsVideoGiven(JSON.stringify({ kind: "video-given" }))).toBe(true);
    expect(claimsVideoGiven(JSON.stringify({ kind: "welcome" }))).toBe(false);
    expect(claimsVideoGiven("not json")).toBe(false);
    expect(claimsVideoGiven("null")).toBe(false);
    expect(claimsVideoGiven('"video-given"')).toBe(false);
  });
});

describe("the picture a tile shows", () => {
  it("is the lower of what it wants and what is given, in every combination", () => {
    const expected: Record<VideoQuality, Record<VideoQuality, VideoQuality>> = {
      main: { main: "main", middle: "middle", light: "light" },
      middle: { main: "middle", middle: "middle", light: "light" },
      light: { main: "light", middle: "light", light: "light" },
    };
    for (const wanted of ALL) {
      for (const given of ALL) {
        expect(effectiveQuality(wanted, given), `${wanted}/${given}`).toBe(expected[wanted][given]);
        expect(lowerQuality(wanted, given)).toBe(expected[wanted][given]);
      }
    }
  });

  it("is what it wants while the SFU has said nothing", () => {
    for (const wanted of ALL) {
      expect(effectiveQuality(wanted, undefined)).toBe(wanted);
      expect(effectiveQuality(wanted, null)).toBe(wanted);
    }
  });

  it("keeps the wanted heavy picture when light is given of a person with no light copy", () => {
    expect(shownQuality("main", "light", true)).toBe("light");
    expect(shownQuality("main", "light", false)).toBe("main");
    expect(shownQuality("middle", "light", false)).toBe("middle");
    // A tile that wants light of such a person asks for middle already.
    expect(shownQuality("light", "light", false)).toBe("middle");
    expect(shownQuality("light", undefined, false)).toBe("middle");
    expect(shownQuality("main", "middle", false)).toBe("middle");
  });
});

describe("the heavy stream of a tile", () => {
  const main = "main-stream";
  const middle = "middle-stream";

  it("is the main stream at main", () => {
    expect(heavyStreamForGiven("main", "main", main, middle, true)).toBe(main);
    expect(heavyStreamForGiven("main", undefined, main, middle, true)).toBe(main);
  });

  it("is the middle stream when middle is given to a tile that wants main", () => {
    expect(heavyStreamForGiven("main", "middle", main, middle, true)).toBe(middle);
    expect(heavyStreamForGiven("middle", "main", main, middle, true)).toBe(middle);
    expect(heavyStreamForGiven("middle", "middle", main, middle, true)).toBe(middle);
    expect(heavyStreamForGiven("middle", undefined, main, middle, true)).toBe(middle);
  });

  it("is the main stream at middle while the person has no middle track", () => {
    expect(heavyStreamForGiven("main", "middle", main, undefined, true)).toBe(main);
  });

  it("is none when light is given: the light stream alone, no heavy slot waiting", () => {
    expect(heavyStreamForGiven("main", "light", main, middle, true)).toBeUndefined();
    expect(heavyStreamForGiven("middle", "light", main, middle, true)).toBeUndefined();
    expect(heavyStreamForGiven("light", "main", main, middle, true)).toBeUndefined();
    expect(heavyStreamForGiven("light", undefined, main, middle, true)).toBeUndefined();
  });

  it("stays what the tile wants when light is given of a person with no light stream", () => {
    expect(heavyStreamForGiven("main", "light", main, middle, false)).toBe(main);
    expect(heavyStreamForGiven("middle", "light", main, middle, false)).toBe(middle);
  });
});

describe("the set the SFU gives", () => {
  it("says nothing before the first frame", () => {
    const book = new VideoGivenBook();
    expect(book.givenOf("a")).toBeUndefined();
    expect(book.size()).toBe(0);
    expect(effectiveQuality("main", book.givenOf("a"))).toBe("main");
  });

  it("says nothing of a person the latest frame does not name", () => {
    const book = new VideoGivenBook();
    book.replace([{ peer: "a", quality: "light" }]);
    expect(book.givenOf("a")).toBe("light");
    expect(book.givenOf("b")).toBeUndefined();
    expect(effectiveQuality("main", book.givenOf("b"))).toBe("main");
  });

  it("is replaced as a whole by a new frame", () => {
    const book = new VideoGivenBook();
    book.replace([
      { peer: "a", quality: "light" },
      { peer: "b", quality: "middle" },
    ]);
    book.replace([{ peer: "b", quality: "main" }]);
    expect(book.givenOf("a")).toBeUndefined();
    expect(book.givenOf("b")).toBe("main");
    expect([...book.snapshot()]).toEqual([["b", "main"]]);
    book.replace([]);
    expect(book.size()).toBe(0);
  });

  it("takes the later entry of a person named twice", () => {
    const book = new VideoGivenBook();
    book.replace([
      { peer: "a", quality: "main" },
      { peer: "a", quality: "light" },
    ]);
    expect(book.givenOf("a")).toBe("light");
    expect(book.size()).toBe(1);
  });

  it("forgets a person who left, and only them", () => {
    const book = new VideoGivenBook();
    book.replace([
      { peer: "a", quality: "light" },
      { peer: "b", quality: "middle" },
    ]);
    expect(book.removePeer("a")).toBe(true);
    expect(book.removePeer("a")).toBe(false);
    expect(book.givenOf("a")).toBeUndefined();
    expect(book.givenOf("b")).toBe("middle");
  });

  it("hands out a copy, and forgets everything when the call is over", () => {
    const book = new VideoGivenBook();
    book.replace([{ peer: "a", quality: "light" }]);
    const snap = book.snapshot();
    book.replace([{ peer: "a", quality: "main" }]);
    expect(snap.get("a")).toBe("light");
    book.clear();
    expect(book.size()).toBe(0);
    expect(book.givenOf("a")).toBeUndefined();
  });
});

describe("whose shown picture a frame changed", () => {
  const wants = new Map<string, VideoQuality>([
    ["a", "main"],
    ["b", "middle"],
    ["c", "light"],
  ]);
  const of = (m: Record<string, VideoQuality>) => (peer: string): VideoQuality | undefined => m[peer];

  it("names the people whose effective quality differs, with what is wanted and given", () => {
    expect(videoGivenChanges(wants, of({}), of({ a: "middle", b: "main", c: "light" }))).toEqual([
      { peer: "a", wanted: "main", given: "middle" },
    ]);
    expect(videoGivenChanges(wants, of({ a: "middle" }), of({ a: "light", b: "light" }))).toEqual([
      { peer: "a", wanted: "main", given: "light" },
      { peer: "b", wanted: "middle", given: "light" },
    ]);
  });

  it("names a person a frame stops naming, when that gives their picture back", () => {
    expect(videoGivenChanges(wants, of({ a: "light" }), of({}))).toEqual([
      { peer: "a", wanted: "main", given: null },
    ]);
  });

  it("names nobody when the frame changes no shown picture", () => {
    expect(videoGivenChanges(wants, of({}), of({ a: "main", b: "main", c: "main" }))).toEqual([]);
    expect(videoGivenChanges(wants, of({ a: "middle" }), of({ a: "middle" }))).toEqual([]);
    // More is given of someone only the light picture is wanted of.
    expect(videoGivenChanges(wants, of({ c: "light" }), of({ c: "main" }))).toEqual([]);
  });
});

describe("what this device asks for", () => {
  it("is not changed by what the SFU gives", () => {
    let now = 1_000;
    const timers: Array<() => void> = [];
    const deps: TimerDeps = {
      now: () => now,
      setTimeout: (fn) => timers.push(fn),
      clearTimeout: () => {},
    };
    const run = (): void => {
      now += 1_000;
      for (const fn of timers.splice(0)) fn();
    };
    const sent: VideoWantsFrame[] = [];
    const wantsBook = new VideoWantsBook();
    const sender = new VideoWantsSender(wantsBook, (f) => {
      sent.push(f);
      return true;
    }, deps);
    wantsBook.setPeers(["a", "b"]);
    wantsBook.setTile({}, "a", "main");
    wantsBook.setTile({}, "b", "middle");
    sender.changed();
    run();
    expect(sent).toEqual([{ peers: ["a", "b"], qualities: ["main", "middle"] }]);

    const given = new VideoGivenBook();
    given.replace([
      { peer: "a", quality: "light" },
      { peer: "b", quality: "light" },
    ]);
    expect(effectiveQuality(wantsBook.qualityOf("a"), given.givenOf("a"))).toBe("light");
    // The wish stands, and nothing new is sent for it.
    expect([...wantsBook.wants()]).toEqual([
      ["a", "main"],
      ["b", "middle"],
    ]);
    sender.changed();
    run();
    expect(sent).toHaveLength(1);
  });
});
