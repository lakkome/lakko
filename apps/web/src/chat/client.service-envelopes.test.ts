// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 — the FOREGROUND receive loop classifies a committer-liveness
 * beacon as a service envelope, never as a chat bubble.
 *
 * ## The defect this pins
 *
 * The beacon is a 27-byte, field-less envelope (`domain(26) ‖ version(1)`)
 * every owner and admin emits on a fixed interval. `ChatClient.pollMessages`
 * filtered exactly two service-envelope kinds — call signaling and the
 * ADR-0042/0044 push-routing secret — and then fell through to
 * `parseBodyEnvelope`, whose lossy-UTF-8 fallback is CORRECT on its own terms
 * (a pre-envelope legacy message must still render) and therefore turns the
 * beacon into a visible bubble reading `lakko/committer-beacon/v1` plus two
 * control bytes. The case "beacon bytes would render as a chat bubble without
 * the arm" below runs the real parser on the real beacon and shows exactly
 * that, so the arm's necessity is demonstrated rather than asserted.
 *
 * ## Why there are no doubles here
 *
 * The whole trap in this area is a stand-in whose `parseBodyEnvelope`
 * THROWS on binary input where the real `parse_body` DEGRADES. Such a double
 * makes every one of these tests pass with the filter arm deleted, because
 * the beacon would take the (nonexistent) throw path instead of becoming a
 * bubble. This suite sidesteps the question entirely: it boots the REAL wasm
 * core from `wasm/lakko_ffi_wasm_bg.wasm` and drives the REAL
 * `composeCommitterBeacon` / `decodeCommitterBeacon` / `parseBodyEnvelope` /
 * `composePushRoutingSecret`. Nothing about the envelope semantics is
 * modelled; only the DS and the MLS group are stand-ins, and neither of them
 * has an opinion about plaintext.
 *
 * That also settles the fixture-hygiene rule: the beacon domain ends in a NUL
 * byte, and here the fixture is `wasm.composeCommitterBeacon()` — the actual
 * producer — so no NUL is ever written into this file.
 *
 * ## Discrimination (verified by running the broken variant)
 *
 * With the `looksLikeCommitterBeacon` arm removed from `toEvent`, four cases
 * fail and six pass:
 *   - "a committer beacon is classified as a beacon, never a chat event"
 *     FAILS with `expected 'application' to be 'committerBeacon'`;
 *   - "a beacon never reaches the timeline as text" FAILS with
 *     `expected 'lakko/committer-beacon/v1 ' not to contain
 *     'committer-beacon'` -- the visible bubble text itself, the two trailing
 *     control bytes rendered as-is by the reporter;
 *   - "carries the sender leaf and the DS timestamp" FAILS with
 *     `expected 'application' to be 'committerBeacon'`;
 *   - "a mixed batch classifies every row independently" FAILS with
 *     `expected [ 'application', 'application', ...(1) ] to deeply equal
 *     [ 'application', ...(2) ]`.
 * The remaining six pass either way and say so where they sit: they are the
 * regression guards for the two arms that already existed, plus the two
 * property pins (the lossy-fallback demonstration and the FFI-length
 * agreement) that describe the world the arm operates in rather than the arm.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasmNs from "../../wasm/lakko_ffi_wasm";
import { ChatClient, type IncomingChatEvent } from "./client";
import type { DsClient } from "../ds/client";
import type { PushClient } from "../push/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

type Wasm = typeof wasmNs;
let wasm: Wasm;

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await initWasm({ module_or_path: readFileSync(wasmPath) });
  wasm = wasmNs;
});

/**
 * A group whose `processIncomingVerifyingRoles` hands the DS row's payload
 * straight back as an `Application` frame. That is exactly what the real core
 * does for an application message once it has decrypted + role-verified it,
 * and it is the ONLY thing this stand-in models — the plaintext then travels
 * through the real classification chain in `toEvent`.
 */
function passthroughGroup(groupId: Uint8Array, senderLeaf = 3): WasmGroup {
  return {
    get groupId(): Uint8Array {
      return groupId;
    },
    processIncomingVerifyingRoles: (_id: unknown, payload: Uint8Array) => ({
      kind: "Application",
      applicationData: payload,
      senderLeaf,
    }),
  } as unknown as WasmGroup;
}

