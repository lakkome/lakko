// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 7 - the "who can find me" setting (Visibility) and the
 * reach key version counters (ReachState) carried between the account's
 * devices as DeviceGroup `ServiceEnvelope` bodies. Envelopes are composed,
 * dispatched and ordered by the REAL wasm core; the DS is faked.
 *
 * What is pinned: a newer Visibility is kept, an older or equal one is not;
 * a ReachState is folded by the core's `mergeReachState` and the client
 * stores and sends exactly what it returns; a re-send carries the original
 * stamp; anything else inside a `ServiceEnvelope` is dropped.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";
import type { DsClient } from "../ds/client";
import { EMPTY_REACH_BOOK, adoptReachState, bumpVersion, reachStateEntries } from "./reach-model";
import { ReachSession } from "./reach-service";
import { classifyServiceEnvelope } from "./service-envelope-intake";
import {
  currentServiceEnvelopes,
  flushServiceEnvelopes,
  markServiceEnvelopeDirty,
  registerServiceEnvelopeSender,
  resetServiceEnvelopeOutbox,
  setServiceEnvelopeComposer,
} from "./service-envelope-outbox";
import { loadVisibility, receiveVisibility, visibilitySetting } from "./visibility-store";

const W = (): Wasm => wasm as unknown as Wasm;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

/** A Visibility envelope: the default setting with contacts of contacts
 *  switched as given, under `stamp`. */
function visibilityAt(stamp: bigint, fof: boolean): Uint8Array {
  const base = wasm.defaultVisibilitySetting();
  const moved = wasm.setContactsOfContacts(base, fof);
  const stamped = moved.withUpdatedAtMs(stamp);
  try {
    return wasm.composeVisibilityEnvelope(stamped);
  } finally {
    base.free();
    moved.free();
    stamped.free();
  }
}

/** A ReachState envelope with one `contact` counter, under `stamp`. */
function reachStateAt(stamp: bigint, contactVersion: number): Uint8Array {
  const state = new wasm.ReachState(
    [new wasm.ReachStateEntry(wasm.ReachClass.Contact, new Uint8Array(0), contactVersion)],
    stamp,
  );
  try {
    return wasm.composeReachStateEnvelope(state);
  } finally {
    state.free();
  }
}

function stampOfReachState(bytes: Uint8Array): bigint {
  const s = wasm.decodeReachStateEnvelope(bytes);
  try {
    return s.updatedAtMs;
  } finally {
    s.free();
  }
}

/** A session over a fake DS that accepts every key set. */
async function session(clock: { now: number }): Promise<ReachSession> {
  const seed = crypto.getRandomValues(new Uint8Array(32));
  const accountKey = wasm.reachVerifyingKey(seed);
  const ds = { putMailboxReach: vi.fn(async () => undefined) } as unknown as DsClient;
  return ReachSession.open("YWNjb3VudA==", seed, accountKey, { ds, now: () => clock.now });
}

describe("classifyServiceEnvelope", () => {
  const held = { visibility: 100n };

  it("keeps a newer Visibility", () => {
    expect(classifyServiceEnvelope(W(), visibilityAt(101n, true), held)).toEqual({
      kind: "visibility",
      updatedAtMs: 101n,
    });
  });

  it("refuses an older or an equal Visibility", () => {
    for (const stamp of [99n, 100n]) {
      expect(classifyServiceEnvelope(W(), visibilityAt(stamp, true), held)).toEqual({
        kind: "stale",
        of: "visibility",
      });
    }
  });

  it("hands every ReachState on, whatever its stamp: the core's merge decides", () => {
    for (const stamp of [1n, 100n, 101n]) {
      expect(classifyServiceEnvelope(W(), reachStateAt(stamp, 1), held)).toEqual({
        kind: "reachState",
        updatedAtMs: stamp,
      });
    }
  });

  it("drops every other envelope kind, and bytes that are no envelope", () => {
    const foreign = [
      wasm.composePinEnvelope(7n, true, 500n),
      wasm.composeProfileRequest(),
      new TextEncoder().encode("hello"),
      new Uint8Array(0),
    ];
    for (const bytes of foreign) {
      expect(classifyServiceEnvelope(W(), bytes, held)).toEqual({ kind: "foreign" });
    }
  });
});

