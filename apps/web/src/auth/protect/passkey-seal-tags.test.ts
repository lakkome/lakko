// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import init, * as wasm from "../../../wasm/lakko_ffi_wasm";
import type { OwnSealFacts } from "../seal/own-seals";
import { hexOf, passkeyRowSaysNoSeal, sealTagsOf } from "./passkey-seal-tags";

const facts = (tags: readonly string[]): OwnSealFacts => ({
  password: true,
  phrase: false,
  passkeyTagsHex: tags,
  seq: "1",
});

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

describe("which passkeys keep the chats (UX 19.1d)", () => {
  it("a passkey's tag is the core's, from its credential id", () => {
    const id = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
    const idB64 = "AQIDBAUGBwg";
    const tags = sealTagsOf(wasm, [idB64]);
    expect(tags.get(idB64)).toBe(hexOf(wasm.resetSealPasskeyTag(id)));
    expect(tags.get(idB64)).toMatch(/^[0-9a-f]{32}$/);
  });

  it("an id that does not decode has no tag", () => {
    const core = { resetSealPasskeyTag: () => new Uint8Array(16) };
    expect(sealTagsOf(core, ["***"]).size).toBe(0);
  });

  it("the row says it only when the protection is known not to list the passkey", () => {
    const tag = "ab".repeat(16);
    expect(passkeyRowSaysNoSeal(facts([]), tag)).toBe(true);
    expect(passkeyRowSaysNoSeal(facts(["cd".repeat(16)]), tag)).toBe(true);
    expect(passkeyRowSaysNoSeal(facts([tag]), tag)).toBe(false);
  });

  it("says nothing while the facts or the tag are not known", () => {
    expect(passkeyRowSaysNoSeal(null, "ab".repeat(16))).toBe(false);
    expect(passkeyRowSaysNoSeal(facts([]), undefined)).toBe(false);
  });
});
