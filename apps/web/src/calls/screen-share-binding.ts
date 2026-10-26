// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One call's screen share, wired: the capture, the fold, the store the call
 * screen reads (`docs/ux/screen-share.md` sections 2 to 5).
 *
 * The same for a group call and a one-to-one call. What differs is handed in:
 * how a share's media is added to and removed from the call
 * ({@link ScreenShareMedia}), how a signed state is composed and sent, and
 * the peer id this device shares as. The call adapters
 * (`sfu-call-ui.ts`, `CallHostContext.tsx`) build one per call and feed it
 * what arrives.
 *
 * No Solid and no DOM beyond what is handed in, so a test drives it with
 * fakes; the store writes go through {@link ScreenShareBindingDeps.store}.
 */

import { t } from "../l10n";
import {
  browserScreenShareEnv,
  canSendScreenShare,
  classifyShareStartError,
  displayMediaOptions,
  isMacOs,
  screenShareRefusedText,
} from "./screen-share-capability";
import {
  ScreenShareController,
  type CurrentShare,
  type ShareFold,
} from "./screen-share-controller";
import type { ScreenShareControl, ShareOnScreen } from "./screen-share-store";
import { ScreenShareNotSealedError } from "./screen-share-tracks";

/** How a share's tracks join and leave the call. */
export interface ScreenShareMedia {
  /**
   * Add the capture's tracks to the call, sealed, before any offer describes
   * them. Rejects with {@link ScreenShareNotSealedError} when the share could
   * not be protected (it is then not sent at all, invariant #4), or with any
   * other error when it could not be added.
   */
  attach(capture: MediaStream): Promise<void>;
  /** Take the share's tracks out of the call. Idempotent. */
  detach(): void;
}

export { ScreenShareNotSealedError } from "./screen-share-tracks";

/** The browser's screen capture, as {@link ScreenShareBinding} uses it. */
export interface ScreenCapture {
  /** Whether this browser may send at all (section 1). */
  available(): boolean;
  getDisplayMedia(options: DisplayMediaStreamOptions): Promise<MediaStream>;
  supportedConstraints(): Partial<Record<string, boolean>>;
  /** Whether the page runs on macOS. */
  macOs(): boolean;
  now(): number;
}

/** The page's own capture. */
export function browserScreenCapture(): ScreenCapture {
  return {
    available: () => canSendScreenShare(browserScreenShareEnv()),
    getDisplayMedia: (options) => navigator.mediaDevices.getDisplayMedia(options),
    supportedConstraints: () => {
      try {
        return navigator.mediaDevices.getSupportedConstraints() as Partial<Record<string, boolean>>;
      } catch {
        return {};
      }
    },
    macOs: () => {
      try {
        const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
        return isMacOs(nav.userAgent, nav.userAgentData?.platform ?? nav.platform);
      } catch {
        return false;
      }
    },
    now: () => Date.now(),
  };
}

/** The store the call screen reads, as the binding writes it. */
export interface ScreenShareStoreWriter {
  setCurrent(next: ShareOnScreen | null): void;
  setControl(next: ScreenShareControl | null): void;
  setBusy(on: boolean): void;
}

/** What one call's binding needs. */
export interface ScreenShareBindingDeps<V> {
  readonly fold: ShareFold<V>;
  /** This device's signed state for share `seq` as the peer id it shares
   *  under, or `null` when it cannot be composed now. */
  compose(peerId: string, seq: bigint, sharing: boolean, withSound: boolean): Uint8Array | null;
  /** Put a state on the call's transport (to everyone in the call). */
  send(state: Uint8Array): void;
  readonly media: ScreenShareMedia;
  /** The peer id this device shares as: its SFU peer id, or the one-to-one
   *  peer id; `null` while it has none yet (before the SFU's Welcome). */
  ownPeerId(): string | null;
  /** The name of a person by base64 account key; empty when unknown. */
  nameOf(accountKeyB64: string): string;
  readonly store: ScreenShareStoreWriter;
  /** One line above the row for a few seconds. */
  notice(text: string): void;
  readonly capture?: ScreenCapture;
  /** Logged lines (diagnostics: no key, no name). */
  log?(line: string): void;
}

/**
 * One call's share.
 *
 * {@link control} is what the call screen's button calls; it is registered in
 * the store by {@link enable} once the call is connected, and taken away by
 * {@link dispose}.
 */
export class ScreenShareBinding<V> {
  readonly controller: ScreenShareController<V>;
  private captureStream: MediaStream | null = null;
  private starting = false;
  private disposed = false;
  private readonly capture: ScreenCapture;

  constructor(private readonly deps: ScreenShareBindingDeps<V>) {
    this.capture = deps.capture ?? browserScreenCapture();
    this.controller = new ScreenShareController<V>({
      fold: deps.fold,
      compose: (seq, sharing, withSound) => {
        const peerId = this.sharedAs ?? deps.ownPeerId();
        return peerId === null ? null : deps.compose(peerId, seq, sharing, withSound);
      },
      send: (state) => deps.send(state),
      stopCapture: () => this.endCapture(),
      onChange: (current) => this.publish(current),
      onSuperseded: (by) => {
        const name = by === null || by.accountKeyB64 === null ? "" : deps.nameOf(by.accountKeyB64);
        deps.notice(t("call.screen_share.superseded", { name }));
      },
    });
  }

