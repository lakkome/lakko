// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Call signaling goes out through the SAME seam every other application
 * message does.
 *
 * The publisher used to be a closure inside `Chat.tsx` that called
 * `group.createApplicationMessage` and `ds.publishGroup` directly — the only
 * such call site in `apps/web/src` outside `chat/client.ts`. It therefore held
 * no per-group MLS lock and ran NO durable persist, not even best-effort. This
 * file pins both, plus the own-echo registration the raw path also skipped.
 *
 * ## The hazards, and which case covers which
 *
 *   - **the straddle.** An application message sealed at one epoch and
 *     published across a commit that changes it is not late but permanently
 *     undecryptable: `lakko-mls` never sets `max_past_epochs`, so it is
 *     openmls's default of 0 and a peer keeps no past-epoch secrets. For
 *     signaling that means a silently dropped SDP answer or ICE candidate and a
 *     call that never connects. Covered by the staged-sweep case, whose oracle
 *     is {@link replayAsPeer} — a replay of the recorded journal in DS id
 *     order, the way a recipient replays it — not an assertion about call
 *     order.
 *   - **nonce reuse on crash.** `createApplicationMessage` consumes generation
 *     N and leaves N+1 only in the in-memory provider, so publishing the gen-N
 *     ciphertext without durably landing N+1 lets a reload re-consume N — an
 *     identical (key, nonce) on a different plaintext, i.e. `C1 XOR C2 =
 *     P1 XOR P2` to the zero-trust DS plus Poly1305 forgery. A 1×1 call emits
 *     one signaling message per offer / answer / ICE candidate and closing the
 *     tab afterwards is the ordinary user action. Covered by the two persist
 *     cases: it must run, and a failure must ABORT the publish.
 *
 * ## Which of these discriminate (measured by RUNNING the break, quoted
 * verbatim — not predicted)
 *
 * Reverting {@link makeCallSignalingPublisher} to the pre-fix body (a bare
 * `group.createApplicationMessage` + `ds.publishGroup`, as it stood in
 * `Chat.tsx`):
 *   - "does not seal behind a staged sweep's published commit" FAILS with
 *     `expected [ 'app@5' ] to deeply equal []`;
 *   - "persists the advanced ratchet durably before publishing" FAILS with
 *     `expected [ 'publish' ] to deeply equal [ 'persist', 'publish' ]`;
 *   - "does not publish when the durable persist fails" FAILS with
 *     `expected 1 to be +0`;
 *   - "registers the DS id so the sender's own echo is dropped" FAILS with
 *     `expected "vi.fn()" to not be called at all, but actually been called 1
 *     times`.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChatClient } from "./client";
import { makeCallSignalingPublisher } from "./call-signaling-publish";
import { withGroupMlsLock } from "../mls/group-lock";
import { base64Encode } from "../auth/account-id";
import type { DsClient } from "../ds/client";
import type { PushClient } from "../push/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

/** Wire tags for the group double's two artifact kinds. Byte 1 carries the
 *  epoch the artifact was built on, which is the whole point of the oracle. */
const TAG_APP = 0xa0;
const TAG_COMMIT = 0xc0;

