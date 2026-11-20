// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CallChatChannel,
  MAX_PENDING_PER_PEER,
  type CallChatMessage,
  type CallChatSigner,
} from "./call-chat";

const enc = new TextEncoder();
const dec = new TextDecoder();

/** A signer whose "wire" is `seq|text|keyByte`, verified when the key byte
 *  matches the key the channel holds for the peer. */
function fakeSigner(): CallChatSigner & { composed: string[] } {
  const composed: string[] = [];
  return {
    composed,
    composeCallChat: (self, seq, text) => {
      composed.push(`${self}|${seq}|${text}`);
      return enc.encode(`${seq}|${text}|1`);
    },
    verifyCallChat: (_from, key, bytes) => {
      const [seq, text, k] = dec.decode(bytes).split("|");
      if (Number(k) !== key[0]) return null;
      return { seq: BigInt(seq ?? "0"), text: text ?? "" };
    },
  };
}

function wire(seq: number, text: string, key = 1): Uint8Array {
  return enc.encode(`${seq}|${text}|${key}`);
}

const KEY = new Uint8Array([1]);

function setup(opts: { self?: string | null; sendOk?: () => boolean } = {}) {
  const signer = fakeSigner();
  const sent: Uint8Array[] = [];
  const changes: (readonly CallChatMessage[])[] = [];
  const incoming: CallChatMessage[] = [];
  const logs: string[] = [];
  let now = 1_000;
  let self: string | null = opts.self === undefined ? "me" : opts.self;
  const ch = new CallChatChannel({
    signer,
    sendSealed: (p) => {
      const ok = opts.sendOk?.() ?? true;
      if (ok) sent.push(p);
      return ok;
    },
    selfPeerId: () => self,
    now: () => now,
    onChange: (m) => changes.push(m),
    onIncoming: (m) => incoming.push(m),
    log: (l) => logs.push(l),
  });
  return {
    ch,
    signer,
    sent,
    changes,
    incoming,
    logs,
    tick: (ms: number) => {
      now += ms;
    },
    setSelf: (s: string | null) => {
      self = s;
    },
  };
}

describe("CallChatChannel — sending", () => {
  it("numbers own messages 1, 2, 3 and shows them at once as sent", () => {
    const t = setup();
    t.ch.send("hello");
    t.ch.send("again");
    expect(t.signer.composed).toEqual(["me|1|hello", "me|2|again"]);
    expect(t.sent.map((b) => dec.decode(b))).toEqual(["1|hello|1", "2|again|1"]);
    const list = t.ch.list();
    expect(list.map((m) => [m.id, m.own, m.delivery])).toEqual([
      ["own:1", true, "sent"],
      ["own:2", true, "sent"],
    ]);
    // Own messages are never "incoming".
    expect(t.incoming).toEqual([]);
  });

  it("marks a message not sent when it cannot leave the device, and Send again sends the same bytes", () => {
    let ok = false;
    const t = setup({ sendOk: () => ok });
    const m = t.ch.send("lost");
    expect(m.delivery).toBe("not-sent");
    expect(t.ch.list()[0]?.delivery).toBe("not-sent");
    ok = true;
    expect(t.ch.sendAgain(m.id)).toBe(true);
    expect(t.ch.list()[0]?.delivery).toBe("sent");
    expect(t.sent.map((b) => dec.decode(b))).toEqual(["1|lost|1"]);
    // Composed once: the retry re-sent those bytes, not a new message.
    expect(t.signer.composed).toEqual(["me|1|lost"]);
    // The next message takes the next number.
    t.ch.send("next");
    expect(t.signer.composed.at(-1)).toBe("me|2|next");
  });

  it("composes a not-sent message with its own number once the device has a peer id", () => {
    const t = setup({ self: null });
    const m = t.ch.send("early");
    expect(m.delivery).toBe("not-sent");
    t.setSelf("me");
    expect(t.ch.sendAgain(m.id)).toBe(true);
    expect(t.signer.composed).toEqual(["me|1|early"]);
  });

  it("does nothing for Send again on a sent message or someone else's", () => {
    const t = setup();
    const m = t.ch.send("fine");
    expect(t.ch.sendAgain(m.id)).toBe(false);
    t.ch.onPeerIdentified("anna", KEY);
    t.ch.onReceived("anna", wire(1, "hi"));
    expect(t.ch.sendAgain("anna:1")).toBe(false);
    expect(t.sent.length).toBe(1);
  });
});

