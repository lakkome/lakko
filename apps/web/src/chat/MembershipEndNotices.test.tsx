// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

/**
 * `docs/ux/group-and-channel-privacy.md` section 9.1 — the banner at the top of
 * Chats and the dialog over the conversation.
 */
import { createSignal } from "solid-js";
import { beforeAll, describe, expect, it, vi } from "vitest";

import { all, one, present, renderUi, settle } from "../test/ui";

const dismissed: string[] = [];
const notices: { value: readonly EndNotice[] } = { value: [] };

vi.mock("./MembershipEndContext", () => ({
  useMembershipEnd: () => ({
    notices: () => notices.value,
    dismissNotice: (key: string) => dismissed.push(key),
  }),
}));

import { MembershipEndBanners, MembershipEndDialog, noticeTitle } from "./MembershipEndNotices";
import type { EndNotice } from "./membership-end";

const NOTICE: EndNotice = {
  key: "2:aa",
  title: "Book club",
  routeIdsB64: ["qg=="],
  draft: null,
  atMs: 1,
};

beforeAll(() => {
  // jsdom has no modal dialog.
  HTMLDialogElement.prototype.showModal = function showModal(this: HTMLDialogElement): void {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function close(this: HTMLDialogElement): void {
    this.removeAttribute("open");
    this.dispatchEvent(new Event("close"));
  };
});

describe("the banner", () => {
  it("says the name and never who, and closes", () => {
    notices.value = [NOTICE];
    dismissed.length = 0;
    renderUi(() => <MembershipEndBanners activeSlug={null} />);
    const banner = one(".lk-end-banner");
    expect(banner.textContent).toContain("You are no longer in Book club.");
    one<HTMLButtonElement>("button", banner).click();
    expect(dismissed).toEqual(["2:aa"]);
  });

  it("is not shown for the conversation open behind its dialog", () => {
    notices.value = [NOTICE, { ...NOTICE, key: "2:bb", title: "Other", routeIdsB64: ["uw=="] }];
    renderUi(() => <MembershipEndBanners activeSlug="qg" />);
    const shown = all(".lk-end-banner");
    expect(shown).toHaveLength(1);
    expect(shown[0]?.textContent).toContain("Other");
  });

  it("a conversation with no name known still reads as a sentence", () => {
    expect(noticeTitle({ title: "  " })).not.toContain("  .");
    expect(noticeTitle({ title: "  " }).endsWith(" .")).toBe(false);
  });
});

describe("the dialog", () => {
  it("has the title, the body and one button", async () => {
    const done = vi.fn();
    renderUi(() => <MembershipEndDialog notice={NOTICE} onDone={done} />);
    await settle();
    const dialog = one<HTMLDialogElement>("dialog");
    expect(dialog.hasAttribute("open")).toBe(true);
    expect(dialog.textContent).toContain("You are no longer in Book club.");
    expect(dialog.textContent).toContain("The group and its messages are gone from your devices.");
    expect(all("button", dialog)).toHaveLength(1);
    expect(present(".lk-end-dialog-text")).toBe(false);
    one<HTMLButtonElement>("button", dialog).click();
    expect(done).toHaveBeenCalledTimes(1);
  });

  it("shows held text selectable, never in a field, with Copy", async () => {
    const write = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { ...globalThis.navigator, clipboard: { writeText: write } });
    const [notice] = createSignal<EndNotice>({ ...NOTICE, draft: "half a sentence" });
    renderUi(() => <MembershipEndDialog notice={notice()} onDone={() => undefined} />);
    await settle();
    expect(one(".lk-end-dialog-text").textContent).toBe("half a sentence");
    expect(present("textarea")).toBe(false);
    expect(present("input")).toBe(false);
    one<HTMLButtonElement>(".lk-end-dialog-draft button").click();
    await settle();
    expect(write).toHaveBeenCalledWith("half a sentence");
    vi.unstubAllGlobals();
  });
});
