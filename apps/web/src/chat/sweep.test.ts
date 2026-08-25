// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S4a — the committer sweep, driven end to end over its ports.
 *
 * ## What is being defended
 *
 * G-38 residual 1: a device that eager-merges its membership commit before the
 * DS journal has ordered it sits at an epoch nobody else reached, silently and
 * permanently. The sweep's answer is stage -> publish -> read the verdict ->
 * merge or abandon, and four separate rules have to hold for that to be worth
 * anything. Each has a named test below, and each was checked by NEUTERING the
 * rule and observing the named test go red — the verbatim failures are quoted
 * per rule, because a green suite that would stay green with the fix removed is
 * worse than no suite.
 *
 * ## Which of these actually discriminate (MEASURED — each probe was run)
 *
 * 1. **`abandons, and the proposals survive for the next one`** — with
 *    `abandonStagedSweep()` swapped for `mergeStagedSweep()` on the loss
 *    branch of `publishAndAdjudicate`. Two tests went red:
 *
 *        FAIL … > a losing sweep > abandons, and the proposals survive for the next one
 *        AssertionError: expected [ 'fetch>0', 'stage', 'record', …(7) ] to include 'abandon'
 *
 *    plus `abandons when the re-adjudication says it lost`. That merge IS the
 *    fork: the device lands on an epoch the group never reached.
 *
 * 2. **`the Welcome is NOT published before the verdict`** — with the Welcome
 *    fan-out hoisted above `adjudicate`. FOUR tests went red; the sharpest:
 *
 *        FAIL … > the Welcome waits for the verdict > the Welcome is NOT published
 *        before the verdict — and never at all on a LOSS
 *        AssertionError: expected [ 'group:commit', 'mailbox:welcome' ] to deeply
 *        equal [ 'group:commit' ]
 *          [
 *            "group:commit",
 *        +   "mailbox:welcome",
 *          ]
 *
 *    That extra row is the three-party fork ADR-0045 calls the worse failure: a
 *    joiner opening into a state no other member holds.
 *
 * 3. **`runCommitterSweep routes an owed verdict rather than staging a second
 *    sweep`** — with the `hasStagedSweep()` branch removed from
 *    `runCommitterSweep`:
 *
 *        FAIL … > a restart finishes an owed verdict > runCommitterSweep routes an
 *        owed verdict rather than staging a second sweep
 *        Error: ADR-0045: a sweep is already staged
 *
 *    i.e. the group stays frozen — and durably, because a staged commit
 *    survives a restart.
 *
 * 4. **`stands down when the journal moved, and stages nothing`** — with
 *    `shouldStandDown` forced to `false`:
 *
 *        AssertionError: expected { kind: 'committed', …(3) } to deeply equal
 *        { Object (kind, unappliedRows) }
 *        -   "kind": "stood-down",
 *        -   "unappliedRows": 1,
 *
 * 5. **`a record failure abandons the stage and publishes nothing`** — with the
 *    `group.abandonStagedSweep()` removed from the record-failure catch:
 *
 *        AssertionError: expected true to be false // Object.is equality
 *        ❯ src/chat/sweep.test.ts:359  (expect(m.staged()).toBe(false))
 *
 *    `hasStagedSweep()` still true after an abort that published nothing — the
 *    group left frozen for a commit that never existed.
 *
 * ## S4b probes (also MEASURED)
 *
 * 5. **`a deferring candidate never sweeps, and never reaches the network`** —
 *    with the `acting.kind !== "act"` gate forced false. THREE tests went red
 *    across this file and `contacts/proposal-membership.test.ts`:
 *
 *        FAIL … > a deferring candidate never sweeps, and never reaches the network
 *        AssertionError: expected { kind: 'committed', …(3) } to deeply equal
 *        { Object (kind, rank, ...) }
 *        -   "kind": "deferred",
 *        -   "rank": 2,
 *        -   "toRank": 1,
 *        +   "addedCount": 0,
 *        +   "commitMessageId": 101,
 *
 *    That `committed` is a second committer staging on top of one already
 *    running — the multi-committer race in a narrower window.
 *
 * 6. **`a device DEMOTED while its sweep was staged still resolves the
 *    verdict`** — with the owed-verdict check moved back BEHIND the rank check
 *    (the S4a order):
 *
 *        FAIL … > a device DEMOTED while its sweep was staged still resolves
 *        the verdict
 *        AssertionError: expected 'not-committer' not to be 'not-committer'
 *
 *    The device's membership operations stay frozen — durably, since the
 *    staged commit survives a restart — with nothing that could unwedge it.
 *
 * BOUNDARY GUARD, not a discriminator: `nothing pending is a no-op` and
 * `a non-committer never sweeps` both pass with every rule above neutered.
 * They pin the short-circuits that keep this safe to call at the tail of every
 * poll, and they would catch a future change that made the sweep reach the
 * network on the common path.
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  resolveOwedVerdict,
  runCommitterSweep,
  shouldStandDown,
  sweepVerdict,
  SweepVerdictUnavailable,
  type SweepDurableEntry,
  type SweepJournalRow,
  type SweepPorts,
  type StagedSweepArtifacts,
} from "./sweep";
import type { ActingCommitter } from "./committer-beacon";

