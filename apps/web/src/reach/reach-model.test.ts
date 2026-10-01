// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  EMPTY_REACH_BOOK,
  REPLY_TTL_MS,
  FOF_ROTATION_MS,
  addLink,
  addReply,
  applyAcceptedDiff,
  bumpVersion,
  decodeReachBook,
  desiredReachKeys,
  dropLink,
  dropRepliesTo,
  encodeReachBook,
  fofRotationDue,
  isEmptyDiff,
  pruneExpired,
  raiseVersion,
  reachClassName,
  reachDiff,
  rotateFof,
  versionOf,
  type ReachInputs,
} from "./reach-model";

const NOW = 1_000_000;

function inputs(over: Partial<ReachInputs> = {}): ReachInputs {
  return {
    groupIdsHex: [],
    channels: [],
    allowsGroup: () => false,
    allowsChannel: () => false,
    allowsContactsOfContacts: false,
    everyone: false,
    nowMs: NOW,
    ...over,
  };
}

const classes = (book = EMPTY_REACH_BOOK, over: Partial<ReachInputs> = {}) =>
  desiredReachKeys(book, inputs(over)).map((k) => `${k.cls}:${k.scopeHex}:${k.version}`);

describe("the wanted reach keys", () => {
  it("always wants the contact key, even for Nobody", () => {
    expect(classes()).toEqual(["contact::0"]);
  });

  it("wants a member key per group whatever the setting, a group key only where allowed", () => {
    expect(
      classes(EMPTY_REACH_BOOK, { groupIdsHex: ["aa", "bb"], allowsGroup: (g) => g === "bb" }),
    ).toEqual(["contact::0", "member:aa:0", "member:bb:0", "group:bb:0"]);
  });

  it("versions a channel key by the channel epoch, never backwards", () => {
    const book = raiseVersion(EMPTY_REACH_BOOK, "channel", "cc", 9);
    expect(
      classes(book, { channels: [{ idHex: "cc", epoch: 4 }], allowsChannel: () => true }),
    ).toContain("channel:cc:9");
    expect(
      classes(EMPTY_REACH_BOOK, { channels: [{ idHex: "cc", epoch: 4 }], allowsChannel: () => true }),
    ).toContain("channel:cc:4");
    expect(classes(EMPTY_REACH_BOOK, { channels: [{ idHex: "cc", epoch: 4 }] })).not.toContain(
      "channel:cc:4",
    );
  });

  it("wants the fof key only while contacts of contacts may find the owner", () => {
    expect(classes(EMPTY_REACH_BOOK, { allowsContactsOfContacts: true })).toContain("fof::0");
    expect(classes()).not.toContain("fof::0");
  });

  it("carries live links and open requests with their expiry, and drops expired ones", () => {
    let book = addLink(EMPTY_REACH_BOOK, { linkIdHex: "11", expiresAtMs: NOW + 10 });
    book = addLink(book, { linkIdHex: "22", expiresAtMs: NOW - 1 });
    book = addLink(book, { linkIdHex: "33", expiresAtMs: null });
    book = addReply(book, { scopeHex: "44", expiresAtMs: NOW + REPLY_TTL_MS, purpose: "contact-request" });
    book = addReply(book, { scopeHex: "55", expiresAtMs: NOW - 1, purpose: "group-join" });
    const wanted = desiredReachKeys(book, inputs());
    expect(wanted.filter((w) => w.cls === "link").map((w) => [w.scopeHex, w.expiresAtMs])).toEqual([
      ["11", NOW + 10],
      ["33", null],
    ]);
    expect(wanted.filter((w) => w.cls === "reply").map((w) => w.scopeHex)).toEqual(["44"]);
    const pruned = pruneExpired(book, NOW);
    expect(pruned.links.map((l) => l.linkIdHex)).toEqual(["11", "33"]);
    expect(pruned.replies.map((r) => r.scopeHex)).toEqual(["44"]);
  });

  it("a rotation replaces the old version in the wanted set", () => {
    const book = bumpVersion(EMPTY_REACH_BOOK, "contact", "");
    expect(versionOf(book, "contact", "")).toBe(1);
    expect(classes(book)).toEqual(["contact::1"]);
  });

  it("closes a peer's requests and forgets a revoked link", () => {
    let book = addReply(EMPTY_REACH_BOOK, { scopeHex: "a1", expiresAtMs: NOW + 5, purpose: "contact-request", peerHex: "p" });
    book = addReply(book, { scopeHex: "a2", expiresAtMs: NOW + 5, purpose: "contact-request", peerHex: "q" });
    expect(dropRepliesTo(book, "p").replies.map((r) => r.scopeHex)).toEqual(["a2"]);
    expect(dropLink(addLink(EMPTY_REACH_BOOK, { linkIdHex: "l", expiresAtMs: null }), "l").links).toEqual([]);
  });
});

