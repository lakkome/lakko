// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 — the BACKGROUND notification path (`public/sw-push-decrypt.js`)
 * suppresses the scoped-discovery envelopes (visibility, reach state,
 * introductions bundle, committer delegation), READ-ONLY (C-59), exactly as
 * it does the pin. The envelopes are composed by the real core and classified
 * by the real `dispatchIncomingEnvelope`; the MLS group, the IDB reads and the
 * DS are stand-ins.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview } from "../../public/sw-push-decrypt.js";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_HEX = "ad".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xad);
const SENDER_KEY = new Uint8Array(32).fill(0x5f);

const chat = (id: number, text: string) => ({
  id,
  payload: new TextEncoder().encode(JSON.stringify({ kind: "chat", text, attachmentCount: 0 })),
  createdAt: new Date(id * 1000).toISOString(),
});
const raw = (id: number, payload: Uint8Array) => ({ id, payload, createdAt: new Date(id * 1000).toISOString() });

function wasmStub(withDispatcher = true) {
  return {
    Identity: { fromSeed: () => ({ restoreStorage: () => undefined, free: () => undefined }) },
    Group: {
      load: () => ({
        processIncomingVerifyingRoles: (_i: unknown, payload: Uint8Array) => ({
          kind: "Application",
          applicationData: payload,
          senderLeaf: 2,
        }),
        members: () => [{ leafIndex: 2, signaturePublicKey: SENDER_KEY }],
        free: () => undefined,
      }),
    },
    parseBodyEnvelope: (bytes: Uint8Array) => {
      const text = new TextDecoder().decode(bytes);
      try {
        return { ...(JSON.parse(text) as object), free: () => undefined };
      } catch {
        return { kind: "chat", text, attachmentCount: 0, free: () => undefined };
      }
    },
    ...(withDispatcher
      ? {
          dispatchIncomingEnvelope: (bytes: Uint8Array) => realWasm.dispatchIncomingEnvelope(bytes),
          IncomingEnvelopeKind: realWasm.IncomingEnvelopeKind,
        }
      : {}),
  };
}

function deps(messages: unknown[], withDispatcher = true) {
  const forbidden = { saveSnapshot: vi.fn(), saveMessageLog: vi.fn(), advanceCursor: vi.fn() };
  return {
    forbidden,
    deps: {
      readWakeBinding: async () => ({ accountId: "acct" }),
      // A sender always has a name on this device (the mirror the app writes).
      lookupSenderName: async () => "Test Sender",
      lookupRoute: async () => ({ accountId: "acct", groupIdB64: Buffer.from(GROUP_BYTES).toString("base64"), subIdHex: "dd".repeat(16) }),
      getDeviceKey: async () => ({}),
      getIdentitySeed: async () => new Uint8Array(64),
      loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
      wasm: wasmStub(withDispatcher),
      dsFetchGroup: async () => ({ messages }),
      ...forbidden,
    },
  };
}

const wake = { subHex: "dd".repeat(16), groupHex: GROUP_HEX, pointer: 42 };

function committerDelegation(): Uint8Array {
  // ADR-0078: the envelope carries the added person's mailbox binding, and
  // each delegation names that binding's address.
  const person = realWasm.Identity.fromSeed(new Uint8Array(64).fill(9));
  const added = person.signaturePublicKey;
  const binding = realWasm.signMailboxBinding(person);
  person.free();
  const mailbox = realWasm.mailboxAddress(realWasm.verifyMailboxBinding(binding, added));
  const delegate = realWasm.reachVerifyingKey(new Uint8Array(32).fill(1));
  const blob = realWasm.signDelegationBlob(
    new Uint8Array(32).fill(0x44),
    mailbox,
    delegate,
    BigInt(Date.now() + 60_000),
    new Uint8Array(16).fill(3),
  );
  return realWasm.composeCommitterDelegationEnvelope(
    new realWasm.CommitterDelegationEnvelope(added, binding, [blob]),
  );
}

const envelopes: [string, () => Uint8Array][] = [
  ["visibility", () => realWasm.composeVisibilityEnvelope(realWasm.defaultVisibilitySetting())],
  ["reach state", () => realWasm.composeReachStateEnvelope(new realWasm.ReachState([], 5n))],
  ["committer delegation", committerDelegation],
];

describe("the background path never previews a scoped-discovery envelope", () => {
  for (const [name, make] of envelopes) {
    it(`the unfiltered ${name} envelope really would render as text (the defect)`, () => {
      const parsed = wasmStub().parseBodyEnvelope(make()) as { kind: string; text: string };
      expect(parsed.kind).toBe("chat");
      expect(parsed.text.startsWith("lakko/")).toBe(true);
    });

    it(`returns null for a lone ${name} envelope`, async () => {
      const { deps: d } = deps([raw(42, make())]);
      expect(await decryptPushPreview(wake, d)).toBeNull();
    });

    it(`falls back to the older chat message when a ${name} envelope is NEWEST`, async () => {
      const { deps: d } = deps([chat(41, "real message"), raw(42, make())]);
      expect((await decryptPushPreview(wake, d))?.body).toBe("real message");
    });
  }

  it("still previews a legacy plaintext starting with `l`", async () => {
    const { deps: d } = deps([raw(42, new TextEncoder().encode("lunch at six?"))]);
    expect((await decryptPushPreview(wake, d))?.body).toBe("lunch at six?");
  });

  it("classifies without ever persisting (C-59)", async () => {
    const { deps: d, forbidden } = deps([raw(42, committerDelegation())]);
    await decryptPushPreview(wake, d);
    expect(forbidden.saveSnapshot).not.toHaveBeenCalled();
    expect(forbidden.saveMessageLog).not.toHaveBeenCalled();
    expect(forbidden.advanceCursor).not.toHaveBeenCalled();
  });
});
