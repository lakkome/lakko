// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A publisher request has no deadline (ADR-0045 decision 2, ADR-0087
 * section 15): the request is recorded, and the Welcome the owner's device
 * sends whenever it approves lands through the mailbox receive loop. Driven
 * through the REAL wasm core: an owner retains a bare KeyPackage and approves
 * it, the Welcome is sealed RAW into a `welcome-rendezvous` row (as web
 * `approvePorts` and Android `CoPublisher.kt` send it, seen live 2026-11-07),
 * and the joiner's mailbox dispatch surfaces it and lands the request.
 */
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm, wasmSync: () => wasm }));

const { setKeys, persisted } = vi.hoisted(() => ({
  setKeys: vi.fn(),
  persisted: vi.fn(async () => undefined),
}));
vi.mock("../state/channels-state", async (importOriginal) => {
  const real = await importOriginal<typeof import("../state/channels-state")>();
  return {
    ...real,
    channelsState: { ...real.channelsState, keysByChannel: () => new Map(), channels: () => [] },
    channelsActions: {
      ...real.channelsActions,
      addChannel: vi.fn(),
      setKeys,
      persistPublisherGroupDurable: persisted,
    },
  };
});
vi.mock("./composer", async (importOriginal) => {
  const real = await importOriginal<typeof import("./composer")>();
  return { ...real, freshPublisherKeys: vi.fn(async () => ({ joined: true })) };
});
vi.mock("./api", async (importOriginal) => {
  const real = await importOriginal<typeof import("./api")>();
  return { ...real, rememberPublisherRole: vi.fn(async () => undefined) };
});

import { landPendingPublisherWelcome } from "./channel-publishers";
import { loadPendingPublisherRequests, recordPendingPublisherRequest } from "./pending-publisher-requests";
import { dispatchMailboxRows } from "../contacts/client";
import { sealRowFor, type MailboxTarget } from "../contacts/mailbox-row";
import { base64Encode } from "../auth/account-id";
import { MemoryStorage } from "../test/fake-storage";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

beforeEach(() => {
  vi.stubGlobal("localStorage", new MemoryStorage());
  setKeys.mockClear();
  persisted.mockClear();
});

function targetOf(owner: wasm.Identity): MailboxTarget {
  const mailboxPk = wasm.verifyMailboxBinding(wasm.signMailboxBinding(owner), owner.signaturePublicKey);
  return { mailboxPk, address: wasm.mailboxAddress(mailboxPk) };
}

const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const ACCOUNT = "joiner-account";

describe("a publisher request's Welcome lands whenever it arrives", () => {
  it("a raw Welcome in a welcome-rendezvous row lands the recorded request", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x51));
    const joiner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x52));
    const channelId = new Uint8Array(16).fill(0x07);
    const pg = wasm.PublisherGroup.openOrCreate(owner, channelId);
    const joinerKey = wasm.MailboxKey.fromAccountIdentity(joiner);
    try {
      // The joiner knocks with a KeyPackage and records the request.
      const kp = joiner.createKeyPackage();
      recordPendingPublisherRequest(ACCOUNT, {
        channelIdHex: hex(channelId),
        groupIdHex: hex(pg.groupId()),
        joinerKeyB64: base64Encode(joiner.signaturePublicKey),
        name: "Pixel Pub Walk",
        description: "",
        requestedAtMs: 1,
      });
      // The owner approves, any time later.
      const req = pg.retainJoinRequest(owner, kp);
      const added = pg.approveExternalJoin(owner, req.joinerSignatureKey);
      const welcome = added.welcome;
      req.free();
      added.free();

      // Its Welcome reaches the joiner's mailbox as a welcome-rendezvous row
      // whose body is the raw Welcome.
      const row = sealRowFor(wasm, targetOf(joiner), "welcome-rendezvous", welcome);
      const out = dispatchMailboxRows(wasm, joinerKey, [{ id: 40, payload: row, createdAt: "t" }], 0, joiner.x25519SecretKey);
      const ev = out.events[0];
      if (ev?.kind !== "publisher-welcome") throw new Error("expected a publisher welcome");

      expect(await landPendingPublisherWelcome(joiner, ACCOUNT, ev.welcomeBytes)).toBe(true);
      expect(setKeys).toHaveBeenCalledWith(hex(channelId), { joined: true });
      expect(persisted).toHaveBeenCalled();
      expect(loadPendingPublisherRequests(ACCOUNT)).toEqual([]);
    } finally {
      joinerKey.free();
      pg.free();
      owner.free();
      joiner.free();
    }
  });

  it("a Welcome that is no pending request's changes nothing", async () => {
    const joiner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x53));
    try {
      // No request recorded.
      expect(await landPendingPublisherWelcome(joiner, ACCOUNT, new Uint8Array([1, 2, 3]))).toBe(false);
      recordPendingPublisherRequest(ACCOUNT, {
        channelIdHex: "07".repeat(16),
        groupIdHex: "ab".repeat(32),
        joinerKeyB64: base64Encode(joiner.signaturePublicKey),
        name: "",
        description: "",
        requestedAtMs: 1,
      });
      // Garbage, or another group's Welcome: the request stays.
      expect(await landPendingPublisherWelcome(joiner, ACCOUNT, new Uint8Array([1, 2, 3]))).toBe(false);
      expect(loadPendingPublisherRequests(ACCOUNT)).toHaveLength(1);
      expect(setKeys).not.toHaveBeenCalled();
    } finally {
      joiner.free();
    }
  });
});
