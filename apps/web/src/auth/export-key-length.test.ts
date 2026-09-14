// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import {
  assertExportKeyLength,
  EXPORT_KEY_ENVELOPE_LEN,
  OPAQUE_EXPORT_KEY_LEN,
} from "./export-key-length";

const here = dirname(fileURLToPath(import.meta.url));
const CIPHER_SUITE_RS = resolve(
  here,
  "../../../../crates/lakko-auth-client/src/cipher_suite.rs",
);

describe("OPAQUE export-key length", () => {
  it("mirrors EXPORT_KEY_LEN in the Rust OPAQUE client", () => {
    // The value the wasm glue hands us is `out.export_key.as_ref().to_vec()`,
    // sized by this suite constant. Reading the source keeps the TS mirror from
    // drifting silently the next time the suite changes.
    const src = readFileSync(CIPHER_SUITE_RS, "utf8");
    const m = /pub const EXPORT_KEY_LEN: usize = (\d+);/.exec(src);
    expect(m).not.toBeNull();
    expect(Number(m?.[1])).toBe(OPAQUE_EXPORT_KEY_LEN);
  });

  it("accepts a key of the real suite length", () => {
    expect(() =>
      assertExportKeyLength(new Uint8Array(64), "export key"),
    ).not.toThrow();
  });

  it("refuses the 32-byte length the store used to demand", () => {
    // The old check accepted exactly this and refused every real key.
    expect(() => assertExportKeyLength(new Uint8Array(32), "export key")).toThrow(
      /export key length 32 != 64/,
    );
  });

  it("refuses any other length, including empty", () => {
    for (const n of [0, 1, 63, 65, 128]) {
      expect(() =>
        assertExportKeyLength(new Uint8Array(n), "decrypted export key"),
      ).toThrow(/decrypted export key length/);
    }
  });

  it("sizes a well-formed AES-GCM envelope as IV + key + tag", () => {
    expect(EXPORT_KEY_ENVELOPE_LEN).toBe(12 + 64 + 16);
  });
});
