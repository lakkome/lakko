// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The pin control of a group call and the large tile it holds
 * (`docs/ux/group-call-grid.md` sections 6.1 and 6.2).
 *
 * Reported from a live five-person call: in the Focus layout there was no
 * way to choose whom to pin. A press anywhere on a tile did pin it, but
 * nothing on the screen said so: the tile was a plain element with no
 * button, no name and no place in the keyboard order.
 *
 * The rule now: pinning and unpinning happen only through the pin button of
 * a tile. A press on a tile itself never pins (it was too easy to pin
 * someone by accident); it only brings that tile's controls up for a few
 * seconds, which is how a touch screen reaches the button.
 *
 * The component, the store and the shared rules are the real ones; only the
 * browser facts jsdom lacks (layout, media) are stood in for. What this
 * cannot show is whether the button is where a pointer can reach it: jsdom
 * computes no layout and no stacking.
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route } from "@solidjs/router";
import { createSignal } from "solid-js";
import { render } from "solid-js/web";

import { callActions, callState, type Participant } from "../state/call-state";
import { all, initWasm, installStylesheet, one, pointer, present, settle } from "../test/ui";
import { TILE_CONTROLS_REVEAL_MS } from "../calls/tile-controls-reveal";
import { VideoWantsBook, type VideoQuality } from "../calls/video-wants";

type CallComponent = typeof import("./Call")["default"];
let Call: CallComponent;

/** Every tile measures this wide: wide enough for the rule to answer `main`
 *  for a pinned or stage tile and `middle` for any other. */
const TILE_WIDTH = 1000;

beforeAll(async () => {
  class FakeResizeObserver {
    constructor(private readonly cb: ResizeObserverCallback) {}
    observe(): void {
      this.cb(
        [{ contentRect: { width: 1200, height: 700 } } as ResizeObserverEntry],
        this as unknown as ResizeObserver,
      );
    }
    unobserve(): void {}
    disconnect(): void {}
  }
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    () => ({ width: TILE_WIDTH, height: 300, left: 0, top: 0, right: TILE_WIDTH, bottom: 300, x: 0, y: 0 }) as DOMRect,
  );
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(async () => {});
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
  await initWasm();
  Call = (await import("./Call")).default;
  await settle();
});

beforeEach(() => {
  callActions.reset();
});

function person(id: string, over: Partial<Participant> = {}): Participant {
  return {
    id,
    displayName: id,
    isLocal: false,
    micOn: true,
    camOn: true,
    quality: "good",
    speaking: false,
    ...over,
  };
}

function fakeStream(): MediaStream {
  return {
    getTracks: () => [],
    getVideoTracks: () => [],
    getAudioTracks: () => [],
  } as unknown as MediaStream;
}

let dispose: (() => void) | null = null;
afterEach(() => {
  dispose?.();
  dispose = null;
  document.body.replaceChildren();
});

/** A connected five-person group call: this device and Anna, Ben, Cleo, Dan. */
function mount(): VideoWantsBook {
  const names = ["Anna", "Ben", "Cleo", "Dan"];
  const streams = new Map(names.map((n) => [`peer-${n}`, fakeStream()]));
  callActions.setParticipants([
    person("local", { isLocal: true, displayName: "Me", camOn: false }),
    ...names.map((n) => person(`peer-${n}`, { displayName: n })),
  ]);
  callActions.setPhase({ kind: "connected" });
  const book = new VideoWantsBook();
  book.setPeers(names);
  const host = document.createElement("div");
  document.body.appendChild(host);
  dispose = render(
    () => (
      <MemoryRouter>
        <Route
          path="*"
          component={() => (
            <Call
              embedded={true}
              group={true}
              onExit={() => {}}
              remoteStreamFor={(id) => streams.get(id)}
              reportTileVideoQuality={(tile, participantId, quality) => {
                book.setTile(tile, participantId.replace(/^peer-/, ""), quality);
              }}
            />
          )}
        />
      </MemoryRouter>
    ),
    host,
  );
  return book;
}

