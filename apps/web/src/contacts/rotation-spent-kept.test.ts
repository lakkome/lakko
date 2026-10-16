// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.5a rule 5 on a KEPT device, and the pieces the third
 * pass added: the durable marker of the spend round, the links this device
 * minted itself (`/c/` call links among them), the kept intro of a sent
 * request, and the five-minute throttle of the address re-check.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  ADDRESS_RECHECK_MIN_INTERVAL_MS,
  addressRecheckAllowed,
} from "./address-recheck-throttle";
import {
  decodeMintedLink,
  encodeMintedLink,
  inlineLinkSlug,
  isInlineLinkSlug,
  parseMintedRowKey,
} from "./minted-links";
import {
  clearSpendOwed,
  decodeSpendOwed,
  installSpentRunners,
  localLinkSpent,
  markSpendOwed,
  readSpendOwed,
  runOwedSpendRound,
  spendOwedAfterAdoption,
  spendOwedAfterRun,
  spendOwedStorageKey,
  type SpentOutcome,
} from "./rotation-spent";
import {
  MAX_STORED_INTRO_CHARS,
  decodeSentIntros,
  encodeSentIntros,
  withSentIntro,
  withoutSentIntro,
} from "./sent-request-intros";

const ACCOUNT = "a2VwdC1kZXZpY2UtYWNjb3VudA==";
const HEX_A = "00112233445566778899aabbccddeeff";
const HEX_B = "ffeeddccbbaa99887766554433221100";

describe("the marker of the spend round on a kept device", () => {
  it("an adoption owes both steps; a later adoption replaces an unfinished marker with the later time", () => {
    expect(spendOwedAfterAdoption(null, 1000.7)).toEqual({ adoptedAtMs: 1000, links: true, requests: true });
    const half = { adoptedAtMs: 1000, links: false, requests: true };
    expect(spendOwedAfterAdoption(half, 5000)).toEqual({ adoptedAtMs: 5000, links: true, requests: true });
    // A clock that went backwards never moves the time back.
    expect(spendOwedAfterAdoption(half, 10).adoptedAtMs).toBe(1000);
  });

  it("a step that answered done is no longer owed; nothing owed clears the marker", () => {
    const both = { adoptedAtMs: 7, links: true, requests: true };
    expect(spendOwedAfterRun(both, { links: "done", requests: "later" })).toEqual({
      adoptedAtMs: 7,
      links: false,
      requests: true,
    });
    expect(spendOwedAfterRun(both, { links: "later", requests: "later" })).toEqual(both);
    expect(spendOwedAfterRun(both, { links: "done", requests: "done" })).toBeNull();
    // A step that was not run stays as it was.
    expect(spendOwedAfterRun({ adoptedAtMs: 7, links: false, requests: true }, { requests: "done" })).toBeNull();
    expect(spendOwedAfterRun(both, {})).toEqual(both);
  });

  it("junk, and a marker that owes nothing, read as nothing owed", () => {
    expect(decodeSpendOwed(null)).toBeNull();
    expect(decodeSpendOwed("{")).toBeNull();
    expect(decodeSpendOwed(JSON.stringify({ adoptedAtMs: 0, links: true, requests: true }))).toBeNull();
    expect(decodeSpendOwed(JSON.stringify({ adoptedAtMs: 5, links: "yes", requests: true }))).toBeNull();
    expect(decodeSpendOwed(JSON.stringify({ adoptedAtMs: 5, links: false, requests: false }))).toBeNull();
    expect(decodeSpendOwed(JSON.stringify({ adoptedAtMs: 5, links: true, requests: false }))).toEqual({
      adoptedAtMs: 5,
      links: true,
      requests: false,
    });
  });

  describe("the round", () => {
    let storage: MemoryStorage;
    beforeEach(() => {
      storage = new MemoryStorage();
    });
    afterEach(() => installSpentRunners(null));

    function runners(links: SpentOutcome[] | null, requests: SpentOutcome[] | null): number[] {
      const seen: number[] = [];
      installSpentRunners({
        links: (at) => {
          seen.push(at);
          return Promise.resolve(links);
        },
        requests: (at) => {
          seen.push(at);
          return Promise.resolve(requests);
        },
      });
      return seen;
    }

    it("with no marker nothing runs", async () => {
      const seen = runners([], []);
      await runOwedSpendRound(ACCOUNT, storage);
      expect(seen).toEqual([]);
    });

    it("both steps run with the adoption time; done on both clears the marker", async () => {
      markSpendOwed(ACCOUNT, 4242, storage);
      const seen = runners(["done", "skipped"], []);
      await runOwedSpendRound(ACCOUNT, storage);
      expect(seen).toEqual([4242, 4242]);
      expect(readSpendOwed(ACCOUNT, storage)).toBeNull();
      expect(storage.getItem(spendOwedStorageKey(ACCOUNT))).toBeNull();
    });

    it("an unreachable server keeps that step owed for the next session start, and only that step runs then", async () => {
      markSpendOwed(ACCOUNT, 4242, storage);
      runners(["done"], ["unreachable"]);
      await runOwedSpendRound(ACCOUNT, storage);
      expect(readSpendOwed(ACCOUNT, storage)).toEqual({ adoptedAtMs: 4242, links: false, requests: true });
      const seen = runners(["done"], ["done"]);
      await runOwedSpendRound(ACCOUNT, storage);
      expect(seen).toEqual([4242]);
      expect(readSpendOwed(ACCOUNT, storage)).toBeNull();
    });

    it("a runner that cannot run yet, or no runner at all, leaves everything owed", async () => {
      markSpendOwed(ACCOUNT, 4242, storage);
      runners(null, null);
      await runOwedSpendRound(ACCOUNT, storage);
      expect(readSpendOwed(ACCOUNT, storage)).toEqual({ adoptedAtMs: 4242, links: true, requests: true });
      installSpentRunners(null);
      await runOwedSpendRound(ACCOUNT, storage);
      expect(readSpendOwed(ACCOUNT, storage)).toEqual({ adoptedAtMs: 4242, links: true, requests: true });
    });

    it("an adoption during a round is not lost to that round's verdict", async () => {
      markSpendOwed(ACCOUNT, 4242, storage);
      installSpentRunners({
        links: () => {
          markSpendOwed(ACCOUNT, 9000, storage);
          return Promise.resolve(["done"]);
        },
        requests: () => Promise.resolve(["done"]),
      });
      await runOwedSpendRound(ACCOUNT, storage);
      expect(readSpendOwed(ACCOUNT, storage)).toEqual({ adoptedAtMs: 9000, links: true, requests: true });
    });

    it("the wipe's clear drops the marker", () => {
      markSpendOwed(ACCOUNT, 4242, storage);
      clearSpendOwed(ACCOUNT, storage);
      expect(readSpendOwed(ACCOUNT, storage)).toBeNull();
    });
  });
});

