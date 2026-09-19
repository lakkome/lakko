// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 trigger 4 — the profile request, over its ports.
 *
 * No wasm here: the decoder and the core's rule are injected. The rule double
 * below is a transcription of `should_request_profiles` in
 * `crates/lakko-contacts/src/profile_request.rs`; the agreement with the real
 * export is pinned by `chat/client.service-envelopes.test.ts`, which boots the
 * real core.
 */
import { describe, expect, it } from "vitest";

import {
  decideProfileRequest,
  lastProfileRequestAt,
  recordProfileRequest,
  sniffProfileRequest,
  unnamedMemberCount,
  type KeyValueStorage,
  type ProfileRequestRule,
} from "./profile-request";
import type { NameSource } from "./resolver";

/** `b"lakko/profile-request/v1\x00"` — the NUL appended programmatically so
 *  this source file stays NUL-free. */
const DOMAIN = (() => {
  const ascii = new TextEncoder().encode("lakko/profile-request/v1");
  const out = new Uint8Array(ascii.length + 1);
  out.set(ascii, 0);
  return out;
})();
const ENVELOPE_LEN = DOMAIN.length + 1;

function request(version = 0x01): Uint8Array {
  const out = new Uint8Array(ENVELOPE_LEN);
  out.set(DOMAIN, 0);
  out[DOMAIN.length] = version;
  return out;
}

/** Models the fail-closed wasm codec: throws on wrong domain / bad length /
 *  unknown version, returns nothing on success. */
function decoder(len = ENVELOPE_LEN) {
  return {
    profileRequestEnvelopeLen: () => len,
    decodeProfileRequest: (bytes: Uint8Array) => {
      if (bytes.length !== ENVELOPE_LEN) throw new Error("BadLength");
      for (let i = 0; i < DOMAIN.length; i += 1) {
        if (bytes[i] !== DOMAIN[i]) throw new Error("WrongDomain");
      }
      if (bytes[DOMAIN.length] !== 0x01) throw new Error("UnknownVersion");
    },
  };
}

const DAY_MS = 24n * 60n * 60n * 1000n;

/** A transcription of the core rule. */
const rule: ProfileRequestRule = (joined, unnamed, last, now) => {
  if (joined) return true;
  if (unnamed === 0) return false;
  if (last === undefined) return true;
  const since = now > last ? now - last : 0n;
  return since >= DAY_MS;
};

class MemoryStorage implements KeyValueStorage {
  readonly map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
}

describe("sniffProfileRequest", () => {
  it("recognises the well-formed request", () => {
    expect(sniffProfileRequest(decoder(), request())).toBe(true);
  });

  it("a chat envelope (JSON) is skipped by the first byte", () => {
    expect(sniffProfileRequest(decoder(), new TextEncoder().encode('{"v":1}'))).toBe(false);
  });

  it("a real message that starts with `l` and is 26 bytes long is not a request", () => {
    const text = new TextEncoder().encode("lunch at six tomorrow, ok?");
    expect(text.length).toBe(ENVELOPE_LEN);
    expect(sniffProfileRequest(decoder(), text)).toBe(false);
  });

  it("an unknown version is NOT swallowed — it falls through to the chat parser", () => {
    expect(sniffProfileRequest(decoder(), request(0xff))).toBe(false);
  });

  it("reads the length from the FFI, not a local 26", () => {
    // With the FFI reporting a different length a 26-byte request must miss
    // the peek; a sniff carrying the literal would still match.
    expect(sniffProfileRequest(decoder(99), request())).toBe(false);
  });

  it("never throws when the decoder does", () => {
    const broken = {
      profileRequestEnvelopeLen: () => {
        throw new Error("no wasm");
      },
      decodeProfileRequest: () => {
        throw new Error("no wasm");
      },
    };
    expect(sniffProfileRequest(broken, request())).toBe(false);
  });
});

describe("unnamedMemberCount", () => {
  const sources: Record<string, NameSource> = {
    me: "profile",
    alice: "profile",
    bob: "fingerprint",
    carol: "username",
    dave: "rename",
    erin: "fingerprint",
  };
  const sourceOf = (k: string): NameSource => sources[k] ?? "fingerprint";
  const isOwn = (k: string): boolean => k === "me";

  it("counts only people the resolver could not name, the viewer excluded", () => {
    expect(unnamedMemberCount(["me", "alice", "bob", "carol", "dave", "erin"], isOwn, sourceOf)).toBe(2);
  });

  it("the viewer never counts, even when their own row would read as a fingerprint", () => {
    expect(unnamedMemberCount(["me"], isOwn, () => "fingerprint")).toBe(0);
  });

  it("a person listed twice counts once", () => {
    expect(unnamedMemberCount(["bob", "bob"], isOwn, sourceOf)).toBe(1);
  });
});

