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
 * ## The swallowed-rows probe (also MEASURED)
 *
 * 7. **`a chat message ordered below our own commit is SURFACED, not
 *    swallowed`** — this pins a defect that WAS live. `adjudicate` called
 *    `group.applyIncoming(row.payload)` and discarded whatever it produced, and
 *    the cursor then advanced past the row, so an ordinary chat message another
 *    member sent in the window between the stand-down re-read and our publish
 *    was decrypted, consumed, and lost from the timeline for good. With the old
 *    call restored (payload only, un-awaited), four tests in this block go red:
 *
 *        FAIL … > the verdict apply must not swallow the rows it consumes >
 *        a chat message ordered below our own commit is SURFACED, not swallowed
 *        AssertionError: expected [] to deeply equal [ 'chat#50@t-50' ]
 *
 *    plus `surfaces every applied row in journal order`, `a row that THROWS
 *    surfaces nothing`, and `the restart path surfaces them too`. (The same
 *    probe also reds three pre-existing verdict tests, because a model whose
 *    `applyIncoming` is handed a bare payload cannot apply anything at all —
 *    that part is an artefact of the probe, not evidence.)
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
  resolveOwedWelcomes,
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

function kindName(kind: number | undefined): string {
  if (kind === COMMIT) return "commit";
  if (kind === APPLICATION) return "chat";
  if (kind === PROPOSAL) return "proposal";
  return "other";
}

function frame(kind: number, epoch: number, tag = 0): Uint8Array {
  return new Uint8Array([kind, epoch, tag]);
}

/**
 * One DS journal row, timestamp included.
 *
 * The `createdAt` is not decoration: the adapter turns each applied row into a
 * timeline event through the SAME `toEvent` the poll uses, and that function
 * requires the DS row timestamp. A row shape that carried only the payload is
 * exactly what made the sweep unable to surface anything.
 */
function row(id: number, kind: number, epoch: number, tag = 0): SweepJournalRow {
  return { id, payload: frame(kind, epoch, tag), createdAt: `t-${id}` };
}