describe("CallChatChannel — receiving", () => {
  it("shows a message from an identified peer, verified against its key", () => {
    const t = setup();
    t.ch.onPeerIdentified("anna", KEY);
    t.tick(5);
    t.ch.onReceived("anna", wire(1, "hi"));
    expect(t.incoming.map((m) => [m.id, m.peerId, m.text, m.arrivedAtMs])).toEqual([
      ["anna:1", "anna", "hi", 1_005],
    ]);
    expect(t.ch.list().length).toBe(1);
  });

  it("drops a message that does not verify, saying nothing about its content", () => {
    const t = setup();
    t.ch.onPeerIdentified("anna", KEY);
    t.ch.onReceived("anna", wire(1, "forged", 9));
    expect(t.ch.list()).toEqual([]);
    expect(t.logs.length).toBe(1);
    expect(t.logs[0]).not.toContain("forged");
    expect(t.logs[0]).not.toContain("anna");
  });

  it("holds messages until the peer's announcement verifies, then shows them in arrival order", () => {
    const t = setup();
    t.ch.onReceived("bob", wire(1, "first"));
    t.tick(10);
    t.ch.onReceived("bob", wire(2, "second"));
    expect(t.ch.list()).toEqual([]);
    t.tick(100);
    t.ch.onPeerIdentified("bob", KEY);
    expect(t.ch.list().map((m) => [m.text, m.arrivedAtMs])).toEqual([
      ["first", 1_000],
      ["second", 1_010],
    ]);
  });

  it("never shows held messages from a peer that never verifies", () => {
    const t = setup();
    t.ch.onReceived("eve", wire(1, "x"));
    t.ch.onPeerLeft("eve");
    t.ch.onPeerIdentified("eve", KEY);
    expect(t.ch.list()).toEqual([]);
  });

  it("holds at most fifty per peer, dropping the oldest", () => {
    const t = setup();
    for (let i = 1; i <= MAX_PENDING_PER_PEER + 5; i += 1) t.ch.onReceived("bob", wire(i, `m${i}`));
    t.ch.onPeerIdentified("bob", KEY);
    const shown = t.ch.list();
    expect(shown.length).toBe(MAX_PENDING_PER_PEER);
    expect(shown[0]?.text).toBe("m6");
  });

  it("shows each (peer, seq) once", () => {
    const t = setup();
    t.ch.onPeerIdentified("anna", KEY);
    t.ch.onReceived("anna", wire(1, "hi"));
    t.ch.onReceived("anna", wire(1, "hi"));
    t.ch.onPeerIdentified("bob", KEY);
    t.ch.onReceived("bob", wire(1, "hi"));
    expect(t.ch.list().map((m) => m.id)).toEqual(["anna:1", "bob:1"]);
    expect(t.incoming.length).toBe(2);
  });

  it("keeps a peer's messages when the peer leaves", () => {
    const t = setup();
    t.ch.onPeerIdentified("anna", KEY);
    t.ch.onReceived("anna", wire(1, "bye"));
    t.ch.onPeerLeft("anna");
    expect(t.ch.list().map((m) => m.text)).toEqual(["bye"]);
  });

  it("ignores a frame that claims to come from this device", () => {
    const t = setup();
    t.ch.onPeerIdentified("me", KEY);
    t.ch.onReceived("me", wire(1, "echo"));
    expect(t.ch.list()).toEqual([]);
  });
});

describe("CallChatChannel — the call ends", () => {
  it("clears every message, the held ones and the numbering state", () => {
    const t = setup();
    t.ch.send("mine");
    t.ch.onPeerIdentified("anna", KEY);
    t.ch.onReceived("anna", wire(1, "hers"));
    t.ch.onReceived("bob", wire(1, "held"));
    t.ch.clear();
    expect(t.ch.list()).toEqual([]);
    expect(t.changes.at(-1)).toEqual([]);
    // Nothing held survives: an identification afterwards shows nothing.
    t.ch.onPeerIdentified("bob", KEY);
    expect(t.ch.list()).toEqual([]);
  });
});