describe("the Visibility intake", () => {
  it("keeps a newer setting, refuses an older or equal one, and re-sends it with its stamp", async () => {
    const loaded = await loadVisibility("dmlzaWJpbGl0eQ==");
    const start = loaded.updatedAtMs;
    const newer = visibilityAt(start + 10n, !wasm.allowsContactsOfContacts(loaded));

    expect(await receiveVisibility(newer)).toBe(true);
    expect(visibilitySetting()?.updatedAtMs).toBe(start + 10n);

    expect(await receiveVisibility(newer)).toBe(false);
    expect(await receiveVisibility(visibilityAt(start + 5n, true))).toBe(false);
    expect(visibilitySetting()?.updatedAtMs).toBe(start + 10n);

    // What this device would hand a newly admitted device: the kept setting,
    // byte for byte, its stamp untouched.
    setServiceEnvelopeComposer("reachState", null);
    const handed = await currentServiceEnvelopes();
    expect(handed).toHaveLength(1);
    expect(handed[0]).toEqual(newer);
  });
});

/** Capture what the outbox sends for `sess`'s ReachState. */
function captureSends(sess: ReachSession): { sent: Uint8Array[]; stop: () => void } {
  resetServiceEnvelopeOutbox();
  setServiceEnvelopeComposer("visibility", null);
  setServiceEnvelopeComposer("reachState", async () => sess.reachStateEnvelope());
  const sent: Uint8Array[] = [];
  const unregister = registerServiceEnvelopeSender(async (bytes) => {
    sent.push(bytes);
  });
  return {
    sent,
    stop: () => {
      unregister();
      setServiceEnvelopeComposer("reachState", null);
    },
  };
}

/** What the core's rule returns for this device's state and `incoming`,
 *  composed: the bytes the device must hold afterwards. */
function coreMerge(
  sess: ReachSession,
  incoming: Uint8Array,
  nowMs: number,
): { store: boolean; send: boolean; bytes: Uint8Array } {
  const stored = wasm.decodeReachStateEnvelope(sess.reachStateEnvelope()!);
  const inc = wasm.decodeReachStateEnvelope(incoming);
  const merge = wasm.mergeReachState(stored, inc, BigInt(nowMs));
  const state = merge.state;
  try {
    return { store: merge.store, send: merge.send, bytes: wasm.composeReachStateEnvelope(state) };
  } finally {
    state.free();
    merge.free();
    stored.free();
    inc.free();
  }
}

async function receive(sess: ReachSession, bytes: Uint8Array): Promise<boolean> {
  const state = wasm.decodeReachStateEnvelope(bytes);
  try {
    return await sess.receiveReachState(state);
  } finally {
    state.free();
  }
}

