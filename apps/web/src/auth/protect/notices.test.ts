// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../../test/fake-storage";
import {
  clearNoticeStores,
  consumeSelfMarks,
  cursorAfterSkipped,
  firstNoticeQuery,
  FIRST_LANDING_WINDOW_MS,
  noticePasswordOpen,
  NOTICE_PASSWORD_OPEN_MS,
  splitOwnActions,
  isOwnAction,
  markOwnAction,
  noticeCursorKnown,
  noticesToShow,
  noticeView,
  NOTICE_KINDS,
  parseNoticePage,
  planNotices,
  readNoticeCursor,
  readSelfMarks,
  secureActions,
  SELF_WINDOW_MS,
  startNoticeCursor,
  writeNoticeCursor,
  type AccountNotice,
} from "./notices";
import { t } from "../../l10n";

const NOW = 1_800_000_000_000;
const n = (id: number, kind: AccountNotice["kind"], atMs = NOW): AccountNotice => ({ id, kind, atMs });

describe("reading GET /account/notices", () => {
  it("takes the notices, the cursor and whether more follow", () => {
    expect(
      parseNoticePage({
        notices: [{ id: 4, kind: "recovery-email-added", at_ms: NOW }],
        next_after: 4,
        more: false,
      }),
    ).toEqual({ notices: [n(4, "recovery-email-added")], nextAfter: 4, more: false });
  });

  it("drops a kind this build does not know, and still moves past it", () => {
    const page = parseNoticePage({
      notices: [
        { id: 5, kind: "something-new", at_ms: NOW },
        { id: 6, kind: "recovery-phrase-saved", at_ms: NOW },
      ],
      next_after: 6,
      more: true,
    });
    expect(page?.notices.map((x) => x.id)).toEqual([6]);
    expect(page?.nextAfter).toBe(6);
  });

  it("refuses a body that is not a page", () => {
    expect(parseNoticePage(null)).toBeNull();
    expect(parseNoticePage({ notices: [], more: false })).toBeNull();
    expect(parseNoticePage({ notices: {}, next_after: 0, more: false })).toBeNull();
  });
});

describe("which notices a device shows", () => {
  it("shows those above its cursor, oldest first, each once", () => {
    const all = [n(7, "recovery-email-removed"), n(3, "recovery-email-added"), n(5, "recovery-phrase-saved")];
    expect(noticesToShow(all, 3, []).map((x) => x.id)).toEqual([5, 7]);
    // Answered ones are behind the cursor and never come back.
    expect(noticesToShow(all, 7, [])).toEqual([]);
  });

  it("does not show a device its own action", () => {
    const own = n(8, "recovery-email-added", NOW + 2_000);
    expect(isOwnAction(own, [{ kind: "recovery-email-added", atMs: NOW }])).toBe(true);
    expect(noticesToShow([own], 0, [{ kind: "recovery-email-added", atMs: NOW }])).toEqual([]);
  });

  it("still shows the same kind of action taken by someone else at another time", () => {
    const later = n(9, "recovery-email-added", NOW + SELF_WINDOW_MS + 1);
    expect(isOwnAction(later, [{ kind: "recovery-email-added", atMs: NOW }])).toBe(false);
    // A clock a little ahead of the server's is tolerated both ways.
    expect(isOwnAction(n(9, "recovery-email-added", NOW - SELF_WINDOW_MS), [{ kind: "recovery-email-added", atMs: NOW }])).toBe(true);
  });

  it("an own action never hides a notice of another kind", () => {
    const other = n(10, "password-changed-by-email", NOW);
    expect(isOwnAction(other, [{ kind: "recovery-email-added", atMs: NOW }])).toBe(false);
  });

  it("moves the cursor at once past what it skips, and no further than the first shown", () => {
    const marks = [{ kind: "recovery-email-added", atMs: NOW } as const];
    const page = {
      notices: [n(4, "recovery-email-added"), n(5, "password-changed-by-email"), n(6, "recovery-email-added")],
      nextAfter: 6,
      more: false,
    };
    // 4 is its own (skipped); 5 is shown and not yet answered.
    expect(cursorAfterSkipped(page, 3, marks)).toBe(4);
    // Nothing to show: to the end.
    expect(cursorAfterSkipped({ ...page, notices: [n(4, "recovery-email-added")], nextAfter: 4 }, 3, marks)).toBe(4);
    // Never backwards.
    expect(cursorAfterSkipped({ notices: [], nextAfter: 2, more: false }, 9, [])).toBe(9);
  });
});

