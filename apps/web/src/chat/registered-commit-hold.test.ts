// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 1 - the verdict, the backoff, the behind mark and the log
 * line. The same truth table is pinned on iOS (`RegisteredCommitHoldTests`)
 * and Android (`RegisteredCommitHoldTest`).
 */
import { describe, expect, it } from "vitest";

import {
  REGISTERED_COMMIT_MAX_ATTEMPTS,
  REGISTERED_COMMIT_RETRY_DELAYS_SECS,
  RegisteredCommitHolds,
  divergenceLine,
  exhaustionLine,
  failedApplyKind,
  failedRowLine,
  registeredCommitLeaving,
  registeredCommitVerdict,
} from "./registered-commit-hold";

/** Row 7967 is the commit the DS registered as leaving epoch 6. */
const REGISTERED: ReadonlyMap<bigint, number> = new Map([
  [7n, 7967],
  [8n, 8100],
]);
const HOLD = { isOwnEcho: false, rowId: 7967, local: 6n, registeredCommits: REGISTERED };

describe("registeredCommitVerdict", () => {
  it("holds the commit the DS registered as leaving this device's epoch", () => {
    expect(registeredCommitVerdict(HOLD)).toBe("hold");
    expect(registeredCommitVerdict({ ...HOLD, rowId: 8100, local: 7n })).toBe("hold");
  });

  it.each([
    ["an own echo", { ...HOLD, isOwnEcho: true }],
    ["no local epoch", { ...HOLD, local: undefined }],
    ["a read that named no commits", { ...HOLD, registeredCommits: undefined }],
    ["an empty list", { ...HOLD, registeredCommits: new Map<bigint, number>() }],
    // A commit-shaped row posted through the plain publish route.
    ["another row at the same epoch", { ...HOLD, rowId: 7966 }],
    ["the commit of the epoch after the next", { ...HOLD, rowId: 8100 }],
    ["a commit this device is already past", { ...HOLD, local: 7n }],
  ])("skips %s", (_name, input) => {
    expect(registeredCommitVerdict(input)).toBe("skip");
  });

  it("names the commit leaving an epoch", () => {
    expect(registeredCommitLeaving(REGISTERED, 6n)).toBe(7967);
    expect(registeredCommitLeaving(REGISTERED, 8n)).toBeUndefined();
    expect(registeredCommitLeaving(undefined, 6n)).toBeUndefined();
    expect(registeredCommitLeaving(REGISTERED, undefined)).toBeUndefined();
  });
});

function tracker(start = 1_000_000) {
  let now = start;
  const lines: { level: "warn" | "error"; line: string }[] = [];
  const holds = new RegisteredCommitHolds(
    () => now,
    {
      warn: (line) => lines.push({ level: "warn", line }),
      error: (line) => lines.push({ level: "error", line }),
    },
  );
  return {
    holds,
    lines,
    advance: (ms: number) => {
      now += ms;
    },
  };
}

const ROW = {
  id: 7967,
  isOwnEcho: false,
  sealed: 6n,
  after: 7n,
  local: 6n,
  latest: 8n,
  registeredCommits: REGISTERED,
  error: new Error("InvalidCommit(MissingProposal)"),
};

