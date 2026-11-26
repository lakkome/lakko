// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The history bundle carries no bookkeeping system rows (`[mls <kind>]`):
 * the new web device restored every one as `Members changed` (live
 * 2026-10-28, a one-to-one after a pairing). Neither built nor applied.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import * as wasm from "../../wasm/lakko_ffi_wasm";
import init from "../../wasm/lakko_ffi_wasm";
import { WebDeviceCipher, type DeviceCipher } from "../auth/device-cipher";
import { appendToGroupSlot, summarizeGroupSlot } from "../chat/background-log";
import {
  loadMessageLog,
  saveMessageLog,
  updateMessageLog,
  type MessageLogPayload,
  type PersistedAttachment,
  type PersistedMessage,
} from "../chat/message-log-store";
import {
  attachmentRefFromPersisted,
  b64UrlEncode,
  persistedFromAttachmentRef,
} from "../chat/persisted-attachment";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import {
  applyGapHistoryBundle,
  applyHistoryBundleToLog,
  buildGapHistoryBundle,
  buildHistoryBundleFromLog,
  gapBundleRow,
  isBookkeepingSystemRow,
  placeholderAuthorKeyB64,
} from "./history-bundle";

const SENDER = "c2VuZGVyLWFjY291bnQtMDA=";
const JOINER = "am9pbmVyLWFjY291bnQtMDA=";
const GROUP_HEX = "ab".repeat(32);
const SECRET = new Uint8Array(32).fill(7);

function hexBytes(h: string): Uint8Array {
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(h.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function hexOf(b: Uint8Array): string {
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
});

async function deviceKey(): Promise<DeviceCipher> {
  return new WebDeviceCipher(
    await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]),
  );
}

const row = (id: number, direction: PersistedMessage["direction"], text: string): PersistedMessage => ({
  messageId: String(id),
  direction,
  text,
  attachments: [],
  createdAt: new Date(1_700_000_000_000 + id).toISOString(),
});

function texts(messages: readonly PersistedMessage[] | undefined): string[] {
  return (messages ?? []).map((m) => `${m.direction}:${m.text}`);
}

describe("bookkeeping system rows in the history bundle", () => {
  it("recognises the commit and join-request rows of every client, not a call row", () => {
    expect(isBookkeepingSystemRow(row(1, "system", "[mls commit]"))).toBe(true);
    expect(isBookkeepingSystemRow(row(1, "system", "[mls external-join-proposal]"))).toBe(true);
    expect(isBookkeepingSystemRow(row(1, "system", "[mls external join]"))).toBe(true);
    expect(isBookkeepingSystemRow(row(1, "incoming", "[mls commit]"))).toBe(false);
    expect(isBookkeepingSystemRow(row(1, "system", "call:v1 ..."))).toBe(false);
  });

  it("a web sender does not put them in the bundle", async () => {
    const key = await deviceKey();
    await saveMessageLog(SENDER, key, {
      version: 1,
      groups: {
        [GROUP_HEX]: {
          cursor: "5",
          messages: [
            row(1, "system", "[mls commit]"),
            row(2, "incoming", "hello"),
            row(3, "system", "[mls external-join-proposal]"),
            row(4, "outgoing", "hi back"),
            row(5, "system", "[mls commit]"),
          ],
        },
      },
    });
    const sealed = await buildHistoryBundleFromLog({ accountIdB64: SENDER, deviceKey: key, secret: SECRET });
    if (sealed === null) throw new Error("no bundle");
    // What is on the wire, not only what a web joiner keeps.
    const opened = wasm.openHistoryBundleStructured(SECRET, sealed);
    const carried = (opened.groups() as { messages(): unknown[] }[]).flatMap((g) => g.messages());
    expect(carried).toHaveLength(2);
    const joinerKey = await deviceKey();
    expect(await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: joinerKey, secret: SECRET, sealedBytes: sealed })).toBe(2);
    const log = await loadMessageLog(JOINER, joinerKey);
    expect(texts(log?.groups[GROUP_HEX]?.messages)).toEqual(["incoming:hello", "outgoing:hi back"]);
  });

  it("a received bundle that carries them (another client) does not log them", async () => {
    const messages = [
      [1, 2, "[mls commit]"],
      [2, 1, "from the other person"],
      [3, 2, "[mls external join]"],
      [4, 2, "[mls commit]"],
    ] as const;
    const inputs = messages.map(([id, direction, text]) => {
      const placeholder = wasm.encodeHistoryPlaceholder(
        BigInt(id),
        direction,
        new Date(1_700_000_000_000 + id).toISOString(),
        text,
        [],
      );
      const idBytes = new Uint8Array(8);
      idBytes[7] = id;
      return new wasm.HistoryMessageInput(placeholder, 0, idBytes, BigInt(1_700_000_000_000 + id));
    });
    const groupId = new Uint8Array(32).fill(0xab);
    const sealed = wasm.sealHistoryBundleStructured(SECRET, [new wasm.HistoryGroupInput(groupId, inputs, false)], 4n);
    const key = await deviceKey();
    expect(await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: key, secret: SECRET, sealedBytes: sealed })).toBe(1);
    const log = await loadMessageLog(JOINER, key);
    expect(texts(log?.groups[GROUP_HEX]?.messages)).toEqual(["incoming:from the other person"]);
  });
});

