// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S4a obligation 5 — the four membership paths become
 * propose-or-sweep, and a group with no role table keeps direct commits.
 *
 * `chat/sweep.test.ts` pins the sweep ALGORITHM over its ports. This file pins
 * the thing that decides which algorithm runs at all: the branch in
 * `ContactsClient`, driven through the real `withGroupMlsLock` regions against
 * faithful wasm-`Group` doubles.
 *
 * ## Which of these actually discriminate (MEASURED — each probe was run)
 *
 * 1. **`a group with NO role table still direct-commits`** — with
 *    `usesProposalPath` forced to `true`:
 *
 *        ContactsError: group.proposeRemoveMember is not a function
 *
 *    i.e. a legacy group would be routed into a path whose peers' receipt gates
 *    drop every proposal it emits, so nothing it asks for would ever apply.
 *
 * 2. **`a role-table group PROPOSES rather than committing`** — with
 *    `usesProposalPath` forced to `false`. FIVE tests went red; the first:
 *
 *        AssertionError: expected { kind: 'committed', …(2) } to match object
 *        { kind: 'proposed' }
 *        -   "kind": "proposed",
 *        +   "kind": "committed",
 *
 *    That `committed` is the eager merge this whole ADR exists to remove from
 *    the concurrent path.
 *
 * 3. **`the owner's own change still applies immediately`** — with the
 *    `rank !== 0` gate in `runCommitterSweep` changed to reject rank 0. THREE tests
 *    went red:
 *
 *        AssertionError: expected 'not-committer' to be 'committed'
 *
 *    The owner would publish a request nobody ever sweeps, so "tap, done"
 *    becomes an indefinite wait in the overwhelmingly common case.
 *
 * 4. **`a plain member's request waits for a committer`** — with the
 *    `rank === undefined` gate made permissive:
 *
 *        AssertionError: expected { kind: 'committed', …(3) } to deeply equal
 *        { kind: 'not-committer', rank: undefined }
 *
 *    A plain member's commit is refused by every peer's role gate, so it would
 *    eager-merge alone at N+1 — this ADR's own hazard through the back door.
 *
 * 5. **S4b — `an admin sweeps when nobody better placed is running`** and
 *    `an admin defers to a fresh beacon from a lower rank`. The admin arm is
 *    the whole of S4b: an admin-only group used to wait indefinitely.
 *
 * 6. **`a chat message ordered below our own commit is SURFACED, not
 *    swallowed`** — the adapter half of a defect that WAS live. The port
 *    implementation read `group.processIncomingVerifyingRoles(identity,
 *    payload)` and threw the result away, so every row the verdict consumed
 *    (and the shared cursor then moved past) lost its plaintext silently. With
 *    the discard restored:
 *
 *        FAIL … > the verdict apply surfaces the rows it consumes > a chat
 *        message ordered below our own commit is SURFACED, not swallowed
 *        AssertionError: expected [] to have a length of 1 but got +0
 *
 * 7. **`a row whose apply THROWS surfaces nothing`** — with the surfacing
 *    hoisted ABOVE the apply, so a skipped row leaks an event for a message
 *    that was never decrypted:
 *
 *        FAIL … > a row whose apply THROWS surfaces nothing
 *        AssertionError: expected [ { kind: 'system', …(3) } ] to deeply equal []
 *
 *    plus a second failure on the case above (two events for one row).
 *
 * BOUNDARY GUARD, not a discriminator: `the sweep never runs on a legacy
 * group`. It passes under every probe above; its job is to catch a future
 * change that let the sweep reach a group whose core would refuse it.
 */

import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import {
  fakeAccessSigner,
  fakeRegistrarAuthority,
  fakeRootAuthority,
} from "../ds/access-test-doubles";
import { dsAccess } from "../ds/access";