describe("the links this device minted itself", () => {
  it("a link minted before the keys changed is spent; one minted after is not; a row with no time is spent", () => {
    expect(localLinkSpent(999, 1000)).toBe(true);
    expect(localLinkSpent(1000, 1000)).toBe(false);
    expect(localLinkSpent(1001, 1000)).toBe(false);
    expect(localLinkSpent(null, 1000)).toBe(true);
  });

  it("a row keeps the mint time, the revocation seed and the list facts (ADR-0078 section 10)", () => {
    const link = {
      linkSecret: new Uint8Array(12).fill(1),
      linkId: new Uint8Array(16).fill(2),
      revocationSeed: new Uint8Array(32).fill(3),
      facts: { label: "L", expiryMs: 5, maxUses: 10, bootstrapReferral: true },
    };
    const decoded = decodeMintedLink(encodeMintedLink(link, 1234.9));
    expect(decoded?.mintedAtMs).toBe(1234);
    expect(Array.from(decoded?.linkId ?? [])).toEqual(Array.from(link.linkId));
    expect(Array.from(decoded?.revocationSeed ?? [])).toEqual(Array.from(link.revocationSeed));
    expect(decoded?.facts).toEqual(link.facts);
    // A row kept before ADR-0078 has no seed and no facts: it cannot be revoked.
    const old = decodeMintedLink(JSON.stringify({ s: "AQ==", l: "Ag==", t: 5 }));
    expect(old?.revocationSeed).toBeNull();
    expect(old?.facts).toBeNull();
    expect(decodeMintedLink(JSON.stringify({ s: "AQ==", l: "Ag==" }))?.mintedAtMs).toBeNull();
    expect(decodeMintedLink(JSON.stringify({ s: 1, l: "Ag==" }))).toBeNull();
    expect(decodeMintedLink("{")).toBeNull();
  });

  it("a row key names the kind and the ledger id, of this account only", () => {
    expect(parseMintedRowKey(`${ACCOUNT}|call|abc_DEF-1`, ACCOUNT)).toEqual({ kind: "call", blobIdSlug: "abc_DEF-1" });
    expect(parseMintedRowKey(`${ACCOUNT}|contact|slug`, ACCOUNT)).toEqual({ kind: "contact", blobIdSlug: "slug" });
    expect(parseMintedRowKey(`other|call|slug`, ACCOUNT)).toBeNull();
    expect(parseMintedRowKey(`${ACCOUNT}|group|slug`, ACCOUNT)).toEqual({ kind: "group", blobIdSlug: "slug" });
    expect(parseMintedRowKey(`${ACCOUNT}|other|slug`, ACCOUNT)).toBeNull();
    expect(parseMintedRowKey(`${ACCOUNT}|call|`, ACCOUNT)).toBeNull();
  });

  it("a call link minted while the ledger was unreachable has a row with no ledger id", () => {
    const slug = inlineLinkSlug("AAAA");
    expect(isInlineLinkSlug(slug)).toBe(true);
    expect(isInlineLinkSlug("AAAA")).toBe(false);
    expect(parseMintedRowKey(`${ACCOUNT}|call|${slug}`, ACCOUNT)).toEqual({ kind: "call", blobIdSlug: slug });
  });
});

