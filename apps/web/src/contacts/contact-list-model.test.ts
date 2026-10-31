// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  compareContactLabels,
  foundNameOf,
  groupByLetter,
  letterOf,
  matchesContactQuery,
  OTHER_LETTER,
  sortIncomingRequests,
  sentRowLines,
  upsertSentRequest,
  waitingSentRequests,
  contactsBadge,
} from "./contact-list-model";
import { base64Encode } from "../auth/account-id";
import { badgeText } from "../feed/feed-marks";
import { fingerprintLabel } from "../profile/resolver";

describe("letterOf", () => {
  it("takes the first letter, upper-cased, ignoring a leading @", () => {
    expect(letterOf("anna")).toBe("A");
    expect(letterOf("@boris")).toBe("B");
    expect(letterOf("  ольга")).toBe("О");
    expect(letterOf("émile")).toBe("É");
  });

  it("files digits, punctuation and empty labels under #", () => {
    expect(letterOf("42")).toBe(OTHER_LETTER);
    expect(letterOf("_x")).toBe(OTHER_LETTER);
    expect(letterOf("")).toBe(OTHER_LETTER);
    expect(letterOf("@")).toBe(OTHER_LETTER);
  });
});

describe("groupByLetter", () => {
  const labels = ["zoe", "Anna", "@anton", "42", "Émile", "bob", "_tmp"];

  it("sorts case- and accent-insensitively and puts # last", () => {
    const groups = groupByLetter(labels, (l) => l);
    expect(groups.map((g) => g.letter)).toEqual(["A", "B", "É", "Z", OTHER_LETTER]);
    expect(groups[0]?.items).toEqual(["Anna", "@anton"]);
    // The order inside "#" is the collator's (punctuation vs digits); only
    // membership and the position of the group are the rule.
    expect([...(groups[groups.length - 1]?.items ?? [])].sort()).toEqual(["42", "_tmp"]);
  });

  it("keeps the input untouched and returns nothing for nothing", () => {
    const input = ["b", "a"];
    groupByLetter(input, (l) => l);
    expect(input).toEqual(["b", "a"]);
    expect(groupByLetter([], (l: string) => l)).toEqual([]);
  });

  it("orders numbers naturally", () => {
    expect(compareContactLabels("device 2", "device 10")).toBeLessThan(0);
  });
});

describe("matchesContactQuery", () => {
  it("matches name or username, ignoring case and @", () => {
    expect(matchesContactQuery("AN", "Anna", "@anna")).toBe(true);
    expect(matchesContactQuery("@ann", "Somebody", "@anna")).toBe(true);
    expect(matchesContactQuery("zz", "Anna", "@anna")).toBe(false);
    expect(matchesContactQuery("x", "Anna", null)).toBe(false);
  });

  it("matches everything on an empty query", () => {
    expect(matchesContactQuery("  ", "Anna", null)).toBe(true);
  });
});

describe("sortIncomingRequests", () => {
  it("puts the newest delivery first", () => {
    const sorted = sortIncomingRequests([{ messageId: 3 }, { messageId: 9 }, { messageId: 1 }]);
    expect(sorted.map((r) => r.messageId)).toEqual([9, 3, 1]);
  });
});

describe("sent requests", () => {
  const a = { recipientUsername: "anna", recipientAccountIdHex: "aa", sentAtMs: 100 };
  const b = { recipientUsername: "bob", recipientAccountIdHex: "bb", sentAtMs: 200 };

  it("keeps one record per recipient, the newest send winning", () => {
    const again = { ...a, sentAtMs: 300 };
    const list = upsertSentRequest(upsertSentRequest([a], b), again);
    expect(list).toHaveLength(2);
    expect(list.find((r) => r.recipientAccountIdHex === "aa")?.sentAtMs).toBe(300);
  });

  it("drops a request once its recipient is an accepted contact", () => {
    expect(waitingSentRequests([a, b], new Set(["aa"]))).toEqual([b]);
  });

  it("lists the waiting ones newest first", () => {
    expect(waitingSentRequests([a, b], new Set()).map((r) => r.recipientUsername)).toEqual([
      "bob",
      "anna",
    ]);
  });
});

