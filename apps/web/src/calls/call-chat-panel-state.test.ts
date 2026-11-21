// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CHAT_PREVIEW_MS,
  chatControlName,
  chatPanelShape,
  composerInput,
  insertEmoji,
  EMPTY_COMPOSER,
  enterSends,
  initialChatPanel,
  panelClose,
  panelExpirePreview,
  panelIncoming,
  panelOpen,
  panelOwnSent,
  panelScrolled,
  previewAllowed,
  previewAt,
  startsOthersRun,
  unreadBadge,
  type ChatPanelMessage,
  type ChatTextRule,
} from "./call-chat-panel-state";

const from = (peerId: string, id: string, text = "hi"): ChatPanelMessage => ({
  id,
  own: false,
  peerId,
  text,
});
const own = (id: string): ChatPanelMessage => ({ id, own: true, peerId: null, text: "mine" });

describe("unread count and badge", () => {
  it("counts messages from others while the panel is closed", () => {
    let s = initialChatPanel();
    s = panelIncoming(s, from("a", "a:1"), 0);
    s = panelIncoming(s, from("b", "b:1"), 1);
    expect(s.unread).toBe(2);
  });

  it("never counts this device's own messages", () => {
    const s = panelIncoming(initialChatPanel(), own("own:1"), 0);
    expect(s.unread).toBe(0);
    expect(s.preview).toBeNull();
  });

  it("does not count while the panel is open, and opening clears the count", () => {
    let s = panelIncoming(initialChatPanel(), from("a", "a:1"), 0);
    s = panelOpen(s);
    expect(s.unread).toBe(0);
    s = panelIncoming(s, from("a", "a:2"), 1);
    expect(s.unread).toBe(0);
  });

  it("draws 1 to 9, then 9+", () => {
    expect(unreadBadge(0)).toBeNull();
    expect(unreadBadge(1)).toBe("1");
    expect(unreadBadge(9)).toBe("9");
    expect(unreadBadge(10)).toBe("9+");
    expect(unreadBadge(250)).toBe("9+");
  });

  it("names the circle by action and count", () => {
    expect(chatControlName(false, 0)).toEqual({ key: "call.control.chat_show_a11y" });
    expect(chatControlName(false, 3)).toEqual({ key: "call.control.chat_unread_a11y", count: 3 });
    expect(chatControlName(true, 3)).toEqual({ key: "call.control.chat_hide_a11y" });
  });
});

describe("the preview", () => {
  it("shows a message from someone else for four seconds while closed", () => {
    const s = panelIncoming(initialChatPanel(), from("a", "a:1", "hello"), 100);
    expect(previewAt(s, 100)?.text).toBe("hello");
    expect(previewAt(s, 100 + CHAT_PREVIEW_MS - 1)?.text).toBe("hello");
    expect(previewAt(s, 100 + CHAT_PREVIEW_MS)).toBeNull();
    expect(panelExpirePreview(s, 100 + CHAT_PREVIEW_MS).preview).toBeNull();
    expect(panelExpirePreview(s, 101).preview).not.toBeNull();
  });

  it("is replaced by a second message, which restarts the time", () => {
    let s = panelIncoming(initialChatPanel(), from("a", "a:1", "one"), 0);
    s = panelIncoming(s, from("b", "b:1", "two"), 3_000);
    expect(previewAt(s, 5_000)?.text).toBe("two");
    expect(previewAt(s, 5_000)?.peerId).toBe("b");
    expect(previewAt(s, 3_000 + CHAT_PREVIEW_MS)).toBeNull();
  });

  it("goes when the panel opens, and none is made while it is open", () => {
    let s = panelIncoming(initialChatPanel(), from("a", "a:1"), 0);
    s = panelOpen(s);
    expect(s.preview).toBeNull();
    s = panelIncoming(s, from("a", "a:2"), 10);
    expect(previewAt(s, 10)).toBeNull();
  });

  it("is never drawn over a sheet, the list or a menu, nor while the bar is faded", () => {
    const base = { barFaded: false, settingsOpen: false, participantsOpen: false, menuOpen: false };
    expect(previewAllowed(base)).toBe(true);
    expect(previewAllowed({ ...base, barFaded: true })).toBe(false);
    expect(previewAllowed({ ...base, settingsOpen: true })).toBe(false);
    expect(previewAllowed({ ...base, participantsOpen: true })).toBe(false);
    expect(previewAllowed({ ...base, menuOpen: true })).toBe(false);
  });
});

