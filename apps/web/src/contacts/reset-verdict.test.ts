// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 - the acceptance rule's whole table (`reset-verdict.ts`).
 */
import { describe, expect, it } from "vitest";

import {
  logOffAcceptsReset,
  resetRepinMarker,
  resetVerdict,
  type AttestCheck,
  type LogCheck,
  type ResetVerdictInput,
} from "./reset-verdict";

const VERIFIED: AttestCheck = { kind: "verified", keyGeneration: 2, signedAtMs: 1_700_000_000_000 };
const OFF: LogCheck = { status: "off", isReset: null };

function input(over: Partial<ResetVerdictInput> = {}): ResetVerdictInput {
  return {
    heldKeyB64: "old",
    acceptedGeneration: 1,
    fetchedKeyB64: "new",
    bundleGeneration: 2,
    attest: VERIFIED,
    log: OFF,
    ...over,
  };
}

describe("resetVerdict (ADR-0073 R.2)", () => {
  it("accepts a verified attestation at a higher generation with the log off (D3)", () => {
    expect(logOffAcceptsReset()).toBe(true);
    expect(resetVerdict(input())).toEqual({
      kind: "accepted",
      keyGeneration: 2,
      signedAtMs: 1_700_000_000_000,
    });
  });

  it("accepts when the log confirms the key as a reset", () => {
    expect(resetVerdict(input({ log: { status: "confirmed", isReset: true } })).kind).toBe("accepted");
  });

  it("row 1: no attestation is refused, whatever the log says", () => {
    for (const log of [
      OFF,
      { status: "confirmed", isReset: true },
      { status: "unreachable", isReset: null },
      { status: "absent", isReset: null },
    ] as LogCheck[]) {
      expect(resetVerdict(input({ attest: { kind: "absent" }, log }))).toEqual({
        kind: "refused",
        why: "unconfirmed",
      });
    }
  });

  it("row 1: no attestation and a log that shows NO reset is `unexplained`", () => {
    expect(
      resetVerdict(input({ attest: { kind: "absent" }, log: { status: "confirmed", isReset: false } })),
    ).toEqual({ kind: "refused", why: "unexplained" });
  });

  it("row 2: an attestation that fails a binding is refused", () => {
    expect(resetVerdict(input({ attest: { kind: "invalid" } }))).toEqual({
      kind: "refused",
      why: "unconfirmed",
    });
  });

  it("row 3: no attest key is no verdict, not a refusal", () => {
    expect(resetVerdict(input({ attest: { kind: "no-key" } }))).toEqual({ kind: "no-verdict" });
  });

  it("row 4: the attested generation must equal the bundle's", () => {
    expect(resetVerdict(input({ bundleGeneration: 3 })).kind).toBe("refused");
  });

  it("row 4: a generation not above the accepted one is refused (no replay, no rollback)", () => {
    expect(resetVerdict(input({ acceptedGeneration: 2 })).kind).toBe("refused");
    expect(resetVerdict(input({ acceptedGeneration: 5 })).kind).toBe("refused");
  });

  it("row 5: the log confirms the key WITHOUT a reset: refused, `unexplained`", () => {
    expect(resetVerdict(input({ log: { status: "confirmed", isReset: false } }))).toEqual({
      kind: "refused",
      why: "unexplained",
    });
    // An unreadable marker on a confirmed entry is not a reset either.
    expect(resetVerdict(input({ log: { status: "confirmed", isReset: null } }))).toEqual({
      kind: "refused",
      why: "unconfirmed",
    });
  });

  it("row 6: a log mismatch or rollback is refused", () => {
    for (const status of ["mismatch", "rollback"] as const) {
      expect(resetVerdict(input({ log: { status, isReset: null } }))).toEqual({
        kind: "refused",
        why: "unconfirmed",
      });
    }
  });

  it("row 7: an unreachable log or an absent entry is no verdict", () => {
    for (const status of ["unreachable", "absent"] as const) {
      expect(resetVerdict(input({ log: { status, isReset: null } }))).toEqual({ kind: "no-verdict" });
    }
  });

  it("an unchanged key is nothing to judge", () => {
    expect(resetVerdict(input({ fetchedKeyB64: "old" }))).toEqual({ kind: "no-verdict" });
  });

  it("a first pin with no recorded generation accepts generation 1 and above only above 0", () => {
    expect(
      resetVerdict(
        input({
          acceptedGeneration: 0,
          bundleGeneration: 1,
          attest: { kind: "verified", keyGeneration: 1, signedAtMs: 5 },
        }),
      ).kind,
    ).toBe("accepted");
  });
});

describe("resetRepinMarker", () => {
  it("never moves the marker backwards", () => {
    expect(resetRepinMarker(0, 5_000)).toBe(5);
    expect(resetRepinMarker(9_999_999_999, 5_000)).toBe(9_999_999_999);
  });
});
