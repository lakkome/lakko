// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-26 cross-stack SFrame interop — the WEB leg.
 *
 * Pins the SAME golden vector the Rust core
 * (`crates/lakko-calls/src/sframe.rs::golden_vector_*`) and the native FFI
 * (`apps/ios/LakkoTests/SFrameGoldenTests.swift`,
 * `apps/android/.../calls/SframeGoldenTest.kt`) pin: sealing
 * `(seed=[0x42;32], leaf=3, stream_index=0, plaintext="lakko sframe golden
 * vector v1")` on a fresh `ParticipantKey` (counter 0) MUST produce these EXACT
 * bytes via the wasm `sealFramed`, and the wasm `openFramed` MUST recover the
 * plaintext from them. The stream index `0` composes the SFrame KID
 * `(leaf << 8) | stream_index = (3 << 8) | 0 = 0x0300`, which is folded into the
 * per-participant key derivation and stamped into the wire header (ADR-0035), so
 * this vector is the canonical stream-0 (audio) wire — a stream-1 (video) seal
 * of the same leaf derives a different key and different bytes. Because all four
 * stacks pin the identical wire, a frame sealed on any one opens on any other
 * (the cross-runtime half of G-26) — proven by construction, no live SFU needed
 * (the SFU forwards the wire verbatim, which the live str0m e2e already shows).
 * Any drift in the web SFrame stack fails here.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { CallKey } from "../../wasm/lakko_ffi_wasm";

const GOLDEN_SEED = new Uint8Array(32).fill(0x42);
const GOLDEN_LEAF = 3;
/** ADR-0035 stream index for the canonical vector: 0 = audio. With leaf 3 this
 *  composes the SFrame KID `(3 << 8) | 0 = 0x0300`, matching the core golden +
 *  native C ABI. */
const GOLDEN_STREAM_AUDIO = 0;
const GOLDEN_PLAINTEXT = "lakko sframe golden vector v1";
const GOLDEN_WIRE_HEX =
  "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a";

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("SFrame cross-stack golden (G-26)", () => {
  it("seals to the canonical cross-stack wire (counter 0)", () => {
    const pk = new CallKey(GOLDEN_SEED).participantKey(
      GOLDEN_LEAF,
      GOLDEN_STREAM_AUDIO,
    );
    const wire = pk.sealFramed(new TextEncoder().encode(GOLDEN_PLAINTEXT));
    expect(bytesToHex(wire)).toBe(GOLDEN_WIRE_HEX);
  });

  it("opens the canonical wire (sealed on any stack) back to the plaintext", () => {
    const pk = new CallKey(GOLDEN_SEED).participantKey(
      GOLDEN_LEAF,
      GOLDEN_STREAM_AUDIO,
    );
    const opened = pk.openFramed(hexToBytes(GOLDEN_WIRE_HEX));
    expect(new TextDecoder().decode(opened)).toBe(GOLDEN_PLAINTEXT);
  });
});
