// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

/**
 * `docs/ux/group-and-channel-privacy.md` section 9 — the short notice at the
 * bottom (2026-11-06) and the dialog over the conversation.
 */
import { createSignal } from "solid-js";
import { beforeAll, describe, expect, it, vi } from "vitest";

import { all, one, present, renderUi, settle } from "../test/ui";

const dismissed: string[] = [];
const claimed = new Set<string>();
const [noticeList, setNoticeList] = createSignal<readonly EndNotice[]>([]);

vi.mock("./MembershipEndContext", () => ({
  useMembershipEnd: () => ({
    notices: noticeList,
    dismissNotice: (key: string) => {
      dismissed.push(key);
      setNoticeList((l) => l.filter((n) => n.key !== key));
    },
    claimNotice: (key: string) => claimed.add(key),
    isNoticeClaimed: (key: string) => claimed.has(key),
  }),
}));

import { MembershipEndDialog, MembershipEndToast, noticeTitle } from "./MembershipEndNotices";
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

describe("the short notice at the bottom", () => {
  it("names the conversation, has no close icon, and goes by itself after 4 s", async () => {
    vi.useFakeTimers();
    try {
      dismissed.length = 0;
      claimed.clear();
      setNoticeList([]);
      renderUi(() => <MembershipEndToast />);
      setNoticeList([NOTICE]);
      await vi.advanceTimersByTimeAsync(400);
      const toast = one(".lk-end-toast");
      expect(toast.textContent).toContain("You are no longer in Book club.");
      expect(toast.querySelector("button")).toBeNull();
      // Told and forgotten: nothing is kept for later.
      expect(dismissed).toEqual(["2:aa"]);
      await vi.advanceTimersByTimeAsync(4_000);
      expect(present(".lk-end-toast")).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("removals noticed together, or while one is up, make one notice", async () => {
    vi.useFakeTimers();
    try {
      claimed.clear();
      setNoticeList([]);
      renderUi(() => <MembershipEndToast />);
      setNoticeList([NOTICE, { ...NOTICE, key: "2:bb", title: "Other" }]);
      await vi.advanceTimersByTimeAsync(400);
      expect(all(".lk-end-toast")).toHaveLength(1);
      expect(one(".lk-end-toast").textContent).toContain("You are no longer in Book club and 1 more.");
      setNoticeList((l) => [...l, { ...NOTICE, key: "2:cc", title: "Third" }]);
      await vi.advanceTimersByTimeAsync(400);
      expect(all(".lk-end-toast")).toHaveLength(1);
      expect(one(".lk-end-toast").textContent).toContain("You are no longer in Book club and 2 more.");
    } finally {
      vi.useRealTimers();
    }
  });

  it("a conversation whose dialog was shown gets no notice", async () => {
    vi.useFakeTimers();
    try {
      claimed.clear();
      setNoticeList([]);
      renderUi(() => <MembershipEndToast />);
      claimed.add("2:aa");
      setNoticeList([NOTICE]);
      await vi.advanceTimersByTimeAsync(400);
      expect(present(".lk-end-toast")).toBe(false);
    } finally {
      vi.useRealTimers();
    }
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
