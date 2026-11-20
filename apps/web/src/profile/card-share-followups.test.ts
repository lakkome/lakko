// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 section 7 / UX `attachments.md` section 11a, the follow-ups:
 *
 *   - a switch to or from `Nobody` restamps the OWN profile (receivers keep
 *     a strictly newer one only);
 *   - a held content message (a content forward into a superseded
 *     one-to-one included) keeps its content through the queue, the store
 *     and the release;
 *   - the feed's channel posts keep their files and content, so a forward
 *     from the feed carries them;
 *   - a forwarded content message carries its forward line to the receiver
 *     and through the persisted row (the bubble draws `forward` above the
 *     content, `chat/TimelineBubble.tsx`).
 *
 * Envelopes go through the REAL wasm core.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { MessageContent } from "../../wasm/lakko_ffi_wasm";
import { ownCardFlagFlipped } from "./card-share";
import { loadOwnProfile, restampOwnProfile, saveOwnProfileFields, type OwnProfileStorage } from "./own-profile";
import { composeHeldEnvelope, heldFromPersisted, heldToPersisted } from "../chat/held-send-queue";
import { parseBodyEnvelope, type AttachmentRefBytes } from "../chat/attachments";
import { persistedFromAttachmentRef } from "../chat/persisted-attachment";
import { eventToPersistedMessage } from "../chat/background-log";
import { CHANNEL_POST_LOG_VERSION, decodeChannelPostLog, syncChannelSlot } from "../feed/channel-post-log";
import { buildFeedInput, type FeedSource } from "../feed/feed-model";
import { feedForwardPayload } from "../feed/feed-forward";
import { contentRetryAllowed, pollBodyOf } from "../attach/actions";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const fill = (n: number, v: number): Uint8Array => new Uint8Array(n).fill(v);
const PHOTO: AttachmentRefBytes = {
  blobId: fill(32, 1),
  key: fill(32, 2),
  nonce: fill(12, 3),
  mime: "image/jpeg",
  size: 4096,
  name: "photo.jpg",
  width: 640,
  height: 480,
};
const location: MessageContent = {
  kind: "location",
  location: { latE7: 600000000, lonE7: 250000000, accuracyM: 10, fixAtMs: 1700000000000, name: null },
};
const article: MessageContent = {
  kind: "article",
  article: { title: "T", blocks: [{ kind: "image", attachmentIndex: 0, caption: [] }] },
};
const FORWARD = { author: "Bob", origin: { groupIdB64: "Z3JvdXA=", messageId: 41 } };

function memoryStorage(): OwnProfileStorage {
  const m = new Map<string, string>();
  return {
    getItem: (k) => m.get(k) ?? null,
    setItem: (k, v) => void m.set(k, v),
    removeItem: (k) => void m.delete(k),
  };
}

describe("own profile restamp on a Nobody switch", () => {
  it("only a change between two known settings is a switch", () => {
    expect(ownCardFlagFlipped(true, false)).toBe(true);
    expect(ownCardFlagFlipped(false, true)).toBe(true);
    expect(ownCardFlagFlipped(true, true)).toBe(false);
    expect(ownCardFlagFlipped(null, false)).toBe(false);
    expect(ownCardFlagFlipped(false, null)).toBe(false);
  });

  it("keeps the name and photo under a strictly newer stamp, even with the clock standing still", () => {
    const storage = memoryStorage();
    const saved = saveOwnProfileFields("acct", { displayName: "Ann", avatar: null }, 1000, storage);
    const next = restampOwnProfile("acct", 1000, storage);
    expect(next.displayName).toBe("Ann");
    expect(next.updatedAtMs).toBeGreaterThan(saved.updatedAtMs);
    expect(loadOwnProfile("acct", storage).updatedAtMs).toBe(next.updatedAtMs);
    // A never-saved profile gets a first stamp.
    expect(restampOwnProfile("other", 2000, storage).updatedAtMs).toBe(2000);
  });
});

describe("held content messages", () => {
  it("survive the stored form with their content and forward", () => {
    const stored = heldToPersisted("ab", {
      heldAtMs: 5,
      payload: { text: "", attached: [], reply: undefined, forward: FORWARD, content: location },
    });
    const back = heldFromPersisted(JSON.parse(JSON.stringify(stored)));
    expect(back?.held.payload.content).toEqual(location);
    expect(back?.held.payload.forward).toEqual(FORWARD);
  });

  it("an old row (no content) with no text and no files is still not a message", () => {
    expect(heldFromPersisted({ conversationHex: "ab", heldAtMs: 5, text: "", attachments: [] })).toBeNull();
  });

  it("are released as the same content message with the forward line", async () => {
    const bytes = await composeHeldEnvelope(
      { text: "", attached: [PHOTO], reply: undefined, forward: FORWARD, content: article },
      undefined,
    );
    const body = await parseBodyEnvelope(bytes);
    expect(body.kind).toBe("content");
    if (body.kind !== "content") return;
    expect(body.content).toEqual(article);
    expect(body.attachments).toHaveLength(1);
    expect(body.forward).toEqual(FORWARD);
  });

  it("a plain held message is unchanged", async () => {
    const body = await parseBodyEnvelope(await composeHeldEnvelope({ text: "hi", attached: [], reply: undefined }, undefined));
    expect(body.kind).toBe("chat");
  });
});

