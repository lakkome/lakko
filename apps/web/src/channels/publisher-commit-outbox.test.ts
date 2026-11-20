// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-50, ADR-0090 section 3 — the durable record of a STAGED PublisherGroup
 * commit: the journal decides (merged on its acceptance, abandoned on a
 * 409, owed when unanswered), nothing is resealed from a commit that did
 * not merge, the steps after the merge run in order and each is recorded as
 * it lands; a restart re-sends a recorded stage, resumes a merged one and
 * drops one that never reached disk.
 */
import { describe, expect, it } from "vitest";

import {
  hasOwedPublisherCommit,
  owesJournal,
  parsePublisherCommitEntry,
  PUBLISHER_COMMIT_REFUSAL_CODES,
  publisherCommitBackoffMs,
  publisherCommitFailureLabel,
  publisherCommitFailureOf,
  PublisherCommitRetry,
  remainingSteps,
  replayVerdict,
  runPublisherCommit,
  type PublisherCommitEntry,
  type PublisherCommitPorts,
} from "./publisher-commit-outbox";
import { CHANNEL_DELETED, PUBLISHER_REFUSED } from "./api";

const APPROVE: PublisherCommitEntry = {
  id: "0a".repeat(16),
  kind: "approve",
  channelIdHex: "11".repeat(16),
  groupIdHex: "22".repeat(32),
  targetEpoch: "5",
  serverTargetEpoch: 8,
  commitB64: btoa("commit"),
  welcomeB64: btoa("welcome"),
  joinerKeyB64: btoa("j".repeat(32)),
  replyGrantB64: "Z3JhbnQ=",
  proofEpoch: "4",
  proofSeedB64: btoa("s".repeat(32)),
  journalLost: false,
  done: [],
};

const ROTATION: PublisherCommitEntry = {
  ...APPROVE,
  id: "0b".repeat(16),
  kind: "rotation",
  // ADR-0081: a rotation's commit reaches the journal with its epoch.
  commitB64: btoa("rotation"),
  welcomeB64: null,
  joinerKeyB64: null,
  replyGrantB64: null,
};

function ports(opts: { failAt?: string; serverEpoch?: number; lost?: boolean } = {}) {
  const calls: string[] = [];
  const proofs: (string | null)[] = [];
  const saved: PublisherCommitEntry[] = [];
  let finished: PublisherCommitEntry | null = null;
  const step = (name: string) => {
    calls.push(name);
    if (opts.failAt === name) throw new Error(`${name} failed`);
  };
  const p: PublisherCommitPorts = {
    async commitToJournal(commit, proof) {
      proofs.push(proof === null ? null : `${proof.epoch}:${new TextDecoder().decode(proof.seed)}`);
      step(`commit:${new TextDecoder().decode(commit)}`);
    },
    classifyCommitFailure: () => (opts.lost === true ? "lost" : "unknown"),
    async merge() {
      step("merge");
      return true;
    },
    async abandon() {
      step("abandon");
    },
    async handover(key) {
      step(`handover:${new TextDecoder().decode(key).slice(0, 1)}`);
    },
    async deliverWelcome(welcome, _key, grant) {
      step(`welcome:${new TextDecoder().decode(welcome)}:${grant ?? "none"}`);
    },
    async serverEpoch() {
      step("server-epoch");
      return opts.serverEpoch ?? 7;
    },
    async reseal(target) {
      step(`reseal:${target}`);
    },
    async appoint() {
      step("appoint");
    },
    async save(e) {
      saved.push(e);
    },
    async finish(e) {
      finished = e;
    },
  };
  return { p, calls, saved, proofs, finished: () => finished };
}

const ADMIT: PublisherCommitEntry = {
  ...APPROVE,
  id: "0c".repeat(16),
  kind: "admit",
  commitB64: btoa("admit"),
  welcomeB64: btoa("welcome"),
  joinerKeyB64: btoa("o".repeat(32)),
  replyGrantB64: null,
};