/** One DS row per supplied plaintext, ids `1..n`. */
function dsWith(...payloads: Uint8Array[]): DsClient {
  return {
    fetchGroup: vi.fn(async () => ({
      messages: payloads.map((payload, i) => ({
        id: i + 1,
        payload,
        createdAt: "2026-08-24T00:00:00Z",
      })),
      hasMore: false,
    })),
  } as unknown as DsClient;
}

function mockPush(): PushClient {
  return { notifyGroup: vi.fn(async () => {}) } as unknown as PushClient;
}

/** A group id no other case in this file reuses — `withGroupMlsLock`'s queue
 *  is module-level, so a shared id would let one failure wedge the next case. */
let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xb0, nextGid]);
}

/** Poll one batch of plaintexts through the real classification chain. */
async function classify(...payloads: Uint8Array[]): Promise<IncomingChatEvent[]> {
  const chat = new ChatClient(
    dsWith(...payloads),
    { identityProvider: () => IDENTITY_TOKEN },
    mockPush(),
  );
  const res = await chat.pollMessages(null, passthroughGroup(freshGid()), 0);
  return res.events;
}

/** A real ADR-0023 chat envelope, composed by the real wasm composer. */
function chatEnvelope(text: string): Uint8Array {
  return wasm.composeEnvelopeText(text);
}

/** The first byte of a `CallSignalingPayload` — field 12 (candidate),
 *  tag `(12 << 3) | 2 = 0x62`. The chat client's call-signaling arm keys on
 *  exactly this byte set; the payload past it is never parsed by `toEvent`. */
const CALL_SIGNALING_BYTES = new Uint8Array([0x62, 0x02, 0x08, 0x01]);

describe("ADR-0045 — the committer beacon is filtered in the foreground loop", () => {
  it("a committer beacon is classified as a beacon, never a chat event", async () => {
    const [ev] = await classify(wasm.composeCommitterBeacon());

    expect(ev?.kind).toBe("committerBeacon");
    // Named explicitly: the failure mode is not "some other kind", it is
    // specifically "a chat bubble".
    expect(ev?.kind).not.toBe("application");
  });

  it("a beacon never reaches the timeline as text", async () => {
    const [ev] = await classify(wasm.composeCommitterBeacon());

    // Whatever the classification, no event this loop emits may carry the
    // domain separator as user-visible text. Written against the event shape
    // rather than the kind so it keeps discriminating if the arm is ever
    // re-routed to a different kind.
    const asText = (ev as { text?: string } | undefined)?.text ?? "";
    expect(asText).not.toContain("committer-beacon");
  });

  it("carries the sender leaf and the DS timestamp — the two fields the envelope omits", async () => {
    // ADR-0045: the beacon deliberately names neither its sender nor its age,
    // so the fold's only sources are the MLS-authenticated leaf and the DS
    // row. Surfacing them is the reason this is an event and not a drop.
    const chat = new ChatClient(
      dsWith(wasm.composeCommitterBeacon()),
      { identityProvider: () => IDENTITY_TOKEN },
      mockPush(),
    );
    const res = await chat.pollMessages(null, passthroughGroup(freshGid(), 7), 0);
    const ev = res.events[0];

    expect(ev?.kind).toBe("committerBeacon");
    if (ev?.kind !== "committerBeacon") throw new Error("unreachable");
    expect(ev.senderLeaf).toBe(7);
    expect(ev.createdAt).toBe("2026-08-24T00:00:00Z");
  });

  it("beacon bytes would render as a chat bubble without the arm", async () => {
    // The defect, demonstrated on the real parser: `parse_body`'s lossy-UTF-8
    // fallback DEGRADES binary input to `chat` (it does NOT throw), which is
    // both correct for legacy plaintext and exactly why the beacon needs its
    // own arm ahead of it. This is also the check that this suite's stand-ins
    // are faithful: the real parser's behaviour here is the thing a naive
    // double would get wrong.
    const parsed = wasm.parseBodyEnvelope(wasm.composeCommitterBeacon());

    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("lakko/committer-beacon/v1");
  });

  it("an unknown beacon version is NOT swallowed by the arm", async () => {
    // Fail-closed on the version byte: the arm claims only what this build's
    // codec accepts. A future v2 beacon falls through to the chat parser like
    // any other unrecognised plaintext rather than being silently eaten by a
    // length-only match — which is what keeps the peek honest about being
    // backed by the codec.
    const notABeacon = wasm.composeCommitterBeacon();
    notABeacon[notABeacon.length - 1] = 0xff;

    const [ev] = await classify(notABeacon);

    expect(ev?.kind).not.toBe("committerBeacon");
    expect(ev?.kind).toBe("application");
  });

  it("the length the arm reads from the FFI matches the wire", async () => {
    // The arm sizes its peek with `committerBeaconEnvelopeLen()` rather than a
    // local `27`. With the real core there is nothing to monkeypatch, so what
    // is pinned here is the agreement itself: if the envelope ever grows and
    // the constant does not follow (or vice versa), this fails and the peek
    // stops matching real beacons.
    expect(wasm.committerBeaconEnvelopeLen()).toBe(
      wasm.composeCommitterBeacon().length,
    );
  });
});