describe("the forward line on a content message", () => {
  it("reaches the receiver and survives the persisted row", async () => {
    const bytes = wasm.composeContentEnvelope(location, [], {
      forwardedFrom: FORWARD.author,
      forwardOriginGroupB64: FORWARD.origin.groupIdB64,
      forwardOriginMessageId: FORWARD.origin.messageId,
    });
    const body = await parseBodyEnvelope(bytes);
    expect(body.kind === "content" && body.forward).toEqual(FORWARD);
    const persisted = eventToPersistedMessage({
      kind: "application",
      messageId: 9,
      text: "",
      attachments: [],
      plaintext: "",
      senderLeaf: 1,
      senderKey: "k",
      createdAt: "2026-11-18T00:00:00Z",
      content: location,
      forward: FORWARD,
    } as never);
    expect(persisted.content).toEqual(location);
    expect(persisted.forward).toEqual({ author: "Bob", origin: { groupIdB64: FORWARD.origin.groupIdB64, messageId: "41" } });
  });
});

describe("channel posts in the feed keep their files and content", () => {
  const CHANNEL: FeedSource = { kind: "channel", idHex: "cc".repeat(16), slug: "cc".repeat(16), title: "News", muted: false, audience: 0 };
  const NOW = Date.parse("2026-11-18T12:00:00Z");

  it("from the sync, through the stored log, into the feed row and its forward", async () => {
    const file = persistedFromAttachmentRef(PHOTO);
    const slot = await syncChannelSlot(
      CHANNEL.idHex,
      undefined,
      {
        fetchPage: async () => ({
          posts: [
            { cursor: 1, serverTsMs: NOW - 1000, payloadB64: "a" },
            { cursor: 2, serverTsMs: NOW - 500, payloadB64: "b" },
          ],
          nextAfter: 2,
        }),
        decode: async (_id, payload) =>
          payload === "a"
            ? { kind: "text", text: "photo post", media: [{ mime: file.mime ?? null, kind: file.kind ?? null, ref: file }] }
            : { kind: "text", text: "", media: [{ mime: file.mime ?? null, kind: file.kind ?? null, ref: file }], content: article },
      },
      NOW,
    );
    const log = decodeChannelPostLog(JSON.parse(JSON.stringify({ version: CHANNEL_POST_LOG_VERSION, channels: { [CHANNEL.idHex]: slot } })));
    const stored = log?.channels[CHANNEL.idHex];
    expect(stored?.posts.map((p) => p.media?.length)).toEqual([1, 1]);
    expect(stored?.posts[1]?.content).toEqual(article);

    const input = buildFeedInput({
      sources: [CHANNEL],
      log: {},
      channelPosts: { [CHANNEL.idHex]: stored! },
      ownChannels: new Set(),
      ownUsername: null,
      contactKeysB64: new Set(),
      mentions: () => false,
      nowMs: NOW,
    });
    expect(input.candidates.every((c) => c.hasMedia)).toBe(true);
    const photoRef = input.refs.find((r) => r.message.text === "photo post")!;
    const articleRef = input.refs.find((r) => r.message.content !== undefined)!;
    const photoForward = feedForwardPayload({ kind: "channel", message: photoRef.message, channelTitle: "News" });
    expect(photoForward.attachments).toHaveLength(1);
    expect(photoForward.text).toBe("photo post");
    const articleForward = feedForwardPayload({ kind: "channel", message: articleRef.message, channelTitle: "News" });
    expect(articleForward.content).toEqual(article);
    expect(articleForward.attachments).toHaveLength(1);
  });
});

describe("Retry of a failed content message", () => {
  const A = btoa(String.fromCharCode(...fill(32, 0xa1)));
  const B = btoa(String.fromCharCode(...fill(32, 0xb2)));
  const C = btoa(String.fromCharCode(...fill(32, 0xc3)));
  const draft = { question: "Lunch?", options: ["Yes", "No"], multiple: false, anonymous: true };

  it("an anonymous poll is re-sent only while its roster is the one it was made with", async () => {
    const body = await pollBodyOf(draft, [A, B], 1000);
    const poll: MessageContent = { kind: "poll", poll: { body, authorSig: null } };
    expect(await contentRetryAllowed(poll, [B, A, A])).toBe(true);
    expect(await contentRetryAllowed(poll, [A, B, C])).toBe(false);
    expect(await contentRetryAllowed(poll, [A])).toBe(false);
  });

  it("anything else is re-sent as stored", async () => {
    const body = await pollBodyOf({ ...draft, anonymous: false }, [A, B], 1000);
    expect(await contentRetryAllowed({ kind: "poll", poll: { body, authorSig: null } }, [C])).toBe(true);
    expect(await contentRetryAllowed(location, [C])).toBe(true);
    expect(await contentRetryAllowed(article, [C])).toBe(true);
    expect(await contentRetryAllowed(undefined, [C])).toBe(true);
  });
});
