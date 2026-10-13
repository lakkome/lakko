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
  it("loggedReset=true is never a copy of its own: it renders as unconfirmed (ADR-0073 R.2)", () => {
    // A change that IS an attested reset is accepted silently and reaches no
    // refusal; if the marker ever arrives here, nothing says "reset".
    expect(supersessionRefusedMessage(true, "@alice")).toBe(
      supersessionRefusedMessage(null, "@alice"),
    );
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
    expect(supersessionRefusedMessage(false)).toContain("This contact");
    expect(supersessionRefusedMessage(null, "")).toContain("This contact");
  });

  it("the two refusals produce two DISTINCT messages", () => {
    const b = supersessionRefusedMessage(false, "@x");
    const c = supersessionRefusedMessage(null, "@x");
    expect(b).not.toBe(c);
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