describe("RegisteredCommitHolds", () => {
  it("retries on the stated schedule and skips after the last attempt, marking the device behind", () => {
    const { holds, advance } = tracker();
    for (let attempt = 1; attempt < REGISTERED_COMMIT_MAX_ATTEMPTS; attempt += 1) {
      expect(holds.recordFailure("g", ROW)).toEqual({ kind: "held", attempt });
      const delayMs = (REGISTERED_COMMIT_RETRY_DELAYS_SECS[attempt - 1] ?? 0) * 1000;
      expect(holds.msUntilDue("g")).toBe(delayMs);
      advance(delayMs - 1);
      expect(holds.msUntilDue("g")).toBe(1);
      advance(1);
      expect(holds.msUntilDue("g")).toBe(0);
    }
    expect(holds.recordFailure("g", ROW)).toEqual({ kind: "exhausted" });
    expect(holds.heldRowId("g")).toBeUndefined();
    expect(holds.behindEpoch("g")).toBe(6n);
    expect(holds.msUntilDue("g")).toBe(0);
  });

  it("a different row starts its own count", () => {
    const { holds } = tracker();
    holds.recordFailure("g", ROW);
    holds.recordFailure("g", ROW);
    const other = new Map([[7n, 8000]]);
    expect(holds.recordFailure("g", { ...ROW, id: 8000, registeredCommits: other })).toEqual({
      kind: "held",
      attempt: 1,
    });
  });

  it("an applied row ends its hold; a moved epoch ends the hold and the behind mark", () => {
    const { holds } = tracker();
    holds.recordFailure("g", ROW);
    holds.recordApplied("g", ROW.id);
    expect(holds.heldRowId("g")).toBeUndefined();

    holds.recordFailure("g", ROW);
    holds.observeEpoch("g", 7n);
    expect(holds.heldRowId("g")).toBeUndefined();

    for (let i = 0; i < REGISTERED_COMMIT_MAX_ATTEMPTS; i += 1) holds.recordFailure("h", ROW);
    expect(holds.behindEpoch("h")).toBe(6n);
    holds.observeEpoch("h", 6n);
    expect(holds.behindEpoch("h")).toBe(6n);
    holds.observeEpoch("h", 7n);
    expect(holds.behindEpoch("h")).toBeUndefined();
  });

  it("a skipped row is logged and never held", () => {
    const { holds, lines } = tracker();
    expect(holds.recordFailure("g", { ...ROW, id: 7966, latest: undefined })).toEqual({ kind: "skipped" });
    expect(holds.heldRowId("g")).toBeUndefined();
    expect(lines).toEqual([
      {
        level: "warn",
        line: "chat row not applied: kind=missing-proposal sealed=6 after=7 local=6 latest=- verdict=skip attempt=-/8",
      },
    ]);
  });

  it("blocks journal repair while holding, and while behind at or above the noted epoch", () => {
    const { holds } = tracker();
    expect(holds.blocksRepair("g", 6n)).toBe(false);
    holds.recordFailure("g", ROW);
    expect(holds.blocksRepair("g", 6n)).toBe(true);
    for (let i = 1; i < REGISTERED_COMMIT_MAX_ATTEMPTS; i += 1) holds.recordFailure("g", ROW);
    expect(holds.blocksRepair("g", 6n)).toBe(true);
    expect(holds.blocksRepair("g", 7n)).toBe(false);
  });

  it("logs a refusal of a registered commit as a divergence", () => {
    const { holds, lines } = tracker();
    const refused = Object.assign(new Error("add proposal refused"), { refusalReason: "notOwner" });
    holds.recordFailure("g", { ...ROW, error: refused });
    expect(lines).toEqual([
      {
        level: "warn",
        line: "chat row not applied: kind=refused:notOwner sealed=6 after=7 local=6 latest=8 verdict=hold attempt=1/8",
      },
      {
        level: "error",
        line: "chat registered commit refused by this device's rules (refused:notOwner): the other devices applied it; a bug or version skew",
      },
    ]);
  });
});

describe("the log lines", () => {
  it("never carry a group id, a row id or the error text", () => {
    const groupIdB64 = "wUrjs9ZrtmYAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const rowId = 987654321;
    const registeredCommits = new Map([[7n, rowId]]);
    const { holds, lines } = tracker();
    const err = new Error(`WrongEpoch in ${groupIdB64} row ${rowId}`);
    for (let i = 0; i < REGISTERED_COMMIT_MAX_ATTEMPTS; i += 1) {
      holds.recordFailure(groupIdB64, { ...ROW, id: rowId, registeredCommits, error: err });
    }
    const all = [
      ...lines.map((l) => l.line),
      failedRowLine({ kind: "other", sealed: 1n, after: 2n, local: 1n, latest: 2n, verdict: "hold", attempt: 3 }),
      exhaustionLine(6n, 16n),
      divergenceLine("refused:notOwner"),
    ];
    expect(all.length).toBeGreaterThan(REGISTERED_COMMIT_MAX_ATTEMPTS);
    for (const line of all) {
      expect(line).not.toContain(groupIdB64);
      expect(line).not.toContain(String(rowId));
      expect(line).not.toContain("WrongEpoch in");
    }
    expect(lines.at(-1)?.line).toBe(
      "chat registered commit skipped after 8 attempts: this device is now behind in a conversation (local=6 latest=8)",
    );
  });

  it("classifies the failure from a fixed set", () => {
    expect(failedApplyKind(new Error("ValidationError(WrongEpoch)"))).toBe("wrong-epoch");
    expect(failedApplyKind(new Error("CannotDecryptOwnMessage"))).toBe("own-message");
    expect(failedApplyKind(new Error("InvalidCommit(MissingProposal)"))).toBe("missing-proposal");
    expect(failedApplyKind(new Error("AEAD decryption failed"))).toBe("decrypt");
    expect(failedApplyKind(new Error("something else"))).toBe("other");
  });
});