interface Model {
  readonly ports: SweepPorts;
  readonly order: string[];
  readonly published: string[];
  /** What the applied rows handed to the timeline, in journal order. */
  readonly surfaced: string[];
  epoch: () => bigint;
  pending: () => number;
  staged: () => boolean;
  entries: () => Map<string, SweepDurableEntry>;
  /** Entry ids whose VERDICT is settled and whose Welcome fan-out is still
   *  owed — the durable marker the ghost-member fix turns on. */
  welcomesOwed: () => Set<string>;
  cursorValue: () => number;
  /** Rows the DS will return, in id order. */
  journal: SweepJournalRow[];
  /** Rows to splice in the instant the commit publishes — the real race. */
  raceRows: SweepJournalRow[];
  /** Flip to make the mailbox fan-out fail (a transient DS blip), and back
   *  again to model the retry after a restart. */
  failWelcomePublish: (fail: boolean) => void;
  /** Flip to make the durable welcomes-owed marker write fail. */
  failMarkWelcomesOwed: (fail: boolean) => void;
  /** ADR-0050 2.5a item 7 — flip to make every mailbox unreachable: no grant
   *  opens it, so nothing is written and the port answers `null`. */
  mailboxUnreachable: (on: boolean) => void;
  /** Flip to make the verdict apply's FAIL-CLOSED snapshot write fail — the
   *  swallowed-IndexedDB-error shape that used to strand the device. */
  failPersistApplied: (fail: boolean) => void;
  /** ADR-0053 — flip to make the staged epoch's registration be refused. */
  failRegister: (fail: boolean) => void;
  /** ADR-0081 — flip to make the commit route answer 409 (another commit
   *  owns the staged epoch). */
  conflictCommit: (on: boolean) => void;
  /** ADR-0053 — each `beginApply` completion: `release`, or `heal@N->M`. */
  readonly heals: string[];
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
  /** ADR-0057 — does this device hold the root or a registrar seed?
   *  Undefined = the port is absent (a test not about ADR-0057). */
  mayRegister?: boolean;
}): Model {
  const order: string[] = [];
  const published: string[] = [];
  /** Everything the applied rows surfaced to the timeline, in journal order. */
  const surfaced: string[] = [];
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
  /** The durable welcomes-owed marker, as a set of entry ids. */
  const welcomesOwed = new Set<string>();
  let welcomePublishFails = false;
  let mailboxUnreachable = false;
  let markWelcomesOwedFails = false;
  let persistAppliedFails = false;
  let registerFails = false;
  let commitConflicts = false;
  const heals: string[] = [];

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
      // The double stands in for the REAL adapter, which both applies the row
      // and surfaces what it produced. Splitting those two in the model would
      // make the defect untestable here: the whole failure was that the second
      // half did not exist.
      applyIncoming: async (journalRow) => {
        const payload = journalRow.payload;
        const kind = payload[0];
        const framedAt = BigInt(payload[1] ?? 0);
        if (kind === NOT_MLS) throw new Error("not an MLS message");
        if (framedAt !== epoch) throw new Error("wrong epoch");
        order.push(`apply:${kind}@${framedAt}`);
        // Surfacing happens only AFTER a successful apply, exactly as in the
        // adapter — a skipped row must produce no event.
        surfaced.push(`${kindName(kind)}#${journalRow.id}@${journalRow.createdAt}`);
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
      // ADR-0081 — the staged epoch's registration and the commit row in one
      // request: "register" then "publish" in the order log, one call.
      commitStaged: async (bytes) => {
        if (registerFails) throw new Error("ds access: 401 proof required");
        if (commitConflicts) throw Object.assign(new Error("409"), { status: 409 });
        order.push("register");
        // ADR-0053 contract point 8 — the sweep writes only its commit to the
        // journal; anything else here is a defect the assertions will show.
        const label = bytes[0] === COMMIT ? "commit" : "other";
        published.push(`group:${label}`);
        order.push(label === "commit" ? "publish" : "publish-other");
        const id = (nextId += 1);
        journal.push({ id, payload: bytes, createdAt: `t-${id}` });
        // Splice the racing rows in at ids BELOW ours, the way the DS would
        // have ordered a commit that arrived first.
        for (const r of raceRows.splice(0)) journal.push(r);
        journal.sort((a, b) => a.id - b.id);
        return id;
      },
      publishMailbox: async () => {
        if (welcomePublishFails) throw new Error("DS unreachable");
        if (mailboxUnreachable) {
          order.push("welcome-kept-owed");
          return null;
        }
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
      // The real store's condition, verbatim: an un-acked SWEEP entry whose
      // welcomes are NOT yet owed. A welcome-owed entry has already reached a
      // verdict, so it is no longer the verdict finder's business.
      findOwed: async () =>
        [...entries.values()].find((e) => !welcomesOwed.has(e.id)) ?? null,
      markWelcomesOwed: async (id, commitMessageId) => {
        if (markWelcomesOwedFails) throw new Error("IndexedDB quota");
        order.push("welcomes-owed");
        const existing = entries.get(id);
        if (existing !== undefined) {
          entries.set(id, { ...existing, commitMessageId });
        }
        welcomesOwed.add(id);
      },
      findOwedWelcomes: async (skip) =>
        [...entries.values()].find((e) => welcomesOwed.has(e.id) && skip?.has(e.id) !== true) ?? null,
      ack: async (id) => {
        order.push("ack");
        entries.delete(id);
        welcomesOwed.delete(id);
      },
      discard: async (id) => {
        order.push("discard");
        entries.delete(id);
        welcomesOwed.delete(id);
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
    ...(opts.mayRegister === undefined
      ? {}
      : {
          mayRegister: () => {
            order.push(`may-register?${opts.mayRegister}`);
            return opts.mayRegister === true;
          },
        }),
    // ADR-0081 — what a failed commit call means.
    classifyCommitFailure: (err) =>
      (err as { status?: unknown }).status === 409
        ? "lost"
        : String(err).includes("401")
          ? "refused"
          : "unknown",
    // ADR-0053 contract point 4 — records whether the sweep asked to heal.
    beginApply: () => {
      const epochAtBegin = epoch;
      return async (heal) => {
        heals.push(heal ? `heal@${epochAtBegin}->${epoch}` : "release");
      };
    },
    markOwnPublished: (id) => order.push(`own:${id}`),
    persist: async () => {
      order.push("persist");
    },
    // The FAIL-CLOSED persist the verdict apply runs before it advances the
    // shared cursor. Distinct from `persist` above because it must REJECT.
    persistApplied: async () => {
      if (persistAppliedFails) throw new Error("IndexedDB quota");
      order.push("persist-applied");
    },
    encodeB64: (b) => Buffer.from(b).toString("base64"),
    decodeB64: (s) => new Uint8Array(Buffer.from(s, "base64")),
  };

  return {
    ports,
    order,
    published,
    surfaced,
    epoch: () => epoch,
    pending: () => pending,
    staged: () => staged,
    entries: () => entries,
    welcomesOwed: () => welcomesOwed,
    cursorValue: () => cursorValue,
    journal,
    raceRows,
    failWelcomePublish: (fail) => {
      welcomePublishFails = fail;
    },
    failMarkWelcomesOwed: (fail) => {
      markWelcomesOwedFails = fail;
    },
    mailboxUnreachable: (on) => {
      mailboxUnreachable = on;
    },
    failPersistApplied: (fail) => {
      persistAppliedFails = fail;
    },
    failRegister: (fail) => {
      registerFails = fail;
    },
    conflictCommit: (on) => {
      commitConflicts = on;
    },
    heals,
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
    m.journal.push(row(1, APPLICATION, 6));

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
      "record", // BEFORE anything leaves the device (ADR-0040)
      "register", // ADR-0081 — the staged epoch's key and the commit row, one call
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
    expect(m.published).toEqual([
      "group:commit",
      "mailbox:welcome",
    ]);
  });

  it("the Welcome is NOT published before the verdict — and never at all on a LOSS", async () => {
    const m = model({ rank: 0, pending: 1, adds: [ALICE], epoch: 6 });
    // A foreign commit at OUR epoch, ordered BELOW ours by the DS.
    m.raceRows.push(row(50, COMMIT, 6, 0xfa));

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
    m.raceRows.push(row(50, COMMIT, 6, 0xfa));

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
    m.raceRows.push(row(50, COMMIT, 6, 0xfa));

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
    expect(m.published).toEqual([
      "group:commit",
      "mailbox:welcome",
    ]);
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
    m.raceRows.push(row(50, COMMIT, 6, 0xfa));

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

describe("ADR-0050 2.5a item 7 — a Welcome no grant delivers is kept owed, never lost", () => {
  const ADDED = new Uint8Array([0xa2]);

  it("the commit still settles as committed, nothing is written, and the entry stays owed", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6, adds: [ADDED] });
    m.mailboxUnreachable(true);
    const out = await runCommitterSweep(m.ports);
    // Committed, so every post-commit step of the caller still runs.
    expect(out).toMatchObject({ kind: "committed", welcomesOwed: true, welcomeMessageIds: [] });
    expect(m.published).toEqual(["group:commit"]);
    expect(m.order).not.toContain("ack");
    expect(m.welcomesOwed().size).toBe(1);
  });

  it("is retried on the next attempt and delivered once a grant arrives; the sweep is not blocked meanwhile", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6, adds: [ADDED] });
    m.mailboxUnreachable(true);
    await runCommitterSweep(m.ports);
    // Still no grant: tried again, nothing written, and the sweep goes on to
    // its own business (here nothing is pending) instead of stopping.
    expect(await runCommitterSweep(m.ports)).toEqual({ kind: "nothing-pending" });
    expect(m.published).toEqual(["group:commit"]);
    expect(m.welcomesOwed().size).toBe(1);
    // A grant arrived.
    m.mailboxUnreachable(false);
    const out = await runCommitterSweep(m.ports);
    expect(out.kind).toBe("welcomes-republished");
    expect(m.published).toEqual(["group:commit", "mailbox:welcome"]);
    expect(m.welcomesOwed().size).toBe(0);
  });
});

