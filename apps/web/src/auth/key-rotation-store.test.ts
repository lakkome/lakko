// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import {
  EMPTY_KEY_ROTATION_STATE,
  conversationSeed,
  decodeKeyRotationState,
  encodeKeyRotationState,
  predecessorSeeds,
  withPredecessorSeed,
} from "./key-rotation-store";
import { base64Encode } from "./account-id";

const seed = (fill: number): Uint8Array => new Uint8Array(64).fill(fill);

describe("the sealed key-rotation state (ADR-0066 part 3)", () => {
  it("keeps the predecessor seeds oldest first, each once", () => {
    let state = withPredecessorSeed(EMPTY_KEY_ROTATION_STATE, seed(1));
    state = withPredecessorSeed(state, seed(2));
    expect(withPredecessorSeed(state, seed(1))).toBe(state);
    expect(state.predecessorSeedsB64).toEqual([base64Encode(seed(1)), base64Encode(seed(2))]);
  });

  it("the conversation key is the OLDEST predecessor, across a chain of rotations", () => {
    expect(conversationSeed(EMPTY_KEY_ROTATION_STATE)).toBeNull();
    const state = withPredecessorSeed(withPredecessorSeed(EMPTY_KEY_ROTATION_STATE, seed(1)), seed(2));
    expect(Array.from(conversationSeed(state) as Uint8Array)).toEqual(Array.from(seed(1)));
  });

  it("round-trips, with the pending envelopes and a prepared rotation", () => {
    const state = {
      predecessorSeedsB64: [base64Encode(seed(1))],
      pending: [{ newSignaturePubkeyB64: "K", envelopeB64: "E", preparedAtMs: 5 }],
      prepared: {
        deviceIdB64: "D",
        entropyB64: "N",
        proofB64: "P",
        newSignaturePubkeyB64: "K2",
        preparedAtMs: 6,
        proofExpiresAtSecs: 7,
      },
    };
    expect(decodeKeyRotationState(encodeKeyRotationState(state))).toEqual(state);
  });

  it("a damaged part never costs the predecessor seed, and junk reads as empty", () => {
    const good = base64Encode(seed(1));
    const decoded = decodeKeyRotationState(
      JSON.stringify({ predecessorSeedsB64: [good, "short", 7], pending: "junk", prepared: { deviceIdB64: 1 } }),
    );
    expect(decoded).toEqual({ predecessorSeedsB64: [good], pending: [], prepared: null });
    expect(decodeKeyRotationState("{")).toEqual(EMPTY_KEY_ROTATION_STATE);
    expect(decodeKeyRotationState("null")).toEqual(EMPTY_KEY_ROTATION_STATE);
  });
});

describe("own predecessors are a chain (ADR-0066 section 3.5a)", () => {
  it("every held seed, oldest first: the conversation key, then the keys held in between", () => {
    const one = new Uint8Array(64).fill(1);
    const two = new Uint8Array(64).fill(2);
    const state = withPredecessorSeed(withPredecessorSeed(EMPTY_KEY_ROTATION_STATE, one), two);
    expect(predecessorSeeds(state)).toEqual([one, two]);
    expect(predecessorSeeds(EMPTY_KEY_ROTATION_STATE)).toEqual([]);
  });
});
