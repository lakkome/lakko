// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Pictures in a call's chat (`docs/ux/calls.md` section 4.1.4.1): the model's
 * picture states, and the pure helpers of `call-chat-pictures.ts`.
 */
import { describe, expect, it } from "vitest";

import {
  CallChatChannel,
  type CallChatMessage,
  type CallChatPictureIo,
  type CallChatPictureRef,
  type CallChatSigner,
} from "./call-chat";
import { firstPicture, looksLikePicture, pictureBox } from "./call-chat-pictures";

const enc = new TextEncoder();
const dec = new TextDecoder();

/** The wire of a picture: `seq|picture|<blob byte>`; of a text `seq|text|<t>`. */
function signer(): CallChatSigner {
  return {
    composeCallChat: (_self, seq, text) => enc.encode(`${seq}|text|${text}`),
    composeCallChatPicture: (_self, seq, p) => enc.encode(`${seq}|picture|${p.blobId[0]}`),
    verifyCallChat: (_from, _key, bytes) => {
      const [seq, kind, rest] = dec.decode(bytes).split("|");
      if (kind === "picture") {
        return { seq: BigInt(seq ?? "0"), text: "", picture: ref(Number(rest)) };
      }
      return { seq: BigInt(seq ?? "0"), text: rest ?? "", picture: null };
    },
  };
}

function ref(byte: number): CallChatPictureRef {
  return {
    blobId: new Uint8Array(32).fill(byte),
    key: new Uint8Array(32).fill(9),
    nonce: new Uint8Array(12).fill(8),
    size: 3,
    width: 640,
    height: 480,
  };
}

interface Harness {
  ch: CallChatChannel;
  sent: string[];
  list(): readonly CallChatMessage[];
  uploads: number;
  revoked: string[];
  deleted: string[][];
  /** Settles the next upload: a blob byte, or `null` for a failure. */
  finishUpload(byte: number | null): Promise<void>;
  /** Settles the next download with bytes or `null`. */
  finishDownload(ok: boolean): Promise<void>;
}

function setup(): Harness {
  const sent: string[] = [];
  let messages: readonly CallChatMessage[] = [];
  const uploadQueue: ((r: { ref: CallChatPictureRef; blobIdHex: string } | null) => void)[] = [];
  const downloadQueue: ((r: Uint8Array | null) => void)[] = [];
  let urlSeq = 0;
  const h = {
    uploads: 0,
    revoked: [] as string[],
    deleted: [] as string[][],
  };
  const io: CallChatPictureIo = {
    upload: () => {
      h.uploads += 1;
      return new Promise((resolve) => uploadQueue.push(resolve));
    },
    download: () => new Promise((resolve) => downloadQueue.push(resolve)),
    makeUrl: () => `blob:${++urlSeq}`,
    revokeUrl: (url) => h.revoked.push(url),
    deleteUploaded: (ids) => h.deleted.push([...ids]),
  };
  const ch = new CallChatChannel({
    signer: signer(),
    sendSealed: (p) => {
      sent.push(dec.decode(p));
      return true;
    },
    selfPeerId: () => "me",
    now: () => 5,
    onChange: (m) => {
      messages = m;
    },
    onIncoming: () => {},
    pictures: io,
  });
  const flush = async (): Promise<void> => {
    for (let i = 0; i < 5; i++) await Promise.resolve();
  };
  return Object.assign(h, {
    ch,
    sent,
    list: () => messages,
    async finishUpload(byte: number | null) {
      const next = uploadQueue.shift();
      next?.(byte === null ? null : { ref: ref(byte), blobIdHex: `blob${byte}` });
      await flush();
    },
    async finishDownload(ok: boolean) {
      const next = downloadQueue.shift();
      next?.(ok ? new Uint8Array([1, 2, 3]) : null);
      await flush();
    },
  });
}

