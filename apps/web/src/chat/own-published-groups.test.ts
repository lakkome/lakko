// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The own-echo registry is keyed by GROUP, and one `ChatClient` serves them all.
 *
 * ## The defect these pin
 *
 * A DS message id is per-group monotonic: it names a row only together with the
 * journal it came from. `ChatClient` nevertheless kept a second, group-BLIND
 * `Set<number>` of the ids this device had published, and the poll checked it
 * FIRST — `ownPublishedIds.has(id) || isOwnPublished(group, id)` — so the
 * correct registry could never rescue the incorrect one. `markOwnPublishedCommit`
 * wrote only the blind set.
 *
 * That is reachable on ONE client, with no second device and no adversary. The
 * background receive loop (`chat/BackgroundReceiveContext.tsx`) polls every chat
 * whose view is not open through a SINGLE `ChatClient`, and armed the mark
 * group-blind from two places: the ADR-0045 sweep commit and the ADR-0043
 * push-routing rotation. So a sweep commit landing at row N in chat A made chat
 * B's row N an "echo": dropped inside the batch, AFTER `cursor = message.id` had
 * already moved to it, and therefore never fetched again. A real message, gone,
 * permanently, silently.
 *
 * The second harm runs the other way. B's poll CONSUMED the shared entry
 * (`ownPublishedIds.delete(id)`), disarming A's genuine echo suppression — so
 * A's own commit later reached OpenMLS, was refused as `CannotDecryptOwnMessage`,
 * and rendered a phantom system row.
 *
 * ## Measured discrimination — run, and quoted verbatim
 *
 * Re-introducing the blind set (the field, the `||` in the apply, the blind
 * write in `markOwnPublishedCommit`, and the blind `delete`) turns exactly the
 * two tests that name those two harms red, and nothing else in the 1820-test
 * suite:
 *
 *     FAIL … > a commit marked in one chat does not suppress another chat's row
 *     of the same id
 *     AssertionError: B's row 10 is a real message, not A's echo: expected []
 *     to deeply equal [ 'system' ]
 *
 *     FAIL … > polling another chat does not consume this chat's mark
 *     AssertionError: expected [ 'system' ] to deeply equal []
 *
 * The other two stay green in BOTH worlds, deliberately: the blind set really
 * did suppress a device's own echo (that was never the defect) and really did
 * hold it across a held cursor. They are here to say the fix did not buy the
 * first two by breaking those.
 */
import { describe, expect, it, vi } from "vitest";

// No IndexedDB in the node test env; the outbox round-trip is covered by
// `commit-outbox.test.ts`.
vi.mock("./commit-outbox", () => ({
  markAcked: async () => {},
  loadUnacked: async () => [],
}));

import { base64Encode } from "../auth/account-id";
import { ChatClient } from "./client";
import type { DsClient } from "../ds/client";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

/** Ids start at the same number in EVERY journal, because they really do: a DS
 *  id is per-group monotonic. `A:10` and `B:10` are two unrelated rows, and a
 *  group-blind registry cannot tell them apart. */
const FIRST_ID = 10;

interface JournalRow {
  readonly id: number;
  readonly payload: Uint8Array;
  readonly createdAt: string;
}

/** One chat: its own MLS group double, its own DS journal, its own cursor. */
interface Chat {
  readonly group: WasmGroup;
  readonly groupIdB64: string;
  /** Append a row; returns its DS id. */
  readonly push: () => number;
  readonly cursorRef: { read(): number; advance(to: number): void };
  readonly cursor: () => number;
}

interface World {
  readonly a: Chat;
  readonly b: Chat;
  /** THE shape that makes the defect reachable: one client over both chats,
   *  exactly as the background receive loop runs. */
  readonly chat: ChatClient;
  readonly failPersist: (fail: boolean) => void;
  readonly persistCalls: () => number;
}

/**
 * Two chats behind one `ChatClient`.
 *
 * Rows are opaque markers and the group double reports every one of them as a
 * Commit, because nothing here turns on what a row MEANS — what is under test
 * is which journal a suppression decision is scoped to. A Commit surfaces as a
 * `system` event, which is all the assertion needs and costs no wasm.
 */
function world(): World {
  const journals = new Map<string, JournalRow[]>();
  let persistFails = false;
  let persistCalls = 0;

  function makeChat(idByte: number): Chat {
    const groupId = new Uint8Array([idByte, 0x00]);
    const groupIdB64 = base64Encode(groupId);
    const journal: JournalRow[] = [];
    journals.set(groupIdB64, journal);
    let nextId = FIRST_ID - 1;
    let cursor = 0;

    const group = {
      get groupId(): Uint8Array {
        return groupId;
      },
      get epoch(): bigint {
        return 0n;
      },
      members: () => [],
      processIncomingVerifyingRoles: (): unknown => ({ kind: "Commit" }),
    } as unknown as WasmGroup;

    return {
      group,
      groupIdB64,
      push: () => {
        nextId += 1;
        journal.push({
          id: nextId,
          payload: new Uint8Array([0x01]),
          createdAt: `t-${nextId}`,
        });
        return nextId;
      },
      cursorRef: {
        read: () => cursor,
        advance: (to) => {
          cursor = to;
        },
      },
      cursor: () => cursor,
    };
  }

  const a = makeChat(0xa1);
  const b = makeChat(0xb1);

  const ds = {
    fetchGroup: async (gid: Uint8Array, opts: { after: number }) => ({
      messages: (journals.get(base64Encode(gid)) ?? []).filter(
        (r) => r.id > opts.after,
      ),
      hasMore: false,
    }),
  } as unknown as DsClient;

  const chat = new ChatClient(
    ds,
    {
      identityProvider: () => IDENTITY_TOKEN,
      onPersist: async () => {
        persistCalls += 1;
        if (persistFails) throw new Error("IndexedDB quota exceeded");
      },
    },
  );

  return {
    a,
    b,
    chat,
    failPersist: (fail) => {
      persistFails = fail;
    },
    persistCalls: () => persistCalls,
  };
}