describe("history is not news (docs/ux/second-device.md section 2)", () => {
  /** A bundle holding `rows`, sealed by a real sender. */
  async function bundleOf(rows: readonly PersistedMessage[]): Promise<Uint8Array> {
    const key = await deviceKey();
    await saveMessageLog(SENDER, key, {
      version: 1,
      groups: { [GROUP_HEX]: { cursor: "9", messages: rows } },
    });
    const sealed = await buildHistoryBundleFromLog({ accountIdB64: SENDER, deviceKey: key, secret: SECRET });
    if (sealed === null) throw new Error("no bundle");
    return sealed;
  }

  const apply = (key: DeviceCipher, sealedBytes: Uint8Array): Promise<number> =>
    applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: key, secret: SECRET, sealedBytes });

  async function slotOf(key: DeviceCipher) {
    return (await loadMessageLog(JOINER, key))?.groups[GROUP_HEX];
  }

  it("what came with the bundle is not unread; what comes after it is", async () => {
    const key = await deviceKey();
    await apply(key, await bundleOf([row(2, "incoming", "old"), row(5, "incoming", "older still?")]));
    const slot = await slotOf(key);
    expect(slot?.syncedThroughMessageId).toBe("5");
    expect(summarizeGroupSlot(slot, null).unread).toBe(0);
    // A synced read position that lands later, behind the bundle.
    expect(summarizeGroupSlot(slot, 1n).unread).toBe(0);
    // No read position of the account moved.
    expect(slot?.lastViewedMessageId).toBeUndefined();
    const later = appendToGroupSlot(slot, [row(6, "incoming", "new")], 6);
    expect(summarizeGroupSlot(later, null).unread).toBe(1);
  });

  it("a second bundle keeps the higher floor and what the slot already said", async () => {
    const key = await deviceKey();
    await apply(key, await bundleOf([row(8, "incoming", "newer")]));
    await updateMessageLog(JOINER, key, (current) => {
      const slot = current?.groups[GROUP_HEX];
      if (current === null || slot === undefined) return null;
      return {
        version: current.version,
        groups: { [GROUP_HEX]: { ...slot, lastViewedMessageId: "8", feedSeen: ["9"] } },
      };
    });
    await apply(key, await bundleOf([row(3, "incoming", "older")]));
    const slot = await slotOf(key);
    expect(slot?.syncedThroughMessageId).toBe("8");
    expect(slot?.lastViewedMessageId).toBe("8");
    expect(slot?.feedSeen).toEqual(["9"]);
    expect(slot?.messages.map((m) => m.messageId)).toEqual(["3", "8"]);
  });

  it("the floor covers a bookkeeping row another client carried, which is not filed", async () => {
    const inputs = [
      [1, 1, "from the other person"],
      [4, 2, "[mls commit]"],
    ] as const;
    const messages = inputs.map(([id, direction, text]) => {
      const placeholder = wasm.encodeHistoryPlaceholder(BigInt(id), direction, "t", text, []);
      const idBytes = new Uint8Array(8);
      idBytes[7] = id;
      return new wasm.HistoryMessageInput(placeholder, 0, idBytes, 1n);
    });
    const sealed = wasm.sealHistoryBundleStructured(
      SECRET,
      [new wasm.HistoryGroupInput(new Uint8Array(32).fill(0xab), messages, false)],
      4n,
    );
    const key = await deviceKey();
    await apply(key, sealed);
    const slot = await slotOf(key);
    expect(slot?.messages).toHaveLength(1);
    expect(slot?.syncedThroughMessageId).toBe("4");
  });

  it("does not lose what the background receive files while it is written", async () => {
    const key = await deviceKey();
    const sealed = await bundleOf([row(2, "incoming", "old")]);
    const other = "cd".repeat(32);
    const background = (hex: string, id: number): Promise<void> =>
      updateMessageLog(JOINER, key, (current) => {
        const groups = current?.groups ?? {};
        return {
          version: 1,
          groups: { ...groups, [hex]: appendToGroupSlot(groups[hex], [row(id, "incoming", "live")], id) },
        };
      });
    await Promise.all([background(other, 7), apply(key, sealed), background(GROUP_HEX, 9)]);
    const log = await loadMessageLog(JOINER, key);
    expect(log?.groups[other]?.messages.map((m) => m.messageId)).toEqual(["7"]);
    expect(log?.groups[GROUP_HEX]?.messages.map((m) => m.messageId)).toEqual(["2", "9"]);
    expect(log?.groups[GROUP_HEX]?.syncedThroughMessageId).toBe("2");
  });

  it("keeps the sends the log holds", async () => {
    const key = await deviceKey();
    const held = [{ conversationHex: "ee" }] as unknown as NonNullable<MessageLogPayload["held"]>;
    await saveMessageLog(JOINER, key, { version: 1, groups: {}, held });
    await apply(key, await bundleOf([row(2, "incoming", "old")]));
    expect((await loadMessageLog(JOINER, key))?.held).toEqual(held);
  });
});

