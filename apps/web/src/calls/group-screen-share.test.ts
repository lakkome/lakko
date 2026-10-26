// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A group call's share between the session and the store (ADR-0083
 * section 3), with the real core: a state is folded only once it verifies
 * against the leaf key the sender's own announcement verified with (and is
 * held until then), a share track is never a participant, and a sharer who
 * leaves takes their share with them.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasmNs from "../../wasm/lakko_ffi_wasm";
import { Group, GroupKind, Identity } from "../../wasm/lakko_ffi_wasm";
import { groupPeerAnnouncer } from "./call-relay";
import { GroupScreenShare, type GroupScreenShareStore } from "./group-screen-share";
import type { ScreenShareControl, ShareOnScreen } from "./screen-share-store";

const CALL = new Uint8Array(16).fill(0x21);

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function setup(): {
  alice: Identity;
  share: GroupScreenShare;
  store: {
    current: ShareOnScreen | null;
    control: ScreenShareControl | null;
    streams: Map<string, MediaStream>;
  } & GroupScreenShareStore;
  sent: Uint8Array[];
  compose: (seq: bigint, sharing: boolean) => Uint8Array;
} {
  const alice = Identity.fromSeed(new Uint8Array(32).fill(0xa1));
  const bob = Identity.fromSeed(new Uint8Array(32).fill(0xb2));
  const group = new Group(alice, GroupKind.Group);
  group.addMember(alice, bob.createKeyPackage());
  const sent: Uint8Array[] = [];
  const store = {
    current: null as ShareOnScreen | null,
    control: null as ScreenShareControl | null,
    streams: new Map<string, MediaStream>(),
    setCurrent(c: ShareOnScreen | null): void {
      store.current = c;
    },
    setControl(c: ScreenShareControl | null): void {
      store.control = c;
    },
    setBusy(): void {},
    setStream(peerId: string, stream: MediaStream | null, only?: MediaStream): void {
      if (stream === null) {
        if (only === undefined || store.streams.get(peerId) === only) store.streams.delete(peerId);
      } else {
        store.streams.set(peerId, stream);
      }
    },
  };
  const share = new GroupScreenShare({
    announcer: groupPeerAnnouncer(wasmNs, bob, CALL, () => group),
    session: {
      sendSealedRelay: (p) => {
        sent.push(p);
        return true;
      },
      attachScreenShare: () => Promise.resolve(),
      detachScreenShare: () => {},
      getSelfPeerId: () => "peer-b",
    },
    nameOf: () => "Anna",
    notice: () => {},
    store,
    capture: {
      available: () => true,
      getDisplayMedia: () => Promise.reject(new Error("unused")),
      supportedConstraints: () => ({}),
      macOs: () => false,
      now: () => 0,
    },
  });
  share.init(wasmNs);
  const compose = (seq: bigint, sharing: boolean): Uint8Array =>
    wasmNs.composeCallScreenShare(alice, CALL, "peer-a", seq, sharing, false);
  return { alice, share, store, sent, compose };
}

function fakeTrackAndStream(): { track: MediaStreamTrack; stream: MediaStream } {
  const track = {
    readyState: "live",
    addEventListener: () => {},
  } as unknown as MediaStreamTrack;
  const stream = { getTracks: () => [track] } as unknown as MediaStream;
  return { track, stream };
}

describe("GroupScreenShare", () => {
  it("holds a state from an unidentified peer and folds it once the peer is", () => {
    const { alice, share, store, compose } = setup();
    share.onState("peer-a", compose(1n, true));
    expect(store.current).toBeNull();
    share.onPeerIdentified("peer-a", hex(alice.signaturePublicKey));
    expect(store.current).toMatchObject({ peerId: "peer-a", isOwn: false, name: "Anna" });
  });

  it("drops a state that does not verify against the sender's announced key", () => {
    const { share, store, compose } = setup();
    share.onPeerIdentified("peer-a", hex(new Uint8Array(32).fill(7)));
    share.onState("peer-a", compose(1n, true));
    expect(store.current).toBeNull();
  });

  it("drops a state relayed under another peer's id", () => {
    const { alice, share, store, compose } = setup();
    share.onPeerIdentified("peer-c", hex(alice.signaturePublicKey));
    share.onState("peer-c", compose(1n, true));
    expect(store.current).toBeNull();
  });

  it("takes a share with its sharer when they leave", () => {
    const { alice, share, store, compose } = setup();
    share.onPeerIdentified("peer-a", hex(alice.signaturePublicKey));
    share.onState("peer-a", compose(1n, true));
    expect(store.current).not.toBeNull();
    share.onPeerLeft("peer-a");
    expect(store.current).toBeNull();
  });

  it("keeps a share track out of the roster, in the store", () => {
    const { share, store } = setup();
    const { track, stream } = fakeTrackAndStream();
    expect(share.onRemoteTrack("peer-a~screen", track, stream)).toBe(true);
    expect(store.streams.get("peer-a")).toBe(stream);
    expect(share.onRemoteTrack("peer-a", track, stream)).toBe(false);
    expect(share.onRemoteTrack("peer-a~light", track, stream)).toBe(false);
  });

  it("offers the button once connected", () => {
    const { share, store } = setup();
    expect(store.control).toBeNull();
    share.onConnected();
    expect(store.control).not.toBeNull();
    share.dispose();
    expect(store.control).toBeNull();
  });
});