describe("the registration diff", () => {
  it("adds what is missing, removes what is no longer wanted, re-adds a changed expiry", () => {
    const diff = reachDiff(
      [
        { publicKeyHex: "k1", expiresAtMs: null },
        { publicKeyHex: "k2", expiresAtMs: 5 },
        { publicKeyHex: "k3", expiresAtMs: 9 },
      ],
      { k1: null, k2: 4, k0: null },
      false,
      false,
    );
    expect(diff.add).toEqual([
      { publicKeyHex: "k2", expiresAtMs: 5 },
      { publicKeyHex: "k3", expiresAtMs: 9 },
    ]);
    expect(diff.remove).toEqual(["k0"]);
    expect(diff.open).toBeUndefined();
  });

  it("sets open only when it differs from what was last set", () => {
    expect(reachDiff([], {}, true, false).open).toBe(true);
    expect(reachDiff([], {}, true, true).open).toBeUndefined();
    expect(reachDiff([], {}, false, null).open).toBe(false);
  });

  it("an accepted diff becomes the new base, and an empty diff changes nothing", () => {
    const diff = reachDiff([{ publicKeyHex: "k1", expiresAtMs: null }], { k0: null }, true, null);
    const book = applyAcceptedDiff({ ...EMPTY_REACH_BOOK, registered: { k0: null } }, diff);
    expect(book.registered).toEqual({ k1: null });
    expect(book.open).toBe(true);
    expect(isEmptyDiff(reachDiff([{ publicKeyHex: "k1", expiresAtMs: null }], book.registered, true, true))).toBe(true);
  });
});

describe("the fof schedule and the persisted form", () => {
  it("rotates fof on its schedule", () => {
    expect(fofRotationDue(EMPTY_REACH_BOOK, FOF_ROTATION_MS)).toBe(true);
    const rotated = rotateFof(EMPTY_REACH_BOOK, FOF_ROTATION_MS);
    expect(versionOf(rotated, "fof", "")).toBe(1);
    expect(fofRotationDue(rotated, FOF_ROTATION_MS + 1)).toBe(false);
  });

  it("round-trips and reads garbage as empty", () => {
    const book = addReply(bumpVersion(EMPTY_REACH_BOOK, "member", "aa"), {
      scopeHex: "bb",
      expiresAtMs: 7,
      purpose: "call-join",
    });
    expect(decodeReachBook(encodeReachBook(book))).toEqual(book);
    expect(decodeReachBook("not json")).toEqual(EMPTY_REACH_BOOK);
    expect(decodeReachBook(JSON.stringify({ v: 2 }))).toEqual(EMPTY_REACH_BOOK);
  });

  it("names the frozen class bytes", () => {
    expect([1, 2, 3, 4, 5, 6, 7].map(reachClassName)).toEqual([
      "contact",
      "link",
      "reply",
      "group",
      "member",
      "channel",
      "fof",
    ]);
    expect(reachClassName(0)).toBeNull();
  });
});
