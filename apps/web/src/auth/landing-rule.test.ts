// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * No publish-back (`landing-rule.ts`, ADR-0073 section 4.2): what a landing
 * does with the identity the directory publishes, before it uploads
 * anything. The load-bearing property is that a different published key
 * never leads to an upload: it is either a known reset (a higher
 * generation: wipe) or an anomaly (block), never a heal.
 */
import { describe, expect, it } from "vitest";

import { landingMayUpload, landingSignsOut, landingVerdict } from "./landing-rule";

describe("landingVerdict", () => {
  it("lands and may upload when the published key is this device's", () => {
    const verdict = landingVerdict({ keyMatches: true, keyGeneration: 1n }, 1n);
    expect(verdict).toBe("matches");
    expect(landingMayUpload(verdict)).toBe(true);
    expect(landingSignsOut(verdict)).toBe(false);
  });

  it("a matching key lands whatever the generations say (a higher published generation with the same key cannot happen, and is not a reason to wipe)", () => {
    expect(landingVerdict({ keyMatches: true, keyGeneration: 5n }, 1n)).toBe("matches");
    expect(landingVerdict({ keyMatches: true, keyGeneration: 1n }, 5n)).toBe("matches");
  });

  it("publishes when nothing is published yet", () => {
    const verdict = landingVerdict(null, 1n);
    expect(verdict).toBe("not-published");
    expect(landingMayUpload(verdict)).toBe(true);
    expect(landingSignsOut(verdict)).toBe(false);
  });

  it("lands on local data and uploads nothing when the directory could not be read", () => {
    const verdict = landingVerdict(undefined, 1n);
    expect(verdict).toBe("unknown");
    expect(landingMayUpload(verdict)).toBe(false);
    expect(landingSignsOut(verdict)).toBe(false);
  });

  it("signs out and wipes on a different key of a higher generation: a known reset", () => {
    const verdict = landingVerdict({ keyMatches: false, keyGeneration: 2n }, 1n);
    expect(verdict).toBe("superseded");
    expect(landingMayUpload(verdict)).toBe(false);
    expect(landingSignsOut(verdict)).toBe(true);
  });

  it("blocks (wipe on the tap) on a different key at the same or a lower generation: an anomaly", () => {
    for (const published of [1n, 0n]) {
      const verdict = landingVerdict({ keyMatches: false, keyGeneration: published }, 1n);
      expect(verdict).toBe("mismatch");
      expect(landingMayUpload(verdict)).toBe(false);
      expect(landingSignsOut(verdict)).toBe(true);
    }
    expect(landingVerdict({ keyMatches: false, keyGeneration: 3n }, 4n)).toBe("mismatch");
  });

  it("never allows an upload over a different key", () => {
    for (const published of [0n, 1n, 2n, 100n]) {
      for (const own of [1n, 2n, 3n]) {
        expect(landingMayUpload(landingVerdict({ keyMatches: false, keyGeneration: published }, own))).toBe(false);
      }
    }
  });
});
