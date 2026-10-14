// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The receive side's counters (`sframe-receive-ledger.ts`): what a frame that
 * did not open is, relative to a frame of the same picture that did.
 *
 * The frames here are the patterns a live call can produce on a track whose
 * packets are sent twice; which of them it does produce is what the counters
 * are for, and is not asserted here.
 */
import { describe, expect, it } from "vitest";

import {
  MAX_TALLY_KEYS,
  ReceiveLedger,
  REMEMBERED_BYTES,
  REMEMBERED_FRAMES,
  reasonKey,
  receivedFrameInfo,
  relationToOpened,
} from "./sframe-receive-ledger";

/** `n` distinct-looking bytes. */
function bytes(n: number, seed = 1): Uint8Array {
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i += 1) out[i] = (i * 31 + seed * 17 + 7) & 0xff;
  return out;
}

const at = (rtpTimestamp: number, payloadType = 96) => ({ rtpTimestamp, payloadType });

describe("relationToOpened", () => {
  const opened = bytes(600);
  const head = opened.slice(0, REMEMBERED_BYTES);

  it("names a copy that lost its tail by how much", () => {
    expect(relationToOpened(opened.slice(0, 597), head, 600)).toBe("cut3");
    expect(relationToOpened(opened.slice(0, 100), head, 600)).toBe("cut500");
  });

  it("names a copy with more bytes at its end by how many", () => {
    const longer = new Uint8Array(602);
    longer.set(opened);
    expect(relationToOpened(longer, head, 600)).toBe("extra2");
  });

  it("names an identical copy", () => {
    expect(relationToOpened(opened.slice(), head, 600)).toBe("identical");
  });

  it("names the same bytes moved by a few", () => {
    const later = new Uint8Array(601);
    later.set(opened, 1);
    later[0] = 0xee;
    expect(relationToOpened(later, head, 600)).toBe("shift+1");
    expect(relationToOpened(opened.slice(2), head, 600)).toBe("shift-2");
  });

  it("says where different bytes start to differ", () => {
    expect(relationToOpened(bytes(600, 9), head, 600)).toBe("diff0");
    const early = opened.slice();
    early[5] = (early[5] ?? 0) ^ 0xff;
    expect(relationToOpened(early, head, 600)).toBe("diff<16");
    const late = opened.slice();
    late[300] = (late[300] ?? 0) ^ 0xff;
    expect(relationToOpened(late, head, 600)).toBe("diff>=16");
    expect(relationToOpened(new Uint8Array(0), head, 600)).toBe("diff0");
  });

  it("compares only the remembered start of a frame larger than that", () => {
    const big = bytes(4000);
    const bigHead = big.slice(0, REMEMBERED_BYTES);
    expect(relationToOpened(big.slice(), bigHead, 4000)).toBe("same-start");
    expect(relationToOpened(big.slice(0, 3999), bigHead, 4000)).toBe("cut1");
    expect(relationToOpened(big.slice(0, 1200), bigHead, 4000)).toBe("cut2800");
  });
});

describe("receivedFrameInfo", () => {
  it("reads the metadata's RTP timestamp and payload type", () => {
    const frame = { timestamp: 7, getMetadata: () => ({ rtpTimestamp: 90_000, payloadType: 96 }) };
    expect(receivedFrameInfo(frame)).toEqual({ rtpTimestamp: 90_000, payloadType: 96 });
  });

  it("falls back to the frame's own timestamp, and says what is absent", () => {
    expect(receivedFrameInfo({ timestamp: 1234, getMetadata: () => ({}) })).toEqual({
      rtpTimestamp: 1234,
      payloadType: undefined,
    });
    expect(receivedFrameInfo({})).toEqual({ rtpTimestamp: undefined, payloadType: undefined });
  });

  it("never throws on a frame that does", () => {
    const frame = {
      get timestamp(): number {
        throw new Error("no");
      },
      getMetadata: (): unknown => {
        throw new Error("no");
      },
    };
    expect(receivedFrameInfo(frame)).toEqual({ rtpTimestamp: undefined, payloadType: undefined });
  });
});

describe("reasonKey", () => {
  it("drops the numbers so one cause is one key", () => {
    expect(reasonKey("frame too short: needed 20, had 17")).toBe(
      reasonKey("frame too short: needed 20, had 9"),
    );
    expect(reasonKey("call crypto error: ring open: Crypto(\"x\")")).toBe('ring open: Crypto("x")');
    expect(reasonKey(undefined)).toBe("unknown");
    expect(reasonKey("x".repeat(200)).length).toBe(48);
  });
});

