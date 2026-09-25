// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { createMemo, createRoot, createSignal, mapArray, onCleanup } from "solid-js";
import { beforeEach, describe, expect, it } from "vitest";

import type { CallLogEntry } from "../calls/call-log";
import {
  type ConversationEntry,
  entryCreatedAt,
  mergeConversationEntries,
  type RowTimeOf,
} from "./call-timeline";

/** A row type of the test's own choosing, to pin that the merge never reads
 *  the row itself — only the accessors it was handed. */
interface Row {
  /** What this row IS, across the replacements of the object holding it — the
   *  test's stand-in for `rowIdentity`. */
  readonly id: string;
  readonly text: string;
  readonly at: string | undefined;
}

const timeOf = (row: Row): string | undefined => row.at;
const identityOf = (row: Row): string => row.id;

function msg(text: string, at?: string): Row {
  return { id: text, text, at };
}

/**
 * The conversation's rows, as the accessor the module takes.
 *
 * Fresh per test, and that is deliberate rather than tidiness: the module's
 * bookkeeping is keyed on this function — one per conversation — so a shared one
 * would let a test be handed the wrapper another test made for a row of the same
 * identity.
 */
let listed: readonly Row[] = [];
let rowsOf: () => readonly Row[] = () => listed;

beforeEach(() => {
  listed = [];
  rowsOf = () => listed;
});

/** The merge over a fixed list. The module takes the rows as a FUNCTION because
 *  an entry's `row` is a live read; a test that never changes its list passes a
 *  constant one, and the tests about the live read pass a signal. */
function merge(
  rows: readonly Row[],
  calls: readonly CallLogEntry[] = [],
  time: RowTimeOf<Row> = timeOf,
): ConversationEntry<Row>[] {
  listed = rows;
  return mergeConversationEntries(rowsOf, calls, time, identityOf);
}

function call(id: string, startedAtMs: number): CallLogEntry {
  return {
    id,
    outcome: "outgoing",
    video: false,
    startedAtMs,
    durationMs: 1_000,
  };
}

/** A compact read of the merged shape: `m:<text>` / `c:<id>`, in order. */
function shape(entries: ReturnType<typeof mergeConversationEntries<Row>>): string[] {
  return entries.map((e) => (e.kind === "message" ? `m:${e.row.text}` : `c:${e.call.id}`));
}

const T = (ms: number): string => new Date(ms).toISOString();

