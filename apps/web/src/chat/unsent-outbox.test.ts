// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The outbox (`docs/ux/connection-state.md` section 7.3), over the REAL
 * stores: the message log (`message-log-store.ts`) and the prepared-file
 * store (`unsent-files.ts`) on the in-memory IndexedDB of
 * `test/fake-storage.ts`, encrypted with a real AES-GCM device key. The
 * runner is the real `unsent-runner.ts` and the connection state the real
 * `net/connection-state.ts`; only the network side of a send (upload,
 * publish, filing the sent row) is a fake, recorded per test.
 *
 * "A reload" is a new `UnsentOutbox` over the same IndexedDB: nothing of the
 * first instance's memory reaches it.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { resetAccountStoreGateForTests } from "../auth/account-store-gate";
import { WebDeviceCipher, type DeviceCipher } from "../auth/device-cipher";
import { noteConnectionEvent, resetConnectionStateForTest } from "../net/connection-state";
import type { AttachmentRefBytes } from "./attachments";
import { loadMessageLog, MESSAGE_LOG_VERSION, updateMessageLog } from "./message-log-store";
import {
  allocateUnsentTempId,
  deviceStorePersistence,
  unsentFromPersisted,
  unsentToPersisted,
  UnsentOutbox,
  type UnsentEntry,
} from "./unsent-outbox";
import { decodePreparedFile, encodePreparedFile, unsentFileKeysFor } from "./unsent-files";
import { sendUnsentHeadless, UnsentRunner, type HeadlessDeps } from "./unsent-runner";
import { isNetworkSendFailure, MAX_AUTO_SENDS, runWhenServersAnswer } from "./waiting-sends";

const ACCOUNT = "QUFBQUFBQUFBQUFBQUFBQQ==";
const G = "Z3JvdXAtb25lLWlkLWJ5dGVzLTMyLWxvbmctIQ==";

let cipher: DeviceCipher;

beforeEach(async () => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
  resetAccountStoreGateForTests();
  const key = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
  cipher = new WebDeviceCipher(key);
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
  resetAccountStoreGateForTests();
  resetConnectionStateForTest();
});

/** A store over the real persistence, bound to the account. */
function newOutbox(): UnsentOutbox {
  const outbox = new UnsentOutbox(deviceStorePersistence(async () => cipher));
  outbox.bind(ACCOUNT);
  return outbox;
}

function textEntry(tempId: number, text: string, state: UnsentEntry["state"] = "waiting"): UnsentEntry {
  return { groupIdB64: G, tempId, writtenAtMs: 1_000 + tempId, state, autoSends: 0, text, attachments: [] };
}

function voiceEntry(tempId: number): UnsentEntry {
  return {
    groupIdB64: G,
    tempId,
    writtenAtMs: 1_000 + tempId,
    state: "waiting",
    autoSends: 0,
    text: "",
    attachments: [],
    media: {
      name: "voice.m4a",
      mime: "audio/mp4",
      meta: { kind: "voice", durationMs: 2_000, waveform: new Uint8Array([1, 2, 3]) },
    },
  };
}

function ref(tag: number): AttachmentRefBytes {
  return {
    blobId: new Uint8Array(32).fill(tag),
    key: new Uint8Array(32).fill(tag),
    nonce: new Uint8Array(12).fill(tag),
    mime: "audio/mp4",
    size: 3,
    name: "voice.m4a",
    kind: "voice",
    durationMs: 2_000,
  };
}

/** The network side of a send, recorded. */
class FakeNetwork {
  up = false;
  /** Temp ids the server refuses. */
  readonly refused = new Set<number>();
  /** What reached the conversation, in order: text, or `upload:<bytes>`. */
  readonly published: string[] = [];
  readonly uploads: string[] = [];
  /** Stored entries seen at the moment each publish was tried. */
  readonly storedAtAttempt: number[][] = [];
  private nextMessageId = 100;
  /** The one-to-one send decision the gate returns. */
  verdict: "send" | "wait" | "give-up" = "send";
  /** Per publish attempt that composed: the first byte of each blob id. */
  readonly composedWith: number[][] = [];
  /** Tray uploads to adopt, by tray item id. */
  readonly adoptions = new Map<number, Promise<AttachmentRefBytes | null>>();
  /** Entries handed to the held queue. */
  readonly held: number[] = [];

  constructor(private readonly outbox: UnsentOutbox) {}