// No IndexedDB in the node test env; the store round-trip is covered by
// `src/chat/commit-outbox.test.ts`.
const markAcked = vi.fn(async () => {});
const loadUnacked = vi.fn(async () => [] as unknown[]);
const deleteEntry = vi.fn(async () => {});
vi.mock("../chat/commit-outbox", () => ({
  markAcked: (...a: unknown[]) => markAcked(...(a as [])),
  loadUnacked: (...a: unknown[]) => loadUnacked(...(a as [])),
  deleteEntry: (...a: unknown[]) => deleteEntry(...(a as [])),
}));

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    IdentityPseudonym: {
      fromIdentity: (pk: Uint8Array) => ({
        asBytes: (): Uint8Array => new Uint8Array([0xbb, pk[0] ?? 0]),
      }),
    },
    // ADR-0045 — the sweep's verdict apply now routes an applied row through
    // `chat/client.ts`'s `toEvent`, which reaches the chat-envelope parser for
    // an Application message. The other three pre-filters in that ladder
    // (call signaling, the push-routing secret, the committer beacon) all
    // short-circuit on the first byte or the length before touching wasm, so
    // this is the only export they need.
    // Present so the PRE-S7 approve path would run to completion here rather
    // than dying on a missing mock — the S7 assertions below have to fail on
    // what the path DOES, not on the double.
    wrapWelcomeRendezvous: (ref: Uint8Array, welcome: Uint8Array): Uint8Array =>
      new Uint8Array([0xcc, ref[0] ?? 0, ...welcome]),
    parseBodyEnvelope: (bytes: Uint8Array) => ({
      kind: "chat",
      text: new TextDecoder().decode(bytes),
      attachmentCount: 0,
      free: (): void => {},
    }),
  }),
}));

import { ContactsClient } from "./client";
import type { ActingCommitter } from "../chat/committer-beacon";
import type { IncomingChatEvent } from "../chat/client";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;
const OWNER = new Uint8Array(32).fill(1);

/** A group id no other case reuses — `withGroupMlsLock`'s queue is
 *  module-level, so a shared id would let one failure wedge the next case. */
let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xc0, nextGid]);
}

interface Fake {
  group: WasmGroup;
  order: string[];
  epoch: () => bigint;
  pending: () => number;
  staged: () => boolean;
}

/**
 * A faithful MLS group double.
 *
 * The fidelity that matters: `propose*` does NOT advance the epoch and DOES
 * grow the pending set; `stagePendingProposals` does NOT advance the epoch;
 * only `mergeStagedSweep` does, and only a merge empties the pending set. A
 * double that advanced the epoch on a proposal would make every assertion here
 * vacuous, because the code under test reads exactly that.
 */
function fakeGroup(opts: {
  gid: Uint8Array;
  roleTable: boolean;
  rank?: number | undefined;
  pending?: number;
  approveThrows?: boolean;
}): Fake {
  const order: string[] = [];
  let epoch = 6n;
  let pending = opts.pending ?? 0;
  let staged = false;
  const base: Record<string, unknown> = {
    get groupId(): Uint8Array {
      return opts.gid;
    },
    get epoch(): bigint {
      return epoch;
    },
    dsAccessSigner: fakeAccessSigner(() => epoch),
    get memberCount(): number {
      return 3;
    },
    pendingProposals: () =>
      Array.from({ length: pending }, () => ({
        kind: "remove",
        senderLeaf: 1,
        stored: true,
        addSignatureKey: undefined,
        removeTargetLeaf: 4,
      })),
    // The direct-commit paths. Eager merge, exactly like the core.
    removeMember: vi.fn(() => {
      order.push("removeMember");
      epoch += 1n;
      return new Uint8Array([1, 2, 3]);
    }),
    addMember: vi.fn(() => {
      order.push("addMember");
      epoch += 1n;
      return { commit: new Uint8Array([1]), welcome: new Uint8Array([2]) };
    }),
  };
  if (opts.roleTable) {
    Object.assign(base, {
      roleTableOwner: () => OWNER,
      roleTableAdmins: () => new Uint8Array(0),
      proposeRemoveMember: vi.fn(() => {
        order.push("proposeRemoveMember");
        pending += 1; // and the epoch deliberately does NOT move
        return new Uint8Array([0xaa]);
      }),
      proposeAddMember: vi.fn(() => {
        order.push("proposeAddMember");
        pending += 1;
        return new Uint8Array([0xab]);
      }),
      // ADR-0045 S7 — the approve is a proposal now: bytes to publish, the
      // pending set grows, and the epoch deliberately does NOT move. A double
      // that still returned `{ commit, welcome }` would make every assertion
      // below vacuous.
      approveExternalJoin: vi.fn((_id: unknown, ref: Uint8Array) => {
        if (opts.approveThrows === true) {
          throw new Error("external join approve refused: not owner or admin");
        }
        order.push("approveExternalJoin");
        pending += 1;
        return new Uint8Array([0xac, ref[0] ?? 0]);
      }),
      rejectExternalJoin: vi.fn((_id: unknown, _ref: Uint8Array) => {
        order.push("rejectExternalJoin");
      }),
      pendingExternalJoins: vi.fn(() => new Uint8Array(0)),
      ownCommitterRank: () => opts.rank,
      hasStagedSweep: () => staged,
      // ADR-0053 — the signer of the epoch a staged sweep moves to.
      dsStagedAccessSigner: (_id: unknown) =>
        staged ? fakeAccessSigner(() => epoch + 1n)(undefined) : undefined,
      stagePendingProposals: vi.fn(() => {
        if (pending === 0) return undefined;
        order.push("stage");
        staged = true;
        return {
          commit: new Uint8Array([0x5e]),
          welcome: undefined,
          swept: () => [],
        };
      }),
      mergeStagedSweep: vi.fn(() => {
        order.push("merge");
        staged = false;
        epoch += 1n;
        pending = 0;
      }),
      abandonStagedSweep: vi.fn(() => {
        order.push("abandon");
        staged = false;
      }),
      processIncomingVerifyingRoles: vi.fn(() => {
        order.push("apply");
        return { kind: "Commit" };
      }),
    });
  } else {
    // A legacy group: reading the role table throws, exactly like a real wasm
    // `Group` with no `0xFF02` extension. `ownCommitterRank` is still PRESENT
    // and returns `undefined` — the real method exists on every `Group` — so
    // the short-circuit under test has to be a real decision rather than an
    // accidental TypeError.
    Object.assign(base, {
      roleTableOwner: () => {
        throw new Error("no role table");
      },
      ownCommitterRank: () => undefined,
      hasStagedSweep: () => false,
    });
  }
  return {
    group: base as unknown as WasmGroup,
    order,
    epoch: () => epoch,
    pending: () => pending,
    staged: () => staged,
  };
}

