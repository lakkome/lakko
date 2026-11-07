// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  bindingVerdict,
  matchPublisherLink,
  persistThenPublish,
  removedPublisherFallback,
  withAppointedKey,
  type BindingFacts,
} from "./publisher-app-messages";

const BASE: BindingFacts = {
  isOwner: true,
  senderB64: "PUB",
  ownerKeyB64: "OWNER",
  bindingAccountB64: "PUB",
  bindingChannelHex: "aa11",
  channelIdHex: "AA11",
};

describe("bindingVerdict", () => {
  it("files a binding signed by the sender, for this channel, on an owner device", () => {
    expect(bindingVerdict(BASE)).toBe("file");
  });

  it("drops a binding for another account than the sender", () => {
    expect(bindingVerdict({ ...BASE, bindingAccountB64: "SOMEONE" })).toBe("drop");
  });

  it("drops a binding for another channel", () => {
    expect(bindingVerdict({ ...BASE, bindingChannelHex: "bb22" })).toBe("drop");
  });

  it("drops the owner's own binding and anything with no member sender", () => {
    expect(bindingVerdict({ ...BASE, senderB64: "OWNER", bindingAccountB64: "OWNER" })).toBe("drop");
    expect(bindingVerdict({ ...BASE, senderB64: undefined })).toBe("drop");
    expect(bindingVerdict({ ...BASE, ownerKeyB64: null })).toBe("drop");
  });

  it("a publisher device files nothing", () => {
    expect(bindingVerdict({ ...BASE, isOwner: false })).toBe("drop");
  });
});

describe("withAppointedKey", () => {
  it("adds an entry, updates one, and keeps the array when nothing changes", () => {
    const added = withAppointedKey([], "PUB", "K1");
    expect(added).toEqual([{ accountKeyB64: "PUB", copublisherKeyB64: "K1", accountIdHex: "", sentVersions: [] }]);
    const updated = withAppointedKey(added, "PUB", "K2");
    expect(updated[0]?.copublisherKeyB64).toBe("K2");
    expect(withAppointedKey(updated, "PUB", "K2")).toBe(updated);
  });
});

describe("persistThenPublish", () => {
  it("publishes after the persist", async () => {
    const calls: string[] = [];
    await persistThenPublish(
      async () => {
        calls.push("persist");
      },
      async () => {
        calls.push("publish");
      },
    );
    expect(calls).toEqual(["persist", "publish"]);
  });

  it("a failed persist publishes nothing", async () => {
    const calls: string[] = [];
    await expect(
      persistThenPublish(
        async () => {
          throw new Error("store sealed");
        },
        async () => {
          calls.push("publish");
        },
      ),
    ).rejects.toThrow(/sealed/);
    expect(calls).toEqual([]);
  });
});

describe("removedPublisherFallback", () => {
  it("a publisher who was subscribed falls back to the subscriber view; one who never was ends the channel", () => {
    expect(removedPublisherFallback(true)).toBe("subscriber");
    expect(removedPublisherFallback(false)).toBe("end");
  });
});

describe("matchPublisherLink", () => {
  const local = { kind: "publisher", ledgerIdB64Url: "L1", capabilityB64: "CAP", maxUses: 0, revoked: false };
  const record = { ledgerIdB64Url: "R1", maxUses: 0, revoked: false, live: true, capabilityB64: "CAP" };

  it("matches a live one-use link recorded here or in the register", () => {
    expect(matchPublisherLink("CAP", [local], [])).toEqual({ localLedger: "L1", recordIndex: -1 });
    expect(matchPublisherLink("CAP", [], [record])).toEqual({ localLedger: null, recordIndex: 0 });
  });

  it("refuses a link that is not one use, so a shareable link never auto-admits", () => {
    expect(matchPublisherLink("CAP", [{ ...local, maxUses: 0xffffffff }], [])).toBeNull();
    expect(matchPublisherLink("CAP", [], [{ ...record, maxUses: 0xffffffff }])).toBeNull();
    expect(matchPublisherLink("CAP", [local], [{ ...record, ledgerIdB64Url: "L1", maxUses: 5 }])).toBeNull();
  });

  it("refuses a used, revoked or dead link, and another capability", () => {
    expect(matchPublisherLink("CAP", [{ ...local, used: true }], [])).toBeNull();
    expect(matchPublisherLink("CAP", [{ ...local, revoked: true }], [])).toBeNull();
    expect(matchPublisherLink("CAP", [local], [{ ...record, ledgerIdB64Url: "L1", revoked: true }])).toBeNull();
    expect(matchPublisherLink("CAP", [], [{ ...record, live: false }])).toBeNull();
    expect(matchPublisherLink("OTHER", [local], [record])).toBeNull();
  });
});