describe("the invitation card, the reply and the forward reach the new device", () => {
  // Seen live 2026-11-13: an invitation the old device drew as a card reached
  // the new device as its bare `/g/` link (the placeholder carried text only).
  const card = new Uint8Array(400).fill(0x5a);
  const originGroup = new Uint8Array(32).fill(0x42);

  it("a web sender carries them and a web joiner stores them", async () => {
    const key = await deviceKey();
    const invitation: PersistedMessage = {
      ...row(10, "outgoing", "https://lakko.me/g/abcdefgh#secret"),
      channelInviteB64: btoa(String.fromCharCode(...card)),
    };
    const reply: PersistedMessage = {
      ...row(11, "incoming", "yes"),
      reply: { target: "10", author: "Ann", excerpt: "come along" },
    };
    const forward: PersistedMessage = {
      ...row(12, "incoming", "look"),
      forward: {
        author: "Bob",
        origin: { groupIdB64: btoa(String.fromCharCode(...originGroup)), messageId: "77" },
      },
    };
    await saveMessageLog(SENDER, key, {
      version: 1,
      groups: { [GROUP_HEX]: { cursor: "12", messages: [invitation, reply, forward] } },
    });
    const sealed = await buildHistoryBundleFromLog({ accountIdB64: SENDER, deviceKey: key, secret: SECRET });
    if (sealed === null) throw new Error("no bundle");
    const joinerKey = await deviceKey();
    expect(
      await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: joinerKey, secret: SECRET, sealedBytes: sealed }),
    ).toBe(3);
    const rows = (await loadMessageLog(JOINER, joinerKey))?.groups[GROUP_HEX]?.messages ?? [];
    expect(rows.find((m) => m.messageId === "10")?.channelInviteB64).toBe(invitation.channelInviteB64);
    expect(rows.find((m) => m.messageId === "11")?.reply).toEqual(reply.reply);
    expect(rows.find((m) => m.messageId === "12")?.forward).toEqual(forward.forward);
    // A plain row gains none of them.
    expect(rows.find((m) => m.messageId === "11")?.channelInviteB64).toBeUndefined();
    expect(rows.find((m) => m.messageId === "10")?.reply).toBeUndefined();
  });

  it("a native sender's placeholder (raw bytes on the wire) decodes the same", () => {
    const placeholder = wasm.encodeHistoryPlaceholder(
      12n,
      1,
      "t",
      "look",
      [],
      card,
      3n,
      undefined,
      "quoted",
      undefined,
      originGroup,
      77n,
    );
    const opened = wasm.decodeHistoryPlaceholder(placeholder);
    expect(Array.from(opened.channelInvite ?? [])).toEqual(Array.from(card));
    expect(opened.replyTarget).toBe(3n);
    expect(opened.replyAuthor).toBeUndefined();
    expect(opened.replyExcerpt).toBe("quoted");
    expect(opened.forwardedFrom).toBeUndefined();
    expect(Array.from(opened.forwardOriginGroup ?? [])).toEqual(Array.from(originGroup));
    expect(opened.forwardOriginMessage).toBe(77n);
  });
});

