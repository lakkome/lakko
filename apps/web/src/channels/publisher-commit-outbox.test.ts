// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-50 — the durable record of a PublisherGroup commit: the steps run in
 * order and each is recorded as it lands; a failure leaves the entry with
 * what landed, and the next run resumes there; a restart drops an entry whose
 * group never reached its target epoch; a reseal already landed on the
 * channels service is not sent twice.
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
  done: [],
};

const ROTATION: PublisherCommitEntry = {
  ...APPROVE,
  id: "0b".repeat(16),
  kind: "rotation",
  commitB64: null,
  welcomeB64: null,
  joinerKeyB64: null,
  replyGrantB64: null,
};

function ports(opts: { failAt?: string; serverEpoch?: number } = {}) {
  const calls: string[] = [];
  const saved: PublisherCommitEntry[] = [];
  let finished: PublisherCommitEntry | null = null;
  const step = (name: string) => {
    calls.push(name);
    if (opts.failAt === name) throw new Error(`${name} failed`);
  };
  const p: PublisherCommitPorts = {
    async registerEpoch() {
      step("register");
    },
    async publishCommit(commit) {
      step(`publish:${new TextDecoder().decode(commit)}`);
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
  return { p, calls, saved, finished: () => finished };
}

describe("running a recorded PublisherGroup commit", () => {
  it("an approve runs the journal, the Welcome, the reseal and the appointment in order", async () => {
    const t = ports();
    await runPublisherCommit(APPROVE, t.p);
    expect(t.calls).toEqual([
      "register",
      "publish:commit",
      "welcome:welcome:Z3JhbnQ=",
      "server-epoch",
      "reseal:8",
      "appoint",
    ]);
    expect(t.saved.map((e) => e.done)).toEqual([
      ["journal"],
      ["journal", "welcome"],
      ["journal", "welcome", "reseal"],
      ["journal", "welcome", "reseal", "appoint"],
    ]);
    expect(t.finished()?.id).toBe(APPROVE.id);
  });

  it("a rotation registers the epoch and reseals; it publishes no commit and no Welcome", async () => {
    const t = ports();
    await runPublisherCommit(ROTATION, t.p);
    expect(t.calls).toEqual(["register", "server-epoch", "reseal:8"]);
  });

  it("a failed step leaves what landed recorded, and the next run resumes there", async () => {
    const first = ports({ failAt: "reseal:8" });
    await expect(runPublisherCommit(APPROVE, first.p)).rejects.toThrow(/reseal/);
    expect(first.finished()).toBeNull();
    const recorded = first.saved.at(-1)!;
    expect(remainingSteps(recorded)).toEqual(["reseal", "appoint"]);
    const second = ports();
    await runPublisherCommit(recorded, second.p);
    expect(second.calls).toEqual(["server-epoch", "reseal:8", "appoint"]);
    expect(second.finished()?.done).toEqual(["journal", "welcome", "reseal", "appoint"]);
  });

  it("a reseal the channels service already holds is not sent again", async () => {
    const t = ports({ serverEpoch: 8 });
    await runPublisherCommit({ ...ROTATION, done: ["journal"] }, t.p);
    expect(t.calls).toEqual(["server-epoch"]);
    expect(t.finished()?.done).toEqual(["journal", "reseal"]);
  });

  it("a refused epoch registration does not hold a rotation, which publishes nothing", async () => {
    const t = ports({ failAt: "register" });
    await runPublisherCommit(ROTATION, t.p);
    expect(t.calls).toEqual(["register", "server-epoch", "reseal:8"]);
    expect(t.finished()?.done).toEqual(["journal", "reseal"]);
  });

  it("a refused epoch registration stops an approve before its commit is published", async () => {
    const t = ports({ failAt: "register" });
    await expect(runPublisherCommit(APPROVE, t.p)).rejects.toThrow(/register/);
    expect(t.calls).toEqual(["register"]);
    expect(t.saved).toEqual([]);
    expect(t.finished()).toBeNull();
  });

  it("a failed appointment never holds the entry", async () => {
    const t = ports({ failAt: "appoint" });
    await runPublisherCommit({ ...APPROVE, done: ["journal", "welcome", "reseal"] }, t.p);
    expect(t.finished()).not.toBeNull();
  });
});

describe("a restart", () => {
  it("drops an entry whose group is below its target (a torn write) or gone, replays the rest", () => {
    expect(replayVerdict(APPROVE, 4n)).toBe("discard");
    expect(replayVerdict(APPROVE, null)).toBe("discard");
    expect(replayVerdict(APPROVE, 5n)).toBe("replay");
    expect(replayVerdict(APPROVE, 6n)).toBe("replay");
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
  });
});
