// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  clearRestoreMarker,
  isRestoreMarked,
  markRestoreFinished,
  restoredListState,
} from "./restored-marker";

describe("the restored marker", () => {
  it("is set only when the backup named a conversation", () => {
    const s = new MemoryStorage();
    markRestoreFinished("a", 0, s);
    expect(isRestoreMarked("a", s)).toBe(false);
    markRestoreFinished("a", 2, s);
    expect(isRestoreMarked("a", s)).toBe(true);
  });

  it("is per account and goes for good", () => {
    const s = new MemoryStorage();
    markRestoreFinished("a", 1, s);
    expect(isRestoreMarked("b", s)).toBe(false);
    clearRestoreMarker("a", s);
    expect(isRestoreMarked("a", s)).toBe(false);
  });
});

describe("restoredListState (identity-recovery 7.2)", () => {
  const base = { marked: true, conversationCount: 0, otherDeviceSignedIn: false as boolean | null };

  it("is the card on an empty list with no other device", () => {
    expect(restoredListState(base)).toBe("card");
  });

  it("is the banner while another device is signed in", () => {
    expect(restoredListState({ ...base, otherDeviceSignedIn: true })).toBe("banner");
  });

  it("claims nothing until it is known whether another device is signed in", () => {
    expect(restoredListState({ ...base, otherDeviceSignedIn: null })).toBe("none");
  });

  it("is nothing once a conversation arrived, and without a marker", () => {
    expect(restoredListState({ ...base, conversationCount: 1 })).toBe("none");
    expect(restoredListState({ ...base, marked: false })).toBe("none");
  });
});