describe("decideProfileRequest", () => {
  const named = (): NameSource => "profile";
  const unnamed = (): NameSource => "fingerprint";
  const notOwn = (): boolean => false;
  const now = 1_700_000_000_000;

  it("a join always asks, even with nobody unnamed and a fresh stamp", () => {
    const d = decideProfileRequest(
      { joinedJustNow: true, memberKeysB64: ["a"], isOwnKey: notOwn, sourceOf: named, lastRequestedAtMs: now, nowMs: now },
      rule,
    );
    expect(d).toEqual({ ask: true, unnamed: 0 });
  });

  it("nothing to ask when everyone is named", () => {
    const d = decideProfileRequest(
      { joinedJustNow: false, memberKeysB64: ["a", "b"], isOwnKey: notOwn, sourceOf: named, lastRequestedAtMs: undefined, nowMs: now },
      rule,
    );
    expect(d.ask).toBe(false);
  });

  it("asks the first time an unnamed member is seen", () => {
    const d = decideProfileRequest(
      { joinedJustNow: false, memberKeysB64: ["a"], isOwnKey: notOwn, sourceOf: unnamed, lastRequestedAtMs: undefined, nowMs: now },
      rule,
    );
    expect(d).toEqual({ ask: true, unnamed: 1 });
  });

  it("does not repeat within a day — the answers may simply not be back yet", () => {
    const d = decideProfileRequest(
      { joinedJustNow: false, memberKeysB64: ["a"], isOwnKey: notOwn, sourceOf: unnamed, lastRequestedAtMs: now - 60_000, nowMs: now },
      rule,
    );
    expect(d.ask).toBe(false);
  });

  it("repeats once a day has passed", () => {
    const d = decideProfileRequest(
      {
        joinedJustNow: false,
        memberKeysB64: ["a"],
        isOwnKey: notOwn,
        sourceOf: unnamed,
        lastRequestedAtMs: now - Number(DAY_MS),
        nowMs: now,
      },
      rule,
    );
    expect(d.ask).toBe(true);
  });

  it("a stamp that is not a finite number reads as never asked", () => {
    const d = decideProfileRequest(
      { joinedJustNow: false, memberKeysB64: ["a"], isOwnKey: notOwn, sourceOf: unnamed, lastRequestedAtMs: Number.NaN, nowMs: now },
      rule,
    );
    expect(d.ask).toBe(true);
  });

  it("hands the rule the inputs as bigints, truncated and never negative", () => {
    const seen: unknown[] = [];
    const spy: ProfileRequestRule = (...args) => {
      seen.push(args);
      return false;
    };
    decideProfileRequest(
      { joinedJustNow: false, memberKeysB64: ["a"], isOwnKey: notOwn, sourceOf: unnamed, lastRequestedAtMs: -5.7, nowMs: 12.9 },
      spy,
    );
    expect(seen).toEqual([[false, 1, 0n, 12n]]);
  });
});

describe("the last-asked store", () => {
  it("round-trips per (account, group) and reads absent as undefined", () => {
    const s = new MemoryStorage();
    expect(lastProfileRequestAt("acct", "g1", s)).toBeUndefined();
    recordProfileRequest("acct", "g1", 1234.9, s);
    expect(lastProfileRequestAt("acct", "g1", s)).toBe(1234);
    expect(lastProfileRequestAt("acct", "g2", s)).toBeUndefined();
    expect(lastProfileRequestAt("other", "g1", s)).toBeUndefined();
  });

  it("a corrupt row reads as never asked, and a non-finite stamp is not written", () => {
    const s = new MemoryStorage();
    s.setItem("lakko.profile-requests.v1.acct.g1", "not a number");
    expect(lastProfileRequestAt("acct", "g1", s)).toBeUndefined();
    recordProfileRequest("acct", "g1", Number.POSITIVE_INFINITY, s);
    expect(lastProfileRequestAt("acct", "g1", s)).toBeUndefined();
  });

  it("survives a throwing storage and an absent one", () => {
    const throwing: KeyValueStorage = {
      getItem: () => {
        throw new Error("quota");
      },
      setItem: () => {
        throw new Error("quota");
      },
    };
    expect(() => recordProfileRequest("acct", "g1", 1, throwing)).not.toThrow();
    expect(lastProfileRequestAt("acct", "g1", throwing)).toBeUndefined();
    expect(lastProfileRequestAt("acct", "g1", undefined)).toBeUndefined();
    expect(() => recordProfileRequest("acct", "g1", 1, undefined)).not.toThrow();
  });
});
