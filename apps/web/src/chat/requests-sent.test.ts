// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { joinPageView } from "../routes/join-group-wait";
import {
  ASK_AGAIN_AFTER_MS,
  mayAskAgain,
  sentRequestState,
  sentRequestsAfter,
  type SentRequest,
} from "./requests-sent";

const NOW = 1_800_000_000_000;
const DAY = 24 * 60 * 60 * 1000;

function sent(sentAtMs: number): SentRequest {
  return { idB64: "AQ==", name: "Berlin", handle: "", sentAtMs, avatar: null, knockKeyB64: "", knockVersion: 0 };
}

describe("Requests sent (UX 6.2, a day since 2026-11-12)", () => {
  it("under a day: Waiting for an admin, and the device may not ask again", () => {
    expect(ASK_AGAIN_AFTER_MS).toBe(DAY);
    expect(sentRequestState(sent(NOW - DAY + 1), NOW)).toBe("waiting");
    expect(mayAskAgain(sent(NOW - DAY + 1), NOW)).toBe(false);
  });

  it("a day or more: No answer yet, and Ask again is offered", () => {
    expect(sentRequestState(sent(NOW - DAY), NOW)).toBe("no-answer");
    expect(mayAskAgain(sent(NOW - DAY), NOW)).toBe(true);
  });

  it("no request yet: the device may ask", () => {
    expect(mayAskAgain(undefined, NOW)).toBe(true);
  });
});

// Seen live 2026-11-23: a member left a group and opened its invite link
// again; the page showed "Request sent." and nothing reached the admin,
// because the row of the request that had led to the group outlived the
// membership. The row now goes when the group lands and when the membership
// ends.
describe("Requests sent rows are forgotten at the join and at the end", () => {
  const row = (idB64: string, sentAtMs: number): SentRequest => ({ ...sent(sentAtMs), idB64 });

  it("the old row made the page wait without a request", () => {
    expect(joinPageView({ phase: "idle", sentAtMs: 1_000, nowMs: 2_000 })).toBe("waiting");
  });

  it("after the join and the leave the page offers Request to join", () => {
    let list = [row("AQ==", 1_000), row("Ag==", 1_500)];
    list = sentRequestsAfter(list, "AQ==", 2_000); // the group lands
    list = sentRequestsAfter(list, "AQ==", 3_000); // the person leaves
    const at = list.find((r) => r.idB64 === "AQ==")?.sentAtMs ?? null;
    expect(joinPageView({ phase: "idle", sentAtMs: at, nowMs: 4_000 })).toBe("request");
    expect(list).toEqual([row("Ag==", 1_500)]);
  });

  it("a request sent after the cutoff stays", () => {
    const list = [row("AQ==", 5_000)];
    expect(sentRequestsAfter(list, "AQ==", 4_000)).toEqual(list);
  });
});