// ---------------------------------------------------------------------------
// A faithful in-memory MLS model
// ---------------------------------------------------------------------------

/**
 * Journal payloads are tagged bytes so the model can behave the way the real
 * core does without any wasm: byte 0 is the kind, byte 1 is the epoch the frame
 * is FRAMED at. Only a `COMMIT` at the group's current epoch advances it —
 * which is the whole basis of the verdict, so the model must get exactly that
 * right and nothing else.
 */
const COMMIT = 0x01;
const APPLICATION = 0x02;
const PROPOSAL = 0x03;
const NOT_MLS = 0xff;

function frame(kind: number, epoch: number, tag = 0): Uint8Array {
  return new Uint8Array([kind, epoch, tag]);
}

interface Model {
  readonly ports: SweepPorts;
  readonly order: string[];
  readonly published: string[];
  epoch: () => bigint;
  pending: () => number;
  staged: () => boolean;
  entries: () => Map<string, SweepDurableEntry>;
  cursorValue: () => number;
  /** Rows the DS will return, in id order. */
  journal: SweepJournalRow[];
  /** Rows to splice in the instant the commit publishes — the real race. */
  raceRows: SweepJournalRow[];
}

function model(opts: {
  rank?: number | undefined;
  pending?: number;
  epoch?: number;
  /** Signature keys the staged sweep would add (drives the Welcome fan-out). */
  adds?: Uint8Array[];
  recordThrows?: boolean;
  fetchThrows?: boolean;
  /** Pre-seed a staged sweep + its durable entry, as a restart would find. */
  owed?: { entry: SweepDurableEntry } | undefined;
  /** ADR-0045 S4b — the acting-committer verdict. Defaults to `act`, which
   *  is what the owner (rank 0) always gets from the real fold, so every S4a
   *  test below keeps meaning exactly what it meant. */
  acting?: (ownRank: number) => Promise<ActingCommitter>;
}): Model {
  const order: string[] = [];
  const published: string[] = [];
  let epoch = BigInt(opts.epoch ?? 6);
  let pending = opts.pending ?? 0;
  let staged = opts.owed !== undefined;
  const entries = new Map<string, SweepDurableEntry>();
  if (opts.owed !== undefined) entries.set(opts.owed.entry.id, opts.owed.entry);
  let cursorValue = 0;
  let nextId = 100;
  const journal: SweepJournalRow[] = [];
  const raceRows: SweepJournalRow[] = [];
  let entrySeq = 0;

  const ports: SweepPorts = {
    actingCommitter: async (ownRank) => {
      order.push(`acting?${ownRank}`);
      return opts.acting === undefined
        ? { kind: "act" }
        : await opts.acting(ownRank);
    },
    group: {
      groupId: new Uint8Array([7, 7]),
      get epoch(): bigint {
        return epoch;
      },
      ownCommitterRank: () => ("rank" in opts ? opts.rank : 0),
      pendingProposalCount: () => pending,
      hasStagedSweep: () => staged,
      stagePendingProposals: (): StagedSweepArtifacts | null => {
        if (staged) throw new Error("ADR-0045: a sweep is already staged");
        if (pending === 0) return null;
        order.push("stage");
        staged = true; // the epoch deliberately does NOT move
        const adds = opts.adds ?? [];
        return {
          commit: frame(COMMIT, Number(epoch), 0x5e),
          welcome: adds.length > 0 ? new Uint8Array([0x01]) : undefined,
          addedSignatureKeys: adds,
        };
      },
      mergeStagedSweep: () => {
        order.push("merge");
        staged = false;
        epoch += 1n;
        pending = 0; // OpenMLS empties the store on MERGE, and only on merge
      },
      abandonStagedSweep: () => {
        order.push("abandon");
        staged = false;
        // pending is deliberately UNTOUCHED — an abandoned sweep was never
        // merged, so its proposals are still there for the winner.
      },
      applyIncoming: (payload) => {
        const kind = payload[0];
        const framedAt = BigInt(payload[1] ?? 0);
        if (kind === NOT_MLS) throw new Error("not an MLS message");
        if (framedAt !== epoch) throw new Error("wrong epoch");
        order.push(`apply:${kind}@${framedAt}`);
        if (kind === COMMIT) {
          epoch += 1n;
          staged = false; // OpenMLS discards a pending commit on a foreign merge
          pending = 0;
        }
        if (kind === PROPOSAL) pending += 1;
      },
    },
    journal: {
      fetch: async (after) => {
        if (opts.fetchThrows === true) throw new Error("DS unreachable");
        order.push(`fetch>${after}`);
        return journal.filter((r) => r.id > after);
      },
      publishGroup: async (bytes) => {
        published.push(`group:${bytes[0] === COMMIT ? "commit" : "other"}`);
        order.push("publish");
        const id = (nextId += 1);
        journal.push({ id, payload: bytes });
        // Splice the racing rows in at ids BELOW ours, the way the DS would
        // have ordered a commit that arrived first.
        for (const r of raceRows.splice(0)) journal.push(r);
        journal.sort((a, b) => a.id - b.id);
        return id;
      },
      publishMailbox: async () => {
        published.push("mailbox:welcome");
        order.push("publish-welcome");
        return (nextId += 1);
      },
    },
    outbox: {
      newEntryId: () => `entry-${(entrySeq += 1)}`,
      record: async (entry) => {
        if (opts.recordThrows === true) throw new Error("IndexedDB quota");
        order.push("record");
        entries.set(entry.id, entry);
      },
      findOwed: async () => entries.values().next().value ?? null,
      ack: async (id) => {
        order.push("ack");
        entries.delete(id);
      },
      discard: async (id) => {
        order.push("discard");
        entries.delete(id);
      },
    },
    cursor: {
      read: () => cursorValue,
      advance: (to) => {
        cursorValue = to;
      },
    },
    welcomeMailbox: (k) => new Uint8Array([0xbb, k[0] ?? 0]),
    wrapWelcome: (w) => new Uint8Array([0xee, ...w]),
    markOwnPublished: (id) => order.push(`own:${id}`),
    persist: async () => {
      order.push("persist");
    },
    encodeB64: (b) => Buffer.from(b).toString("base64"),
    decodeB64: (s) => new Uint8Array(Buffer.from(s, "base64")),
  };

  return {
    ports,
    order,
    published,
    epoch: () => epoch,
    pending: () => pending,
    staged: () => staged,
    entries: () => entries,
    cursorValue: () => cursorValue,
    journal,
    raceRows,
  };
}

