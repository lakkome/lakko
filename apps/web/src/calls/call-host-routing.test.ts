// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { decideInboundSignaling, type InboundSignalingInput } from "./call-host-routing";
import { routeSignalingKind, STALE_OFFER_MS } from "./call-signaling-dispatch";

const NOW = Date.parse("2026-09-13T12:00:00.000Z");
const FRESH = new Date(NOW - 2_000).toISOString();
const OLD = new Date(NOW - STALE_OFFER_MS - 1_000).toISOString();

function input(over: Partial<InboundSignalingInput> & { kind: string }): InboundSignalingInput {
  return {
    route: routeSignalingKind(over.kind),
    callIdHex: "aa",
    groupIdB64: "group-a",
    rowCreatedAt: FRESH,
    nowMs: NOW,
    active: null,
    endedCallIds: new Set(),
    groupCallBusy: false,
    groupRecordKnowsCall: false,
    ...over,
  };
}

describe("call host routing", () => {
  it("rings for a fresh offer from ANY joined group when the slot is free", () => {
    // Not only the chat that happens to be open: the host is fed by every
    // group's receive path, and each is an equal source of an offer.
    for (const groupIdB64 of ["group-a", "group-b", "group-z"]) {
      expect(decideInboundSignaling(input({ kind: "offer", groupIdB64 })).oneToOne).toBe("ring");
    }
  });

  it("answers busy to a second offer while a call is in the slot", () => {
    const active = { callIdHex: "aa", groupIdB64: "group-a" };
    const d = decideInboundSignaling(
      input({ kind: "offer", callIdHex: "bb", groupIdB64: "group-b", active }),
    );
    expect(d.oneToOne).toBe("busy");
  });

  it("answers busy while a group call is starting or live", () => {
    const d = decideInboundSignaling(input({ kind: "offer", groupCallBusy: true }));
    expect(d.oneToOne).toBe("busy");
  });

  it("does not ring, and does not answer busy, for a stale offer", () => {
    expect(decideInboundSignaling(input({ kind: "offer", rowCreatedAt: OLD })).oneToOne).toBe(
      "drop",
    );
    const active = { callIdHex: "cc", groupIdB64: "group-a" };
    expect(
      decideInboundSignaling(input({ kind: "offer", rowCreatedAt: OLD, active })).oneToOne,
    ).toBe("drop");
    // An unreadable timestamp is stale too.
    expect(
      decideInboundSignaling(input({ kind: "offer", rowCreatedAt: "not a date" })).oneToOne,
    ).toBe("drop");
  });

  it("never rings again for a call that ended here", () => {
    const d = decideInboundSignaling(input({ kind: "offer", endedCallIds: new Set(["aa"]) }));
    expect(d.oneToOne).toBe("drop");
  });

  it("hands the active call its own answer, ICE and renegotiation offer", () => {
    const active = { callIdHex: "aa", groupIdB64: "group-a" };
    for (const kind of ["answer", "iceCandidate", "offer", "end"]) {
      expect(decideInboundSignaling(input({ kind, active })).oneToOne).toBe("apply");
    }
  });

  it("drops the active call id when it arrives on a different group's journal (C-12)", () => {
    const active = { callIdHex: "aa", groupIdB64: "group-a" };
    for (const kind of ["answer", "iceCandidate", "end"]) {
      expect(
        decideInboundSignaling(input({ kind, active, groupIdB64: "group-b" })).oneToOne,
      ).toBe("drop");
    }
  });

  it("drops answers / ICE / end for a call the slot does not hold", () => {
    for (const kind of ["answer", "iceCandidate", "end"]) {
      expect(decideInboundSignaling(input({ kind })).oneToOne).toBe("drop");
    }
  });

  it("never routes a group-control body to the 1x1 session (C-11)", () => {
    const active = { callIdHex: "aa", groupIdB64: "group-a" };
    for (const kind of ["groupStart", "groupJoin", "groupLeave"]) {
      for (const a of [null, active]) {
        const d = decideInboundSignaling(input({ kind, active: a }));
        expect(d.oneToOne).toBe("drop");
        expect(d.groupRecord).toBe("apply-control");
      }
    }
  });

  it("ends a group-call record only for a call id the record knows", () => {
    expect(decideInboundSignaling(input({ kind: "end", groupRecordKnowsCall: true })).groupRecord).toBe(
      "end",
    );
    expect(decideInboundSignaling(input({ kind: "end" })).groupRecord).toBe("none");
  });

  it("drops an unknown (newer-schema) kind everywhere", () => {
    const d = decideInboundSignaling(input({ kind: "somethingNew" }));
    expect(d).toEqual({ groupRecord: "none", oneToOne: "drop" });
  });
});
