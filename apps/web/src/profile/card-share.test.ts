// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 section 7 / UX `attachments.md` sections 10.1 and 11a / UX
 * `who-can-find-me.md` section 6.2 — the card flag, the picker, the forward
 * rules and the group username.
 *
 * The flag is composed and decoded by the REAL core, so these tests follow
 * its wire.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { MessageContent } from "../../wasm/lakko_ffi_wasm";
import {
  VISIBILITY_MODE_NOBODY,
  cardPickState,
  ownCardShareable,
  usernameForConversation,
  withCardShareSignature,
} from "./card-share";
import { sniffProfile, type ProfileDecoder } from "./profile-envelope";
import { MemoryProfileBackend, ProfileStore, profileCardShareable, type StoredProfile } from "./profile-store";
import { decodeProfileRow, encodeProfileRow } from "./profile-idb";
import { owedProfileConversations } from "./rotation-resend";
import { canForwardContent, forwardPayloadOf } from "../chat/forward";
import { forwardEnabled } from "../chat/selection";
import { feedForwardPayload, feedItemForwardable } from "../feed/feed-forward";
import type { PersistedMessage } from "../chat/message-log-store";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const ACCOUNT = "acct";
const KEY = "a".repeat(43) + "=";
const CARD_KEY = "c".repeat(43) + "=";
const isNewer = (a: bigint, b: bigint): boolean => wasm.profileIsNewer(a, b);

const location: MessageContent = {
  kind: "location",
  location: { latE7: 600000000, lonE7: 250000000, accuracyM: 10, fixAtMs: 1700000000000, name: null },
};
const card: MessageContent = { kind: "contactCard", card: { name: "Ann", username: "ann", accountKey: CARD_KEY } };
const article: MessageContent = { kind: "article", article: { title: "T", blocks: [{ kind: "image", attachmentIndex: 0, caption: [] }] } };
const poll = { kind: "poll", poll: {} } as unknown as MessageContent;
const checklist = { kind: "checklist", checklist: {} } as unknown as MessageContent;

describe("own card flag", () => {
  it("is off exactly for Nobody", () => {
    expect(VISIBILITY_MODE_NOBODY).toBe(wasm.VisibilityMode.Nobody);
    expect(ownCardShareable({ mode: wasm.VisibilityMode.Nobody })).toBe(false);
    expect(ownCardShareable({ mode: wasm.VisibilityMode.Some })).toBe(true);
    expect(ownCardShareable({ mode: wasm.VisibilityMode.Everyone })).toBe(true);
    expect(ownCardShareable(null)).toBe(true);
  });

  it("is carried by the composed envelope and read back by the sniffer", () => {
    const off = wasm.composeProfileEnvelope("Ann", undefined, 5n, undefined, undefined, undefined, undefined, ownCardShareable({ mode: wasm.VisibilityMode.Nobody }));
    const on = wasm.composeProfileEnvelope("Ann", undefined, 5n, undefined, undefined, undefined, undefined, true);
    const decoder = wasm as unknown as ProfileDecoder;
    const a = sniffProfile(decoder, off);
    const b = sniffProfile(decoder, on);
    expect(a.kind === "profile" && a.profile.cardShareable).toBe(false);
    expect(b.kind === "profile" && b.profile.cardShareable !== false).toBe(true);
  });

  it("a switch to or from Nobody makes every conversation's profile due again", () => {
    const grants = "key:abc|member:1";
    const before = withCardShareSignature(grants, true);
    const after = withCardShareSignature(grants, false);
    expect(after).not.toBe(before);
    const rows = ["g1", "g2"].map((id) => ({ groupIdB64: id, active: true, publishedSignature: before, currentSignature: after }));
    expect(owedProfileConversations(rows)).toEqual(["g1", "g2"]);
    // And back.
    const back = rows.map((r) => ({ ...r, publishedSignature: after, currentSignature: before }));
    expect(owedProfileConversations(back)).toEqual(["g1", "g2"]);
    // No switch: nothing is due.
    expect(owedProfileConversations(rows.map((r) => ({ ...r, currentSignature: before })))).toEqual([]);
  });
});

describe("received card flag", () => {
  function stored(at: bigint, cardShareable?: boolean): StoredProfile {
    return {
      displayName: "Ann",
      username: null,
      updatedAtMs: at,
      accountIdHex: null,
      usernameCertificateB64: null,
      ...(cardShareable === undefined ? {} : { cardShareable }),
    };
  }

  it("no profile, or one without the field, counts as shareable", () => {
    expect(profileCardShareable(undefined)).toBe(true);
    expect(profileCardShareable(stored(1n))).toBe(true);
    expect(profileCardShareable(stored(1n, false))).toBe(false);
  });

  it("the store takes it only from a strictly newer profile", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.open(ACCOUNT);
    await store.ingest(ACCOUNT, KEY, { displayName: "Ann", username: null, updatedAtMs: 5n });
    expect(profileCardShareable(store.get(KEY))).toBe(true);
    // The same stamp sent again changes nothing: the sender restamps on a
    // switch to Nobody (see own-flag tests below).
    expect(await store.ingest(ACCOUNT, KEY, { displayName: "Ann", username: null, updatedAtMs: 5n, cardShareable: false })).toBe(false);
    expect(profileCardShareable(store.get(KEY))).toBe(true);
    // The restamped profile is newer and is taken.
    expect(await store.ingest(ACCOUNT, KEY, { displayName: "Ann", username: null, updatedAtMs: 6n, cardShareable: false })).toBe(true);
    expect(profileCardShareable(store.get(KEY))).toBe(false);
    // An older stamp never changes it.
    await store.ingest(ACCOUNT, KEY, { displayName: "Ann", username: null, updatedAtMs: 4n, cardShareable: true });
    expect(profileCardShareable(store.get(KEY))).toBe(false);
  });

  it("survives the persisted row", () => {
    const row = { profiles: new Map([[KEY, stored(5n, false)], [CARD_KEY, stored(6n)]]), verifications: new Map() };
    const back = decodeProfileRow(encodeProfileRow(row, new Map()))!;
    expect(profileCardShareable(back.profiles.get(KEY))).toBe(false);
    expect(profileCardShareable(back.profiles.get(CARD_KEY))).toBe(true);
  });
});

