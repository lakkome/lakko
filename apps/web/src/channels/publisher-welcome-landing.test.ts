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
      updateAccessDurable: vi.fn(async () => undefined),
    },
  };
});
vi.mock("./composer", async (importOriginal) => {
  const real = await importOriginal<typeof import("./composer")>();
  return { ...real, freshPublisherKeys: vi.fn(async () => ({ joined: true })) };
});
vi.mock("./api", async (importOriginal) => {
  const real = await importOriginal<typeof import("./api")>();
  return {
    ...real,
    rememberPublisherRole: vi.fn(async () => undefined),
    fetchEpoch: vi.fn(async () => ({ epoch: 1 })),
  };
});

import { landPendingPublisherWelcome } from "./channel-publishers";
import { loadPendingPublisherRequests, recordPendingPublisherRequest } from "./pending-publisher-requests";
import { dispatchMailboxRows } from "../contacts/client";
import { sealRowFor, type MailboxTarget } from "../contacts/mailbox-row";
import { base64Encode } from "../auth/account-id";
import { MemoryStorage } from "../test/fake-storage";
import { installPublisherDeviceSource } from "./publisher-device";

/** ADR-0090: a device identity bound to `account`. */
function deviceOf(account: wasm.Identity, fill: number): wasm.Identity {
  return wasm.Identity.fromSeedBound(new Uint8Array(64).fill(fill), account.ed25519Seed, 1_700_000_000n);
}

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
    // ADR-0090: both leaves are devices bound to their accounts.
    const ownerDevice = deviceOf(owner, 0x61);
    const joinerDevice = deviceOf(joiner, 0x62);
    const uninstall = installPublisherDeviceSource(() => joinerDevice);
    const channelId = new Uint8Array(16).fill(0x07);
    const pg = new wasm.PublisherGroup(ownerDevice, owner.ed25519Seed, channelId);
    const joinerKey = wasm.MailboxKey.fromAccountIdentity(joiner);
    try {
      // The joiner's DEVICE knocks with a KeyPackage and records the request,
      // with the owner's account key the link named.
      const kp = joinerDevice.createKeyPackage();
      recordPendingPublisherRequest(ACCOUNT, {
        channelIdHex: hex(channelId),
        groupIdHex: hex(pg.groupId()),
        joinerKeyB64: base64Encode(joinerDevice.signaturePublicKey),
        ownerKeyB64: base64Encode(owner.signaturePublicKey),
        name: "Pixel Pub Walk",
        description: "",
        requestedAtMs: 1,
      });
      // The owner's device approves, any time later; the journal took it.
      const req = pg.retainJoinRequest(ownerDevice, kp);
      const added = pg.stageApproveExternalJoin(ownerDevice, req.joinerSignatureKey);
      const welcome = added.welcome;
      req.free();
      added.free();
      expect(pg.mergeStagedCommit(ownerDevice)).toBe(true);

      // Its Welcome reaches the joiner's mailbox as a welcome-rendezvous row
      // whose body is the raw Welcome.
      const row = sealRowFor(wasm, targetOf(joiner), "welcome-rendezvous", welcome);
      const out = dispatchMailboxRows(wasm, joinerKey, [{ id: 40, payload: row, createdAt: "t" }], 0, joiner.x25519SecretKey);
      const ev = out.events[0];
      if (ev?.kind !== "publisher-welcome") throw new Error("expected a publisher welcome");

      expect(await landPendingPublisherWelcome(joiner, ACCOUNT, ev.welcomeBytes)).toBe(true);
      expect(setKeys).toHaveBeenCalledWith(hex(channelId), expect.objectContaining({ joined: true }));
      expect(persisted).toHaveBeenCalled();
      expect(loadPendingPublisherRequests(ACCOUNT)).toEqual([]);
    } finally {
      uninstall();
      joinerKey.free();
      pg.free();
      owner.free();
      joiner.free();
    }
  });

  it("a Welcome sent by a device of another account than the link's owner does not land", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x54));
    const stranger = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x55));
    const joiner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x56));
    const strangerDevice = deviceOf(stranger, 0x64);
    const joinerDevice = deviceOf(joiner, 0x65);
    const uninstall = installPublisherDeviceSource(() => joinerDevice);
    const channelId = new Uint8Array(16).fill(0x08);
    // The stranger founds a group of its own; the request names the owner.
    const pg = new wasm.PublisherGroup(strangerDevice, stranger.ed25519Seed, channelId);
    try {
      const kp = joinerDevice.createKeyPackage();
      recordPendingPublisherRequest(ACCOUNT, {
        channelIdHex: hex(channelId),
        groupIdHex: hex(pg.groupId()),
        joinerKeyB64: base64Encode(joinerDevice.signaturePublicKey),
        ownerKeyB64: base64Encode(owner.signaturePublicKey),
        name: "",
        description: "",
        requestedAtMs: 1,
      });
      const req = pg.retainJoinRequest(strangerDevice, kp);
      const added = pg.stageApproveExternalJoin(strangerDevice, req.joinerSignatureKey);
      const welcome = added.welcome;
      req.free();
      added.free();
      expect(await landPendingPublisherWelcome(joiner, ACCOUNT, welcome)).toBe(false);
      expect(setKeys).not.toHaveBeenCalled();
      expect(loadPendingPublisherRequests(ACCOUNT)).toHaveLength(1);
    } finally {
      uninstall();
      pg.free();
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
