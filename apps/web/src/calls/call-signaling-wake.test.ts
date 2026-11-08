// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The outbound wake policy, driven with frames the REAL core composed.
 *
 * Using `composeCall*Signaling` rather than hand-written tag bytes is the
 * point of the suite: the rule is "an offer and an end wake, nothing else
 * does", and a hand-written fixture would keep passing after a wire change
 * that moved the field numbers. These bytes are the ones that actually
 * travel.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  callSignalingBodyKind,
  looksLikeCallSignaling,
  shouldWakeForCallSignaling,
} from "./call-signaling-wake";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const CALL_ID = new Uint8Array(16).fill(0x11);

const offer = (video = false): Uint8Array =>
  wasm.composeCallOfferSignaling(CALL_ID, "v=0", 1_700_000_000_000n, true, video);
const answer = (): Uint8Array => wasm.composeCallAnswerSignaling(CALL_ID, "v=0");
const candidate = (): Uint8Array =>
  wasm.composeCallIceCandidateSignaling(CALL_ID, "candidate:1 1 udp", "0", 0);
const end = (): Uint8Array => wasm.composeCallEndSignaling(CALL_ID, "hungUp");

describe("callSignalingBodyKind", () => {
  it("names each real frame the core composes", () => {
    expect(callSignalingBodyKind(offer())).toBe("offer");
    expect(callSignalingBodyKind(answer())).toBe("answer");
    expect(callSignalingBodyKind(candidate())).toBe("candidate");
    expect(callSignalingBodyKind(end())).toBe("end");
  });

  it("names the group-control frames from their tag bytes", () => {
    // The core exposes no composer for these on the wasm surface, so the tag
    // table is exercised directly. `groupLeave` is the two-byte varint case.
    expect(callSignalingBodyKind(new Uint8Array([0x72, 0x00]))).toBe("groupStart");
    expect(callSignalingBodyKind(new Uint8Array([0x7a, 0x00]))).toBe("groupJoin");
    expect(callSignalingBodyKind(new Uint8Array([0x82, 0x01, 0x00]))).toBe("groupLeave");
    // ADR-0083 one-to-one screen-share state (field 17): seen live as a garbage
    // bubble on 2026-10-29 while this table did not know it.
    expect(callSignalingBodyKind(new Uint8Array([0x8a, 0x01, 0x00]))).toBe("screenShare");
    // One-to-one camera and microphone state (field 18, 2026-11-07).
    expect(callSignalingBodyKind(new Uint8Array([0x92, 0x01, 0x00]))).toBe("mediaState");
  });

  it("is null for anything that is not call signaling", () => {
    expect(callSignalingBodyKind(new Uint8Array())).toBeNull();
    // A chat envelope is JSON.
    expect(callSignalingBodyKind(new TextEncoder().encode('{"v":1}'))).toBeNull();
    // Every `lakko/...` service envelope starts with `l`.
    expect(callSignalingBodyKind(new TextEncoder().encode("lakko/profile/v1"))).toBeNull();
    // A lone 0x82 without its continuation byte is not a group-leave tag.
    expect(callSignalingBodyKind(new Uint8Array([0x82]))).toBeNull();
    expect(callSignalingBodyKind(new Uint8Array([0x82, 0x02]))).toBeNull();
    expect(callSignalingBodyKind(new Uint8Array([0x8a]))).toBeNull();
    expect(callSignalingBodyKind(new Uint8Array([0x92]))).toBeNull();
  });

  it("agrees with looksLikeCallSignaling", () => {
    for (const bytes of [offer(), answer(), candidate(), end()]) {
      expect(looksLikeCallSignaling(bytes)).toBe(true);
    }
    expect(looksLikeCallSignaling(new TextEncoder().encode('{"v":1}'))).toBe(false);
  });
});

describe("shouldWakeForCallSignaling", () => {
  it("wakes for an offer — the ring itself", () => {
    expect(shouldWakeForCallSignaling(offer(), "call")).toBe(true);
    expect(shouldWakeForCallSignaling(offer(true), "call")).toBe(true);
  });

  it("wakes for an end — so a cancelled call stops ringing a closed tab", () => {
    expect(shouldWakeForCallSignaling(end(), "call")).toBe(true);
    for (const reason of ["declined", "busy", "timedOut", "error"]) {
      expect(shouldWakeForCallSignaling(wasm.composeCallEndSignaling(CALL_ID, reason), "call")).toBe(true);
    }
  });

  it("stays silent for answer and candidate — the peer is already awake", () => {
    expect(shouldWakeForCallSignaling(answer(), "call")).toBe(false);
    expect(shouldWakeForCallSignaling(candidate(), "call")).toBe(false);
  });

  it("stays silent for the group-control frames", () => {
    expect(shouldWakeForCallSignaling(new Uint8Array([0x72, 0x00]), "call")).toBe(false);
    expect(shouldWakeForCallSignaling(new Uint8Array([0x7a, 0x00]), "call")).toBe(false);
    expect(shouldWakeForCallSignaling(new Uint8Array([0x82, 0x01, 0x00]), "call")).toBe(false);
    expect(shouldWakeForCallSignaling(new Uint8Array([0x8a, 0x01, 0x00]), "call")).toBe(false);
  });

  it("never wakes for a frame the session did not ask a wake for", () => {
    // A renegotiation offer and the end of a connected call carry the same
    // tags as the dial and the cancel; the session's intent is what separates
    // them.
    expect(shouldWakeForCallSignaling(offer(), "none")).toBe(false);
    expect(shouldWakeForCallSignaling(offer(true), "none")).toBe(false);
    expect(shouldWakeForCallSignaling(end(), "none")).toBe(false);
  });

  it("fails toward silence on bytes that are not call signaling at all", () => {
    expect(shouldWakeForCallSignaling(new Uint8Array(), "call")).toBe(false);
    expect(shouldWakeForCallSignaling(new TextEncoder().encode('{"v":1}'), "call")).toBe(false);
    expect(shouldWakeForCallSignaling(new TextEncoder().encode("lakko/group-name/v1"), "call")).toBe(false);
  });
});
