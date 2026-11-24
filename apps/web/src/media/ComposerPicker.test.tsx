// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The picker the composer's one emoji button opens (ADR-0047 UX sections 4
 * and 5).
 *
 * ## Why jsdom and not Playwright
 *
 * Every question here is about the tree and the wiring: are both ways in
 * reachable from the same control, which DOM event a press is on, what the
 * component hands its collaborators, and whether the picker stays open or
 * closes afterwards. None of it needs layout or a media engine. The one thing
 * about this surface that WOULD need a real browser — that the popover is
 * anchored over the composer and does not run off the edge of the pane — is
 * named at the bottom as uncovered rather than approximated here.
 *
 * ## What this covers that nothing else does
 *
 * `emoji-index.test.ts` covers the search and the tone preferences;
 * `gif-client.test.ts` covers the proxy's wire and its five outcomes. Between
 * them they leave the whole of the component: that the GIF tab EXISTS beside
 * Emoji, that picking one reaches `onGif` and closes, that picking an emoji
 * reaches `onEmoji` and does NOT close, and — the one a rule test cannot see at
 * all — that an emoji press `preventDefault`s its `mousedown`, which is the
 * only thing keeping the caret in the message field on web. Swap that for a
 * plain click handler and every rule test stays green while every inserted
 * emoji lands at the start of the draft.
 *
 * ## What is real and what is stood in for
 *
 * The component, the bundled emoji dataset, the production GIF client and its
 * parser are all real. Two things are stood in for, both of them the
 * environment rather than the product: `fetch`, because a trending page has to
 * come from somewhere and this file has no server, and `localStorage`, because
 * in this environment there is not one (see `installLocalStorage`).
 *
 * Deliberately NOT covered: the composer's own toggle button, and the trailing
 * microphone-or-send switch beside it. Both are inline in the 7,700-line
 * `ChatView` of `routes/Chat.tsx` behind six providers — see the note at the
 * end of this file.
 */

// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { EmojiPicker } from "./EmojiPicker";
import { setGifProviderMissing } from "./media-prefs";
import { GIF_API_BASE, type GifItem } from "./gif-client";
import { all, installLocalStorage, one, present, renderUi } from "../test/ui";

/** One page of trending, in the proxy's own wire shape. */
const TRENDING = {
  items: [
    {
      id: "aaa111",
      preview_url: "/gifs/media/aaa111/preview",
      full_url: "/gifs/media/aaa111/full",
      width: 200,
      height: 150,
    },
    {
      id: "bbb222",
      preview_url: "/gifs/media/bbb222/preview",
      full_url: "/gifs/media/bbb222/full",
      width: 220,
      height: 160,
    },
  ],
  next_offset: 24,
};

let requested: string[] = [];

