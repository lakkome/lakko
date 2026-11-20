// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 1 — the receive side of the account-deleted statement,
 * against the REAL core (`composeAccountDeletedEnvelope`,
 * `verifyAccountDeletedEnvelope`): accepted only from the key that signed it,
 * in the conversation it names.
 */
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  acceptAccountDeleted,
  deliverAccountDeleted,
  installAccountDeletedSink,
  isAccountDeletedBytes,
  type ArrivedAccountDeleted,
} from "./account-deleted-envelope";

let signer: wasm.Identity;
let other: wasm.Identity;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  signer = new wasm.Identity("leaving");
  other = new wasm.Identity("someone else");
});

afterEach(() => {
  installAccountDeletedSink(null);
});

const GROUP = new Uint8Array(32).fill(0x42);
const OTHER_GROUP = new Uint8Array(32).fill(0x43);

function statement(groupId: Uint8Array = GROUP): Uint8Array {
  return wasm.composeAccountDeletedEnvelope(signer.ed25519Seed, groupId);
}

describe("recognising a statement", () => {
  it("is the fixed length and is recognised; a chat message is not", () => {
    expect(statement().length).toBe(wasm.accountDeletedEnvelopeLen());
    expect(isAccountDeletedBytes(wasm, statement())).toBe(true);
    expect(isAccountDeletedBytes(wasm, new TextEncoder().encode('{"v":1,"text":"hi"}'))).toBe(false);
    expect(isAccountDeletedBytes(wasm, new Uint8Array(0))).toBe(false);
  });
});

describe("accepting a statement", () => {
  it("is accepted from the key that signed it, in the conversation it names", () => {
    expect(acceptAccountDeleted(wasm, statement(), GROUP, signer.signaturePublicKey)).toBe(true);
  });

  it("is dropped from any other sender", () => {
    expect(acceptAccountDeleted(wasm, statement(), GROUP, other.signaturePublicKey)).toBe(false);
  });

  it("is dropped in another conversation", () => {
    expect(acceptAccountDeleted(wasm, statement(), OTHER_GROUP, signer.signaturePublicKey)).toBe(false);
  });

  it("is dropped with a bad signature", () => {
    const bad = statement();
    bad[bad.length - 1] = (bad[bad.length - 1] ?? 0) ^ 0x01;
    expect(acceptAccountDeleted(wasm, bad, GROUP, signer.signaturePublicKey)).toBe(false);
  });

  it("is dropped when the sender could not be resolved", () => {
    expect(acceptAccountDeleted(wasm, statement(), GROUP, undefined)).toBe(false);
  });
});

describe("handing a statement on", () => {
  it("keeps one that arrived before the runtime and hands it over on install", async () => {
    const got: ArrivedAccountDeleted[] = [];
    const arrived: ArrivedAccountDeleted = {
      groupId: GROUP,
      groupIdB64: "g",
      plaintext: statement(),
      senderAccountKey: signer.signaturePublicKey,
    };
    await deliverAccountDeleted(arrived);
    installAccountDeletedSink(async (a) => {
      got.push(a);
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(got).toHaveLength(1);
    await deliverAccountDeleted(arrived);
    expect(got).toHaveLength(2);
  });
});
