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

import { fingerprintLabel, ownPreviewLabel, resolveDisplay } from "./resolver";

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

  it("previews how others see you", () => {
    expect(ownPreviewLabel("Alice Petrova", "alice")).toBe("Alice Petrova · @alice");
    expect(ownPreviewLabel(null, "alice")).toBe("@alice");
    expect(ownPreviewLabel("  Guest ", null)).toBe("Guest");
    expect(ownPreviewLabel(null, null)).toBeNull();
  });
});
