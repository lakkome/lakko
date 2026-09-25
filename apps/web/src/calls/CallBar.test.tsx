// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The collapsed call bar, rendered (`docs/ux/calls.md` section 4.5).
 *
 * ## Why jsdom and not Playwright
 *
 * Every question section 4.5 asks about this strip is a question about the
 * tree and the wiring: is the bar there at all, does its one line carry one
 * thing, which gesture reaches which handler, and is the End control absent.
 * None of them needs layout, a media engine or a browser policy, so the fast
 * harness is the right one. The one thing about the bar that DOES need a real
 * browser — that it makes room rather than covering the rail, via the
 * `data-call-bar` mark and `box-sizing: border-box` padding on `body` — is
 * named at the bottom of this file as uncovered rather than asserted through a
 * proxy here, because jsdom computes no layout and a padding that resolves is
 * not a rail that is still reachable.
 *
 * ## What is real and what is stood in for
 *
 * The component, the store, the stylesheet's class names, and the shared Rust
 * rules are all the real ones: `initWasm` loads the built core, so
 * `callBarVisible` / `callBarStatusKind` / `callBarElapsedMs` are the same
 * functions iOS and Android reach over UniFFI. Nothing is mocked. That is the
 * point of covering the bar here rather than covering `call-bar-model.ts`
 * again with a spy: a spy can show what the rule was ASKED, never that the
 * component asked it about the right thing, and the two defects this file is
 * written against are both of that shape.
 *
 * `CallBar` takes its four inputs as plain props and reads the rest off the
 * module-level call store, so there is no context to stand up.
 *
 * ## The two properties worth the most
 *
 *   * **One line, one thing.** Section 4 says the status line carries the
 *     state or the running time and never two at once. The rule is a sum type
 *     precisely so a client cannot hold two independent reads; whether THIS
 *     client then prints one of them is a DOM fact and is asserted as one —
 *     the bar's whole text, not just the slot's.
 *   * **No End.** Section 4.5 keeps the one irreversible control off a strip a
 *     thumb crosses on the way to a chat. An absence is the easiest thing to
 *     regress and the hardest to notice, so it is pinned by enumerating every
 *     button the bar renders rather than by looking for a label.
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { callActions, callState, type Participant } from "../state/call-state";
import { all, initWasm, one, present, renderUi, settle } from "../test/ui";

type CallBarComponent = typeof import("./CallBar")["CallBar"];
let CallBar: CallBarComponent;

beforeAll(async () => {
  // Before `./CallBar` is imported, and that order is load-bearing. Its
  // `chat/ui-rules` primes the shared rules at import with `loadWasm()`, whose
  // `init()` fetches the `.wasm` URL — which jsdom cannot do. Initialising the
  // same module from bytes first makes that `init()` a resolved no-op, so the
  // rules signal fills and the bar renders against the real core instead of
  // the `null` posture it (correctly) falls back to.
  await initWasm();
  ({ CallBar } = await import("./CallBar"));
  // `loadWasm().then(setRules)` lands on a microtask.
  await settle();
});

beforeEach(() => {
  callActions.reset();
});

afterEach(() => {
  // `connectedFor` moves the clock; put it back so a duration wound forward in
  // one test cannot be read by the next one's ticker.
  vi.useRealTimers();
});

function participant(over: Partial<Participant> = {}): Participant {
  return {
    id: "local",
    displayName: "",
    isLocal: true,
    micOn: true,
    camOn: false,
    quality: "good",
    speaking: false,
    ...over,
  };
}

/** The bar as the app-level host mounts it: a 1x1 call with a resolved peer. */
function mountBar(over: Partial<Parameters<CallBarComponent>[0]> = {}): {
  expanded: () => number;
} {
  const onExpand = vi.fn();
  renderUi(() => (
    <CallBar
      peerName="Dana"
      groupCall={false}
      ringing={false}
      callScreenUp={false}
      onExpand={onExpand}
      {...over}
    />
  ));
  return { expanded: () => onExpand.mock.calls.length };
}