  deps(): HeadlessDeps<{ id: string }> {
    return {
      group: (g) => (g === G ? { id: g } : undefined),
      ended: () => false,
      contentSendable: async () => true,
      preparedFile: (e) => this.outbox.preparedFile(e.groupIdB64, e.tempId),
      loadGif: async () => {
        throw new Error("no GIF here");
      },
      upload: async (bytes) => {
        if (!this.up) throw new TypeError("fetch failed");
        this.uploads.push(Array.from(bytes).join(","));
        return ref(bytes[0] ?? 0);
      },
      uploadPoster: async () => {
        throw new Error("no poster here");
      },
      uploaded: (e, r) => this.outbox.update(e.groupIdB64, e.tempId, { attachments: [r], endMedia: true }),
      adoptTrayUpload: (id) => this.adoptions.get(id) ?? null,
      uploadPart: async (part) => {
        if (!this.up) throw new TypeError("fetch failed");
        this.uploads.push(Array.from(part.bytes).join(","));
        return ref(part.bytes[0] ?? 0);
      },
      partUploaded: (e, index, r) => this.outbox.partUploaded(e.groupIdB64, e.tempId, index, r),
      partsDone: (e, attachments) => this.outbox.partsDone(e.groupIdB64, e.tempId, attachments),
      compose: async (e, attachments) => {
        this.composedWith.push(attachments.map((a) => a.blobId[0] ?? -1));
        return new TextEncoder().encode(e.text.length > 0 ? e.text : `media:${attachments.length}`);
      },
      publish: async (_g, envelope) => {
        const log = await loadMessageLog(ACCOUNT, cipher);
        this.storedAtAttempt.push((log?.outbox ?? []).map((r) => Number((r as { tempId: string }).tempId)));
        if (!this.up) throw new TypeError("fetch failed");
        const text = new TextDecoder().decode(envelope);
        this.published.push(text);
        this.nextMessageId += 1;
        return { messageId: this.nextMessageId, createdAt: "2026-11-24T10:00:00Z" };
      },
      file: async () => undefined,
      sendVerdict: async () => this.verdict,
      hold: (e) => {
        this.held.push(e.tempId);
        return true;
      },
      holdForSuccessor: () => false,
      isNetwork: (err) => isNetworkSendFailure(err, false),
    };
  }
}

/** A runner whose sends are refused per temp id, by wrapping publish. */
function runnerOver(outbox: UnsentOutbox, net: FakeNetwork): UnsentRunner {
  const deps = net.deps();
  const wrapped: HeadlessDeps<{ id: string }> = {
    ...deps,
    publish: async (g, envelope) => {
      const pending = outbox.entries(G).find((e) => e.state === "pending");
      if (pending !== undefined && net.refused.has(pending.tempId)) {
        throw new Error("403 refused");
      }
      return deps.publish(g, envelope);
    },
  };
  return new UnsentRunner(outbox, (entry) => sendUnsentHeadless(entry, wrapped));
}

/** Let every queued promise run (the pass is fired and forgotten). */
async function settle(): Promise<void> {
  for (let i = 0; i < 50; i += 1) await new Promise((r) => setTimeout(r, 0));
}

/** The servers stop answering, then answer again (section 7.2's trigger). */
function networkDropsAndReturns(): void {
  noteConnectionEvent("network-failure");
  noteConnectionEvent("answered");
}

describe("the stored form", () => {
  it("reads back what it wrote, and a pending entry reads back waiting", () => {
    const e: UnsentEntry = { ...voiceEntry(7), state: "pending", autoSends: 2 };
    const back = unsentFromPersisted(JSON.parse(JSON.stringify(unsentToPersisted(e))));
    expect(back?.state).toBe("waiting");
    expect(back?.autoSends).toBe(2);
    expect(back?.media?.meta.kind).toBe("voice");
    expect(back?.media?.meta.waveform).toEqual(new Uint8Array([1, 2, 3]));
    expect(back?.media?.name).toBe("voice.m4a");
  });

  it("drops junk instead of sending it", () => {
    expect(unsentFromPersisted({ groupIdB64: G, tempId: "x", state: "waiting" })).toBeNull();
    expect(unsentFromPersisted({ ...unsentToPersisted(textEntry(3, "")), text: "" })).toBeNull();
    expect(unsentFromPersisted(null)).toBeNull();
  });

  it("a prepared file round-trips and junk does not decode", () => {
    const f = { bytes: new Uint8Array([9, 8, 7]), posterBytes: new Uint8Array([1]) };
    expect(decodePreparedFile(encodePreparedFile(f))).toEqual(f);
    expect(decodePreparedFile(new Uint8Array([1, 4]))).toBeNull();
    expect(decodePreparedFile(new Uint8Array([1, 1, 0, 0, 0, 9, 1]))).toBeNull();
  });

  it("row ids keep growing across a clock that went back", () => {
    const a = allocateUnsentTempId(2_000_000);
    const b = allocateUnsentTempId(1_000_000);
    expect(b).toBeGreaterThan(a);
    expect(Number.isSafeInteger(b)).toBe(true);
  });
});

