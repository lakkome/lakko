// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { epochSeedsToAdd, epochSeedsToSend, keepsEpochSeeds, takesRotateRequest } from "./publisher-handover";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

describe("epochSeedsToSend (UX 15.2.1)", () => {
  it("sends every held epoch not yet sent, ascending", () => {
    expect(epochSeedsToSend([3, 0, 1, 2], [1])).toEqual([0, 2, 3]);
  });

  it("sends nothing once all were recorded as sent", () => {
    expect(epochSeedsToSend([0, 1], [0, 1])).toEqual([]);
  });

  it("caps a hand-over at the newest 64", () => {
    const held = Array.from({ length: 100 }, (_, i) => i);
    const out = epochSeedsToSend(held, []);
    expect(out).toHaveLength(64);
    expect(out[0]).toBe(36);
    expect(out.at(-1)).toBe(99);
  });

  it("a removed publisher added again has no record, so gets everything again", () => {
    expect(epochSeedsToSend([0, 1, 2], [])).toEqual([0, 1, 2]);
  });
});

describe("keepsEpochSeeds", () => {
  const base = {
    deviceIsOwner: false,
    channelIdHex: "aa",
    envelopeChannelHex: "aa",
    memberAccountKeyB64: "ME",
    ownAccountKeysB64: ["ME"],
    senderB64: "OWNER",
    ownerKeyB64: "OWNER",
  };

  it("keeps a hand-over from the owner, for this channel, addressed to this account", () => {
    expect(keepsEpochSeeds(base)).toBe(true);
    // Addressed to the account's other key (lineage vs current).
    expect(keepsEpochSeeds({ ...base, memberAccountKeyB64: "OLD", ownAccountKeysB64: ["ME", "OLD"] })).toBe(true);
  });

  it("drops one from anyone but the owner", () => {
    expect(keepsEpochSeeds({ ...base, senderB64: "OTHER" })).toBe(false);
    expect(keepsEpochSeeds({ ...base, senderB64: undefined })).toBe(false);
    expect(keepsEpochSeeds({ ...base, ownerKeyB64: null })).toBe(false);
  });

  it("drops one naming another channel, another account, or arriving at the owner's device", () => {
    expect(keepsEpochSeeds({ ...base, envelopeChannelHex: "bb" })).toBe(false);
    expect(keepsEpochSeeds({ ...base, memberAccountKeyB64: "SOMEONE" })).toBe(false);
    // ADR-0090 section 6: an owner device keeps one its own account was
    // sent by another owner device.
    expect(keepsEpochSeeds({ ...base, deviceIsOwner: true })).toBe(true);
  });
});

describe("epochSeedsToAdd", () => {
  it("adds only epochs with no seed held and never overwrites", () => {
    const a = new Uint8Array(32).fill(1);
    const b = new Uint8Array(32).fill(2);
    const out = epochSeedsToAdd(
      [
        { epoch: 0, seed: a },
        { epoch: 1, seed: b },
      ],
      (e) => e === 1,
    );
    expect(out).toEqual([{ epoch: 0, seed: a }]);
  });
});

describe("takesRotateRequest (ADR-0087 section 15, amended 2026-11-08)", () => {
  const base = {
    deviceIsOwner: true,
    channelIdHex: "aa",
    requestChannelHex: "aa",
    senderB64: "PUB",
    ownerKeyB64: "OWNER",
    memberKeysB64: ["OWNER", "PUB"],
  };

  it("the owner's device takes one from a current publisher", () => {
    expect(takesRotateRequest(base)).toBe(true);
  });

  it("a publisher device never acts on one", () => {
    expect(takesRotateRequest({ ...base, deviceIsOwner: false })).toBe(false);
  });

  it("drops one from a non-member, a removed publisher, the owner, or naming another channel", () => {
    expect(takesRotateRequest({ ...base, senderB64: "STRANGER" })).toBe(false);
    expect(takesRotateRequest({ ...base, memberKeysB64: ["OWNER"] })).toBe(false);
    expect(takesRotateRequest({ ...base, senderB64: "OWNER" })).toBe(false);
    expect(takesRotateRequest({ ...base, senderB64: undefined })).toBe(false);
    expect(takesRotateRequest({ ...base, requestChannelHex: "bb" })).toBe(false);
  });
});

describe("the core's envelopes these rules read", () => {
  it("a hand-over opens only for the account it is sealed to, ascending, and dispatches as its own kind", () => {
    const member = new wasm.Identity("member");
    const other = new wasm.Identity("other");
    const channel = new Uint8Array(16).fill(3);
    try {
      const env = wasm.composeChannelEpochSeedsEnvelope(
        channel,
        member.signaturePublicKey,
        member.x25519PublicKey,
        Uint32Array.from([5, 2]),
        new Uint8Array([...new Uint8Array(32).fill(5), ...new Uint8Array(32).fill(2)]),
      );
      const d = wasm.dispatchIncomingEnvelope(env);
      expect(d.kind).toBe(wasm.IncomingEnvelopeKind.ChannelEpochSeeds);
      d.free();
      const opened = wasm.openChannelEpochSeedsEnvelope(env, member);
      expect(Array.from(opened.epochs)).toEqual([2, 5]);
      expect(opened.seeds.slice(0, 32)).toEqual(new Uint8Array(32).fill(2));
      opened.free();
      expect(() => wasm.openChannelEpochSeedsEnvelope(env, other)).toThrow(/channelEpochSeeds\./);
    } finally {
      member.free();
      other.free();
    }
  });

  it("a rotate request carries the channel id and dispatches as its own kind", () => {
    const channel = new Uint8Array(16).fill(4);
    const env = wasm.composeChannelRotateRequestEnvelope(channel);
    expect(env.length).toBe(wasm.channelRotateRequestEnvelopeLen());
    expect(wasm.decodeChannelRotateRequestEnvelope(env)).toEqual(channel);
    const d = wasm.dispatchIncomingEnvelope(env);
    expect(d.kind).toBe(wasm.IncomingEnvelopeKind.ChannelRotateRequest);
    d.free();
  });
});
