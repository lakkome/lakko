// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0021 — the `peer_tag -> person` map over contacts AND received
 * profiles (ADR-0048 v2).
 *
 * Discrimination: dropping the `seen` dedup makes "a contact who also sent a
 * profile is one person, not a collision" fail (the second identical pair is
 * read as a second key); keying collisions on the pair instead of the tag
 * makes "two keys claiming one account id name nobody" fail.
 */
import { describe, expect, it } from "vitest";

import { buildPeerTagMap, personForPeerTag } from "./peer-tag-map";

// A stand-in for `sfuPeerTag` bound to a call: deterministic, one tag per id.
const tagOf = (accountIdHex: string): string => `tag(${accountIdHex})`;

const ALICE_KEY = "alice-key";
const BOB_KEY = "bob-key";
const ALICE_ID = "a".repeat(32);
const BOB_ID = "b".repeat(32);

describe("buildPeerTagMap", () => {
  it("names a person from either source", () => {
    const map = buildPeerTagMap(
      [
        { personKeyB64: ALICE_KEY, accountIdHex: ALICE_ID }, // a contact
        { personKeyB64: BOB_KEY, accountIdHex: BOB_ID }, // a profile only
      ],
      tagOf,
    );
    expect(personForPeerTag(map, tagOf(ALICE_ID))).toBe(ALICE_KEY);
    expect(personForPeerTag(map, tagOf(BOB_ID))).toBe(BOB_KEY);
  });

  it("a contact who also sent a profile is one person, not a collision", () => {
    const map = buildPeerTagMap(
      [
        { personKeyB64: ALICE_KEY, accountIdHex: ALICE_ID },
        { personKeyB64: ALICE_KEY, accountIdHex: ALICE_ID },
      ],
      tagOf,
    );
    expect(map.collided.size).toBe(0);
    expect(personForPeerTag(map, tagOf(ALICE_ID))).toBe(ALICE_KEY);
  });

  it("two keys claiming one account id name nobody", () => {
    const map = buildPeerTagMap(
      [
        { personKeyB64: ALICE_KEY, accountIdHex: ALICE_ID },
        { personKeyB64: BOB_KEY, accountIdHex: ALICE_ID }, // Bob claims Alice's id
        { personKeyB64: BOB_KEY, accountIdHex: BOB_ID },
      ],
      tagOf,
    );
    expect(personForPeerTag(map, tagOf(ALICE_ID))).toBeNull();
    // Bob's own binding still resolves.
    expect(personForPeerTag(map, tagOf(BOB_ID))).toBe(BOB_KEY);
  });

  it("an unknown or absent tag names nobody", () => {
    const map = buildPeerTagMap([{ personKeyB64: ALICE_KEY, accountIdHex: ALICE_ID }], tagOf);
    expect(personForPeerTag(map, undefined)).toBeNull();
    expect(personForPeerTag(map, "tag(nobody)")).toBeNull();
  });

  it("derives each distinct pair once", () => {
    let calls = 0;
    buildPeerTagMap(
      [
        { personKeyB64: ALICE_KEY, accountIdHex: ALICE_ID },
        { personKeyB64: ALICE_KEY, accountIdHex: ALICE_ID },
        { personKeyB64: BOB_KEY, accountIdHex: BOB_ID },
      ],
      (hex) => {
        calls += 1;
        return tagOf(hex);
      },
    );
    expect(calls).toBe(2);
  });
});
