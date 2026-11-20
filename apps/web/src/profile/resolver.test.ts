// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §5 / UX spec §3, §6 — the display precedence table.
 *
 * ## Discrimination (verified by running the broken variants)
 *
 *   - Showing a claimed username without checking `verification` fails
 *     "an unverified claim is not shown at all" with
 *     `expected '@pavel' to be 'a1b2 c3d4'`.
 *   - Letting the profile name beat a local rename fails "a local rename wins
 *     over everything" with `expected 'Alice Petrova' to be 'Mum'`.
 *   - Restoring the old `@a1b2c3d4` fingerprint label fails "falls back to the
 *     fingerprint without @" with `expected '@a1b2c3d4' to be 'a1b2 c3d4'`.
 */
import { describe, expect, it } from "vitest";

import { memberListLabelOf, memberNameOf, UNNAMED_MEMBER } from "./member-label";
import { fingerprintLabel, profileInputOf, resolveDisplay, resolvePerson } from "./resolver";

const KEY = new Uint8Array([0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, ...new Array<number>(26).fill(0)]);

describe("resolveDisplay — precedence", () => {
  it("a local rename wins over everything", () => {
    const r = resolveDisplay({
      signatureKey: KEY,
      rename: "Mum",
      profile: { displayName: "Alice Petrova", username: "alice" },
      verification: "verified",
    });
    expect(r.name).toBe("Mum");
    expect(r.source).toBe("rename");
    expect(r.theirName).toBe("Alice Petrova");
    expect(r.listLabel).toBe("Mum · @alice");
  });

  it("the profile name comes next, with a verified username under it", () => {
    const r = resolveDisplay({
      signatureKey: KEY,
      profile: { displayName: "Alice Petrova", username: "alice" },
      verification: "verified",
    });
    expect(r.name).toBe("Alice Petrova");
    expect(r.source).toBe("profile");
    expect(r.username).toBe("@alice");
    expect(r.listLabel).toBe("Alice Petrova · @alice");
    expect(r.theirName).toBeNull();
  });

  it("a verified username stands in when there is no name", () => {
    const r = resolveDisplay({
      signatureKey: KEY,
      profile: { displayName: null, username: "alice" },
      verification: "verified",
    });
    expect(r.name).toBe("@alice");
    expect(r.source).toBe("username");
    // Not repeated as `@alice · @alice`.
    expect(r.listLabel).toBe("@alice");
  });

  it("falls back to the fingerprint without @", () => {
    const r = resolveDisplay({ signatureKey: KEY });
    expect(r.name).toBe("a1b2 c3d4");
    expect(r.name.includes("@")).toBe(false);
    expect(r.source).toBe("fingerprint");
    expect(r.fingerprint).toBe("a1b2 c3d4");
  });

  it("a blank rename is no rename", () => {
    const r = resolveDisplay({
      signatureKey: KEY,
      rename: "   ",
      profile: { displayName: "Alice", username: null },
    });
    expect(r.name).toBe("Alice");
  });
});

describe("resolveDisplay — a username is shown only when verified", () => {
  for (const verification of [undefined, "pending", "mismatch", "error"] as const) {
    it(`an unverified claim is not shown at all (${verification ?? "unknown"})`, () => {
      const r = resolveDisplay({
        signatureKey: KEY,
        profile: { displayName: null, username: "pavel" },
        verification,
      });
      expect(r.name).toBe("a1b2 c3d4");
      expect(r.username).toBeNull();
      expect(r.listLabel).toBe("a1b2 c3d4");
    });
  }

  it("a pending verification shows the name without the username", () => {
    const r = resolveDisplay({
      signatureKey: KEY,
      profile: { displayName: "Pavel", username: "pavel" },
      verification: "pending",
    });
    expect(r.listLabel).toBe("Pavel");
    expect(r.usernameNotVerified).toBe(false);
  });

  it("a mismatched claim is noted for the contact detail", () => {
    const r = resolveDisplay({
      signatureKey: KEY,
      profile: { displayName: "Pavel", username: "pavel" },
      verification: "mismatch",
    });
    expect(r.usernameNotVerified).toBe(true);
    expect(r.listLabel).toBe("Pavel");
  });

  it("a check that could not reach the directory is not noted: it reads as still running", () => {
    const r = resolveDisplay({
      signatureKey: KEY,
      profile: { displayName: "Pavel", username: "pavel" },
      verification: "error",
    });
    expect(r.usernameNotVerified).toBe(false);
    expect(r.username).toBeNull();
    expect(r.listLabel).toBe("Pavel");
  });

  it("no claim is no note", () => {
    const r = resolveDisplay({
      signatureKey: KEY,
      profile: { displayName: "Pavel", username: null },
      verification: "mismatch",
    });
    expect(r.usernameNotVerified).toBe(false);
  });
});

describe("fingerprint + own preview", () => {
  it("groups the first four bytes", () => {
    expect(fingerprintLabel(new Uint8Array([0, 1, 0xab, 0xff, 9]))).toBe("0001 abff");
  });
});

