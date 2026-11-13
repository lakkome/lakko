// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The collapsed call window, rendered (`docs/ux/calls.md` section 4.5).
 *
 * ## Why jsdom and not Playwright
 *
 * What this file asks are questions about the tree and the wiring: is the
 * window there at all, does its status line carry one thing, who is in the
 * picture and under what name, which gesture reaches which handler, is the
 * End control absent, and does a drag land it in a corner instead of
 * expanding the call. None of them needs real layout or a media engine.
 * What does (the window's look over a real picture, the snap animation) is
 * named at the bottom as uncovered.
 *
 * ## What is real and what is stood in for
 *
 * The component, the store and the shared Rust rules are the real ones:
 * `initWasm` loads the built core, so `callBarVisible`, `callBarStatusKind`,
 * `callStage` and `tileVideoQuality` are the functions iOS and Android reach
 * over UniFFI. The call's pictures are handed in as plain functions; a
 * stream is a stand-in object with no tracks, which is all the window's own
 * logic reads of it.
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { callActions, callState, type Participant } from "../state/call-state";
import { all, initWasm, one, pointer, present, renderUi, settle } from "../test/ui";
import type { CallWindowSources } from "./CallWindow";
import { resetRememberedCorner } from "./call-window-model";
import { reserveBannerStrip } from "../sw/SwUpdateBanner";

type CallWindowComponent = typeof import("./CallWindow")["CallWindow"];
let CallWindow: CallWindowComponent;

beforeAll(async () => {
  // Before `./CallWindow` is imported, and that order is load-bearing: its
  // `chat/ui-rules` primes the shared rules at import with `loadWasm()`, whose
  // `init()` fetches the `.wasm` URL, which jsdom cannot do. Initialising the
  // same module from bytes first makes that `init()` a resolved no-op.
  await initWasm();
  ({ CallWindow } = await import("./CallWindow"));
  await settle();
});

beforeEach(() => {
  callActions.reset();
  resetRememberedCorner();
});

afterEach(() => {
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

/** A stand-in stream: the window reads nothing of it but its presence, and
 *  the pictures read its video tracks. */
const fakeStream = (): MediaStream => ({ getVideoTracks: () => [], getTracks: () => [] }) as unknown as MediaStream;

interface Mounted {
  expanded: () => number;
  reports: { participantId: string; quality: string | null }[];
}

/** The window as the app-level host mounts it: a 1x1 call with a resolved
 *  peer, unless overridden. */
function mountWindow(
  over: Partial<Parameters<CallWindowComponent>[0]> = {},
  sources: Partial<CallWindowSources> = {},
): Mounted {
  const onExpand = vi.fn();
  const reports: Mounted["reports"] = [];
  const full: CallWindowSources = {
    remoteStreamFor: () => undefined,
    localStream: () => undefined,
    ...sources,
  };
  if (sources.reportTileVideoQuality === undefined && over.groupCall === true) {
    (full as { reportTileVideoQuality?: CallWindowSources["reportTileVideoQuality"] }).reportTileVideoQuality = (
      _tile,
      participantId,
      quality,
    ) => {
      reports.push({ participantId, quality });
    };
  }
  renderUi(() => (
    <CallWindow
      peerName="Dana"
      peerId="group-dana"
      groupCall={false}
      ringing={false}
      callScreenUp={false}
      onExpand={onExpand}
      sources={full}
      {...over}
    />
  ));
  return { expanded: () => onExpand.mock.calls.length, reports };
}

/** A connected 1x1 call, `ms` into it. */
function connectedFor(ms: number, local: Partial<Participant> = {}): void {
  callActions.setParticipants([
    participant(local),
    participant({ id: "peer", displayName: "Dana", isLocal: false }),
  ]);
  callActions.setPhase({ kind: "connected" });
  const started = callState.connectedAt();
  expect(started).not.toBeNull();
  vi.setSystemTime(new Date((started ?? 0) + ms));
}

const WINDOW = ".lk-call-window";

describe("whether the window is there at all", () => {
  it("shows nothing when there is no call", () => {
    mountWindow();
    expect(present(WINDOW)).toBe(false);
  });

  it("puts a connected call away into the window, with the other person in it", () => {
    connectedFor(0);
    mountWindow();
    expect(present(WINDOW)).toBe(true);
    expect(one(".lk-call-window-name").textContent).toBe("Dana");
  });

  it("no longer makes room at the top of the app: the bar is gone", () => {
    connectedFor(0);
    mountWindow();
    expect(document.documentElement.hasAttribute("data-call-bar")).toBe(false);
    expect(present(".lk-callbar")).toBe(false);
  });

  it("is never up at the same time as the call screen", () => {
    connectedFor(0);
    mountWindow({ callScreenUp: true });
    expect(present(WINDOW)).toBe(false);
  });

  it("does not collapse a call that is only ringing in", () => {
    mountWindow({ ringing: true });
    expect(present(WINDOW)).toBe(false);
  });

  it("goes away when a collapsed call ends under it", () => {
    connectedFor(0);
    mountWindow();
    expect(present(WINDOW)).toBe(true);
    callActions.endCall();
    expect(present(WINDOW)).toBe(false);
  });

  it("shows the person called, and Calling..., for an outgoing call", () => {
    callActions.setParticipants([participant()]);
    callActions.setPhase({ kind: "calling" });
    mountWindow();
    expect(one(".lk-call-window-name").textContent).toBe("Dana");
    expect(one(".lk-call-window-state").textContent).toBe("Calling...");
    // Their avatar, not this device's own picture.
    expect(present(".lk-call-window-picture .lk-call-tile-avatar")).toBe(true);
  });
});

describe("the one status line", () => {
  it("says the state while the call is coming up, and no time", () => {
    callActions.setParticipants([participant()]);
    callActions.setPhase({ kind: "connecting" });
    mountWindow();

    expect(one(".lk-call-window-state").textContent).toBe("Connecting...");
    expect(one(".lk-call-window-top").textContent).not.toMatch(/\d+:\d\d/);
    expect(all(".lk-call-window-state")).toHaveLength(1);
  });

  it("says the running time once connected, and no state word", () => {
    connectedFor(62_000);
    mountWindow();

    expect(one(".lk-call-window-state").textContent).toBe("1:02");
    const text = one(".lk-call-window-top").textContent ?? "";
    expect(text).not.toContain("Connecting");
    expect(text).not.toContain("Calling");
  });

  it("reads who and how long to a screen reader, as a duration", () => {
    connectedFor(62_000);
    mountWindow();

    const spoken = one(".lk-call-window-open").getAttribute("aria-label") ?? "";
    expect(spoken).toContain("Dana");
    expect(spoken).toContain("1 minute 2 seconds");
    expect(spoken).not.toContain("1:02");
  });

  it("announces a state change, never the ticking time", () => {
    callActions.setParticipants([participant()]);
    callActions.setPhase({ kind: "connecting" });
    mountWindow();
    const live = one('.lk-call-window [role="status"]');
    expect(live.getAttribute("aria-live")).toBe("polite");
    expect(live.textContent).toContain("onnecting");

    connectedFor(62_000);
    expect(live.textContent).toBe("Connected");
  });
});

describe("what the window carries", () => {
  it("shows the lock only when there is a safety number to open", () => {
    connectedFor(0);
    mountWindow();
    expect(all(".lk-call-window-mark")).toHaveLength(0);

    callActions.setE2ee({ active: true, shortFingerprint: "a1b2c3d4e5f6" });

    expect(all(".lk-call-window-mark")).toHaveLength(1);
    expect(one(".lk-call-window-open").getAttribute("aria-label")).toContain("encrypted");
    // A mark, never the word.
    expect(one(WINDOW).textContent).not.toContain("ncrypted");
  });

  it("insets your own camera while it sends, and says so to a screen reader", () => {
    connectedFor(0, { camOn: true });
    mountWindow({}, { localStream: fakeStream });

    expect(present(".lk-call-window-inset")).toBe(true);
    // Mirrored, as on the call screen.
    expect(one(".lk-call-window-inset video").classList.contains("mirrored")).toBe(true);
    // Away from the right edge the window starts against.
    expect(one(".lk-call-window-inset").getAttribute("data-corner")).toBe("bottom-left");
    expect(one(".lk-call-window-open").getAttribute("aria-label")).toContain("your camera is on");
  });

  it("draws no inset while your camera is off", () => {
    connectedFor(0);
    mountWindow({}, { localStream: fakeStream });
    expect(present(".lk-call-window-inset")).toBe(false);
  });

  it("carries no End control", () => {
    connectedFor(0);
    mountWindow();

    // Enumerated rather than searched for: an absence regresses by a button
    // being ADDED.
    const labels = all<HTMLButtonElement>(`${WINDOW} button`).map((b) => b.getAttribute("aria-label"));
    expect(labels).toEqual([expect.stringContaining("Return to the call"), "Mute microphone"]);
  });
});

describe("a group call", () => {
  function groupOf(n: number): void {
    const people = [participant()];
    for (let i = 1; i < n; i++) {
      people.push(participant({ id: `peer-${i}`, displayName: `Person ${i}`, isLocal: false }));
    }
    callActions.setParticipants(people);
    callActions.setPhase({ kind: "connected" });
  }

  it("names the person on the stage and +N for everyone else beyond them and you", () => {
    groupOf(18);
    callActions.pin("peer-5");
    mountWindow({ groupCall: true, peerName: null, peerId: null });

    expect(one(".lk-call-window-name").textContent).toBe("Person 5");
    expect(one(".lk-call-window-more").textContent).toBe("+16");
  });

  it("prints no +N when there is nobody beyond the person shown", () => {
    groupOf(2);
    mountWindow({ groupCall: true, peerName: null, peerId: null });
    expect(present(".lk-call-window-more")).toBe(false);
  });

  it("asks the SFU for the shown person's picture at the window's width", () => {
    const people = [participant(), participant({ id: "peer-a", displayName: "A", isLocal: false, camOn: true })];
    people.push(participant({ id: "peer-b", displayName: "B", isLocal: false }));
    callActions.setParticipants(people);
    callActions.setPhase({ kind: "connected" });
    callActions.pin("peer-a");
    const { reports } = mountWindow(
      { groupCall: true, peerName: null, peerId: null },
      {
        remoteStreamFor: (id) => (id === "peer-a" ? fakeStream() : undefined),
        remoteLightStreamFor: (id) => (id === "peer-a" ? fakeStream() : undefined),
      },
    );

    // Only the shown person, and a picture the shared rule picked for a
    // window, not the stage's full one.
    expect(reports.length).toBeGreaterThan(0);
    expect(new Set(reports.map((r) => r.participantId))).toEqual(new Set(["peer-a"]));
    const last = reports[reports.length - 1];
    expect(last?.quality === "light" || last?.quality === "middle").toBe(true);
  });
});

describe("the gestures", () => {
  it("brings the call back when the window is clicked", () => {
    connectedFor(0);
    const { expanded } = mountWindow();
    one<HTMLButtonElement>(".lk-call-window-open").click();
    expect(expanded()).toBe(1);
  });

  it("mutes without bringing the call back", () => {
    connectedFor(0);
    const { expanded } = mountWindow();
    one<HTMLButtonElement>(".lk-call-window-mute").click();
    expect(callState.participants().find((p) => p.isLocal)?.micOn).toBe(false);
    expect(expanded()).toBe(0);
  });

  it("shows mute inert rather than absent before there is anything to mute", () => {
    callActions.setParticipants([participant()]);
    callActions.setPhase({ kind: "calling" });
    mountWindow();
    expect(one<HTMLButtonElement>(".lk-call-window-mute").disabled).toBe(true);
  });

  it("starts in the top right corner", () => {
    connectedFor(0);
    mountWindow();
    expect(one(WINDOW).getAttribute("data-corner")).toBe("top-right");
  });

  it("lands a drag in the nearest corner without expanding the call", () => {
    connectedFor(0);
    const { expanded } = mountWindow();
    const open = one<HTMLButtonElement>(".lk-call-window-open");

    pointer(open, "pointerdown", { clientX: 900, clientY: 100 });
    pointer(open, "pointermove", { clientX: 600, clientY: 400 });
    pointer(open, "pointermove", { clientX: 100, clientY: 700 });
    expect(one(WINDOW).classList.contains("dragging")).toBe(true);
    pointer(open, "pointerup", { clientX: 100, clientY: 700 });
    // The click a real pointer sends after the release.
    open.click();

    expect(one(WINDOW).getAttribute("data-corner")).toBe("bottom-left");
    expect(one(WINDOW).classList.contains("dragging")).toBe(false);
    expect(expanded()).toBe(0);
  });

  it("stays above the update banner in a bottom corner, and moves up when it appears", () => {
    // Base walk 2026-11-13: the banner (z-index 1000) is drawn over anything
    // at the bottom of the viewport, and this window (z-index 900) places
    // itself from script, not from the page's layout.
    connectedFor(0);
    mountWindow();
    const open = one<HTMLButtonElement>(".lk-call-window-open");
    pointer(open, "pointerdown", { clientX: 900, clientY: 100 });
    pointer(open, "pointermove", { clientX: 100, clientY: 700 });
    pointer(open, "pointerup", { clientX: 100, clientY: 700 });
    expect(one(WINDOW).getAttribute("data-corner")).toBe("bottom-left");
    const yOf = (): number => {
      const m = /translate\((-?\d+)px, (-?\d+)px\)/.exec(one(WINDOW).style.transform);
      expect(m).not.toBeNull();
      return Number(m?.[2]);
    };
    const before = yOf();
    try {
      reserveBannerStrip(document.documentElement, 60);
      expect(yOf()).toBe(before - 60);
    } finally {
      reserveBannerStrip(document.documentElement, null);
    }
    expect(yOf()).toBe(before);
  });

  it("opens the next collapsed call in the corner the last one landed in", () => {
    connectedFor(0);
    mountWindow();
    const open = one<HTMLButtonElement>(".lk-call-window-open");
    pointer(open, "pointerdown", { clientX: 900, clientY: 100 });
    pointer(open, "pointermove", { clientX: 100, clientY: 700 });
    pointer(open, "pointerup", { clientX: 100, clientY: 700 });
    expect(one(WINDOW).getAttribute("data-corner")).toBe("bottom-left");

    callActions.endCall();
    expect(present(WINDOW)).toBe(false);
    connectedFor(0);
    expect(one(WINDOW).getAttribute("data-corner")).toBe("bottom-left");
  });
});

/**
 * ## Not covered here
 *
 * **The window over a real picture.** The person's video, the inset's mirror
 * and the snap animation need a browser with a media engine and layout.
 *
 * **The header clearance.** The top corners sit below the header under them,
 * measured from the page; jsdom lays nothing out, so every header measures 0.
 */
