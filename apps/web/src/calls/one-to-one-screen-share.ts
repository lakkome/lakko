// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The screen share of a one-to-one call (ADR-0083 section 4).
 *
 * The share state rides the conversation as every call signal does (an MLS
 * application message, `ScreenShare` body), so its sender is authenticated
 * by MLS; the receiver also checks that the state is signed by that sender's
 * own leaf key (`verifyCallScreenShare` with the one-to-one peer id). The
 * share's media is further tracks on the existing connection
 * (`CallSession.attachScreenShare`).
 *
 * No Solid and no DOM: the store writes go through the handed-in writer.
 */

import type {
  CallScreenShareState,
  Group as WasmGroup,
  Identity as WasmIdentity,
  VerifiedCallScreenShare,
} from "../../wasm/lakko_ffi_wasm";
import type { GroupScreenShareStore } from "./group-screen-share";
import { ShareStreams } from "./share-streams";
import { ScreenShareBinding, type ScreenCapture } from "./screen-share-binding";
import type { ShareFold } from "./screen-share-controller";

/** The wasm this module uses. */
export interface OneToOneScreenShareWasm {
  readonly CallScreenShareState: new (ownKey: Uint8Array) => CallScreenShareState;
  composeCallScreenShare(
    identity: WasmIdentity,
    callId: Uint8Array,
    peerId: string,
    seq: bigint,
    sharing: boolean,
    withSound: boolean,
  ): Uint8Array;
  composeCallScreenShareSignaling(callId: Uint8Array, state: Uint8Array): Uint8Array;
  callScreenShareOneToOnePeerId(): string;
  verifyCallScreenShare(
    group: WasmGroup,
    callId: Uint8Array,
    peerId: string,
    peerSignerKey: Uint8Array,
    state: Uint8Array,
  ): VerifiedCallScreenShare;
}

/** What {@link OneToOneScreenShare} needs. */
export interface OneToOneScreenShareDeps {
  readonly wasm: OneToOneScreenShareWasm;
  readonly identity: WasmIdentity;
  readonly callId: Uint8Array;
  /** The conversation's group, read when a state is verified. */
  group(): WasmGroup | null;
  readonly session: {
    attachScreenShare(capture: MediaStream): Promise<void>;
    detachScreenShare(): void;
  };
  /** Publish a call signal on the conversation. */
  publish(signal: Uint8Array): void;
  nameOf(personKeyB64: string): string;
  notice(text: string): void;
  readonly store: GroupScreenShareStore;
  readonly capture?: ScreenCapture;
  /** Builds the stream a share is shown from; left out, `new MediaStream`. */
  readonly makeStream?: (tracks: MediaStreamTrack[]) => MediaStream;
}

export class OneToOneScreenShare {
  private readonly binding: ScreenShareBinding<VerifiedCallScreenShare>;
  /** The one-to-one peer id (`p2p`), the core's. */
  readonly peerId: string;
  private disposed = false;
  /** The stream the other person's share is shown from (`share-streams.ts`). */
  private readonly streams: ShareStreams;

  constructor(private readonly deps: OneToOneScreenShareDeps) {
    this.streams = new ShareStreams(
      (peerId, stream, only) => deps.store.setStream(peerId, stream, only),
      deps.makeStream,
      // eslint-disable-next-line no-console
      (line) => console.info(line),
    );
    this.peerId = deps.wasm.callScreenShareOneToOnePeerId();
    const fold = new deps.wasm.CallScreenShareState(deps.identity.signaturePublicKey);
    this.binding = new ScreenShareBinding<VerifiedCallScreenShare>({
      fold: fold as unknown as ShareFold<VerifiedCallScreenShare>,
      compose: (peerId, seq, sharing, withSound) => {
        try {
          return deps.wasm.composeCallScreenShare(deps.identity, deps.callId, peerId, seq, sharing, withSound);
        } catch {
          return null;
        }
      },
      send: (state) => {
        deps.publish(deps.wasm.composeCallScreenShareSignaling(deps.callId, state));
      },
      media: {
        attach: (capture) => deps.session.attachScreenShare(capture),
        detach: () => deps.session.detachScreenShare(),
      },
      ownPeerId: () => this.peerId,
      nameOf: (key) => deps.nameOf(key),
      store: deps.store,
      notice: (text) => deps.notice(text),
      ...(deps.capture !== undefined ? { capture: deps.capture } : {}),
      // eslint-disable-next-line no-console
      log: (line) => console.info(line),
    });
  }

  /** The call connected: the button may appear; a live share is re-sent. */
  onConnected(): void {
    this.binding.enable();
    this.binding.resend();
  }

  /**
   * A `ScreenShare` call signal from the conversation. `senderLeafKey` is
   * the MLS sender's own leaf signature key, resolved at receipt; a state
   * without it, or not signed by it, is dropped.
   */
  receive(state: Uint8Array, senderLeafKey: Uint8Array | null): void {
    if (this.disposed || senderLeafKey === null || senderLeafKey.length !== 32) return;
    const group = this.deps.group();
    if (group === null) return;
    let verified: VerifiedCallScreenShare;
    try {
      verified = this.deps.wasm.verifyCallScreenShare(group, this.deps.callId, this.peerId, senderLeafKey, state);
    } catch {
      return;
    }
    try {
      this.binding.receive(verified);
    } finally {
      verified.free();
    }
  }

  /** A track of the other person's share arrived. The share is shown from
   *  a stream of its own, never the browser's stream it came in, which can
   *  keep an earlier share's track beside it (`share-streams.ts`). */
  onRemoteTrack(track: MediaStreamTrack): void {
    if (this.disposed) return;
    this.streams.add(this.peerId, track);
  }

  /** Names may have changed. */
  refreshNames(): void {
    this.binding.refreshNames();
  }

  /** The call ended. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.binding.dispose();
  }
}
