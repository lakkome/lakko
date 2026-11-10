// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The page of the collapsed call's own window in the desktop app
 * (`docs/ux/calls.md` section 4.5.4a), rendered. The host is stood in for at
 * `platform/host.ts`: what the page sends back is recorded, and a view is
 * handed in the way the host's script call hands it. Images are not covered:
 * jsdom decodes none.
 */

// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from "vitest";

import { one, pointer, present, renderUi, settle } from "../test/ui";
import type { CallWindowView } from "./desktop-call-window-model";

const host = vi.hoisted(() => ({ sent: [] as string[] }));

vi.mock("../platform/host", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../platform/host")>();
  return {
    ...actual,
    fetchCallWindowState: (): Promise<null> => Promise.resolve(null),
    postCallWindowAction: (action: string): Promise<void> => {
      host.sent.push(action);
      return Promise.resolve();
    },
    startCallWindowDrag: (): Promise<void> => {
      host.sent.push("drag");
      return Promise.resolve();
    },
  };
});

import { DesktopCallWindowPage } from "./DesktopCallWindowPage";

const VIEW: CallWindowView = {
  status: "12:04",
  encrypted: true,
  name: "Dana",
  others: 3,
  mute: { icon: "mic", tone: "quiet", pressed: false, label: "Mute microphone", disabled: false },
  picture: { kind: "initials", text: "DA", tone: 2 },
  inset: false,
  spoken: "Return to the call with Dana",
  announced: "Connected",
  region: "Call",
  lang: "en",
  dir: "ltr",
};

beforeEach(() => {
  host.sent = [];
});

async function mounted(view: unknown = VIEW): Promise<void> {
  renderUi(() => <DesktopCallWindowPage />);
  await settle();
  window.__lakkoCallWindow?.view(view);
}

describe("the OS call window's page", () => {
  it("draws what the main page relayed: time, lock, name, +N, the initials", async () => {
    await mounted();
    expect(one(".lk-call-window-state").textContent).toBe("12:04");
    expect(present(".lk-call-window-mark")).toBe(true);
    expect(one(".lk-call-window-name").textContent).toBe("Dana");
    expect(one(".lk-call-window-more").textContent).toBe("+3");
    expect(one(".lk-call-tile-avatar").textContent).toBe("DA");
    expect(one(".lk-call-window-picture").getAttribute("data-tone")).toBe("2");
    expect(one(".lk-call-window-mute").getAttribute("aria-label")).toBe("Mute microphone");
  });

  it("shows nothing for a malformed view, and nothing after the host clears it", async () => {
    await mounted({ name: "Dana" });
    expect(present(".lk-call-window")).toBe(false);
    window.__lakkoCallWindow?.view(VIEW);
    expect(present(".lk-call-window")).toBe(true);
    window.__lakkoCallWindow?.clear();
    expect(present(".lk-call-window")).toBe(false);
  });

  it("brings the call back on a click that does not move", async () => {
    await mounted();
    const press = one(".lk-call-window-press");
    pointer(press, "pointerdown", { screenX: 100, screenY: 100, buttons: 1 });
    pointer(press, "pointermove", { screenX: 102, screenY: 101, buttons: 1 });
    pointer(press, "pointerup", { screenX: 102, screenY: 101 });
    expect(host.sent).toEqual(["expand"]);
  });

  it("hands a press that moves to the OS as a drag, and does not bring the call back", async () => {
    await mounted();
    const press = one(".lk-call-window-press");
    pointer(press, "pointerdown", { screenX: 100, screenY: 100, buttons: 1 });
    pointer(press, "pointermove", { screenX: 110, screenY: 100, buttons: 1 });
    pointer(press, "pointermove", { screenX: 140, screenY: 100, buttons: 1 });
    pointer(press, "pointerup", { screenX: 140, screenY: 100 });
    expect(host.sent).toEqual(["drag"]);
  });

  it("mutes from its button and does nothing else", async () => {
    await mounted();
    const mute = one<HTMLButtonElement>(".lk-call-window-mute");
    pointer(mute, "pointerdown", { screenX: 300, screenY: 10, buttons: 1 });
    mute.click();
    pointer(mute, "pointerup", { screenX: 300, screenY: 10 });
    expect(host.sent).toEqual(["mute"]);
  });

  it("draws the mute button inert while there is nothing to mute, and no End", async () => {
    await mounted({ ...VIEW, mute: { ...VIEW.mute, disabled: true } });
    expect(one<HTMLButtonElement>(".lk-call-window-mute").disabled).toBe(true);
    const buttons = Array.from(document.querySelectorAll("button")).map((b) => b.getAttribute("aria-label"));
    expect(buttons).toEqual(["Mute microphone"]);
  });

  it("takes the interface direction from the view", async () => {
    await mounted({ ...VIEW, lang: "fa", dir: "rtl" });
    expect(document.documentElement.dir).toBe("rtl");
    expect(document.documentElement.lang).toBe("fa");
  });
});
