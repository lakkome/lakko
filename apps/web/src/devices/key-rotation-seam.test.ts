// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.2: "the stolen device is out first" (the C-61 pattern).
 *
 * The `key-rotation` envelope carries the account's NEW entropy, so it must
 * be sealed at a DeviceGroup epoch the removed device never held. No crate
 * and no server can check that (T5 item 5); it is the sender's obligation,
 * and on this client it is ONE named seam, `sealKeyRotationAfterRemoval`.
 *
 * Through the real wasm core: a real DeviceGroup of two devices, a real
 * Remove commit, the real `ServiceEnvelope` application message, and the
 * epoch read off the EMITTED message's own framing (`epochOfMessage`). The
 * inverted-order companion runs the swap: it shows that the same envelope
 * sealed before the removal carries the pre-removal epoch (so the first
 * test's assertion discriminates) and that the seam refuses to seal it.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, {
  DeviceGroup,
  Identity,
  RecoveryMnemonic,
  composeKeyRotationEnvelope,
  dsRootEndorsementLen,
  epochOfMessage,
  keyRotationEnvelopeLen,
  keyRotationPendingExpired,
  keyRotationPendingMaxAgeMs,
  signDsRootEndorsement,
  signSessionUpgradeStatement,
  signSupersessionProof,
  verifyKeyRotationEnvelope,
} from "../../wasm/lakko_ffi_wasm";
import { rootEndorsementFor } from "./lifecycle";
import { KeyRotationOrderError, sealKeyRotationAfterRemoval, type KeyRotationSealPorts } from "./key-rotation";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const ACCOUNT = new Uint8Array(16).fill(3);
const NOW_SECS = 1_800_000_000;

function accountSeed(fill: number): Uint8Array {
  return new Uint8Array(64).fill(fill);
}

/** A rotation's material: the old account, the new phrase, the proof, the
 *  envelope. */
function rotationMaterial() {
  const oldAccount = Identity.fromSeed(accountSeed(1));
  const mnemonic = RecoveryMnemonic.generate();
  const entropy = mnemonic.toEntropy();
  const newAccount = Identity.fromSeed(mnemonic.toSeed());
  mnemonic.free();
  const proof = signSupersessionProof(
    oldAccount.ed25519Seed,
    newAccount.ed25519Seed,
    BigInt(NOW_SECS),
    BigInt(NOW_SECS + 14 * 24 * 60 * 60),
  );
  const envelope = composeKeyRotationEnvelope(BigInt(NOW_SECS * 1000), entropy, proof);
  return { oldAccount, newAccount, entropy, proof, envelope };
}

/** A DeviceGroup of two devices: the one that rotates and the stolen one. */
function twoDevices() {
  const mine = Identity.fromSeed(new Uint8Array(32).fill(11));
  const stolen = Identity.fromSeed(new Uint8Array(32).fill(12));
  const group = DeviceGroup.createFounder(mine, ACCOUNT);
  group.addDeviceFromWire(mine, stolen.createKeyPackage()).free();
  const stolenKey = stolen.signaturePublicKey;
  const epoch = (): bigint => {
    const signer = group.dsAccessSigner(mine);
    try {
      return signer.epoch;
    } finally {
      signer.free();
    }
  };
  const emitted: Uint8Array[] = [];
  const ports: KeyRotationSealPorts = {
    rosterKeys: () => (group.devices() as { signaturePublicKey: Uint8Array }[]).map((m) => m.signaturePublicKey),
    epoch,
    seal: (envelope) => group.createServiceEnvelopeMessage(mine, envelope, 1n),
    publish: async (wire) => {
      emitted.push(wire);
    },
  };
  const remove = (): void => {
    const leaf = (group.devices() as { leafIndex: number; signaturePublicKey: Uint8Array }[]).find(
      (m) => m.signaturePublicKey.every((b, i) => b === stolenKey[i]),
    );
    if (leaf === undefined) throw new Error("the stolen device holds no leaf");
    group.removeDeviceAt(mine, leaf.leafIndex);
  };
  return { group, mine, stolenKey, epoch, emitted, ports, remove };
}