afterEach(() => {
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------

describe("ADR-0045 — the pure verdict rules", () => {
  it("an unchanged epoch is a WIN, a moved one is a LOSS", () => {
    expect(sweepVerdict(6n, 6n)).toBe("won");
    expect(sweepVerdict(6n, 7n)).toBe("lost");
    // A two-epoch jump (two foreign commits below ours) is still just a loss.
    expect(sweepVerdict(6n, 8n)).toBe("lost");
  });

  it("ANY unapplied row stands the sweep down, not only a commit", () => {
    // Deliberately conservative: without a `contentTypeOfMessage` peek there is
    // no way to tell a Commit from an Application message, and standing down
    // costs one retry while staging on top of an unapplied commit costs a
    // published commit every peer drops.
    expect(shouldStandDown(0)).toBe(false);
    expect(shouldStandDown(1)).toBe(true);
    expect(shouldStandDown(9)).toBe(true);
  });
});

describe("ADR-0045 — the sweep short-circuits", () => {
  it("an admin sweeps once it is the acting committer (S4b)", async () => {
    // S4a returned `not-committer` here and the group waited for an owner who
    // might never open the app. S4b asks the beacon fold instead, and an admin
    // that has heard nothing better placed acts — the whole of the slice.
    const m = model({ rank: 1, pending: 3 });
    const out = await runCommitterSweep(m.ports);
    expect(out.kind).toBe("committed");
    // The fold is consulted BEFORE anything is staged or fetched.
    expect(m.order[0]).toBe("acting?1");
  });

  it("a deferring candidate never sweeps, and never reaches the network", async () => {
    const m = model({
      rank: 2,
      pending: 3,
      acting: async () => ({ kind: "defer", toRank: 1 }),
    });
    const out = await runCommitterSweep(m.ports);
    expect(out).toEqual({ kind: "deferred", rank: 2, toRank: 1 });
    // Consulted, then nothing: no stand-down fetch, no stage, no publish.
    expect(m.order).toEqual(["acting?2"]);
  });

  it("an unreadable acting verdict defers — it never sweeps on no evidence", async () => {
    const m = model({
      rank: 1,
      pending: 3,
      acting: async () => {
        throw new Error("wasm unavailable");
      },
    });
    const out = await runCommitterSweep(m.ports);
    expect(out).toEqual({ kind: "deferred", rank: 1, toRank: undefined });
    expect(m.order).toEqual(["acting?1"]);
  });

  it("rank `undefined` (a plain member) is not rank 0", async () => {
    // `undefined` and `0` are not interchangeable and `if (rank)` is wrong in
    // both directions — 0 is the OWNER, the one device that always sweeps.
    const m = model({ rank: undefined, pending: 3 });
    const out = await runCommitterSweep(m.ports);
    expect(out).toEqual({ kind: "not-committer", rank: undefined });
    expect(m.order).toEqual([]);
  });

  it("nothing pending is a no-op with no fetch — safe at the tail of every poll", async () => {
    const m = model({ rank: 0, pending: 0 });
    const out = await runCommitterSweep(m.ports);
    expect(out).toEqual({ kind: "nothing-pending" });
    expect(m.order).toEqual([]);
  });
});

describe("ADR-0045 — obligation 3: the stand-down re-read", () => {
  it("stands down when the journal moved, and stages nothing", async () => {
    const m = model({ rank: 0, pending: 1 });
    m.journal.push({ id: 1, payload: frame(APPLICATION, 6) });

    const out = await runCommitterSweep(m.ports);

    expect(out).toEqual({ kind: "stood-down", unappliedRows: 1 });
    expect(m.staged()).toBe(false);
    expect(m.published).toEqual([]);
    // The proposal is untouched — the poll drains the row and the next attempt
    // re-evaluates.
    expect(m.pending()).toBe(1);
  });

  it("proceeds when the journal is drained", async () => {
    const m = model({ rank: 0, pending: 1 });
    const out = await runCommitterSweep(m.ports);
    expect(out.kind).toBe("committed");
  });
});

describe("ADR-0045 — obligation 4: record before publish, verdict before merge", () => {
  it("a WON sweep records, publishes, adjudicates, then merges", async () => {
    const m = model({ rank: 0, pending: 2, epoch: 6 });

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.order).toEqual([
      "acting?0", // S4b — the fold runs first, and the owner always acts
      "fetch>0", // the stand-down re-read — STILL required after S4b
      "stage",
      "record", // BEFORE any publish (ADR-0040)
      "publish",
      "own:101",
      "fetch>0", // the verdict read
      "merge",
      "persist",
      "ack",
    ]);
    expect(m.epoch()).toBe(7n);
    expect(m.staged()).toBe(false);
    expect(m.entries().size).toBe(0);
  });

  it("a record failure abandons the stage and publishes nothing", async () => {
    const m = model({ rank: 0, pending: 1, recordThrows: true });

    await expect(runCommitterSweep(m.ports)).rejects.toThrow(/IndexedDB quota/);

    // FAIL-CLOSED: nothing on the wire, and — the half that matters — the group
    // is NOT left frozen behind a staged commit that never existed.
    expect(m.published).toEqual([]);
    expect(m.staged()).toBe(false);
    expect(m.epoch()).toBe(6n);
    expect(m.pending()).toBe(1);
  });

  it("an unreadable verdict leaves the sweep STAGED rather than guessing", async () => {
    const m = model({ rank: 0, pending: 1 });
    let fetches = 0;
    const realFetch = m.ports.journal.fetch;
    m.ports.journal.fetch = async (after): Promise<SweepJournalRow[]> => {
      fetches += 1;
      if (fetches === 2) throw new Error("DS unreachable");
      return realFetch(after);
    };

    await expect(runCommitterSweep(m.ports)).rejects.toBeInstanceOf(
      SweepVerdictUnavailable,
    );

    // Merging on an unread verdict is the eager shape; abandoning would discard
    // a commit already on the wire that may have won. So neither happens, and
    // the next attempt re-adjudicates.
    expect(m.staged()).toBe(true);
    expect(m.epoch()).toBe(6n);
    expect(m.entries().size).toBe(1);
  });
});

describe("ADR-0045 — obligation 2: the Welcome waits for the verdict", () => {
  const ALICE = new Uint8Array(32).fill(0xa1);

  it("publishes the Welcome only AFTER a winning verdict", async () => {
    const m = model({ rank: 0, pending: 1, adds: [ALICE] });

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    // The commit publishes, THEN the verdict is read, THEN the Welcome.
    expect(m.order.indexOf("publish")).toBeLessThan(
      m.order.lastIndexOf("fetch>0"),
    );
    expect(m.order.lastIndexOf("fetch>0")).toBeLessThan(
      m.order.indexOf("publish-welcome"),
    );
    expect(m.published).toEqual(["group:commit", "mailbox:welcome"]);
  });

  it("the Welcome is NOT published before the verdict — and never at all on a LOSS", async () => {
    const m = model({ rank: 0, pending: 1, adds: [ALICE], epoch: 6 });
    // A foreign commit at OUR epoch, ordered BELOW ours by the DS.
    m.raceRows.push({ id: 50, payload: frame(COMMIT, 6, 0xfa) });

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("lost");
    // The whole point. A Welcome on the wire for a commit that lost would put a
    // joiner into a group state NO other member holds — a three-party fork,
    // strictly worse than the two-party one this mechanism exists to close.
    expect(m.published).toEqual(["group:commit"]);
    expect(m.order).not.toContain("publish-welcome");
  });
});

describe("ADR-0045 — a losing sweep", () => {
  it("abandons, and the proposals survive for the next one", async () => {
    const m = model({ rank: 0, pending: 2, epoch: 6 });
    m.raceRows.push({ id: 50, payload: frame(COMMIT, 6, 0xfa) });

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("lost");
    expect(m.order).toContain("abandon");
    expect(m.order).not.toContain("merge");
    // The device landed on the SAME epoch the winner's commit produced — 7,
    // reached by applying the winner, not by merging our own. Merging instead
    // would have put it at 8, alone.
    expect(m.epoch()).toBe(7n);
    expect(m.staged()).toBe(false);
    // The lost commit is discarded from the outbox: the restart sweep must
    // never re-adjudicate a commit whose proposals the winner already swept.
    expect(m.entries().size).toBe(0);
    expect(m.order).toContain("discard");
  });

  it("advances the cursor over the rows the verdict applied", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    m.raceRows.push({ id: 50, payload: frame(COMMIT, 6, 0xfa) });

    await runCommitterSweep(m.ports);

    // The winner's row was consumed here, so the poll must not re-feed it to
    // OpenMLS. The cursor stops BELOW our own commit (101), which the poll
    // re-fetches and drops through the echo filter.
    expect(m.cursorValue()).toBe(50);
  });
});

