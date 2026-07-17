// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * CallKeyRing wasm binding — the mid-call SFrame overlap keyer (G-18,
 * ADR-0029) exercised from JS exactly as the SFrame worker drives it: seal a
 * frame under epoch A, rekey to epoch B with a 1 s overlap window, and prove
 * a late epoch-A frame still opens through the ring's overlap fall-back while
 * a fresh epoch-B frame opens under the current key — then that pruning the
 * expired window closes it (seed-boundary forward secrecy, C-9 / C-10).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { CallKeyRing } from "../../wasm/lakko_ffi_wasm";

const SEED_A = new Uint8Array(32).fill(0x11);
const SEED_B = new Uint8Array(32).fill(0x22);
const LEAF = 3;
/** ADR-0034 media-type byte: 0 = audio. The overlap mechanism is media-type
 *  agnostic — seal and open just have to agree, so audio is used throughout. */
const AUDIO = 0;
const enc = new TextEncoder();
const dec = new TextDecoder();

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("CallKeyRing mid-call overlap (G-18, ADR-0029)", () => {
  it("opens both the old and new epoch frames during the overlap window", () => {
    const ring = new CallKeyRing(SEED_A);
    // A frame sealed under epoch A, still in flight when the rotation lands.
    const oldWire = ring.participantKey(LEAF, AUDIO).sealFramed(enc.encode("old"));

    // Rotate to epoch B with a 1 s overlap window at t=1000.
    ring.rekeyFromSeedAt(SEED_B, 1000n, 1000n);
    expect(ring.hasLivePrevious(1500n)).toBe(true);

    // A frame sealed under the new epoch B.
    const newWire = ring.participantKey(LEAF, AUDIO).sealFramed(enc.encode("new"));

    // Old opens via the retained previous key; new opens via the current key.
    expect(dec.decode(ring.openFramedWithOverlap(LEAF, AUDIO, oldWire))).toBe(
      "old",
    );
    expect(dec.decode(ring.openFramedWithOverlap(LEAF, AUDIO, newWire))).toBe(
      "new",
    );
  });

  it("prune closes the overlap window (old frame no longer opens)", () => {
    const ring = new CallKeyRing(SEED_A);
    const oldWire = ring.participantKey(LEAF, AUDIO).sealFramed(enc.encode("old"));
    ring.rekeyFromSeedAt(SEED_B, 1000n, 1000n); // expires at 2000

    // Inside the window the old frame still opens.
    expect(() => ring.openFramedWithOverlap(LEAF, AUDIO, oldWire)).not.toThrow();

    // After expiry, prune drops the previous key and the old frame is refused.
    ring.pruneExpired(2000n);
    expect(ring.hasLivePrevious(2000n)).toBe(false);
    expect(() => ring.openFramedWithOverlap(LEAF, AUDIO, oldWire)).toThrow();
  });

  it("rejects a wrong-length seed", () => {
    expect(() => new CallKeyRing(new Uint8Array(31))).toThrow();
    expect(() => new CallKeyRing(new Uint8Array(33))).toThrow();
  });
});
