// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0073 R.5 - one conversation over several groups (`lineage.ts`). */
import { describe, expect, it } from "vitest";

import {
  buildLineage,
  compareTimeline,
  decodeReadPosition,
  encodeReadPosition,
  headSegment,
  historyRowId,
  isHistoryRowId,
  laterSegmentExists,
  NO_LINEAGE,
  OLD_SEGMENT_POLL_WINDOW_MS,
  oldSegmentPolled,
  planConversations,
  readUpToInSegment,
  retiresOldSegmentPush,
} from "./lineage";

const ROOT = "aa";
const S1 = "bb";
const S2 = "cc";
const OTHER = "dd";

const LINEAGE = buildLineage([
  { successorHex: S2, rootHex: ROOT, hlc: 20n },
  { successorHex: S1, rootHex: ROOT, hlc: 10n },
]);

describe("buildLineage", () => {
  it("the conversation id is the root; a group with no register is its own", () => {
    expect(LINEAGE.conversationOf(S1)).toBe(ROOT);
    expect(LINEAGE.conversationOf(S2)).toBe(ROOT);
    expect(LINEAGE.conversationOf(ROOT)).toBe(ROOT);
    expect(LINEAGE.conversationOf(OTHER)).toBe(OTHER);
    expect(NO_LINEAGE.conversationOf(S1)).toBe(S1);
  });
  it("segments are the root, then the successors by stamp", () => {
    expect(LINEAGE.segmentsOf(ROOT)).toEqual([ROOT, S1, S2]);
    expect(LINEAGE.segmentsOf(OTHER)).toEqual([OTHER]);
    expect(LINEAGE.segmentIndexOf(S2)).toBe(2);
    expect(LINEAGE.segmentIndexOf(ROOT)).toBe(0);
    expect(LINEAGE.isSuccessor(S1)).toBe(true);
    expect(LINEAGE.isSuccessor(ROOT)).toBe(false);
  });
  it("equal stamps break on the id, so every device agrees", () => {
    const l = buildLineage([
      { successorHex: "zz", rootHex: ROOT, hlc: 5n },
      { successorHex: "mm", rootHex: ROOT, hlc: 5n },
    ]);
    expect(l.segmentsOf(ROOT)).toEqual([ROOT, "mm", "zz"]);
  });
  it("a register that names the parent instead of the root still reads as one conversation", () => {
    const l = buildLineage([
      { successorHex: S1, rootHex: ROOT, hlc: 1n },
      { successorHex: S2, rootHex: S1, hlc: 2n },
    ]);
    expect(l.conversationOf(S2)).toBe(ROOT);
    expect(l.segmentsOf(ROOT)).toEqual([ROOT, S1, S2]);
  });
  it("a register naming itself, and a cycle, are ignored", () => {
    const l = buildLineage([
      { successorHex: ROOT, rootHex: ROOT, hlc: 1n },
      { successorHex: S1, rootHex: S2, hlc: 2n },
      { successorHex: S2, rootHex: S1, hlc: 3n },
    ]);
    expect(l.conversationOf(ROOT)).toBe(ROOT);
    expect(l.conversationOf(S1)).toBe(S1);
    expect(l.conversationOf(S2)).toBe(S2);
    expect(l.segmentsOf(S1)).toEqual([S1]);
  });
});

describe("headSegment (the conversation's current group)", () => {
  const segments = [ROOT, S1, S2];
  it("is the last segment this device is an active member of", () => {
    expect(headSegment(segments, () => "active")).toBe(S2);
    expect(headSegment(segments, (h) => (h === S2 ? "absent" : "active"))).toBe(S1);
  });
  it("a successor the device has not joined is not its head: it sends where it is", () => {
    expect(headSegment(segments, (h) => (h === ROOT ? "active" : "absent"))).toBe(ROOT);
  });
  it("with every membership ended the last held segment keeps the row reachable", () => {
    expect(headSegment(segments, (h) => (h === S2 ? "absent" : "ended"))).toBe(S1);
    expect(headSegment(segments, () => "absent")).toBeNull();
  });
  it("an ended newer segment does not hide an active older one", () => {
    expect(headSegment(segments, (h) => (h === S2 ? "ended" : "active"))).toBe(S1);
  });
});