describe("ADR-0045 — a Welcome that failed AFTER the merge is still owed", () => {
  const ADDED = new Uint8Array([0xa1]);

  /** The exact shape the finding describes: the commit won, the merge and the
   *  persist both landed, and only the mailbox fan-out failed. */
  async function ghostMemberState(): Promise<Model> {
    const m = model({ rank: 0, pending: 1, epoch: 6, adds: [ADDED] });
    m.failWelcomePublish(true);
    await expect(runCommitterSweep(m.ports)).rejects.toThrow(/DS unreachable/);
    return m;
  }

  it("re-publishes the Welcome after a restart — the ghost member is repaired", async () => {
    // THE headline. Before the fix nothing retried: the next tail sweep found
    // `hasStagedSweep()` false (it merged) and `pendingProposalCount()` 0 (the
    // merge consumed them) and reported `nothing-pending`, while the restart
    // path's `resolveOwedVerdict` reported `no-verdict-owed` for the same
    // reason and the legacy loop skipped the entry outright. The added member
    // was in every other member's roster, every later message was encrypted to
    // their leaf, and they could never construct group state.
    const m = await ghostMemberState();
    expect(m.epoch()).toBe(7n);
    expect(m.published).toEqual(["group:commit"]);

    // The DS comes back; the next sweep runs.
    m.failWelcomePublish(false);
    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("welcomes-republished");
    expect(m.published).toEqual([
      "group:commit",
      "mailbox:welcome",
    ]);
    // Acked, so nothing is owed any more.
    expect(m.entries().size).toBe(0);
    expect(m.welcomesOwed().size).toBe(0);
  });

  it("the restart seam finishes it too, without re-adjudicating", async () => {
    const m = await ghostMemberState();
    m.failWelcomePublish(false);
    // `order` accumulates across both runs, so the retry's own steps are the
    // tail — the first run legitimately staged and published.
    const before = m.order.length;

    const out = await resolveOwedWelcomes(m.ports);

    expect(out.kind).toBe("welcomes-republished");
    expect(m.published).toEqual([
      "group:commit",
      "mailbox:welcome",
    ]);
    // The verdict is already known — the retry must not stage, publish a
    // second commit, or apply another row to re-read the journal order.
    expect(m.order.slice(before)).toEqual([
      "publish-welcome",
      "ack",
    ]);
    expect(m.published.filter((p) => p === "group:commit")).toHaveLength(1);
  });

  it("a Welcome-publish failure does NOT roll the merged commit back", async () => {
    // The commit won the order and peers are at N+1. Abandoning or otherwise
    // undoing the merge here would strand this device below the group.
    const m = await ghostMemberState();

    expect(m.epoch()).toBe(7n);
    expect(m.staged()).toBe(false);
    expect(m.order).toContain("merge");
    expect(m.order).not.toContain("abandon");
    // Persisted BEFORE the fan-out was attempted, so a crash here keeps N+1.
    expect(m.order.indexOf("persist")).toBeLessThan(
      m.order.indexOf("welcomes-owed"),
    );
    // The entry survives un-acked, carrying the marker that says what is left.
    expect(m.entries().size).toBe(1);
    expect(m.welcomesOwed().size).toBe(1);
  });

  it("the marker is written BEFORE the fan-out, and only after the merge", async () => {
    // The ordering that keeps ADR-0045 obligation 2 intact: the marker is what
    // a restart reads as "the verdict said WON", so recording it any earlier
    // would let a pre-verdict restart publish a Welcome for a commit that then
    // lost — the three-party fork.
    const m = model({ rank: 0, pending: 1, epoch: 6, adds: [ADDED] });
    await runCommitterSweep(m.ports);

    const merge = m.order.indexOf("merge");
    const marker = m.order.indexOf("welcomes-owed");
    const fanout = m.order.indexOf("publish-welcome");
    expect(merge).toBeGreaterThanOrEqual(0);
    expect(marker).toBeGreaterThan(merge);
    expect(fanout).toBeGreaterThan(marker);
  });

  it("a pre-verdict restart never publishes the Welcome", async () => {
    // A staged sweep owes a VERDICT, not a Welcome. `resolveOwedWelcomes` must
    // refuse it outright rather than reading the entry's mailboxes, or the
    // whole obligation-2 ordering collapses on the restart path.
    const m = model({
      rank: 0,
      pending: 1,
      epoch: 6,
      owed: {
        entry: {
          id: "entry-owed",
          commitB64: Buffer.from(frame(COMMIT, 6, 0x5e)).toString("base64"),
          welcomeEnvelopeB64: Buffer.from([0xee, 0x01]).toString("base64"),
          welcomeMailboxesB64: [Buffer.from([0xbb, 0xa1]).toString("base64")],
        },
      },
    });

    const out = await resolveOwedWelcomes(m.ports);

    expect(out).toEqual({ kind: "no-welcomes-owed" });
    expect(m.published).toEqual([]);
    expect(m.staged()).toBe(true);
  });

  it("a duplicate re-publish is harmless — the second attempt is a no-op", async () => {
    // The first fan-out may actually have landed and only the ack failed, so a
    // retry can put a second identical Welcome in the joiner's mailbox. That is
    // inert: `StagedWelcome::into_group` deletes the joiner's KeyPackage bundle
    // (openmls-0.8.1 `group/mls_group/creation.rs`), so the second copy fails to
    // stage and every client's mailbox handler catches and skips it. On THIS
    // side the ack clears the marker, so nothing re-publishes a third time.
    const m = await ghostMemberState();
    m.failWelcomePublish(false);

    await resolveOwedWelcomes(m.ports);
    const again = await resolveOwedWelcomes(m.ports);

    expect(again).toEqual({ kind: "no-welcomes-owed" });
    expect(m.published.filter((p) => p === "mailbox:welcome")).toHaveLength(1);
  });

  it("nothing is owed when the sweep carried no add", async () => {
    // A remove / role sweep has no Welcome, so it must not pay for a marker
    // write and must leave nothing behind for a restart to chase.
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.order).not.toContain("welcomes-owed");
    expect(m.welcomesOwed().size).toBe(0);
    await expect(resolveOwedWelcomes(m.ports)).resolves.toEqual({
      kind: "no-welcomes-owed",
    });
  });

  it("the ack path is unchanged when everything succeeds", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6, adds: [ADDED] });
    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.published).toEqual([
      "group:commit",
      "mailbox:welcome",
    ]);
    expect(m.order).toContain("ack");
    // Acked in the same run, so the marker never survives a healthy sweep.
    expect(m.entries().size).toBe(0);
    expect(m.welcomesOwed().size).toBe(0);
  });

  it("a marker write that FAILS still publishes the Welcome now", async () => {
    // The marker only buys a retry. The verdict is already in, so refusing to
    // publish because the bookkeeping write failed would MANUFACTURE the ghost
    // member this whole branch exists to prevent.
    const m = model({ rank: 0, pending: 1, epoch: 6, adds: [ADDED] });
    m.failMarkWelcomesOwed(true);

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.published).toEqual([
      "group:commit",
      "mailbox:welcome",
    ]);
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
    m.raceRows.push(row(40, NOT_MLS, 0));
    m.raceRows.push(row(41, APPLICATION, 6));
    m.raceRows.push(row(42, PROPOSAL, 6));

    const out = await runCommitterSweep(m.ports);

    // Only a Commit advances an epoch, so the verdict is a WIN — which is what
    // makes "apply, then read the epoch" exact where an epoch-only peek would
    // have over-detected all three of these as losses and bricked the device.
    expect(out.kind).toBe("committed");
    expect(m.order).toContain("merge");
  });
});

