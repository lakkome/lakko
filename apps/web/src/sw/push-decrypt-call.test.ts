// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The background path CLASSIFIES call signalling instead of suppressing it.
 *
 * ## The defect this closes
 *
 * `public/sw-push-decrypt.js` dropped every call frame — correct for the
 * PREVIEW path, since a prost frame has no text, but it also meant a closed or
 * backgrounded tab never learned a call was happening. `docs/ux/calls.md` §2.1
 * states the requirement and §7 names this filter as the single change that
 * makes it possible.
 *
 * The frames here are composed by the REAL core and parsed by the REAL
 * `parseCallSignaling`; only the MLS group, the IDB reads and the DS are
 * stand-ins. That matters for the same reason it does in
 * `push-decrypt-group-name.test.ts`: a hand-written fixture would keep passing
 * after a wire change that moved the field numbers.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview } from "../../public/sw-push-decrypt.js";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_HEX = "ab".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xab);
const SENDER_KEY = new Uint8Array(32).fill(0x5e);
const SENDER_KEY_B64 = btoa(String.fromCharCode(...SENDER_KEY));

const CALL_ID = new Uint8Array(16).fill(0x11);
const CALL_ID_HEX = "11".repeat(16);
const OTHER_CALL_ID = new Uint8Array(16).fill(0x22);

/** A fixed "now", so a row's age is decided by the fixture and not the clock. */
const NOW = 1_700_000_000_000;

const offer = (video = false): Uint8Array =>
  realWasm.composeCallOfferSignaling(CALL_ID, "v=0", BigInt(NOW), true, video);
const answer = (): Uint8Array => realWasm.composeCallAnswerSignaling(CALL_ID, "v=0");
const candidate = (): Uint8Array =>
  realWasm.composeCallIceCandidateSignaling(CALL_ID, "candidate:1 1 udp", "0", 0);
const end = (id: Uint8Array = CALL_ID): Uint8Array =>
  realWasm.composeCallEndSignaling(id, "hungUp");

/** A DS row carrying raw application bytes, `ageMs` old at {@link NOW}. */
const row = (id: number, payload: Uint8Array, ageMs = 0) => ({
  id,
  payload,
  createdAt: new Date(NOW - ageMs).toISOString(),
});

const chat = (id: number, text: string, ageMs = 0) =>
  row(id, new TextEncoder().encode(JSON.stringify({ kind: "chat", text, attachmentCount: 0 })), ageMs);

function wasmStub() {
  return {
    Identity: { fromSeed: () => ({ restoreStorage: () => undefined, free: () => undefined }) },
    Group: {
      load: () => ({
        processIncomingVerifyingRoles: (_i: unknown, payload: Uint8Array) => ({
          kind: "Application",
          applicationData: payload,
          senderLeaf: 2,
        }),
        members: () => [{ leafIndex: 2, signaturePublicKey: SENDER_KEY }],
        free: () => undefined,
      }),
    },
    parseBodyEnvelope: (bytes: Uint8Array) => {
      const text = new TextDecoder().decode(bytes);
      try {
        return { ...(JSON.parse(text) as object), free: () => undefined };
      } catch {
        return { kind: "chat", text, attachmentCount: 0, free: () => undefined };
      }
    },
    parseCallSignaling: (bytes: Uint8Array) => realWasm.parseCallSignaling(bytes),
    // The REAL shared ring rule (`crates/lakko-ui-rules/src/ring.rs`), not a
    // stub of it: whether an offer is still live and whether a muted
    // conversation may interrupt are its decisions, and a stub would let
    // these tests pass while the rule said something else.
    ringDecision: realWasm.ringDecision,
    RingDecision: realWasm.RingDecision,
  };
}

/** The one home of the window, read rather than restated. A function because
 *  wasm is only initialised in `beforeAll`. */
const ringWindowMs = (): number => Number(realWasm.callRingWindowMs());

function deps(
  messages: unknown[],
  opts: { senderName?: string | null; wasm?: object; muted?: boolean | null | "throws" } = {},
) {
  const forbidden = { saveSnapshot: vi.fn(), saveMessageLog: vi.fn(), advanceCursor: vi.fn() };
  return {
    forbidden,
    deps: {
      readWakeBinding: async () => ({ devicePseudonymHex: "dd".repeat(32), accountId: "acct" }),
      resolveAccount: async () => ({ accountId: "acct", groupIdBytes: GROUP_BYTES }),
      getDeviceKey: async () => ({}),
      getIdentitySeed: async () => new Uint8Array(64),
      loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
      wasm: opts.wasm ?? wasmStub(),
      dsFetchGroup: async () => ({ messages }),
      lookupSenderName: async (_a: string, _k: unknown, key: string) =>
        key === SENDER_KEY_B64 ? (opts.senderName ?? null) : null,
      nowMs: () => NOW,
      lookupMuted: async () => {
        if (opts.muted === "throws") throw new Error("idb is having a day");
        return opts.muted ?? null;
      },
      ...forbidden,
    },
  };
}