describe("one notice per marker (alignment E)", () => {
  const mark = { kind: "recovery-email-added", atMs: NOW } as const;

  it("the window is ten minutes", () => {
    expect(SELF_WINDOW_MS).toBe(10 * 60 * 1000);
  });

  it("a marker hides the earliest notice it matches, and only that one", () => {
    // Two "email added" notices inside the same ten minutes: this device's
    // own, and someone else's.
    const split = splitOwnActions(
      [n(5, "recovery-email-added", NOW + 9_000), n(4, "recovery-email-added", NOW + 1_000)],
      0,
      [mark],
    );
    expect(split.own.map((x) => x.id)).toEqual([4]);
    expect(split.show.map((x) => x.id)).toEqual([5]);
    expect(split.used).toEqual([mark]);
  });

  it("two own actions need two markers", () => {
    const two = [mark, { kind: "recovery-email-added", atMs: NOW + 60_000 } as const];
    const split = splitOwnActions(
      [n(4, "recovery-email-added", NOW), n(5, "recovery-email-added", NOW + 61_000)],
      0,
      two,
    );
    expect(split.show).toEqual([]);
    expect(split.used).toHaveLength(2);
  });

  it("a used marker is forgotten, so it never hides a later notice", () => {
    const local = new MemoryStorage();
    markOwnAction(local, "alice", "recovery-email-added", NOW);
    markOwnAction(local, "alice", "recovery-phrase-saved", NOW);
    consumeSelfMarks(local, "alice", [mark]);
    expect(readSelfMarks(local, "alice")).toEqual([{ kind: "recovery-phrase-saved", atMs: NOW }]);
  });
});

describe("one landing", () => {
  it("a device with no cursor asks for the last thirty days (alignment F)", () => {
    expect(firstNoticeQuery(false, 0, NOW)).toEqual({ kind: "since", sinceMs: NOW - FIRST_LANDING_WINDOW_MS });
    expect(FIRST_LANDING_WINDOW_MS).toBe(30 * 24 * 60 * 60 * 1000);
    // A clock near the epoch never asks for a negative time.
    expect(firstNoticeQuery(false, 0, 5)).toEqual({ kind: "since", sinceMs: 0 });
  });

  it("a device with a cursor asks from it", () => {
    expect(firstNoticeQuery(true, 12, NOW)).toEqual({ kind: "after", after: 12 });
  });

  it("the first landing shows what came back, minus its own action, and keeps a cursor", () => {
    const plan = planNotices({
      cursor: 0,
      notices: [n(2, "recovery-email-added", NOW + 100), n(3, "account-opened-by-email")],
      nextAfter: 3,
      marks: [{ kind: "recovery-email-added", atMs: NOW }],
    });
    expect(plan.show.map((x) => x.id)).toEqual([3]);
    // Past its own action, not past the notice still to be answered.
    expect(plan.cursor).toBe(2);
    expect(plan.usedMarks).toEqual([{ kind: "recovery-email-added", atMs: NOW }]);
  });

  it("a landing that found nothing still keeps a cursor", () => {
    expect(planNotices({ cursor: 0, notices: [], nextAfter: 0, marks: [] })).toEqual({
      show: [],
      cursor: 0,
      usedMarks: [],
    });
    const local = new MemoryStorage();
    startNoticeCursor(local, "alice", 0);
    expect(noticeCursorKnown(local, "alice")).toBe(true);
  });

  it("a device with a cursor shows what came after it", () => {
    const plan = planNotices({
      cursor: 2,
      notices: [n(3, "account-opened-by-email"), n(4, "password-changed-by-email")],
      nextAfter: 4,
      marks: [],
    });
    expect(plan.show.map((x) => x.id)).toEqual([3, 4]);
    expect(plan.cursor).toBe(2);
  });

  it("a device that just recovered by email is not told so", () => {
    const plan = planNotices({
      cursor: 2,
      notices: [n(3, "password-changed-by-email", NOW + 500)],
      nextAfter: 3,
      marks: [{ kind: "password-changed-by-email", atMs: NOW }],
    });
    expect(plan.show).toEqual([]);
    expect(plan.cursor).toBe(3);
  });
});

describe("the secure screen's password change (alignment G)", () => {
  it("is open for ten minutes from the notice, and only from a notice", () => {
    expect(noticePasswordOpen(NOW, NOW)).toBe(true);
    expect(noticePasswordOpen(NOW, NOW + NOTICE_PASSWORD_OPEN_MS - 1)).toBe(true);
    expect(noticePasswordOpen(NOW, NOW + NOTICE_PASSWORD_OPEN_MS)).toBe(false);
    expect(noticePasswordOpen(null, NOW)).toBe(false);
    // A clock that went backwards closes it.
    expect(noticePasswordOpen(NOW, NOW - 1)).toBe(false);
    expect(NOTICE_PASSWORD_OPEN_MS).toBe(10 * 60 * 1000);
  });
});

