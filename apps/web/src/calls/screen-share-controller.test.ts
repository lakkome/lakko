// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One share at a time (ADR-0083 section 2, C-87), driven through the real
 * core fold and the real signed states: two devices that started in the same
 * moment end with the same current share, the loser stops its capture and
 * says so, and a later start wins.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasmNs from "../../wasm/lakko_ffi_wasm";
import { Group, GroupKind, Identity, type VerifiedCallScreenShare } from "../../wasm/lakko_ffi_wasm";
import { ScreenShareController, type CurrentShare, type ShareFold } from "./screen-share-controller";

const CALL = new Uint8Array(16).fill(0x42);

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

interface Device {
  readonly identity: Identity;
  readonly peerId: string;
  readonly controller: ScreenShareController<VerifiedCallScreenShare>;
  readonly sent: Uint8Array[];
  readonly changes: (CurrentShare | null)[];
  stops: number;
  superseded: number;
}

function device(identity: Identity, peerId: string): Device {
  const d: Device = {
    identity,
    peerId,
    sent: [],
    changes: [],
    stops: 0,
    superseded: 0,
    controller: undefined as unknown as ScreenShareController<VerifiedCallScreenShare>,
  };
  const fold = new wasmNs.CallScreenShareState(identity.signaturePublicKey);
  (d as { controller: ScreenShareController<VerifiedCallScreenShare> }).controller =
    new ScreenShareController<VerifiedCallScreenShare>({
      fold: fold as unknown as ShareFold<VerifiedCallScreenShare>,
      compose: (seq, sharing, withSound) =>
        wasmNs.composeCallScreenShare(identity, CALL, peerId, seq, sharing, withSound),
      send: (state) => d.sent.push(state),
      stopCapture: () => {
        d.stops += 1;
      },
      onChange: (c) => d.changes.push(c),
      onSuperseded: () => {
        d.superseded += 1;
      },
    });
  return d;
}

/** Hand everything `from` sent so far to `to`, verified as the receiver
 *  would: against the leaf key `from`'s announcement verified with. */
function deliver(group: Group, from: Device, to: Device): void {
  for (const state of from.sent.splice(0)) {
    const verified = wasmNs.verifyCallScreenShare(
      group,
      CALL,
      from.peerId,
      from.identity.signaturePublicKey,
      state,
    );
    try {
      to.controller.receive(verified);
    } finally {
      verified.free();
    }
  }
}

function pair(): { group: Group; a: Device; b: Device } {
  const alice = Identity.fromSeed(new Uint8Array(32).fill(0xa1));
  const bob = Identity.fromSeed(new Uint8Array(32).fill(0xb2));
  const group = new Group(alice, GroupKind.Group);
  group.addMember(alice, bob.createKeyPackage());
  return { group, a: device(alice, "peer-a"), b: device(bob, "peer-b") };
}

describe("ScreenShareController", () => {
  it("shows another device's share and its stop", () => {
    const { group, a, b } = pair();
    expect(a.controller.startOwn(a.peerId, true)).not.toBeNull();
    expect(a.controller.current()?.isOwn).toBe(true);
    deliver(group, a, b);
    expect(b.controller.current()).toMatchObject({ peerId: "peer-a", isOwn: false, withSound: true });
    expect(b.controller.current()?.accountKeyB64).not.toBeNull();

    a.controller.stopOwn();
    expect(a.controller.ownSharing()).toBe(false);
    deliver(group, a, b);
    expect(b.controller.current()).toBeNull();
  });

  it("ends two starts in the same moment with one share, the same on both", () => {
    const { group, a, b } = pair();
    a.controller.startOwn(a.peerId, false);
    b.controller.startOwn(b.peerId, false);
    deliver(group, a, b);
    deliver(group, b, a);
    // Each loser's stop reaches the winner.
    deliver(group, a, b);
    deliver(group, b, a);

    const ca = a.controller.current();
    const cb = b.controller.current();
    expect(ca).not.toBeNull();
    expect(ca?.key).toBe(cb?.key);
    // Exactly one of them lost, stopped its capture and was told.
    expect(a.stops + b.stops).toBe(1);
    expect(a.superseded + b.superseded).toBe(1);
    expect(a.controller.ownSharing() !== b.controller.ownSharing()).toBe(true);
    const winner = a.controller.ownSharing() ? a : b;
    expect(winner.controller.current()?.isOwn).toBe(true);
  });

  it("lets a start made after hearing another share win", () => {
    const { group, a, b } = pair();
    a.controller.startOwn(a.peerId, false);
    deliver(group, a, b);
    b.controller.startOwn(b.peerId, false);
    deliver(group, b, a);
    expect(a.stops).toBe(1);
    expect(a.controller.ownSharing()).toBe(false);
    expect(a.controller.current()?.peerId).toBe("peer-b");
    // A said it stopped; B keeps showing its own.
    deliver(group, a, b);
    expect(b.controller.current()?.isOwn).toBe(true);
  });

  it("ends a share when its sharer leaves", () => {
    const { group, a, b } = pair();
    a.controller.startOwn(a.peerId, false);
    deliver(group, a, b);
    b.controller.participantLeft(a.identity.signaturePublicKey);
    expect(b.controller.current()).toBeNull();
  });

  it("re-sends a live share only", () => {
    const { a } = pair();
    a.controller.resend();
    expect(a.sent).toHaveLength(0);
    a.controller.startOwn(a.peerId, false);
    a.sent.splice(0);
    a.controller.resend();
    expect(a.sent).toHaveLength(1);
    a.controller.stopOwn();
    a.sent.splice(0);
    a.controller.resend();
    expect(a.sent).toHaveLength(0);
  });

  it("refuses a second start while sharing", () => {
    const { a } = pair();
    expect(a.controller.startOwn(a.peerId, false)).not.toBeNull();
    expect(a.controller.startOwn(a.peerId, false)).toBeNull();
  });
});