function client(
  order: string[],
  ds: Partial<DsClient>,
  discovery: Partial<DiscoveryClient> = {},
): ContactsClient {
  return new ContactsClient(
    discovery as unknown as DiscoveryClient,
    ds as unknown as DsClient,
    {
      identityProvider: () => IDENTITY_TOKEN,
      onPersist: async () => {
        order.push("persist");
      },
      onPersistWithCommit: async () => {
        order.push("record");
      },
    },
  );
}

function ds(order: string[], journal: unknown[] = []): Partial<DsClient> {
  let id = 100;
  return {
    // ADR-0053 — the epoch registration a commit and a sweep run before they
    // publish. Accepted, and deliberately not logged into `order`: the
    // ordering is pinned in `contacts/ds-access-commit.test.ts`.
    registerGroupEpoch: vi.fn(async () => {}),
    publishGroup: vi.fn(async () => {
      order.push("publish");
      return { id: (id += 1), createdAt: "t" };
    }),
    publishMailbox: vi.fn(async () => {
      order.push("publish-mailbox");
      return { id: (id += 1), createdAt: "t" };
    }),
    fetchGroup: vi.fn(async () => ({
      order: order.push("fetch"),
      messages: journal,
      hasMore: false,
    })) as unknown as DsClient["fetchGroup"],
  };
}

// ADR-0057 — the sweep stages only on a device that holds the journal's root
// or a registrar seed. These tests are not about ADR-0057, so every device in
// them holds a registrar seed (the DS double accepts every registration).
beforeAll(() => {
  dsAccess.setAuthorityFactory({
    root: () => fakeRootAuthority(),
    registrar: () => fakeRegistrarAuthority("reg", 2),
  });
});
afterAll(() => {
  dsAccess.setAuthorityFactory(null);
});

afterEach(() => {
  vi.clearAllMocks();
});

const CURSOR = { read: (): number => 0, advance: (): void => {} };
const NOOP = (): void => {};
/** ADR-0045 — the timeline sink the verdict apply surfaces into. These cases
 *  put nothing below the sweep's own commit, so nothing is applied and nothing
 *  is surfaced; the surfacing itself is pinned in its own describe below. */
const NO_EVENTS = (): void => {};
/** ADR-0045 S4b — the acting-committer port. `ACT` is what the real fold gives
 *  the owner unconditionally and gives an admin that has heard nothing; `defer`
 *  is what a fresh beacon from a strictly better-placed candidate gives. */
