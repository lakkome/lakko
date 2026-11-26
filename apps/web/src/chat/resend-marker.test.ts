// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0073 R.7 case 1 - `resend-marker.ts`. */
import { describe, expect, it } from "vitest";

import { heldRowAuthor, resendVerdict, resentOriginIds } from "./resend-marker";

const marker = { groupIdB64: "old-group", messageId: 7 };
const match = {
  marker,
  receivedConversation: "conv",
  originConversation: "conv",
  originIsOlderSegment: true,
  heldAuthor: "founder",
  senderAuthor: "founder",
};

describe("resendVerdict", () => {
  it("match: the original is held in an older segment by the same author, so the successor row is not drawn", () => {
    expect(resendVerdict(match)).toBe("skip");
  });
  it("no match: the named row is not held, so the message renders normally", () => {
    expect(resendVerdict({ ...match, heldAuthor: null })).toBe("draw");
  });
  it("other author: a marker naming somebody else's row hides nothing", () => {
    expect(resendVerdict({ ...match, senderAuthor: "someone-else" })).toBe("draw");
    expect(resendVerdict({ ...match, senderAuthor: null })).toBe("draw");
  });
  it("other conversation: a marker naming a row of another conversation hides nothing", () => {
    expect(resendVerdict({ ...match, originConversation: "another" })).toBe("draw");
    expect(resendVerdict({ ...match, originConversation: null })).toBe("draw");
  });
  it("a marker naming the receiving group itself or a later one hides nothing", () => {
    expect(resendVerdict({ ...match, originIsOlderSegment: false })).toBe("draw");
  });
  it("no marker is an ordinary message", () => {
    expect(resendVerdict({ ...match, marker: undefined })).toBe("draw");
  });
});

describe("resendVerdict, a returned history's copy (ADR-0101 walk D3)", () => {
  const reset = {
    marker,
    receivedConversation: "successor",
    originConversation: null,
    originIsOlderSegment: false,
    heldAuthor: null,
    senderAuthor: "peer",
  };
  it("the receiving group holds the copy by the same author: not drawn twice", () => {
    expect(resendVerdict({ ...reset, heldInReceivedAuthor: "peer" })).toBe("skip");
  });
  it("a copy by somebody else, or none, hides nothing", () => {
    expect(resendVerdict({ ...reset, heldInReceivedAuthor: "other" })).toBe("draw");
    expect(resendVerdict({ ...reset, heldInReceivedAuthor: null })).toBe("draw");
    expect(resendVerdict(reset)).toBe("draw");
    expect(resendVerdict({ ...reset, heldInReceivedAuthor: "peer", senderAuthor: null })).toBe("draw");
  });
});

describe("resentOriginIds (ADR-0101 walk D3)", () => {
  it("collects the original ids the held re-sends name", () => {
    expect(
      resentOriginIds([
        { resendOf: { messageId: "41" } },
        {},
        { resendOf: { messageId: "x" } },
        { resendOf: { messageId: "7" } },
      ]),
    ).toEqual(new Set(["41", "7"]));
  });
});

describe("heldRowAuthor", () => {
  const same = (k: string): string => k;
  it("a row this device sent is by its own account", () => {
    expect(heldRowAuthor({ direction: "outgoing" }, "own", same)).toBe("own");
    expect(heldRowAuthor({ direction: "outgoing" }, null, same)).toBeNull();
  });
  it("a received row is by its stored author, through the aliases", () => {
    expect(heldRowAuthor({ direction: "incoming", authorKeyB64: "old" }, "own", () => "row")).toBe("row");
  });
  it("a system row, a missing row and a row with no author have none", () => {
    expect(heldRowAuthor({ direction: "system" }, "own", same)).toBeNull();
    expect(heldRowAuthor(undefined, "own", same)).toBeNull();
    expect(heldRowAuthor({ direction: "incoming", authorKeyB64: null }, "own", same)).toBeNull();
  });
});
