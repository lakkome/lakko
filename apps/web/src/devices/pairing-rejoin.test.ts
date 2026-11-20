// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-56 (ADR-0074 "Joining after a rotation") — a device that joined the
 * account's DeviceGroup by pairing, then parked for the account's earlier
 * keys, pairs again from the parked screen. It keeps its device identity, so
 * an Add would carry a leaf key the group already holds; instead its payload
 * carries a self-removal proof and the confirming device replaces its leaf in
 * one commit. Driven over the real wasm DeviceGroup.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { WebDeviceCipher, type DeviceCipher } from "../auth/device-cipher";
import { loginHandleWith } from "../auth/login-handle";
import { getDeviceIdentitySeed } from "../auth/device-identity-store";
import { deleteKeyRotationState } from "../auth/key-rotation-store";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { parsePairingQrPayloadWith } from "./pairing";
import { decodeJournalEntry } from "./journal-envelope";
import { buildPairingCommit, SelfRemovalProofNotCoveringError } from "./pairing-commit";
import { readJoinedDeviceGroupId, recordJoinedDeviceGroupId } from "./joined-device-group-id";
import { prepareJoinFlow } from "./orchestrator";

const ACCOUNT = new Uint8Array(16).fill(0x42);
const ACCOUNT_B64 = btoa(String.fromCharCode(...ACCOUNT));
const alice = (): Uint8Array => loginHandleWith(wasm, "alice").bytes;
// No locate request: the place has answered "nothing".
const place = { current: () => null, settled: () => Promise.resolve() };

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

let deviceKey: DeviceCipher;
beforeEach(async () => {
  vi.stubGlobal("localStorage", new MemoryStorage());
  vi.stubGlobal("indexedDB", new FakeIndexedDb());
  vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 500 })));
  deviceKey = new WebDeviceCipher(
    await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]),
  );
  vi.spyOn(console, "info").mockImplementation(() => {});
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

/** The confirming device: the founder of ACCOUNT's DeviceGroup. */
function founder(): { identity: wasm.Identity; group: wasm.DeviceGroup } {
  const identity = wasm.Identity.fromSeed(new Uint8Array(32).fill(7));
  const lineage = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x5a));
  const group = wasm.DeviceGroup.createFounder(identity, ACCOUNT, lineage);
  lineage.free();
  return { identity, group };
}

function keysOf(group: wasm.DeviceGroup): string[] {
  return (group.devices() as { signaturePublicKey: Uint8Array }[]).map((m) =>
    Array.from(m.signaturePublicKey, (b) => b.toString(16).padStart(2, "0")).join(""),
  );
}

/** This device (B) joins the founder's DeviceGroup by an ordinary pairing. */
async function joinOnce(confirming: { identity: wasm.Identity; group: wasm.DeviceGroup }): Promise<void> {
  const session = await prepareJoinFlow({
    accountIdBytes: ACCOUNT,
    accountIdB64: ACCOUNT_B64,
    deviceKey,
    loginHandle: alice(),
    place,
  });
  const decoded = parsePairingQrPayloadWith(wasm, session.qrPayload, alice());
  expect(decoded.selfRemovalProof).toBeNull();
  const built = buildPairingCommit(wasm, confirming, decoded);
  expect(built.kind).toBe("add");
  await session.accept(built.welcome!.bytes);
  session.dispose();
}

/** B's stored DeviceGroup, read back from its snapshot. */
async function storedGroupOfB(): Promise<{ identity: wasm.Identity; group: wasm.DeviceGroup }> {
  const seed = await getDeviceIdentitySeed(ACCOUNT_B64, deviceKey);
  if (seed === null) throw new Error("no device seed");
  const { getDeviceGroupSnapshot } = await import("./device-group-snapshot-store");
  const snapshot = await getDeviceGroupSnapshot(ACCOUNT_B64, deviceKey);
  if (snapshot === null) throw new Error("no snapshot");
  const identity = wasm.Identity.fromSeed(seed);
  identity.restoreStorage(snapshot);
  const groupId = await readJoinedDeviceGroupId(ACCOUNT_B64, deviceKey);
  const group = wasm.loadDeviceGroupById(identity, ACCOUNT, groupId!);
  if (group === undefined) throw new Error("no group");
  return { identity, group };
}