  /** The peer id the live share was started under. */
  private sharedAs: string | null = null;

  /** What the screen's button calls. */
  readonly control: ScreenShareControl = {
    start: (): void => {
      void this.start();
    },
    stop: (): void => {
      this.stop();
    },
  };

  /** The call is connected: the button may appear, where the browser can
   *  send at all. A receive-only device registers nothing. */
  enable(): void {
    if (this.disposed) return;
    if (!this.capture.available()) return;
    this.deps.store.setControl(this.control);
  }

  /** The call is not connected (any more): the button goes. A live share is
   *  left running; it is re-sent when the call comes back. */
  disable(): void {
    this.deps.store.setControl(null);
  }

  /** A verified state from someone else in the call. */
  receive(state: V): void {
    this.controller.receive(state);
  }

  /** Someone left the call. */
  participantLeft(signerKey: Uint8Array): void {
    this.controller.participantLeft(signerKey);
  }

  /** Someone joined, or the call came back: say again that this device
   *  shares, if it does. */
  resend(): void {
    this.controller.resend();
  }

  /** Names may have changed (a profile landed). */
  refreshNames(): void {
    this.publish(this.controller.current());
  }

  /** Whether this device is sharing now. */
  sharing(): boolean {
    return this.controller.ownSharing();
  }

  /**
   * Open the picker and share what the person picks (section 2). Every way
   * this does not start is handled here: a cancel says nothing, a refusal
   * says how to allow it, a share that could not be sealed is not sent.
   */
  async start(): Promise<void> {
    if (this.disposed || this.starting || this.controller.ownSharing()) return;
    this.starting = true;
    this.deps.store.setBusy(true);
    const startedAt = this.capture.now();
    let stream: MediaStream;
    try {
      stream = await this.capture.getDisplayMedia(
        displayMediaOptions(this.capture.supportedConstraints()),
      );
    } catch (err) {
      this.finishStart();
      const failure = classifyShareStartError(err, this.capture.now() - startedAt);
      this.deps.log?.(`call screen share not started: ${failure}`);
      if (failure === "refused" && !this.disposed) {
        this.deps.notice(screenShareRefusedText(this.capture.macOs()));
      }
      return;
    }
    const video = stream.getVideoTracks()[0];
    if (this.disposed || video === undefined) {
      stopAll(stream);
      this.finishStart();
      return;
    }
    // A screen is read, not watched: the encoder keeps sharpness over motion.
    try {
      video.contentHint = "detail";
    } catch {
      // A browser without the hint encodes as it would.
    }
    const withSound = stream.getAudioTracks().length > 0;
    try {
      await this.deps.media.attach(stream);
    } catch (err) {
      stopAll(stream);
      this.deps.media.detach();
      this.finishStart();
      if (this.disposed) return;
      this.deps.log?.(
        `call screen share not started: ${err instanceof ScreenShareNotSealedError ? "not sealed" : "not added"}`,
      );
      this.deps.notice(t("call.screen_share.not_sealed"));
      return;
    }
    if (this.disposed) {
      stopAll(stream);
      this.deps.media.detach();
      this.finishStart();
      return;
    }
    const peerId = this.deps.ownPeerId();
    if (peerId === null) {
      stopAll(stream);
      this.deps.media.detach();
      this.finishStart();
      return;
    }
    this.captureStream = stream;
    this.sharedAs = peerId;
    // The browser's own "Stop sharing" bar, or the shared window closing,
    // ends the track: that is a Stop like Lakko's own.
    video.addEventListener("ended", () => {
      if (this.captureStream === stream) this.stop();
    });
    this.controller.startOwn(peerId, withSound);
    this.deps.log?.(`call screen share started sound=${withSound ? "yes" : "no"}`);
    this.finishStart();
  }

  /** Stop this device's share (the button, the strip, the browser's bar). */
  stop(): void {
    if (this.captureStream === null && !this.controller.ownSharing()) return;
    this.endCapture();
    this.controller.stopOwn();
    this.sharedAs = null;
    this.deps.log?.("call screen share stopped");
  }

  /** The call is over: the capture ends, the button goes, the fold is freed. */
  dispose(): void {
    if (this.disposed) return;
    this.endCapture();
    this.controller.stopOwn();
    this.disposed = true;
    this.controller.dispose();
    this.deps.store.setControl(null);
    this.deps.store.setBusy(false);
    this.deps.store.setCurrent(null);
  }

  private finishStart(): void {
    this.starting = false;
    this.deps.store.setBusy(false);
  }

  private endCapture(): void {
    const stream = this.captureStream;
    this.captureStream = null;
    if (stream !== null) stopAll(stream);
    this.deps.media.detach();
  }

  private publish(current: CurrentShare | null): void {
    if (this.disposed && current !== null) return;
    this.deps.store.setCurrent(
      current === null
        ? null
        : {
            key: current.key,
            peerId: current.peerId,
            name:
              current.isOwn || current.accountKeyB64 === null
                ? ""
                : this.deps.nameOf(current.accountKeyB64),
            isOwn: current.isOwn,
            withSound: current.withSound,
          },
    );
  }
}

function stopAll(stream: MediaStream): void {
  for (const track of stream.getTracks()) {
    try {
      track.stop();
    } catch {
      // best-effort
    }
  }
}