const ACT = async (): Promise<ActingCommitter> => ({ kind: "act" });
const DEFER_TO = (toRank: number) => async (): Promise<ActingCommitter> => ({
  kind: "defer",
  toRank,
});

describe("ADR-0045 obligation 5 — which path a group takes", () => {
  it("a group with NO role table still direct-commits", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: false });
    const c = client(g.order, ds(g.order));

    const out = await c.removeMemberFromGroup(null, g.group, 4);

    expect(out).toMatchObject({ kind: "committed" });
    // create -> record -> publish, the ADR-0040 sequence, byte-for-byte as it
    // was. A legacy group has no owner, hence no committer, and every peer's
    // receipt gate would drop a proposal it emitted — so routing it into the
    // proposal path would mean nothing it asks for ever applies.
    expect(g.order).toEqual(["removeMember", "record", "publish"]);
    expect(g.epoch()).toBe(7n);
  });

  it("a role-table group PROPOSES rather than committing", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: undefined });
    const c = client(g.order, ds(g.order));

    const out = await c.removeMemberFromGroup(null, g.group, 4);

    expect(out).toMatchObject({ kind: "proposed" });
    expect(g.order).toEqual(["proposeRemoveMember", "persist", "publish"]);
    // The whole point of the option: the epoch did NOT move, so a proposal that
    // loses a race costs nothing, where a commit that loses one forks its
    // author off the group.
    expect(g.epoch()).toBe(6n);
    expect(g.pending()).toBe(1);
    // And NO outbox entry — there is nothing to roll back, so writing one would
    // put a never-committed artifact into the restart sweep's re-publish set.
    expect(g.order).not.toContain("record");
  });

  it("the sweep never runs on a legacy group", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: false });
    const c = client(g.order, ds(g.order));

    const out = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT, NO_EVENTS);

    // `ownCommitterRank` is absent on a legacy group, so the adapter's read
    // throws and the sweep must not proceed regardless.
    expect(out.kind).toBe("not-committer");
    expect(g.order).toEqual([]);
  });
});

describe("ADR-0045 — who sweeps", () => {
  it("the owner's own change still applies immediately", async () => {
    // UX §2 — the load-bearing common case. The owner is rank 0, so its own
    // device is the acting committer and sweeps its own request with no round
    // trip to anyone: the user experience stays "tap, done".
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 0 });
    const c = client(g.order, ds(g.order));

    const proposed = await c.removeMemberFromGroup(null, g.group, 4);
    expect(proposed).toMatchObject({ kind: "proposed" });

    const swept = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT, NO_EVENTS);

    expect(swept.kind).toBe("committed");
    expect(g.order).toEqual([
      "proposeRemoveMember",
      "persist",
      "publish",
      "fetch", // the stand-down re-read
      "stage",
      "record", // ADR-0040, BEFORE the publish
      "publish",
      "fetch", // the verdict read
      "merge",
      "persist",
    ]);
    expect(g.epoch()).toBe(7n);
    expect(g.pending()).toBe(0);
    expect(g.staged()).toBe(false);
  });

  it("an admin defers to a fresh beacon from a lower rank — it does not sweep", async () => {
    // ADR-0045 S4b. An admin (rank 1) that hears the owner (rank 0) beacon
    // inside the freshness window stands down: only a STRICTLY lower rank
    // defers it, and rank 0 is strictly lower. Nothing is staged, so nothing
    // can race the owner's own sweep.
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 1 });
    const c = client(g.order, ds(g.order));

    await c.removeMemberFromGroup(null, g.group, 4);
    const swept = await c.sweepIfCommitter(
      null,
      g.group,
      CURSOR,
      NOOP,
      DEFER_TO(0),
      NO_EVENTS,
    );

    expect(swept).toEqual({ kind: "deferred", rank: 1, toRank: 0 });
    expect(g.pending()).toBe(1);
    expect(g.epoch()).toBe(6n);
    expect(g.order).not.toContain("stage");
    // The stand-down happens BEFORE the journal re-read, so a deferring device
    // does not even reach the network.
    expect(g.order).not.toContain("fetch");
  });

  it("an admin sweeps when nobody better placed is running", async () => {
    // ADR-0045 S4b — the whole point of the slice. Under S4a this admin
    // returned `not-committer` and the group waited for an owner who might
    // never open the app. Hearing nothing means ACT.
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 1 });
    const c = client(g.order, ds(g.order));

    await c.removeMemberFromGroup(null, g.group, 4);
    const swept = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT, NO_EVENTS);

    expect(swept.kind).toBe("committed");
    expect(g.epoch()).toBe(7n);
    expect(g.pending()).toBe(0);
    expect(g.staged()).toBe(false);
    // The stand-down re-read is still there — the beacon removed the WAIT,
    // not the RACE.
    expect(g.order).toContain("fetch");
  });

  it("a plain member's request waits for a committer", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: undefined });
    const c = client(g.order, ds(g.order));

    await c.removeMemberFromGroup(null, g.group, 4);
    const swept = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT, NO_EVENTS);

    // `undefined` and `0` are never interchangeable: `0` is the owner.
    expect(swept).toEqual({ kind: "not-committer", rank: undefined });
    expect(g.pending()).toBe(1);
  });

  it("an admin's proposal reaches the owner and the owner sweeps it", async () => {
    // The two devices share nothing but the DS journal, which is modelled here
    // as the admin's published proposal arriving in the owner's pending set —
    // which is what the owner's receipt gate does with it.
    const admin = fakeGroup({ gid: freshGid(), roleTable: true, rank: 1 });
    const adminClient = client(admin.order, ds(admin.order));
    await adminClient.removeMemberFromGroup(null, admin.group, 4);
    expect(admin.pending()).toBe(1);

    // The owner, holding that same proposal.
    const owner = fakeGroup({
      gid: freshGid(),
      roleTable: true,
      rank: 0,
      pending: 1,
    });
    const ownerClient = client(owner.order, ds(owner.order));

    const swept = await ownerClient.sweepIfCommitter(
      null,
      owner.group,
      CURSOR,
      NOOP,
      ACT,
      NO_EVENTS,
    );

    expect(swept.kind).toBe("committed");
    expect(owner.epoch()).toBe(7n);
    expect(owner.pending()).toBe(0);
  });
});