describe("the key-rotation envelope is sealed AFTER the removal", () => {
  it("the emitted message is of an epoch past the removal's, and the removed device is not in its roster", async () => {
    const dg = twoDevices();
    const { envelope } = rotationMaterial();
    const before = dg.epoch();
    dg.remove();
    const sealed = await sealKeyRotationAfterRemoval(
      { removedDeviceKey: dg.stolenKey, preRemovalEpoch: before, envelope },
      dg.ports,
    );
    expect(dg.emitted).toHaveLength(1);
    // The epoch of the EMITTED message, read off its own framing.
    const wireEpoch = epochOfMessage(dg.emitted[0] as Uint8Array);
    expect(wireEpoch).toBeDefined();
    expect(wireEpoch as bigint).toBeGreaterThan(before);
    expect(wireEpoch).toBe(sealed.epoch);
    expect(dg.ports.rosterKeys().some((k) => k.every((b, i) => b === dg.stolenKey[i]))).toBe(false);
  });

  it("INVERTED: sealed before the removal the same envelope carries the pre-removal epoch, and the seam refuses it", async () => {
    const dg = twoDevices();
    const { envelope } = rotationMaterial();
    const before = dg.epoch();
    // The swap, run for real: what the first test's assertion exists to
    // catch. Sealed directly (not through the seam) at the pre-removal
    // epoch, which the stolen device holds.
    const early = dg.group.createServiceEnvelopeMessage(dg.mine, envelope, 1n);
    expect(epochOfMessage(early)).toBe(before);
    expect((epochOfMessage(early) as bigint) > before).toBe(false);

    // Through the seam the swap seals and sends nothing.
    await expect(
      sealKeyRotationAfterRemoval({ removedDeviceKey: dg.stolenKey, preRemovalEpoch: before, envelope }, dg.ports),
    ).rejects.toMatchObject({ name: "KeyRotationOrderError", reason: "device-still-member" });
    await expect(
      sealKeyRotationAfterRemoval({ removedDeviceKey: dg.stolenKey, preRemovalEpoch: null, envelope }, dg.ports),
    ).rejects.toBeInstanceOf(KeyRotationOrderError);
    expect(dg.emitted).toHaveLength(0);
  });

  it("a handle that has not advanced past the pre-removal epoch is refused even with the device gone from its roster", async () => {
    const dg = twoDevices();
    const { envelope } = rotationMaterial();
    dg.remove();
    const after = dg.epoch();
    await expect(
      sealKeyRotationAfterRemoval({ removedDeviceKey: dg.stolenKey, preRemovalEpoch: after, envelope }, dg.ports),
    ).rejects.toMatchObject({ reason: "epoch-not-advanced" });
    expect(dg.emitted).toHaveLength(0);
  });

  it("a retry after the device was already removed (no epoch recorded) seals on the roster check alone", async () => {
    const dg = twoDevices();
    const { envelope } = rotationMaterial();
    dg.remove();
    await sealKeyRotationAfterRemoval({ removedDeviceKey: dg.stolenKey, preRemovalEpoch: null, envelope }, dg.ports);
    expect(dg.emitted).toHaveLength(1);
  });
});

