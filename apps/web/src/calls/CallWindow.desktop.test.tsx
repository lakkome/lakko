// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The collapsed call in the desktop app: a window of the operating system
 * the page feeds (`docs/ux/calls.md` section 4.5.4a).
 *
 * The host is stood in for at the module boundary (`platform/host.ts`): what
 * the page posts is recorded, and a click on the OS window is delivered as the
 * host would deliver it. The component, the store and the shared Rust rules
 * are the real ones, as in `CallWindow.test.tsx`. Frames are not covered here:
 * jsdom has no media engine and no canvas; the drawing rules are in
 * `desktop-call-window-model.test.ts`.
 */

// @vitest-environment jsdom

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { callActions, callState, type Participant } from "../state/call-state";
import { initWasm, present, renderUi, settle } from "../test/ui";
import type { CallWindowSources } from "./CallWindow";
import { parseCallWindowView, type CallWindowView } from "./desktop-call-window-model";

const host = vi.hoisted(() => ({
  views: [] as (object | null)[],
  handler: null as ((action: "toggle-mute" | "expand") => void) | null,
}));

vi.mock("../platform/host", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../platform/host")>();
  return {
    ...actual,
    postCallWindowView: (view: object | null): Promise<void> => {
      host.views.push(view);
      return Promise.resolve();
    },
    postCallWindowFrame: (): Promise<boolean> => Promise.resolve(true),
    onHostCallWindowAction: (h: (action: "toggle-mute" | "expand") => void): (() => void) => {
      host.handler = h;
      return () => {
        if (host.handler === h) host.handler = null;
      };
    },
  };
});

type CallWindowComponent = typeof import("./CallWindow")["CallWindow"];
let CallWindow: CallWindowComponent;

beforeAll(async () => {
  await initWasm();
  ({ CallWindow } = await import("./CallWindow"));
  await settle();
});

beforeEach(() => {
  callActions.reset();
  host.views = [];
  host.handler = null;
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

function connected(local: Partial<Participant> = {}): void {
  callActions.setParticipants([
    participant(local),
    participant({ id: "peer", displayName: "Dana", isLocal: false }),
  ]);
  callActions.setPhase({ kind: "connected" });
}

function mount(over: Partial<Parameters<CallWindowComponent>[0]> = {}): { expanded: () => number } {
  const onExpand = vi.fn();
  const sources: CallWindowSources = { remoteStreamFor: () => undefined, localStream: () => undefined };
  renderUi(() => (
    <CallWindow
      peerName="Dana"
      peerId="group-dana"
      groupCall={false}
      ringing={false}
      callScreenUp={false}
      onExpand={onExpand}
      sources={sources}
      presentation="os-window"
      {...over}
    />
  ));
  return { expanded: () => onExpand.mock.calls.length };
}

function lastView(): CallWindowView | null {
  const v = host.views[host.views.length - 1];
  return v === null || v === undefined ? null : parseCallWindowView(v);
}

describe("the collapsed call in the desktop app", () => {
  it("draws no in-app window: the picture is built out of sight, with no controls", () => {
    connected();
    mount();
    expect(present(".lk-call-window.lk-call-window-relay")).toBe(true);
    expect(present(".lk-call-window-open")).toBe(false);
    expect(present(".lk-call-window-mute")).toBe(false);
    expect(document.querySelector(".lk-call-window")?.getAttribute("aria-hidden")).toBe("true");
  });

  it("hands the OS window a view it can read, with the name, the time and the mute button", () => {
    connected();
    mount();
    const v = lastView();
    expect(v).not.toBeNull();
    expect(v?.name).toBe("Dana");
    expect(v?.others).toBe(0);
    expect(v?.mute?.label).toBe("Mute microphone");
    expect(v?.mute?.disabled).toBe(false);
    // No camera, no photo: the initials on the person's colour.
    expect(v?.picture.kind).toBe("initials");
    expect(v?.inset).toBe(false);
  });

  it("follows a change: muting shows on the OS window's button", async () => {
    connected();
    mount();
    callActions.setMic(false);
    // The relay posts on its timer; the next view carries the change.
    await new Promise<void>((resolve) => setTimeout(resolve, 120));
    expect(lastView()?.mute?.label).toBe("Unmute microphone");
  });

  it("mutes when the OS window's mute button is pressed, without bringing the call back", () => {
    connected();
    const { expanded } = mount();
    expect(host.handler).not.toBeNull();
    host.handler?.("toggle-mute");
    expect(callState.participants().find((p) => p.isLocal)?.micOn).toBe(false);
    host.handler?.("toggle-mute");
    expect(callState.participants().find((p) => p.isLocal)?.micOn).toBe(true);
    expect(expanded()).toBe(0);
  });

  it("brings the call back when the OS window is clicked", () => {
    connected();
    const { expanded } = mount();
    host.handler?.("expand");
    expect(expanded()).toBe(1);
  });

  it("does not mute through the OS window before there is anything to mute", () => {
    callActions.setParticipants([participant()]);
    callActions.setPhase({ kind: "calling" });
    mount();
    expect(lastView()?.mute?.disabled).toBe(true);
    host.handler?.("toggle-mute");
    expect(callState.participants().find((p) => p.isLocal)?.micOn).toBe(true);
  });

  it("hides the OS window when the call ends, and stops listening", () => {
    connected();
    mount();
    expect(host.handler).not.toBeNull();
    callActions.endCall();
    expect(present(".lk-call-window")).toBe(false);
    expect(host.views[host.views.length - 1]).toBeNull();
    expect(host.handler).toBeNull();
  });

  it("is not drawn in a browser build as anything but the in-app window", () => {
    connected();
    mount({ presentation: "in-app" });
    expect(present(".lk-call-window-relay")).toBe(false);
    expect(present(".lk-call-window-open")).toBe(true);
    expect(host.views).toHaveLength(0);
  });
});
