// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "A device was added to your account" (`added-notice.ts`;
 * identity-recovery UX section 17.5): what makes the notice appear, what
 * suppresses it, and what ends it.
 */
import { describe, expect, it } from "vitest";

import {
  dismissNotice,
  letIn,
  noticeView,
  observeRoster,
  parseNoticeState,
  shouldNotify,
  type AddedNoticeState,
} from "./added-notice";

const roster = (keys: string[], addedHere: string[] = []) => ({ keys, ownKey: "me", addedHere });

function seeded(keys: string[]): AddedNoticeState {
  const first = observeRoster(null, roster(keys), 1).state;
  if (first === null) throw new Error("not seeded");
  return first;
}

describe("observeRoster", () => {
  it("the first roster seeds the acknowledged set: nothing about itself or what was already there", () => {
    const r = observeRoster(null, roster(["me", "old-1", "old-2"]), 100);
    expect(r.added).toEqual([]);
    expect([...(r.state?.known ?? [])].sort()).toEqual(["me", "old-1", "old-2"]);
    expect(noticeView(r.state).visible).toBe(false);
  });

  it("a device that joins later is the notice, with the time this device learned of it", () => {
    const r = observeRoster(seeded(["me", "old"]), roster(["me", "old", "new"]), 5_000);
    expect(r.added).toEqual(["new"]);
    expect(r.state?.pending).toEqual([{ key: "new", atMs: 5_000 }]);
    const view = noticeView(r.state);
    expect(view).toEqual({
      visible: true,
      titleKey: "devices.added_notice.title",
      latestAtMs: 5_000,
      newKeys: ["new"],
    });
  });

  it("seeing the same roster again adds nothing and keeps the first time", () => {
    const once = observeRoster(seeded(["me"]), roster(["me", "new"]), 5_000).state;
    const again = observeRoster(once, roster(["me", "new"]), 9_000);
    expect(again.added).toEqual([]);
    expect(again.state?.pending).toEqual([{ key: "new", atMs: 5_000 }]);
  });

  it("two or more devices use the plural title and the most recent time", () => {
    const one = observeRoster(seeded(["me"]), roster(["me", "a"]), 1_000).state;
    const two = observeRoster(one, roster(["me", "a", "b"]), 3_000).state;
    expect(noticeView(two)).toMatchObject({
      titleKey: "devices.added_notice.title_many",
      latestAtMs: 3_000,
      newKeys: ["a", "b"],
    });
  });

  it("a device this device let in is acknowledged at once, in either order", () => {
    // The change was made here: the roster names it as added here.
    const viaRoster = observeRoster(seeded(["me"]), roster(["me", "new"], ["new"]), 1_000);
    expect(viaRoster.added).toEqual([]);
    expect(noticeView(viaRoster.state).visible).toBe(false);
    // The let-in mark arrives before the roster.
    const marked = letIn(seeded(["me"]), "new");
    expect(observeRoster(marked, roster(["me", "new"]), 1_000).added).toEqual([]);
    // The let-in mark arrives after the roster already flagged it.
    const flagged = observeRoster(seeded(["me"]), roster(["me", "new"]), 1_000).state;
    expect(noticeView(letIn(flagged, "new")).visible).toBe(false);
  });

  it("never shows the notice about this device itself", () => {
    const r = observeRoster({ v: 1, known: ["old"], letIn: [], pending: [] }, roster(["me", "old"]), 1);
    expect(r.added).toEqual([]);
    expect(r.state?.known).toContain("me");
  });

  it("the device's removal ends the notice", () => {
    const flagged = observeRoster(seeded(["me"]), roster(["me", "new"]), 1_000).state;
    const after = observeRoster(flagged, roster(["me"]), 2_000);
    expect(noticeView(after.state).visible).toBe(false);
    // The same key coming back is news again.
    expect(observeRoster(after.state, roster(["me", "new"]), 3_000).added).toEqual(["new"]);
  });

  it("a torn read changes nothing: empty, or without this device", () => {
    const flagged = observeRoster(seeded(["me", "old"]), roster(["me", "old", "new"]), 1_000).state;
    expect(observeRoster(flagged, roster([]), 2_000)).toEqual({ state: flagged, added: [] });
    expect(observeRoster(flagged, roster(["old", "x"]), 2_000)).toEqual({ state: flagged, added: [] });
    // And it never seeds: the next real roster is still the first one.
    expect(observeRoster(null, roster([]), 1).state).toBeNull();
  });
});

describe("dismissNotice", () => {
  it("acknowledges every pending device; the `New` label goes with the banner", () => {
    const flagged = observeRoster(seeded(["me"]), roster(["me", "a", "b"]), 1_000).state;
    if (flagged === null) throw new Error("unreachable");
    const dismissed = dismissNotice(flagged);
    expect(noticeView(dismissed)).toMatchObject({ visible: false, newKeys: [] });
    // It does not come back for the same devices.
    expect(observeRoster(dismissed, roster(["me", "a", "b"]), 9_000).added).toEqual([]);
  });
});

describe("shouldNotify", () => {
  it("only when something was added, the app is not in front, and notifications are allowed", () => {
    expect(shouldNotify({ added: 1, hidden: true, permission: "granted" })).toBe(true);
    expect(shouldNotify({ added: 0, hidden: true, permission: "granted" })).toBe(false);
    expect(shouldNotify({ added: 1, hidden: false, permission: "granted" })).toBe(false);
    for (const permission of ["denied", "default", "unsupported"] as const) {
      expect(shouldNotify({ added: 1, hidden: true, permission })).toBe(false);
    }
  });
});

describe("parseNoticeState", () => {
  it("round-trips a stored row", () => {
    const state: AddedNoticeState = {
      v: 1,
      known: ["me"],
      letIn: ["x"],
      pending: [{ key: "new", atMs: 7 }],
    };
    expect(parseNoticeState(JSON.stringify(state))).toEqual(state);
  });

  it("reads anything of another shape as nothing", () => {
    for (const raw of [
      null,
      "",
      "not json",
      "null",
      JSON.stringify({ v: 2, known: [], letIn: [], pending: [] }),
      JSON.stringify({ v: 1, known: [1], letIn: [], pending: [] }),
      JSON.stringify({ v: 1, known: [], letIn: [], pending: [{ key: "a" }] }),
      JSON.stringify({ v: 1, known: [], letIn: [], pending: [null] }),
    ]) {
      expect(parseNoticeState(raw)).toBeNull();
    }
  });
});
