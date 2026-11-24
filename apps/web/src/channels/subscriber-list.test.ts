// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX 15.1 / 7.3 against the real wasm rule: base walk 2026-11-13, a private
 * channel's owner saw a nameless card as "Member", "1 other." and then both
 * subscribers again as anonymous rows, so it could not tell whom `Remove`
 * took.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { ChannelMemberRow } from "./member-cards";
import { subscriberListView } from "./subscriber-list";
import { ownPublisherKeys, publisherReaderRows, shownSubscriberCount } from "./subscriber-count";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function member(key: string, name: string, pks: string[], named: boolean, username: string | null = null): ChannelMemberRow {
  return { key, name, username, isSelf: false, subscriberPksHex: pks, named };
}

const iphoneKey = "aa".repeat(32);
const pixelKey = "bb".repeat(32);
const listed = [
  { pkHex: iphoneKey, via: { kind: "invitation" as const } },
  { pkHex: pixelKey, via: { kind: "invitation" as const } },
];

describe("a channel's subscriber list", () => {
  it("does not show a nameless card as a named row", () => {
    const view = subscriberListView(wasm, [member("iphone", "Member", [iphoneKey], false)], listed, 2, true);
    expect(view.people).toEqual([]);
    expect(view.others.map((o) => o.pkHex)).toEqual([iphoneKey, pixelKey]);
    expect(view.othersCount).toBe(2);
  });

  it("does not list a named person again among the others, and offers Remove on them", () => {
    const view = subscriberListView(
      wasm,
      [member("pixel", "Pixel Four", [pixelKey], true, "qa_px"), member("iphone", "Member", [iphoneKey], false)],
      listed,
      2,
      true,
    );
    expect(view.people.map((p) => [p.row.name, p.pksHex, p.removable])).toEqual([["Pixel Four", [pixelKey], true]]);
    expect(view.others.map((o) => o.pkHex)).toEqual([iphoneKey]);
    expect(view.othersCount).toBe(1);
  });

  it("removes nobody in a public channel and counts the others", () => {
    const view = subscriberListView(wasm, [member("pixel", "Pixel Four", [pixelKey], true)], [], 3, false);
    expect(view.people[0]?.removable).toBe(false);
    expect(view.others).toEqual([]);
    expect(view.othersCount).toBe(2);
  });

  it("shows this device's own key as You, never as a subscriber to remove", () => {
    const ownKey = "cc".repeat(32);
    const view = subscriberListView(
      wasm,
      [member("pixel", "Pixel Four", [pixelKey], true)],
      [...listed, { pkHex: ownKey.toUpperCase(), via: null }],
      3,
      true,
      ownKey,
    );
    expect(view.people.map((p) => [p.row.isSelf, p.removable])).toEqual([
      [true, false],
      [false, true],
    ]);
    expect(view.others.map((o) => o.pkHex)).toEqual([iphoneKey]);
    expect(view.othersCount).toBe(1);
  });

  // Live walk 2026-11-24: Android and iOS subscribed to the owner's private
  // channel, and the owner read "Subscribers (3)": the two of them, "1
  // other." and "From an invitation" with Remove. The third row was the
  // owner's own account reader key (`account-reader-key.ts`).
  it("leaves the publishing viewer's own reader key out of the list and the count", () => {
    const readerKey = "dd".repeat(32);
    const iphoneNamed = member("iphone", "Eighteen iOS", [iphoneKey], true);
    const pixelNamed = member("pixel", "Nineteen Droid", [pixelKey], true);
    const rows = [...listed, { pkHex: readerKey, via: { kind: "invitation" as const } }];
    const count = shownSubscriberCount(3, 1);
    const view = subscriberListView(
      wasm,
      [iphoneNamed, pixelNamed],
      rows,
      count,
      true,
      readerKey,
      ownPublisherKeys(true, [readerKey.toUpperCase(), null, ""]),
    );
    expect(count).toBe(2);
    expect(view.people.map((p) => p.row.name)).toEqual(["Eighteen iOS", "Nineteen Droid"]);
    expect(view.people.every((p) => !p.row.isSelf)).toBe(true);
    expect(view.others).toEqual([]);
    expect(view.othersCount).toBe(0);
  });

  it("leaves the reader key out even when it has a card of its own", () => {
    const readerKey = "dd".repeat(32);
    const self: ChannelMemberRow = { key: "self", name: "", username: null, isSelf: true, named: true, subscriberPksHex: [readerKey] };
    const view = subscriberListView(
      wasm,
      [self, member("pixel", "Nineteen Droid", [pixelKey], true)],
      [{ pkHex: pixelKey, via: { kind: "invitation" as const } }, { pkHex: readerKey, via: null }],
      1,
      true,
      null,
      ownPublisherKeys(true, [readerKey]),
    );
    expect(view.people.map((p) => p.row.name)).toEqual(["Nineteen Droid"]);
    expect(view.others).toEqual([]);
    expect(view.othersCount).toBe(0);
  });

  // Live walk 2026-11-24, "Night walk": web owner, iOS publisher. Each saw the
  // other's reader key as "From an invitation" with Remove, counted.
  it("leaves another publisher's announced reader key out too", () => {
    const ownReader = "dd".repeat(32);
    const theirReader = "ee".repeat(32);
    const rows = [
      { pkHex: pixelKey, via: { kind: "invitation" as const } },
      { pkHex: ownReader, via: null },
      { pkHex: theirReader.toUpperCase(), via: { kind: "invitation" as const } },
    ];
    const count = shownSubscriberCount(3, publisherReaderRows(ownReader, [theirReader]));
    const view = subscriberListView(
      wasm,
      [member("pixel", "Nineteen Droid", [pixelKey], true)],
      rows,
      count,
      true,
      null,
      ownPublisherKeys(true, [ownReader, theirReader]),
    );
    expect(count).toBe(1);
    expect(view.people.map((p) => p.row.name)).toEqual(["Nineteen Droid"]);
    expect(view.others).toEqual([]);
    expect(view.othersCount).toBe(0);
  });
});