const layout = (): string | null => one(".lk-call-main").getAttribute("data-layout");
const stageName = (): string => one(".lk-call-stage .lk-call-tile-name").textContent ?? "";
const stripNames = (): string[] =>
  all(".lk-call-strip .lk-call-tile-name").map((el) => el.textContent ?? "");
/** The tile of the person called `name`. */
function tileOf(name: string): HTMLElement {
  const tile = all<HTMLElement>(".lk-call-tile").find(
    (el) => el.querySelector(".lk-call-tile-name")?.textContent === name,
  );
  expect(tile, `no tile for ${name}`).toBeDefined();
  return tile as HTMLElement;
}
const pinButtonOf = (name: string): HTMLButtonElement =>
  one<HTMLButtonElement>(".lk-call-tile-pin-btn", tileOf(name));
const wants = (book: VideoWantsBook): Record<string, VideoQuality> =>
  Object.fromEntries(book.wants());

describe("the pin button in the grid", () => {
  it("is on every other person's tile, named for the person, and not on this device's own", async () => {
    mount();
    await settle();
    expect(layout()).toBe("grid");
    for (const name of ["Anna", "Ben", "Cleo", "Dan"]) {
      const button = pinButtonOf(name);
      expect(button.tagName).toBe("BUTTON");
      expect(button.getAttribute("aria-label")).toBe(`Pin ${name}`);
      expect(button.getAttribute("aria-pressed")).toBe("false");
    }
    expect(present(".lk-call-tile-pin-btn", tileOf("Me"))).toBe(false);
    // A tile is never itself the control: a press on it pins nobody.
    expect(tileOf("Ben").getAttribute("role")).toBeNull();
    expect(pinButtonOf("Ben").classList.contains("compact")).toBe(false);
    tileOf("Ben").click();
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(layout()).toBe("grid");
  });

  it("pins the person, switches to the stage view and puts them on the stage", async () => {
    mount();
    await settle();
    pinButtonOf("Cleo").click();
    await settle();
    expect(callState.pinnedId()).toBe("peer-Cleo");
    expect(layout()).toBe("focus");
    expect(stageName()).toBe("Cleo");
    expect(stripNames()).toEqual(["Anna", "Ben", "Dan", "Me"]);
    const button = pinButtonOf("Cleo");
    expect(button.getAttribute("aria-pressed")).toBe("true");
    expect(button.getAttribute("aria-label")).toBe("Unpin Cleo");
    expect(tileOf("Cleo").classList.contains("pinned")).toBe(true);
    expect(present(".lk-call-tile-pin", tileOf("Cleo"))).toBe(true);
  });

  it("unpins from the pinned tile's button and stays in the stage view", async () => {
    mount();
    await settle();
    pinButtonOf("Cleo").click();
    await settle();
    pinButtonOf("Cleo").click();
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(layout()).toBe("focus");
    // Nobody has spoken: the first person with a picture.
    expect(stageName()).toBe("Anna");
  });
});

