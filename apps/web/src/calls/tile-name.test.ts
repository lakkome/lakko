// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { resolveDisplay } from "../profile/resolver";
import {
  callTileName,
  EARLY_RELAY_LIMIT,
  EarlyRelays,
  personNameOrNull,
  boundProfileClaim,
} from "./tile-name";

const KEY = new Uint8Array(32).fill(0xa1);
const NONE = { resolved: null, announcedName: null, claimedUsername: null };

describe("callTileName", () => {
  it("prefers what the one resolver calls the person", () => {
    expect(
      callTileName({ resolved: "Anna (work)", announcedName: "Anna", claimedUsername: "anna" }),
    ).toBe("Anna (work)");
  });

  it("uses the name announced in the call when the resolver has none", () => {
    expect(callTileName({ ...NONE, announcedName: "Guest", claimedUsername: "guest_1" })).toBe(
      "Guest",
    );
  });

  it("names a person with no profile name by the username they claim, without @", () => {
    expect(callTileName({ ...NONE, claimedUsername: "valentin" })).toBe("valentin");
  });

  it("is unnamed only when nothing has arrived, and never a piece of an id", () => {
    expect(callTileName(NONE)).toBeNull();
    expect(callTileName({ resolved: "  ", announcedName: "", claimedUsername: " " })).toBeNull();
  });
});

describe("personNameOrNull", () => {
  it("shows a verified username with @", () => {
    const resolved = resolveDisplay({
      signatureKey: KEY,
      profile: { displayName: null, username: "valentin" },
      verification: "verified",
    });
    expect(personNameOrNull(resolved)).toBe("@valentin");
  });

  it("leaves a username that is not verified to the plain-text rule", () => {
    for (const verification of ["pending", "mismatch", "error"] as const) {
      const resolved = resolveDisplay({
        signatureKey: KEY,
        profile: { displayName: null, username: "valentin" },
        verification,
      });
      expect(personNameOrNull(resolved)).toBeNull();
    }
  });

  it("never returns the key fingerprint", () => {
    expect(personNameOrNull(resolveDisplay({ signatureKey: KEY }))).toBeNull();
  });

  it("puts the viewer's rename first", () => {
    const resolved = resolveDisplay({
      signatureKey: KEY,
      rename: "Mum",
      profile: { displayName: "Elena", username: null },
    });
    expect(personNameOrNull(resolved)).toBe("Mum");
  });
});

describe("boundProfileClaim", () => {
  const owner = new Uint8Array(16).fill(0x22);
  const cert = new Uint8Array(153).fill(9);
  const profile = (accountId: Uint8Array | null) => ({
    displayName: "Anna",
    accountId,
    usernameCertificate: cert,
  });

  it("keeps the claim of the owner's profile naming the capability's owner account", () => {
    const p = profile(owner);
    expect(boundProfileClaim(p, true, owner)).toBe(p);
  });

  it("strips the claim of anyone else, and of the owner naming another account", () => {
    for (const [p, fromOwner] of [
      [profile(owner), false],
      [profile(new Uint8Array(16).fill(0x33)), true],
      [profile(null), true],
    ] as const) {
      const out = boundProfileClaim(p, fromOwner, owner);
      expect(out.accountId).toBeNull();
      expect(out.usernameCertificate).toBeNull();
      expect(out.displayName).toBe("Anna");
    }
  });
});

describe("EarlyRelays", () => {
  it("hands back what it kept, oldest first, once", () => {
    const relays = new EarlyRelays<number>();
    relays.keep(1);
    relays.keep(2);
    expect(relays.drain()).toEqual([1, 2]);
    expect(relays.drain()).toEqual([]);
  });

  it("keeps the newest when more arrive than it holds", () => {
    const relays = new EarlyRelays<number>();
    for (let i = 0; i < EARLY_RELAY_LIMIT + 5; i++) relays.keep(i);
    const kept = relays.drain();
    expect(kept).toHaveLength(EARLY_RELAY_LIMIT);
    expect(kept[0]).toBe(5);
    expect(kept[kept.length - 1]).toBe(EARLY_RELAY_LIMIT + 4);
  });
});
