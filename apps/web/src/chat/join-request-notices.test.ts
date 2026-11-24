// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  EMPTY_JOIN_NOTICES,
  joinRequestBar,
  joinRequestBody,
  joinRequestNotificationTag,
  joinRequestsPath,
  joinNoticesAfterRead,
  managesJoinRequests,
  onJoinRequestArrived,
  onJoinRequestBarShown,
  pruneJoinNotices,
  rowUnreadWithRequests,
  sameJoinNotices,
} from "./join-request-notices";

const away = { openAndVisible: false, muted: false, notificationsAllowed: true };

describe("the role gate", () => {
  it("is the owner or an admin while the membership lasts", () => {
    expect(managesJoinRequests("owner", false)).toBe(true);
    expect(managesJoinRequests("admin", false)).toBe(true);
    expect(managesJoinRequests("member", false)).toBe(false);
    expect(managesJoinRequests(null, false)).toBe(false);
    expect(managesJoinRequests("owner", true)).toBe(false);
    expect(managesJoinRequests("admin", true)).toBe(false);
  });
});

describe("the bar", () => {
  it("is absent for a member and with nothing waiting", () => {
    expect(joinRequestBar({ manages: false, pendingCount: 3 })).toBeNull();
    expect(joinRequestBar({ manages: true, pendingCount: 0 })).toBeNull();
  });

  it("counts through the plural key, one and many", () => {
    expect(joinRequestBar({ manages: true, pendingCount: 1 })).toEqual({ key: "chat.join_requests.banner", count: 1 });
    expect(joinRequestBar({ manages: true, pendingCount: 5 })).toEqual({ key: "chat.join_requests.banner", count: 5 });
    expect(joinRequestBar({ manages: true, pendingCount: 63 })).toEqual({ key: "chat.join_requests.banner", count: 63 });
  });

  it("reads 64+ once the members panel heading does", () => {
    expect(joinRequestBar({ manages: true, pendingCount: 64 })).toEqual({ key: "chat.join_requests.banner_full" });
    expect(joinRequestBar({ manages: true, pendingCount: 70 })).toEqual({ key: "chat.join_requests.banner_full" });
  });
});

describe("an arriving request", () => {
  it("notifies once and counts as unshown while the conversation is not in front", () => {
    const first = onJoinRequestArrived(EMPTY_JOIN_NOTICES, "A", away);
    expect(first.notify).toBe(true);
    expect(first.sets).toEqual({ announced: ["A"], unshown: ["A"] });
    // The same person asking again (an "already pending" knock).
    const again = onJoinRequestArrived(first.sets, "A", away);
    expect(again.notify).toBe(false);
    expect(again.sets).toBe(first.sets);
  });

  it("does not notify while the conversation is open in front of the person", () => {
    const out = onJoinRequestArrived(EMPTY_JOIN_NOTICES, "A", { ...away, openAndVisible: true });
    expect(out.notify).toBe(false);
    expect(out.sets).toEqual({ announced: ["A"], unshown: [] });
    // Announced: leaving the conversation and a re-ask raise nothing.
    expect(onJoinRequestArrived(out.sets, "A", away).notify).toBe(false);
  });

  it("is only unshown in a muted group or with notifications not allowed", () => {
    const muted = onJoinRequestArrived(EMPTY_JOIN_NOTICES, "A", { ...away, muted: true });
    expect(muted.notify).toBe(false);
    expect(muted.sets.unshown).toEqual(["A"]);
    const off = onJoinRequestArrived(EMPTY_JOIN_NOTICES, "A", { ...away, notificationsAllowed: false });
    expect(off.notify).toBe(false);
    expect(off.sets.unshown).toEqual(["A"]);
  });

  it("keeps each request's own state", () => {
    const a = onJoinRequestArrived(EMPTY_JOIN_NOTICES, "A", away);
    const b = onJoinRequestArrived(a.sets, "B", away);
    expect(b.notify).toBe(true);
    expect(b.sets).toEqual({ announced: ["A", "B"], unshown: ["A", "B"] });
  });
});

describe("the bar drawn in the open conversation", () => {
  it("clears the unshown set and announces what it counts", () => {
    const sets = { announced: ["A"], unshown: ["A"] };
    const shown = onJoinRequestBarShown(sets, ["A", "B"]);
    expect(shown).toEqual({ announced: ["A", "B"], unshown: [] });
    // A later re-ask of B raises nothing.
    expect(onJoinRequestArrived(shown, "B", away).notify).toBe(false);
  });
});

describe("pruning", () => {
  it("drops requests no longer waiting from both sets", () => {
    const sets = { announced: ["A", "B", "C"], unshown: ["B", "C"] };
    expect(pruneJoinNotices(sets, ["C"])).toEqual({ announced: ["C"], unshown: ["C"] });
    expect(pruneJoinNotices(sets, [])).toEqual(EMPTY_JOIN_NOTICES);
  });

  it("lets a request asked again after it left be announced again", () => {
    const sets = pruneJoinNotices({ announced: ["A"], unshown: [] }, []);
    expect(onJoinRequestArrived(sets, "A", away).notify).toBe(true);
  });

  it("compares set pairs", () => {
    expect(sameJoinNotices({ announced: ["A"], unshown: [] }, { announced: ["A"], unshown: [] })).toBe(true);
    expect(sameJoinNotices({ announced: ["A"], unshown: [] }, { announced: ["A"], unshown: ["A"] })).toBe(false);
  });
});

describe("a read of the group", () => {
  const sets = { announced: ["A", "B"], unshown: ["A"] };

  it("clears both sets once this device is demoted to member", () => {
    expect(joinNoticesAfterRead(sets, { manages: false, active: true, pendingKeysB64: ["A", "B"] })).toEqual(
      EMPTY_JOIN_NOTICES,
    );
  });

  it("clears both sets once the membership ended", () => {
    expect(joinNoticesAfterRead(sets, { manages: true, active: false, pendingKeysB64: ["A", "B"] })).toEqual(
      EMPTY_JOIN_NOTICES,
    );
  });

  it("only prunes for an owner or admin, never adds", () => {
    expect(joinNoticesAfterRead(sets, { manages: true, active: true, pendingKeysB64: ["B", "C"] })).toEqual({
      announced: ["B"],
      unshown: [],
    });
  });
});

describe("the chat-list row", () => {
  it("adds each unshown request to the unread count", () => {
    expect(rowUnreadWithRequests(0, 0)).toBe(0);
    expect(rowUnreadWithRequests(0, 2)).toBe(2);
    expect(rowUnreadWithRequests(3, 2)).toBe(5);
    expect(rowUnreadWithRequests(3, -1)).toBe(3);
  });
});

describe("the notification", () => {
  it("names the joiner when there is a name, else no one", () => {
    expect(joinRequestBody("Anna", "Hikers")).toEqual({
      key: "notification.join_request.body",
      params: { name: "Anna", group: "Hikers" },
    });
    expect(joinRequestBody(null, "Hikers")).toEqual({
      key: "notification.join_request.body_unnamed",
      params: { group: "Hikers" },
    });
    expect(joinRequestBody("  ", "Hikers").key).toBe("notification.join_request.body_unnamed");
  });

  it("is tagged per group and request and opens the info at Join requests", () => {
    expect(joinRequestNotificationTag("g", "j")).toBe("join-request:g.j");
    expect(joinRequestNotificationTag("g", "j")).not.toBe(joinRequestNotificationTag("g", "k"));
    expect(joinRequestsPath("/chat/abc")).toBe("/chat/abc?info=group&section=join-requests");
  });
});