describe("ADR-0045 — the verdict apply must not swallow the rows it consumes", () => {
  /**
   * The defect this pins.
   *
   * `adjudicate` applies every row the DS ordered below our own commit, and the
   * cursor then advances past them, so the poll never fetches them again. When
   * one of those rows is an ordinary chat message from another member, applying
   * it correctly advances the ratchet — and, before this, threw the plaintext
   * away. The message was permanently lost from the timeline, silently.
   *
   * The window is narrow but real and reachable without any fault: the
   * stand-down check proves the journal empty, and between that check and the
   * publish another member can send. Anything the DS orders below our commit
   * lands in exactly this loop.
   */
  it("a chat message ordered below our own commit is SURFACED, not swallowed", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    // Sent by another member in the window between the stand-down re-read and
    // our publish, so the DS orders it BELOW our commit.
    m.raceRows.push(row(50, APPLICATION, 6));

    const out = await runCommitterSweep(m.ports);

    // The verdict is unaffected — an application message moves no epoch.
    expect(out.kind).toBe("committed");
    // …and the message reached the timeline, carrying the DS row id and the DS
    // row timestamp, which is what the shared `toEvent` needs.
    expect(m.surfaced).toEqual(["chat#50@t-50"]);
    // The cursor moved past it, which is exactly why surfacing is mandatory:
    // the poll will never fetch this row again.
    expect(m.cursorValue()).toBe(50);
  });

  it("surfaces every applied row in journal order, and only those below our commit", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    m.raceRows.push(row(40, APPLICATION, 6));
    m.raceRows.push(row(41, PROPOSAL, 6));
    m.raceRows.push(row(42, APPLICATION, 6));

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.surfaced).toEqual(["chat#40@t-40", "proposal#41@t-41", "chat#42@t-42"]);
  });

  it("a row that THROWS surfaces nothing, and still advances the cursor over it", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    // A non-MLS Lakko envelope (rendezvous / attribution) — skipped exactly as
    // the poll skips it. A skipped row was never decrypted, so there is no
    // plaintext to surface and inventing one would be a lie.
    m.raceRows.push(row(40, NOT_MLS, 0));
    m.raceRows.push(row(41, APPLICATION, 6));

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.surfaced).toEqual(["chat#41@t-41"]);
    // `lastApplied` still advanced over the skipped row, byte-for-byte as
    // before: the cursor stops at the last row this run consumed.
    expect(m.cursorValue()).toBe(41);
  });

  it("rows AT or ABOVE our own commit surface nothing and do not move the cursor past it", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    const out = await runCommitterSweep(m.ports);
    expect(out.kind).toBe("committed");

    // Our own commit is id 101 and is the only row in the journal. It is not
    // part of the question, so nothing is applied, nothing is surfaced, and the
    // cursor is untouched — the poll re-fetches it and drops it through the
    // echo filter.
    expect(m.surfaced).toEqual([]);
    expect(m.cursorValue()).toBe(0);
  });

  /**
   * The verdict apply advances the SAME cursor `ChatClient.pollMessages` drives,
   * over rows the poll will never fetch again — so it is subject to the same
   * ordering rule, and for the same ADR-0045 reason: a PROPOSAL applied here but
   * never written to disk, with the cursor already past it, makes the commit
   * that carries it by reference permanently unappliable. The device sits at
   * epoch N while the group moves on, silently.
   *
   * MEASURED — with the `gate.commit(...)` wrapper in `adjudicate` reverted to
   * the bare `ports.cursor.advance(lastApplied)` it replaced:
   *
   *     FAIL  src/chat/sweep.test.ts > … > the verdict HOLDS the cursor when its
   *     snapshot write fails
   *     AssertionError: expected 42 to be +0 // Object.is equality
   */
  it("the verdict HOLDS the cursor when its snapshot write fails", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    m.raceRows.push(row(41, APPLICATION, 6));
    m.raceRows.push(row(42, PROPOSAL, 6));
    m.failPersistApplied(true);

    const out = await runCommitterSweep(m.ports);

    // The verdict is still read — the epoch comparison is in memory and
    // unaffected — so a storage failure never turns a won sweep into a lost one.
    expect(out.kind).toBe("committed");
    // The rows still reach the timeline: they were decrypted, they are real.
    expect(m.surfaced).toEqual(["chat#41@t-41", "proposal#42@t-42"]);
    // But the cursor does NOT move past them. The next poll re-fetches, and the
    // proposal is re-delivered ahead of any commit that references it.
    expect(m.cursorValue()).toBe(0);
  });

  it("the verdict advances the cursor only AFTER the snapshot write lands", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    m.raceRows.push(row(42, PROPOSAL, 6));

    await runCommitterSweep(m.ports);

    expect(m.cursorValue()).toBe(42);
    // Order, not merely presence: the write precedes the advance, which is the
    // whole rule. `persist-applied` is recorded by the fail-closed port and
    // sits between the last apply and the merge.
    const applied = m.order.indexOf(`apply:${PROPOSAL}@6`);
    const persisted = m.order.indexOf("persist-applied");
    expect(applied).toBeGreaterThanOrEqual(0);
    expect(persisted).toBeGreaterThan(applied);
  });

  it("a verdict that applied NOTHING neither persists nor touches the cursor", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    const out = await runCommitterSweep(m.ports);
    expect(out.kind).toBe("committed");
    expect(m.order).not.toContain("persist-applied");
    expect(m.cursorValue()).toBe(0);
  });

  it("a LOSING sweep still surfaces the rows it applied", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    m.raceRows.push(row(50, APPLICATION, 6));
    // The winner's commit, ordered below ours — this is what makes us lose.
    m.raceRows.push(row(51, COMMIT, 6, 0xfa));

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("lost");
    // Losing the ORDER says nothing about the rows: they were applied, the
    // ratchet moved, and the cursor advanced past them either way.
    expect(m.surfaced).toEqual(["chat#50@t-50", "commit#51@t-51"]);
    expect(m.cursorValue()).toBe(51);
  });

  it("the restart path surfaces them too — an owed verdict consumes rows exactly as a fresh sweep does", async () => {
    const OWED_ENTRY: SweepDurableEntry = {
      id: "entry-owed",
      commitB64: Buffer.from(frame(COMMIT, 6, 0x5e)).toString("base64"),
      welcomeEnvelopeB64: undefined,
      welcomeMailboxesB64: [],
    };
    const m = model({
      rank: 0,
      pending: 1,
      epoch: 6,
      owed: { entry: OWED_ENTRY },
    });
    m.raceRows.push(row(50, APPLICATION, 6));

    const out = await resolveOwedVerdict(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.surfaced).toEqual(["chat#50@t-50"]);
  });
});

