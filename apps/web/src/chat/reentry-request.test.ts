// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 section 3 - the request to come back: due by the core's rule,
 * routed by the core's rule, and recorded durably BEFORE it is sent. The
 * envelope and the knock row are the real core's.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { bindContactsAccount } from "../contacts/local-store";
import { toWasmBehindState, type BehindStateView, type BehindVerdictWasm } from "./conversation-behind";
import {
  ownDeviceInConversation,
  otherPerson,
  requestReentryIfDue,
  resetReentryRequestMemory,
  SEND_RETRY_MS,
  type ReentryRequestPorts,
} from "./reentry-request";
import { endReentryEpisode, REENTRY_KEY_PACKAGES_KEPT, reentryRecordFor } from "./reentry-store";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const SUSPECTED: BehindStateView = { kind: "suspected", sinceMs: 1_000, stopsActing: true };
const IN_STEP: BehindStateView = { kind: "in-step", stopsActing: false };
const GROUP_ID = new Uint8Array(32).fill(5);
const GID = btoa(String.fromCharCode(...GROUP_ID));
const OWN = btoa(String.fromCharCode(...new Uint8Array(32).fill(1)));
const OWN_LEAF = new Uint8Array(32).fill(2);
const OTHER_LEAF_OWN_ACCOUNT = btoa(String.fromCharCode(...new Uint8Array(32).fill(3)));
const PEER = btoa(String.fromCharCode(...new Uint8Array(32).fill(9)));

let now = 10_000_000;
let data: Map<string, string>;
let minted: number;
let forgotten: Uint8Array[];
const events: string[] = [];

function identity(): WasmIdentity {
  return {
    signaturePublicKey: OWN_LEAF,
    createReentryKeyPackage: (_g: Uint8Array, epoch: bigint) => {
      minted += 1;
      events.push("mint");
      return new Uint8Array([minted, Number(epoch)]);
    },
    forgetKeyPackage: (kp: Uint8Array) => void forgotten.push(kp),
  } as unknown as WasmIdentity;
}

const group = { groupId: GROUP_ID } as unknown as WasmGroup;

function ports(
  options: Partial<Omit<ReentryRequestPorts, "roster">> & {
    roster?: { leafKeyB64: string; personKeyB64: string }[];
    kind?: "chat" | "group";
  } = {},
) {
  const { roster: rosterRows, kind: groupKind, ...over } = options;
  const sent: { route: string; bytes: Uint8Array; to?: Uint8Array }[] = [];
  const p: ReentryRequestPorts = {
    now: () => now,
    identity,
    ownPersonKeyB64: () => OWN,
    roster: () =>
      rosterRows ?? [
        { leafKeyB64: btoa(String.fromCharCode(...OWN_LEAF)), personKeyB64: OWN },
        { leafKeyB64: PEER, personKeyB64: PEER },
      ],
    kindOf: () => groupKind ?? "chat",
    due: (state, last, lastWasOwn, n) => {
      const s = toWasmBehindState(wasm as unknown as BehindVerdictWasm, state);
      try {
        return wasm.reentryRequestDue(s, last === undefined ? undefined : BigInt(last), lastWasOwn, BigInt(n));
      } finally {
        s.free();
      }
    },
    route: (own, kind, ownUnanswered) => {
      const r = wasm.reentryRoute(own, kind === "chat" ? wasm.GroupKind.OneToOne : wasm.GroupKind.Group, ownUnanswered);
      return r === wasm.ReentryRoute.OwnDevice ? "own-device" : r === wasm.ReentryRoute.Mailbox ? "mailbox" : "knock";
    },
    ownDeviceReachable: () => true,
    knockKey: async () => null,
    persistDurable: async () => void events.push("persist"),
    composeEnvelope: (g, e, gap, kp) => {
      events.push(`compose:${reentryRecordFor(GID) === undefined ? "unrecorded" : "recorded"}`);
      return new Uint8Array([...g.slice(0, 1), Number(e), Number(gap % 256n), ...kp]);
    },
    sealKnockRow: (pk, v, env) => wasm.sealReentryKnockRow(pk, v, env),
    sendOwnDevice: async (b) => void sent.push({ route: "own-device", bytes: b }),
    sendMailbox: async (to, b) => void sent.push({ route: "mailbox", bytes: b, to }),
    sendKnock: async (_g, b) => void sent.push({ route: "knock", bytes: b }),
    probeEpoch: async () => 7n,
    ...over,
  };
  return { p, sent };
}