describe("ReceiveLedger", () => {
  it("says a line at the first frame of each outcome and then every 250th", () => {
    const ledger = new ReceiveLedger();
    const lines: string[] = [];
    for (let i = 0; i < 500; i += 1) {
      const line = ledger.note("opened", bytes(100, i), at(3000 * i), 259n);
      if (line !== null) lines.push(line);
    }
    expect(lines).toHaveLength(3);
    expect(lines[0]).toBe(
      "sframe receive opened kid=259: opened=1 badHeader=0 openFailed=0 frames=1 opened_again=0 pt_opened=[96:1]",
    );
    expect(lines[2]).toContain("opened=500 badHeader=0 openFailed=0 frames=500");
  });

  it("counts a byte-identical second copy that opens as opened again", () => {
    const ledger = new ReceiveLedger();
    const wire = bytes(400);
    ledger.note("opened", wire, at(1000), 3n);
    ledger.note("opened", wire.slice(), at(1000), 3n);
    ledger.note("opened", bytes(400, 2), at(4000), 3n);
    expect(ledger.line("opened", 3n)).toContain("opened=3 badHeader=0 openFailed=0 frames=3 opened_again=1");
  });

  it("tells a second copy that lost its tail from bytes that are no copy", () => {
    const ledger = new ReceiveLedger();
    for (let i = 0; i < 10; i += 1) {
      const wire = bytes(500 + i, i);
      const ts = 3000 * i;
      ledger.note("opened", wire, at(ts), 259n);
      // The same picture again, three bytes short: it does not parse.
      ledger.note("badHeader", wire.slice(0, wire.length - 3), at(ts), undefined, "unknown version 7");
    }
    // Something of a picture never opened, on another payload type.
    ledger.note("badHeader", bytes(40, 99), at(999_999, 127), undefined, "frame too short: needed 20, had 9");
    // And a frame the browser gives no timestamp for.
    ledger.note("openFailed", bytes(300, 5), { rtpTimestamp: undefined, payloadType: undefined }, 259n, "auth");

    const line = ledger.line("badHeader");
    expect(line).toBe(
      "sframe receive badHeader: opened=10 badHeader=11 openFailed=1 frames=22 opened_again=0" +
        " pt_opened=[96:10] dropped_same_ts=10 dropped_new_ts=1 dropped_no_ts=1 rel=[cut3:10]" +
        " dropped_len=40..506 pt_dropped=[96:10 127:1 ?:1]" +
        " reasons=[unknown version #:10 frame too short: needed #, had #:1 auth:1]",
    );
  });

  it("forgets opened frames beyond its memory, and bounds every tally", () => {
    const ledger = new ReceiveLedger();
    for (let i = 0; i <= REMEMBERED_FRAMES; i += 1) ledger.note("opened", bytes(64, i), at(i), 1n);
    // The oldest is gone, the newest is there.
    ledger.note("badHeader", bytes(60, 0), at(0), undefined, "r");
    ledger.note("badHeader", bytes(64, REMEMBERED_FRAMES).slice(0, 60), at(REMEMBERED_FRAMES), undefined, "r");
    expect(ledger.line("badHeader")).toContain("dropped_same_ts=1 dropped_new_ts=1");

    for (let pt = 0; pt < 40; pt += 1) ledger.note("badHeader", bytes(30), at(50_000 + pt, pt), undefined, `reason ${"x".repeat(pt)}`);
    const text = ledger.line("badHeader");
    const ptKeys = /pt_dropped=\[([^\]]*)\]/.exec(text)?.[1]?.split(" ") ?? [];
    expect(ptKeys.length).toBeLessThanOrEqual(MAX_TALLY_KEYS + 1);
    expect(text).toContain("more:");
  });

  it("keeps no more of a frame than the bounded start", () => {
    const ledger = new ReceiveLedger();
    const big = bytes(50_000);
    ledger.note("opened", big, at(1), 1n);
    // A change far beyond the remembered start cannot be seen, and is said so.
    const changed = big.slice();
    changed[40_000] = (changed[40_000] ?? 0) ^ 1;
    ledger.note("openFailed", changed, at(1), 1n, "auth");
    expect(ledger.line("openFailed", 1n)).toContain("rel=[same-start:1]");
  });
});