describe("the intro kept for a sent request", () => {
  it("round-trips, one per recipient, the newest text wins", () => {
    let intros = withSentIntro({}, HEX_A, "hello");
    intros = withSentIntro(intros, HEX_B, "we met at the fair");
    intros = withSentIntro(intros, HEX_A.toUpperCase(), "hello again");
    expect(decodeSentIntros(encodeSentIntros(intros))).toEqual({ [HEX_B]: "we met at the fair", [HEX_A]: "hello again" });
  });

  it("answered or withdrawn drops it; an empty or over-long intro is not kept", () => {
    const intros = withSentIntro(withSentIntro({}, HEX_A, "hello"), HEX_B, "hi");
    expect(withoutSentIntro(intros, HEX_A)).toEqual({ [HEX_B]: "hi" });
    expect(withSentIntro(intros, HEX_A, "")).toEqual({ [HEX_B]: "hi" });
    expect(withSentIntro({}, HEX_A, "x".repeat(MAX_STORED_INTRO_CHARS + 1))).toEqual({});
    expect(withSentIntro({}, HEX_A, "x".repeat(MAX_STORED_INTRO_CHARS))[HEX_A]).toHaveLength(MAX_STORED_INTRO_CHARS);
    expect(withSentIntro({}, "not-an-account-id", "hello")).toEqual({});
  });

  it("junk reads as no intros, entry by entry", () => {
    expect(decodeSentIntros("{")).toEqual({});
    expect(decodeSentIntros(JSON.stringify({ v: 1, intros: [] }))).toEqual({});
    expect(
      decodeSentIntros(JSON.stringify({ v: 1, intros: { [HEX_A]: "ok", [HEX_B]: 7, short: "x" } })),
    ).toEqual({ [HEX_A]: "ok" });
  });
});

describe("the address re-check throttle", () => {
  it("is five minutes per person", () => {
    expect(ADDRESS_RECHECK_MIN_INTERVAL_MS).toBe(5 * 60 * 1000);
    expect(addressRecheckAllowed(null, 1_000_000)).toBe(true);
    expect(addressRecheckAllowed(1_000_000, 1_000_000 + 299_999)).toBe(false);
    expect(addressRecheckAllowed(1_000_000, 1_000_000 + 300_000)).toBe(true);
  });

  it("a last time in the future reads as just re-checked", () => {
    expect(addressRecheckAllowed(2_000_000, 1_000_000)).toBe(false);
    expect(addressRecheckAllowed(Number.NaN, 1_000_000)).toBe(false);
  });
});