interface Deferred<T> {
  readonly promise: Promise<T>;
  resolve: (v: T) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

/** Let every pending microtask (and one macrotask turn) drain. */
function settle(): Promise<void> {
  return new Promise((r) => setTimeout(r, 0));
}

/** Fail loudly instead of hanging the suite when a lock deadlocks. */
async function within<T>(ms: number, p: Promise<T>, what: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  const bomb = new Promise<never>((_, rej) => {
    timer = setTimeout(() => rej(new Error(`DEADLOCK: ${what}`)), ms);
  });
  try {
    return await Promise.race([p, bomb]);
  } finally {
    clearTimeout(timer!);
  }
}

/** A stand-in for an MLS group that STAMPS every artifact with the epoch it was
 *  built on, so the journal can be replayed the way a recipient replays it.
 *  Mirrors `send-epoch-straddle.test.ts`'s double. */
function stampingGroup(
  groupId: Uint8Array,
  startEpoch: number,
): {
  group: WasmGroup;
  processIncomingVerifyingRoles: ReturnType<typeof vi.fn>;
  stage: () => Uint8Array;
  merge: () => void;
} {
  let epoch = startEpoch;
  const processIncomingVerifyingRoles = vi.fn(() => ({}));
  const group = {
    get groupId(): Uint8Array {
      return groupId;
    },
    get epoch(): bigint {
      return BigInt(epoch);
    },
    createApplicationMessage: vi.fn((_id: unknown, _plaintext: Uint8Array) => {
      // Seals at the live epoch and leaves it where it is — the real
      // `create_message` advances the sender ratchet, never the epoch.
      return new Uint8Array([TAG_APP, epoch]);
    }),
    processIncomingVerifyingRoles,
  } as unknown as WasmGroup;
  return {
    group,
    processIncomingVerifyingRoles,
    // The ADR-0045 staged pair: the commit exists and is publishable while the
    // group deliberately stays at N until the journal verdict is read.
    stage: () => new Uint8Array([TAG_COMMIT, epoch]),
    merge: () => {
      epoch += 1;
    },
  };
}

/** A DS double that keeps the journal in ARRIVAL order, which is the order the
 *  real DS assigns ids in and therefore the order every peer replays. */
function journalDs(): { ds: DsClient; journal: Uint8Array[] } {
  const journal: Uint8Array[] = [];
  const ds = {
    publishGroup: vi.fn(async (_gid: Uint8Array, bytes: Uint8Array) => {
      journal.push(bytes);
      return { id: journal.length, createdAt: "t" };
    }),
  } as unknown as DsClient;
  return { ds, journal };
}

/**
 * Replay `journal` in DS id order the way a recipient does, at
 * `max_past_epochs = 0`, and return the labels of the rows that recipient never
 * sees. A commit applies only when it was built on the epoch the peer is
 * currently at; an application message decrypts only at exactly that epoch.
 */
function replayAsPeer(
  journal: readonly Uint8Array[],
  startEpoch: number,
): string[] {
  let peerEpoch = startEpoch;
  const dropped: string[] = [];
  for (const row of journal) {
    const [tag, builtOn] = [row[0], row[1]];
    if (tag === TAG_COMMIT) {
      if (builtOn === peerEpoch) peerEpoch += 1;
      else dropped.push(`commit@${builtOn}`);
    } else {
      if (builtOn !== peerEpoch) dropped.push(`app@${builtOn}`);
    }
  }
  return dropped;
}

function mockPush(): PushClient {
  return { notifyGroup: vi.fn(async () => {}) } as unknown as PushClient;
}

/** A group id no other case reuses — `withGroupMlsLock`'s queue is MODULE-level
 *  (deliberately), so a shared id would let a case that fails while holding the
 *  lock wedge every later one. */
let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xc5, nextGid]);
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("call signaling publishes through the locked, persisting seam", () => {
  it("does not seal behind a staged sweep's published commit", async () => {
    // The ADR-0045 sweep shape: stage (epoch stays N), publish the commit, then
    // spend a round-trip on the journal verdict before merging. A signaling
    // message sealed in that window goes out at N with the commit already on
    // the wire, so the loss is a certainty rather than a race — and a lost SDP
    // answer or ICE candidate silently kills the call.
    const g = stampingGroup(freshGid(), 5);
    const { ds, journal } = journalDs();
    const verdictGate = deferred<void>();
    const chat = new ChatClient(
      ds,
      { identityProvider: () => IDENTITY_TOKEN },
      mockPush(),
    );
    const publish = makeCallSignalingPublisher({
      chat,
      identity: () => null,
      group: () => g.group,
      onError: () => {},
    });

    const sweep = withGroupMlsLock(base64Encode(g.group.groupId), async () => {
      journal.push(g.stage());
      await verdictGate.promise; // the verdict read
      g.merge();
    });

    await settle();
    const signaling = publish(new Uint8Array([1, 2, 3]));
    await settle();

    verdictGate.resolve();
    await sweep;
    await within(1000, signaling, "signaling queued behind a staged sweep");

    expect(replayAsPeer(journal, 5)).toEqual([]);
  });

  it("persists the advanced ratchet durably before publishing", async () => {
    // `createApplicationMessage` consumes generation N and leaves N+1 only in
    // the in-memory provider, so the order here is the whole nonce-reuse
    // defence, not bookkeeping.
    const g = stampingGroup(freshGid(), 5);
    const order: string[] = [];
    const ds = {
      publishGroup: vi.fn(async () => {
        order.push("publish");
        return { id: 1, createdAt: "t" };
      }),
    } as unknown as DsClient;
    const chat = new ChatClient(
      ds,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersist: async () => {
          order.push("persist");
        },
      },
      mockPush(),
    );
    const publish = makeCallSignalingPublisher({
      chat,
      identity: () => null,
      group: () => g.group,
      onError: () => {},
    });

    await within(1000, publish(new Uint8Array([1])), "signaling publish");

    expect(order).toEqual(["persist", "publish"]);
  });

  it("does not publish when the durable persist fails", async () => {
    // FAIL-CLOSED: a gen-N ciphertext must never reach the DS while N+1 is only
    // in memory. Only one gen-N ciphertext can ever exist on the wire, so
    // aborting keeps N safe to re-consume after a reload.
    const g = stampingGroup(freshGid(), 5);
    const { ds, journal } = journalDs();
    const errors: unknown[] = [];
    const chat = new ChatClient(
      ds,
      {
        identityProvider: () => IDENTITY_TOKEN,
        onPersist: async () => {
          throw new Error("IDB is gone");
        },
      },
      mockPush(),
    );
    const publish = makeCallSignalingPublisher({
      chat,
      identity: () => null,
      group: () => g.group,
      onError: (err) => errors.push(err),
    });

    await within(1000, publish(new Uint8Array([1])), "signaling publish");

    expect(journal.length).toBe(0);
    expect(errors.length).toBe(1);
  });

  it("registers the DS id so the sender's own echo is dropped", async () => {
    // A signaling row the sender published comes straight back on its own next
    // poll. Unregistered, OpenMLS refuses it (`CannotDecryptOwnMessage`) and
    // every row logs a warning. The raw path never registered anything.
    const g = stampingGroup(freshGid(), 5);
    const { ds, journal } = journalDs();
    const fetched: { id: number; payload: Uint8Array; createdAt: string }[] = [];
    (ds as unknown as { fetchGroup: unknown }).fetchGroup = vi.fn(async () => ({
      messages: fetched,
      hasMore: false,
    }));
    const chat = new ChatClient(
      ds,
      { identityProvider: () => IDENTITY_TOKEN },
      mockPush(),
    );
    const publish = makeCallSignalingPublisher({
      chat,
      identity: () => null,
      group: () => g.group,
      onError: () => {},
    });

    await within(1000, publish(new Uint8Array([1])), "signaling publish");
    expect(journal.length).toBe(1);

    // The DS hands the sender its own row back.
    fetched.push({ id: 1, payload: journal[0]!, createdAt: "t" });
    await within(1000, chat.pollMessages(null, g.group, 0), "poll");

    expect(g.processIncomingVerifyingRoles).not.toHaveBeenCalled();
  });
});

