// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - the read key a public channel's record carries
 * (`group-handle.ts` `channelSubject`). A channel's read key never changes,
 * so a channel an account took back after a key reset keeps the key its
 * first owner seed derived; a record that derived it from the CURRENT
 * lineage seed would publish a key the channel does not use.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as realWasm from "../../wasm/lakko_ffi_wasm";
import { channelSubject } from "./group-handle";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const CHANNEL_ID = new Uint8Array(16).fill(0x0c);
const FACTS = { name: "News", description: "" };

function lineage(seed: number): realWasm.Identity {
  return realWasm.Identity.fromSeed(new Uint8Array(32).fill(seed));
}

describe("channelSubject: the read key of a public channel's record", () => {
  it("is the key the caller holds for the channel, when it holds one", () => {
    const first = realWasm.channelReadKey(lineage(1), CHANNEL_ID);
    // The account reset: its lineage seed is another one now.
    const afterReset = lineage(2);
    const subject = channelSubject(realWasm, afterReset, CHANNEL_ID, FACTS, first);
    expect(subject.key).toEqual(first);
    expect(subject.key).not.toEqual(realWasm.channelReadKey(afterReset, CHANNEL_ID));
  });

  it("is derived from the lineage seed when the caller holds none (a channel this identity founded)", () => {
    const owner = lineage(1);
    const derived = realWasm.channelReadKey(owner, CHANNEL_ID);
    expect(channelSubject(realWasm, owner, CHANNEL_ID, FACTS).key).toEqual(derived);
    expect(channelSubject(realWasm, owner, CHANNEL_ID, FACTS, null).key).toEqual(derived);
  });

  it("names the channel and nothing of a group", () => {
    const subject = channelSubject(realWasm, lineage(1), CHANNEL_ID, FACTS);
    expect(subject).toMatchObject({ kind: "channel", id: CHANNEL_ID, name: "News", knockVersion: 0 });
  });
});