describe("ADR-0048 — the profile request is filtered in the foreground loop", () => {
  // The newcomer's "introduce yourselves": field-less like the beacon, 26
  // bytes, byte-identical on every call. Same failure mode without its arm —
  // a bubble reading the domain string.

  it("a profile request is classified as a request, never a chat event", async () => {
    const [ev] = await classify(wasm.composeProfileRequest());

    expect(ev?.kind).toBe("profileRequest");
    expect(ev?.kind).not.toBe("application");
  });

  it("a request never reaches the timeline as text", async () => {
    const [ev] = await classify(wasm.composeProfileRequest());

    const asText = (ev as { text?: string } | undefined)?.text ?? "";
    expect(asText).not.toContain("profile-request");
  });

  it("carries the sender leaf, the resolved sender key and the DS timestamp", async () => {
    // The self-check ("is this our own echo?") and the answer both key on the
    // sender resolved AT RECEIPT (C-68), never on the leaf.
    const chat = new ChatClient(
      dsWith(wasm.composeProfileRequest()),
      { identityProvider: () => IDENTITY_TOKEN },
      mockPush(),
    );
    const res = await chat.pollMessages(null, passthroughGroup(freshGid(), 7), 0);
    const ev = res.events[0];

    expect(ev?.kind).toBe("profileRequest");
    if (ev?.kind !== "profileRequest") throw new Error("unreachable");
    expect(ev.senderLeaf).toBe(7);
    expect(ev.createdAt).toBe("2026-08-24T00:00:00Z");
  });

  it("request bytes would render as a chat bubble without the arm", () => {
    const parsed = wasm.parseBodyEnvelope(wasm.composeProfileRequest());

    expect(parsed.kind).toBe("chat");
    expect(parsed.text).toContain("lakko/profile-request/v1");
  });

  it("an unknown request version is NOT swallowed by the arm", async () => {
    const notARequest = wasm.composeProfileRequest();
    notARequest[notARequest.length - 1] = 0xff;

    const [ev] = await classify(notARequest);

    expect(ev?.kind).not.toBe("profileRequest");
    expect(ev?.kind).toBe("application");
  });

  it("the length the arm reads from the FFI matches the wire", () => {
    expect(wasm.profileRequestEnvelopeLen()).toBe(wasm.composeProfileRequest().length);
  });

  it("a request and a beacon are told apart, both from a chat row", async () => {
    // Two field-less `l`-prefixed envelopes of nearly the same length: the
    // domain, not the shape, decides.
    const events = await classify(
      wasm.composeCommitterBeacon(),
      wasm.composeProfileRequest(),
      chatEnvelope("after"),
    );

    expect(events.map((e) => e.kind)).toEqual([
      "committerBeacon",
      "profileRequest",
      "application",
    ]);
  });

  it("the real `shouldRequestProfiles` agrees with the rule the pure tests transcribe", () => {
    // `profile/profile-request.test.ts` drives `decideProfileRequest` with a
    // transcription of the core rule; this pins the transcription against the
    // real export on the same four cases.
    const day = wasm.profileRequestMinIntervalMs();
    const now = 1_700_000_000_000n;
    expect(wasm.shouldRequestProfiles(true, 0, now, now)).toBe(true);
    expect(wasm.shouldRequestProfiles(false, 0, undefined, now)).toBe(false);
    expect(wasm.shouldRequestProfiles(false, 1, undefined, now)).toBe(true);
    expect(wasm.shouldRequestProfiles(false, 1, now - 60_000n, now)).toBe(false);
    expect(wasm.shouldRequestProfiles(false, 1, now - day, now)).toBe(true);
    expect(day).toBe(24n * 60n * 60n * 1000n);
  });
});

