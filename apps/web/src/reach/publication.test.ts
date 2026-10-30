// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { committerCandidates } from "./delegation-rules";
import {
  EMPTY_PUBLICATION,
  bundleTargets,
  decodePublication,
  encodePublication,
  everyoneAction,
  everyoneKey,
  offerKey,
  offerNeedsRebuild,
  offerTargets,
  recordsDigest,
  termsToEvaluate,
} from "./publication-model";
import { keepsScopedEnvelope } from "./scoped-intake";

describe("the Everyone publication", () => {
  const anna = everyoneKey("Anna", "anna", "c");
  it("publishes on entering Everyone and whenever the name, username or certificate moved, and only then", () => {
    expect(everyoneAction(null, true, anna)).toEqual({ kind: "publish" });
    expect(everyoneAction({ public: true, key: anna }, true, anna)).toEqual({ kind: "none" });
    for (const moved of [
      everyoneKey("Anna W", "anna", "c"),
      everyoneKey("Anna", null, null),
      everyoneKey("Anna", "anna", "c2"),
    ]) {
      expect(everyoneAction({ public: true, key: anna }, true, moved)).toEqual({ kind: "publish" });
    }
  });

  it("withdraws on leaving, and never withdraws what was never published", () => {
    expect(everyoneAction({ public: true, key: anna }, false, anna)).toEqual({ kind: "withdraw" });
    expect(everyoneAction(null, false, anna)).toEqual({ kind: "none" });
    expect(everyoneAction({ public: false, key: null }, false, anna)).toEqual({ kind: "none" });
  });
});

describe("the introduction record", () => {
  it("is rebuilt when the fof version, the name or the username moved, and only while 2.1 is on", () => {
    const k = offerKey(0, "Anna", "anna");
    expect(offerNeedsRebuild(null, true, k)).toBe(true);
    expect(offerNeedsRebuild({ key: k, recordB64: "r" }, true, k)).toBe(false);
    expect(offerNeedsRebuild({ key: k, recordB64: "r" }, true, offerKey(1, "Anna", "anna"))).toBe(true);
    expect(offerNeedsRebuild({ key: k, recordB64: "r" }, true, offerKey(0, "Ann", "anna"))).toBe(true);
    expect(offerNeedsRebuild(null, false, k)).toBe(false);
  });

  it("goes to every one-to-one not yet sent this version", () => {
    const state = {
      ...EMPTY_PUBLICATION,
      offer: { key: "k2", recordB64: "r" },
      offerSentTo: { g1: "k2", g2: "k1" },
    };
    expect(offerTargets(state, true, ["g1", "g2", "g3"])).toEqual(["g2", "g3"]);
    expect(offerTargets(state, false, ["g1", "g2", "g3"])).toEqual([]);
  });

  it("relays a bundle only where its record set was not sent", () => {
    const d = recordsDigest(["b", "a"]);
    expect(d).toBe(recordsDigest(["a", "b"]));
    const state = { ...EMPTY_PUBLICATION, bundleSentTo: { g1: d } };
    expect(bundleTargets(state, d, ["g1", "g2"])).toEqual(["g2"]);
  });

  it("spends a token only for a term not evaluated before", () => {
    const state = { ...EMPTY_PUBLICATION, termOutputs: { "n:ann": "x" } };
    expect(termsToEvaluate(state, ["n:ann", "n:anna", "n:anna"])).toEqual(["n:anna"]);
  });

  it("round-trips", () => {
    const state = { ...EMPTY_PUBLICATION, everyone: { public: true, key: "k" }, termOutputs: { t: "h" } };
    expect(decodePublication(encodePublication(state))).toEqual(state);
    expect(decodePublication("[")).toEqual(EMPTY_PUBLICATION);
  });

  it("reads a version-1 row (raw OPRF outputs sent to discovery) as empty, so it is published again", () => {
    const v1 = JSON.stringify({ v: 1, everyone: { public: true, name: "A" }, termHashes: { t: "h" } });
    expect(decodePublication(v1)).toEqual(EMPTY_PUBLICATION);
  });
});

describe("committer candidates", () => {
  it("owner first, admins by rank, unranked last, deduplicated and capped", () => {
    const ranks: Record<string, number> = { a2: 2, a1: 1 };
    expect(committerCandidates("o", ["x", "a2", "a1", "o"], (h) => ranks[h], 16)).toEqual([
      "o",
      "a1",
      "a2",
      "x",
    ]);
    expect(committerCandidates(null, ["a2", "a1"], (h) => ranks[h], 1)).toEqual(["a1"]);
  });
});

describe("which scoped envelopes a conversation keeps", () => {
  const base = { senderResolved: true, fromSelf: false, oneToOne: true };
  it("keeps delegations anywhere, introductions only in a one-to-one", () => {
    expect(keepsScopedEnvelope("committer-delegation", { ...base, oneToOne: false })).toBe(true);
    expect(keepsScopedEnvelope("introduction-offer", base)).toBe(true);
    expect(keepsScopedEnvelope("introductions", { ...base, oneToOne: false })).toBe(false);
  });
  it("drops DeviceGroup-only kinds, our own and unattributed envelopes", () => {
    for (const k of ["visibility", "reach-state", "registrar-seed"] as const) {
      expect(keepsScopedEnvelope(k, base)).toBe(false);
      expect(keepsScopedEnvelope(k, { ...base, fromSelf: true })).toBe(false);
    }
    expect(keepsScopedEnvelope("introduction-offer", { ...base, fromSelf: true })).toBe(false);
    expect(keepsScopedEnvelope("introductions", { ...base, fromSelf: true })).toBe(false);
    expect(keepsScopedEnvelope("committer-delegation", { ...base, senderResolved: false })).toBe(false);
  });
  it("keeps a committer envelope from this account's other device", () => {
    expect(keepsScopedEnvelope("committer-delegation", { ...base, fromSelf: true, oneToOne: false })).toBe(true);
  });
});
