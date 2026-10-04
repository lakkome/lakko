// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * KT-log S4 — unit tests for the reset-aware `SupersessionRefused`
 * message mapping. This is the pure `(loggedReset, peerLabel)` → copy
 * function the Contacts route renders; testing it here covers the
 * three-way distinction (reset / no-reset / unknown) without dragging
 * the SolidJS route + JSX transform into the plugin-free vitest config.
 */
import { describe, expect, it } from "vitest";
import { supersessionRefusedMessage } from "./supersession-message";

describe("supersessionRefusedMessage (KT-log S4)", () => {
  it("loggedReset=true → points at the recorded reset state (ADR-0065 phase 2)", () => {
    // A change the log confirms as a reset is a STATE of the contact
    // (UX 16.1): the action's line says what to do, not what the log says.
    const msg = supersessionRefusedMessage(true, "@alice");
    expect(msg).toBe("Verify @alice's new safety number first.");
    // It must NOT frame a confirmed owner reset as a compromise.
    expect(msg).not.toContain("compromise");
  });

  it("loggedReset=false → possible-compromise copy", () => {
    const msg = supersessionRefusedMessage(false, "@bob");
    expect(msg).toContain("@bob");
    expect(msg).toContain("does NOT show an owner reset");
    expect(msg).toContain("possible account compromise");
    expect(msg).toContain("Verify out of band");
  });

  it("loggedReset=null → neutral could-not-confirm copy", () => {
    const msg = supersessionRefusedMessage(null, "@carol");
    expect(msg).toContain("@carol");
    expect(msg).toContain("could not be confirmed");
    expect(msg).toContain("Verify out of band");
    // The neutral message must not assert a compromise OR a reset.
    expect(msg).not.toContain("compromise");
    expect(msg).not.toContain("owner-initiated reset");
  });

  it("falls back to a subject-less sentence when no peer label is given", () => {
    expect(supersessionRefusedMessage(true)).toBe("Verify the new safety number first.");
    expect(supersessionRefusedMessage(true, "")).toBe("Verify the new safety number first.");
    expect(supersessionRefusedMessage(false)).toContain("This contact");
    expect(supersessionRefusedMessage(null, "")).toContain("This contact");
  });

  it("the three verdicts produce three DISTINCT messages", () => {
    const a = supersessionRefusedMessage(true, "@x");
    const b = supersessionRefusedMessage(false, "@x");
    const c = supersessionRefusedMessage(null, "@x");
    expect(new Set([a, b, c]).size).toBe(3);
  });

  it("contains no emoji / pictographs (copy-style invariant)", () => {
    for (const lr of [true, false, null] as const) {
      const msg = supersessionRefusedMessage(lr, "@x");
      // Match any non-ASCII pictograph; the em-dash (U+2014) is allowed.
      // eslint-disable-next-line no-control-regex
      expect(/[\u{1F000}-\u{1FAFF}\u{2600}-\u{27BF}]/u.test(msg)).toBe(false);
    }
  });
});