describe("mergeConversationEntries", () => {
  it("returns the messages untouched when there are no calls", () => {
    const rows = [msg("a", T(1)), msg("b", T(2))];
    expect(shape(merge(rows, []))).toEqual(["m:a", "m:b"]);
  });

  it("returns the calls alone when there are no messages", () => {
    expect(shape(merge([], [call("x", 5), call("y", 1)]))).toEqual([
      "c:y",
      "c:x",
    ]);
  });

  it("interleaves by time", () => {
    const rows = [msg("a", T(10)), msg("b", T(30))];
    const calls = [call("mid", 20), call("late", 40), call("early", 5)];
    expect(shape(merge(rows, calls))).toEqual([
      "c:early",
      "m:a",
      "c:mid",
      "m:b",
      "c:late",
    ]);
  });

  it("draws a call that ties with a message after it", () => {
    // Arbitrary but fixed: an unstable answer would let a row jump between
    // renders.
    const rows = [msg("a", T(10))];
    expect(shape(merge(rows, [call("c", 10)]))).toEqual(["m:a", "c:c"]);
  });

  it("keeps an unacknowledged outgoing bubble pinned after the row it followed", () => {
    // It has no `createdAt` yet and is the newest thing in the conversation;
    // treating `undefined` as low would float it above every call row.
    const rows = [msg("a", T(10)), msg("sending")];
    expect(shape(merge(rows, [call("older", 5)]))).toEqual([
      "c:older",
      "m:a",
      "m:sending",
    ]);
  });

  it("leaves a leading unstamped row at the top rather than pushing it down", () => {
    const rows = [msg("sending"), msg("a", T(10))];
    expect(shape(merge(rows, [call("c", 5)]))).toEqual([
      "m:sending",
      "c:c",
      "m:a",
    ]);
  });

  it("treats an unparseable stamp as no stamp, the way the layout does", () => {
    // Leading and unparseable: nothing to carry forward, so the row keeps its
    // place rather than being pushed below a call it may well predate. The
    // call then lands before the first row whose stamp it does precede.
    const rows = [msg("a", "not a date"), msg("b", T(20))];
    expect(shape(merge(rows, [call("c", 10)]))).toEqual([
      "m:a",
      "c:c",
      "m:b",
    ]);
  });

  it("preserves message order", () => {
    const rows = [msg("a", T(10)), msg("b", T(20)), msg("c", T(30))];
    const merged = merge(rows, [call("k", 15)]);
    const messages = merged.filter((e) => e.kind === "message");
    expect(messages.map((e) => (e.kind === "message" ? e.row.text : ""))).toEqual(["a", "b", "c"]);
  });

  it("hands back the SAME wrapper for a row it has already seen", () => {
    // Solid's `<For>` reconciles by reference: a fresh wrapper every recompute
    // would tear down and rebuild every bubble in the conversation each time a
    // message arrived, losing playing video, open previews and scroll
    // position.
    const a = msg("a", T(10));
    const b = msg("b", T(30));
    const k = call("k", 20);
    const first = merge([a], [k]);
    const second = merge([a, b], [k]);
    expect(second[0]).toBe(first[0]);
    expect(second[1]).toBe(first[1]);
  });

  it("gives a row that has gone and come back an entry that still matches it", () => {
    const a = msg("a", T(10));
    expect(merge([a], [])[0]?.kind).toBe("message");
    const again = merge([a], [call("k", 99)]);
    expect(again[0]?.kind === "message" && again[0].row).toBe(a);
  });

  it("never drops or duplicates anything", () => {
    const rows = [msg("a", T(10)), msg("b"), msg("c", T(30))];
    const calls = [call("x", 1), call("y", 20), call("z", 99)];
    const merged = merge(rows, calls);
    expect(merged).toHaveLength(rows.length + calls.length);
    expect(new Set(shape(merged)).size).toBe(rows.length + calls.length);
  });
});

describe("entryCreatedAt", () => {
  it("reads a message's own stamp", () => {
    const merged = merge([msg("a", T(10))], []);
    expect(entryCreatedAt(merged[0] as never, timeOf)).toBe(T(10));
  });

  it("gives a call an ISO stamp the day-separator logic can parse", () => {
    const merged = merge([], [call("c", 1_700_000_000_000)]);
    const iso = entryCreatedAt(merged[0] as never, timeOf);
    expect(iso).toBe(new Date(1_700_000_000_000).toISOString());
    expect(Number.isNaN(new Date(iso as string).getTime())).toBe(false);
  });
});

/**
 * The wrapper stability, pinned at the shape the renderer actually produces.
 *
 * The suite above proves two indices survive one append. These prove the
 * property over the receive loop's real update shape -- `setRows([...rows(),
 * ...fresh])` in `routes/Chat.tsx`, repeated on every poll, with call rows
 * arriving on their own schedule -- by counting the objects that are new, not
 * by naming the ones that are old. A regression that re-created some rows and
 * not others would slip past an indexed assertion and is caught here.
 */
