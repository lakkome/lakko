// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { oneToOneEndReason } from "./call-session";
import { endOpensMenu, endedForEveryoneLine, endsTheCallForMe, ENDED_FOR_ALL } from "./end-for-everyone";
import { reduceGroupCalls, type GroupCallRecords } from "./group-call-record";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

describe("the reason on the wire", () => {
  // Composed and parsed by the real core: a reason the core does not know
  // reads back as `hungUp`, and the end would then take nobody out.
  it("survives the core's compose and parse", () => {
    const callId = new Uint8Array(16).fill(0x2a);
    const parsed = wasm.parseCallSignaling(wasm.composeCallEndSignaling(callId, ENDED_FOR_ALL));
    try {
      expect(parsed.kind).toBe("end");
      expect(parsed.endReason).toBe(ENDED_FOR_ALL);
    } finally {
      parsed.free();
    }
  });
});

describe("endOpensMenu", () => {
  it("opens the choice for the starter while someone else is in the call", () => {
    expect(endOpensMenu(true, 1)).toBe(true);
    expect(endOpensMenu(true, 5)).toBe(true);
  });

  it("ends at once for the starter alone in the call", () => {
    expect(endOpensMenu(true, 0)).toBe(false);
  });

  it("never opens for anyone else", () => {
    expect(endOpensMenu(false, 3)).toBe(false);
  });
});

describe("endsTheCallForMe", () => {
  const starter = "c3RhcnRlcg==";

  it("takes the device out when the starter ended it for everyone", () => {
    expect(
      endsTheCallForMe({ endReason: ENDED_FOR_ALL, senderPersonKey: starter, starterPersonKey: starter }),
    ).toBe(true);
  });

  it("ignores the same message from anyone else", () => {
    expect(
      endsTheCallForMe({ endReason: ENDED_FOR_ALL, senderPersonKey: "b3RoZXI=", starterPersonKey: starter }),
    ).toBe(false);
  });

  it("ignores an ordinary end, even from the starter", () => {
    expect(
      endsTheCallForMe({ endReason: "hungUp", senderPersonKey: starter, starterPersonKey: starter }),
    ).toBe(false);
  });

  it("does nothing when the sender or the starter is unknown", () => {
    expect(
      endsTheCallForMe({ endReason: ENDED_FOR_ALL, senderPersonKey: undefined, starterPersonKey: starter }),
    ).toBe(false);
    expect(
      endsTheCallForMe({ endReason: ENDED_FOR_ALL, senderPersonKey: "", starterPersonKey: "" }),
    ).toBe(false);
    expect(
      endsTheCallForMe({ endReason: ENDED_FOR_ALL, senderPersonKey: starter, starterPersonKey: null }),
    ).toBe(false);
  });
});

describe("endedForEveryoneLine", () => {
  it("names the starter when the device can", () => {
    expect(endedForEveryoneLine("Anna")).toContain("Anna");
  });

  it("says it without a name otherwise", () => {
    expect(endedForEveryoneLine(null)).not.toContain("{name}");
    expect(endedForEveryoneLine("  ")).toBe(endedForEveryoneLine(null));
  });
});

describe("a one-to-one call", () => {
  it("reads the group reason as a plain hang-up", () => {
    expect(oneToOneEndReason(ENDED_FOR_ALL)).toBe("hungUp");
    expect(oneToOneEndReason("declined")).toBe("declined");
    expect(oneToOneEndReason("something else")).toBe("hungUp");
  });
});

// The receiving side of a chat-group call reads the starter out of the chat's
// own record of the call (`group-call-record.ts`), which is written from the
// `GroupCallStart` row and from nothing else. These pin that a device which
// came to the call late still knows who may end it.
describe("who may end the call, for a device that joined late", () => {
  const starter = "c3RhcnRlcg==";
  const callIdHex = "2a".repeat(16);
  const start = (records: GroupCallRecords, observedAtMs: number): GroupCallRecords =>
    reduceGroupCalls(records, {
      kind: "start",
      callIdHex,
      initiatorLeaf: 0,
      initiatorPersonKey: starter,
      createdAtMs: 1_000,
      mediaAudio: true,
      mediaVideo: true,
      observedAtMs,
    });
  const starterOf = (records: GroupCallRecords): string | null =>
    records.get(callIdHex)?.initiatorPersonKey ?? null;

  it("knows the starter from the start row it read long after the call began", () => {
    let records: GroupCallRecords = new Map();
    // The start row is read on a later poll, after other people's joins.
    records = start(records, 600_000);
    records = reduceGroupCalls(records, {
      kind: "join",
      callIdHex,
      memberKey: "bGF0ZQ==",
      personKey: "bGF0ZQ==",
      observedAtMs: 601_000,
    });
    expect(
      endsTheCallForMe({
        endReason: ENDED_FOR_ALL,
        senderPersonKey: starter,
        starterPersonKey: starterOf(records),
      }),
    ).toBe(true);
  });

  it("keeps the starter when the start row is seen again", () => {
    let records: GroupCallRecords = new Map();
    records = start(records, 2_000);
    const again = reduceGroupCalls(records, {
      kind: "start",
      callIdHex,
      initiatorLeaf: 7,
      initiatorPersonKey: "c29tZW9uZSBlbHNl",
      createdAtMs: 1_000,
      mediaAudio: true,
      mediaVideo: true,
      observedAtMs: 3_000,
    });
    // A second start for the same call names nobody new.
    expect(starterOf(again)).toBe(starter);
  });

  it("still knows the starter after the starter left and came back", () => {
    let records: GroupCallRecords = new Map();
    records = start(records, 2_000);
    records = reduceGroupCalls(records, { kind: "leave", callIdHex, memberKey: starter, observedAtMs: 3_000 });
    records = reduceGroupCalls(records, {
      kind: "join",
      callIdHex,
      memberKey: starter,
      personKey: starter,
      observedAtMs: 4_000,
    });
    expect(
      endsTheCallForMe({
        endReason: ENDED_FOR_ALL,
        senderPersonKey: starter,
        starterPersonKey: starterOf(records),
      }),
    ).toBe(true);
  });

  it("is not taken out by a call it never saw start", () => {
    // No start row was ever read: a join alone opens no record (C-12), so
    // there is no starter to compare with and the end is a plain end.
    const records = reduceGroupCalls(new Map(), {
      kind: "join",
      callIdHex,
      memberKey: "bGF0ZQ==",
      personKey: "bGF0ZQ==",
      observedAtMs: 601_000,
    });
    expect(starterOf(records)).toBeNull();
    expect(
      endsTheCallForMe({
        endReason: ENDED_FOR_ALL,
        senderPersonKey: starter,
        starterPersonKey: starterOf(records),
      }),
    ).toBe(false);
  });
});
