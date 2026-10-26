// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One call's screen-share state on this device (ADR-0083 sections 1 and 2;
 * `docs/ux/screen-share.md` section 4).
 *
 * The rule that decides which share is current is the core's one-share fold
 * (`CallScreenShareState`, C-87); this class is the client's side of it:
 * what is sent when this device starts or stops, what happens when its own
 * share is superseded, and what the screen is told. It never decides a
 * winner itself.
 *
 *   * **Start**: the seq comes from the fold (`nextStartSeq`), the fold is
 *     told first (`startOwn`), then the signed `sharing` state is sent.
 *   * **Stop** (the button, the strip, the browser's own bar, the call
 *     ending): the fold is told (`stopOwn`) and `stopped` is sent with the
 *     same seq.
 *   * **Superseded**: after every change the fold is asked whether this
 *     device's own share lost; if it did, the capture is stopped, the fold
 *     told, `stopped` sent, and the person told who shares now.
 *   * **A peer joins**: {@link resend} sends this device's live `sharing`
 *     state again, as the camera state is re-sent.
 *
 * The transport (the sealed SFU relay, or the one-to-one call signalling),
 * the verification of what arrives and the capture itself are the caller's,
 * handed in as functions, so this runs with fakes in a test.
 */

import { base64Encode } from "../auth/account-id";

/** What the fold answers about the share on screen. Mirrors the wasm
 *  `CallCurrentScreenShare`. */
export interface FoldCurrent {
  readonly seq: bigint;
  readonly signerKey: Uint8Array;
  readonly peerId: string;
  readonly withSound: boolean;
  readonly accountKey: Uint8Array;
  readonly isOwn: boolean;
  free?(): void;
}

/** The fold, as this class uses it. The wasm `CallScreenShareState`
 *  satisfies it; `V` is its `VerifiedCallScreenShare`. */
export interface ShareFold<V> {
  nextStartSeq(): bigint;
  observe(state: V): boolean;
  startOwn(seq: bigint, peerId: string, withSound: boolean): boolean;
  stopOwn(): bigint | undefined;
  ownSuperseded(): bigint | undefined;
  participantLeft(signerKey: Uint8Array): boolean;
  current(): FoldCurrent | undefined;
  free?(): void;
}

/** The share on screen, copied out of the fold. */
export interface CurrentShare {
  /** `seq:signerHex`: one share, whichever device folds it. */
  readonly key: string;
  readonly seq: bigint;
  /** The sharer's SFU peer id, or the one-to-one peer id. */
  readonly peerId: string;
  readonly withSound: boolean;
  /** The person (base64 of the account key); `null` for this device's own
   *  share, whose fold entry carries none. */
  readonly accountKeyB64: string | null;
  readonly isOwn: boolean;
  readonly signerKeyHex: string;
}

/** What the controller needs from its call. */
export interface ScreenShareControllerDeps<V> {
  readonly fold: ShareFold<V>;
  /** This device's signed state for share `seq`, or `null` when it cannot be
   *  composed now (no identity, no relay key). */
  compose(seq: bigint, sharing: boolean, withSound: boolean): Uint8Array | null;
  /** Put a state on the call's transport. Best-effort. */
  send(state: Uint8Array): void;
  /** End this device's capture (the share was superseded). */
  stopCapture(): void;
  /** The share on screen changed. */
  onChange(current: CurrentShare | null): void;
  /** This device's share was stopped because `by` started. */
  onSuperseded?(by: CurrentShare | null): void;
}

/** Lower-case hex. */
export function hexOfBytes(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}


/** Copy the fold's answer into plain data and release the wasm object. */
export function copyCurrent(c: FoldCurrent | undefined): CurrentShare | null {
  if (c === undefined) return null;
  try {
    const signerKeyHex = hexOfBytes(c.signerKey);
    const account = c.accountKey;
    return {
      key: `${c.seq.toString()}:${signerKeyHex}`,
      seq: c.seq,
      peerId: c.peerId,
      withSound: c.withSound,
      accountKeyB64: c.isOwn || account.length === 0 ? null : base64Encode(account),
      isOwn: c.isOwn,
      signerKeyHex,
    };
  } finally {
    c.free?.();
  }
}

