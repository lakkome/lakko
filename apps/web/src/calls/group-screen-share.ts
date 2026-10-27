// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The screen share of an SFU call (ADR-0083 section 3), between the session
 * and the call screen's store. Shared by the two SFU adapters
 * (`sfu-call-ui.ts` for a call in a conversation, `multi-joiner-call.ts` for
 * a link call), so the rules are written once:
 *
 *   * a share state arrives sealed on the relay; it is verified against the
 *     leaf key the sender's own peer announcement verified with, and only
 *     then folded. A state from a peer not yet identified is held (the
 *     newest one per peer) and verified once the peer is;
 *   * a peer that leaves ends its share as if it had said `stopped`;
 *   * a peer that joins is told again that this device shares, if it does;
 *   * a remote track under `<peer>~screen` is that peer's share, never a
 *     participant.
 *
 * No Solid and no DOM: the store writes go through the handed-in writer.
 */

import type { CallScreenShareState, VerifiedCallScreenShare } from "../../wasm/lakko_ffi_wasm";
import { bytesOfHex, type PeerAnnouncer } from "./call-relay";
import { ScreenShareBinding, type ScreenCapture, type ScreenShareStoreWriter } from "./screen-share-binding";
import type { ShareFold } from "./screen-share-controller";
import { ShareStreams } from "./share-streams";
import { screenStreamPeerOf } from "./screen-share-tracks";

/** The session, as the share needs it. `SfuCallSession` satisfies it. */
export interface ScreenShareSession {
  sendSealedRelay(plaintext: Uint8Array): boolean;
  attachScreenShare(capture: MediaStream): Promise<void>;
  detachScreenShare(): void;
  getSelfPeerId(): string | null;
}

/** The store, including where a received share's stream goes. */
export interface GroupScreenShareStore extends ScreenShareStoreWriter {
  setStream(peerId: string, stream: MediaStream | null, only?: MediaStream): void;
}

/** What {@link GroupScreenShare} needs. */
export interface GroupScreenShareDeps {
  readonly announcer: PeerAnnouncer | undefined;
  readonly session: ScreenShareSession;
  nameOf(personKeyB64: string): string;
  notice(text: string): void;
  readonly store: GroupScreenShareStore;
  readonly capture?: ScreenCapture;
  /** Builds the stream a share is shown from; left out, `new MediaStream`. */
  readonly makeStream?: (tracks: MediaStreamTrack[]) => MediaStream;
}

/** The wasm the share builds its fold with. */
export interface ScreenShareFoldFactory {
  readonly CallScreenShareState: new (ownKey: Uint8Array) => CallScreenShareState;
}

export class GroupScreenShare {
  private binding: ScreenShareBinding<VerifiedCallScreenShare> | null = null;
  /** The leaf key each identified peer's announcement verified with. */
  private readonly signerKeys = new Map<string, Uint8Array>();
  /** The newest state of each peer not identified yet. */
  private readonly held = new Map<string, Uint8Array>();
  private connected = false;
  private disposed = false;
  /** The stream each received share is shown from (`share-streams.ts`). */
  private readonly streams: ShareStreams;

  constructor(private readonly deps: GroupScreenShareDeps) {
    this.streams = new ShareStreams(
      (peerId, stream, only) => deps.store.setStream(peerId, stream, only),
      deps.makeStream,
      // eslint-disable-next-line no-console
      (line) => console.info(line),
    );
  }

