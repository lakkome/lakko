// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 4, "A device that holds nothing asks". The throttle is the
 * REAL core's `shouldRequestProfiles`, so "at most once a day" is asserted
 * against the rule the three clients share.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it, vi } from "vitest";

import initWasm, { profileRequestMinIntervalMs, shouldRequestProfiles } from "../../wasm/lakko_ffi_wasm";
import {
  EMPTY_PUBLICATION,
  INTRODUCTIONS_ASK_WITH_GRANT_MIN_MS,
  INTRODUCTIONS_RESEND_MIN_MS,
  askForMissingIntroductions,
  askResetsRecords,
  decodePublication,
  encodePublication,
  forgetSentRecords,
  holdsIntroductionsFrom,
  offerTargets,
  bundleTargets,
  oneToOnePeerHex,
  shouldAskForIntroductions,
  type PublicationState,
} from "./publication-model";

let day: number;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  day = Number(profileRequestMinIntervalMs());
});

const NOW = 1_800_000_000_000;

const nothingHeld = {
  oneToOne: true,
  holdsRecords: false,
  holdsContactGrant: false,
  lastAskedAtMs: undefined,
  nowMs: NOW,
};

describe("when to ask", () => {
  it("asks in a one-to-one when nothing is held", () => {
    expect(shouldAskForIntroductions(nothingHeld, shouldRequestProfiles)).toBe(true);
  });

  it("does not ask when an offer or a bundle is held", () => {
    expect(shouldAskForIntroductions({ ...nothingHeld, holdsRecords: true }, shouldRequestProfiles)).toBe(false);
  });

  it("asks at most once a day", () => {
    const asked = (lastAskedAtMs: number) =>
      shouldAskForIntroductions({ ...nothingHeld, lastAskedAtMs }, shouldRequestProfiles);
    expect(asked(NOW - 60_000)).toBe(false);
    expect(asked(NOW - day + 1)).toBe(false);
    expect(asked(NOW - day)).toBe(true);
  });

  it("never asks in a group", () => {
    expect(shouldAskForIntroductions({ ...nothingHeld, oneToOne: false }, shouldRequestProfiles)).toBe(false);
  });

  describe("with a contact grant held", () => {
    const granted = { ...nothingHeld, holdsContactGrant: true };
    const asked = (lastAskedAtMs: number) =>
      shouldAskForIntroductions({ ...granted, lastAskedAtMs }, shouldRequestProfiles);

    it("asks the first time", () => {
      expect(shouldAskForIntroductions(granted, shouldRequestProfiles)).toBe(true);
    });

    it("asks at most weekly, not daily", () => {
      expect(asked(NOW - day)).toBe(false);
      expect(asked(NOW - INTRODUCTIONS_ASK_WITH_GRANT_MIN_MS + 1)).toBe(false);
      expect(asked(NOW - INTRODUCTIONS_ASK_WITH_GRANT_MIN_MS)).toBe(true);
    });

    it("does not ask when an offer or a bundle is held", () => {
      expect(shouldAskForIntroductions({ ...granted, holdsRecords: true }, shouldRequestProfiles)).toBe(false);
    });

    it("never asks in a group", () => {
      expect(shouldAskForIntroductions({ ...granted, oneToOne: false }, shouldRequestProfiles)).toBe(false);
    });
  });

  it("finds what is held under any key of the person", () => {
    const held = { offers: { old: {} }, bundles: { other: {} } };
    expect(holdsIntroductionsFrom(held, ["new", "old"])).toBe(true);
    expect(holdsIntroductionsFrom(held, ["other"])).toBe(true);
    expect(holdsIntroductionsFrom(held, ["new"])).toBe(false);
  });
});

describe("the other person of a conversation", () => {
  it("is the one key that is not this account's, however many devices", () => {
    expect(oneToOnePeerHex(["me", "bo", "bo", "me"], "me")).toBe("bo");
  });

  it("is nobody in a group, or alone", () => {
    expect(oneToOnePeerHex(["me", "bo", "cy"], "me")).toBeNull();
    expect(oneToOnePeerHex(["me"], "me")).toBeNull();
  });
});