describe("ADR-0045 — the restart sweep resolves an owed verdict FIRST", () => {
  it("skips a staged-sweep entry in the legacy re-publish loop", async () => {
    // A sweep entry is not re-publishable on the ADR-0040 path: its device is
    // durably at N (not N+1), and its Welcome must not reach the wire until the
    // verdict. Re-publishing it there would publish that Welcome unconditionally
    // — the three-party fork.
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 0 });
    const order: string[] = [];
    loadUnacked.mockResolvedValueOnce([
      {
        id: "sweep-1",
        accountId: "a",
        groupId: "g",
        targetEpoch: "6",
        commitB64: "AQID",
        secondArtifactB64: "BAUG",
        secondArtifactKind: "welcome",
        sweepVerdictPending: true,
        acked: false,
      },
    ]);
    const c = client(order, ds(order));

    // No cursor supplied -> the owed-verdict step is skipped, and the legacy
    // loop must still refuse to touch the sweep entry.
    await c.sweepPendingCommits(g.group, NOOP);

    expect(order).not.toContain("publish");
    expect(order).not.toContain("publish-mailbox");
  });

  it("re-publishes an ordinary direct-commit entry as before", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: false });
    const order: string[] = [];
    loadUnacked.mockResolvedValueOnce([
      {
        id: "direct-1",
        accountId: "a",
        groupId: "g",
        targetEpoch: "7",
        commitB64: "AQID",
        acked: false,
      },
    ]);
    const c = client(order, ds(order));

    await c.sweepPendingCommits(g.group, NOOP);

    expect(order).toContain("publish");
  });
});