describe("the stage view", () => {
  async function inFocus(): Promise<VideoWantsBook> {
    const book = mount();
    await settle();
    one<HTMLElement>(".lk-call-layout").click();
    await settle();
    expect(layout()).toBe("focus");
    return book;
  }

  it("does not pin on a press on a strip tile: only its button does", async () => {
    await inFocus();
    expect(stageName()).toBe("Anna");
    const dan = tileOf("Dan");
    // The tile is not a control of any kind.
    expect(dan.getAttribute("role")).toBeNull();
    expect(dan.getAttribute("tabindex")).toBeNull();
    expect(dan.getAttribute("aria-pressed")).toBeNull();
    dan.click();
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(stageName()).toBe("Anna");
    // Neither does a press on the picture or the name inside it.
    one<HTMLElement>(".lk-call-tile-name", dan).click();
    one<HTMLElement>("video", dan).click();
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(stageName()).toBe("Anna");

    const button = pinButtonOf("Dan");
    expect(button.classList.contains("compact")).toBe(true);
    expect(button.getAttribute("aria-label")).toBe("Pin Dan");
    button.click();
    await settle();
    expect(callState.pinnedId()).toBe("peer-Dan");
    expect(stageName()).toBe("Dan");
    expect(stripNames()).toEqual(["Anna", "Ben", "Cleo", "Me"]);
  });

  it("does not pin on Enter or Space on a strip tile; the button is what the keyboard reaches", async () => {
    await inFocus();
    for (const key of ["Enter", " "]) {
      tileOf("Ben").dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
    }
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(stageName()).toBe("Anna");
    // A real button: in the tab order by itself, and a browser turns Enter
    // and Space on it into the click this is.
    const button = pinButtonOf("Ben");
    expect(button.tagName).toBe("BUTTON");
    expect(button.getAttribute("type")).toBe("button");
    expect(button.hasAttribute("disabled")).toBe(false);
    expect(button.tabIndex).toBe(0);
    button.focus();
    expect(document.activeElement).toBe(button);
    button.click();
    await settle();
    expect(stageName()).toBe("Ben");
  });

  it("does not change the stage on a press on the stage tile either", async () => {
    await inFocus();
    tileOf("Anna").click();
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(stageName()).toBe("Anna");
  });

  it("has a button on the stage tile to pin whoever is on it and to unpin", async () => {
    await inFocus();
    const button = pinButtonOf("Anna");
    expect(button.getAttribute("aria-label")).toBe("Pin Anna");
    button.click();
    await settle();
    expect(callState.pinnedId()).toBe("peer-Anna");
    expect(pinButtonOf("Anna").getAttribute("aria-pressed")).toBe("true");
    pinButtonOf("Anna").click();
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(layout()).toBe("focus");
  });

  it("moves the pin to another person: one pin at a time", async () => {
    await inFocus();
    pinButtonOf("Ben").click();
    await settle();
    pinButtonOf("Dan").click();
    await settle();
    expect(callState.pinnedId()).toBe("peer-Dan");
    expect(stageName()).toBe("Dan");
    expect(all(".lk-call-tile.pinned")).toHaveLength(1);
  });

  it("gives this device's own strip tile no way to be pinned", async () => {
    await inFocus();
    const mine = tileOf("Me");
    expect(present(".lk-call-tile-pin-btn", mine)).toBe(false);
    mine.click();
    expect(mine.classList.contains("controls-shown")).toBe(false);
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(stageName()).toBe("Anna");
  });

  it("follows the floor with nobody pinned, and a pin outranks it", async () => {
    await inFocus();
    callActions.setSpeech(new Set(["peer-Cleo"]), {
      activeSpeakerId: "peer-Cleo",
      lastSpokeAtMs: { "peer-Cleo": 1_000 },
    });
    await settle();
    expect(stageName()).toBe("Cleo");
    expect(tileOf("Cleo").classList.contains("speaking")).toBe(true);
    // Cleo stops; nobody else has the floor: the stage stays on her.
    callActions.setSpeech(new Set(), { activeSpeakerId: null, lastSpokeAtMs: { "peer-Cleo": 2_000 } });
    await settle();
    expect(stageName()).toBe("Cleo");
    // Dan takes the floor.
    callActions.setSpeech(new Set(["peer-Dan"]), {
      activeSpeakerId: "peer-Dan",
      lastSpokeAtMs: { "peer-Cleo": 2_000, "peer-Dan": 3_000 },
    });
    await settle();
    expect(stageName()).toBe("Dan");
    // Ben is pinned while Dan speaks.
    pinButtonOf("Ben").click();
    await settle();
    expect(stageName()).toBe("Ben");
    // Unpinned: back to the person speaking.
    pinButtonOf("Ben").click();
    await settle();
    expect(stageName()).toBe("Dan");
  });

  it("clears the pin when the pinned person leaves and returns to the speaker", async () => {
    await inFocus();
    callActions.setSpeech(new Set(["peer-Dan"]), {
      activeSpeakerId: "peer-Dan",
      lastSpokeAtMs: { "peer-Dan": 3_000 },
    });
    pinButtonOf("Ben").click();
    await settle();
    expect(stageName()).toBe("Ben");
    callActions.participantLeft("peer-Ben");
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(layout()).toBe("focus");
    expect(stageName()).toBe("Dan");
    expect(stripNames()).toEqual(["Anna", "Cleo", "Me"]);
  });

  it("clears the pin when the roster is replaced without the pinned person", async () => {
    await inFocus();
    pinButtonOf("Ben").click();
    await settle();
    callActions.setParticipants(callState.participants().filter((p) => p.id !== "peer-Ben"));
    await settle();
    expect(callState.pinnedId()).toBeNull();
  });

  it("asks for the main picture of the person on the stage and of nobody else", async () => {
    const book = await inFocus();
    expect(wants(book)).toEqual({ Anna: "main", Ben: "middle", Cleo: "middle", Dan: "middle" });
    pinButtonOf("Dan").click();
    await settle();
    expect(wants(book)).toEqual({ Anna: "middle", Ben: "middle", Cleo: "middle", Dan: "main" });
  });

  it("unpins and returns to the grid from the layout button", async () => {
    await inFocus();
    pinButtonOf("Ben").click();
    await settle();
    one<HTMLElement>(".lk-call-layout").click();
    await settle();
    expect(callState.pinnedId()).toBeNull();
    expect(layout()).toBe("grid");
  });
});

