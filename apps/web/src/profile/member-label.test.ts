// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { memberListLabelOf, memberNameOf, UNNAMED_MEMBER } from "./member-label";
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