beforeEach(() => {
  installLocalStorage();
  // jsdom implements no scrolling API on an element, and a fresh GIF page
  // scrolls its body back to the top. Unstubbed, the load throws before any
  // tile is set and the tab renders as a permanent skeleton — a harness gap
  // that reads exactly like a product one.
  Element.prototype.scrollTo = function scrollTo(): void {};
  // Module state, remembered for the page session: a run that probed an
  // unconfigured proxy would otherwise hide the GIF tab in the next test.
  setGifProviderMissing(false);
  requested = [];
  vi.stubGlobal("fetch", (input: RequestInfo | URL) => {
    requested.push(String(input));
    return Promise.resolve(
      new Response(JSON.stringify(TRENDING), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

interface Picked {
  emoji: string[];
  gifs: GifItem[];
  closes: number;
}

function mountPicker(
  mode: "compose" | "reaction" = "compose",
  Picker: typeof EmojiPicker = EmojiPicker,
): Picked {
  const picked: Picked = { emoji: [], gifs: [], closes: 0 };
  renderUi(() => (
    <Picker
      mode={mode}
      onEmoji={(e) => picked.emoji.push(e)}
      onGif={(item) => picked.gifs.push(item)}
      onClose={() => (picked.closes += 1)}
      sessionToken={() => "session-token"}
    />
  ));
  return picked;
}

/** A tab by its visible name. */
function tab(name: "Emoji" | "GIF"): HTMLButtonElement {
  const found = all<HTMLButtonElement>('[role="tab"]').find((b) => b.textContent?.trim() === name);
  expect(found, `no ${name} tab`).not.toBeUndefined();
  return found as HTMLButtonElement;
}

/** The first emoji cell in the grid. */
function firstEmoji(): HTMLButtonElement {
  return one<HTMLButtonElement>(".lakko-emoji-grid button");
}

describe("the two ways in", () => {
  it("offers Emoji and GIF from the one control", () => {
    mountPicker();

    expect(all('[role="tab"]').map((t) => t.textContent?.trim())).toEqual(["Emoji", "GIF"]);
    // Emoji is what a person lands on: it needs no network and is what the
    // button they pressed is called.
    expect(tab("Emoji").getAttribute("aria-selected")).toBe("true");
    expect(present(".lakko-emoji-grid")).toBe(true);
  });

  it("has no GIF tab in reaction mode", () => {
    // Section 5: a reaction is an emoji. The tab strip is not merely inert —
    // there is nothing to switch, so there is no strip.
    mountPicker("reaction");

    expect(all('[role="tab"]')).toHaveLength(0);
    expect(present(".lakko-emoji-grid")).toBe(true);
  });

  it("hides the GIF tab when the server has no provider configured", async () => {
    // The probe runs on the first compose open and is remembered in a
    // module-level flag for the rest of the page session, so the tab is gone
    // before anyone reaches for it rather than disappearing under their finger.
    // That flag is why this test loads its own copy of the module: another test
    // in this file has already probed successfully, and "once per session" is
    // the behaviour, not an obstacle to route around.
    vi.stubGlobal("fetch", () =>
      Promise.resolve(
        new Response(JSON.stringify({ error: "gif-provider-not-configured" }), {
          status: 503,
          headers: { "content-type": "application/json" },
        }),
      ),
    );
    vi.resetModules();
    const fresh = await import("./EmojiPicker");

    mountPicker("compose", fresh.EmojiPicker);
    // Both tabs are there for the instant before the probe answers; what must
    // not happen is a person meeting a GIF tab that cannot work.
    await vi.waitFor(() => expect(all('[role="tab"]')).toHaveLength(0));

    expect(present(".lakko-emoji-grid")).toBe(true);
  });
});

describe("the tabs to a screen reader", () => {
  // Seen on iOS: both tabs were read with the picker's own name ("Emoji and
  // GIFs"). Here each tab is named by its own text, says whether it is
  // selected, and points at the one panel, which is named by the selected tab.
  /** The name a screen reader reads for a tab: an aria-label wins, else the text. */
  const nameOf = (el: Element): string | undefined =>
    el.getAttribute("aria-label") ?? el.textContent?.trim();

  it("names each tab by what it shows, never by the picker's name", () => {
    mountPicker();

    const tabs = all('[role="tab"]');
    expect(tabs.map(nameOf)).toEqual(["Emoji", "GIF"]);
    expect(tabs.some((t) => nameOf(t) === "Emoji and GIFs")).toBe(false);
    expect(one('[role="dialog"]').getAttribute("aria-label")).toBe("Emoji and GIFs");
    expect(one('[role="tablist"]').contains(tab("Emoji"))).toBe(true);
  });

  it("marks exactly the selected tab and keeps only it in the Tab order", () => {
    mountPicker();

    expect(tab("Emoji").getAttribute("aria-selected")).toBe("true");
    expect(tab("GIF").getAttribute("aria-selected")).toBe("false");
    expect(tab("Emoji").tabIndex).toBe(0);
    expect(tab("GIF").tabIndex).toBe(-1);

    tab("GIF").click();

    expect(tab("GIF").getAttribute("aria-selected")).toBe("true");
    expect(tab("Emoji").getAttribute("aria-selected")).toBe("false");
    expect(tab("GIF").tabIndex).toBe(0);
    expect(tab("Emoji").tabIndex).toBe(-1);
  });

  it("links both tabs to the one panel, named by the selected tab", () => {
    mountPicker();

    const panel = one('[role="tabpanel"]');
    expect(tab("Emoji").getAttribute("aria-controls")).toBe(panel.id);
    expect(tab("GIF").getAttribute("aria-controls")).toBe(panel.id);
    expect(panel.getAttribute("aria-labelledby")).toBe(tab("Emoji").id);

    tab("GIF").click();

    expect(one('[role="tabpanel"]').getAttribute("aria-labelledby")).toBe(tab("GIF").id);
  });

  it("moves between the tabs with the arrow keys", () => {
    mountPicker();

    tab("Emoji").focus();
    tab("Emoji").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(tab("GIF").getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(tab("GIF"));

    tab("GIF").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(tab("Emoji").getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(tab("Emoji"));

    tab("Emoji").dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true }));
    expect(tab("GIF").getAttribute("aria-selected")).toBe("true");
  });

  it("has no tab panel role when there are no tabs", () => {
    mountPicker("reaction");

    expect(all('[role="tabpanel"]')).toHaveLength(0);
  });
});

describe("picking an emoji", () => {
  it("hands the glyph over and stays open", () => {
    const picked = mountPicker();

    firstEmoji().click();

    expect(picked.emoji).toHaveLength(1);
    expect(picked.emoji[0]?.length).toBeGreaterThan(0);
    // Section 4: several emoji in a row without reopening the picker each
    // time. A picker that closed would make the second one two gestures.
    expect(picked.closes).toBe(0);
    expect(present(".lakko-picker")).toBe(true);
  });

  it("does not take the caret out of the message field", () => {
    // The whole of "an emoji is inserted AT THE CARET". The press must cancel
    // its own `mousedown`, because that is the event that moves focus, and the
    // route reads `selectionStart` off the field to decide where to insert.
    // A `click`-only handler leaves every insertion at position 0.
    mountPicker();

    const cell = firstEmoji();
    const down = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    cell.dispatchEvent(down);

    expect(down.defaultPrevented).toBe(true);
  });

  it("closes after a reaction, because a reaction is one choice", () => {
    const picked = mountPicker("reaction");

    firstEmoji().click();

    expect(picked.emoji).toHaveLength(1);
    expect(picked.closes).toBe(1);
  });
});

describe("picking a GIF", () => {
  it("sends it and closes, and never hands out an upstream URL", async () => {
    const picked = mountPicker();
    tab("GIF").click();

    const tiles = await vi.waitFor(() => {
      const found = all<HTMLButtonElement>(".lakko-gif-grid button");
      expect(found.length).toBeGreaterThan(0);
      return found;
    });
    expect(tiles[0]?.getAttribute("aria-label")).toBe("Send GIF");

    tiles[0]?.click();

    expect(picked.gifs).toHaveLength(1);
    expect(picked.gifs[0]?.id).toBe("aaa111");
    // Section 5 — the picked GIF is re-uploaded as an encrypted attachment, so
    // what the composer is handed must be a path on Lakko's own proxy. An
    // upstream URL reaching a recipient is the privacy cost this design exists
    // to avoid, and the client resolves the wire's relative media path against
    // the proxy base rather than trusting whatever the body carried.
    expect(picked.gifs[0]?.fullUrl).toBe(`${GIF_API_BASE}/gifs/media/aaa111/full`);
    // One choice sends a message, so unlike an emoji this one closes.
    expect(picked.closes).toBe(1);
    // And everything this component asked for went to Lakko.
    expect(requested.every((u) => u.includes("/gifs/"))).toBe(true);
    expect(requested.some((u) => u.includes("giphy"))).toBe(false);
  });
});

describe("paging GIF results across a new search", () => {
  it("still loads a second page of the new results after an abandoned load-more", async () => {
    // The sentinel at the foot of the grid is watched by an
    // IntersectionObserver, which jsdom does not have: stand one in and fire
    // it by hand.
    const observers: Array<{ fire: () => void; targets: Element[] }> = [];
    vi.stubGlobal(
      "IntersectionObserver",
      class {
        private readonly targets: Element[] = [];
        constructor(private readonly cb: (entries: unknown[]) => void) {
          observers.push({
            targets: this.targets,
            fire: () => this.cb(this.targets.map((target) => ({ isIntersecting: true, target }))),
          });
        }
        observe(el: Element): void {
          this.targets.push(el);
        }
        unobserve(): void {}
        disconnect(): void {}
      },
    );
    // List requests answer when the test says so; previews fail fast.
    const lists: Array<{ url: string; answer: () => void }> = [];
    vi.stubGlobal("fetch", (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/gifs/media/")) return Promise.resolve(new Response("", { status: 404 }));
      return new Promise<Response>((resolve) => {
        lists.push({
          url,
          answer: () =>
            resolve(
              new Response(JSON.stringify(TRENDING), {
                status: 200,
                headers: { "content-type": "application/json" },
              }),
            ),
        });
      });
    });
    const scrollToFoot = (): void => {
      for (const o of observers) {
        if (o.targets.some((el) => (el as HTMLElement).dataset.gifId === undefined)) o.fire();
      }
    };

    mountPicker();
    tab("GIF").click();
    await vi.waitFor(() => expect(lists).toHaveLength(1));
    lists[0]?.answer();
    await vi.waitFor(() => expect(all(".lakko-gif-grid button").length).toBeGreaterThan(0));

    // Scroll to the foot: the second page of Trending is asked for and hangs.
    scrollToFoot();
    await vi.waitFor(() => expect(lists).toHaveLength(2));
    expect(lists[1]?.url).toContain("trending?offset=24");

    // Meanwhile the person searches.
    const input = one<HTMLInputElement>(".lakko-picker-search input");
    input.value = "cat";
    input.dispatchEvent(new InputEvent("input", { bubbles: true }));
    await vi.waitFor(() => expect(lists).toHaveLength(3), { timeout: 2000 });
    expect(lists[2]?.url).toContain("search?q=cat&offset=0");
    lists[2]?.answer();
    // The abandoned Trending page answers late, and is dropped.
    lists[1]?.answer();
    await vi.waitFor(() => expect(all(".lakko-gif-grid button").length).toBeGreaterThan(0));

    // The new results must still be able to load their own second page.
    scrollToFoot();
    await vi.waitFor(() => expect(lists).toHaveLength(4));
    expect(lists[3]?.url).toContain("search?q=cat&offset=24");
  });
});

/**
 * ## Not covered here
 *
 * **The composer's own trailing slot** — a microphone with an empty draft, a
 * send arrow otherwise — and the attach control and the button that opens this
 * picker. All four are inline JSX in `ChatView` (`routes/Chat.tsx`, ~7,700
 * lines), which reads six contexts whose `Context` objects are not exported, so
 * reaching them means either a route mounted over an MLS identity, an OPAQUE
 * session and IndexedDB, or a fixture that reproduces the row's markup — and a
 * fixture that reproduces the thing under test measures itself. Covering them
 * honestly needs the row extracted into a component of its own, which is a
 * production change and not a test.
 *
 * **Where the popover sits.** Anchoring over the composer, and not running off
 * the pane's edge, is a geometry: `playwright/specs/`, not here.
 *
 * **The GIF previews.** The tiles render, and their images do not: the preview
 * fetch is driven by an `IntersectionObserver`, which jsdom does not implement,
 * so the queue is never pumped. What a tile LOOKS like once its bytes arrive
 * needs a real browser.
 */