describe("running a recorded STAGED PublisherGroup commit", () => {
  it("an approve: journal, merge, reseal, then the Welcome and the appointment", async () => {
    const t = ports();
    expect(await runPublisherCommit(APPROVE, t.p)).toBe("landed");
    expect(t.calls).toEqual([
      "commit:commit",
      "merge",
      "server-epoch",
      "reseal:8",
      "welcome:welcome:Z3JhbnQ=",
      "appoint",
    ]);
    expect(t.saved.map((e) => e.done)).toEqual([
      ["journal"],
      ["journal", "reseal"],
      ["journal", "reseal", "welcome"],
      ["journal", "reseal", "welcome", "appoint"],
    ]);
    expect(t.finished()?.id).toBe(APPROVE.id);
  });

  it("an admission of devices: journal, merge, reseal, the Welcome, the hand-over reset", async () => {
    const t = ports();
    expect(await runPublisherCommit(ADMIT, t.p)).toBe("landed");
    expect(t.calls).toEqual(["commit:admit", "merge", "server-epoch", "reseal:8", "welcome:welcome:none", "handover:o"]);
  });

  it("a rotation sends its commit, merges on the answer and reseals; it sends no Welcome", async () => {
    const t = ports();
    await runPublisherCommit(ROTATION, t.p);
    expect(t.calls).toEqual(["commit:rotation", "merge", "server-epoch", "reseal:8"]);
  });

  it("a failed step after the merge leaves what landed recorded, and the next run resumes there", async () => {
    const first = ports({ failAt: "reseal:8" });
    await expect(runPublisherCommit(APPROVE, first.p)).rejects.toThrow(/reseal/);
    expect(first.finished()).toBeNull();
    const recorded = first.saved.at(-1)!;
    expect(remainingSteps(recorded)).toEqual(["reseal", "welcome", "appoint"]);
    const second = ports();
    await runPublisherCommit(recorded, second.p);
    expect(second.calls).toEqual(["server-epoch", "reseal:8", "welcome:welcome:Z3JhbnQ=", "appoint"]);
    expect(second.finished()?.done).toEqual(["journal", "reseal", "welcome", "appoint"]);
  });

  it("a reseal the channels service already holds is not sent again", async () => {
    const t = ports({ serverEpoch: 8 });
    await runPublisherCommit({ ...ROTATION, done: ["journal"] }, t.p);
    expect(t.calls).toEqual(["server-epoch"]);
    expect(t.finished()?.done).toEqual(["journal", "reseal"]);
  });

  it("the journal step is proven under the recorded seed of the epoch the commit left", async () => {
    const t = ports();
    await runPublisherCommit(APPROVE, t.p);
    expect(t.proofs).toEqual([`4:${"s".repeat(32)}`]);
  });

  it("C-90: a commit the DS did not answer stays owed; nothing is merged or resealed", async () => {
    for (const entry of [ROTATION, APPROVE, ADMIT]) {
      const t = ports({ failAt: `commit:${atob(entry.commitB64 ?? "")}` });
      expect(await runPublisherCommit(entry, t.p)).toBe("owed");
      expect(t.calls).toEqual([`commit:${atob(entry.commitB64 ?? "")}`]);
      expect(t.saved).toEqual([]);
      expect(t.finished()).toBeNull();
    }
  });

  it("a commit another owner device beat (409) is abandoned; nothing else of it runs", async () => {
    const t = ports({ failAt: "commit:rotation", lost: true });
    expect(await runPublisherCommit(ROTATION, t.p)).toBe("lost");
    expect(t.calls).toEqual(["commit:rotation", "abandon"]);
    expect(t.finished()?.journalLost).toBe(true);
  });

  it("an approve that lost delivers no Welcome, appoints nobody and reseals nothing", async () => {
    const t = ports({ failAt: "commit:commit", lost: true });
    expect(await runPublisherCommit(APPROVE, t.p)).toBe("lost");
    expect(t.calls).toEqual(["commit:commit", "abandon"]);
  });

  it("a publisher removal runs like a rotation", async () => {
    const REMOVAL: PublisherCommitEntry = { ...ROTATION, kind: "removal" };
    const t = ports();
    await runPublisherCommit(REMOVAL, t.p);
    expect(t.calls).toEqual(["commit:rotation", "merge", "server-epoch", "reseal:8"]);
    expect(parsePublisherCommitEntry(JSON.parse(JSON.stringify(REMOVAL)))?.kind).toBe("removal");
  });

  it("a failed appointment never holds the entry", async () => {
    const t = ports({ failAt: "appoint" });
    await runPublisherCommit({ ...APPROVE, done: ["journal", "reseal", "welcome"] }, t.p);
    expect(t.finished()).not.toBeNull();
  });
});

