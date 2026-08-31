// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The receive path's DURABILITY ORDERING — the journal cursor may LAG the
 * durable MLS state, never LEAD it.
 *
 * ## The defect these pin
 *
 * ADR-0045 made a receiver's durable proposal store a PRECONDITION for applying
 * a future by-reference commit: a peer that dropped a proposal cannot apply the
 * commit that carries it, failing `InvalidCommit(MissingProposal)` before any
 * Lakko gate runs (`crates/lakko-mls/src/group.rs`, `gate_incoming_proposal`
 * doc). That is correct for a proposal the gate REFUSED and fatal for one the
 * device LOST.
 *
 * Losing one took no adversary at all. The receive path's snapshot write was
 * best-effort and swallowed its failures, the cursor lives in a different
 * IndexedDB DATABASE (`lakko-message-log`) from the MLS snapshot (`lakko-mls`)
 * so the two writes can never be atomic, and the cursor advanced regardless. A
 * swallowed quota error — or a crash between the two writes — left the device
 * with an MLS state that never saw the proposal and a cursor already past it.
 * The committer's commit then threw, was logged and skipped, the cursor advanced
 * past THAT too, and the device sat at epoch N forever while the group moved to
 * N+1. `isActive()` does not catch it: the device is still a member, just
 * epoch-stranded, recoverable only by being removed and re-added.
 *
 * ## The rule
 *
 * Cursor BEHIND the durable state is benign and self-healing: rows are
 * re-fetched, an already-applied commit is rejected as wrong-epoch and skipped
 * exactly as the poll already skips it, an already-stored proposal is
 * de-duplicated by `ProposalRef` in `lakko-mls`, and the cursor catches up.
 * Cursor AHEAD is fatal and permanent. So: persist the durable effect first,
 * advance the cursor second, and hold the cursor when the persist does not land.
 *
 * ## The double
 *
 * A group whose DURABLE state is modelled separately from its in-memory state,
 * so a restart is a real operation (discard memory, reload from disk) rather
 * than an assertion about one. Its `processIncomingVerifyingRoles` reproduces
 * the one OpenMLS behaviour the whole defect turns on: a commit that references
 * a proposal THROWS unless that proposal is in the store. A double that applied
 * commits unconditionally would make every test below vacuous.
 *
 * ## Measured discrimination — each was run, and the failure is quoted verbatim
 *
 * With the fix reverted (the persist back to best-effort, the cursor advanced
 * unconditionally), SIX of the seven go red:
 *
 *     FAIL … > a lost proposal strands the device forever — the cursor must not
 *     pass a batch whose persist failed
 *     AssertionError: expected 10 to be +0 // Object.is equality
 *
 *     FAIL … > no interleaving of batches and storage failures can ever strand
 *     the device
 *     AssertionError: stranded BEFORE consuming {"t":"commit","needs":"A"}
 *     (cursor 10): expected true to be false // Object.is equality
 *
 *     FAIL … > a held batch is re-fetched until a write lands …
 *     AssertionError: expected 10 to be +0
 *     FAIL … > the good path costs exactly one persist per batch …
 *     AssertionError: expected 1 to be +0
 *     FAIL … > a batch of nothing but our own echoes …
 *     AssertionError: expected 1 to be +0
 *     FAIL … > re-delivering an echo row after a held batch still suppresses it
 *     AssertionError: expected 11 to be +0
 *
 * The one that stays green is "a permanently undecryptable row never wedges the
 * poll", and that is the point of it: the fix must NOT change that behaviour.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

// No IndexedDB in the node test env; the outbox round-trip is covered by
// `commit-outbox.test.ts`.
vi.mock("./commit-outbox", () => ({
  markAcked: async () => {},
  loadUnacked: async () => [],
}));

import { ChatClient } from "./client";
import type { DsClient } from "../ds/client";
import type { PushClient } from "../push/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

const GROUP_ID = new Uint8Array([0xa1, 0xb2]);

// ---------------------------------------------------------------------------
// Journal rows — a tiny JSON wire so a row's meaning is readable in a failure.
// ---------------------------------------------------------------------------

type Row =
  /** A membership proposal the receiver must STORE to later resolve a commit. */
  | { readonly t: "proposal"; readonly ref: string }
  /** A commit carrying `needs` BY REFERENCE — throws if it is not stored. */
  | { readonly t: "commit"; readonly needs: string }
  /** A row that can never be applied, on any device, ever. */
  | { readonly t: "junk" };

function payload(row: Row): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(row));
}