describe("the ReachState intake (the core's mergeReachState)", () => {
  it("adopts a newer state verbatim: its stamp kept, nothing sent back", async () => {
    const clock = { now: 1_000 };
    const a = await session(clock);
    await a.rotateOnContactRemoved();
    const sent = a.reachStateEnvelope()!;
    expect(stampOfReachState(sent)).toBe(1_000n);

    const b = await session(clock);
    clock.now = 50_000; // B's clock is far ahead: the stamp still comes from A.
    const expected = coreMerge(b, sent, clock.now);
    expect(expected).toMatchObject({ store: true, send: false });
    const out = captureSends(b);
    expect(await receive(b, sent)).toBe(true);
    await flushServiceEnvelopes();
    // Held exactly as the core returned it: A's state under A's stamp.
    expect(b.reachStateEnvelope()).toEqual(expected.bytes);
    expect(b.reachStateEnvelope()).toEqual(sent);
    expect(b.currentVersion("contact")).toBe(1);
    expect(out.sent).toEqual([]);
    out.stop();
    a.stop();
    b.stop();
  });

  it("stores nothing for the same state again, or an older one with nothing new", async () => {
    const clock = { now: 1_000 };
    const b = await session(clock);
    await b.rotateOnContactRemoved(); // contact v1, fof v1 @ 1000
    const held = b.reachStateEnvelope()!;
    const out = captureSends(b);
    for (const incoming of [held, reachStateAt(999n, 1)]) {
      expect(coreMerge(b, incoming, clock.now)).toMatchObject({ store: false, send: false });
      expect(await receive(b, incoming)).toBe(false);
      expect(b.reachStateEnvelope()).toEqual(held);
    }
    await flushServiceEnvelopes();
    expect(out.sent).toEqual([]);
    out.stop();
    b.stop();
  });

  it("keeps and sends back the merge when an older state carries a counter this device lacks", async () => {
    const clock = { now: 1_000 };
    const b = await session(clock);
    await b.rotateOnContactRemoved(); // contact v1, fof v1 @ 1000
    clock.now = 1_200;
    const older = reachStateAt(900n, 5); // contact v5 @ 900
    const expected = coreMerge(b, older, clock.now);
    expect(expected).toMatchObject({ store: true, send: true });
    const out = captureSends(b);
    expect(await receive(b, older)).toBe(true);
    await flushServiceEnvelopes();
    expect(b.reachStateEnvelope()).toEqual(expected.bytes);
    expect(b.currentVersion("contact")).toBe(5);
    // Sent as returned: the core's stamp, strictly after both sides.
    expect(out.sent).toEqual([expected.bytes]);
    expect(stampOfReachState(expected.bytes)).toBeGreaterThan(1_000n);
    out.stop();
    b.stop();
  });

  it("keeps and sends back a union when each side held a counter the other lacked", async () => {
    const clock = { now: 2_000 };
    const b = await session(clock);
    await b.rotateOnShrink(new Uint8Array(32).fill(9)); // group + member v1 @ 2000
    clock.now = 2_500;
    const incoming = reachStateAt(3_000n, 1); // contact v1 @ 3000
    const expected = coreMerge(b, incoming, clock.now);
    expect(expected).toMatchObject({ store: true, send: true });
    const out = captureSends(b);
    expect(await receive(b, incoming)).toBe(true);
    await flushServiceEnvelopes();
    expect(b.reachStateEnvelope()).toEqual(expected.bytes);
    expect(b.currentVersion("contact")).toBe(1);
    expect(b.currentVersion("group", new Uint8Array(32).fill(9))).toBe(1);
    expect(out.sent).toEqual([expected.bytes]);
    expect(stampOfReachState(expected.bytes)).toBeGreaterThan(3_000n);
    out.stop();
    b.stop();
  });
});

describe("adoptReachState", () => {
  it("sets each carried counter and the stamp exactly, and leaves the rest", () => {
    const book = bumpVersion(bumpVersion(EMPTY_REACH_BOOK, "contact", ""), "link", "aa");
    const next = adoptReachState(
      book,
      [
        { cls: "contact", scopeHex: "", version: 4 },
        { cls: "fof", scopeHex: "", version: 2 },
        { cls: "group", scopeHex: "", version: 9 }, // a scope that does not fit: skipped
      ],
      77,
    );
    expect(reachStateEntries(next)).toEqual([
      { cls: "contact", scopeHex: "", version: 4 },
      { cls: "link", scopeHex: "aa", version: 1 },
      { cls: "fof", scopeHex: "", version: 2 },
    ]);
    expect(next.stateUpdatedAtMs).toBe(77);
  });
});

describe("the outbox", () => {
  it("sends the current envelope as composed, and keeps a failed kind dirty", async () => {
    resetServiceEnvelopeOutbox();
    const envelope = reachStateAt(42n, 2);
    setServiceEnvelopeComposer("reachState", async () => envelope);
    setServiceEnvelopeComposer("visibility", null);
    const sent: Uint8Array[] = [];
    let fail = true;
    const unregister = registerServiceEnvelopeSender(async (bytes) => {
      if (fail) throw new Error("offline");
      sent.push(bytes);
    });
    markServiceEnvelopeDirty("reachState");
    await flushServiceEnvelopes();
    expect(sent).toHaveLength(0);

    fail = false;
    await flushServiceEnvelopes();
    expect(sent).toEqual([envelope]);
    // Nothing left: a further flush sends nothing.
    await flushServiceEnvelopes();
    expect(sent).toHaveLength(1);
    unregister();
    setServiceEnvelopeComposer("reachState", null);
  });

  it("waits for a sender: a change made before the DeviceGroup loaded goes out when it does", async () => {
    resetServiceEnvelopeOutbox();
    const envelope = visibilityAt(7n, true);
    setServiceEnvelopeComposer("visibility", async () => envelope);
    markServiceEnvelopeDirty("visibility");
    await flushServiceEnvelopes();
    const sent: Uint8Array[] = [];
    const unregister = registerServiceEnvelopeSender(async (bytes) => {
      sent.push(bytes);
    });
    await flushServiceEnvelopes();
    expect(sent).toEqual([envelope]);
    unregister();
  });
});
