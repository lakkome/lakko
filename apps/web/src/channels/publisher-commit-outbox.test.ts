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
  parsePublisherCommitEntry,
  remainingSteps,
  replayVerdict,
  runPublisherCommit,
  type PublisherCommitEntry,
  type PublisherCommitPorts,
} from "./publisher-commit-outbox";

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