describe("timeline order and the read position", () => {
  it("orders by (segment, message id): a successor's ids restart", () => {
    expect(compareTimeline({ segment: 0, messageId: 900 }, { segment: 1, messageId: 1 })).toBeLessThan(0);
    expect(compareTimeline({ segment: 1, messageId: 2 }, { segment: 1, messageId: 1 })).toBeGreaterThan(0);
    expect(compareTimeline({ segment: 2, messageId: 5 }, { segment: 2, messageId: 5 })).toBe(0);
  });
  it("a position in the first segment is the 8 bytes every build writes", () => {
    const bytes = encodeReadPosition({ segment: 0, messageId: 370 });
    expect(Array.from(bytes)).toEqual([0, 0, 0, 0, 0, 0, 1, 114]);
    expect(decodeReadPosition(bytes)).toEqual({ segment: 0, messageId: 370 });
  });
  it("a later segment carries its index after the id, and round-trips", () => {
    const bytes = encodeReadPosition({ segment: 2, messageId: 7 });
    expect(bytes.length).toBe(12);
    expect(decodeReadPosition(bytes)).toEqual({ segment: 2, messageId: 7 });
    // A reader of the 8-byte form sees message 7 of the first segment: less
    // read than was read, never more.
    expect(decodeReadPosition(bytes.slice(0, 8))).toEqual({ segment: 0, messageId: 7 });
  });
  it("too short to hold an id is no position", () => {
    expect(decodeReadPosition(new Uint8Array(4))).toBeNull();
  });
  it("means, per segment: nothing before it, the id in it, everything after it", () => {
    const position = { segment: 1, messageId: 12 };
    expect(readUpToInSegment(position, 0)).toBe("all");
    expect(readUpToInSegment(position, 1)).toBe(12);
    expect(readUpToInSegment(position, 2)).toBeNull();
    expect(readUpToInSegment(null, 0)).toBeNull();
  });
  it("an old segment's rows get unique, ordered, negative on-screen ids", () => {
    const a = historyRowId(900, 2);
    const b = historyRowId(1, 1);
    const c = historyRowId(2, 1);
    expect(a).toBeLessThan(b);
    expect(b).toBeLessThan(c);
    expect(c).toBeLessThan(0);
    expect(isHistoryRowId(a)).toBe(true);
    expect(isHistoryRowId(1)).toBe(false);
    expect(isHistoryRowId(undefined)).toBe(false);
  });
});

describe("planConversations (one row per conversation)", () => {
  it("folds a conversation's groups into one row keyed on the root, opening the head", () => {
    const plans = planConversations(
      [
        { groupHex: ROOT, active: true },
        { groupHex: OTHER, active: true },
        { groupHex: S1, active: true },
      ],
      LINEAGE,
    );
    expect(plans).toEqual([
      { conversationHex: ROOT, headHex: S1, heldSegments: [ROOT, S1] },
      { conversationHex: OTHER, headHex: OTHER, heldSegments: [OTHER] },
    ]);
  });
  it("a device that holds only the successor shows it under the conversation id", () => {
    expect(planConversations([{ groupHex: S2, active: true }], LINEAGE)).toEqual([
      { conversationHex: ROOT, headHex: S2, heldSegments: [S2] },
    ]);
  });
  it("without a register the successor is its own conversation (R.5 (c))", () => {
    expect(planConversations([{ groupHex: S2, active: true }], NO_LINEAGE)).toEqual([
      { conversationHex: S2, headHex: S2, heldSegments: [S2] },
    ]);
  });
});

describe("oldSegmentPolled (R.5)", () => {
  const base = { isHead: false, oneToOne: false, movedAtMs: 1000, nowMs: 2000 };
  it("the head is always polled", () => {
    expect(oldSegmentPolled({ ...base, isHead: true, oneToOne: true })).toBe(true);
  });
  it("a one-to-one's old journal is not polled at all after the move", () => {
    expect(oldSegmentPolled({ ...base, oneToOne: true })).toBe(false);
  });
  it("a group's old journal is polled for the retention window", () => {
    expect(oldSegmentPolled(base)).toBe(true);
    expect(oldSegmentPolled({ ...base, nowMs: 1000 + OLD_SEGMENT_POLL_WINDOW_MS })).toBe(false);
    expect(oldSegmentPolled({ ...base, movedAtMs: null })).toBe(true);
  });
});

describe("retiresOldSegmentPush (R.5: the old group's subscription is retired)", () => {
  const base = { groupHex: "old", headHex: "new", headActive: true, alreadyRetired: false };
  it("an older segment is retired once this device is live in the current group", () => {
    expect(retiresOldSegmentPush(base)).toBe(true);
  });
  it("the current group, and a group that is its own conversation, keep theirs", () => {
    expect(retiresOldSegmentPush({ ...base, headHex: "old" })).toBe(false);
    expect(retiresOldSegmentPush({ ...base, headHex: null })).toBe(false);
  });
  it("a device not live in the successor keeps the old subscription", () => {
    expect(retiresOldSegmentPush({ ...base, headActive: false })).toBe(false);
  });
  it("is done once", () => {
    expect(retiresOldSegmentPush({ ...base, alreadyRetired: true })).toBe(false);
  });
});

describe("laterSegmentExists (R.5 / R.8: the conversation already moved)", () => {
  const lineage = buildLineage([
    { successorHex: "b", rootHex: "a", hlc: 1n },
    { successorHex: "c", rootHex: "a", hlc: 2n },
  ]);
  it("every segment but the last has a later one", () => {
    expect(laterSegmentExists(lineage, "a")).toBe(true);
    expect(laterSegmentExists(lineage, "b")).toBe(true);
    expect(laterSegmentExists(lineage, "c")).toBe(false);
  });
  it("a group no register names is its own, unmoved, conversation", () => {
    expect(laterSegmentExists(lineage, "z")).toBe(false);
    expect(laterSegmentExists(NO_LINEAGE, "a")).toBe(false);
  });
});