describe("a restart", () => {
  it("resumes a merged stage, re-sends a recorded one, drops one that never reached disk", () => {
    // APPROVE: N = 4, target 5.
    expect(replayVerdict(APPROVE, 5n)).toBe("merged");
    expect(replayVerdict(APPROVE, 6n)).toBe("merged");
    expect(replayVerdict(APPROVE, 4n, true)).toBe("resend");
    expect(replayVerdict(APPROVE, 4n, false)).toBe("discard");
    expect(replayVerdict(APPROVE, 3n, true)).toBe("discard");
    expect(replayVerdict(APPROVE, null)).toBe("discard");
  });
});

describe("the persisted entry", () => {
  it("round-trips through JSON and refuses a malformed one", () => {
    const back = parsePublisherCommitEntry(JSON.parse(JSON.stringify({ ...APPROVE, done: ["journal"] })));
    expect(back).toEqual({ ...APPROVE, done: ["journal"] });
    expect(parsePublisherCommitEntry({ ...APPROVE, kind: "other" })).toBeNull();
    expect(parsePublisherCommitEntry({ ...APPROVE, groupIdHex: "zz" })).toBeNull();
    expect(parsePublisherCommitEntry({ ...APPROVE, targetEpoch: "-1" })).toBeNull();
    expect(parsePublisherCommitEntry({ ...APPROVE, done: ["sideways"] })).toBeNull();
    // An approve needs its commit, its Welcome and the joiner.
    expect(parsePublisherCommitEntry({ ...APPROVE, commitB64: null })).toBeNull();
    expect(parsePublisherCommitEntry(ROTATION)).toEqual(ROTATION);
    expect(parsePublisherCommitEntry({ ...ADMIT, done: ["journal", "handover"] })?.kind).toBe("admit");
  });
});

