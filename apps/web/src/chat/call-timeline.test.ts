// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { CallLogEntry } from "../calls/call-log";
import { entryCreatedAt, mergeConversationEntries } from "./call-timeline";

/** A row type of the test's own choosing, to pin that the merge never reads
 *  the row itself — only the accessor it was handed. */
interface Row {
  readonly text: string;
  readonly at: string | undefined;
}

const timeOf = (row: Row): string | undefined => row.at;

function msg(text: string, at?: string): Row {
  return { text, at };
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
    expect(shape(mergeConversationEntries(rows, [], timeOf))).toEqual(["m:a", "m:b"]);
  });

  it("returns the calls alone when there are no messages", () => {
    expect(shape(mergeConversationEntries<Row>([], [call("x", 5), call("y", 1)], timeOf))).toEqual([
      "c:y",
      "c:x",
    ]);
  });

  it("interleaves by time", () => {
    const rows = [msg("a", T(10)), msg("b", T(30))];
    const calls = [call("mid", 20), call("late", 40), call("early", 5)];
    expect(shape(mergeConversationEntries(rows, calls, timeOf))).toEqual([
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
    expect(shape(mergeConversationEntries(rows, [call("c", 10)], timeOf))).toEqual(["m:a", "c:c"]);
  });

  it("keeps an unacknowledged outgoing bubble pinned after the row it followed", () => {
    // It has no `createdAt` yet and is the newest thing in the conversation;
    // treating `undefined` as low would float it above every call row.
    const rows = [msg("a", T(10)), msg("sending")];
    expect(shape(mergeConversationEntries(rows, [call("older", 5)], timeOf))).toEqual([
      "c:older",
      "m:a",
      "m:sending",
    ]);
  });

  it("leaves a leading unstamped row at the top rather than pushing it down", () => {
    const rows = [msg("sending"), msg("a", T(10))];
    expect(shape(mergeConversationEntries(rows, [call("c", 5)], timeOf))).toEqual([
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
    expect(shape(mergeConversationEntries(rows, [call("c", 10)], timeOf))).toEqual([
      "m:a",
      "c:c",
      "m:b",
    ]);
  });

  it("preserves message order", () => {
    const rows = [msg("a", T(10)), msg("b", T(20)), msg("c", T(30))];
    const merged = mergeConversationEntries(rows, [call("k", 15)], timeOf);
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
    const first = mergeConversationEntries([a], [k], timeOf);
    const second = mergeConversationEntries([a, b], [k], timeOf);
    expect(second[0]).toBe(first[0]);
    expect(second[1]).toBe(first[1]);
  });

  it("gives a row that has gone and come back an entry that still matches it", () => {
    const a = msg("a", T(10));
    expect(mergeConversationEntries([a], [], timeOf)[0]?.kind).toBe("message");
    const again = mergeConversationEntries([a], [call("k", 99)], timeOf);
    expect(again[0]?.kind === "message" && again[0].row).toBe(a);
  });

  it("never drops or duplicates anything", () => {
    const rows = [msg("a", T(10)), msg("b"), msg("c", T(30))];
    const calls = [call("x", 1), call("y", 20), call("z", 99)];
    const merged = mergeConversationEntries(rows, calls, timeOf);
    expect(merged).toHaveLength(rows.length + calls.length);
    expect(new Set(shape(merged)).size).toBe(rows.length + calls.length);
  });
});

describe("entryCreatedAt", () => {
  it("reads a message's own stamp", () => {
    const merged = mergeConversationEntries([msg("a", T(10))], [], timeOf);
    expect(entryCreatedAt(merged[0] as never, timeOf)).toBe(T(10));
  });

  it("gives a call an ISO stamp the day-separator logic can parse", () => {
    const merged = mergeConversationEntries<Row>([], [call("c", 1_700_000_000_000)], timeOf);
    const iso = entryCreatedAt(merged[0] as never, timeOf);
    expect(iso).toBe(new Date(1_700_000_000_000).toISOString());
    expect(Number.isNaN(new Date(iso as string).getTime())).toBe(false);
  });
});