const wake = { deviceHex: "dd".repeat(32), groupHex: GROUP_HEX, pointer: 42 };

describe("an offer becomes a ring", () => {
  it("classifies a fresh offer as a ring naming the call", async () => {
    const { deps: d } = deps([row(42, offer())], { senderName: "Anna" });
    const out = await decryptPushPreview(wake, d);
    expect(out?.call).toEqual({
      kind: "ring",
      callIdHex: CALL_ID_HEX,
      groupHex: GROUP_HEX,
      video: false,
      callerName: "Anna",
      silent: false,
      offerAgeMs: 0,
    });
  });

  it("carries the kind the CALLER chose", async () => {
    // UX §1: "the word on the callee's screen matches what the caller chose".
    const { deps: d } = deps([row(42, offer(true))]);
    expect((await decryptPushPreview(wake, d))?.call.video).toBe(true);
  });

  it("names the caller from what this device already knows, and nothing else", async () => {
    const { deps: d } = deps([row(42, offer())], { senderName: null });
    const out = await decryptPushPreview(wake, d);
    expect(out?.call.callerName).toBeNull();
    // ...and the carried title degrades to the product name, never a guess.
    expect(out?.title).toBe("Lakko");
  });

  it("does not ring for an offer the shared rule calls over", async () => {
    // A wake can be delivered late or replayed to a device that was offline.
    // A phone must not start ringing for a call that ended minutes ago. How
    // long is too long is `lakko_ui_rules::ring`'s to say, and this window is
    // read from it rather than restated: it is the same number the caller's
    // own ring timer runs on, so the two sides give up together.
    const { deps: d } = deps([row(42, offer(), ringWindowMs())]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
    const { deps: fresh } = deps([row(42, offer(), ringWindowMs() - 1_000)]);
    expect((await decryptPushPreview(wake, fresh))?.call.kind).toBe("ring");
  });

  it("does not ring for an offer whose row has no readable timestamp", async () => {
    // Age is judged on the DS server's stamp, never on anything the caller
    // wrote. A call whose age cannot be established does not ring.
    const bad = { id: 42, payload: offer(), createdAt: "not a date" };
    const { deps: d } = deps([bad]);
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });
});

/** `docs/ux/calls.md` section 2.1 — "a muted chat never rings". */
describe("a muted conversation is announced without interrupting", () => {
  it("marks a call in a muted conversation silent, and still announces it", async () => {
    const { deps: d } = deps([row(42, offer())], { muted: true, senderName: "Anna" });
    const out = await decryptPushPreview(wake, d);
    // Not null: dropping it would be telling the person nothing happened.
    expect(out?.call.kind).toBe("ring");
    expect(out?.call.silent).toBe(true);
    expect(out?.call.callerName).toBe("Anna");
  });

  it("asks mute and the caller's name of the BOUND account only (ADR-0042, 2026-09-30)", async () => {
    const asked: string[] = [];
    const { deps: d } = deps([row(42, offer())], { muted: true, senderName: "Anna" });
    const bound = {
      ...d,
      readWakeBinding: async () => ({ devicePseudonymHex: "dd".repeat(32), accountId: "bound" }),
      resolveAccount: async (accountId: string) => ({ accountId, groupIdBytes: GROUP_BYTES }),
      lookupMuted: async (accountId: string) => {
        asked.push(`muted:${accountId}`);
        return true;
      },
      lookupSenderName: async (accountId: string, _k: unknown, key: string) => {
        asked.push(`name:${accountId}`);
        return key === SENDER_KEY_B64 ? "Anna" : null;
      },
    };
    const out = await decryptPushPreview(wake, bound);
    expect(out?.call.silent).toBe(true);
    expect(asked.sort()).toEqual(["muted:bound", "name:bound"]);
  });

  it("leaves an unmuted conversation exactly as it was", async () => {
    const { deps: d } = deps([row(42, offer())], { muted: false });
    expect((await decryptPushPreview(wake, d))?.call.silent).toBe(false);
  });

  it("rings when the muted set cannot be read", async () => {
    // The one read on this path that fails OPEN. Silencing a call because a
    // database did not open is a defect nobody can see; an unwanted ring is
    // one anybody can.
    for (const muted of [null, "throws"] as const) {
      const { deps: d } = deps([row(42, offer())], { muted });
      expect((await decryptPushPreview(wake, d))?.call.silent).toBe(false);
    }
  });

  it("rings when the worker has no way to ask at all", async () => {
    const { deps: d } = deps([row(42, offer())]);
    const { lookupMuted: _drop, ...noMuteDep } = d;
    expect((await decryptPushPreview(wake, noMuteDep))?.call.silent).toBe(false);
  });

  it("does not consult mute for a call that is already over", async () => {
    // `tooLate` outranks mute: the rule judges age first, and a call nobody
    // is waiting on is not a call to be quiet about.
    const { deps: d } = deps([row(42, offer(), ringWindowMs())], { muted: true });
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });

  it("never asks about mute to take a ring DOWN", async () => {
    // An end closes whatever is showing. Mute has nothing to say about that,
    // and asking would risk leaving a ring up on an IDB failure.
    let asked = 0;
    const { deps: d } = deps([row(42, end())], { muted: true });
    const counted = {
      ...d,
      lookupMuted: async () => {
        asked += 1;
        return true;
      },
    };
    expect((await decryptPushPreview(wake, counted))?.call.silent).toBe(false);
    expect(asked).toBe(0);
  });
});

describe("an end takes the ring down", () => {
  it("classifies an end as a cancel for that call", async () => {
    const { deps: d } = deps([row(42, end())]);
    expect((await decryptPushPreview(wake, d))?.call).toEqual({
      kind: "end",
      callIdHex: CALL_ID_HEX,
      groupHex: GROUP_HEX,
      video: false,
      callerName: null,
      silent: false,
      offerAgeMs: 0,
    });
  });

  it("an end that follows its own offer wins", async () => {
    // The ordinary cancelled-call shape, and both rows can arrive in one
    // wake: the device must not start ringing for a call that is over.
    const { deps: d } = deps([row(41, offer()), row(42, end())]);
    expect((await decryptPushPreview(wake, d))?.call.kind).toBe("end");
  });

  it("an offer that follows an unrelated call's end still rings", async () => {
    const { deps: d } = deps([row(41, end(OTHER_CALL_ID)), row(42, offer())]);
    const out = await decryptPushPreview(wake, d);
    expect(out?.call.kind).toBe("ring");
    expect(out?.call.callIdHex).toBe(CALL_ID_HEX);
  });

  it("is NOT stale-gated", async () => {
    // Refusing a late end would leave a ring on screen for a call that has
    // ended — and `requireInteraction: true` makes that permanent.
    const { deps: d } = deps([row(42, end(), 10 * 60_000)]);
    expect((await decryptPushPreview(wake, d))?.call.kind).toBe("end");
  });
});

describe("everything else stays as silent as it was", () => {
  it("never rings for answer, candidate or group control", async () => {
    const groupStart = new Uint8Array([0x72, 0x00]);
    for (const frame of [answer(), candidate(), groupStart]) {
      const { deps: d } = deps([row(42, frame)]);
      expect(await decryptPushPreview(wake, d)).toBeNull();
    }
  });

  it("still never renders a call frame as chat text", async () => {
    // The suppression that predates this change is intact: a frame the
    // classifier declined must not fall through to the lossy-UTF-8 preview.
    const { deps: d } = deps([chat(41, "real message"), row(42, answer())]);
    expect((await decryptPushPreview(wake, d))?.body).toBe("real message");
  });

  it("suppresses every call frame when no codec is wired", async () => {
    // An older deps object has no `parseCallSignaling`; the behaviour then is
    // exactly the pre-ring one, never a garbled preview.
    const stub = wasmStub() as Record<string, unknown>;
    delete stub.parseCallSignaling;
    const { deps: d } = deps([row(42, offer())], { wasm: stub });
    expect(await decryptPushPreview(wake, d)).toBeNull();
  });
});

describe("a call outranks a message preview", () => {
  it("rings even when a newer chat row follows the offer", async () => {
    // A call is an interruption the user has seconds to answer; a message
    // waits.
    const { deps: d } = deps([row(41, offer()), chat(42, "hello")]);
    expect((await decryptPushPreview(wake, d))?.call.kind).toBe("ring");
  });

  it("leaves a plain message batch exactly as it was", async () => {
    // The message shape carries no `call` key at all, so nothing downstream
    // of the pre-call behaviour can see a difference.
    const { deps: d } = deps([chat(42, "hello world")]);
    expect(await decryptPushPreview(wake, d)).toEqual({
      title: "Lakko",
      body: "hello world",
      tag: GROUP_HEX,
    });
  });
});

describe("read-only throwaway (C-59)", () => {
  it("classifies a call without ever persisting or advancing a cursor", async () => {
    const { deps: d, forbidden } = deps([row(41, offer()), row(42, end())]);
    await decryptPushPreview(wake, d);
    expect(forbidden.saveSnapshot).not.toHaveBeenCalled();
    expect(forbidden.saveMessageLog).not.toHaveBeenCalled();
    expect(forbidden.advanceCursor).not.toHaveBeenCalled();
  });
});