function decode(bytes: Uint8Array): Row {
  return JSON.parse(new TextDecoder().decode(bytes)) as Row;
}

// ---------------------------------------------------------------------------
// The device: durable state, in-memory state, and a restart between them.
// ---------------------------------------------------------------------------

interface DurableState {
  /** Proposal refs the durable snapshot holds. */
  proposals: string[];
  epoch: bigint;
}

interface Device {
  readonly group: WasmGroup;
  /** The MLS snapshot on disk. */
  readonly durable: () => DurableState;
  /** The journal position on disk — a SEPARATE store, written separately. */
  readonly durableCursor: () => number;
  readonly liveEpoch: () => bigint;
  /** `ChatClient`'s `onPersist` hook: whole-provider snapshot -> disk. */
  readonly persist: () => Promise<void>;
  readonly persistCalls: () => number;
  readonly failPersist: (fail: boolean) => void;
  /** The cursor read/write pair the poll drives. */
  readonly cursorRef: { read(): number; advance(to: number): void };
  /** Discard everything in memory and reload from disk. */
  readonly restart: () => void;
  /** Append a row to the DS journal. */
  readonly push: (row: Row) => number;
  /** Every journal row strictly above `after`, decoded. */
  readonly journalAfter: (after: number) => Row[];
  readonly ds: DsClient;
}

function device(): Device {
  let durable: DurableState = { proposals: [], epoch: 0n };
  let memProposals = new Set<string>();
  let memEpoch = 0n;
  let cursor = 0;
  let persistFails = false;
  let persistCalls = 0;
  let nextId = 9;
  const journal: { id: number; payload: Uint8Array; createdAt: string }[] = [];

  const group = {
    get groupId(): Uint8Array {
      return GROUP_ID;
    },
    get epoch(): bigint {
      return memEpoch;
    },
    get memberCount(): number {
      return 2;
    },
    members: () => [],
    processIncomingVerifyingRoles: (_id: unknown, bytes: Uint8Array): unknown => {
      const row = decode(bytes);
      if (row.t === "junk") {
        // The permanently-undecryptable class: a corrupt frame, an
        // already-applied commit re-fetched at the wrong epoch, a non-MLS
        // envelope. Nothing about the device makes it applicable later.
        throw new Error("not an MLS message");
      }
      if (row.t === "proposal") {
        // `lakko-mls` de-duplicates by `ProposalRef` before storing, so a
        // re-delivered proposal is idempotent — modelled by the Set.
        memProposals.add(row.ref);
        return {
          kind: "Proposal",
          proposal: {
            kind: "remove",
            stored: true,
            senderLeaf: 1,
            addSignatureKey: undefined,
            removeTargetLeaf: 2,
          },
        };
      }
      if (!memProposals.has(row.needs)) {
        // THE defect's payload: OpenMLS resolves a by-reference proposal
        // against the RECEIVER's own store.
        throw new Error("InvalidCommit(MissingProposal)");
      }
      memEpoch += 1n;
      memProposals.clear(); // OpenMLS empties the store on merge.
      return { kind: "Commit" };
    },
  } as unknown as WasmGroup;

  const ds = {
    fetchGroup: async (_gid: Uint8Array, opts: { after: number }) => ({
      messages: journal.filter((r) => r.id > opts.after),
      hasMore: false,
    }),
  } as unknown as DsClient;

  return {
    group,
    ds,
    durable: () => ({ proposals: [...durable.proposals], epoch: durable.epoch }),
    durableCursor: () => cursor,
    liveEpoch: () => memEpoch,
    persistCalls: () => persistCalls,
    failPersist: (fail) => {
      persistFails = fail;
    },
    persist: async () => {
      persistCalls += 1;
      if (persistFails) throw new Error("IndexedDB quota exceeded");
      durable = { proposals: [...memProposals], epoch: memEpoch };
    },
    cursorRef: {
      read: () => cursor,
      advance: (to) => {
        cursor = to;
      },
    },
    restart: () => {
      memProposals = new Set(durable.proposals);
      memEpoch = durable.epoch;
    },
    push: (row) => {
      nextId += 1;
      journal.push({ id: nextId, payload: payload(row), createdAt: `t-${nextId}` });
      return nextId;
    },
    journalAfter: (after) =>
      journal.filter((r) => r.id > after).map((r) => decode(r.payload)),
  };
}

