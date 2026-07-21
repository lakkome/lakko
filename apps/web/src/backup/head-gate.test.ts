// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  BACKUP_HEAD_STALENESS_SECS_DEFAULT,
  backupHeadStalenessMillis,
  backupHeadStalenessSecs,
  NO_FACTS,
  restoreVerdict,
  type RestoreGateFacts,
} from "./head-gate";

/** Spread over [`NO_FACTS`] for a terse per-case fact record. */
const facts = (over: Partial<RestoreGateFacts>): RestoreGateFacts => ({
  ...NO_FACTS,
  ...over,
});

describe("restoreVerdict — the ADR-0037 §6 three-case policy", () => {
  it("absent head + NO floor (zero-anchor fresh install) → fail-open", () => {
    const v = restoreVerdict(facts({ framed: false, hasFloor: false }));
    expect(v.kind).toBe("fail-open");
  });

  it("F4: absent head + floor + byte-identical envelope → proceed", () => {
    // A returning client whose head was benignly stripped in transit but the
    // envelope byte-matches the last known one — safe to proceed.
    const v = restoreVerdict(
      facts({ framed: false, hasFloor: true, absentHashMatchesFloor: true }),
    );
    expect(v.kind).toBe("proceed");
  });

  it("F4: absent head + floor + DIFFERENT envelope → stale-override", () => {
    // Head-stripping downgrade against head-protected history — refuse unless
    // the user explicitly acknowledges (never fail-open for a returning client).
    const v = restoreVerdict(
      facts({ framed: false, hasFloor: true, absentHashMatchesFloor: false }),
    );
    expect(v.kind).toBe("stale-override");
  });

  it("F5a: corrupt floor → stale-override (never demote to the fresh path)", () => {
    const v = restoreVerdict(facts({ floorCorrupt: true }));
    expect(v.kind).toBe("stale-override");
  });

  it("F5a: corrupt floor dominates even a framed blob → stale-override", () => {
    const v = restoreVerdict(
      facts({ floorCorrupt: true, framed: true, hasFloor: false }),
    );
    expect(v.kind).toBe("stale-override");
  });

  it("truncated / corrupt framed blob (parse error) → hard-fail", () => {
    // parseError dominates even a framed blob with a floor.
    const v = restoreVerdict(facts({ parseError: true, framed: true, hasFloor: true }));
    expect(v.kind).toBe("hard-fail");
  });

  it("returning client: invalid signature / account binding → hard-fail", () => {
    const v = restoreVerdict(
      facts({ framed: true, hasFloor: true, bindingInvalid: true }),
    );
    expect(v.kind).toBe("hard-fail");
  });

  it("returning client: envelope-hash mismatch → hard-fail", () => {
    const v = restoreVerdict(
      facts({ framed: true, hasFloor: true, hashMismatch: true }),
    );
    expect(v.kind).toBe("hard-fail");
  });

  it("returning client: version below the persisted floor → stale-override", () => {
    const v = restoreVerdict(
      facts({ framed: true, hasFloor: true, versionBelowFloor: true }),
    );
    expect(v.kind).toBe("stale-override");
  });

  it("returning client: valid head at or above the floor → proceed", () => {
    const v = restoreVerdict(facts({ framed: true, hasFloor: true }));
    expect(v.kind).toBe("proceed");
  });

  it("fresh install: invalid head → hard-fail", () => {
    const v = restoreVerdict(
      facts({ framed: true, hasFloor: false, freshInvalid: true }),
    );
    expect(v.kind).toBe("hard-fail");
  });

  it("fresh install: signed_at older than the freshness window → stale-override", () => {
    const v = restoreVerdict(
      facts({ framed: true, hasFloor: false, freshStale: true }),
    );
    expect(v.kind).toBe("stale-override");
  });

  it("fresh install: valid + fresh head → proceed", () => {
    const v = restoreVerdict(facts({ framed: true, hasFloor: false }));
    expect(v.kind).toBe("proceed");
  });

  it("branch order: parseError beats floorCorrupt → hard-fail", () => {
    // A truncated framed blob is a hard tamper regardless of the anchor state.
    const v = restoreVerdict(
      facts({ parseError: true, floorCorrupt: true, framed: true }),
    );
    expect(v.kind).toBe("hard-fail");
  });

  it("branch order: floorCorrupt beats the framed / absent branches → stale-override", () => {
    // floorCorrupt short-circuits before the framed hard-fail checks; a
    // corrupt anchor is always the user-acknowledged stale path (fail-closed).
    const v = restoreVerdict(
      facts({
        floorCorrupt: true,
        framed: true,
        hasFloor: false,
        bindingInvalid: true,
        versionBelowFloor: true,
      }),
    );
    expect(v.kind).toBe("stale-override");
  });

  it("hard-fail dominates a returning invalid+stale head (fail-closed order)", () => {
    // Both bindingInvalid and versionBelowFloor set — the hard tamper wins over
    // the overridable stale case (never let an override escape a bad signature).
    const v = restoreVerdict(
      facts({
        framed: true,
        hasFloor: true,
        bindingInvalid: true,
        versionBelowFloor: true,
      }),
    );
    expect(v.kind).toBe("hard-fail");
  });
});

describe("backupHeadStalenessSecs / Millis", () => {
  it("defaults to 30 days when the env var is unset", () => {
    // The vitest env carries no VITE_LAKKO_BACKUP_HEAD_STALENESS_SECS override.
    expect(backupHeadStalenessSecs()).toBe(BACKUP_HEAD_STALENESS_SECS_DEFAULT);
    expect(BACKUP_HEAD_STALENESS_SECS_DEFAULT).toBe(2_592_000);
  });

  it("millis is secs * 1000 as a bigint", () => {
    expect(backupHeadStalenessMillis()).toBe(
      BigInt(BACKUP_HEAD_STALENESS_SECS_DEFAULT) * 1000n,
    );
  });
});