describe("ADR-0053 contract point 8 — the sweep delivers every Welcome to a mailbox", () => {
  // ADR-0045 S7 posted a `WelcomeRendezvous` to the group journal for every
  // added key, because a `/g/` joiner could only read the journal. A claimed
  // journal answers only its members, so the joiner now waits on its own
  // device mailbox like every other added member and the journal copy is gone.
  const A1 = new Uint8Array(32).fill(0xa1);
  const A2 = new Uint8Array(32).fill(0xa2);

  it("posts one mailbox Welcome per add and NOTHING but the commit to the journal", async () => {
    const m = model({ rank: 0, pending: 2, epoch: 6, adds: [A1, A2] });

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.published).toEqual(["group:commit", "mailbox:welcome", "mailbox:welcome"]);
    expect(m.published.filter((p) => p.startsWith("group:"))).toEqual(["group:commit"]);
  });

  it("holds the mailbox Welcome back until the verdict", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6, adds: [A1] });
    m.raceRows.push(row(50, COMMIT, 6, 0xfa));

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("lost");
    expect(m.published).toEqual(["group:commit"]);
  });

  it("a sweep carrying no add posts nothing but the commit", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6, adds: [] });

    await runCommitterSweep(m.ports);

    expect(m.published).toEqual(["group:commit"]);
  });
});