describe("the micro thumbnail travels in the history bundle (ADR-0047 section 6)", () => {
  const b64 = (n: number, fill: number): string => btoa(String.fromCharCode(...new Uint8Array(n).fill(fill)));

  it("a joiner's log keeps the first photo's inline thumbnail", async () => {
    const jpeg = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 1, 2, 3]);
    const key = await deviceKey();
    const photo: PersistedMessage = {
      ...row(1, "incoming", "The trip"),
      attachments: [
        {
          blobIdB64: b64(32, 1),
          keyB64: b64(32, 2),
          nonceB64: b64(12, 3),
          mime: "image/jpeg",
          sizeStr: "100",
          name: null,
          width: 640,
          height: 480,
          inlineThumbB64: b64UrlEncode(jpeg),
        },
      ],
    };
    await saveMessageLog(SENDER, key, { version: 1, groups: { [GROUP_HEX]: { cursor: "1", messages: [photo] } } });
    const sealed = await buildHistoryBundleFromLog({ accountIdB64: SENDER, deviceKey: key, secret: SECRET });
    if (sealed === null) throw new Error("no bundle");
    const joinerKey = await deviceKey();
    expect(await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: joinerKey, secret: SECRET, sealedBytes: sealed })).toBe(1);
    const log = await loadMessageLog(JOINER, joinerKey);
    const carried = log?.groups[GROUP_HEX]?.messages[0]?.attachments[0];
    expect(attachmentRefFromPersisted(carried as PersistedAttachment).ref.inlineThumb).toEqual(jpeg);
  });
});