describe("ADR-0045 — the verdict apply surfaces the rows it consumes", () => {
  /**
   * The adapter half of the defect.
   *
   * `chat/sweep.ts` hands the whole journal row to `applyIncoming`; this pins
   * that the adapter turns it into a timeline event through the SAME `toEvent`
   * the poll uses, rather than applying it and discarding the plaintext. Those
   * rows are the ones the poll will never fetch again — the sweep advances the
   * shared cursor over them — so a discarded event is a message lost from the
   * UI for good.
   */

  /** A DS that is EMPTY for the stand-down re-read and carries `rows` for the
   *  verdict read — the real shape of the race, where another member's message
   *  lands between the two. */
  function racingDs(
    order: string[],
    rows: { id: number; payload: Uint8Array; createdAt: string }[],
  ): Partial<DsClient> {
    let id = 100;
    let fetches = 0;
    return {
      registerGroupEpoch: vi.fn(async () => {}),
      publishGroup: vi.fn(async () => {
        order.push("publish");
        return { id: (id += 1), createdAt: "t" };
      }),
      publishMailbox: vi.fn(async () => {
        order.push("publish-mailbox");
        return { id: (id += 1), createdAt: "t" };
      }),
      fetchGroup: vi.fn(async () => {
        fetches += 1;
        order.push("fetch");
        return { messages: fetches === 1 ? [] : rows, hasMore: false };
      }) as unknown as DsClient["fetchGroup"],
    };
  }

  /** Make the group's receive path decode `text` for whatever row it is given,
   *  so the surfaced event carries something identifiable. */
  function applyingChat(g: Fake, text: string): void {
    (g.group as unknown as Record<string, unknown>).processIncomingVerifyingRoles =
      vi.fn(() => {
        g.order.push("apply");
        return {
          kind: "Application",
          applicationData: new TextEncoder().encode(text),
          senderLeaf: 2,
        };
      });
  }

  it("a chat message ordered below our own commit is SURFACED, not swallowed", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 0, pending: 1 });
    applyingChat(g, "hello from the race window");
    const c = client(g.order, racingDs(g.order, [
      { id: 50, payload: new Uint8Array([0x02]), createdAt: "2026-08-25T00:00:05Z" },
    ]));

    const surfaced: IncomingChatEvent[] = [];
    const swept = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT, (e) =>
      surfaced.push(e),
    );

    expect(swept.kind).toBe("committed");
    expect(surfaced).toHaveLength(1);
    // Not a content-less system row: the actual plaintext, attributed to its
    // author's leaf, stamped with the DS row's own id and timestamp.
    expect(surfaced[0]).toMatchObject({
      kind: "application",
      messageId: 50,
      text: "hello from the race window",
      senderLeaf: 2,
      createdAt: "2026-08-25T00:00:05Z",
    });
  });

  it("a row whose apply THROWS surfaces nothing", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: 0, pending: 1 });
    (g.group as unknown as Record<string, unknown>).processIncomingVerifyingRoles =
      vi.fn(() => {
        throw new Error("wrong epoch");
      });
    const c = client(g.order, racingDs(g.order, [
      { id: 50, payload: new Uint8Array([0xff]), createdAt: "t-50" },
    ]));

    const surfaced: IncomingChatEvent[] = [];
    const swept = await c.sweepIfCommitter(null, g.group, CURSOR, NOOP, ACT, (e) =>
      surfaced.push(e),
    );

    // The row was skipped exactly as the poll skips it, so it was never
    // decrypted — there is no plaintext to surface and inventing one would be a
    // lie. The verdict is unaffected.
    expect(swept.kind).toBe("committed");
    expect(surfaced).toEqual([]);
  });
});


describe("ADR-0045 S7 — approving an external join is a PROPOSAL", () => {
  const REF = new Uint8Array(32).fill(0xd1);

  it("publishes exactly ONE artifact, advances no epoch, and records no outbox entry", async () => {
    // THE headline. Before S7 this path eager-merged N -> N+1, published a
    // commit AND a rendezvous, and wrote a two-artifact ADR-0040 outbox entry
    // to make that durable. All three are gone: a proposal advances nothing,
    // so there is nothing to make durable and nothing to roll back.
    const order: string[] = [];
    const f = fakeGroup({ gid: freshGid(), roleTable: true, rank: 0 });
    const c = client(order, ds(order));

    const out = await c.approveExternalJoin(null, f.group, REF);

    expect(out).toEqual({ kind: "proposed", proposalMessageId: 101 });
    expect(f.epoch()).toBe(6n);
    expect(order.filter((o) => o === "publish")).toHaveLength(1);
    expect(order).not.toContain("record");
    expect(f.pending()).toBe(1);
  });

  it("does not drain the pending set first — an approve composes with what is pending", async () => {
    // The pre-check that gated an approve on an empty pending set is gone with
    // the direct commit it guarded. Under ADR-0045 a pending proposal is the
    // NORMAL state, so an admin deferring to a live owner could essentially
    // never approve.
    const order: string[] = [];
    const f = fakeGroup({ gid: freshGid(), roleTable: true, rank: 1, pending: 2 });
    const c = client(order, ds(order));

    const out = await c.approveExternalJoin(null, f.group, REF);

    expect(out.kind).toBe("proposed");
    expect(order).not.toContain("stage");
    expect(order).not.toContain("merge");
    expect(f.epoch()).toBe(6n);
  });

  it("a refused approve publishes nothing and consumes nothing", async () => {
    // The core keeps the request pending on a refusal so the admin can retry;
    // the client must not turn that into a publish, an outbox entry, or a
    // local reject that would discard a stranger's request nobody can get back.
    const order: string[] = [];
    const f = fakeGroup({
      gid: freshGid(),
      roleTable: true,
      rank: undefined,
      approveThrows: true,
    });
    const c = client(order, ds(order));

    await expect(c.approveExternalJoin(null, f.group, REF)).rejects.toThrow(
      /not owner or admin/,
    );
    expect(order).not.toContain("publish");
    expect(order).not.toContain("record");
    expect(order).not.toContain("rejectExternalJoin");
    expect(f.epoch()).toBe(6n);
  });
});

