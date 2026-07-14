// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0032 committer election + presence — pure glue exercised exactly as
 * the multi-joiner orchestrator drives it, plus the real `electCommitter`
 * wasm decision to prove owner-if-present-else-lowest-leaf end to end.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { electCommitter } from "../../wasm/lakko_ffi_wasm";
import {
  CallPresence,
  buildElectionArrays,
  electActingCommitterLeaf,
  hexKey,
  type ElectionMember,
} from "./multi-joiner-election";

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

const KEY_OWNER = new Uint8Array(32).fill(0xa1);
const KEY_B = new Uint8Array(32).fill(0xb2);
const KEY_C = new Uint8Array(32).fill(0xc3);
const OWNER_HEX = hexKey(KEY_OWNER);

/** Owner @ leaf 0, joiner-B @ leaf 1, joiner-C @ leaf 2. */
const MEMBERS: ElectionMember[] = [
  { leafIndex: 0, signatureKeyHex: OWNER_HEX },
  { leafIndex: 1, signatureKeyHex: hexKey(KEY_B) },
  { leafIndex: 2, signatureKeyHex: hexKey(KEY_C) },
];

const wasm = { electCommitter };

describe("buildElectionArrays (pure)", () => {
  it("marks the owner + present flags parallel to the leaf indices", () => {
    const present = new Set([OWNER_HEX, hexKey(KEY_C)]);
    const { leafIndices, isOwner, isPresent } = buildElectionArrays(
      MEMBERS,
      OWNER_HEX,
      present,
    );
    expect(Array.from(leafIndices)).toEqual([0, 1, 2]);
    expect(Array.from(isOwner)).toEqual([1, 0, 0]);
    expect(Array.from(isPresent)).toEqual([1, 0, 1]);
  });
});

describe("electActingCommitterLeaf (owner-if-present-else-lowest-leaf)", () => {
  it("elects the owner when the owner is present", () => {
    const present = new Set([OWNER_HEX, hexKey(KEY_B), hexKey(KEY_C)]);
    expect(electActingCommitterLeaf(wasm, MEMBERS, OWNER_HEX, present)).toBe(0);
  });

  it("elects the lowest-leaf present member when the owner has left", () => {
    const present = new Set([hexKey(KEY_B), hexKey(KEY_C)]);
    expect(electActingCommitterLeaf(wasm, MEMBERS, OWNER_HEX, present)).toBe(1);
  });

  it("elects the sole present member even if it is not the lowest leaf", () => {
    const present = new Set([hexKey(KEY_C)]);
    expect(electActingCommitterLeaf(wasm, MEMBERS, OWNER_HEX, present)).toBe(2);
  });

  it("returns undefined when no member is present", () => {
    expect(
      electActingCommitterLeaf(wasm, MEMBERS, OWNER_HEX, new Set()),
    ).toBeUndefined();
  });

  it("reverts to the owner on reclaim (owner returns to the present set)", () => {
    const afterLeave = new Set([hexKey(KEY_B)]);
    expect(electActingCommitterLeaf(wasm, MEMBERS, OWNER_HEX, afterLeave)).toBe(
      1,
    );
    const afterReclaim = new Set([OWNER_HEX, hexKey(KEY_B)]);
    expect(
      electActingCommitterLeaf(wasm, MEMBERS, OWNER_HEX, afterReclaim),
    ).toBe(0);
  });
});

describe("CallPresence", () => {
  it("always counts self present", () => {
    const p = new CallPresence(hexKey(KEY_B), OWNER_HEX, "tag-owner");
    expect(p.presentKeySet().has(hexKey(KEY_B))).toBe(true);
  });

  it("counts the owner present iff the owner peer_tag is in the roster", () => {
    const p = new CallPresence(hexKey(KEY_B), OWNER_HEX, "tag-owner");
    p.setRoster([{ peerId: "p-owner", peerTag: "tag-owner" }]);
    expect(p.ownerPresent()).toBe(true);
    expect(p.presentKeySet().has(OWNER_HEX)).toBe(true);

    p.removePeer("p-owner");
    expect(p.ownerPresent()).toBe(false);
    expect(p.presentKeySet().has(OWNER_HEX)).toBe(false);
  });

  it("treats owner-is-self as always present (owner's own client)", () => {
    // On the owner's client, ownerPeerTag is null and owner == self.
    const p = new CallPresence(OWNER_HEX, OWNER_HEX, null);
    expect(p.ownerPresent()).toBe(true);
  });

  it("counts a bound peer present only while its peer is in the roster", () => {
    const p = new CallPresence(OWNER_HEX, OWNER_HEX, null);
    p.addPeer("p-c", "tag-c");
    p.bindPeerToMember("p-c", hexKey(KEY_C));
    expect(p.presentKeySet().has(hexKey(KEY_C))).toBe(true);
    p.removePeer("p-c");
    expect(p.presentKeySet().has(hexKey(KEY_C))).toBe(false);
  });
});
