// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sealed call relay and the peer announcement (ADR-0078 section 2),
 * through the real wasm core: a payload sealed under one epoch opens after a
 * rekey (the previous key is kept), an unsealed one never opens, and an
 * announcement verifies only against a leaf of the call group, only for the
 * peer id the SFU stamped as the sender.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import * as wasmNs from "../../wasm/lakko_ffi_wasm";
import init, { Group, GroupKind, Identity } from "../../wasm/lakko_ffi_wasm";
import { CallRelay, groupPeerAnnouncer } from "./call-relay";

const CALL = new Uint8Array(16).fill(0x11);

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("CallRelay", () => {
  it("opens under the current and the previous key, and never an unsealed payload", () => {
    const sender = new CallRelay(wasmNs);
    const receiver = new CallRelay(wasmNs);
    const seedA = new Uint8Array(32).fill(1);
    const seedB = new Uint8Array(32).fill(2);
    sender.setSeed(seedA);
    receiver.setSeed(seedA);
    const state = wasmNs.composeMediaState(true, false);
    const sealedA = sender.seal(state);
    expect(sealedA).not.toBeNull();

    receiver.setSeed(seedB);
    expect(receiver.open(sealedA!)).toEqual({
      kind: "media-state",
      cameraOn: true,
      microphoneOn: false,
    });
    expect(receiver.open(state)).toBeNull();

    receiver.setSeed(new Uint8Array(32).fill(3));
    expect(receiver.open(sealedA!)).toBeNull();
  });

  it("seals nothing before it has a seed", () => {
    expect(new CallRelay(wasmNs).seal(new Uint8Array([1]))).toBeNull();
  });
});

describe("groupPeerAnnouncer", () => {
  it("names a roster member for its own peer id only", () => {
    const alice = Identity.fromSeed(new Uint8Array(32).fill(0xa1));
    const bob = Identity.fromSeed(new Uint8Array(32).fill(0xb2));
    const carol = Identity.fromSeed(new Uint8Array(32).fill(0xc3));
    const group = new Group(alice, GroupKind.Group);
    group.addMember(alice, bob.createKeyPackage());

    const asBob = groupPeerAnnouncer(wasmNs, bob, CALL, () => group);
    const asCarol = groupPeerAnnouncer(wasmNs, carol, CALL, () => group);
    const verifier = groupPeerAnnouncer(wasmNs, alice, CALL, () => group);

    const ann = asBob.compose("bobpeer");
    expect(ann).not.toBeNull();
    const who = verifier.verify("bobpeer", ann!);
    expect(who?.memberKeyHex).toBe(
      Array.from(bob.signaturePublicKey, (b) => b.toString(16).padStart(2, "0")).join(""),
    );
    expect(who?.personKeyB64.length).toBeGreaterThan(0);
    // Relayed under another peer id, or from someone outside the group.
    expect(verifier.verify("alicepeer", ann!)).toBeNull();
    expect(verifier.verify("carolpeer", asCarol.compose("carolpeer")!)).toBeNull();
    // No group yet: nothing is composed or verified.
    const none = groupPeerAnnouncer(wasmNs, bob, CALL, () => null);
    expect(none.compose("bobpeer")).toBeNull();
    expect(none.verify("bobpeer", ann!)).toBeNull();
  });
});

describe("call names through the real core (docs/ux/calls.md 4.1)", () => {
  it("seals, classifies and verifies a name only against the announced leaf", () => {
    const alice = Identity.fromSeed(new Uint8Array(32).fill(0xa1));
    const bob = Identity.fromSeed(new Uint8Array(32).fill(0xb2));
    const group = new Group(alice, GroupKind.Group);
    group.addMember(alice, bob.createKeyPackage());
    const asBob = groupPeerAnnouncer(wasmNs, bob, CALL, () => group);
    const verifier = groupPeerAnnouncer(wasmNs, alice, CALL, () => group);

    const wire = asBob.composeCallName?.("bobpeer", 1n, "Captain Bob") ?? null;
    expect(wire).not.toBeNull();
    const relay = new CallRelay(wasmNs);
    relay.setSeed(new Uint8Array(32).fill(4));
    expect(relay.open(relay.seal(wire!)!)).toEqual({ kind: "call-name", bytes: wire });

    expect(verifier.verifyCallName?.("bobpeer", bob.signaturePublicKey, wire!)).toEqual({
      seq: 1n,
      name: "Captain Bob",
    });
    // Another peer id, or another leaf's key: dropped.
    expect(verifier.verifyCallName?.("alicepeer", bob.signaturePublicKey, wire!)).toBeNull();
    expect(verifier.verifyCallName?.("bobpeer", alice.signaturePublicKey, wire!)).toBeNull();
    // An empty name goes back to the default; an untrimmed one is refused.
    const cleared = asBob.composeCallName?.("bobpeer", 2n, "") ?? null;
    expect(verifier.verifyCallName?.("bobpeer", bob.signaturePublicKey, cleared!)).toEqual({
      seq: 2n,
      name: null,
    });
    expect(asBob.composeCallName?.("bobpeer", 3n, " Bob")).toBeNull();
    expect(wasmNs.callNameIsNewer(2n, 1n)).toBe(true);
    expect(wasmNs.callNameIsNewer(1n, 1n)).toBe(false);
  });
});