beforeEach(() => {
  vi.spyOn(console, "info").mockImplementation(() => {});
  vi.spyOn(console, "warn").mockImplementation(() => {});
  data = new Map();
  (globalThis as unknown as { localStorage: Storage }).localStorage = {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (k: string) => data.get(k) ?? null,
    key: (i: number) => [...data.keys()][i] ?? null,
    removeItem: (k: string) => void data.delete(k),
    setItem: (k: string, v: string) => void data.set(k, String(v)),
  };
  bindContactsAccount("cmVxdWVzdC1hY2NvdW50");
  now = 10_000_000;
  minted = 0;
  forgotten = [];
  events.length = 0;
  resetReentryRequestMemory();
});

afterEach(() => {
  bindContactsAccount(null);
  vi.restoreAllMocks();
});

describe("the roster helpers", () => {
  it("finds another own device, and the other person of a one-to-one", () => {
    const roster = [
      { leafKeyB64: "own-leaf", personKeyB64: "own" },
      { leafKeyB64: "own-other", personKeyB64: "own" },
      { leafKeyB64: "peer-leaf", personKeyB64: "peer" },
    ];
    expect(ownDeviceInConversation(roster, "own", "own-leaf")).toBe(true);
    expect(ownDeviceInConversation(roster.slice(0, 1).concat(roster.slice(2)), "own", "own-leaf")).toBe(false);
    expect(otherPerson(roster, "own")).toBe("peer");
  });
});

