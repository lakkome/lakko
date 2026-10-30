// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The network search's decisions, with every network answer faked: which
 * questions are asked, how their answers merge into one list, what is
 * dropped silently (a failed reach check, a record pairing a key with
 * someone else's account), and when "not found" may be said.
 */
import { describe, expect, it, vi } from "vitest";

import { base64Encode } from "../auth/account-id";
import { AnonTokenQuotaExhaustedError } from "../tokens/anon-token-pool";
import type { IntroductionProbe, OpenedIntroduction } from "./introductions";
import { toHex } from "./people";
import {
  boundToPublishedKey,
  searchPeople,
  type KnownPerson,
  type PeopleSearchDeps,
} from "./people-search";

const ANNA_KEY = new Uint8Array(32).fill(0xa1);
const ANNA_ACC = new Uint8Array(16).fill(0xa2);
const BO_KEY = new Uint8Array(32).fill(0xb1);
const BO_ACC = new Uint8Array(16).fill(0xb2);
const OTHER_KEY = new Uint8Array(32).fill(0xc1);
const FOF_WIRE = new Uint8Array(37).fill(0x0f);

/** A fake OPRF round whose two halves differ, so a test can tell which one
 *  reached which question. */
const round = (prefix: string, text: string) => ({
  lookupKey: new TextEncoder().encode(`key:${prefix}:${text}`),
  output: new TextEncoder().encode(`out:${prefix}:${text}`),
});

/** A lookup hit with no public record unless one is given. */
const hit = (accountId: Uint8Array, publicRecord: Uint8Array | null = null) => ({ accountId, publicRecord });

const NOBODY: KnownPerson = {
  name: null,
  contactFallbackName: null,
  verifiedUsername: null,
  isContact: false,
  keptGrantWires: [],
};

function deps(over: Partial<PeopleSearchDeps> = {}, known: Record<string, KnownPerson> = {}): PeopleSearchDeps {
  const bundles = new Map<string, Uint8Array>([
    [toHex(ANNA_ACC), ANNA_KEY],
    [toHex(BO_ACC), BO_KEY],
  ]);
  return {
    canonicalUsername: (typed) => {
      const bare = typed.trim().replace(/^@+/, "").toLowerCase();
      return /^[a-z0-9_]{3,32}$/.test(bare) ? bare : null;
    },
    queryTerm: (q) => {
      const words = q.trim().toLowerCase().split(/\s+/).filter((w) => w.length > 0);
      if (words.join("").length < 2) return null;
      return words.reduce((a, b) => (b.length > a.length ? b : a)).slice(0, 16);
    },
    nameMatches: (name, q) => {
      const words = name.toLowerCase().split(/\s+/);
      return q
        .toLowerCase()
        .split(/\s+/)
        .filter((w) => w.length > 0)
        .every((part) => words.some((w) => w.startsWith(part)));
    },
    lookupChannel: async () => null,
    usernameOprf: async (u) => round("u", u),
    accountOfUsernameLookupKey: async () => null,
    nameTermOprf: async (t) => round("t", t),
    accountsOfNameTermLookupKey: async () => [],
    identityKeyOf: async (acc) => bundles.get(toHex(acc)) ?? null,
    known: (keyB64) => known[keyB64] ?? NOBODY,
    openIntroductions: async () => [],
    openPublicRecord: () => null,
    verifyUsername: async () => false,
    reachCheck: async () => true,
    encodeGrant: (wire) => `W:${toHex(wire)}`,
    ownKeyHex: null,
    ...over,
  };
}

const intro = (over: Partial<OpenedIntroduction> = {}): OpenedIntroduction => ({
  identityPk: BO_KEY,
  accountId: BO_ACC,
  displayName: "Bo Lind",
  username: null,
  usernameCertificate: null,
  fofWire: FOF_WIRE,
  via: "term",
  ...over,
});

describe("searchPeople: the username", () => {
  it("finds a person in Everyone and shows the username discovery resolved", async () => {
    const out = await searchPeople(
      "anna",
      deps(
        { accountOfUsernameLookupKey: async () => hit(ANNA_ACC) },
        { [base64Encode(ANNA_KEY)]: { ...NOBODY, name: "Anna Weber" } },
      ),
    );
    expect(out.state).toEqual({ kind: "found" });
    expect(out.people).toHaveLength(1);
    expect(out.people[0]).toMatchObject({
      name: "Anna Weber",
      username: "anna",
      reach: "open",
      accountIdHex: toHex(ANNA_ACC),
      keyHex: toHex(ANNA_KEY),
    });
  });

  it("names someone it holds no profile for by the username itself", async () => {
    const out = await searchPeople("anna", deps({ accountOfUsernameLookupKey: async () => hit(ANNA_ACC) }));
    expect(out.people[0]).toMatchObject({ name: "@anna", username: "anna" });
  });

  it("asks no username or channel question for text that cannot be a username", async () => {
    const usernameOprf = vi.fn(async () => round("u", "x"));
    const lookupChannel = vi.fn(async () => null);
    await searchPeople("Anna Weber", deps({ usernameOprf, lookupChannel }));
    expect(usernameOprf).not.toHaveBeenCalled();
    expect(lookupChannel).not.toHaveBeenCalled();
  });
});

describe("searchPeople: which half of an OPRF round goes where (ADR-0060 section 5)", () => {
  it("asks discovery with lookup keys and opens records with outputs", async () => {
    const accountOfUsernameLookupKey = vi.fn(async () => null);
    const accountsOfNameTermLookupKey = vi.fn(async () => []);
    const openIntroductions = vi.fn(
      async (_probes: readonly IntroductionProbe[]): Promise<OpenedIntroduction[]> => [],
    );
    await searchPeople(
      "bolind",
      deps({ accountOfUsernameLookupKey, accountsOfNameTermLookupKey, openIntroductions }),
    );
    expect(accountOfUsernameLookupKey).toHaveBeenCalledWith(round("u", "bolind").lookupKey);
    expect(accountsOfNameTermLookupKey).toHaveBeenCalledWith(round("t", "bolind").lookupKey);
    const probes = openIntroductions.mock.calls[0]![0];
    expect(probes.map((p) => [p.via, p.oprfOutput]).sort()).toEqual(
      [
        ["term", round("t", "bolind").output],
        ["username", round("u", "bolind").output],
      ].sort(),
    );
  });
});

describe("searchPeople: the states", () => {
  it("says not found when every question was answered and nothing matched", async () => {
    const out = await searchPeople("ghost", deps());
    expect(out).toMatchObject({ people: [], channel: null, state: { kind: "not-found" } });
  });

  it("never says not found when a question failed", async () => {
    const out = await searchPeople(
      "anna",
      deps({
        accountOfUsernameLookupKey: async () => {
          throw new Error("offline");
        },
      }),
    );
    expect(out.state).toEqual({ kind: "failed" });
  });

  it("says the allowance is used when the daily tokens are", async () => {
    const out = await searchPeople(
      "anna",
      deps({
        usernameOprf: async () => {
          throw new AnonTokenQuotaExhaustedError("discovery-lookup", null);
        },
      }),
    );
    expect(out.state).toEqual({ kind: "allowance-used" });
  });

  it("still finds a channel by handle", async () => {
    const out = await searchPeople("news", deps({ lookupChannel: async () => "ab12" }));
    expect(out.channel).toEqual({ idHex: "ab12", handle: "news" });
    expect(out.state).toEqual({ kind: "found" });
  });
});

describe("searchPeople: names in Everyone", () => {
  it("shows a person the name lookup returns only when this device can name them", async () => {
    const out = await searchPeople(
      "anna w",
      deps(
        { accountsOfNameTermLookupKey: async () => [hit(ANNA_ACC), hit(BO_ACC)] },
        { [base64Encode(ANNA_KEY)]: { ...NOBODY, name: "Anna Weber" } },
      ),
    );
    expect(out.people.map((p) => p.name)).toEqual(["Anna Weber"]);
    expect(out.people[0]!.reach).toBe("open");
  });

  it("filters by the whole query, not the one term the network matched", async () => {
    const out = await searchPeople(
      "anna x",
      deps(
        { accountsOfNameTermLookupKey: async () => [hit(ANNA_ACC)] },
        { [base64Encode(ANNA_KEY)]: { ...NOBODY, name: "Anna Weber" } },
      ),
    );
    expect(out.people).toEqual([]);
    expect(out.state).toEqual({ kind: "not-found" });
  });
});

describe("searchPeople: contacts of contacts", () => {
  it("shows an introduced person after the reach check says their key still opens the mailbox", async () => {
    const reachCheck = vi.fn(async () => true);
    const out = await searchPeople("bo", deps({ openIntroductions: async () => [intro()], reachCheck }));
    expect(out.people).toHaveLength(1);
    expect(out.people[0]).toMatchObject({
      name: "Bo Lind",
      reach: "grant",
      grantWireB64: `W:${toHex(FOF_WIRE)}`,
      username: null,
    });
    expect(reachCheck).toHaveBeenCalledWith(BO_KEY, `W:${toHex(FOF_WIRE)}`);
    // What passed is marked, so it beats a kept grant nobody asked about.
    expect(out.people[0]?.grantChecked).toBe(true);
  });

  it("logs one line of counts per finished search", async () => {
    const logCounts = vi.fn();
    await searchPeople(
      "bo",
      deps({
        openIntroductions: async () => [
          intro(),
          intro({ displayName: "Someone Else" }),
          intro({ identityPk: ANNA_KEY, accountId: ANNA_ACC, displayName: "Bo Other" }),
        ],
        introductionRecordsHeld: () => 7,
        reachCheck: async (pk) => toHex(pk) === toHex(ANNA_KEY),
        logCounts,
      }),
    );
    expect(logCounts).toHaveBeenCalledTimes(1);
    expect(logCounts).toHaveBeenCalledWith({
      recordsHeld: 7,
      recordsOpened: 3,
      keptAfterMatch: 2,
      refusedByReachCheck: 1,
      people: 1,
    });
  });

  it("drops the person silently when the reach check says no - as if not found", async () => {
    const out = await searchPeople(
      "bo",
      deps({ openIntroductions: async () => [intro()], reachCheck: async () => false }),
    );
    expect(out.people).toEqual([]);
    expect(out.state).toEqual({ kind: "not-found" });
  });

  it("does not say not found when the reach check could not be asked", async () => {
    const out = await searchPeople(
      "bo",
      deps({
        openIntroductions: async () => [intro()],
        reachCheck: async () => {
          throw new Error("offline");
        },
      }),
    );
    expect(out.state).toEqual({ kind: "failed" });
  });

  it("drops a record pairing its key with someone else's account id", async () => {
    const out = await searchPeople(
      "bo",
      deps({ openIntroductions: async () => [intro({ identityPk: OTHER_KEY })] }),
    );
    expect(out.people).toEqual([]);
  });

  it("drops a record whose name does not match the whole query", async () => {
    const out = await searchPeople("bo x", deps({ openIntroductions: async () => [intro()] }));
    expect(out.people).toEqual([]);
  });

  it("shows the username only after its certificate verifies, reusing the typed username's round", async () => {
    const usernameOprf = vi.fn(async (u: string) => round("u", u));
    const verifyUsername = vi.fn(async () => true);
    const opened = intro({ via: "username", username: "bolind", usernameCertificate: new Uint8Array([1]) });
    const out = await searchPeople(
      "bolind",
      deps({ openIntroductions: async () => [opened], usernameOprf, verifyUsername }),
    );
    expect(out.people[0]).toMatchObject({ username: "bolind", name: "Bo Lind" });
    expect(usernameOprf).toHaveBeenCalledTimes(1);
    // The certificate binds the lookup key, never the output.
    expect(verifyUsername).toHaveBeenCalledWith(new Uint8Array([1]), round("u", "bolind").lookupKey, BO_ACC);
  });

  it("shows no username when the certificate does not verify", async () => {
    const opened = intro({ via: "username", username: "bolind", usernameCertificate: new Uint8Array([1]) });
    const out = await searchPeople("bolind", deps({ openIntroductions: async () => [opened] }));
    expect(out.people[0]!.username).toBeNull();
  });

  it("matches a record opened by the username only for that exact username", async () => {
    const opened = intro({ via: "username", username: "someone_else" });
    const out = await searchPeople("bolind", deps({ openIntroductions: async () => [opened] }));
    expect(out.people).toEqual([]);
  });
});

describe("searchPeople: one list", () => {
  it("keeps one row for a person found twice, and needs no check when their mailbox is open", async () => {
    const reachCheck = vi.fn(async () => true);
    const out = await searchPeople(
      "bolind",
      deps({
        accountOfUsernameLookupKey: async () => hit(BO_ACC),
        openIntroductions: async () => [intro({ via: "username", username: "bolind" })],
        reachCheck,
      }),
    );
    expect(out.people).toHaveLength(1);
    expect(out.people[0]!.reach).toBe("open");
    expect(reachCheck).not.toHaveBeenCalled();
  });

  it("keeps a contact a contact, unchecked", async () => {
    const reachCheck = vi.fn(async () => true);
    const out = await searchPeople(
      "bo",
      deps(
        { openIntroductions: async () => [intro()], reachCheck },
        { [base64Encode(BO_KEY)]: { ...NOBODY, name: "Bo", isContact: true } },
      ),
    );
    expect(out.people[0]).toMatchObject({ reach: "contact", name: "Bo", grantWireB64: null });
    expect(reachCheck).not.toHaveBeenCalled();
  });

  it("never lists the viewer", async () => {
    const out = await searchPeople(
      "anna",
      deps({ accountOfUsernameLookupKey: async () => hit(ANNA_ACC), ownKeyHex: toHex(ANNA_KEY) }),
    );
    expect(out.people).toEqual([]);
  });
});

describe("searchPeople: a contact nothing names", () => {
  const FP = "b1b1 b1b1";
  const unnamedBo: Record<string, KnownPerson> = {
    [base64Encode(BO_KEY)]: { ...NOBODY, isContact: true, contactFallbackName: FP },
  };

  it("an introduction's name beats the fingerprint", async () => {
    const out = await searchPeople(
      "bo lind",
      deps({ openIntroductions: async () => [intro()] }, unnamedBo),
    );
    expect(out.people[0]).toMatchObject({ name: "Bo Lind", reach: "contact" });
    expect(out.people[0]?.nameIsFallback).toBeUndefined();
  });

  it("an unnamed name hit is dropped, never listed by a fingerprint the query did not match", async () => {
    const out = await searchPeople(
      "bo",
      deps({ accountsOfNameTermLookupKey: async () => [hit(BO_ACC)] }, unnamedBo),
    );
    expect(out.people).toEqual([]);
  });

  it("a held name still wins over a published one", async () => {
    const out = await searchPeople(
      "bo",
      deps({ openIntroductions: async () => [intro({ displayName: "Bo Lind" })] }, {
        [base64Encode(BO_KEY)]: { ...NOBODY, name: "Bo", isContact: true },
      }),
    );
    expect(out.people[0]).toMatchObject({ name: "Bo", reach: "contact" });
  });
});

describe("a found key against the published one (ADR-0074 section 3.5a)", () => {
  const FOUND = new Uint8Array(32).fill(0x11);
  const PUBLISHED = new Uint8Array(32).fill(0x22);
  const ACCOUNT = new Uint8Array(16).fill(0x33);

  it("the published key itself is the account's, with no further question", async () => {
    const provenPredecessor = vi.fn(async () => false);
    expect(await boundToPublishedKey({ provenPredecessor }, FOUND, ACCOUNT, FOUND)).toBe(true);
    expect(provenPredecessor).not.toHaveBeenCalled();
  });

  it("a proven predecessor of the published key is the same person", async () => {
    const provenPredecessor = vi.fn(async () => true);
    expect(await boundToPublishedKey({ provenPredecessor }, FOUND, ACCOUNT, PUBLISHED)).toBe(true);
    expect(provenPredecessor).toHaveBeenCalledWith(FOUND, ACCOUNT);
  });

  it("an unproven mismatch, no bundle, or no way to prove it is refused", async () => {
    expect(
      await boundToPublishedKey({ provenPredecessor: async () => false }, FOUND, ACCOUNT, PUBLISHED),
    ).toBe(false);
    expect(await boundToPublishedKey({ provenPredecessor: async () => true }, FOUND, ACCOUNT, null)).toBe(false);
    expect(await boundToPublishedKey({}, FOUND, ACCOUNT, PUBLISHED)).toBe(false);
  });
});
