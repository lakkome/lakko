// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §4 / UX spec §4 — the UI decisions over the two name rules, driven
 * by the REAL core (`validateUsername`, `normalizeDisplayName`), so the copy a
 * user sees is chosen from the reason codes the core actually throws.
 *
 * ## Discrimination (verified by running the broken variants)
 *
 *   - Mapping every refusal to `unknown` (ignoring `usernameError`) fails
 *     "names the reason for each refusal" with
 *     `expected 'This username can\'t be used' to be 'At least 3 characters'`.
 *   - Treating an unloaded core as valid (`validate === null` -> valid) fails
 *     "blocks a lookup while the core is still loading" with
 *     `expected 'valid' to be 'unchecked'`.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  DISPLAY_NAME_REASON,
  USERNAME_REASON,
  checkDisplayName,
  checkUsername,
  mayLookUp,
  maySaveDisplayName,
  savedDisplayName,
  usernameReason,
} from "./rules";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const validate = (s: string): string => wasm.validateUsername(s);
const normalize = (s: string): string | undefined => wasm.normalizeDisplayName(s);

describe("username field", () => {
  it("accepts a conforming name and shows it lowercased", () => {
    expect(checkUsername("@Alice_42", validate)).toEqual({ kind: "valid", normalised: "alice_42" });
    expect(mayLookUp(checkUsername("@Alice_42", validate))).toBe(true);
  });

  it("shows nothing while the field is blank, and sends nothing", () => {
    const c = checkUsername("   ", validate);
    expect(c.kind).toBe("blank");
    expect(usernameReason(c)).toBeNull();
    expect(mayLookUp(c)).toBe(false);
  });

  it("names the reason for each refusal", () => {
    expect(usernameReason(checkUsername("ab", validate))).toBe(USERNAME_REASON.tooShort);
    expect(usernameReason(checkUsername("a".repeat(33), validate))).toBe(USERNAME_REASON.tooLong);
    expect(usernameReason(checkUsername("al-ice", validate))).toBe(USERNAME_REASON.invalidCharacters);
    // Cyrillic look-alikes are refused, not normalised into Latin.
    expect(usernameReason(checkUsername("\u0430lice", validate))).toBe(USERNAME_REASON.invalidCharacters);
    expect(usernameReason(checkUsername("@", validate))).toBe(USERNAME_REASON.empty);
  });

  it("the copy is the spec's", () => {
    expect(USERNAME_REASON.invalidCharacters).toBe("Only Latin letters, digits and _");
    expect(USERNAME_REASON.tooShort).toBe("At least 3 characters");
    expect(USERNAME_REASON.tooLong).toBe("At most 32 characters");
  });

  it("an invalid name never permits a lookup", () => {
    for (const input of ["ab", "al ice", "a".repeat(33), "bob!"]) {
      expect(mayLookUp(checkUsername(input, validate))).toBe(false);
    }
  });

  it("blocks a lookup while the core is still loading", () => {
    const c = checkUsername("alice", null);
    expect(c.kind).toBe("unchecked");
    expect(mayLookUp(c)).toBe(false);
  });

  it("fails closed on a refusal with no recognised code", () => {
    const c = checkUsername("alice", () => {
      throw new Error("something else");
    });
    expect(c).toEqual({ kind: "invalid", code: "unknown", reason: USERNAME_REASON.unknown });
  });
});

describe("display-name field", () => {
  it("trims, and blank means no name", () => {
    expect(checkDisplayName("  Alice Petrova ", normalize)).toEqual({ kind: "valid", name: "Alice Petrova" });
    const none = checkDisplayName("   ", normalize);
    expect(none.kind).toBe("none");
    expect(maySaveDisplayName(none)).toBe(true);
    expect(savedDisplayName(none)).toBeNull();
  });

  it("names the reason for each refusal", () => {
    const reasonOf = (s: string): string | null => {
      const c = checkDisplayName(s, normalize);
      return c.kind === "invalid" ? c.reason : null;
    };
    expect(reasonOf("Alice\nPetrova")).toBe(DISPLAY_NAME_REASON.lineBreak);
    // RIGHT-TO-LEFT OVERRIDE, written as an escape so the source stays clean.
    expect(reasonOf("Alice\u202Eavortep")).toBe(DISPLAY_NAME_REASON.controlOrFormatCharacter);
    expect(reasonOf("a".repeat(65))).toBe(DISPLAY_NAME_REASON.tooLong);
  });

  it("accepts emoji sequences and joiners", () => {
    // Family emoji (ZWJ sequence) and a regional-indicator flag.
    for (const name of ["Ann \u{1F468}\u200D\u{1F469}\u200D\u{1F467}", "Kai \u{1F1EB}\u{1F1EE}"]) {
      const c = checkDisplayName(name, normalize);
      expect(c.kind).toBe("valid");
      expect(maySaveDisplayName(c)).toBe(true);
    }
  });

  it("an invalid or unchecked name cannot be saved", () => {
    expect(maySaveDisplayName(checkDisplayName("a\nb", normalize))).toBe(false);
    expect(maySaveDisplayName(checkDisplayName("Alice", null))).toBe(false);
  });
});
