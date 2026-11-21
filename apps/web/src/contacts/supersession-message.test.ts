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

  it("names the contact and says only the outcome", () => {
    expect(supersessionRefusedMessage(null, "@carol")).toBe(
      "Couldn't confirm this is still @carol's account, so nothing was sent.",
    );
  });

  it("falls back to a subject-less sentence when no peer label is given", () => {
    const unnamed = "Couldn't confirm this is still the same account, so nothing was sent.";
    expect(supersessionRefusedMessage(false)).toBe(unnamed);
    expect(supersessionRefusedMessage(null, "")).toBe(unnamed);
  });

  it("does not tell the person which case it was: both refusals read the same", () => {
    expect(supersessionRefusedMessage(false, "@x")).toBe(supersessionRefusedMessage(null, "@x"));
    expect(supersessionRefusedMessage(false)).toBe(supersessionRefusedMessage(null));
  });

  it("explains no machinery (no key ceremony in the copy)", () => {
    for (const lr of [true, false, null] as const) {
      for (const label of ["@x", undefined]) {
        const msg = supersessionRefusedMessage(lr, label).toLowerCase();
        for (const word of ["identity", "transparency", "out of band", "verify", "compromise", "key"]) {
          expect(msg).not.toContain(word);
        }
      }
    }
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