describe("a bundled message keeps its author (ADR-0091 live walk 3)", () => {
  // Seen on Android 2026-11-20: the gap bundle's messages came with a "?"
  // avatar and no name; the placeholder carried no author at all.
  const key = (fill: number): string => btoa(String.fromCharCode(...new Uint8Array(32).fill(fill)));
  const OWNER = key(0x0a);
  const MEMBER = key(0x0b);
  const THIRD = key(0x0c);

  it("the sender names the author of each row: stored, own for sent, none for system", () => {
    expect(placeholderAuthorKeyB64({ ...row(1, "incoming", "x"), authorKeyB64: THIRD }, OWNER)).toBe(THIRD);
    expect(placeholderAuthorKeyB64(row(2, "outgoing", "x"), OWNER)).toBe(OWNER);
    expect(placeholderAuthorKeyB64({ ...row(3, "outgoing", "x"), authorKeyB64: OWNER }, null)).toBe(OWNER);
    expect(placeholderAuthorKeyB64({ ...row(4, "system", "x"), authorKeyB64: OWNER }, OWNER)).toBeUndefined();
    expect(placeholderAuthorKeyB64({ ...row(5, "incoming", "x"), authorKeyB64: "c2hvcnQ=" }, OWNER)).toBeUndefined();
  });

  it("the device that came back files the owner's rows as the owner's and its own as its own", async () => {
    const ownerKey = await deviceKey();
    const rows: PersistedMessage[] = [
      { ...row(1, "outgoing", "from the owner"), authorKeyB64: null },
      { ...row(2, "incoming", "from a third"), authorKeyB64: THIRD },
      { ...row(3, "incoming", "from the member, another device"), authorKeyB64: MEMBER },
    ];
    await saveMessageLog(SENDER, ownerKey, { version: 1, groups: { [GROUP_HEX]: { cursor: "3", messages: rows } } });
    const readmitter = wasm.Identity.fromSeed(new Uint8Array(32).fill(0x31));
    const gap = wasm.generateGapKey();
    const gapSecret = gap.secret;
    const gapPublic = gap.publicKey;
    gap.free();
    const sealed = await buildGapHistoryBundle({
      accountIdB64: SENDER,
      deviceKey: ownerKey,
      readmitter,
      gapKey: gapPublic,
      slotHex: GROUP_HEX,
      fromMs: 0,
      ownAuthorKeyB64: OWNER,
    });
    if (sealed === null) throw new Error("no bundle");
    const memberKey = await deviceKey();
    const signers: string[] = [];
    const filed = await applyGapHistoryBundle({
      accountIdB64: JOINER,
      deviceKey: memberKey,
      gapSecret,
      groupId: hexBytes(GROUP_HEX),
      sealedBytes: sealed,
      signerAccepted: (signer) => {
        signers.push(hexOf(signer));
        return true;
      },
      ownAuthorKeyB64: MEMBER,
      route: "knock",
    });
    if (filed === null) throw new Error("not filed");
    // The signer the group is asked about is the re-admitter's leaf key.
    expect(signers).toEqual([hexOf(readmitter.signaturePublicKey)]);
    const byText = new Map(filed.rows.map((m) => [m.text, m]));
    expect(byText.get("from the owner")).toMatchObject({ direction: "incoming", authorKeyB64: OWNER });
    expect(byText.get("from a third")).toMatchObject({ direction: "incoming", authorKeyB64: THIRD });
    expect(byText.get("from the member, another device")).toMatchObject({ direction: "outgoing", authorKeyB64: MEMBER });
    // The log holds the same rows as the open chat was shown.
    const logged = (await loadMessageLog(JOINER, memberKey))?.groups[GROUP_HEX]?.messages ?? [];
    expect(logged.find((m) => m.messageId === "1")).toMatchObject({ direction: "incoming", authorKeyB64: OWNER });
    expect(logged.find((m) => m.messageId === "3")?.direction).toBe("outgoing");
  });

  it("a gap bundle whose signer the joined group does not accept is dropped; nothing is filed", async () => {
    const ownerKey = await deviceKey();
    await saveMessageLog(SENDER, ownerKey, {
      version: 1,
      groups: { [GROUP_HEX]: { cursor: "1", messages: [{ ...row(1, "incoming", "x"), authorKeyB64: THIRD }] } },
    });
    const readmitter = wasm.Identity.fromSeed(new Uint8Array(32).fill(0x32));
    const gap = wasm.generateGapKey();
    const gapSecret = gap.secret;
    const gapPublic = gap.publicKey;
    gap.free();
    const sealed = await buildGapHistoryBundle({
      accountIdB64: SENDER,
      deviceKey: ownerKey,
      readmitter,
      gapKey: gapPublic,
      slotHex: GROUP_HEX,
      fromMs: 0,
      ownAuthorKeyB64: OWNER,
    });
    if (sealed === null) throw new Error("no bundle");
    const memberKey = await deviceKey();
    const filed = await applyGapHistoryBundle({
      accountIdB64: JOINER,
      deviceKey: memberKey,
      gapSecret,
      groupId: hexBytes(GROUP_HEX),
      sealedBytes: sealed,
      signerAccepted: () => false,
      ownAuthorKeyB64: MEMBER,
      route: "knock",
    });
    expect(filed).toBeNull();
    expect((await loadMessageLog(JOINER, memberKey))?.groups[GROUP_HEX]).toBeUndefined();
    // Another device's gap secret does not open it at all.
    const other = wasm.generateGapKey();
    const otherSecret = other.secret;
    other.free();
    await expect(
      applyGapHistoryBundle({
        accountIdB64: JOINER,
        deviceKey: memberKey,
        gapSecret: otherSecret,
        groupId: hexBytes(GROUP_HEX),
        sealedBytes: sealed,
        signerAccepted: () => true,
        ownAuthorKeyB64: MEMBER,
        route: "knock",
      }),
    ).rejects.toBeDefined();
  });

  it("a row naming no author falls back to the route; a system row names nobody", () => {
    const sent = row(1, "outgoing", "x");
    expect(gapBundleRow(sent, MEMBER, "knock").direction).toBe("incoming");
    expect(gapBundleRow(sent, MEMBER, undefined).direction).toBe("incoming");
    expect(gapBundleRow(sent, MEMBER, "own-device").direction).toBe("outgoing");
    expect(gapBundleRow(sent, MEMBER, "mailbox").direction).toBe("incoming");
    expect(gapBundleRow(row(2, "incoming", "x"), MEMBER, "mailbox").direction).toBe("outgoing");
    const system = gapBundleRow({ ...row(3, "system", "x"), authorKeyB64: OWNER }, MEMBER, "knock");
    expect(system.direction).toBe("system");
    expect(system.authorKeyB64).toBeUndefined();
  });

  it("ADR-0101 walk D2: a row by this account's key of before a reset is its own", () => {
    // The contact received it from this account's previous key and sends it
    // back as incoming, naming that key.
    const old = { ...row(4, "incoming", "a1"), authorKeyB64: OWNER };
    expect(gapBundleRow(old, MEMBER, "mailbox").direction).toBe("incoming");
    expect(gapBundleRow(old, MEMBER, "mailbox", [OWNER]).direction).toBe("outgoing");
    expect(gapBundleRow(old, null, undefined, [OWNER]).direction).toBe("outgoing");
    // Another person's row stays theirs.
    const theirs = { ...row(5, "outgoing", "b1"), authorKeyB64: THIRD };
    expect(gapBundleRow(theirs, MEMBER, "mailbox", [OWNER]).direction).toBe("incoming");
  });

  it("a pairing bundle keeps the author of a received row", async () => {
    const key1 = await deviceKey();
    await saveMessageLog(SENDER, key1, {
      version: 1,
      groups: { [GROUP_HEX]: { cursor: "1", messages: [{ ...row(1, "incoming", "hi"), authorKeyB64: THIRD }] } },
    });
    const sealed = await buildHistoryBundleFromLog({ accountIdB64: SENDER, deviceKey: key1, secret: SECRET });
    if (sealed === null) throw new Error("no bundle");
    const key2 = await deviceKey();
    await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: key2, secret: SECRET, sealedBytes: sealed });
    const got = (await loadMessageLog(JOINER, key2))?.groups[GROUP_HEX]?.messages[0];
    expect(got).toMatchObject({ direction: "incoming", authorKeyB64: THIRD });
  });
});

