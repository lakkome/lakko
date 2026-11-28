// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Walk of 2026-11-28 (UX 16.11, ADR-0101 sections 10 and 11): one person,
 * one contact row after a SEALED reset (`rowsAfterSealedReset`); two rows
 * after an UNSEALED reset that the person accepted, told apart by the old
 * row's second line (`contactRowSubLine`) and its card
 * (`cardMembershipEnded` over the shared `personInfoSections` rule).
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { personInfoSections } from "../chat/conversation-info";
import type { UiRules } from "../chat/ui-rules";
import { accountIdB64OfHex, rowsAfterSealedReset, type AcceptedContact } from "./local-store";
import { cardMembershipEnded, contactRowSubLine, deleteChatOffered } from "./started-over";

const ANNA_HEX = "0123456789abcdef0123456789abcdef";
const ANNA = accountIdB64OfHex(ANNA_HEX) as string;

function row(key: string, over: Partial<AcceptedContact> = {}): AcceptedContact {
  return { signaturePubkeyB64: key, displayName: "", acceptedAtMs: 1, accountIdHex: ANNA_HEX, ...over };
}

describe("rowsAfterSealedReset: one person, one row", () => {
  const reset = { accountIdB64: ANNA, newKeyB64: "k2", retiredKeysB64: ["k1"], leftBehindKeysB64: [] };

  it("re-keys the person's row and keeps its rename, chat and an alias of the old key", () => {
    const out = rowsAfterSealedReset([row("k1", { displayName: "Anna", groupIdB64: "chat" })], reset);
    expect(out).toEqual([
      row("k2", { displayName: "Anna", groupIdB64: "chat", previousSignaturePubkeysB64: ["k1"] }),
    ]);
  });

  it("folds a second row already under the new key into the person's row", () => {
    const out = rowsAfterSealedReset(
      [row("k1", { displayName: "Anna", groupIdB64: "chat" }), row("k2", { groupIdB64: "new-chat" })],
      reset,
    );
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({ signaturePubkeyB64: "k2", displayName: "Anna", groupIdB64: "chat" });
  });

  it("folds a second row under another retired key (the duplicate seen on the walk)", () => {
    const out = rowsAfterSealedReset(
      [row("k0", { displayName: "Anna" }), row("k1")],
      { ...reset, retiredKeysB64: ["k0", "k1"] },
    );
    expect(out.map((r) => r.signaturePubkeyB64)).toEqual(["k2"]);
    expect(out[0]?.previousSignaturePubkeysB64).toEqual(["k0", "k1"]);
    expect(out[0]?.displayName).toBe("Anna");
  });

  it("never touches the row of an account that started over earlier, even listed first", () => {
    const startedOver = row("k-old", { displayName: "Anna (old)", groupIdB64: "old-chat" });
    const out = rowsAfterSealedReset([startedOver, row("k1", { groupIdB64: "chat" })], {
      ...reset,
      leftBehindKeysB64: ["k-old"],
    });
    expect(out).toEqual([startedOver, row("k2", { groupIdB64: "chat", previousSignaturePubkeysB64: ["k1"] })]);
  });

  it("keeps a row that names the account by id alone while a row by key exists", () => {
    const claimed = row("someone-else");
    const out = rowsAfterSealedReset([claimed, row("k1")], reset);
    expect(out).toEqual([claimed, row("k2", { previousSignaturePubkeysB64: ["k1"] })]);
  });

  it("falls back to the row named by id when no row carries a key of the person", () => {
    const out = rowsAfterSealedReset([row("unknown")], reset);
    expect(out.map((r) => r.signaturePubkeyB64)).toEqual(["k2"]);
  });
});

describe("the started-over row in Contacts", () => {
  it("reads 'Started over with a new account' in place of the @username", () => {
    expect(contactRowSubLine("@anna", true)).toBe("Started over with a new account");
    expect(contactRowSubLine(null, true)).toBe("Started over with a new account");
    expect(contactRowSubLine("@anna", false)).toBe("@anna");
    expect(contactRowSubLine(null, false)).toBeNull();
  });
});

describe("the started-over card (shared rule)", () => {
  let rules: UiRules;
  beforeAll(async () => {
    const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
    await initWasm({ module_or_path: readFileSync(path) });
    rules = wasm;
  });

  it("offers no call and keeps Message and the safety number, like an ended one-to-one", () => {
    const ended = cardMembershipEnded({ chatEnded: false, keyLeftBehind: true });
    expect(ended).toBe(true);
    const sections = personInfoSections(rules, { membershipEnded: ended, isContact: true });
    expect(sections).not.toContain("call");
    expect(sections).toContain("message");
    expect(sections).toContain("safety-number");
    expect(sections).toContain("delete-chat");
    // An ordinary contact still has its call.
    const open = personInfoSections(rules, {
      membershipEnded: cardMembershipEnded({ chatEnded: false, keyLeftBehind: false }),
      isContact: true,
    });
    expect(open).toContain("call");
  });
});

describe("Delete chat on the card (UX 16.11, walk of 2026-11-28)", () => {
  it("is offered for an ended one-to-one and for the closed one of a contact who started over", () => {
    expect(deleteChatOffered({ holdsOneToOne: true, chatEnded: true, peerKeyLeftBehind: false })).toBe(true);
    expect(deleteChatOffered({ holdsOneToOne: true, chatEnded: false, peerKeyLeftBehind: true })).toBe(true);
    expect(deleteChatOffered({ holdsOneToOne: true, chatEnded: true, peerKeyLeftBehind: true })).toBe(true);
  });
  it("is not offered for a live one-to-one or when no one-to-one is held", () => {
    expect(deleteChatOffered({ holdsOneToOne: true, chatEnded: false, peerKeyLeftBehind: false })).toBe(false);
    expect(deleteChatOffered({ holdsOneToOne: false, chatEnded: true, peerKeyLeftBehind: true })).toBe(false);
  });
});