/** Poll one chat to exhaustion of the batch in hand. */
async function poll(w: World, c: Chat): Promise<string[]> {
  const r = await w.chat.pollMessages(null, c.group, c.cursor(), 0, c.cursorRef);
  return r.events.map((e) => e.kind);
}

describe("own-published echo suppression is per group", () => {
  it("a commit marked in one chat does not suppress another chat's row of the same id", async () => {
    // THE headline. Nothing below is a different defect; they are its two other
    // faces and the guard around the fix.
    const w = world();

    // A sweep (or a rotation broadcast) lands at row 10 of chat A, published by
    // this device, so A's own poll must drop it.
    const ownId = w.a.push();
    w.chat.markOwnPublishedCommit(w.a.groupIdB64, ownId);

    // Chat B's journal, independently, is also at row 10 — someone ELSE's
    // message, which this device has never published anything into B at all.
    const strangersId = w.b.push();
    expect(strangersId).toBe(ownId);

    expect(
      await poll(w, w.b),
      "B's row 10 is a real message, not A's echo",
    ).toEqual(["system"]);
    // And it is genuinely delivered, not merely counted: the cursor moved past
    // it, so a re-poll can never bring it back. Suppressing it would have lost
    // it forever, which is why the assertion above is the one that matters.
    expect(w.b.cursor()).toBe(strangersId);
    expect(await poll(w, w.b)).toEqual([]);
  });

  it("the device's own echo is still suppressed, in its own chat", async () => {
    // The fix must not become "suppress nothing".
    const w = world();

    const ownId = w.a.push();
    w.chat.markOwnPublishedCommit(w.a.groupIdB64, ownId);

    expect(await poll(w, w.a)).toEqual([]);
    // An echo row never reaches OpenMLS, so nothing durable changed and the
    // batch costs no persist — the cursor still advances past it.
    expect(w.a.cursor()).toBe(ownId);
    expect(w.persistCalls()).toBe(0);

    // A later row in the same chat, which this device did NOT publish, is
    // delivered normally.
    const theirs = w.a.push();
    expect(await poll(w, w.a)).toEqual(["system"]);
    expect(w.a.cursor()).toBe(theirs);
  });

  it("polling another chat does not consume this chat's mark", async () => {
    // The second harm, and the one that is easy to miss: the blind registry was
    // shared, so B's poll DELETED the entry armed for A. A's own commit then
    // reached OpenMLS, was refused as `CannotDecryptOwnMessage`, and rendered a
    // phantom system row.
    const w = world();

    const ownId = w.a.push();
    w.chat.markOwnPublishedCommit(w.a.groupIdB64, ownId);
    w.b.push();

    // B first — under the blind registry this is what disarmed A. Not asserted
    // on: that B's row survives is the previous test's subject, and asserting
    // it again here would abort this one before it reaches its own.
    await poll(w, w.b);
    // A second: the mark is still armed, so its own row is still dropped
    // instead of reaching OpenMLS and coming back as a phantom system row.
    expect(await poll(w, w.a)).toEqual([]);
    expect(w.a.cursor()).toBe(ownId);
  });

  it("a held cursor still re-applies correctly, per chat", async () => {
    // The consume-on-commit rule the suppression shares a code path with: an
    // echo matched in a batch whose persist FAILED must stay armed, or the
    // re-fetch feeds our own row to OpenMLS on every poll, forever. Asserted
    // per chat, because the registry is now per chat.
    const w = world();

    const ownA = w.a.push();
    w.chat.markOwnPublishedCommit(w.a.groupIdB64, ownA);
    // A second row in A that this device did NOT publish, so the batch has
    // something to persist for and the cursor genuinely hangs on the write.
    w.a.push();
    w.b.push();

    w.failPersist(true);
    expect(await poll(w, w.a)).toEqual(["system"]);
    expect(w.a.cursor(), "the cursor must not pass a batch that did not land").toBe(0);

    w.failPersist(false);
    // The re-fetched batch carries our own row again — still suppressed.
    expect(await poll(w, w.a)).toEqual(["system"]);
    expect(w.a.cursor()).toBe(ownA + 1);

    // B was never in scope of any of that and behaves as it always would.
    expect(await poll(w, w.b)).toEqual(["system"]);
    expect(w.b.cursor()).toBe(FIRST_ID);
  });
});
