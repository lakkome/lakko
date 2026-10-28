// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  clearListCommand,
  conversationListMounted,
  folderTabIndexFor,
  neighbourHref,
  noteConversationListMounted,
  pendingListCommand,
  requestListCommand,
  shortcutFor,
  type KeyPress,
} from "./shortcuts";

const key = (k: string, mods: Partial<KeyPress> = {}): KeyPress => ({
  key: k,
  code: "",
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...mods,
});
const page = { editable: false, composer: false };
const field = { editable: true, composer: false };
const composer = { editable: true, composer: true };

describe("shortcutFor", () => {
  it("uses Cmd on a Mac and Ctrl elsewhere", () => {
    expect(shortcutFor(key("k", { metaKey: true }), true, page)).toBe("search");
    expect(shortcutFor(key("k", { ctrlKey: true }), true, page)).toBeNull();
    expect(shortcutFor(key("k", { ctrlKey: true }), false, page)).toBe("search");
    expect(shortcutFor(key("K", { ctrlKey: true }), false, page)).toBe("search");
    expect(shortcutFor(key("n", { metaKey: true }), true, page)).toBe("create");
    expect(shortcutFor(key("n", { metaKey: true }), false, page)).toBeNull();
  });

  it("takes Mod+K and Mod+N from any field", () => {
    expect(shortcutFor(key("k", { metaKey: true }), true, field)).toBe("search");
    expect(shortcutFor(key("n", { metaKey: true }), true, field)).toBe("create");
  });

  it("refuses extra modifiers and other keys", () => {
    expect(shortcutFor(key("k", { metaKey: true, shiftKey: true }), true, page)).toBeNull();
    expect(shortcutFor(key("k", { metaKey: true, altKey: true }), true, page)).toBeNull();
    expect(shortcutFor(key("1", { metaKey: true }), true, page)).toBeNull();
    expect(shortcutFor(key("ArrowUp"), true, page)).toBeNull();
  });

  it("moves between conversations with Alt+Up / Alt+Down, not from another field", () => {
    expect(shortcutFor(key("ArrowUp", { altKey: true }), true, page)).toBe("previous");
    expect(shortcutFor(key("ArrowDown", { altKey: true }), false, composer)).toBe("next");
    expect(shortcutFor(key("ArrowDown", { altKey: true }), false, field)).toBeNull();
    expect(shortcutFor(key("ArrowDown", { altKey: true, metaKey: true }), true, page)).toBeNull();
  });
});

describe("destinations: Mod+Shift+1..5", () => {
  // Shift changes `key` ("!" on US, "&" on French AZERTY), so the code decides.
  const digit = (n: number, key: string, mods: Partial<KeyPress>): KeyPress =>
    ({ ...keyPress(key), code: `Digit${n}`, ...mods });
  function keyPress(k: string): KeyPress {
    return { key: k, code: "", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false };
  }

  it("go to Chats, Feed, Contacts, Calls, Settings by the physical key", () => {
    const shifted = ["!", "@", "#", "$", "%"];
    const want = ["chats", "feed", "contacts", "calls", "settings"];
    for (let n = 1; n <= 5; n += 1) {
      expect(
        shortcutFor(digit(n, shifted[n - 1] ?? "", { metaKey: true, shiftKey: true }), true, page),
      ).toEqual({ destination: want[n - 1] });
      expect(
        shortcutFor(digit(n, "&", { ctrlKey: true, shiftKey: true }), false, page),
      ).toEqual({ destination: want[n - 1] });
    }
  });

  it("work while a field has focus", () => {
    expect(
      shortcutFor(digit(3, "#", { metaKey: true, shiftKey: true }), true, field),
    ).toEqual({ destination: "contacts" });
  });

  it("are only 1..5, and need Mod and Shift without Alt", () => {
    expect(shortcutFor(digit(6, "^", { metaKey: true, shiftKey: true }), true, page)).toBeNull();
    expect(shortcutFor(digit(1, "1", { metaKey: true }), true, page)).toBeNull();
    expect(shortcutFor(digit(1, "!", { shiftKey: true }), true, page)).toBeNull();
    expect(
      shortcutFor(digit(1, "!", { metaKey: true, shiftKey: true, altKey: true }), true, page),
    ).toBeNull();
    expect(shortcutFor(digit(1, "!", { ctrlKey: true, shiftKey: true }), true, page)).toBeNull();
  });

  it("never also select a folder; Mod+1..9 without Shift still does", () => {
    expect(folderTabIndexFor(digit(1, "!", { metaKey: true, shiftKey: true }))).toBeNull();
    expect(folderTabIndexFor(digit(2, "2", { metaKey: true, shiftKey: true }))).toBeNull();
    expect(folderTabIndexFor(digit(1, "1", { metaKey: true }))).toBe(0);
    expect(folderTabIndexFor(digit(9, "9", { ctrlKey: true }))).toBe(8);
    expect(folderTabIndexFor(digit(1, "1", { ctrlKey: true, altKey: true }))).toBeNull();
    expect(folderTabIndexFor(keyPress("1"))).toBeNull();
  });
});

describe("neighbourHref", () => {
  const list = ["/a", "/b", "/c"];
  it("steps in list order and stops at the ends", () => {
    expect(neighbourHref(list, "/b", "next")).toBe("/c");
    expect(neighbourHref(list, "/b", "previous")).toBe("/a");
    expect(neighbourHref(list, "/c", "next")).toBeNull();
    expect(neighbourHref(list, "/a", "previous")).toBeNull();
  });
  it("opens the first or the last with none open", () => {
    expect(neighbourHref(list, null, "next")).toBe("/a");
    expect(neighbourHref(list, "/x", "previous")).toBe("/c");
    expect(neighbourHref([], null, "next")).toBeNull();
  });
});

describe("the list hand-off", () => {
  it("keeps a command until taken and counts mounted lists", () => {
    expect(conversationListMounted()).toBe(false);
    const off = noteConversationListMounted();
    expect(conversationListMounted()).toBe(true);
    requestListCommand("create");
    expect(pendingListCommand()).toBe("create");
    clearListCommand();
    expect(pendingListCommand()).toBeNull();
    off();
    expect(conversationListMounted()).toBe(false);
  });
});