describe("a live row's attachments travel (seen live 2026-11-23)", () => {
  // A device added from web got no history at all: a live row stores its
  // attachment ids as base64url, the bundle encoder read them with `atob`,
  // which throws on `-` and `_`, and the throw dropped the whole bundle.
  const urlHeavy = (n: number): Uint8Array => new Uint8Array(n).fill(0xfb);

  it("a live-stored attachment with - and _ in its ids is carried", async () => {
    const key = await deviceKey();
    const file: PersistedMessage = {
      ...row(2, "outgoing", ""),
      attachments: [
        persistedFromAttachmentRef({
          blobId: urlHeavy(32),
          key: urlHeavy(32),
          nonce: urlHeavy(12),
          mime: "application/pdf",
          size: 1234,
          name: "notes.pdf",
        }),
      ],
    };
    expect(file.attachments[0]?.blobIdB64).toMatch(/[-_]/);
    await saveMessageLog(SENDER, key, {
      version: 1,
      groups: { [GROUP_HEX]: { cursor: "2", messages: [row(1, "incoming", "hi"), file] } },
    });
    const sealed = await buildHistoryBundleFromLog({ accountIdB64: SENDER, deviceKey: key, secret: SECRET });
    if (sealed === null) throw new Error("no bundle");
    const joinerKey = await deviceKey();
    expect(
      await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: joinerKey, secret: SECRET, sealedBytes: sealed }),
    ).toBe(2);
    const carried = (await loadMessageLog(JOINER, joinerKey))?.groups[GROUP_HEX]?.messages.find(
      (m) => m.messageId === "2",
    );
    const ref = attachmentRefFromPersisted(carried?.attachments[0] as PersistedAttachment).ref;
    expect(Array.from(ref.blobId)).toEqual(Array.from(urlHeavy(32)));
    expect(ref.name).toBe("notes.pdf");
  });

  it("a row that cannot be encoded is left out and the rest still goes", async () => {
    const key = await deviceKey();
    const broken: PersistedMessage = {
      ...row(2, "incoming", "broken"),
      attachments: [
        {
          blobIdB64: "not base64 at all!",
          keyB64: "x",
          nonceB64: "y",
          mime: null,
          sizeStr: "1",
          name: null,
        },
      ],
    };
    await saveMessageLog(SENDER, key, {
      version: 1,
      groups: { [GROUP_HEX]: { cursor: "3", messages: [row(1, "incoming", "one"), broken, row(3, "outgoing", "three")] } },
    });
    const sealed = await buildHistoryBundleFromLog({ accountIdB64: SENDER, deviceKey: key, secret: SECRET });
    if (sealed === null) throw new Error("no bundle");
    const joinerKey = await deviceKey();
    await applyHistoryBundleToLog({ accountIdB64: JOINER, deviceKey: joinerKey, secret: SECRET, sealedBytes: sealed });
    const log = await loadMessageLog(JOINER, joinerKey);
    expect(texts(log?.groups[GROUP_HEX]?.messages)).toEqual(["incoming:one", "outgoing:three"]);
  });
});