/** A connected 1x1 call, `ms` into it. */
function connectedFor(ms: number): void {
  callActions.setParticipants([participant(), participant({ id: "leaf-1", displayName: "Dana", isLocal: false })]);
  callActions.setPhase({ kind: "connected" });
  // `setPhase` starts the clock at `Date.now()`; wind the store's own reading
  // of "when media started" back instead of faking time, so the component's
  // one-second ticker is left alone.
  const started = callState.connectedAt();
  expect(started).not.toBeNull();
  vi.setSystemTime(new Date((started ?? 0) + ms));
}

const BAR = ".lk-callbar";

describe("whether the bar is there at all", () => {
  it("shows nothing when there is no call", () => {
    mountBar();
    expect(present(BAR)).toBe(false);
    // And the room it would have asked for was not asked for either — the
    // stylesheet pads `body` off this mark, so a bar that is not shown while
    // the mark is set is a strip of empty space at the top of the app.
    expect(document.documentElement.hasAttribute("data-call-bar")).toBe(false);
  });

  it("puts a connected call away into the bar", () => {
    connectedFor(0);
    mountBar();
    expect(present(BAR)).toBe(true);
    expect(one(".lk-callbar-who").textContent).toBe("Dana");
    expect(document.documentElement.hasAttribute("data-call-bar")).toBe(true);
  });

  it("is never up at the same time as the call screen", () => {
    connectedFor(0);
    mountBar({ callScreenUp: true });
    // Section 4.5: one state, one place. The same live call, the only
    // difference being that its own screen is showing.
    expect(present(BAR)).toBe(false);
    expect(document.documentElement.hasAttribute("data-call-bar")).toBe(false);
  });

  it("does not collapse a call that is only ringing in", () => {
    // An incoming call is answered or declined and takes the whole screen
    // (section 2.2), so there is nothing to put away. The store has no
    // `ringing` phase for exactly that reason, which is why the host passes it
    // separately — and passing it must still reach the rule.
    mountBar({ ringing: true });
    expect(present(BAR)).toBe(false);
  });

  it("gives the room back when a collapsed call ends under it", () => {
    connectedFor(0);
    mountBar();
    expect(document.documentElement.hasAttribute("data-call-bar")).toBe(true);

    callActions.endCall();

    expect(present(BAR)).toBe(false);
    expect(document.documentElement.hasAttribute("data-call-bar")).toBe(false);
  });
});

describe("the one status line", () => {
  it("says the state while the call is coming up, and no time", () => {
    callActions.setParticipants([participant()]);
    callActions.setPhase({ kind: "connecting" });
    mountBar();

    expect(one(".lk-callbar-state").textContent).toBe("Connecting...");
    // The whole bar, not just the slot: "never two at once" is broken by a
    // second element as easily as by a concatenation.
    expect(one(BAR).textContent).not.toMatch(/\d+:\d\d/);
    expect(all(".lk-callbar-state")).toHaveLength(1);
  });

  it("says the running time once connected, and no state word", () => {
    connectedFor(62_000);
    mountBar();

    expect(one(".lk-callbar-state").textContent).toBe("1:02");
    const text = one(BAR).textContent ?? "";
    expect(text).not.toContain("Connecting");
    expect(text).not.toContain("Calling");
    expect(text).not.toContain("Reconnecting");
    expect(all(".lk-callbar-state")).toHaveLength(1);
  });

  it("reads the time back to a screen reader as a duration, not a clock time", () => {
    // Section 8: `1:02` read out is a time of day. The bar has one accessible
    // name for the way back in, so this is read off that rather than off a
    // label on the slot — an `aria-label` on a button replaces everything
    // inside it, which is why the glyphs are folded in there too.
    connectedFor(62_000);
    mountBar();

    const spoken = one(".lk-callbar-open").getAttribute("aria-label") ?? "";
    expect(spoken).toContain("1 minute 2 seconds");
    expect(spoken).not.toContain("1:02");
  });
});

