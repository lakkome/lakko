// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The screen share as the call screen reads it (ADR-0083,
 * `docs/ux/screen-share.md`).
 *
 * One call at a time is on screen, and one share at a time is in a call, so
 * this is one module-level store beside `state/call-state.ts`. The call
 * adapters write it (`sfu-call-ui.ts`, the one-to-one host in
 * `CallHostContext.tsx`); the call screen and the collapsed bar read it. The
 * share's media lives here, not in the roster: `Participant` carries no
 * track by design, and a share is not a participant.
 */

import { createSignal, type Accessor } from "solid-js";
import type { GroupLayoutKind } from "./group-call-layout";
import { isRemoteShare, shareLayoutStep } from "./screen-share-layout";

/** The share on screen, named. */
export interface ShareOnScreen {
  /** `seq:signerHex`: one share, whichever device folds it. */
  readonly key: string;
  /** The sharer's SFU peer id, or the one-to-one peer id. */
  readonly peerId: string;
  /** The sharer's name; empty while it cannot be resolved, and for this
   *  device's own share. */
  readonly name: string;
  readonly isOwn: boolean;
  readonly withSound: boolean;
}

/** What the active call lets the screen do. */
export interface ScreenShareControl {
  /** Open the browser's picker and share what the person picks. Called from
   *  the button's press, so the browser sees the person's gesture. */
  start(): void;
  /** Stop this device's share. */
  stop(): void;
}

const [current, setCurrent] = createSignal<ShareOnScreen | null>(null);
const [streams, setStreams] = createSignal<ReadonlyMap<string, MediaStream>>(new Map());
const [control, setControl] = createSignal<ScreenShareControl | null>(null);
const [busy, setBusy] = createSignal(false);
/** What a share moved the group layout from (`shareLayoutStep`), kept here
 *  rather than in the call screen, which is taken down when the call is put
 *  away and would forget it. */
let layoutShowing = false;
let layoutSaved: GroupLayoutKind | null | undefined;

/** Read-only accessors. */
export const screenShareState: {
  readonly current: Accessor<ShareOnScreen | null>;
  readonly control: Accessor<ScreenShareControl | null>;
  /** The picker is open or the share is being set up. */
  readonly busy: Accessor<boolean>;
  /** The received share's stream, by sharer peer id. */
  readonly streams: Accessor<ReadonlyMap<string, MediaStream>>;
} = { current, control, busy, streams };

/** The stream of the share on screen, or `undefined` (none, this device's
 *  own, or not arrived yet). */
export function currentShareStream(): MediaStream | undefined {
  const c = current();
  if (c === null || c.isOwn) return undefined;
  return streams().get(c.peerId);
}

export const screenShareActions = {
  setCurrent(next: ShareOnScreen | null): void {
    const before = current();
    if (
      before?.key === next?.key &&
      before?.name === next?.name &&
      before?.withSound === next?.withSound &&
      before?.isOwn === next?.isOwn
    ) {
      return;
    }
    setCurrent(next);
  },
  setControl(next: ScreenShareControl | null): void {
    setControl(next);
  },
  setBusy(on: boolean): void {
    setBusy(on);
  },
  /** A share stream of `peerId` arrived (`stream`) or went (`null`). With
   *  `only` given, only that very stream is removed, so a replaced stream's
   *  late `ended` does not drop its successor. */
  setStream(peerId: string, stream: MediaStream | null, only?: MediaStream): void {
    setStreams((prev) => {
      if (stream === null) {
        if (!prev.has(peerId)) return prev;
        if (only !== undefined && prev.get(peerId) !== only) return prev;
        const next = new Map(prev);
        next.delete(peerId);
        return next;
      }
      if (prev.get(peerId) === stream) return prev;
      const next = new Map(prev);
      next.set(peerId, stream);
      return next;
    });
  },
  /**
   * The layout the group call shows now that the share on screen is what it
   * is (section 5.2), from `choice`, the person's own: `null` to leave it.
   */
  stepLayout(choice: GroupLayoutKind | null): { readonly choice: GroupLayoutKind | null } | null {
    const showing = isRemoteShare(current());
    if (showing === layoutShowing) return null;
    const next = shareLayoutStep(layoutShowing, showing, choice, layoutSaved);
    layoutShowing = showing;
    layoutSaved = next.saved;
    return next.choice === choice ? null : { choice: next.choice };
  },
  /** Nothing is shared and nothing can be: the call ended. */
  reset(): void {
    layoutShowing = false;
    layoutSaved = undefined;
    setCurrent(null);
    setStreams(new Map());
    setControl(null);
    setBusy(false);
  },
};