describe("a refused persist publishes no proposal", () => {
  // The proposal lives in this device's pending store. A persist refused
  // because the account's stores were sealed by Forget device, the session
  // ended, or the disk failed used to be swallowed and the proposal published
  // anyway; the rule is that a refused persist means nothing was published.
  function refusingClient(order: string[]): ContactsClient {
    return new ContactsClient(
      {} as unknown as DiscoveryClient,
      ds(order) as unknown as DsClient,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersist: async () => {
          order.push("persist");
          throw new Error("the local stores of this account are sealed");
        },
        onPersistWithCommit: async () => {
          order.push("record");
        },
      },
    );
  }

  it("a removal proposal is not published", async () => {
    const g = fakeGroup({ gid: freshGid(), roleTable: true, rank: undefined });
    const c = refusingClient(g.order);
    await expect(c.removeMemberFromGroup(null, g.group, 4)).rejects.toBeDefined();
    expect(g.order).toEqual(["proposeRemoveMember", "persist"]);
  });

  it("an approve of an external join is not published", async () => {
    const f = fakeGroup({ gid: freshGid(), roleTable: true, rank: 0 });
    const c = refusingClient(f.order);
    await expect(c.approveExternalJoin(null, f.group, new Uint8Array(32).fill(0xd2))).rejects.toBeDefined();
    expect(f.order).toEqual(["approveExternalJoin", "persist"]);
  });
});