describe("ADR-0081 — the staged epoch is registered together with its commit", () => {
  it("records first, then sends the registration and the row in one call", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });

    await runCommitterSweep(m.ports);

    const register = m.order.indexOf("register");
    expect(register).toBeGreaterThan(m.order.indexOf("record"));
    // One call: nothing comes between the registration and the publish.
    expect(m.order[register + 1]).toBe("publish");
  });

  it("a refused commit abandons the stage, drops its entry and publishes nothing", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    m.failRegister(true);

    await expect(runCommitterSweep(m.ports)).rejects.toThrow(/proof required/);

    expect(m.published).toEqual([]);
    expect(m.order).toContain("abandon");
    expect(m.entries().size).toBe(0);
    expect(m.staged()).toBe(false);
    expect(m.epoch()).toBe(6n);
    // The proposals survive for the next attempt.
    expect(m.pending()).toBe(1);
  });

  it("a 409 is a lost verdict: abandoned, entry dropped, nothing published", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    m.conflictCommit(true);

    const out = await runCommitterSweep(m.ports);

    expect(out).toEqual({ kind: "lost" });
    expect(m.published).toEqual([]);
    expect(m.order).toContain("abandon");
    expect(m.entries().size).toBe(0);
    expect(m.staged()).toBe(false);
    expect(m.pending()).toBe(1);
  });

  it("the restart path re-sends the same row through the commit route", async () => {
    const OWED: SweepDurableEntry = {
      id: "entry-owed",
      commitB64: Buffer.from(frame(COMMIT, 6, 0x5e)).toString("base64"),
      welcomeEnvelopeB64: undefined,
      welcomeMailboxesB64: [],
    };
    const m = model({ rank: 0, pending: 1, epoch: 6, owed: { entry: OWED } });

    const out = await resolveOwedVerdict(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.order.filter((o) => o === "register")).toHaveLength(1);
    expect(m.published).toEqual(["group:commit"]);
  });

  it("an unreachable DS leaves the sweep staged with its entry for the owed verdict", async () => {
    const OWED: SweepDurableEntry = {
      id: "entry-owed",
      commitB64: Buffer.from(frame(COMMIT, 6, 0x5e)).toString("base64"),
      welcomeEnvelopeB64: undefined,
      welcomeMailboxesB64: [],
    };
    const m = model({ rank: 0, pending: 1, epoch: 6, owed: { entry: OWED } });
    m.ports.journal.commitStaged = async () => {
      throw new Error("network down");
    };

    await expect(resolveOwedVerdict(m.ports)).rejects.toThrow(/network/);

    expect(m.staged()).toBe(true);
    expect(m.order).not.toContain("abandon");
  });
});

