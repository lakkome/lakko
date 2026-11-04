// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { memberListLabelOf, memberNameOf, ownAvatarLabel, realNameOf, UNNAMED_MEMBER } from "./member-label";
import { resolveDisplay } from "./resolver";

const key = new Uint8Array(32).fill(0xa1);

describe("member labels", () => {
  it("keep a resolved name", () => {
    const r = resolveDisplay({ signatureKey: key, rename: "Anna" });
    expect(memberNameOf(r)).toBe("Anna");
    expect(memberListLabelOf(r)).toBe("Anna");
  });

  it("keep a verified username beside the name in a list", () => {
    const r = resolveDisplay({
      signatureKey: key,
      profile: { displayName: "Anna", username: "anna" },
      verification: "verified",
    });
    expect(memberListLabelOf(r)).toBe("Anna · @anna");
    expect(memberNameOf(r)).toBe("Anna");
  });

  it("never show a fingerprint", () => {
    const r = resolveDisplay({ signatureKey: key });
    expect(r.source).toBe("fingerprint");
    expect(memberNameOf(r)).toBe(UNNAMED_MEMBER());
    expect(memberListLabelOf(r)).toBe(UNNAMED_MEMBER());
    expect(memberListLabelOf(r)).not.toMatch(/[0-9a-f]{4} [0-9a-f]{4}/);
  });
});

describe("realNameOf — what a snapshot, a saved message or a notification label may carry", () => {
  it("a rename, a profile name, a verified username", () => {
    expect(realNameOf(resolveDisplay({ signatureKey: key, rename: "Mum" }))).toBe("Mum");
    expect(realNameOf(resolveDisplay({ signatureKey: key, profile: { displayName: "VG3", username: null } }))).toBe("VG3");
    expect(
      realNameOf(
        resolveDisplay({ signatureKey: key, profile: { displayName: null, username: "vg3" }, verification: "verified" }),
      ),
    ).toBe("@vg3");
  });

  it("nothing known: no name at all, neither the placeholder word nor the fingerprint", () => {
    const r = resolveDisplay({ signatureKey: key });
    expect(realNameOf(r)).toBeNull();
    expect(realNameOf(resolveDisplay({ signatureKey: key, profile: { displayName: null, username: "vg3" } }))).toBeNull();
  });
});

describe("the own avatar's letters", () => {
  it("come from the display name when there is one", () => {
    expect(ownAvatarLabel("Tuuli Aho", "t4x123")).toBe("Tuuli Aho");
  });

  it("come from the username only when there is no name", () => {
    expect(ownAvatarLabel(null, "t4x123")).toBe("t4x123");
    expect(ownAvatarLabel("  ", "t4x123")).toBe("t4x123");
  });

  it("are a question mark for a guest with neither", () => {
    expect(ownAvatarLabel(null, "")).toBe("?");
  });
});