describe("the outbox over the real stores", () => {
  it("an entry is on disk once put resolves (before the first attempt)", async () => {
    const outbox = newOutbox();
    await outbox.put(textEntry(1, "hello", "pending"));
    const log = await loadMessageLog(ACCOUNT, cipher);
    expect(log?.outbox?.length).toBe(1);
    expect((log?.outbox?.[0] as { state: string }).state).toBe("waiting");
  });

  it("the runner's send finds the entry stored before it publishes", async () => {
    const outbox = newOutbox();
    const net = new FakeNetwork(outbox);
    net.up = true;
    await outbox.put(textEntry(5, "first"));
    const runner = runnerOver(outbox, net);
    await runner.pass(G).run();
    expect(net.published).toEqual(["first"]);
    expect(net.storedAtAttempt).toEqual([[5]]);
    await outbox.flushed();
    expect((await loadMessageLog(ACCOUNT, cipher))?.outbox ?? []).toEqual([]);
  });

  it("waiting text and waiting media survive a reload and go in order on entering Online", async () => {
    const before = newOutbox();
    await before.put(textEntry(10, "one"));
    await before.put(voiceEntry(11), { bytes: new Uint8Array([42, 43]) });
    await before.put(textEntry(12, "three"));
    await before.flushed();

    // The reload: a new store, nothing of the old one's memory.
    const after = newOutbox();
    await after.restore(ACCOUNT);
    expect(after.waitingIds(G)).toEqual([10, 11, 12]);
    expect((await after.preparedFile(G, 11))?.bytes).toEqual(new Uint8Array([42, 43]));

    const net = new FakeNetwork(after);
    const runner = runnerOver(after, net);
    const stop = runWhenServersAnswer({ run: async () => runner.runAll() });
    net.up = true;
    networkDropsAndReturns();
    await settle();
    stop();

    expect(net.published).toEqual(["one", "media:1", "three"]);
    expect(net.uploads).toEqual(["42,43"]);
    await after.flushed();
    expect(after.all()).toEqual([]);
    expect((await loadMessageLog(ACCOUNT, cipher))?.outbox ?? []).toEqual([]);
    // The prepared file went once its references existed.
    expect(await unsentFileKeysFor(ACCOUNT)).toEqual([]);
  });

  it("text written after a waiting media message goes after it, never ahead", async () => {
    const outbox = newOutbox();
    await outbox.put(voiceEntry(20), { bytes: new Uint8Array([7]) });
    await outbox.put(textEntry(21, "after"));
    const net = new FakeNetwork(outbox);
    const runner = runnerOver(outbox, net);

    // The network is down: the upload fails, the pass stops, the text waits
    // and spends nothing.
    await runner.pass(G).run();
    expect(net.published).toEqual([]);
    expect(outbox.get(G, 20)?.autoSends).toBe(1);
    expect(outbox.get(G, 21)?.autoSends).toBe(0);
    expect(outbox.waitingIds(G)).toEqual([20, 21]);

    net.up = true;
    await runner.pass(G).run();
    expect(net.published).toEqual(["media:1", "after"]);
  });

  it("a refused entry stays Not sent across a reload and is not sent again", async () => {
    const outbox = newOutbox();
    await outbox.put(textEntry(30, "nope"));
    await outbox.put(textEntry(31, "fine"));
    const net = new FakeNetwork(outbox);
    net.up = true;
    net.refused.add(30);
    await runnerOver(outbox, net).pass(G).run();
    expect(net.published).toEqual(["fine"]);
    expect(outbox.get(G, 30)?.state).toBe("failed");
    await outbox.flushed();

    const reloaded = newOutbox();
    await reloaded.restore(ACCOUNT);
    expect(reloaded.get(G, 30)?.state).toBe("failed");
    expect(reloaded.waitingIds(G)).toEqual([]);
    const net2 = new FakeNetwork(reloaded);
    net2.up = true;
    await runnerOver(reloaded, net2).pass(G).run();
    expect(net2.published).toEqual([]);
  });

  it("the fifth automatic network failure reads Not sent", async () => {
    const outbox = newOutbox();
    await outbox.put(textEntry(40, "slow"));
    const net = new FakeNetwork(outbox);
    const runner = runnerOver(outbox, net);
    for (let i = 0; i < MAX_AUTO_SENDS; i += 1) await runner.pass(G).run();
    expect(outbox.get(G, 40)).toMatchObject({ state: "failed", autoSends: MAX_AUTO_SENDS });
  });

  it("a conversation not loaded yet spends nothing", async () => {
    const outbox = newOutbox();
    await outbox.put({ ...textEntry(50, "elsewhere"), groupIdB64: "b3RoZXI=" });
    const net = new FakeNetwork(outbox);
    net.up = true;
    await runnerOver(outbox, net).pass("b3RoZXI=").run();
    expect(net.published).toEqual([]);
    expect(outbox.get("b3RoZXI=", 50)).toMatchObject({ state: "waiting", autoSends: 0 });
  });

  it("an attached view takes the sends of its rows; a row it does not hold goes headless", async () => {
    const outbox = newOutbox();
    await outbox.put(textEntry(60, "view"));
    await outbox.put(textEntry(61, "headless"));
    const net = new FakeNetwork(outbox);
    net.up = true;
    const runner = runnerOver(outbox, net);
    const viewSent: number[] = [];
    runner.attachView(G, async (id) => {
      if (id !== 60) return undefined;
      viewSent.push(id);
      await outbox.remove(G, id);
      return "sent";
    });
    await runner.pass(G).run();
    expect(viewSent).toEqual([60]);
    expect(net.published).toEqual(["headless"]);
  });

  it("a reset peer's one-to-one: the gate's `wait` hands the message to the held queue", async () => {
    const outbox = newOutbox();
    await outbox.put(textEntry(65, "to a reset peer"));
    const net = new FakeNetwork(outbox);
    net.up = true;
    net.verdict = "wait";
    await runnerOver(outbox, net).pass(G).run();
    expect(net.published).toEqual([]);
    expect(net.held).toEqual([65]);
    expect(outbox.all()).toEqual([]);
  });

  it("the gate's give-up reads Not sent", async () => {
    const outbox = newOutbox();
    await outbox.put(textEntry(66, "too late"));
    const net = new FakeNetwork(outbox);
    net.up = true;
    net.verdict = "give-up";
    await runnerOver(outbox, net).pass(G).run();
    expect(net.published).toEqual([]);
    expect(outbox.get(G, 66)?.state).toBe("failed");
  });

  it("delete removes the entry and its prepared file", async () => {
    const outbox = newOutbox();
    await outbox.put(voiceEntry(70), { bytes: new Uint8Array([1, 2]) });
    expect(await unsentFileKeysFor(ACCOUNT)).toHaveLength(1);
    await outbox.remove(G, 70);
    await outbox.flushed();
    expect(await unsentFileKeysFor(ACCOUNT)).toEqual([]);
    const reloaded = newOutbox();
    await reloaded.restore(ACCOUNT);
    expect(reloaded.all()).toEqual([]);
  });

  it("another writer of the message log keeps the outbox", async () => {
    const outbox = newOutbox();
    await outbox.put(textEntry(80, "kept"));
    await outbox.flushed();
    await updateMessageLog(ACCOUNT, cipher, (current) => ({
      version: MESSAGE_LOG_VERSION,
      groups: { ...(current?.groups ?? {}), aa: { cursor: "1", messages: [] } },
    }));
    const log = await loadMessageLog(ACCOUNT, cipher);
    expect(log?.outbox?.length).toBe(1);
    expect(Object.keys(log?.groups ?? {})).toEqual(["aa"]);
  });

  it("a failed write never stops the send: the entry lives in memory", async () => {
    const outbox = new UnsentOutbox({
      ...deviceStorePersistence(async () => cipher),
      save: async () => {
        throw new Error("disk full");
      },
    });
    outbox.bind(ACCOUNT);
    await outbox.put(textEntry(90, "memory only"));
    expect(outbox.waitingIds(G)).toEqual([90]);
    const net = new FakeNetwork(outbox);
    net.up = true;
    await runnerOver(outbox, net).pass(G).run();
    expect(net.published).toEqual(["memory only"]);
  });
});

