// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CallNameChannel,
  type CallNameSigner,
  rememberCallName,
  rememberedCallName,
} from "./call-name";

/** A signer whose "wire" is `seq:name:signerKeyByte`, verified when the key
 *  byte matches the key the channel holds for the peer. */
function fakeSigner(): CallNameSigner & { composed: string[] } {
  const enc = new TextEncoder();
  const dec = new TextDecoder();
  const composed: string[] = [];
  return {
    composed,
    composeCallName: (self, seq, name) => {
      composed.push(`${self}|${seq}|${name}`);
      return enc.encode(`${seq}:${name}:1`);
    },
    verifyCallName: (_from, key, bytes) => {
      const [seq, name, k] = dec.decode(bytes).split(":");
      if (Number(k) !== key[0]) return null;
      return {
        seq: BigInt(seq ?? "0"),
        name: name === "" ? null : (name ?? null),
      };
    },
  };
}

function wire(seq: number, name: string, key = 1): Uint8Array {
  return new TextEncoder().encode(`${seq}:${name}:${key}`);
}

function channel(selfPeerId: string | null = "me") {
  const signer = fakeSigner();
  const sent: Uint8Array[] = [];
  const names: Array<[string, string | null]> = [];
  const ch = new CallNameChannel({
    signer,
    sendSealed: (pt) => {
      sent.push(pt);
      return true;
    },
    selfPeerId: () => selfPeerId,
    onName: (peer, name) => names.push([peer, name]),
  });
  return { ch, signer, sent, names };
}

const KEY = new Uint8Array([1]);

describe("CallNameChannel", () => {
  it("sends nothing before a name is set, then numbers each change", () => {
    const { ch, signer, sent } = channel();
    ch.resend();
    expect(sent).toHaveLength(0);
    expect(ch.setOwnName("")).toBe(false);
    expect(ch.setOwnName("Anna")).toBe(true);
    expect(ch.setOwnName("Anna")).toBe(false);
    expect(ch.setOwnName("")).toBe(true);
    expect(signer.composed).toEqual(["me|1|Anna", "me|2|"]);
    ch.resend();
    expect(signer.composed.at(-1)).toBe("me|2|");
  });

  it("holds a name set before the Welcome and sends it once the peer id exists", () => {
    let self: string | null = null;
    const signer = fakeSigner();
    const ch = new CallNameChannel({
      signer,
      sendSealed: () => true,
      selfPeerId: () => self,
      onName: () => {},
    });
    ch.setOwnName("Guest Otter");
    expect(signer.composed).toHaveLength(0);
    self = "p1";
    ch.resend();
    expect(signer.composed).toEqual(["p1|1|Guest Otter"]);
  });

  it("reads a name that arrived before the peer's announcement verified", () => {
    const { ch, names } = channel();
    ch.onReceived("bob", wire(1, "Bobby"));
    expect(names).toEqual([]);
    ch.onPeerIdentified("bob", KEY);
    expect(names).toEqual([["bob", "Bobby"]]);
  });

  it("keeps the newest per peer: a re-send or an older copy changes nothing", () => {
    const { ch, names } = channel();
    ch.onPeerIdentified("bob", KEY);
    ch.onReceived("bob", wire(2, "B2"));
    ch.onReceived("bob", wire(2, "B2"));
    ch.onReceived("bob", wire(1, "B1"));
    ch.onReceived("bob", wire(3, ""));
    expect(names).toEqual([
      ["bob", "B2"],
      ["bob", null],
    ]);
  });

  it("drops a name signed by another key than the announcement named", () => {
    const { ch, names } = channel();
    ch.onPeerIdentified("bob", KEY);
    ch.onReceived("bob", wire(1, "Mallory", 9));
    expect(names).toEqual([]);
  });

  it("forgets a peer that left, so its next life starts at seq 1", () => {
    const { ch, names } = channel();
    ch.onPeerIdentified("bob", KEY);
    ch.onReceived("bob", wire(5, "Old"));
    ch.onPeerLeft("bob");
    ch.onPeerIdentified("bob", KEY);
    ch.onReceived("bob", wire(1, "New"));
    expect(names).toEqual([
      ["bob", "Old"],
      ["bob", "New"],
    ]);
  });
});

describe("remembered call names", () => {
  it("keeps a name per call id for a rejoin, and forgets an empty one", () => {
    rememberCallName("aa", "Captain");
    expect(rememberedCallName("aa")).toBe("Captain");
    expect(rememberedCallName("bb")).toBe("");
    rememberCallName("aa", "");
    expect(rememberedCallName("aa")).toBe("");
  });
});
