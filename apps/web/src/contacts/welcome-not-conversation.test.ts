// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Walk of 2026-11-14: a channel PublisherGroup Welcome read from the shared
 * account mailbox reached the conversation intake, the core refused it as not
 * a conversation (C-86, typed `noRoleTable`), and the device showed the
 * join-failed banner. Such a refusal is not a failed join: the intake reads it
 * as `not-ours`. Driven through the real core, with the error wrapped exactly
 * as `contacts/client.ts` wraps it.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { refusalReasonOf } from "../chat/membership-refusal";
import { ContactsError } from "./client";
import { isWelcomeForAnotherLeaf, isWelcomeNotAConversation } from "./MailboxContext";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

function deviceOf(account: wasm.Identity, fill: number): wasm.Identity {
  return wasm.Identity.fromSeedBound(new Uint8Array(64).fill(fill), account.ed25519Seed, 1_700_000_000n);
}

describe("a Welcome the core refuses as not a conversation", () => {
  it("is not a failed join: a PublisherGroup Welcome opened as a conversation reads as not-ours", () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x51));
    const first = deviceOf(owner, 0x52);
    const second = deviceOf(owner, 0x53);
    const g = new wasm.PublisherGroup(first, owner.ed25519Seed, new Uint8Array(16).fill(0x54));
    let refused: unknown = null;
    try {
      const c = g.stageCommit(first, [second.createKeyPackage()], new Uint8Array(), false);
      const welcome = c.welcome ?? new Uint8Array();
      c.free();
      g.mergeStagedCommit(first);
      try {
        wasm.Group.joinFromWelcome(welcome, second).free();
      } catch (err) {
        refused = err;
      }
    } finally {
      g.free();
    }
    expect(refused).not.toBeNull();
    // The typed refusal travels, so the message is not what decides.
    expect(refusalReasonOf(refused)).toBe("noRoleTable");
    const wrapped = ContactsError.wrap(refused);
    expect(isWelcomeNotAConversation(refused)).toBe(true);
    expect(isWelcomeNotAConversation(wrapped)).toBe(true);
    expect(isWelcomeForAnotherLeaf(wrapped)).toBe(false);
  });

  it("any other refusal stays a failed join", () => {
    expect(isWelcomeNotAConversation(new Error("staged welcome: InvalidSignature"))).toBe(false);
    expect(isWelcomeNotAConversation(Object.assign(new Error("x"), { refusalReason: "pendingSetFull" }))).toBe(false);
    expect(isWelcomeNotAConversation("welcome refused: the group carries no role table, so it is not a conversation")).toBe(
      true,
    );
  });
});