describe("ADR-0050 2.5a point 4 — every own proposal records its intent", () => {
  class MemoryStorage {
    private readonly map = new Map<string, string>();
    get length(): number {
      return this.map.size;
    }
    key(i: number): string | null {
      return [...this.map.keys()][i] ?? null;
    }
    getItem(k: string): string | null {
      return this.map.get(k) ?? null;
    }
    setItem(k: string, v: string): void {
      this.map.set(k, v);
    }
    removeItem(k: string): void {
      this.map.delete(k);
    }
  }
  const ACCOUNT_ID = new Uint8Array(16).fill(0x0a);
  const SECRETS = { accountId: ACCOUNT_ID } as unknown as Parameters<
    ContactsClient["removeMemberFromGroup"]
  >[0];
  const OWN_ACCOUNT = new Uint8Array(32).fill(0x11);
  const OWN_LEAF = new Uint8Array(32).fill(0x12);
  const BOB_ACCOUNT = new Uint8Array(32).fill(0x21);
  const BOB_LEAF = new Uint8Array(32).fill(0x22);
  const b64 = (u: Uint8Array): string => btoa(String.fromCharCode(...u));

  beforeEach(() => {
    (globalThis as unknown as { localStorage: unknown }).localStorage = new MemoryStorage();
  });
  afterEach(() => {
    delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
  });

  /** A role-table group whose roster is us (leaf 0) and Bob (leaf 4). */
  function rosterGroup(): Fake {
    const f = fakeGroup({ gid: freshGid(), roleTable: true, rank: undefined });
    Object.assign(f.group as unknown as Record<string, unknown>, {
      members: () => [
        { leafIndex: 0, signaturePublicKey: OWN_LEAF, accountKey: OWN_ACCOUNT },
        { leafIndex: 4, signaturePublicKey: BOB_LEAF, accountKey: BOB_ACCOUNT },
      ],
    });
    return f;
  }

  function ownClient(order: string[]): ContactsClient {
    return new ContactsClient(
      {} as unknown as DiscoveryClient,
      ds(order) as unknown as DsClient,
      {
        identityProvider: () =>
          ({ accountPublicKey: OWN_ACCOUNT, signaturePublicKey: OWN_LEAF }) as unknown as WasmIdentity,
        onPersist: async () => {},
        onPersistWithCommit: async () => {},
      },
    );
  }

  async function intents(f: Fake): Promise<unknown[]> {
    const { loadOwnRequests } = await import("../chat/membership-retry");
    return [
      ...loadOwnRequests(b64(ACCOUNT_ID), b64(f.group.groupId)).values(),
    ];
  }

  it("a removal of another person records `remove` keyed on their ACCOUNT key, at the current epoch", async () => {
    const f = rosterGroup();
    await ownClient(f.order).removeMemberFromGroup(SECRETS, f.group, 4);
    expect(await intents(f)).toEqual([
      { request: { kind: "remove", subjectKeyB64: b64(BOB_ACCOUNT) }, epoch: "6" },
    ]);
  });

  it("naming our own leaf records `leave`", async () => {
    const f = rosterGroup();
    await ownClient(f.order).removeMemberFromGroup(SECRETS, f.group, 0);
    expect(await intents(f)).toEqual([
      { request: { kind: "leave", subjectKeyB64: b64(OWN_ACCOUNT) }, epoch: "6" },
    ]);
  });

  it("an approve records `approve-join` keyed on the joiner's key", async () => {
    const f = rosterGroup();
    const ref = new Uint8Array(32).fill(0xd2);
    await ownClient(f.order).approveExternalJoin(SECRETS, f.group, ref);
    expect(await intents(f)).toEqual([
      { request: { kind: "approve-join", subjectKeyB64: b64(ref) }, epoch: "6" },
    ]);
  });

  /**
   * A role-change re-request does not revert a change that landed meanwhile:
   * the intent is the DELTA (promote Bob), re-issued through the real
   * `setMemberAdmin`, which recomputes the table from the CURRENT one — here
   * already holding Carol, promoted by someone else after our first request.
   */
  it("a role-change re-request keeps a change that landed meanwhile, and records the delta", async () => {
    const { reissueOwnRequest } = await import("../chat/rerequest-membership");
    const CAROL_ACCOUNT = new Uint8Array(32).fill(0x31);
    const f = rosterGroup();
    const proposeGroupRoles = vi.fn(
      (_id: unknown, _owner: Uint8Array, _admins: Uint8Array) => new Uint8Array([0xad]),
    );
    Object.assign(f.group as unknown as Record<string, unknown>, {
      roleTableAdmins: () => CAROL_ACCOUNT,
      proposeGroupRoles,
    });
    const c = ownClient(f.order);
    await reissueOwnRequest(
      { contacts: c, identity: SECRETS as never, group: f.group, markOwnPublished: () => {} },
      { kind: "set-admin", subjectKeyB64: b64(BOB_ACCOUNT), makeAdmin: true },
    );
    expect(proposeGroupRoles).toHaveBeenCalledTimes(1);
    const admins = proposeGroupRoles.mock.calls[0]?.[2] as Uint8Array;
    expect([...admins]).toEqual([...CAROL_ACCOUNT, ...BOB_ACCOUNT]);
    expect(await intents(f)).toEqual([
      {
        request: { kind: "set-admin", subjectKeyB64: b64(BOB_ACCOUNT), makeAdmin: true },
        epoch: "6",
      },
    ]);
  });

  it("a direct commit (no role table) records nothing — it landed or threw", async () => {
    const f = fakeGroup({ gid: freshGid(), roleTable: false });
    Object.assign(f.group as unknown as Record<string, unknown>, {
      members: () => [{ leafIndex: 4, signaturePublicKey: BOB_LEAF, accountKey: BOB_ACCOUNT }],
    });
    await ownClient(f.order).removeMemberFromGroup(SECRETS, f.group, 4);
    expect(await intents(f)).toEqual([]);
  });
});
