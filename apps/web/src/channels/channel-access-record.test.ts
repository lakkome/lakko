// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  decodeAccessRecord,
  emptyAccessRecord,
  foldVisibility,
  newestAdmissionSeed,
  removalSteps,
  versionsToSend,
  rowsToAdmit,
  subscriberVia,
  ticketKeyAdmits,
  letInVerdict,
  type ChannelInviteLinkRecord,
} from "./channel-access-record";

const KEY = "A".repeat(43) + "=";
const PK = "ab".repeat(32);

function link(over: Partial<ChannelInviteLinkRecord>): ChannelInviteLinkRecord {
  return {
    ledgerIdB64Url: "AAAAAAAA",
    url: "https://x/g/AAAAAAAA#s",
    label: "Friends",
    expiryMs: 0,
    maxUses: 0,
    inviteSeedB64: KEY,
    invitePkHex: PK,
    revocationSeedB64: KEY,
    kind: "link",
    revoked: false,
    mintedAtMs: 1,
    ...over,
  };
}

describe("access record decode", () => {
  it("round-trips through JSON", () => {
    const r = {
      ...emptyAccessRecord(),
      visibility: "private" as const,
      handle: "berlin_news",
      topic: 3,
      language: "de",
      admissionSeeds: [{ version: 1, seedB64: KEY }],
      inviteLinks: [link({})],
      admittedThroughSeq: 12,
      publishers: [KEY],
      publisherKeys: [{ accountKeyB64: KEY, copublisherKeyB64: KEY, accountIdHex: "0f".repeat(16), sentVersions: [1] }],
    };
    expect(decodeAccessRecord(JSON.parse(JSON.stringify(r)))).toEqual(r);
  });

  it("drops malformed fields alone, never the record", () => {
    const d = decodeAccessRecord({
      visibility: "secret",
      handle: "Not Valid",
      topic: 300,
      language: "DE",
      admissionSeeds: [{ version: -1, seedB64: KEY }, { version: 2, seedB64: "short" }, { version: 3, seedB64: KEY }],
      inviteLinks: [{ ...link({}), kind: "other" }, link({ ledgerIdB64Url: "BBBBBBBB" })],
      admittedThroughSeq: -4,
      readKeyB64: "x",
    });
    expect(d).not.toBeNull();
    expect(d?.visibility).toBeNull();
    expect(d?.handle).toBeNull();
    expect(d?.topic).toBeNull();
    expect(d?.language).toBeNull();
    expect(d?.admissionSeeds).toEqual([{ version: 3, seedB64: KEY }]);
    expect(d?.inviteLinks.map((l) => l.ledgerIdB64Url)).toEqual(["BBBBBBBB"]);
    expect(d?.admittedThroughSeq).toBe(0);
    expect(d?.readKeyB64).toBeNull();
  });

  it("is null only for a non-object", () => {
    expect(decodeAccessRecord(null)).toBeNull();
    expect(decodeAccessRecord("x")).toBeNull();
    expect(decodeAccessRecord({})).toEqual(emptyAccessRecord());
  });
});

describe("newestAdmissionSeed", () => {
  it("picks the highest version", () => {
    const r = { ...emptyAccessRecord(), admissionSeeds: [{ version: 1, seedB64: "a" }, { version: 4, seedB64: "b" }, { version: 2, seedB64: "c" }] };
    expect(newestAdmissionSeed(r)?.version).toBe(4);
    expect(newestAdmissionSeed(emptyAccessRecord())).toBeNull();
  });
});

describe("ticket check (ADR-0087 section 7)", () => {
  it("admits only a key the service lists as live", () => {
    const live = new Set([PK]);
    expect(ticketKeyAdmits(PK, live)).toBe(true);
    expect(ticketKeyAdmits(PK.toUpperCase(), live)).toBe(true);
    expect(ticketKeyAdmits("cd".repeat(32), live)).toBe(false);
    expect(ticketKeyAdmits(null, live)).toBe(false);
  });

  it("handles only the rows above the high-water mark, in order", () => {
    const rows = [{ seq: 9 }, { seq: 3 }, { seq: 12 }, { seq: 10 }];
    expect(rowsToAdmit(rows, 9).map((r) => r.seq)).toEqual([10, 12]);
    expect(rowsToAdmit(rows, 0).map((r) => r.seq)).toEqual([3, 9, 10, 12]);
  });
});