describe("the New messages pill", () => {
  it("appears for an arrival while scrolled up, and goes at the newest", () => {
    let s = panelOpen(initialChatPanel());
    s = panelScrolled(s, false);
    s = panelIncoming(s, from("a", "a:1"), 0);
    expect(s.newBelow).toBe(true);
    s = panelScrolled(s, true);
    expect(s.newBelow).toBe(false);
  });

  it("does not appear while at the newest message", () => {
    const s = panelIncoming(panelOpen(initialChatPanel()), from("a", "a:1"), 0);
    expect(s.newBelow).toBe(false);
  });

  it("goes when the person sends, or closes the panel", () => {
    let s = panelIncoming(panelScrolled(panelOpen(initialChatPanel()), false), from("a", "a:1"), 0);
    expect(panelOwnSent(s).newBelow).toBe(false);
    s = panelClose(s);
    expect(s.newBelow).toBe(false);
    expect(s.open).toBe(false);
  });
});

describe("runs of bubbles", () => {
  it("puts a name above the first bubble of a run from another person", () => {
    const list = [from("a", "a:1"), from("a", "a:2"), own("own:1"), from("a", "a:3"), from("b", "b:1")];
    expect(list.map((_, i) => startsOthersRun(list, i))).toEqual([true, false, false, true, true]);
  });
});

describe("the panel's shape", () => {
  it("is a sheet under 900 and a column from 900", () => {
    expect(chatPanelShape(375)).toBe("sheet");
    expect(chatPanelShape(899)).toBe("sheet");
    expect(chatPanelShape(900)).toBe("column");
  });
});

describe("the composer", () => {
  // A stand-in for the core's rule with a 10-byte bound.
  const rule: ChatTextRule = {
    clampCallChatText: (t) => {
      let out = "";
      for (const ch of t) {
        if (new TextEncoder().encode(out + ch).length > 10) break;
        out += ch;
      }
      return out;
    },
    checkCallChatText: (t) =>
      t.length === 0 ? "empty" : t.trim() === "" ? "blank" : new TextEncoder().encode(t).length > 10 ? "tooLong" : undefined,
  };

  it("enables Send only for a sendable text", () => {
    expect(composerInput("", EMPTY_COMPOSER, rule).sendable).toBe(false);
    expect(composerInput("   \n", EMPTY_COMPOSER, rule).sendable).toBe(false);
    expect(composerInput("hi", EMPTY_COMPOSER, rule).sendable).toBe(true);
  });

  it("makes line breaks LF", () => {
    expect(composerInput("a\r\nb\rc", EMPTY_COMPOSER, rule).text).toBe("a\nb\nc");
  });

  it("takes no more text at the bound and says so until the text gets shorter", () => {
    const full = composerInput("0123456789", EMPTY_COMPOSER, rule);
    expect(full.tooLong).toBe(false);
    const over = composerInput("0123456789x", full, rule);
    expect(over.text).toBe("0123456789");
    expect(over.tooLong).toBe(true);
    expect(over.sendable).toBe(true);
    // The line stays while the text is still at the bound.
    expect(composerInput("0123456789", over, rule).tooLong).toBe(true);
    expect(composerInput("012345678", over, rule).tooLong).toBe(false);
  });

  it("puts a picked emoji over the selection and the caret after it", () => {
    const r = insertEmoji("ab", 1, 1, "\u{1F600}", EMPTY_COMPOSER, rule);
    expect(r.inserted).toBe(true);
    expect(r.composer.text).toBe("a\u{1F600}b");
    expect(r.caret).toBe(3);
    expect(r.composer.sendable).toBe(true);
    const replaced = insertEmoji("abc", 1, 2, "!", EMPTY_COMPOSER, rule);
    expect(replaced.composer.text).toBe("a!c");
    expect(replaced.caret).toBe(2);
    // A reversed or out-of-range selection reads as the caret at its ends.
    expect(insertEmoji("ab", 9, 9, "x", EMPTY_COMPOSER, rule).composer.text).toBe("abx");
  });

  it("does not put in an emoji that passes the bound, and says so", () => {
    const start = composerInput("0123456", EMPTY_COMPOSER, rule);
    // 7 bytes + a 4-byte emoji = 11 > 10.
    const r = insertEmoji("0123456", 7, 7, "\u{1F600}", start, rule);
    expect(r.inserted).toBe(false);
    expect(r.composer.text).toBe("0123456");
    expect(r.composer.tooLong).toBe(true);
    expect(r.caret).toBe(7);
  });

  it("sends on Enter from a keyboard, never with Shift, while composing, or on a phone", () => {
    const k = { key: "Enter", shiftKey: false, isComposing: false, touchKeyboard: false };
    expect(enterSends(k)).toBe(true);
    expect(enterSends({ ...k, shiftKey: true })).toBe(false);
    expect(enterSends({ ...k, isComposing: true })).toBe(false);
    expect(enterSends({ ...k, touchKeyboard: true })).toBe(false);
    expect(enterSends({ ...k, key: "a" })).toBe(false);
  });
});