describe("what a notice says", () => {
  it("the two recovery notices have a title and a dated body", () => {
    expect(noticeView("password-changed-by-email")).toEqual({
      titleKey: "notice.password_by_email.title",
      bodyKey: "notice.password_by_email.body",
      dated: true,
    });
    expect(t(noticeView("account-opened-by-email").titleKey ?? "")).toBe(
      "Your account was opened with your recovery email",
    );
  });

  it("the four others are one line", () => {
    expect(t(noticeView("recovery-email-added").bodyKey)).toBe("A recovery email was added to your account.");
    expect(t(noticeView("recovery-email-replaced").bodyKey)).toBe("Your recovery email was replaced.");
    expect(t(noticeView("recovery-email-removed").bodyKey)).toBe("Your recovery email was removed.");
    expect(t(noticeView("recovery-phrase-saved").bodyKey)).toBe("A recovery phrase was saved for your account.");
    for (const kind of NOTICE_KINDS) expect(noticeView(kind).bodyKey.length).toBeGreaterThan(0);
  });
});

describe("It was not me", () => {
  it("offers a new password, removing the email and saving a phrase, in that order", () => {
    expect(secureActions({ canReplacePassword: true, emailEnabled: true, phraseSaved: false })).toEqual([
      "new-password",
      "remove-email",
      "save-phrase",
    ]);
  });

  it("does not offer to save a phrase when one is saved, nor to remove an email that is not there", () => {
    expect(secureActions({ canReplacePassword: true, emailEnabled: false, phraseSaved: true })).toEqual([
      "new-password",
    ]);
  });

  it("offers no new password on a device that does not hold the keys", () => {
    expect(secureActions({ canReplacePassword: false, emailEnabled: true, phraseSaved: true })).toEqual([
      "remove-email",
    ]);
  });
});

describe("the per-device stores", () => {
  it("the cursor only moves forward", () => {
    const local = new MemoryStorage();
    expect(noticeCursorKnown(local, "alice")).toBe(false);
    expect(readNoticeCursor(local, "alice")).toBe(0);
    startNoticeCursor(local, "alice", 4);
    expect(noticeCursorKnown(local, "alice")).toBe(true);
    writeNoticeCursor(local, "alice", 9);
    writeNoticeCursor(local, "alice", 6);
    expect(readNoticeCursor(local, "alice")).toBe(9);
    // Starting again does not rewind a device that has one.
    startNoticeCursor(local, "alice", 1);
    expect(readNoticeCursor(local, "alice")).toBe(9);
    expect(readNoticeCursor(local, "bob")).toBe(0);
  });

  it("a device whose first landing found no notices has a cursor from then on", () => {
    const local = new MemoryStorage();
    startNoticeCursor(local, "alice", 0);
    expect(noticeCursorKnown(local, "alice")).toBe(true);
  });

  it("remembers this device's own actions by kind and time", () => {
    const local = new MemoryStorage();
    markOwnAction(local, "alice", "recovery-email-added", NOW);
    markOwnAction(local, "alice", "recovery-phrase-saved", NOW + 5);
    expect(readSelfMarks(local, "alice")).toEqual([
      { kind: "recovery-email-added", atMs: NOW },
      { kind: "recovery-phrase-saved", atMs: NOW + 5 },
    ]);
    expect(readSelfMarks(local, "bob")).toEqual([]);
  });

  it("holds a kind and a time and nothing else: no address, no code", () => {
    const local = new MemoryStorage();
    markOwnAction(local, "alice", "recovery-email-added", NOW);
    startNoticeCursor(local, "alice", 2);
    for (const [, value] of local.entries()) {
      expect(value.includes("@")).toBe(false);
    }
    expect(Object.keys(readSelfMarks(local, "alice")[0] ?? {}).sort()).toEqual(["atMs", "kind"]);
  });

  it("reads a damaged row as empty, and is forgotten by the wipe", () => {
    const local = new MemoryStorage();
    local.setItem("lakko:account-notices-self:alice", "{not json");
    expect(readSelfMarks(local, "alice")).toEqual([]);
    startNoticeCursor(local, "alice", 2);
    clearNoticeStores(local, "alice");
    expect(local.entries()).toEqual([]);
  });
});