// ADR-0079 — a pending contact (an inviter added through their /i/ link, no
// one-to-one yet) is named in rosters from what the link states, below a
// rename and a received profile (seen live 2026-10-29: such a member read
// `Member` in a group roster while the contact list named them).
describe("resolvePerson — a pending contact's link statement", () => {
  const LINK = { displayName: "VG2", username: "vg2" };
  const seen: string[] = [];
  const verifiedFor =
    (state: "verified" | "pending" | "mismatch" | undefined) =>
    (username: string): "verified" | "pending" | "mismatch" | undefined => {
      seen.push(username);
      return state;
    };

  it("a key known only from a pending contact resolves to the link's name", () => {
    const r = resolvePerson({ signatureKey: KEY, linkStatement: LINK, verificationOf: verifiedFor("verified") });
    expect(r.name).toBe("VG2");
    expect(r.source).toBe("profile");
    expect(r.username).toBe("@vg2");
    expect(memberNameOf(r, "a test label")).toBe("VG2");
    expect(memberListLabelOf(r)).toBe("VG2 · @vg2");
  });

  it("the link's username is shown only once verified", () => {
    seen.length = 0;
    const r = resolvePerson({ signatureKey: KEY, linkStatement: LINK, verificationOf: verifiedFor("pending") });
    expect(seen).toEqual(["vg2"]);
    expect(r.name).toBe("VG2");
    expect(r.username).toBeNull();
    expect(memberListLabelOf(r)).toBe("VG2");

    const nameless = resolvePerson({
      signatureKey: KEY,
      linkStatement: { displayName: "", username: "vg2" },
      verificationOf: verifiedFor("mismatch"),
    });
    expect(nameless.source).toBe("fingerprint");
    expect(nameless.usernameNotVerified).toBe(true);
    expect(memberNameOf(nameless, "a test label")).toBe(UNNAMED_MEMBER());
  });

  it("a received profile wins over the link", () => {
    const r = resolvePerson({
      signatureKey: KEY,
      received: { displayName: "Vadim", username: null },
      linkStatement: LINK,
      verificationOf: verifiedFor("verified"),
    });
    expect(r.name).toBe("Vadim");
    expect(r.source).toBe("profile");
    expect(r.username).toBeNull();
  });

  it("a received profile with no name still wins: it is the current statement", () => {
    expect(profileInputOf(undefined, { displayName: null, username: null }, LINK)).toEqual({
      displayName: null,
      username: null,
    });
  });

  it("a local rename wins over the link, which is then not used at all", () => {
    const r = resolvePerson({
      signatureKey: KEY,
      rename: "Brother",
      linkStatement: LINK,
      verificationOf: verifiedFor("verified"),
    });
    expect(r.name).toBe("Brother");
    expect(r.source).toBe("rename");
    expect(r.theirName).toBeNull();
    expect(r.username).toBeNull();
  });

  it("once the pending contact is gone the key reads as `Member` again", () => {
    const r = resolvePerson({ signatureKey: KEY, verificationOf: verifiedFor("verified") });
    expect(r.source).toBe("fingerprint");
    expect(memberNameOf(r, "a test label")).toBe(UNNAMED_MEMBER());
  });

  it("a link that states nothing is no profile", () => {
    expect(profileInputOf(undefined, undefined, { displayName: "  ", username: "" })).toBeUndefined();
  });
});

describe("ADR-0098 — a deleted account and a username not recently confirmed", () => {
  const key = new Uint8Array(32).fill(9);
  const profile = { displayName: "Ann", username: "ann" };

  it("a deleted person is named Deleted account, over a rename and a verified username", () => {
    const r = resolveDisplay({
      signatureKey: key,
      rename: "My Ann",
      profile,
      verification: "verified",
      deleted: true,
      deletedName: "Deleted account",
      usernameShowable: true,
    });
    expect(r.name).toBe("Deleted account");
    expect(r.source).toBe("deleted");
    expect(r.username).toBeNull();
    expect(r.listLabel).toBe("Deleted account");
    expect(r.theirName).toBeNull();
    expect(r.usernameNotVerified).toBe(false);
    // Never the old name.
    expect(memberNameOf(r, "test")).toBe("Deleted account");
  });

  it("a verified username is hidden when it was not confirmed recently", () => {
    const shown = resolveDisplay({ signatureKey: key, profile, verification: "verified", usernameShowable: true });
    expect(shown.username).toBe("@ann");
    const hidden = resolveDisplay({ signatureKey: key, profile, verification: "verified", usernameShowable: false });
    expect(hidden.username).toBeNull();
    expect(hidden.listLabel).toBe("Ann");
  });

  it("resolvePerson carries both through", () => {
    const r = resolvePerson({
      signatureKey: key,
      received: profile,
      verificationOf: () => "verified",
      deleted: true,
      deletedName: "Deleted account",
    });
    expect(r.name).toBe("Deleted account");
    expect(r.username).toBeNull();
  });
});