describe("the envelope on a kept device (through the real core)", () => {
  it("verifies against the held key and yields the entropy of exactly the proof's new key", () => {
    const m = rotationMaterial();
    expect(m.envelope).toHaveLength(keyRotationEnvelopeLen());
    const verified = verifyKeyRotationEnvelope(m.envelope, m.oldAccount.signaturePublicKey, BigInt(NOW_SECS + 60));
    expect(Array.from(verified.newSignaturePubkey)).toEqual(Array.from(m.newAccount.signaturePublicKey));
    expect(Array.from(verified.newEntropy)).toEqual(Array.from(m.entropy));
    // The phrase a kept device rebuilds derives the same identity.
    const rebuilt = RecoveryMnemonic.fromEntropy(verified.newEntropy);
    const adopted = Identity.fromSeed(rebuilt.toSeed());
    expect(Array.from(adopted.signaturePublicKey)).toEqual(Array.from(m.newAccount.signaturePublicKey));
    verified.free();
  });

  it("is refused for another account's key, after the proof expired, and when it names the held key", () => {
    const m = rotationMaterial();
    const stranger = Identity.fromSeed(accountSeed(7));
    expect(() => verifyKeyRotationEnvelope(m.envelope, stranger.signaturePublicKey, BigInt(NOW_SECS + 60))).toThrow();
    expect(() =>
      verifyKeyRotationEnvelope(m.envelope, m.oldAccount.signaturePublicKey, BigInt(NOW_SECS + 15 * 24 * 60 * 60)),
    ).toThrow();
    // A device that already holds the new key does not store its own key.
    expect(() =>
      verifyKeyRotationEnvelope(m.envelope, m.newAccount.signaturePublicKey, BigInt(NOW_SECS + 60)),
    ).toThrow();
  });

  it("a pending envelope is dropped after 14 days", () => {
    const max = keyRotationPendingMaxAgeMs();
    expect(max).toBe(BigInt(14 * 24 * 60 * 60 * 1000));
    expect(keyRotationPendingExpired(1000n, 1000n + max)).toBe(false);
    expect(keyRotationPendingExpired(1000n, 1000n + max + 1n)).toBe(true);
  });

  it("the session-upgrade statement is the 138-byte kind-4 statement by the new key", () => {
    const m = rotationMaterial();
    const statement = signSessionUpgradeStatement(m.newAccount.ed25519Seed, ACCOUNT, "r2.refresh.credential");
    expect(statement).toHaveLength(138);
    expect(statement[1]).toBe(4);
  });
});

describe("the DS root endorsement (ADR-0074 section 4.6) is what a reset and a recall send", () => {
  /** A 177-byte attestation-shaped blob carrying `generation` where the
   *  layout has it; only the generation is read by the endorsement. */
  function attestationWithGeneration(generation: bigint): string {
    const bytes = new Uint8Array(177);
    new DataView(bytes.buffer).setBigUint64(1 + 32 + 32, generation, false);
    let binary = "";
    for (const b of bytes) binary += String.fromCharCode(b);
    return btoa(binary);
  }

  it("is 64 bytes, signed by the NEW account key over the journal, the attested generation and the root key", () => {
    const seed = accountSeed(5);
    const journal = new Uint8Array(32).fill(8);
    const rootKey = new Uint8Array(32).fill(9);
    const wasm = { Identity, signDsRootEndorsement };
    const endorsement = rootEndorsementFor(wasm, seed, journal, attestationWithGeneration(7n), rootKey);
    const raw = Uint8Array.from(atob(endorsement), (c) => c.charCodeAt(0));
    expect(raw).toHaveLength(dsRootEndorsementLen());
    expect(raw).toHaveLength(64);
    // Bound to each input: another generation, journal or root key signs
    // something else (Ed25519 is deterministic, so equal inputs are equal).
    const same = rootEndorsementFor(wasm, seed, journal, attestationWithGeneration(7n), rootKey);
    expect(same).toBe(endorsement);
    expect(rootEndorsementFor(wasm, seed, journal, attestationWithGeneration(8n), rootKey)).not.toBe(endorsement);
    expect(rootEndorsementFor(wasm, seed, new Uint8Array(32).fill(1), attestationWithGeneration(7n), rootKey)).not.toBe(endorsement);
    expect(rootEndorsementFor(wasm, seed, journal, attestationWithGeneration(7n), new Uint8Array(32).fill(1))).not.toBe(endorsement);
    expect(rootEndorsementFor(wasm, accountSeed(6), journal, attestationWithGeneration(7n), rootKey)).not.toBe(endorsement);
  });

  it("is never built without the new account seed", () => {
    const wasm = { Identity, signDsRootEndorsement };
    expect(() =>
      rootEndorsementFor(wasm, undefined, new Uint8Array(32), attestationWithGeneration(1n), new Uint8Array(32)),
    ).toThrow(/new account seed/);
  });
});