function clientFor(d: Device): ChatClient {
  return new ChatClient(
    d.ds,
    { identityProvider: () => IDENTITY_TOKEN, onPersist: d.persist },
    { notifyGroup: async () => {} } as unknown as PushClient,
  );
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("receive-path durability ordering", () => {
  // -------------------------------------------------------------------------
  // THE HEADLINE. Everything else in this file is a guard around it.
  // -------------------------------------------------------------------------
  it("a lost proposal strands the device forever — the cursor must not pass a batch whose persist failed", async () => {
    const d = device();
    const chat = clientFor(d);

    const proposalId = d.push({ t: "proposal", ref: "P" });

    // The device receives the proposal while its snapshot store is broken.
    d.failPersist(true);
    const first = await chat.pollMessages(null, d.group, 0, 0, d.cursorRef);

    // Meanwhile the elected committer sweeps it into a commit that carries it
    // BY REFERENCE — the artifact this device can only apply if it kept the
    // proposal.
    d.push({ t: "commit", needs: "P" });

    // The proposal IS in memory — the apply succeeded, only the write failed.
    expect(first.events.map((e) => e.kind)).toEqual(["membershipProposal"]);
    // ...and the cursor MUST NOT have moved past it, because the durable state
    // did not. This is the whole rule.
    expect(d.durableCursor()).toBe(0);
    expect(d.durable().proposals).toEqual([]);
    expect(proposalId).toBe(10);

    // The storage failure clears, and the tab is reloaded.
    d.failPersist(false);
    d.restart();
    expect(d.liveEpoch()).toBe(0n);

    // The poll resumes from the DURABLE cursor — which is where the durable MLS
    // state is, so the proposal is re-delivered ahead of the commit.
    const second = await chat.pollMessages(
      null,
      d.group,
      d.durableCursor(),
      0,
      d.cursorRef,
    );

    expect(
      second.events.map((e) => e.kind),
      "the proposal must be re-delivered before the commit that references it",
    ).toEqual(["membershipProposal", "system"]);
    expect(d.liveEpoch(), "the device must still apply the commit after a restart").toBe(1n);
    expect(d.durable().epoch).toBe(1n);
    expect(d.durableCursor()).toBe(11);
  });

  it("a held batch is re-fetched until a write lands, and the cursor catches up in one step", async () => {
    // The self-healing half. A held cursor is what makes the rows come back, so
    // every attempt re-applies them and retries the write; the cursor moves only
    // when one lands, and then over everything held back at once.
    //
    // This is also why the gate's STICKY flag has no test of its own: holding
    // the cursor guarantees the touching rows return, so the gate re-marks
    // itself and the flag never changes an outcome under the current call
    // shape. It is kept as defence in depth — see `chat/durable-cursor.ts`.
    const d = device();
    const chat = clientFor(d);

    d.push({ t: "proposal", ref: "P" });
    d.failPersist(true);
    await chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(d.durableCursor()).toBe(0);

    d.push({ t: "junk" });
    await chat.pollMessages(null, d.group, d.durableCursor(), 0, d.cursorRef);
    expect(d.durableCursor()).toBe(0);
    expect(d.persistCalls(), "every attempt retries the write").toBe(2);

    // The store recovers: the very next batch persists and the cursor catches
    // up in one step, over everything held back.
    d.failPersist(false);
    await chat.pollMessages(null, d.group, d.durableCursor(), 0, d.cursorRef);
    expect(d.durableCursor()).toBe(11);
    expect(d.durable().proposals).toEqual(["P"]);
  });

  it("a permanently undecryptable row never wedges the poll", async () => {
    // The trap the ordering rule could have set: if a row that can never apply
    // held the cursor, the poll would re-fetch it forever. Only a PERSIST
    // failure holds; a row failure is skipped exactly as before.
    const d = device();
    const chat = clientFor(d);

    d.push({ t: "junk" });
    d.push({ t: "junk" });

    const r = await chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(r.events).toEqual([]);
    expect(d.durableCursor()).toBe(11);

    // And it is genuinely gone: a re-poll from the new cursor sees nothing.
    const again = await chat.pollMessages(null, d.group, d.durableCursor(), 0, d.cursorRef);
    expect(again.events).toEqual([]);
    expect(d.durableCursor()).toBe(11);
  });

  it("the good path costs exactly one persist per batch, and none at all for an empty one", async () => {
    const d = device();
    const chat = clientFor(d);

    // An empty batch persists nothing — the common long-poll timeout.
    await chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(d.persistCalls()).toBe(0);
    expect(d.durableCursor()).toBe(0);

    d.push({ t: "proposal", ref: "P" });
    d.push({ t: "commit", needs: "P" });
    await chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(d.persistCalls()).toBe(1);
    expect(d.durableCursor()).toBe(11);
  });

  it("a batch of nothing but our own echoes touches no MLS state and persists nothing", async () => {
    // Echo rows never reach OpenMLS, so they cannot have mutated anything —
    // which is exactly why the touched flag is set where the row is HANDED to
    // the core, not merely when a batch is non-empty.
    const d = device();
    const chat = clientFor(d);

    const id = d.push({ t: "proposal", ref: "P" });
    chat.markOwnPublishedCommit(id);

    await chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(d.persistCalls()).toBe(0);
    expect(d.durableCursor()).toBe(10);
  });

  it("re-delivering an echo row after a held batch still suppresses it", async () => {
    // The echo registry is consumed on match. If it were consumed on a batch
    // whose cursor was then HELD, the re-fetch would feed our own row to
    // OpenMLS and log a `CannotDecryptOwnMessage` per poll, forever.
    const d = device();
    const chat = clientFor(d);

    d.push({ t: "proposal", ref: "P" });
    const ownId = d.push({ t: "commit", needs: "P" });
    chat.markOwnPublishedCommit(ownId);

    d.failPersist(true);
    await chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(d.durableCursor()).toBe(0);

    d.failPersist(false);
    d.restart();
    const second = await chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    // Only the proposal surfaces; our own commit is still suppressed.
    expect(second.events.map((e) => e.kind)).toEqual(["membershipProposal"]);
    expect(d.liveEpoch()).toBe(0n);
    expect(d.durableCursor()).toBe(11);
  });

  it("no interleaving of batches and storage failures can ever strand the device", async () => {
    // A property rather than a scenario, and stated as the OUTCOME rather than
    // as the mechanism: after EVERY step, killing the tab right now and
    // replaying the journal from the durable cursor against the durable
    // snapshot must still apply every commit. That is precisely what
    // "the cursor never leads the state" buys, and it is checkable without
    // reference to how the guard is implemented.
    const d = device();
    const chat = clientFor(d);
    // The failure pattern is EXPLICIT, not alternating. Alternating happens to
    // let a successful write land between a proposal and the commit that
    // references it — and a successful write captures the still-live in-memory
    // state, repairing the gap before the replay can see it. The hazard needs a
    // commit arriving while the durable snapshot is behind, so each entry below
    // is (row, does this batch's write fail).
    const seq: [Row, boolean][] = [
      [{ t: "proposal", ref: "A" }, true],
      [{ t: "commit", needs: "A" }, true],
      [{ t: "junk" }, false],
      [{ t: "proposal", ref: "B" }, true],
      [{ t: "commit", needs: "B" }, false],
      [{ t: "proposal", ref: "C" }, true],
      [{ t: "commit", needs: "C" }, true],
    ];
    for (const [row, failWrite] of seq) {
      d.push(row);
      d.failPersist(failWrite);
      // The crash is checked BEFORE the row is consumed, which is the only
      // moment it can be caught: once a commit has been polled, whether it
      // applied or threw, it is no longer above the cursor and the replay has
      // nothing to fail on. Checking after the poll is what made an earlier
      // draft of this test pass against the unfixed client.
      expect(
        strandedAfterCrash(d),
        `stranded BEFORE consuming ${JSON.stringify(row)} (cursor ${d.durableCursor()})`,
      ).toBe(false);
      await chat.pollMessages(null, d.group, d.durableCursor(), 0, d.cursorRef);
      expect(
        strandedAfterCrash(d),
        `stranded after ${JSON.stringify(row)} (cursor ${d.durableCursor()}, durable proposals [${d.durable().proposals.join(",")}], durable epoch ${d.durable().epoch})`,
      ).toBe(false);
    }
  });
});

/**
 * Would a crash RIGHT NOW leave the device unable to apply a later commit?
 *
 * Replays the journal from the durable cursor against a scratch copy of the
 * durable proposal set — exactly what the device would do on its next launch —
 * and reports whether any commit finds its referenced proposal missing. A
 * `true` here is the stranding: no further row can ever repair it, because the
 * proposal it needs is behind a cursor that will never go back.
 */
function strandedAfterCrash(d: Device): boolean {
  const held = new Set(d.durable().proposals);
  for (const row of d.journalAfter(d.durableCursor())) {
    if (row.t === "proposal") held.add(row.ref);
    if (row.t === "commit") {
      if (!held.has(row.needs)) return true;
      held.clear();
    }
  }
  return false;
}