/**
 * The push wake, which is what makes a call ring a closed or backgrounded tab
 * at all (`docs/ux/calls.md` §2.1). `broadcastControl` wakes nobody by
 * default; this publisher opts in per frame.
 *
 * The frames here are raw leading tag bytes rather than composed ones on
 * purpose: `calls/call-signaling-wake.test.ts` already pins the tag table
 * against frames the REAL core composed, so repeating that here would test the
 * same thing twice and pay a wasm init for it. What is pinned here is the
 * wiring — that the decision reaches `notifyGroup`, with the right `kind`, off
 * the critical path, and that its failure cannot reach the caller.
 */
describe("a call offer and a call end wake the group; nothing else does", () => {
  /** Leading `CallSignalingPayload.body` oneof tags. */
  const OFFER = new Uint8Array([0x52, 0x00]);
  const ANSWER = new Uint8Array([0x5a, 0x00]);
  const CANDIDATE = new Uint8Array([0x62, 0x00]);
  const END = new Uint8Array([0x6a, 0x00]);
  const GROUP_START = new Uint8Array([0x72, 0x00]);

  async function publishAndDrain(
    frame: Uint8Array,
    push: PushClient,
  ): Promise<void> {
    const g = stampingGroup(freshGid(), 5);
    const { ds } = journalDs();
    const chat = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN }, push);
    const publish = makeCallSignalingPublisher({
      chat,
      identity: () => null,
      group: () => g.group,
      onError: () => {},
    });
    await within(1000, publish(frame), "signaling publish");
    // The wake is deliberately unawaited, so give it a turn to land.
    await settle();
  }

  it("wakes for an offer, at call priority", async () => {
    const push = mockPush();
    await publishAndDrain(OFFER, push);
    expect(push.notifyGroup).toHaveBeenCalledTimes(1);
    // (groupPseudonym, envelopePointer, kind)
    expect(vi.mocked(push.notifyGroup).mock.calls[0]?.[2]).toBe("call");
  });

  it("wakes for an end, so a cancelled call stops ringing a closed tab", async () => {
    const push = mockPush();
    await publishAndDrain(END, push);
    expect(push.notifyGroup).toHaveBeenCalledTimes(1);
    expect(vi.mocked(push.notifyGroup).mock.calls[0]?.[2]).toBe("call");
  });

  it("stays silent for answer, candidate and group control", async () => {
    for (const frame of [ANSWER, CANDIDATE, GROUP_START]) {
      const push = mockPush();
      await publishAndDrain(frame, push);
      expect(push.notifyGroup).not.toHaveBeenCalled();
    }
  });

  it("does not wait on the wake — the DS publish returns first", async () => {
    // A ring must not sit behind a proof of work. The wake is dispatched
    // without an await, so a wake that never settles still lets the publish
    // resolve; if it were awaited this case would hang and `within` would
    // report a deadlock rather than a failure.
    const push = {
      notifyGroup: vi.fn(() => new Promise<{ devicesWoken: number }>(() => {})),
    } as unknown as PushClient;
    await publishAndDrain(OFFER, push);
    expect(push.notifyGroup).toHaveBeenCalledTimes(1);
  });

  it("a failed wake never fails the call", async () => {
    // The offer is already on the DS by the time the wake runs, so a push-gw
    // outage costs the ring's promptness and nothing else: the callee still
    // sees the call on its next foreground poll, exactly as before the wake
    // existed.
    const push = {
      notifyGroup: vi.fn(async () => {
        throw new Error("push-gw is down");
      }),
    } as unknown as PushClient;
    const errors: unknown[] = [];
    const g = stampingGroup(freshGid(), 5);
    const { ds, journal } = journalDs();
    const chat = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN }, push);
    const publish = makeCallSignalingPublisher({
      chat,
      identity: () => null,
      group: () => g.group,
      onError: (err) => errors.push(err),
    });

    await within(1000, publish(OFFER), "signaling publish");
    await settle();

    expect(journal.length).toBe(1);
    expect(errors).toEqual([]);
  });
});