describe("CallChatChannel — pictures", () => {
  it("shows an own picture at once, sends it once its file uploaded, and keeps the text numbering", async () => {
    const t = setup();
    t.ch.sendPicture(new Uint8Array([7]), 640, 480);
    expect(t.list()[0]).toMatchObject({ own: true, delivery: "sending", text: "" });
    expect(t.list()[0]?.picture).toMatchObject({ status: "ready", width: 640, height: 480 });
    expect(t.sent).toEqual([]);
    // A text written while it uploads waits for it.
    t.ch.send("and this");
    expect(t.list()[1]).toMatchObject({ text: "and this", delivery: "sending" });
    expect(t.sent).toEqual([]);
    await t.finishUpload(4);
    expect(t.sent).toEqual(["1|picture|4", "2|text|and this"]);
    expect(t.list().map((m) => m.delivery)).toEqual(["sent", "sent"]);
  });

  it("sends the waiting text even when its picture failed", async () => {
    const t = setup();
    t.ch.sendPicture(new Uint8Array([7]), 10, 10);
    t.ch.send("caption");
    await t.finishUpload(null);
    expect(t.sent).toEqual(["2|text|caption"]);
    expect(t.list().map((m) => m.delivery)).toEqual(["not-sent", "sent"]);
  });

  it("marks a failed upload not sent, and Send again uploads it again", async () => {
    const t = setup();
    const m = t.ch.sendPicture(new Uint8Array([7]), 10, 10);
    await t.finishUpload(null);
    expect(t.list()[0]?.delivery).toBe("not-sent");
    expect(t.ch.sendAgain(m?.id ?? "")).toBe(true);
    expect(t.list()[0]?.delivery).toBe("sending");
    expect(t.uploads).toBe(2);
    await t.finishUpload(5);
    expect(t.sent).toEqual(["1|picture|5"]);
    expect(t.list()[0]?.delivery).toBe("sent");
  });

  it("downloads a picture from someone else, then shows it or says it is not available", async () => {
    const t = setup();
    t.ch.onPeerIdentified("anna", new Uint8Array([1]));
    t.ch.onReceived("anna", enc.encode("1|picture|3"));
    expect(t.list()[0]?.picture).toMatchObject({ status: "loading", url: null, width: 640 });
    await t.finishDownload(true);
    expect(t.list()[0]?.picture?.status).toBe("ready");
    expect(t.list()[0]?.picture?.url).not.toBeNull();
    t.ch.onReceived("anna", enc.encode("2|picture|4"));
    await t.finishDownload(false);
    expect(t.list()[1]?.picture).toMatchObject({ status: "unavailable", url: null });
    // A frame re-sent after a reconnect does not show twice.
    t.ch.onReceived("anna", enc.encode("1|picture|3"));
    expect(t.list()).toHaveLength(2);
  });

  it("frees every picture and deletes this device's uploads when the call ends", async () => {
    const t = setup();
    t.ch.sendPicture(new Uint8Array([7]), 10, 10);
    await t.finishUpload(6);
    t.ch.onPeerIdentified("anna", new Uint8Array([1]));
    t.ch.onReceived("anna", enc.encode("1|picture|3"));
    await t.finishDownload(true);
    t.ch.clear();
    expect(t.deleted).toEqual([["blob6"]]);
    expect(t.revoked.sort()).toEqual(["blob:1", "blob:2"]);
    expect(t.list()).toEqual([]);
  });

  it("does nothing with work that finishes after the call ended", async () => {
    const t = setup();
    t.ch.sendPicture(new Uint8Array([7]), 10, 10);
    t.ch.clear();
    await t.finishUpload(6);
    expect(t.sent).toEqual([]);
    expect(t.deleted).toEqual([]);
  });
});

describe("picture helpers", () => {
  it("takes the first picture and never an SVG", () => {
    const pdf = new File([new Uint8Array(1)], "a.pdf", { type: "application/pdf" });
    const png = new File([new Uint8Array(1)], "b.png", { type: "image/png" });
    const svg = new File([new Uint8Array(1)], "c.svg", { type: "image/svg+xml" });
    expect(firstPicture([pdf, png])).toBe(png);
    expect(firstPicture([pdf, svg])).toBeNull();
    expect(looksLikePicture("image/heic")).toBe(true);
    expect(looksLikePicture("text/plain")).toBe(false);
  });

  it("fits a picture into 240 x 240 keeping its shape, each side at least 80", () => {
    expect(pictureBox(2560, 1440)).toEqual({ width: 240, height: 135 });
    expect(pictureBox(1000, 4000)).toEqual({ width: 80, height: 240 });
    expect(pictureBox(100, 50)).toEqual({ width: 100, height: 80 });
    expect(pictureBox(4000, 100)).toEqual({ width: 240, height: 80 });
    expect(pictureBox(0, 0)).toEqual({ width: 240, height: 240 });
  });
});