describe("the row under Sent", () => {
  const record = { recipientUsername: "bo", recipientAccountIdHex: "aa", sentAtMs: 1, recipientKeyB64: "KEY" };

  it("goes when the person is a contact by account id", () => {
    expect(waitingSentRequests([record], new Set(["aa"]))).toEqual([]);
  });

  it("goes when a Welcome made them a contact before their profile named the account", () => {
    expect(waitingSentRequests([record], new Set(), new Set(["KEY"]))).toEqual([]);
  });

  it("stays while they are not", () => {
    expect(waitingSentRequests([record], new Set(["bb"]), new Set(["OTHER"]))).toEqual([record]);
  });
});

describe("what a Sent row shows", () => {
  const base = { recipientAccountIdHex: "aa", sentAtMs: 1 };

  it("the name as found, and @username under it", () => {
    expect(sentRowLines({ ...base, recipientUsername: "qa", recipientName: "Quinn" })).toEqual({
      title: "Quinn",
      username: "@qa",
    });
  });

  it("a person found by name alone has no username line", () => {
    expect(sentRowLines({ ...base, recipientUsername: "", recipientName: "Quinn" })).toEqual({
      title: "Quinn",
      username: null,
    });
  });

  it("a person found by username alone is named by it, once", () => {
    expect(sentRowLines({ ...base, recipientUsername: "qa", recipientName: "@qa" })).toEqual({
      title: "@qa",
      username: null,
    });
  });

  it("an older record with no name reads by its username", () => {
    expect(sentRowLines({ ...base, recipientUsername: "qa" })).toEqual({ title: "@qa", username: null });
  });
});

describe("the Contacts badge", () => {
  it("shows nothing at zero, and the Chats badge's text otherwise", () => {
    expect(contactsBadge(0)).toBeNull();
    expect(contactsBadge(5)).toBe(badgeText(5));
    expect(contactsBadge(100)).toBe(badgeText(100));
  });
});

describe("foundNameOf", () => {
  const key = new Uint8Array(32).fill(7);
  const record = {
    recipientUsername: "elin",
    recipientName: "Elin Holm",
    recipientAccountIdHex: "aa",
    sentAtMs: 1,
    recipientKeyB64: base64Encode(key),
  };

  it("is the name and the username the person was found under", () => {
    expect(foundNameOf(record, key)).toEqual({ displayName: "Elin Holm", username: "elin" });
  });

  it("has no username when none was shown", () => {
    expect(foundNameOf({ ...record, recipientUsername: "" }, key)).toEqual({
      displayName: "Elin Holm",
      username: null,
    });
  });

  it("is nothing without a record, or with an empty name", () => {
    expect(foundNameOf(undefined, key)).toBeNull();
    expect(foundNameOf({ ...record, recipientName: " " }, key)).toBeNull();
    const { recipientName: _name, ...nameless } = record;
    expect(foundNameOf(nameless, key)).toBeNull();
  });

  it("says nothing about a key other than the one found", () => {
    expect(foundNameOf(record, new Uint8Array(32).fill(8))).toBeNull();
    const { recipientKeyB64: _key, ...keyless } = record;
    expect(foundNameOf(keyless, key)).toBeNull();
  });

  it("a person found by their username alone is named by it, not given a name", () => {
    expect(foundNameOf({ ...record, recipientName: "@elin" }, key)).toEqual({
      displayName: null,
      username: "elin",
    });
  });

  it("a name that merely looks like a handle stays a name", () => {
    expect(foundNameOf({ ...record, recipientUsername: "", recipientName: "@null" }, key)).toEqual({
      displayName: "@null",
      username: null,
    });
  });

  it("a fingerprint names nobody", () => {
    expect(foundNameOf({ ...record, recipientName: fingerprintLabel(key) }, key)).toBeNull();
  });
});