  /**
   * The wasm is loaded: build the fold. A call whose announcer cannot sign
   * share states (no identity) shares nothing and receives nothing; the
   * screen's own tracks still reach the store.
   */
  init(wasm: ScreenShareFoldFactory): void {
    if (this.disposed || this.binding !== null) return;
    const announcer = this.deps.announcer;
    const ownKey = announcer?.ownSignerKey?.();
    if (announcer === undefined || ownKey === undefined) return;
    let fold: CallScreenShareState;
    try {
      fold = new wasm.CallScreenShareState(ownKey);
    } catch {
      return;
    }
    this.binding = new ScreenShareBinding<VerifiedCallScreenShare>({
      fold: fold as unknown as ShareFold<VerifiedCallScreenShare>,
      compose: (peerId, seq, sharing, withSound) =>
        announcer.composeScreenShare?.(peerId, seq, sharing, withSound) ?? null,
      send: (state) => {
        this.deps.session.sendSealedRelay(state);
      },
      media: {
        attach: (capture) => this.deps.session.attachScreenShare(capture),
        detach: () => this.deps.session.detachScreenShare(),
      },
      ownPeerId: () => this.deps.session.getSelfPeerId(),
      nameOf: (key) => this.deps.nameOf(key),
      store: this.deps.store,
      notice: (text) => this.deps.notice(text),
      ...(this.deps.capture !== undefined ? { capture: this.deps.capture } : {}),
      // eslint-disable-next-line no-console
      log: (line) => console.info(line),
    });
    if (this.connected) this.binding.enable();
    for (const [peerId, state] of [...this.held]) {
      if (this.signerKeys.has(peerId)) {
        this.held.delete(peerId);
        this.verifyAndFold(peerId, state);
      }
    }
  }

  /** The binding, once {@link init} built one. */
  shareBinding(): ScreenShareBinding<VerifiedCallScreenShare> | null {
    return this.binding;
  }

  /** The call connected: the button may appear; a live share is re-sent. */
  onConnected(): void {
    this.connected = true;
    this.binding?.enable();
    this.binding?.resend();
  }

  /** A peer's announcement verified: its leaf key is now known. */
  onPeerIdentified(peerId: string, memberKeyHex: string): void {
    const key = bytesOfHex(memberKeyHex);
    if (key === null || key.length !== 32) return;
    this.signerKeys.set(peerId, key);
    const state = this.held.get(peerId);
    if (state !== undefined && this.binding !== null) {
      this.held.delete(peerId);
      this.verifyAndFold(peerId, state);
    }
  }

  /** Someone joined: they learn this device's share as they learn its
   *  camera state. */
  onPeerJoined(): void {
    this.binding?.resend();
  }

  /** Someone left: their share ends with them. */
  onPeerLeft(peerId: string): void {
    this.held.delete(peerId);
    const key = this.signerKeys.get(peerId);
    this.signerKeys.delete(peerId);
    this.streams.drop(peerId);
    if (key !== undefined) this.binding?.participantLeft(key);
  }

  /** A share state arrived on the relay from `peerId`. */
  onState(peerId: string, state: Uint8Array): void {
    if (this.disposed) return;
    if (this.binding === null || !this.signerKeys.has(peerId)) {
      this.held.set(peerId, state);
      return;
    }
    this.verifyAndFold(peerId, state);
  }

  /**
   * A remote track: `true` when it is a share's, which goes to the store and
   * nowhere else. The share is shown from a stream of its own, not from the
   * browser's stream for `<peer>~screen`, which keeps an earlier share's
   * track beside a later one (`share-streams.ts`).
   */
  onRemoteTrack(streamId: string, track: MediaStreamTrack): boolean {
    const peerId = screenStreamPeerOf(streamId);
    if (peerId === null) return false;
    this.streams.add(peerId, track);
    return true;
  }

  /** Names may have changed. */
  refreshNames(): void {
    this.binding?.refreshNames();
  }

  /** The call is over. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.binding?.dispose();
    this.binding = null;
    this.signerKeys.clear();
    this.held.clear();
  }

  private verifyAndFold(peerId: string, state: Uint8Array): void {
    const key = this.signerKeys.get(peerId);
    const binding = this.binding;
    if (key === undefined || binding === null) return;
    const verified = this.deps.announcer?.verifyScreenShare?.(peerId, key, state) ?? null;
    if (verified === null) return;
    try {
      binding.receive(verified);
    } finally {
      verified.free();
    }
  }
}