export class ScreenShareController<V> {
  private ownSeq: bigint | null = null;
  private ownWithSound = false;
  private last: CurrentShare | null = null;
  private disposed = false;

  constructor(private readonly deps: ScreenShareControllerDeps<V>) {}

  /** Whether this device is sharing now. */
  ownSharing(): boolean {
    return this.ownSeq !== null;
  }

  /** Whether this device's live share carries sound. */
  ownShareHasSound(): boolean {
    return this.ownSeq !== null && this.ownWithSound;
  }

  /** The share on screen now. */
  current(): CurrentShare | null {
    return this.last;
  }

  /**
   * This device started sharing as `peerId`. Returns the share's seq, or
   * `null` when it could not be recorded (already sharing, or disposed).
   */
  startOwn(peerId: string, withSound: boolean): bigint | null {
    if (this.disposed || this.ownSeq !== null) return null;
    const seq = this.deps.fold.nextStartSeq();
    this.deps.fold.startOwn(seq, peerId, withSound);
    this.ownSeq = seq;
    this.ownWithSound = withSound;
    this.sendState(seq, true, withSound);
    this.afterChange();
    return seq;
  }

  /** This device stopped sharing. A no-op when it was not. */
  stopOwn(): void {
    if (this.disposed) return;
    const seq = this.ownSeq;
    if (seq === null) return;
    const folded = this.deps.fold.stopOwn();
    this.ownSeq = null;
    this.sendState(folded ?? seq, false, this.ownWithSound);
    this.afterChange();
  }

  /** A verified state from another participant. */
  receive(state: V): void {
    if (this.disposed) return;
    try {
      this.deps.fold.observe(state);
    } catch {
      // A state the fold refuses (a malformed key) changes nothing.
      return;
    }
    this.afterChange();
  }

  /** The participant whose leaf key is `signerKey` left the call. */
  participantLeft(signerKey: Uint8Array): void {
    if (this.disposed) return;
    this.deps.fold.participantLeft(signerKey);
    this.afterChange();
  }

  /** Send this device's live `sharing` state again (a peer joined, or the
   *  connection came back). Nothing when it is not sharing. */
  resend(): void {
    if (this.disposed || this.ownSeq === null) return;
    this.sendState(this.ownSeq, true, this.ownWithSound);
  }

  /** Free the fold. Further calls do nothing. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.ownSeq = null;
    this.deps.fold.free?.();
    if (this.last !== null) {
      this.last = null;
      this.deps.onChange(null);
    }
  }

  private sendState(seq: bigint, sharing: boolean, withSound: boolean): void {
    let state: Uint8Array | null;
    try {
      state = this.deps.compose(seq, sharing, withSound);
    } catch {
      state = null;
    }
    if (state === null) return;
    try {
      this.deps.send(state);
    } catch {
      // Best-effort: the next join re-sends a live share.
    }
  }

  private afterChange(): void {
    const superseded = this.ownSeq === null ? undefined : this.deps.fold.ownSuperseded();
    if (superseded !== undefined && this.ownSeq !== null) {
      // The share lost: end the capture first, so the browser's own bar goes
      // away, then say it stopped.
      try {
        this.deps.stopCapture();
      } catch {
        // The capture is gone either way once its tracks are stopped.
      }
      this.deps.fold.stopOwn();
      this.ownSeq = null;
      this.sendState(superseded, false, this.ownWithSound);
      const now = copyCurrent(this.deps.fold.current());
      this.emit(now);
      this.deps.onSuperseded?.(now);
      return;
    }
    this.emit(copyCurrent(this.deps.fold.current()));
  }

  private emit(now: CurrentShare | null): void {
    const before = this.last;
    if (before?.key === now?.key && before?.withSound === now?.withSound) return;
    this.last = now;
    this.deps.onChange(now);
  }
}