describe("the pre-existing service-envelope arms still work", () => {
  it("a real chat message is still a chat event", async () => {
    const [ev] = await classify(chatEnvelope("hello"));

    expect(ev?.kind).toBe("application");
    if (ev?.kind !== "application") throw new Error("unreachable");
    expect(ev.text).toBe("hello");
  });

  it("call signaling is still lifted out of the timeline", async () => {
    const [ev] = await classify(CALL_SIGNALING_BYTES);

    expect(ev?.kind).toBe("callSignaling");
  });

  it("a push-routing-secret envelope is still lifted out of the timeline", async () => {
    const secret = new Uint8Array(32).fill(0x5a);
    const [ev] = await classify(wasm.composePushRoutingSecret(11n, secret));

    expect(ev?.kind).toBe("pushRoutingSecret");
    if (ev?.kind !== "pushRoutingSecret") throw new Error("unreachable");
    expect(ev.mintEpoch).toBe(11n);
    expect(Array.from(ev.secret)).toEqual(Array.from(secret));
  });

  it("a mixed batch classifies every row independently", async () => {
    // The arms run per row inside one poll batch; a beacon must not shadow the
    // chat message next to it, nor be shadowed by it.
    const events = await classify(
      chatEnvelope("before"),
      wasm.composeCommitterBeacon(),
      chatEnvelope("after"),
    );

    expect(events.map((e) => e.kind)).toEqual([
      "application",
      "committerBeacon",
      "application",
    ]);
  });
});

describe("group-call control bodies are call signaling, never chat", () => {
  // The call-signaling sniffer knew fields 10..13 (0x52 / 0x5A / 0x62 / 0x6A)
  // and nothing past them, so a GroupCallStart (field 14, 0x72), GroupCallJoin
  // (field 15, 0x7A) and GroupCallLeave (field 16, the two-byte tag 0x82 0x01)
  // fell through to `parseBodyEnvelope` and rendered as a garbage bubble.
  // Every fixture is produced by the real wasm composer.
  const callId = new Uint8Array(16).fill(0x42);

  function groupBodies(): Record<string, Uint8Array> {
    return {
      groupStart: wasm.composeGroupCallStartSignaling(callId, 3, 1_700_000_000_000n, true, true),
      groupJoin: wasm.composeGroupCallJoinSignaling(callId, 3, 1_700_000_000_500n),
      groupLeave: wasm.composeGroupCallLeaveSignaling(callId, 3, "hungUp"),
    };
  }

  it("the leading bytes are the tags the sniffer keys on", () => {
    const b = groupBodies();
    expect(b["groupStart"]?.[0]).toBe(0x72);
    expect(b["groupJoin"]?.[0]).toBe(0x7a);
    expect([b["groupLeave"]?.[0], b["groupLeave"]?.[1]]).toEqual([0x82, 0x01]);
  });

  it("would render as a chat bubble without the arm (the defect)", () => {
    // `parseBodyEnvelope`'s lossy-UTF-8 fallback is correct on its own terms;
    // this pins that the filter, not the parser, is what keeps these out.
    for (const bytes of Object.values(groupBodies())) {
      const parsed = wasm.parseBodyEnvelope(bytes);
      expect(parsed.kind).toBe("chat");
    }
  });

  it("each group body is classified as call signaling and round-trips", async () => {
    const b = groupBodies();
    for (const [kind, bytes] of Object.entries(b)) {
      const [ev] = await classify(bytes);
      expect(ev?.kind, kind).toBe("callSignaling");
      if (ev?.kind !== "callSignaling") throw new Error("unreachable");
      // The sender leaf rides along so the record can refuse a body that
      // speaks for a leaf other than its own sender.
      expect(ev.senderLeaf).toBe(3);
      const parsed = wasm.parseCallSignaling(ev.payloadBytes);
      expect(parsed.kind).toBe(kind);
      expect(Array.from(parsed.callId)).toEqual(Array.from(callId));
    }
  });

  it("a mixed batch keeps the chat rows as chat", async () => {
    const b = groupBodies();
    const events = await classify(
      chatEnvelope("before"),
      b["groupStart"] as Uint8Array,
      b["groupJoin"] as Uint8Array,
      b["groupLeave"] as Uint8Array,
      chatEnvelope("after"),
    );
    expect(events.map((e) => e.kind)).toEqual([
      "application",
      "callSignaling",
      "callSignaling",
      "callSignaling",
      "application",
    ]);
  });
});