describe("ADR-0057 obligation 5 — a committer with no journal authority does not stage", () => {
  it("no root and no registrar seed: nothing staged, fetched or published", async () => {
    const m = model({ rank: 1, pending: 2, mayRegister: false });
    const out = await runCommitterSweep(m.ports);
    expect(out).toEqual({ kind: "no-journal-authority", rank: 1 });
    expect(m.order).toEqual(["may-register?false"]);
    expect(m.staged()).toBe(false);
    expect(m.published).toEqual([]);
    expect(m.pending()).toBe(2);
  });

  it("a device that holds one sweeps as before", async () => {
    const m = model({ rank: 1, pending: 2, mayRegister: true });
    const out = await runCommitterSweep(m.ports);
    expect(out.kind).toBe("committed");
    expect(m.order.indexOf("may-register?true")).toBeLessThan(m.order.indexOf("stage"));
  });

  it("an owed verdict is still resolved without authority (it is never re-registered)", async () => {
    const OWED: SweepDurableEntry = {
      id: "entry-owed",
      commitB64: Buffer.from(frame(COMMIT, 6, 0x5e)).toString("base64"),
      welcomeEnvelopeB64: undefined,
      welcomeMailboxesB64: [],
    };
    const m = model({ rank: 1, pending: 1, epoch: 6, owed: { entry: OWED }, mayRegister: false });
    const out = await runCommitterSweep(m.ports);
    expect(out.kind).toBe("committed");
  });
});

describe("ADR-0053 contract point 4 — heal after the verdict apply", () => {
  it("a LOST sweep asks to heal: the winner's commit moved the epoch in the verdict read", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });
    m.raceRows.push(row(50, COMMIT, 6, 0xfa));

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("lost");
    expect(m.heals).toEqual(["heal@6->7"]);
  });

  it("a WON sweep only releases what it captured: its own key was registered before the publish", async () => {
    const m = model({ rank: 0, pending: 1, epoch: 6 });

    const out = await runCommitterSweep(m.ports);

    expect(out.kind).toBe("committed");
    expect(m.heals).toEqual(["release"]);
  });
});