describe("a composer-tray message with an item still to upload (media-messages section 11)", () => {
  function trayEntry(tempId: number, trayItemId?: number): UnsentEntry {
    return {
      groupIdB64: G,
      tempId,
      writtenAtMs: 1_000 + tempId,
      state: "waiting",
      autoSends: 0,
      text: "two photos",
      attachments: [],
      parts: [
        { kind: "ref", ref: ref(1) },
        {
          kind: "upload",
          localId: new Uint8Array(32).fill(0xee),
          name: "b.jpg",
          mime: "image/jpeg",
          size: 1,
          source: "media",
          dimensions: { width: 4, height: 3 },
          ...(trayItemId === undefined ? {} : { trayItemId }),
        },
      ],
    };
  }

  it("waits with no network, survives a reload, and goes in order on entering Online", async () => {
    const before = newOutbox();
    await before.put(textEntry(200, "before"));
    await before.put(trayEntry(201), { parts: [null, { bytes: new Uint8Array([77]) }] });
    await before.put(textEntry(202, "after"));
    const offline = new FakeNetwork(before);
    await runnerOver(before, offline).pass(G).run();
    expect(offline.published).toEqual([]);
    expect(before.get(G, 201)).toMatchObject({ state: "waiting", autoSends: 0 });
    await before.flushed();

    const after = newOutbox();
    await after.restore(ACCOUNT);
    expect(after.waitingIds(G)).toEqual([200, 201, 202]);
    const restored = after.get(G, 201);
    expect(restored?.parts?.map((p) => p.kind)).toEqual(["ref", "upload"]);
    expect((await after.preparedFile(G, 201))?.parts?.[1]?.bytes).toEqual(new Uint8Array([77]));

    const net = new FakeNetwork(after);
    const runner = runnerOver(after, net);
    const stop = runWhenServersAnswer({ run: async () => runner.runAll() });
    net.up = true;
    networkDropsAndReturns();
    await settle();
    stop();

    expect(net.published).toEqual(["before", "two photos", "after"]);
    expect(net.uploads).toEqual(["77"]);
    // The message carries both items, in order: the one uploaded before Send
    // and the one uploaded as part of the send.
    expect(net.composedWith[1]).toEqual([1, 77]);
    await after.flushed();
    expect(after.all()).toEqual([]);
    expect(await unsentFileKeysFor(ACCOUNT)).toEqual([]);
  });

  it("adopts the tray's own upload instead of uploading the bytes again", async () => {
    const outbox = newOutbox();
    await outbox.put(trayEntry(210, 5), { parts: [null, { bytes: new Uint8Array([33]) }] });
    const net = new FakeNetwork(outbox);
    net.up = true;
    net.adoptions.set(5, Promise.resolve(ref(55)));
    await runnerOver(outbox, net).pass(G).run();
    expect(net.uploads).toEqual([]);
    expect(net.composedWith).toEqual([[1, 55]]);
  });

  it("an item that landed is kept when the publish then gets no answer", async () => {
    const outbox = newOutbox();
    await outbox.put(trayEntry(220), { parts: [null, { bytes: new Uint8Array([44]) }] });
    const net = new FakeNetwork(outbox);
    net.up = true;
    const deps = net.deps();
    const runner = new UnsentRunner(outbox, (entry) =>
      sendUnsentHeadless(entry, {
        ...deps,
        publish: async () => {
          throw new TypeError("fetch failed");
        },
      }),
    );
    await runner.pass(G).run();
    const e = outbox.get(G, 220);
    expect(e?.parts).toBeUndefined();
    expect(e?.attachments.map((a) => a.blobId[0])).toEqual([1, 44]);
    expect(e).toMatchObject({ state: "waiting", autoSends: 1 });
    expect(await unsentFileKeysFor(ACCOUNT)).toEqual([]);
  });
});

describe("the wipe", () => {
  it("clears the outbox fields and the prepared files of the account", async () => {
    const { wipeAccountLocalData } = await import("../auth/wipe-account");
    const outbox = newOutbox();
    await outbox.put(textEntry(100, "gone"));
    await outbox.put(voiceEntry(101), { bytes: new Uint8Array([5]) });
    await outbox.flushed();
    expect(await unsentFileKeysFor(ACCOUNT)).toHaveLength(1);
    expect((await loadMessageLog(ACCOUNT, cipher))?.outbox).toHaveLength(2);

    const report = await wipeAccountLocalData(ACCOUNT);
    expect(report.failed).toEqual([]);
    expect(await loadMessageLog(ACCOUNT, cipher)).toBeNull();
    expect(await unsentFileKeysFor(ACCOUNT)).toEqual([]);
  });
});