describe("wrapper stability across the receive loop's update shape", () => {
  it("makes exactly one new entry per newly arrived row", () => {
    const a = msg("a", T(10));
    const b = msg("b", T(30));
    const c = msg("c", T(50));
    const k = call("k", 20);
    const k2 = call("k2", 40);

    // Poll 1: one message, one call.
    const first = merge([a], [k]);
    expect(first).toHaveLength(2);

    // Poll 2: a message arrived. Everything already drawn must be untouched,
    // and the only object the renderer has not seen is the new bubble.
    const second = merge([a, b], [k]);
    const seenAfterFirst = new Set<unknown>(first);
    expect(second.filter((e) => !seenAfterFirst.has(e))).toHaveLength(1);
    for (const e of first) expect(second).toContain(e);

    // Poll 3: a call ended. Same again -- a call row landing mid-conversation
    // must not rebuild the messages it is being interleaved between.
    const third = merge([a, b], [k, k2]);
    const seenAfterSecond = new Set<unknown>(second);
    expect(third.filter((e) => !seenAfterSecond.has(e))).toHaveLength(1);
    for (const e of second) expect(third).toContain(e);

    // Poll 4: another message, and a re-merge of an unchanged conversation in
    // between -- a recompute that changed nothing must produce nothing new.
    const again = merge([a, b], [k, k2]);
    for (const e of third) expect(again).toContain(e);
    const fourth = merge([a, b, c], [k, k2]);
    const seenAfterThird = new Set<unknown>(third);
    expect(fourth.filter((e) => !seenAfterThird.has(e))).toHaveLength(1);
  });

  it("does not depend on the array or the time reader it was handed", () => {
    // The renderer passes a fresh array on every poll and a module-level
    // `rowCreatedAt`; neither is part of a row's identity, so a merge given a
    // different array and an equivalent reader must still reuse the wrappers.
    const a = msg("a", T(10));
    const first = merge([a], []);
    const second = merge([a], [], (row: Row) => row.at);
    expect(second[0]).toBe(first[0]);
  });

  it("keeps the wrapper when the ROW OBJECT is replaced", () => {
    // The defect, in one assertion. A row object is immutable and replaced on
    // every state change, so keyed on the object every ack flip and every landed
    // upload produced a new wrapper — and `<For>` tore the bubble down. What is
    // stable is the identity.
    const pending: Row = { id: "out:1", text: "sending", at: undefined };
    const sent: Row = { id: "out:1", text: "sent", at: T(10) };
    const first = merge([pending], []);
    const second = merge([sent], []);
    expect(second[0]).toBe(first[0]);
  });

  it("hands out the row as it is NOW, through the entry it made earlier", () => {
    // The other half: a wrapper that outlives a replacement has to answer with
    // the current row, or a bubble that is no longer rebuilt would go on showing
    // the row it was built with — `Not sent` under a message that has been sent.
    const pending: Row = { id: "out:1", text: "sending", at: undefined };
    const sent: Row = { id: "out:1", text: "sent", at: T(10) };
    const entry = merge([pending], [])[0];
    expect(entry?.kind === "message" && entry.row).toBe(pending);
    merge([sent], []);
    expect(entry?.kind === "message" && entry.row).toBe(sent);
  });

  it("gives two rows of different identity their own entries", () => {
    // Keying on something two rows can agree on — the text, a stamp — would
    // hand one bubble the other's wrapper. Identity is `tempId` / `messageId`,
    // which two rows of one conversation never share.
    const one = msg("same", T(10));
    const two: Row = { id: "other", text: "same", at: T(10) };
    const merged = merge([one, two], []);
    expect(merged[0]).not.toBe(merged[1]);
    expect(merged[0]?.kind === "message" && merged[0].row).toBe(one);
    expect(merged[1]?.kind === "message" && merged[1].row).toBe(two);
  });

  it("lets go of a row that has left the conversation", () => {
    // What the `WeakMap` on the row object used to do for free. Without the
    // prune the bookkeeping would hold every row a conversation ever showed.
    const a = msg("a", T(10));
    const first = merge([a], []);
    merge([], []);
    const again = merge([a], []);
    expect(again[0]).not.toBe(first[0]);
  });
});

/**
 * What Solid does with the result, which is the half that actually bites.
 *
 * Stable wrappers only matter because of what the renderer does with them, and
 * `routes/Chat.tsx` is unreachable from a test here -- no JSX transform, no
 * DOM. But `<For>` is a thin wrapper over `mapArray`, and `mapArray` is pure
 * reactivity: it needs neither. So the property the memoization exists for is
 * provable directly -- an entry whose reference did not change keeps its
 * reactive owner, and its `onCleanup` does not run.
 *
 * That `onCleanup` is not hypothetical. `media/MediaBubbles.tsx` registers one
 * per voice bubble that pauses the audio, releases the playback claim and
 * clears the element's `src`; a video note registers one that pauses and
 * releases. Re-creating a row that did not change stops playback in the
 * person's ear.
 *
 * The two tests are a matched pair, and the second is the discriminator: it
 * maps over freshly built wrappers and watches the disposal happen, so the
 * first test cannot pass for a reason other than the one it claims.
 */