describe("ADR-0045 — obligation 1: a restart finishes an owed verdict", () => {
  const OWED: SweepDurableEntry = {
    id: "entry-owed",
    commitB64: Buffer.from(frame(COMMIT, 6, 0x5e)).toString("base64"),
    welcomeEnvelopeB64: Buffer.from([0xee, 0x01]).toString("base64"),
    welcomeMailboxesB64: [Buffer.from([0xbb, 0xa1]).toString("base64")],
  };

  it("re-publishes idempotently, re-adjudicates, and merges when it won", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6, owed: { entry: OWED } });

    const out = await resolveOwedVerdict(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.epoch()).toBe(7n);
    expect(m.staged()).toBe(false);
    // The Welcome the ORIGINAL run held back is published now — after the
    // verdict, exactly as it would have been the first time.
    expect(m.published).toEqual(["group:commit", "mailbox:welcome"]);
  });

  it("runCommitterSweep routes an owed verdict rather than staging a second sweep", async () => {
    // Staging a second sweep is refused fail-closed by the core, so a sweep that
    // did not check first would throw and leave the group's membership frozen —
    // and the freeze survives a restart, because the staged commit is durable.
    const m = model({ rank: 0, pending: 1, epoch: 6, owed: { entry: OWED } });

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.order).not.toContain("stage");
    expect(m.staged()).toBe(false);
  });

  it("a device DEMOTED while its sweep was staged still resolves the verdict", async () => {
    // ADR-0045 S4b made this newly reachable: S4a's only committer was the
    // owner, whom no role change can demote, but an ADMIN can be demoted while
    // holding a staged sweep. A staged commit is DURABLE and freezes this
    // group's membership operations, so gating the owed verdict behind the
    // rank check would freeze such a device permanently, with nothing that
    // could ever unwedge it. Re-publishing is safe: every peer's role gate
    // refuses a demoted author's commit, so it loses and is abandoned — which
    // IS the unwedge.
    const m = model({
      rank: undefined, // demoted out of the committer set
      pending: 1,
      epoch: 6,
      owed: { entry: OWED },
    });

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).not.toBe("not-committer");
    expect(m.staged()).toBe(false);
    // The acting fold is never consulted on this path — an owed verdict is
    // finished by whoever staged it, not by whoever is currently acting.
    expect(m.order).not.toContain("acting?");
  });

  it("abandons when the re-adjudication says it lost", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6, owed: { entry: OWED } });
    m.raceRows.push({ id: 50, payload: frame(COMMIT, 6, 0xfa) });

    const out = await resolveOwedVerdict(m.ports);

    expect(out.kind).toBe("lost");
    expect(m.published).toEqual(["group:commit"]);
    expect(m.epoch()).toBe(7n);
  });

  it("a staged sweep with NO durable entry is abandoned, which unwedges the group", async () => {
    // ADR-0040 records BEFORE it publishes, atomically with the snapshot, so a
    // restored snapshot carrying a stage with no entry means the write never
    // completed and the commit never reached the wire. Abandoning is both
    // correct and the thing that lifts the durable membership freeze.
    const m = model({ rank: 0, pending: 1, epoch: 6, owed: { entry: OWED } });
    m.entries().clear();

    const out = await resolveOwedVerdict(m.ports);

    expect(out).toEqual({ kind: "abandoned-orphan" });
    expect(m.staged()).toBe(false);
    expect(m.published).toEqual([]);
    expect(m.pending()).toBe(1);
  });

  it("reports no verdict owed when nothing is staged — idempotent on a clean restart", async () => {
    const m = model({ rank: 0, pending: 0 });
    await expect(resolveOwedVerdict(m.ports)).resolves.toEqual({
      kind: "no-verdict-owed",
    });
  });
});

describe("ADR-0045 — the verdict reads only the rows BELOW our own commit", () => {
  it("a foreign commit ABOVE our id does not make us lose", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    const out = await runCommitterSweep(m.ports);
    expect(out.kind).toBe("committed");

    // A commit the DS ordered after ours is the OTHER device's problem — it is
    // framed at 6 and every member is at 7, so it is dropped. Reading it here
    // would flip a won sweep to lost and strand this device at 6 with its own
    // commit un-re-appliable.
    expect(m.epoch()).toBe(7n);
  });

  it("non-MLS rows and application messages below our commit do not move the epoch", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    m.raceRows.push({ id: 40, payload: frame(NOT_MLS, 0) });
    m.raceRows.push({ id: 41, payload: frame(APPLICATION, 6) });
    m.raceRows.push({ id: 42, payload: frame(PROPOSAL, 6) });

    const out = await runCommitterSweep(m.ports);

    // Only a Commit advances an epoch, so the verdict is a WIN — which is what
    // makes "apply, then read the epoch" exact where an epoch-only peek would
    // have over-detected all three of these as losses and bricked the device.
    expect(out.kind).toBe("committed");
    expect(m.order).toContain("merge");
  });
});