/**
 * Each of these threw inside Solid's own disposal before ("Cannot read
 * properties of null (reading '2')"), with the update half applied: a
 * picture's teardown read a memo, which re-ran the computation that was in
 * the middle of taking it down. Seen with the committed screen in this
 * harness: a press on a grid tile left the grid up with a pin nobody could
 * see, and a person leaving the large tile left their tile where it was.
 */
describe("taking down a tile that draws a picture", () => {
  it("survives a pin set while the grid is up", async () => {
    mount();
    await settle();
    expect(layout()).toBe("grid");
    expect(() => callActions.pin("peer-Cleo")).not.toThrow();
    await settle();
    expect(layout()).toBe("focus");
    expect(stageName()).toBe("Cleo");
    expect(stripNames()).toEqual(["Anna", "Ben", "Dan", "Me"]);
  });

  it("survives the person on the large tile leaving, with nobody pinned", async () => {
    mount();
    await settle();
    one<HTMLElement>(".lk-call-layout").click();
    await settle();
    expect(stageName()).toBe("Anna");
    expect(() => callActions.participantLeft("peer-Anna")).not.toThrow();
    await settle();
    expect(stageName()).toBe("Ben");
    expect(stripNames()).toEqual(["Cleo", "Dan", "Me"]);
  });

  it("survives the pinned person leaving", async () => {
    mount();
    await settle();
    one<HTMLElement>(".lk-call-layout").click();
    await settle();
    pinButtonOf("Ben").click();
    await settle();
    expect(() => callActions.participantLeft("peer-Ben")).not.toThrow();
    await settle();
    expect(stageName()).toBe("Anna");
    expect(all(".lk-call-tile")).toHaveLength(4);
  });

  it("keeps following the call afterwards", async () => {
    mount();
    await settle();
    callActions.pin("peer-Cleo");
    await settle();
    pinButtonOf("Cleo").click();
    await settle();
    // The pin came from the store, not from a pin control, so no layout was
    // chosen: without the pin the grid is back.
    expect(layout()).toBe("grid");
    one<HTMLElement>(".lk-call-layout").click();
    await settle();
    callActions.setSpeech(new Set(["peer-Dan"]), {
      activeSpeakerId: "peer-Dan",
      lastSpokeAtMs: { "peer-Dan": 5_000 },
    });
    await settle();
    expect(stageName()).toBe("Dan");
    callActions.participantJoined(person("peer-Eve", { displayName: "Eve" }));
    await settle();
    expect(stripNames()).toEqual(["Anna", "Ben", "Cleo", "Eve", "Me"]);
  });
});

describe("the store's pin", () => {
  it("takes only someone else who is in the call", () => {
    callActions.setParticipants([person("local", { isLocal: true }), person("peer-b")]);
    callActions.pin("peer-b");
    expect(callState.pinnedId()).toBe("peer-b");
    callActions.pin("");
    expect(callState.pinnedId()).toBeNull();
    callActions.pin("local");
    expect(callState.pinnedId()).toBeNull();
    callActions.pin("peer-gone");
    expect(callState.pinnedId()).toBeNull();
  });
});

/**
 * The whole path on the screen: the call's stream map, the receiver lookup,
 * the monitor, the store, the ring on the tile and the large tile.
 */