describe("G-56: a parked member pairs again", () => {
  it("the pairing join records the joined DeviceGroup id, and a wipe of the row drops it", async () => {
    const a = founder();
    await joinOnce(a);
    expect(await readJoinedDeviceGroupId(ACCOUNT_B64, deviceKey)).toEqual(a.group.groupId());
    // Forget device, a key reset and a recall each delete the key-rotation row.
    await deleteKeyRotationState(ACCOUNT_B64);
    expect(await readJoinedDeviceGroupId(ACCOUNT_B64, deviceKey)).toBeNull();
    a.group.free();
    a.identity.free();
  });

  it("its leaf is replaced in one commit, with the proof in the journal row, and no leaf is added", async () => {
    const a = founder();
    await joinOnce(a);
    const before = keysOf(a.group);
    expect(before).toHaveLength(2);
    const epochBefore = a.group.epoch();

    const session = await prepareJoinFlow({
      accountIdBytes: ACCOUNT,
      accountIdB64: ACCOUNT_B64,
      deviceKey,
      loginHandle: alice(),
      place,
      rejoinsHeldLeaf: true,
      latestEpoch: async () => a.group.epoch(),
    });
    const decoded = parsePairingQrPayloadWith(wasm, session.qrPayload, alice());
    expect(decoded.selfRemovalProof).not.toBeNull();
    // The code route seals the same proof.
    const sealed = parsePairingQrPayloadWith(wasm, await session.pairingPayloadAtSeal(), alice());
    expect(sealed.selfRemovalProof).toEqual(decoded.selfRemovalProof);
    // The same device identity: the KeyPackage's key is the leaf already in.
    const key = wasm.keyPackageSignatureKey(decoded.keyPackage);
    expect(before).toContain(Array.from(key, (b) => b.toString(16).padStart(2, "0")).join(""));

    const built = buildPairingCommit(wasm, a, decoded);
    expect(built.kind).toBe("rejoin-replace");
    expect(decodeJournalEntry(built.row).attestation).toEqual(decoded.selfRemovalProof);
    // One commit: one epoch, the same two leaves.
    expect(a.group.epoch()).toBe(epochBefore + 1n);
    expect(keysOf(a.group).sort()).toEqual([...before].sort());

    await session.accept(built.welcome!.bytes);
    session.dispose();
    const b = await storedGroupOfB();
    expect(b.group.epoch()).toBe(a.group.epoch());
    expect(keysOf(b.group).sort()).toEqual([...before].sort());
    b.group.free();
    b.identity.free();
    a.group.free();
    a.identity.free();
  });

  it("a proof for a stale epoch is refused and nothing is committed", async () => {
    const a = founder();
    await joinOnce(a);
    const epochBefore = a.group.epoch();
    const keysBefore = keysOf(a.group);
    const session = await prepareJoinFlow({
      accountIdBytes: ACCOUNT,
      accountIdB64: ACCOUNT_B64,
      deviceKey,
      loginHandle: alice(),
      place,
      rejoinsHeldLeaf: true,
      latestEpoch: async () => epochBefore - 1n,
    });
    const decoded = parsePairingQrPayloadWith(wasm, session.qrPayload, alice());
    expect(decoded.selfRemovalProof).not.toBeNull();
    expect(() => buildPairingCommit(wasm, a, decoded)).toThrow(SelfRemovalProofNotCoveringError);
    expect(a.group.epoch()).toBe(epochBefore);
    expect(keysOf(a.group)).toEqual(keysBefore);
    session.dispose();
    a.group.free();
    a.identity.free();
  });

  it("the probe failing falls back to this device's own epoch", async () => {
    const a = founder();
    await joinOnce(a);
    const session = await prepareJoinFlow({
      accountIdBytes: ACCOUNT,
      accountIdB64: ACCOUNT_B64,
      deviceKey,
      loginHandle: alice(),
      place,
      rejoinsHeldLeaf: true,
      latestEpoch: () => Promise.reject(new Error("offline")),
    });
    const decoded = parsePairingQrPayloadWith(wasm, session.qrPayload, alice());
    expect(buildPairingCommit(wasm, a, decoded).kind).toBe("rejoin-replace");
    session.dispose();
    a.group.free();
    a.identity.free();
  });

  it("with no recorded id the parked screen pairs as an ordinary join", async () => {
    const a = founder();
    await joinOnce(a);
    await deleteKeyRotationState(ACCOUNT_B64);
    const session = await prepareJoinFlow({
      accountIdBytes: ACCOUNT,
      accountIdB64: ACCOUNT_B64,
      deviceKey,
      loginHandle: alice(),
      place,
      rejoinsHeldLeaf: true,
      latestEpoch: async () => a.group.epoch(),
    });
    expect(parsePairingQrPayloadWith(wasm, session.qrPayload, alice()).selfRemovalProof).toBeNull();
    session.dispose();
    a.group.free();
    a.identity.free();
  });

  it("a payload with no proof gives the ordinary add", async () => {
    const a = founder();
    await joinOnce(a);
    // Another device: its own storage, its own device identity.
    vi.stubGlobal("indexedDB", new FakeIndexedDb());
    const session = await prepareJoinFlow({
      accountIdBytes: ACCOUNT,
      accountIdB64: ACCOUNT_B64,
      deviceKey,
      loginHandle: alice(),
      place,
    });
    const decoded = parsePairingQrPayloadWith(wasm, session.qrPayload, alice());
    expect(decoded.selfRemovalProof).toBeNull();
    const built = buildPairingCommit(wasm, a, decoded);
    expect(built.kind).toBe("add");
    expect(decodeJournalEntry(built.row).attestation).toBeNull();
    expect(keysOf(a.group)).toHaveLength(3);
    session.dispose();
    a.group.free();
    a.identity.free();
  });

  it("records an id only of the right length, and a second record replaces the first", async () => {
    await recordJoinedDeviceGroupId(ACCOUNT_B64, deviceKey, new Uint8Array(31));
    expect(await readJoinedDeviceGroupId(ACCOUNT_B64, deviceKey)).toBeNull();
    await recordJoinedDeviceGroupId(ACCOUNT_B64, deviceKey, new Uint8Array(32).fill(1));
    await recordJoinedDeviceGroupId(ACCOUNT_B64, deviceKey, new Uint8Array(32).fill(2));
    expect(await readJoinedDeviceGroupId(ACCOUNT_B64, deviceKey)).toEqual(new Uint8Array(32).fill(2));
  });
});