describe("what the bar carries", () => {
  it("shows the lock only when there is a safety number to open", () => {
    connectedFor(0);
    mountBar();
    // The 1x1 path publishes no fingerprint, so there is nothing to claim.
    expect(all(".lk-callbar-mark")).toHaveLength(0);
    expect(one(".lk-callbar-open").getAttribute("aria-label")).not.toContain("encrypted");

    callActions.setE2ee({ active: true, shortFingerprint: "a1b2c3d4e5f6" });

    expect(all(".lk-callbar-mark")).toHaveLength(1);
    expect(one(".lk-callbar-open").getAttribute("aria-label")).toContain("encrypted");
    // A mark, not a word: section 4.5 puts the claim where the number can be
    // read, one tap away.
    expect(one(".lk-callbar-open").textContent).not.toContain("ncrypted");
  });

  it("says so when the camera this device put away is still sending", () => {
    callActions.setParticipants([
      participant({ camOn: true }),
      participant({ id: "leaf-1", displayName: "Dana", isLocal: false }),
    ]);
    callActions.setPhase({ kind: "connected" });
    mountBar();

    // The glyph is the point of the row, and the sentence is the only way a
    // screen reader user is told at all.
    expect(all(".lk-callbar-mark")).toHaveLength(1);
    expect(one(".lk-callbar-open").getAttribute("aria-label")).toContain("your camera is on");
  });

  it("carries no End control", () => {
    connectedFor(0);
    mountBar();

    // Enumerated rather than searched for: an absence regresses by a button
    // being ADDED, and "no button is labelled End" would pass for a red circle
    // labelled "Leave".
    const labels = all<HTMLButtonElement>(`${BAR} button`).map((b) => b.getAttribute("aria-label"));
    expect(labels).toEqual([
      expect.stringContaining("Return to the call"),
      "Mute microphone",
    ]);
  });
});

describe("the two gestures", () => {
  it("brings the call back when the strip is pressed", () => {
    connectedFor(0);
    const { expanded } = mountBar();

    one<HTMLButtonElement>(".lk-callbar-open").click();

    expect(expanded()).toBe(1);
  });

  it("mutes without bringing the call back", () => {
    connectedFor(0);
    const { expanded } = mountBar();
    expect(callState.participants().find((p) => p.isLocal)?.micOn).toBe(true);

    one<HTMLButtonElement>(".lk-callbar-mute").click();

    expect(callState.participants().find((p) => p.isLocal)?.micOn).toBe(false);
    // The one control on the strip that is not the way back in. Without the
    // `stopPropagation` the mute press would also expand the call, which is
    // the opposite of what a person reaching for it wants.
    expect(expanded()).toBe(0);
  });

  it("shows mute inert rather than absent before there is anything to mute", () => {
    callActions.setParticipants([participant()]);
    callActions.setPhase({ kind: "calling" });
    mountBar();

    // Section 4.1.1 — never a hidden button. The same definition the call
    // screen's own row is built from answers this, so the two cannot drift.
    expect(one<HTMLButtonElement>(".lk-callbar-mute").disabled).toBe(true);
  });
});

/**
 * ## Not covered here
 *
 * **That the bar makes room instead of covering the app.** The component marks
 * `documentElement` and the stylesheet pads `body`; the mark is asserted above
 * because it is a DOM fact, but whether the navigation rail, the composer and
 * the mobile tab bar are still fully reachable under a 56px strip is a
 * geometry, and jsdom computes none. It belongs in `playwright/specs/`, and
 * needs a fixture that mounts the app shell rather than one component.
 *
 * **The one-second tick.** `tickingClock` is started and stopped with the bar's
 * own visibility, and the store's `connectedAt` is wound back here rather than
 * time being faked, so what is asserted is the formatting of a duration and
 * not that the line advances on its own.
 */
