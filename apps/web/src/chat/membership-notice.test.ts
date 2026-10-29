// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { t } from "../l10n";
import { initialsOf } from "../ui/Avatar";
import { memberListLabelOf, memberNameOf } from "../profile/member-label";
import type { ResolvedName } from "../profile/resolver";
import { pendingJoinLabel, pendingJoinName } from "./pending-join-label";
import {
  membershipNoticeAfterAttempt,
  membershipNoticeLanded,
  type MembershipAction,
} from "./membership-notice";

const OWNER_ACTIONS: MembershipAction[] = ["add", "remove", "set-admin", "transfer-ownership"];

describe("the notice after a membership attempt", () => {
  it("shows nothing for an owner or admin action, in effect or not", () => {
    // Amendment of 2026-10-29: the member row's annotation is the only sign.
    for (const action of OWNER_ACTIONS) {
      expect(membershipNoticeAfterAttempt(action, true)).toBeNull();
      expect(membershipNoticeAfterAttempt(action, false)).toBeNull();
    }
  });

  it("shows nothing for a leave already in effect, whoever committed it", () => {
    // A background sweep committed the departure before the screen's own
    // sweep looked: the group reads it as done, so no "requested" notice.
    expect(membershipNoticeAfterAttempt("leave", true)).toBeNull();
  });

  it("shows the request notice for a leave still waiting", () => {
    const key = membershipNoticeAfterAttempt("leave", false);
    expect(key).toBe("chat.leave.requested");
    expect(t(key as string).length).toBeGreaterThan(0);
  });

  it("clears the leave notice the moment the departure lands, not before", () => {
    expect(membershipNoticeLanded("leave", false)).toBe(false);
    expect(membershipNoticeLanded("leave", true)).toBe(true);
  });

  it("never waits on an owner or admin action", () => {
    for (const action of OWNER_ACTIONS) expect(membershipNoticeLanded(action, true)).toBe(false);
  });
});

describe("avatar initials come from the name, never the list label", () => {
  const vg2: ResolvedName = {
    name: "VG2",
    source: "profile",
    username: "vg2",
    listLabel: "VG2 · @vg2",
    theirName: "VG2",
    fingerprint: "a1b2 c3d4",
    usernameNotVerified: false,
  } as unknown as ResolvedName;

  it("a member row with a username", () => {
    expect(memberListLabelOf(vg2)).toBe("VG2 · @vg2");
    expect(initialsOf(memberNameOf(vg2))).toBe("VG");
    // The defect seen live: initials taken from the label.
    expect(initialsOf(memberListLabelOf(vg2))).toBe("V·");
  });

  it("a pending join request with a username", () => {
    const group = { pendingExternalJoinAccountKey: () => new Uint8Array(32).fill(2) };
    const leaf = new Uint8Array(32).fill(1);
    const b64 = (b: Uint8Array): string => `k${b[0]}`;
    const resolve = (): ResolvedName => vg2;
    expect(pendingJoinLabel(group, leaf, b64, resolve)).toBe("VG2 · @vg2");
    expect(initialsOf(pendingJoinName(group, leaf, b64, resolve))).toBe("VG");
  });
});
