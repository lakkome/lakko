// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0051 §4 — the reader-side rules for a channel metadata record: how a
 * decrypted plaintext is classified out of the post list, which record of a
 * page is the newest, and when it replaces what the device holds.
 *
 * The classifier is a stand-in shaped like the core's
 * `classifyChannelPostPlaintext`; the order is the same strict rule the
 * core's `channelMetaIsNewer` applies, spelled out inline.
 */
import { describe, expect, it } from "vitest";

import {
  channelMetaErrorText,
  channelMetaSupersedes,
  CHANNEL_NEEDS_NAME,
  newestChannelMeta,
} from "./channel-meta";
import { isHiddenPostDecode, toPostDecode, type PostDecode, type PostPlaintextClassifier } from "./post-decode";

const isNewer = (a: bigint, b: bigint): boolean => a > b;

const meta = (name: string, updatedAtMs: bigint, description = ""): PostDecode => ({
  kind: "meta",
  meta: { name, description, updatedAtMs },
});

/** A classifier that reads a tiny tagged plaintext: `M:<stamp>:<name>` is a
 *  record, `X` is malformed, anything else is text. */
const classify: PostPlaintextClassifier = (plaintext) => {
  const s = new TextDecoder().decode(plaintext);
  if (s === "X") {
    return { kind: "malformed", text: undefined, name: undefined, description: undefined, updatedAtMs: undefined };
  }
  const m = /^M:(\d+):(.*)$/.exec(s);
  if (m !== null) {
    return { kind: "meta", text: undefined, name: m[2], description: "about", updatedAtMs: BigInt(m[1] ?? "0") };
  }
  return { kind: "text", text: plaintext, name: undefined, description: undefined, updatedAtMs: undefined };
};

const enc = (s: string): Uint8Array => new TextEncoder().encode(s);

describe("toPostDecode — every plaintext is classified before the text path", () => {
  it("a record is `meta`, never text", () => {
    const d = toPostDecode({ epochId: 1, plaintext: enc("M:7:Lakko News"), hadSeed: true }, classify);
    expect(d).toEqual({ kind: "meta", meta: { name: "Lakko News", description: "about", updatedAtMs: 7n } });
    expect(isHiddenPostDecode(d)).toBe(true);
  });

  it("a malformed record is dropped, never text", () => {
    const d = toPostDecode({ epochId: 1, plaintext: enc("X"), hadSeed: true }, classify);
    expect(d).toEqual({ kind: "dropped" });
    expect(isHiddenPostDecode(d)).toBe(true);
  });

  it("ordinary text renders as today", () => {
    const d = toPostDecode({ epochId: 1, plaintext: enc("hello"), hadSeed: true }, classify);
    expect(d).toEqual({ kind: "text", text: "hello" });
    expect(isHiddenPostDecode(d)).toBe(false);
  });

  it("an unopenable post never reaches the classifier", () => {
    expect(toPostDecode({ epochId: 1, plaintext: null, hadSeed: true }, classify)).toEqual({ kind: "unreadable" });
    expect(toPostDecode({ epochId: 1, plaintext: null, hadSeed: false }, classify)).toEqual({ kind: "older-key" });
    expect(isHiddenPostDecode(undefined)).toBe(false);
  });
});

describe("newestChannelMeta / channelMetaSupersedes", () => {
  it("picks the newest record of a page and ignores everything else", () => {
    const page: PostDecode[] = [
      { kind: "text", text: "a" },
      meta("Old", 5n),
      { kind: "older-key" },
      meta("New", 9n),
      meta("Middle", 7n),
      { kind: "dropped" },
    ];
    expect(newestChannelMeta(page, isNewer)?.name).toBe("New");
  });

  it("a page with no record yields null", () => {
    expect(newestChannelMeta([{ kind: "text", text: "a" }], isNewer)).toBeNull();
  });

  it("equal stamps keep the first seen (a duplicate is the same record)", () => {
    expect(newestChannelMeta([meta("First", 5n), meta("Second", 5n)], isNewer)?.name).toBe("First");
  });

  it("anything beats nothing; otherwise strictly newer wins and equal is not newer", () => {
    const held = { name: "Held", description: "", updatedAtMs: 5n };
    expect(channelMetaSupersedes({ ...held, name: "X" }, undefined, isNewer)).toBe(true);
    expect(channelMetaSupersedes({ ...held, name: "X", updatedAtMs: 6n }, held, isNewer)).toBe(true);
    expect(channelMetaSupersedes({ ...held, name: "X", updatedAtMs: 5n }, held, isNewer)).toBe(false);
    expect(channelMetaSupersedes({ ...held, name: "X", updatedAtMs: 4n }, held, isNewer)).toBe(false);
  });
});

describe("channelMetaErrorText", () => {
  it("names the empty-name refusal with the spec's copy", () => {
    expect(channelMetaErrorText("name.empty")).toBe(CHANNEL_NEEDS_NAME());
    expect(channelMetaErrorText("description.tooLong")).toBe("The description is too long.");
    expect(channelMetaErrorText("something.else")).toBe("That name or description cannot be used.");
  });
});