describe("retry of recorded commits (2026-11-20)", () => {
  const REFUSAL = { status: 401, code: "owner-proof-required", detail: "this route needs a valid channel owner proof" };
  const merged = (n: number, channelIdHex: string): PublisherCommitEntry => ({
    ...ROTATION,
    id: n.toString(16).padStart(32, "0"),
    channelIdHex,
    done: ["journal"],
  });

  it("a refusal is the owner proof or a deleted channel, and nothing else", () => {
    expect(publisherCommitFailureOf(REFUSAL)).toBe("refused");
    expect(publisherCommitFailureOf({ status: 410, code: "channel-deleted", detail: "" })).toBe("refused");
    expect(publisherCommitFailureOf(new Error("wrapped", { cause: REFUSAL }))).toBe("refused");
    expect(publisherCommitFailureOf({ status: 503, code: "internal", detail: "" })).toBe("transient");
    expect(publisherCommitFailureOf({ status: 409, code: "stale-target-epoch", detail: "" })).toBe("transient");
    expect(publisherCommitFailureOf(new TypeError("Failed to fetch"))).toBe("transient");
    expect(publisherCommitFailureLabel(REFUSAL)).toBe("401 owner-proof-required");
    expect([...PUBLISHER_COMMIT_REFUSAL_CODES].sort()).toEqual([CHANNEL_DELETED, PUBLISHER_REFUSED].sort());
  });

  it("a pile of refused entries of one channel costs one request and is held until the next start", async () => {
    let now = 1_000_000;
    const retry = new PublisherCommitRetry(() => now);
    const entries = Array.from({ length: 172 }, (_, i) => merged(i, "11".repeat(16)));
    let calls = 0;
    const refuse = async (): Promise<"landed"> => {
      calls += 1;
      throw REFUSAL;
    };
    await retry.replay(entries, refuse);
    expect(calls).toBe(1);
    expect(retry.hold("11".repeat(16))?.refused).toBe(true);
    now += 24 * 3_600_000;
    await retry.replay(entries, refuse);
    expect(calls).toBe(1);
    retry.reset();
    await retry.replay(entries, async () => {
      calls += 1;
      return "landed";
    });
    expect(calls).toBe(1 + entries.length);
  });

  it("a refused channel does not hold another channel", async () => {
    const retry = new PublisherCommitRetry(() => 0);
    const a = merged(1, "11".repeat(16));
    const b = merged(2, "33".repeat(16));
    const c = merged(3, "11".repeat(16));
    const ran: string[] = [];
    await retry.replay([a, b, c], async (e) => {
      ran.push(e.id);
      if (e.channelIdHex === a.channelIdHex) throw REFUSAL;
      return "landed";
    });
    expect(ran).toEqual([a.id, b.id]);
    expect(retry.mayAttempt(b.channelIdHex)).toBe(true);
  });

  it("a transient failure (or an unanswered journal) waits 30 s, doubling to 10 minutes", async () => {
    let now = 5_000_000;
    const retry = new PublisherCommitRetry(() => now);
    const entries = [merged(1, "11".repeat(16)), merged(2, "11".repeat(16))];
    let calls = 0;
    const owed = async (): Promise<"owed"> => {
      calls += 1;
      return "owed";
    };
    await retry.replay(entries, owed);
    expect(calls).toBe(1);
    now += 29_999;
    await retry.replay(entries, owed);
    expect(calls).toBe(1);
    now += 1;
    await retry.replay(entries, owed);
    expect(calls).toBe(2);
    now += 59_999;
    expect(retry.mayAttempt("11".repeat(16))).toBe(false);
    now += 1;
    expect(retry.mayAttempt("11".repeat(16))).toBe(true);
    expect(publisherCommitBackoffMs(1)).toBe(30_000);
    expect(publisherCommitBackoffMs(40)).toBe(600_000);
    await retry.replay(entries, async () => {
      calls += 1;
      return "landed";
    });
    expect(retry.hold("11".repeat(16))).toBeUndefined();
    expect(calls).toBe(4);
  });

  it("no rotation is staged while a commit of the channel owes its journal step or reseal", () => {
    const owed = merged(1, "11".repeat(16));
    expect(hasOwedPublisherCommit("11".repeat(16).toUpperCase(), [owed])).toBe(true);
    expect(hasOwedPublisherCommit("33".repeat(16), [owed])).toBe(false);
    expect(hasOwedPublisherCommit("11".repeat(16), [{ ...owed, done: ["journal", "reseal"] }])).toBe(false);
    // An approve owing only its Welcome and appointment does not hold it.
    expect(hasOwedPublisherCommit(APPROVE.channelIdHex, [{ ...APPROVE, done: ["journal", "reseal"] }])).toBe(false);
    expect(hasOwedPublisherCommit(APPROVE.channelIdHex, [APPROVE])).toBe(true);
    expect(hasOwedPublisherCommit(APPROVE.channelIdHex, [{ ...APPROVE, journalLost: true }])).toBe(false);
  });
});

describe("round 2 (2026-11-20): the journal step is never held", () => {
  const REFUSAL = { status: 401, code: "owner-proof-required", detail: "" };

  it("an entry owing its journal step runs every pass whatever the hold, and an unanswered one sets none", async () => {
    const retry = new PublisherCommitRetry(() => 0);
    retry.noteFailure(APPROVE.channelIdHex, REFUSAL);
    expect(owesJournal(APPROVE)).toBe(true);
    let calls = 0;
    for (let i = 0; i < 3; i += 1) {
      await retry.replay(
        [APPROVE],
        async () => {
          calls += 1;
          return "owed";
        },
        () => APPROVE,
      );
    }
    expect(calls).toBe(3);
    expect(retry.hold(APPROVE.channelIdHex)?.failures).toBe(1);
    // The journal landed and the reseal was refused: that holds.
    const fresh = new PublisherCommitRetry(() => 0);
    await fresh.replay(
      [APPROVE],
      async () => {
        throw REFUSAL;
      },
      () => ({ ...APPROVE, done: ["journal"] }),
    );
    expect(fresh.hold(APPROVE.channelIdHex)?.refused).toBe(true);
  });
});