describe("contact picker", () => {
  it("disables a contact without a username, then one who does not let their card be shared", () => {
    expect(cardPickState({ username: null, cardShareable: true })).toBe("no-username");
    expect(cardPickState({ username: null, cardShareable: false })).toBe("no-username");
    expect(cardPickState({ username: "ann", cardShareable: false })).toBe("not-shareable");
    expect(cardPickState({ username: "ann", cardShareable: true })).toBe("enabled");
  });
});

describe("forwarding content (UX section 11a)", () => {
  const shareable = (): boolean => true;
  const notShareable = (k: string): boolean => k !== CARD_KEY;

  it("offers Forward for text, location, contact card and article, never a poll or checklist", () => {
    expect(canForwardContent(undefined, shareable)).toBe(true);
    expect(canForwardContent(location, shareable)).toBe(true);
    expect(canForwardContent(card, shareable)).toBe(true);
    expect(canForwardContent(article, shareable)).toBe(true);
    expect(canForwardContent(poll, shareable)).toBe(false);
    expect(canForwardContent(checklist, shareable)).toBe(false);
  });

  it("does not offer it for a card whose person does not let it be shared", () => {
    expect(canForwardContent(card, notShareable)).toBe(false);
    expect(canForwardContent(location, notShareable)).toBe(true);
  });

  it("the payload carries the content with the forward line and no text", () => {
    const p = forwardPayloadOf({ text: "", attachments: ["img"], messageId: 7, content: article }, "Bob", "grp", false);
    expect(p.content).toEqual(article);
    expect(p.attachments).toEqual(["img"]);
    expect(p.text).toBe("");
    expect(p.forward).toEqual({ author: "Bob", origin: { groupIdB64: "grp", messageId: 7 } });
    // A poll handed in anyway is never carried.
    expect(forwardPayloadOf({ text: "", attachments: [], messageId: 7, content: poll }, "Bob", "grp", false).content).toBeUndefined();
  });

  it("a forward of content composes an envelope the receiver reads as that content with the forward line", () => {
    const bytes = wasm.composeContentEnvelope(location, [], { forwardedFrom: "Bob", forwardOriginGroupB64: "Z3Jw", forwardOriginMessageId: 7 });
    const parsed = wasm.parseBodyEnvelope(bytes);
    try {
      expect(parsed.content?.kind).toBe("location");
      expect(parsed.forwardedFrom).toBe("Bob");
      expect(Number(parsed.forwardOriginMessageId)).toBe(7);
    } finally {
      parsed.free();
    }
  });

  it("bulk Forward is off when any selected message may not be forwarded", () => {
    const kinds = new Map<number, MessageContent | undefined>([[1, undefined], [2, location], [3, poll]]);
    const forwardable = (id: number): boolean => canForwardContent(kinds.get(id), shareable);
    expect(forwardEnabled(new Set([1, 2]), false, forwardable)).toBe(true);
    expect(forwardEnabled(new Set([1, 3]), false, forwardable)).toBe(false);
    expect(forwardEnabled(new Set(), false, forwardable)).toBe(false);
  });

  it("the feed follows the same rules, for a group message and a channel post alike", () => {
    const msg = (content: MessageContent | undefined, attachments: PersistedMessage["attachments"] = []): PersistedMessage =>
      ({ messageId: "7", direction: "incoming", text: "", attachments, createdAt: "", ...(content === undefined ? {} : { content }) }) as PersistedMessage;
    expect(feedItemForwardable(msg(poll), shareable)).toBe(false);
    expect(feedItemForwardable(msg(card), notShareable)).toBe(false);
    expect(feedItemForwardable(msg(article), shareable)).toBe(true);
    expect(feedForwardPayload({ kind: "group", message: msg(location), originGroupIdB64: "grp", authorLabel: "Bob" }).content).toEqual(location);
    expect(feedForwardPayload({ kind: "channel", message: msg(card), channelTitle: "News" }).content).toEqual(card);
    // A channel article carries its images (the post's attachments).
    const image = { blobIdB64: "x" } as unknown as PersistedMessage["attachments"][number];
    const p = feedForwardPayload({ kind: "channel", message: msg(article, [image]), channelTitle: "News" });
    expect(p.content).toEqual(article);
    expect(p.attachments).toEqual([image]);
    expect(feedForwardPayload({ kind: "channel", message: msg(poll), channelTitle: "News" }).content).toBeUndefined();
  });
});

describe("username in a group's profile (who-can-find-me 6.2)", () => {
  it("a one-to-one always carries it, a group only while its members may find the person", () => {
    expect(usernameForConversation("ann", { oneToOne: true, groupAllowed: false })).toBe("ann");
    expect(usernameForConversation("ann", { oneToOne: false, groupAllowed: true })).toBe("ann");
    expect(usernameForConversation("ann", { oneToOne: false, groupAllowed: false })).toBeNull();
    expect(usernameForConversation("ann", null)).toBe("ann");
    expect(usernameForConversation(null, { oneToOne: true, groupAllowed: true })).toBeNull();
  });
});
