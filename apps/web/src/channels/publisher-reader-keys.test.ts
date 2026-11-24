// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX 15.1, live walk 2026-11-24 ("Night walk": web owner, iOS publisher):
 * each publishing account's reader key, announced on the PublisherGroup, is
 * left out of the other publishers' subscriber list and count. The envelope
 * goes through the real wasm core.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { currentPublisherReaderKeys, publisherReaderKeyVerdict, readerKeySendDue } from "./publisher-reader-keys";

beforeAll(async () => {
  const p = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(p) });
});

const channel = "0c".repeat(16);
const owner = "A".repeat(43) + "=";
const publisher = "B".repeat(43) + "=";
const stranger = "C".repeat(43) + "=";
const reader = "d7".repeat(32);

describe("a publisher's reader key on the PublisherGroup", () => {
  it("is a fixed-length envelope the dispatcher never renders", () => {
    const env = wasm.composePublisherReaderKeyEnvelope(new Uint8Array(16).fill(0x0c), new Uint8Array(32).fill(0xd7));
    expect(env.length).toBe(wasm.publisherReaderKeyEnvelopeLen());
    expect(env[0]).toBe(0x6c);
    const decoded = wasm.decodePublisherReaderKeyEnvelope(env);
    expect([...decoded.readerKey]).toEqual(new Array(32).fill(0xd7));
    decoded.free();
    const d = wasm.dispatchIncomingEnvelope(env);
    expect(d.kind).toBe(wasm.IncomingEnvelopeKind.PublisherReaderKey);
    d.free();
    expect(() => wasm.composePublisherReaderKeyEnvelope(new Uint8Array(16), new Uint8Array(31))).toThrow();
  });

  it("is kept only for this channel and from a current publisher", () => {
    const base = { channelIdHex: channel, envelopeChannelHex: channel, readerKeyHex: reader, publisherAccountsB64: [owner, publisher] };
    expect(publisherReaderKeyVerdict({ ...base, senderB64: publisher })).toEqual({ accountKeyB64: publisher, readerKeyHex: reader });
    expect(publisherReaderKeyVerdict({ ...base, senderB64: stranger })).toBeNull();
    expect(publisherReaderKeyVerdict({ ...base, senderB64: undefined })).toBeNull();
    expect(publisherReaderKeyVerdict({ ...base, senderB64: publisher, envelopeChannelHex: "0d".repeat(16) })).toBeNull();
  });

  it("is left out only while its account still publishes", () => {
    const stored = { [owner]: "AA".repeat(32), [publisher]: "bb".repeat(32) };
    expect(currentPublisherReaderKeys(stored, [owner, publisher])).toEqual(["aa".repeat(32), "bb".repeat(32)]);
    expect(currentPublisherReaderKeys(stored, [owner])).toEqual(["aa".repeat(32)]);
    // Before the publisher list was read, every kept key counts.
    expect(currentPublisherReaderKeys(stored, [])).toHaveLength(2);
    expect(currentPublisherReaderKeys(undefined, [owner])).toEqual([]);
  });

  it("is sent once registered, and again when the group grows", () => {
    expect(readerKeySendDue({ registeredHex: null, leaves: 2, lastSent: undefined })).toBeNull();
    const first = readerKeySendDue({ registeredHex: reader, leaves: 2, lastSent: undefined });
    expect(first).not.toBeNull();
    expect(readerKeySendDue({ registeredHex: reader, leaves: 2, lastSent: first ?? undefined })).toBeNull();
    expect(readerKeySendDue({ registeredHex: reader, leaves: 3, lastSent: first ?? undefined })).not.toBeNull();
  });
});