describe("askForMissingIntroductions", () => {
  function deps(over: Partial<Parameters<typeof askForMissingIntroductions>[0]> = {}) {
    const recordAsked = vi.fn<(g: string, at: number) => void>();
    const send = vi.fn<(g: string) => Promise<void>>(async () => undefined);
    return {
      recordAsked,
      send,
      args: {
        conversations: [{ groupIdB64: "g1", peerHex: "bo" }],
        held: { offers: {}, bundles: {} },
        keysOfPerson: (hex: string) => [hex],
        holdsContactGrant: () => false,
        lastAskedAt: () => undefined,
        recordAsked,
        send,
        rule: shouldRequestProfiles,
        now: () => NOW,
        ...over,
      },
    };
  }

  it("sends the ask, then records the time", async () => {
    const d = deps();
    await askForMissingIntroductions(d.args);
    expect(d.send).toHaveBeenCalledWith("g1");
    expect(d.recordAsked).toHaveBeenCalledWith("g1", NOW);
  });

  it("records nothing when the send failed, and goes on to the next conversation", async () => {
    const send = vi.fn(async (g: string) => {
      if (g === "g1") throw new Error("offline");
    });
    const d = deps({
      conversations: [
        { groupIdB64: "g1", peerHex: "bo" },
        { groupIdB64: "g2", peerHex: "cy" },
      ],
      send,
    });
    await askForMissingIntroductions(d.args);
    expect(send).toHaveBeenCalledTimes(2);
    expect(d.recordAsked.mock.calls).toEqual([["g2", NOW]]);
  });

  it("asks where a contact grant is held and nothing has been asked yet", async () => {
    const d = deps({ holdsContactGrant: () => true });
    await askForMissingIntroductions(d.args);
    expect(d.send).toHaveBeenCalledWith("g1");
    expect(d.recordAsked).toHaveBeenCalledWith("g1", NOW);
  });

  it("uses the same stored time for a contact grant, a week apart", async () => {
    const d = deps({ holdsContactGrant: () => true, lastAskedAt: () => NOW - day });
    await askForMissingIntroductions(d.args);
    expect(d.send).not.toHaveBeenCalled();
  });

  it("asks nothing where something is held", async () => {
    const d = deps({ held: { offers: { bo: {} }, bundles: {} } });
    await askForMissingIntroductions(d.args);
    expect(d.send).not.toHaveBeenCalled();
    expect(d.recordAsked).not.toHaveBeenCalled();
  });

  it("asks nothing in a conversation with no one other person", async () => {
    const d = deps({ conversations: [{ groupIdB64: "g1", peerHex: null }] });
    await askForMissingIntroductions(d.args);
    expect(d.send).not.toHaveBeenCalled();
  });
});

describe("when an ask resets the records", () => {
  const ask = { oneToOne: true, fromOtherParticipant: true, lastResetAtMs: undefined, nowMs: NOW };

  it("resets on the other person's ask in a one-to-one", () => {
    expect(askResetsRecords(ask)).toBe(true);
  });

  it("not on anyone else's", () => {
    expect(askResetsRecords({ ...ask, fromOtherParticipant: false })).toBe(false);
  });

  it("not in a group", () => {
    expect(askResetsRecords({ ...ask, oneToOne: false })).toBe(false);
  });

  it("once an hour", () => {
    expect(askResetsRecords({ ...ask, lastResetAtMs: NOW - INTRODUCTIONS_RESEND_MIN_MS + 1 })).toBe(false);
    expect(askResetsRecords({ ...ask, lastResetAtMs: NOW - INTRODUCTIONS_RESEND_MIN_MS })).toBe(true);
  });
});

describe("the reset", () => {
  const state: PublicationState = {
    ...EMPTY_PUBLICATION,
    offer: { key: "k", recordB64: "r" },
    offerSentTo: { g1: "k", g2: "k" },
    bundleSentTo: { g1: "d", g2: "d" },
  };

  it("forgets only that conversation and records when", () => {
    const next = forgetSentRecords(state, "g1", NOW);
    expect(next.offerSentTo).toEqual({ g2: "k" });
    expect(next.bundleSentTo).toEqual({ g2: "d" });
    expect(next.introductionsResentAt).toEqual({ g1: NOW });
  });

  it("makes the offer and the bundle go out to that conversation alone", () => {
    const next = forgetSentRecords(state, "g1", NOW);
    expect(offerTargets(next, true, ["g1", "g2"])).toEqual(["g1"]);
    expect(bundleTargets(next, "d", ["g1", "g2"])).toEqual(["g1"]);
  });

  it("the time survives a reload, and a row from before it reads as none", () => {
    const next = forgetSentRecords(state, "g1", NOW);
    expect(decodePublication(encodePublication(next)).introductionsResentAt).toEqual({ g1: NOW });
    const { introductionsResentAt: _dropped, ...older } = state;
    expect(decodePublication(JSON.stringify({ v: 2, ...older })).introductionsResentAt).toEqual({});
  });
});