describe("the request (section 3)", () => {
  it("is not sent while in step", async () => {
    const { p, sent } = ports();
    expect(await requestReentryIfDue(group, { state: IN_STEP, gapFromMs: 1 }, p)).toBe("not-due");
    expect(sent).toEqual([]);
    expect(minted).toBe(0);
  });

  it("mints, makes durable and records BEFORE it sends; one-to-one goes to the other person's mailbox", async () => {
    const { p, sent } = ports();
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1234 }, p)).toBe("sent");
    expect(events).toEqual(["mint", "persist", "compose:recorded"]);
    expect(sent.map((s) => s.route)).toEqual(["mailbox"]);
    expect(btoa(String.fromCharCode(...(sent[0]?.to ?? [])))).toBe(PEER);
    const record = reentryRecordFor(GID);
    expect(record?.epochLeft).toBe("7");
    expect(record?.gapFromMs).toBe(1234);
    expect(record?.lastSentMs).toBe(now);
    expect(record?.route).toBe("mailbox");
  });

  it("goes to another own device first", async () => {
    const { p, sent } = ports({
      roster: [
        { leafKeyB64: btoa(String.fromCharCode(...OWN_LEAF)), personKeyB64: OWN },
        { leafKeyB64: OTHER_LEAF_OWN_ACCOUNT, personKeyB64: OWN },
        { leafKeyB64: PEER, personKeyB64: PEER },
      ],
      kind: "group",
    });
    await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p);
    expect(sent.map((s) => s.route)).toEqual(["own-device"]);
  });

  it("asks its own device once, then the group's knock after 2 minutes, and knock from then on (live walk 5)", async () => {
    const seed = new Uint8Array(32).fill(0x34);
    const pk = wasm.groupKnockPublicKey(seed);
    const { p, sent } = ports({
      roster: [
        { leafKeyB64: btoa(String.fromCharCode(...OWN_LEAF)), personKeyB64: OWN },
        { leafKeyB64: OTHER_LEAF_OWN_ACCOUNT, personKeyB64: OWN },
        { leafKeyB64: PEER, personKeyB64: PEER },
      ],
      kind: "group",
      knockKey: async () => ({ version: 1, publicKeyB64: btoa(String.fromCharCode(...pk)) }),
      composeEnvelope: (g, e, gap, kp) => wasm.composeConversationReentryEnvelope(g, e, gap, kp),
    });
    const wait = Number(wasm.reentryOwnDeviceAnswerWaitMs());
    expect(wait).toBe(2 * 60_000);
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("sent");
    expect(reentryRecordFor(GID)?.ownDeviceAsked).toBe(true);
    now += wait - 1;
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("not-due");
    now += 1;
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("sent");
    // A knock request repeats on the 30-minute interval, still by knock.
    now += 2 * 60_000;
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("not-due");
    now += 30 * 60_000;
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("sent");
    expect(sent.map((s) => s.route)).toEqual(["own-device", "knock", "knock"]);
    expect(reentryRecordFor(GID)?.ownDeviceAsked).toBe(true);
  });

  it("in a one-to-one, the request after an unanswered own device goes to the other person's mailbox", async () => {
    const { p, sent } = ports({
      roster: [
        { leafKeyB64: btoa(String.fromCharCode(...OWN_LEAF)), personKeyB64: OWN },
        { leafKeyB64: OTHER_LEAF_OWN_ACCOUNT, personKeyB64: OWN },
        { leafKeyB64: PEER, personKeyB64: PEER },
      ],
    });
    await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p);
    now += 2 * 60_000;
    await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p);
    expect(sent.map((s) => s.route)).toEqual(["own-device", "mailbox"]);
  });

  it("back in step, the next episode asks its own device first again", async () => {
    const { p, sent } = ports({
      roster: [
        { leafKeyB64: btoa(String.fromCharCode(...OWN_LEAF)), personKeyB64: OWN },
        { leafKeyB64: OTHER_LEAF_OWN_ACCOUNT, personKeyB64: OWN },
        { leafKeyB64: PEER, personKeyB64: PEER },
      ],
    });
    await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p);
    expect(reentryRecordFor(GID)?.ownDeviceAsked).toBe(true);
    endReentryEpisode(GID);
    expect(reentryRecordFor(GID)?.ownDeviceAsked).toBe(false);
    // The KeyPackage stays: a Welcome that answers it can still land.
    expect(reentryRecordFor(GID)?.keyPackagesB64.length).toBe(1);
    now += 2 * 60_000;
    await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p);
    expect(sent.map((s) => s.route)).toEqual(["own-device", "own-device"]);
  });

  it("a failed own-device send makes the own device unavailable: the retry goes by mailbox", async () => {
    let failing = true;
    const { p, sent } = ports({
      roster: [
        { leafKeyB64: btoa(String.fromCharCode(...OWN_LEAF)), personKeyB64: OWN },
        { leafKeyB64: OTHER_LEAF_OWN_ACCOUNT, personKeyB64: OWN },
        { leafKeyB64: PEER, personKeyB64: PEER },
      ],
      sendOwnDevice: async () => {
        if (failing) throw new Error("offline");
      },
    });
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("failed");
    expect(reentryRecordFor(GID)?.ownDeviceAsked).toBe(true);
    failing = false;
    now += SEND_RETRY_MS;
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("sent");
    expect(sent.map((s) => s.route)).toEqual(["mailbox"]);
  });

  it("with no DeviceGroup sync on this device, the first request goes by the chat's own route", async () => {
    const { p, sent } = ports({
      roster: [
        { leafKeyB64: btoa(String.fromCharCode(...OWN_LEAF)), personKeyB64: OWN },
        { leafKeyB64: OTHER_LEAF_OWN_ACCOUNT, personKeyB64: OWN },
        { leafKeyB64: PEER, personKeyB64: PEER },
      ],
      ownDeviceReachable: () => false,
    });
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("sent");
    expect(sent.map((s) => s.route)).toEqual(["mailbox"]);
  });

  it("in a group without a held knock key, mints nothing", async () => {
    const { p, sent } = ports({ kind: "group" });
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("no-route");
    expect(minted).toBe(0);
    expect(sent).toEqual([]);
  });

  it("in a group, seals a knock row a join opener refuses and a re-entry opener reads", async () => {
    const seed = new Uint8Array(32).fill(0x33);
    const pk = wasm.groupKnockPublicKey(seed);
    const { p, sent } = ports({
      kind: "group",
      knockKey: async () => ({ version: 1, publicKeyB64: btoa(String.fromCharCode(...pk)) }),
      composeEnvelope: (g, e, gap, kp) => wasm.composeConversationReentryEnvelope(g, e, gap, kp),
    });
    await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 99 }, p);
    const row = sent[0]?.bytes ?? new Uint8Array();
    expect(() => wasm.openKnockRow(seed, row)).toThrow();
    const opened = wasm.openReentryKnockRow(seed, row);
    expect(opened.epochLeft).toBe(7n);
    expect(opened.gapFromMs).toBe(99n);
    opened.free();
  });

  it("repeats every 30 minutes, keeps the newest two KeyPackages and forgets older ones", async () => {
    const { p, sent } = ports();
    for (let i = 0; i < 3; i++) {
      await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p);
      // Not due again within the interval.
      expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("not-due");
      now += 30 * 60_000;
    }
    expect(sent.length).toBe(3);
    expect(REENTRY_KEY_PACKAGES_KEPT).toBe(wasm.reentryRequestsKept());
    expect(reentryRecordFor(GID)?.keyPackagesB64.length).toBe(2);
    expect(forgotten.length).toBe(1);
  });

  it("names the epoch the journal answers right before every request, never a stored one (live walk 3)", async () => {
    // The walk: a stale epoch 1 stood in the behind record while the owner
    // was at 67; the request must name what the probe answers, each time.
    const answers = [67n, 70n];
    const probed: number[] = [];
    const { p, sent } = ports({
      probeEpoch: async () => {
        probed.push(now);
        return answers.shift() ?? null;
      },
    });
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("sent");
    expect(reentryRecordFor(GID)?.epochLeft).toBe("67");
    // A request that is not due probes nothing (a probe costs a token).
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("not-due");
    expect(probed.length).toBe(1);
    now += 30 * 60_000;
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("sent");
    expect(reentryRecordFor(GID)?.epochLeft).toBe("70");
    expect(probed.length).toBe(2);
    // The envelope carries the probed epoch (the fake compose puts it second).
    expect(sent.map((s) => s.bytes[1])).toEqual([67, 70]);
  });

  it("a probe that names no epoch mints, records and sends nothing, and is retried soon", async () => {
    let answer: bigint | null = null;
    const { p, sent } = ports({ probeEpoch: async () => answer });
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("no-epoch");
    expect(minted).toBe(0);
    expect(reentryRecordFor(GID)).toBeUndefined();
    expect(sent.length).toBe(0);
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("retry-wait");
    now += SEND_RETRY_MS;
    answer = 12n;
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("sent");
    expect(reentryRecordFor(GID)?.epochLeft).toBe("12");
  });

  it("a failed send is retried after a short delay, not 30 minutes", async () => {
    let fail = true;
    const { p } = ports({
      sendMailbox: async () => {
        if (fail) throw new Error("network");
      },
    });
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("failed");
    // The record holds the KeyPackage (a Welcome answering it can be joined)
    // and was never marked sent.
    expect(reentryRecordFor(GID)?.lastSentMs).toBe(0);
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("retry-wait");
    now += SEND_RETRY_MS;
    fail = false;
    expect(await requestReentryIfDue(group, { state: SUSPECTED, gapFromMs: 1 }, p)).toBe("sent");
  });
});