describe("a peer whose audio arrives after their video, in a later track event", () => {
  it("is measured, gets the ring and comes to the large tile", async () => {
    const stream = fakeStream();
    // What `sfu-call-ui.ts` does on every track event: a new map holding the
    // same stream object.
    const [streams, setStreams] = createSignal<ReadonlyMap<string, MediaStream>>(
      new Map([["peer-Ben", stream], ["peer-Anna", fakeStream()]]),
    );
    const receivers = new Map<string, { getSynchronizationSources(): unknown[]; getStats(): Promise<never> }>();
    callActions.setParticipants([
      person("local", { isLocal: true, displayName: "Me", camOn: false, micOn: false }),
      person("peer-Anna", { displayName: "Anna" }),
      person("peer-Ben", { displayName: "Ben" }),
    ]);
    callActions.setPhase({ kind: "connected" });
    callActions.setLayoutChoice("focus");
    const host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <MemoryRouter>
          <Route
            path="*"
            component={() => (
              <Call
                embedded={true}
                group={true}
                onExit={() => {}}
                remoteStreamFor={(id) => streams().get(id)}
                remoteAudioReceiverFor={(id) => receivers.get(id) as never}
              />
            )}
          />
        </MemoryRouter>
      ),
      host,
    );
    await settle();
    expect(stageName()).toBe("Anna");

    // Ben's audio track event: the receiver exists, the map is replaced.
    let stamp = 0;
    receivers.set("peer-Ben", {
      getSynchronizationSources: () => {
        stamp += 20;
        return [{ audioLevel: 0.4, timestamp: stamp }];
      },
      getStats: () => Promise.reject(new Error("the source path never asks")),
    });
    setStreams(new Map(streams()));
    // Real time: the ring needs 200 ms of sound, the floor 1.5 s.
    await new Promise((r) => setTimeout(r, 600));
    expect(tileOf("Ben").classList.contains("speaking")).toBe(true);
    expect(stageName()).toBe("Anna");
    await new Promise((r) => setTimeout(r, 1_500));
    expect(callState.speech().activeSpeakerId).toBe("peer-Ben");
    expect(stageName()).toBe("Ben");
  });
});