describe("Solid's reconciliation over the merged entries", () => {
  /** A label for an entry, to log creations and disposals against. */
  const nameOf = (entry: ConversationEntry<Row>): string =>
    entry.kind === "message" ? `m:${entry.row.text}` : `c:${entry.call.id}`;

  /**
   * Drive `mapArray` over a growing conversation and report what it built and
   * what it tore down. `entriesOf` is the merge under test -- the real one, or
   * a deliberately unmemoized stand-in.
   */
  function reconcile(
    entriesOf: (rows: () => readonly Row[]) => ConversationEntry<Row>[],
    update: (rows: readonly Row[]) => readonly Row[] = (rows) => [...rows, msg("b", T(30))],
    expected: string[] = ["m:a", "c:k", "m:b"],
  ): {
    created: string[];
    disposed: string[];
  } {
    const created: string[] = [];
    const disposed: string[] = [];
    let duringUpdate: string[] = [];
    const a = msg("a", T(10));
    createRoot((dispose) => {
      const [rows, setRows] = createSignal<readonly Row[]>([a]);
      const mapped = mapArray(
        () => entriesOf(rows),
        (entry) => {
          created.push(nameOf(entry));
          onCleanup(() => disposed.push(nameOf(entry)));
          return nameOf(entry);
        },
      );
      expect(mapped()).toEqual(["m:a", "c:k"]);
      setRows(update([a]));
      expect(mapped()).toEqual(expected);
      // Snapshot before tearing the root down: that teardown disposes whatever
      // is still mounted, which says nothing about the update under test.
      duringUpdate = [...disposed];
      dispose();
    });
    return { created, disposed: duringUpdate };
  }

  const k = call("k", 20);

  const merged = (rows: () => readonly Row[]): ConversationEntry<Row>[] =>
    mergeConversationEntries(rows, [k], timeOf, identityOf);

  it("builds only the new row and disposes nothing when a message arrives", () => {
    const { created, disposed } = reconcile(merged);
    expect(created).toEqual(["m:a", "c:k", "m:b"]);
    // The whole point: the bubble that was already there was not torn down, so
    // a voice message playing in it keeps playing.
    expect(disposed).toEqual([]);
  });

  it("disposes nothing when a row OBJECT is replaced", () => {
    // The defect, at the layer where it was felt. `setRows(rows().map(...))` is
    // how every state change reaches the timeline -- the ack flip, a failed
    // send, an upload landing -- and each one hands `<For>` a new object for a
    // row the person is looking at. Keyed on the object it rebuilt the bubble,
    // taking the element that was making sound with it.
    const { created, disposed } = reconcile(
      merged,
      () => [{ id: "a", text: "a", at: T(10) }],
      ["m:a", "c:k"],
    );
    expect(created).toEqual(["m:a", "c:k"]);
    expect(disposed).toEqual([]);
  });

  it("tears down every row when the wrappers are rebuilt -- why the cache exists", () => {
    // The same merge with the memoization removed: correct output, new objects.
    // `<For>` cannot tell this apart from a conversation in which every row was
    // replaced, so it disposes all of them -- pausing the audio, releasing the
    // claim and clearing the `src` of a bubble nobody navigated away from.
    const unmemoized = (rows: () => readonly Row[]): ConversationEntry<Row>[] =>
      merged(rows).map((e) =>
        e.kind === "message" ? { kind: "message", row: e.row } : { kind: "call", call: e.call },
      );
    const { created, disposed } = reconcile(unmemoized);
    expect(disposed).toEqual(["m:a", "c:k"]);
    expect(created).toEqual(["m:a", "c:k", "m:a", "c:k", "m:b"]);
  });

  it("re-runs a computation that read `entry.row` when the row is replaced", () => {
    // What makes the surviving bubble show the NEW row: `entry.row` reads the
    // rows accessor, so a computation that read it depends on the signal. Under
    // a captured field this reads `sending` twice.
    createRoot((dispose) => {
      const [rows, setRows] = createSignal<readonly Row[]>([
        { id: "out:1", text: "sending", at: undefined },
      ]);
      const entries = createMemo(() => mergeConversationEntries(rows, [], timeOf, identityOf));
      const text = createMemo(() => {
        const first = entries()[0];
        return first?.kind === "message" ? first.row.text : "?";
      });
      expect(text()).toBe("sending");
      setRows([{ id: "out:1", text: "sent", at: T(10) }]);
      expect(text()).toBe("sent");
      dispose();
    });
  });
});
