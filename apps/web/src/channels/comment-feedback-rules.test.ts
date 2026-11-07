// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0087 phase B: the pure rules of reactions on channel comments. */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { beforeAll, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  commentCountBatches,
  commentKey,
  decodeWaitingCommentReactions,
  resolveCommentLabelCounts,
} from "./comment-feedback-rules";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

const ID = "0123456789abcdef0123456789abcdef";

describe("comment key", () => {
  it("names channel, post and comment", () => {
    expect(commentKey({ idHex: ID.toUpperCase(), post: 4, comment: 9 })).toBe(`${ID}:4:9`);
  });
});

describe("waiting comment reactions", () => {
  it("keeps well-formed entries once", () => {
    const one = { idHex: ID, post: 1, comment: 2, epoch: 0, emoji: "👍", postAtMs: 1000 };
    expect(
      decodeWaitingCommentReactions([
        one,
        one,
        { ...one, comment: 0 },
        { ...one, epoch: -1 },
        { ...one, emoji: "" },
        { ...one, postAtMs: undefined },
      ]),
    ).toEqual([one]);
    expect(decodeWaitingCommentReactions("x")).toEqual([]);
  });
});

describe("count batches", () => {
  it("asks at most 50 comments a call, each once", () => {
    const cursors = Array.from({ length: 120 }, (_, i) => i + 1);
    const batches = commentCountBatches([...cursors, 3, 0, -1]);
    expect(batches.map((b) => b.length)).toEqual([50, 50, 20]);
    expect(batches.flat()).toEqual(cursors);
  });
});

describe("label resolution against the real core", () => {
  const seed = new Uint8Array(32).fill(7);
  const channelId = new Uint8Array(16).fill(1);

  it("resolves a comment's labels and nothing of another comment or of a post", () => {
    const key = wasm.ChannelCommentReactionKey.derive(seed, channelId, 3);
    const thumbs = hex(key.label(9n, 4n, "👍"));
    const otherComment = hex(key.label(9n, 5n, "❤️"));
    key.free();
    const postKey = wasm.ChannelReactionKey.derive(seed, channelId, 3);
    const postLabel = hex(postKey.label(9n, "😂"));
    postKey.free();
    const labels = new Map([
      [thumbs, 4],
      [otherComment, 1],
      [postLabel, 2],
    ]);
    const out = resolveCommentLabelCounts(wasm, seed, channelId, 3, 9, 4, labels, ["😂", "❤️", "👍"]);
    expect([...out]).toEqual([["👍", 4]]);
  });

  it("gives a comment's reaction a write key and proofs", () => {
    const identity = wasm.Identity.fromSeed(new Uint8Array(64).fill(3));
    try {
      const key = wasm.ChannelCommentReactionKey.derive(seed, channelId, 3);
      const label = key.label(9n, 4n, "👍");
      key.free();
      const a = new wasm.ChannelCommentReactionWriteKey(identity, channelId, 9n, 4n, label);
      const b = new wasm.ChannelCommentReactionWriteKey(identity, channelId, 9n, 5n, label);
      expect(a.publicKey.length).toBe(32);
      expect(hex(a.publicKey)).not.toBe(hex(b.publicKey));
      expect(a.signAdd().length).toBeGreaterThan(0);
      expect(a.signDelete()).not.toBe(a.signAdd());
      a.free();
      b.free();
    } finally {
      identity.free();
    }
  });
});