describe("a tap on a tile, as on a touch screen", () => {
  const shown = (name: string): boolean => tileOf(name).classList.contains("controls-shown");

  it("only brings the tile's controls up; the second tap, on the button, pins", async () => {
    mount();
    await settle();
    const ben = tileOf("Ben");
    expect(shown("Ben")).toBe(false);
    pointer(ben, "pointerdown");
    ben.click();
    await settle();
    expect(shown("Ben")).toBe(true);
    expect(callState.pinnedId()).toBeNull();
    expect(layout()).toBe("grid");
    const button = pinButtonOf("Ben");
    pointer(button, "pointerdown");
    // A press that begins on the tile's own button keeps the controls up.
    expect(shown("Ben")).toBe(true);
    button.click();
    await settle();
    expect(callState.pinnedId()).toBe("peer-Ben");
    expect(stageName()).toBe("Ben");
    // Pinned: the controls a tap brought up are put away; the pinned tile's
    // button stays by the stylesheet (it is pressed).
    expect(all(".lk-call-tile.controls-shown")).toHaveLength(0);
    expect(pinButtonOf("Ben").getAttribute("aria-pressed")).toBe("true");
  });

  it("works the same on a strip tile", async () => {
    mount();
    await settle();
    one<HTMLElement>(".lk-call-layout").click();
    await settle();
    const dan = tileOf("Dan");
    pointer(dan, "pointerdown");
    dan.click();
    await settle();
    expect(shown("Dan")).toBe(true);
    expect(callState.pinnedId()).toBeNull();
    expect(stageName()).toBe("Anna");
    pinButtonOf("Dan").click();
    await settle();
    expect(stageName()).toBe("Dan");
  });

  it("hides the controls on a tap elsewhere and shows one tile's at a time", async () => {
    mount();
    await settle();
    tileOf("Ben").click();
    expect(shown("Ben")).toBe(true);
    // Another tile.
    pointer(tileOf("Cleo"), "pointerdown");
    expect(shown("Ben")).toBe(false);
    tileOf("Cleo").click();
    expect(shown("Cleo")).toBe(true);
    expect(all(".lk-call-tile.controls-shown")).toHaveLength(1);
    // Somewhere that is no tile at all.
    pointer(one(".lk-call-layout"), "pointerdown");
    expect(all(".lk-call-tile.controls-shown")).toHaveLength(0);
    expect(callState.pinnedId()).toBeNull();
  });

  it("hides the controls when the time runs out", async () => {
    mount();
    await settle();
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      tileOf("Ben").click();
      expect(shown("Ben")).toBe(true);
      vi.advanceTimersByTime(TILE_CONTROLS_REVEAL_MS - 1);
      expect(shown("Ben")).toBe(true);
      vi.advanceTimersByTime(1);
      expect(shown("Ben")).toBe(false);
      expect(callState.pinnedId()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("the pin button in the stylesheet", () => {
  it("is out of sight at rest and takes no tap on touch until the tile's controls are up", async () => {
    mount();
    await settle();
    installStylesheet();
    const top = [...document.styleSheets].flatMap((sheet) => [...sheet.cssRules]);
    const styleRules = (rules: readonly CSSRule[]): CSSStyleRule[] =>
      rules.filter((r): r is CSSStyleRule => typeof (r as CSSStyleRule).selectorText === "string");
    const media = (text: string): CSSStyleRule[] =>
      top
        .filter((r): r is CSSMediaRule => (r as CSSMediaRule).media?.mediaText === text)
        .flatMap((r) => styleRules([...r.cssRules]));
    const parts = (r: CSSStyleRule): string[] => r.selectorText.split(",").map((p) => p.trim());
    const find = (rules: readonly CSSStyleRule[], selector: string): CSSStyleRule | undefined =>
      rules.find((r) => parts(r).includes(selector));

    const base = find(styleRules(top), ".lk-call-tile-pin-btn");
    expect(base?.style.opacity).toBe("0");
    expect(base?.style.width).toBe("32px");
    expect(base?.style.height).toBe("32px");
    // A strip tile's button: smaller with a pointer, not under 24 px.
    const compact = styleRules(top).find((r) => r.selectorText === ".lk-call-tile-pin-btn.compact");
    expect(compact?.style.width).toBe("24px");
    expect(compact?.style.height).toBe("24px");

    // Shown: controls brought up by a tap, keyboard focus, the pinned tile.
    const shownRule = styleRules(top).find(
      (r) => parts(r).includes(".lk-call-tile.controls-shown .lk-call-tile-pin-btn") && r.style.opacity === "1",
    );
    expect(shownRule).toBeDefined();
    expect(parts(shownRule as CSSStyleRule)).toEqual(
      expect.arrayContaining([
        ".lk-call-tile:focus-within .lk-call-tile-pin-btn",
        '.lk-call-tile-pin-btn[aria-pressed="true"]',
      ]),
    );
    // Hover shows it only where there is a hover.
    expect(styleRules(top).some((r) => parts(r).includes(".lk-call-tile:hover .lk-call-tile-pin-btn"))).toBe(false);
    expect(find(media("(hover: hover)"), ".lk-call-tile:hover .lk-call-tile-pin-btn")?.style.opacity).toBe("1");

    // Touch: 44 px on every tile, and not drawn means not tappable.
    const touch = media("(hover: none)");
    const touchBase = find(touch, ".lk-call-tile-pin-btn");
    expect(parts(touchBase as CSSStyleRule)).toContain(".lk-call-tile-pin-btn.compact");
    expect(touchBase?.style.width).toBe("44px");
    expect(touchBase?.style.height).toBe("44px");
    expect(touchBase?.style.pointerEvents).toBe("none");
    expect(touchBase?.style.opacity).toBe("");
    const touchShown = find(touch, ".lk-call-tile.controls-shown .lk-call-tile-pin-btn");
    expect(touchShown?.style.pointerEvents).toBe("auto");
    expect(parts(touchShown as CSSStyleRule)).toContain('.lk-call-tile-pin-btn[aria-pressed="true"]');
  });
});
