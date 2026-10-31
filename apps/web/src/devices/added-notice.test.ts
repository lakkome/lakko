// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "A device was added to your account" (`added-notice.ts`;
 * identity-recovery UX section 17.5): what makes the notice appear, what
 * suppresses it, and what ends it.
 */
import { describe, expect, it } from "vitest";

import {
  applyAcknowledged,
  badgeVisible,
  beginDevicesVisit,
  extendVisit,
  letIn,
  noticeTag,
  noticeView,
  notificationAction,
  observeRoster,
  parseNoticeState,
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

describe("the account's acknowledgement (17.5, amended 2026-10-29)", () => {
  it("an acknowledged device is never pending, and moves to known", () => {
    const flagged = observeRoster(seeded(["me"]), roster(["me", "a", "b"]), 1_000).state;
    if (flagged === null) throw new Error("unreachable");
    const acked = new Set(["a"]);
    expect(noticeView(flagged, acked).newKeys).toEqual(["b"]);
    const folded = applyAcknowledged(flagged, acked);
    expect(folded.known).toContain("a");
    expect(folded.pending.map((p) => p.key)).toEqual(["b"]);
    // Nothing to fold: the same state object.
    expect(applyAcknowledged(folded, acked)).toBe(folded);
    // It does not come back for the same device.
    expect(observeRoster(folded, roster(["me", "a", "b"]), 9_000).added).toEqual([]);
  });

  it("a device learned of only after it was acknowledged elsewhere shows nothing", () => {
    const r = observeRoster(seeded(["me"]), roster(["me", "late"]), 5_000, new Set(["late"]));
    expect(r.added).toEqual([]);
    expect(r.state?.known).toContain("late");
    expect(noticeView(r.state, new Set(["late"])).visible).toBe(false);
  });

  it("an acknowledgement drops a device already pending at the next fold too", () => {
    const flagged = observeRoster(seeded(["me"]), roster(["me", "a"]), 1_000).state;
    const r = observeRoster(flagged, roster(["me", "a"]), 2_000, new Set(["a"]));
    expect(r.state?.pending).toEqual([]);
    expect(r.added).toEqual([]);
  });

  it("the badge is up exactly while something is pending", () => {
    const flagged = observeRoster(seeded(["me"]), roster(["me", "a"]), 1_000).state;
    expect(badgeVisible(noticeView(flagged))).toBe(true);
    expect(badgeVisible(noticeView(flagged, new Set(["a"])))).toBe(false);
    expect(badgeVisible(noticeView(null))).toBe(false);
  });
});

describe("opening Settings > Devices", () => {
  it("with something pending: every pending key is acknowledged and the visit keeps the snapshot", () => {
    const flagged = observeRoster(seeded(["me"]), roster(["me", "a", "b"]), 1_000).state;
    const begun = beginDevicesVisit(noticeView(flagged));
    expect(begun?.toAcknowledge).toEqual(["a", "b"]);
    expect(begun?.visit).toMatchObject({
      visible: true,
      titleKey: "devices.added_notice.title_many",
      newKeys: ["a", "b"],
    });
    // After the acknowledgement nothing is pending, the snapshot is kept.
    expect(noticeView(flagged, new Set(begun?.toAcknowledge)).visible).toBe(false);
  });

  it("with nothing pending: nothing to acknowledge, no snapshot", () => {
    expect(beginDevicesVisit(noticeView(seeded(["me"])))).toBeNull();
  });

  it("a device pending while the screen is open is added to the visit", () => {
    const first = beginDevicesVisit(
      noticeView(observeRoster(seeded(["me"]), roster(["me", "a"]), 1_000).state),
    );
    const later = noticeView(observeRoster(seeded(["me"]), roster(["me", "b"]), 4_000).state);
    const merged = extendVisit(first?.visit ?? null, later);
    expect(merged.newKeys).toEqual(["a", "b"]);
    expect(merged.latestAtMs).toBe(4_000);
    expect(merged.titleKey).toBe("devices.added_notice.title_many");
  });
});

describe("notificationAction", () => {
  it("posts when devices became pending and notifications are allowed, in front or not", () => {
    expect(
      notificationAction({ added: 1, pendingBefore: 0, pendingAfter: 1, permission: "granted" }),
    ).toBe("post");
    // A second device replaces the first (same tag, plural title).
    expect(
      notificationAction({ added: 1, pendingBefore: 1, pendingAfter: 2, permission: "granted" }),
    ).toBe("post");
  });

  it("never posts without permission (and never asks)", () => {
    for (const permission of ["denied", "default", "unsupported"] as const) {
      expect(notificationAction({ added: 1, pendingBefore: 0, pendingAfter: 1, permission })).toBe("none");
    }
  });

  it("withdraws when the pending set becomes empty, whatever acknowledged it", () => {
    for (const permission of ["granted", "denied"] as const) {
      expect(notificationAction({ added: 0, pendingBefore: 2, pendingAfter: 0, permission })).toBe(
        "withdraw",
      );
    }
    expect(
      notificationAction({ added: 0, pendingBefore: 0, pendingAfter: 0, permission: "granted" }),
    ).toBe("none");
    // A fold that adds nothing new posts nothing again.
    expect(
      notificationAction({ added: 0, pendingBefore: 1, pendingAfter: 1, permission: "granted" }),
    ).toBe("none");
  });

  it("uses one identifier per account", () => {
    expect(noticeTag("acc")).toBe(noticeTag("acc"));
    expect(noticeTag("acc")).not.toBe(noticeTag("other"));
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

describe("recordRoster inside an effect", () => {
  it("runs the effect a bounded number of times (it once re-ran itself without end)", async () => {
    const { createEffect, createRoot, createSignal } = await import("solid-js");
    const { bindAddedNotice, recordRoster } = await import("./added-notice");
    const [rosterSignal, setRoster] = createSignal(roster(["me", "old"]));
    let runs = 0;
    const dispose = createRoot((done) => {
      createEffect(() => bindAddedNotice("acc-loop"));
      createEffect(() => {
        runs += 1;
        if (runs > 50) throw new Error("effect re-ran without end");
        recordRoster("acc-loop", rosterSignal(), 1);
      });
      return done;
    });
    setRoster(roster(["me", "old", "new"]));
    dispose();
    expect(runs).toBeGreaterThanOrEqual(2);
    expect(runs).toBeLessThanOrEqual(3);
  });
});

describe("the live state of the signed-in account", () => {
  it("an acknowledgement from another device clears what is pending; a visit keeps its highlight", async () => {
    const {
      addedNotice,
      bindAddedNotice,
      devicesVisit,
      enterDevicesVisit,
      highlightedDeviceKeys,
      leaveDevicesVisit,
      recordAcknowledged,
      recordRoster,
    } = await import("./added-notice");
    bindAddedNotice("acc-live");
    recordRoster("acc-live", roster(["me"]), 1);
    recordRoster("acc-live", roster(["me", "a", "b"]), 2);
    expect(addedNotice().newKeys).toEqual(["a", "b"]);

    // Received from elsewhere: "a" stops being pending here.
    recordAcknowledged("acc-live", ["a"]);
    expect(addedNotice().newKeys).toEqual(["b"]);
    expect(highlightedDeviceKeys().has("a")).toBe(false);

    // Opening Devices: "b" is to be acknowledged, and stays highlighted for
    // the visit after the acknowledgement lands.
    expect(enterDevicesVisit()).toEqual(["b"]);
    recordAcknowledged("acc-live", ["a", "b"]);
    expect(addedNotice().visible).toBe(false);
    expect(devicesVisit()?.newKeys).toEqual(["b"]);
    expect(highlightedDeviceKeys().has("b")).toBe(true);
    // Opening again with nothing pending acknowledges nothing.
    expect(enterDevicesVisit()).toEqual([]);

    leaveDevicesVisit();
    expect(devicesVisit()).toBeNull();
    expect(highlightedDeviceKeys().size).toBe(0);
    bindAddedNotice(null);
  });
});