describe("subscriber row label (UX 15.1)", () => {
  it("names the link a row came by, an invitation card, or nothing", () => {
    const links = [link({}), link({ ledgerIdB64Url: "CCCCCCCC", kind: "card", invitePkHex: "cd".repeat(32) })];
    expect(subscriberVia(PK, links)).toEqual({ kind: "link", label: "Friends" });
    expect(subscriberVia("cd".repeat(32), links)).toEqual({ kind: "invitation" });
    expect(subscriberVia("ef".repeat(32), links)).toBeNull();
    expect(subscriberVia(null, links)).toBeNull();
  });

  it("lets in a live ticket, never removes the own key, and removes only where the mark is this device's", () => {
    expect(letInVerdict(true, false, false)).toBe("admit");
    expect(letInVerdict(true, true, false)).toBe("admit");
    expect(letInVerdict(false, true, false)).toBe("remove");
    // No mark of its own (a co-publisher, another device of the owner): it
    // cannot tell a reader from before going private, so it removes nobody.
    expect(letInVerdict(false, false, false)).toBe("leave");
    // This device's own card key is never removed.
    expect(letInVerdict(false, true, true)).toBe("leave");
  });

  it("reads a link with no label, from any device, as an invitation", () => {
    const links = [link({ label: "  ", invitePkHex: "cd".repeat(32) }), link({ invitePkHex: "EF".repeat(32), label: "Work" })];
    expect(subscriberVia("cd".repeat(32), links)).toEqual({ kind: "invitation" });
    expect(subscriberVia("ef".repeat(32), links)).toEqual({ kind: "link", label: "Work" });
  });
});

describe("visibility fold (ADR-0087 section 16)", () => {
  it("the owner's own setting wins", () => {
    expect(foldVisibility({ ownerSetting: "private", metaListed: true, cameBy: "public" })).toBe("private");
  });
  it("a reader goes by the newest record's flag, then by what it came by", () => {
    expect(foldVisibility({ ownerSetting: null, metaListed: true, cameBy: "private" })).toBe("public");
    expect(foldVisibility({ ownerSetting: null, metaListed: false, cameBy: "public" })).toBe("private");
    expect(foldVisibility({ ownerSetting: null, metaListed: undefined, cameBy: "public" })).toBe("public");
    expect(foldVisibility({ ownerSetting: null, metaListed: undefined, cameBy: null })).toBeNull();
  });
});

describe("removal ordering (ADR-0087 section 7)", () => {
  it("removes before it rotates, so the reseal never includes the removed key", () => {
    expect(removalSteps(false)).toEqual(["remove", "rotate"]);
    expect(removalSteps(true)).toEqual(["rotate"]);
  });
});

describe("admission seeds to publishers (ADR-0087 section 7)", () => {
  it("sends every version a publisher lacks, oldest first, so older links still open", () => {
    const r = {
      ...emptyAccessRecord(),
      admissionSeeds: [
        { version: 3, seedB64: KEY },
        { version: 1, seedB64: KEY },
        { version: 2, seedB64: KEY },
      ],
    };
    const p = { accountKeyB64: KEY, copublisherKeyB64: "", accountIdHex: "", sentVersions: [2] };
    expect(versionsToSend(r, p)).toEqual([1, 3]);
    expect(versionsToSend(r, { ...p, sentVersions: [1, 2, 3] })).toEqual([]);
  });

  it("drops a malformed account id and keeps the entry", () => {
    const d = decodeAccessRecord({
      publisherKeys: [{ accountKeyB64: KEY, copublisherKeyB64: "", accountIdHex: "xyz", sentVersions: [1, -2, 1] }],
    });
    expect(d?.publisherKeys).toEqual([{ accountKeyB64: KEY, copublisherKeyB64: "", accountIdHex: "", sentVersions: [1] }]);
  });
});
