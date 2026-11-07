// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account's own channel reactions and comments (ADR-0087 section 13a):
 * two `lakko-sync` registers, kept in the account's synced state and carried
 * between its devices over the DeviceGroup like the quick reaction. Never
 * asked of the channels service: a request naming several write keys would
 * link exactly what the per-write keys keep apart.
 *
 * The DeviceGroup provider (`devices/sync/DeviceGroupSyncContext.tsx`)
 * installs the port once the synced state is loaded and bumps the version on
 * every refresh; the channel screens read and write through the functions
 * below. Before the port is installed the reads answer "nothing held" and a
 * write is refused (the caller keeps the reaction off).
 */
import { createSignal } from "solid-js";

/** What the provider lends the screens. Each call reads or writes the fold. */
export interface OwnFeedbackPort {
  readonly accountIdB64: string;
  /** The emoji this account holds on the post. */
  reactionsOn(channelId: Uint8Array, postCursor: number): string[];
  /** The 16-byte nonces of this account's live comments under the post. */
  commentNoncesOn(channelId: Uint8Array, postCursor: number): Uint8Array[];
  /** Record `emoji` on (`reacted`) or off the post, fold it here and send it
   *  to the account's other devices. `false` when nothing changed. Throws
   *  for a fourth own reaction on one post. */
  setReaction(channelId: Uint8Array, postCursor: number, emoji: string, reacted: boolean, postAtMs: number): Promise<boolean>;
  /** The emoji this account holds on a comment (ADR-0087 phase B). */
  commentReactionsOn(channelId: Uint8Array, postCursor: number, commentCursor: number): string[];
  /** Record `emoji` on or off a comment, fold it here and send it to the
   *  account's other devices. `postAtMs` is the POST's time. `false` when
   *  nothing changed. Throws for a fourth own reaction on one comment. */
  setCommentReaction(
    channelId: Uint8Array,
    postCursor: number,
    commentCursor: number,
    emoji: string,
    reacted: boolean,
    postAtMs: number,
  ): Promise<boolean>;
  /** Record a comment this account wrote, before it is posted. */
  recordComment(channelId: Uint8Array, postCursor: number, nonce: Uint8Array, postAtMs: number): Promise<void>;
  /** Record that this account deleted one of its comments. */
  deleteComment(channelId: Uint8Array, postCursor: number, nonce: Uint8Array, postAtMs: number): Promise<void>;
}

let port: OwnFeedbackPort | null = null;
const [version, setVersion] = createSignal(0);

/** Bumped by the provider on every refresh of the fold. Reactive. */
export function ownFeedbackVersion(): number {
  return version();
}

/** The provider: the fold changed. */
export function ownFeedbackChanged(): void {
  setVersion((v) => v + 1);
}

/** The synced state is loaded: lend the port. Returns the uninstaller. */
export function installOwnFeedbackRegister(next: OwnFeedbackPort): () => void {
  port = next;
  ownFeedbackChanged();
  return () => {
    if (port === next) port = null;
    ownFeedbackChanged();
  };
}

/** The port, or `null` before the synced state is loaded. Not reactive by
 *  itself: read {@link ownFeedbackVersion} first in a reactive scope. */
export function ownFeedbackPort(): OwnFeedbackPort | null {
  return port;
}

/** The emoji this account holds on the post. Reactive. */
export function ownReactionsOn(channelId: Uint8Array, postCursor: number): string[] {
  void version();
  if (port === null) return [];
  try {
    return port.reactionsOn(channelId, postCursor);
  } catch {
    return [];
  }
}

/** The emoji this account holds on a comment. Reactive. */
export function ownCommentReactionsOn(channelId: Uint8Array, postCursor: number, commentCursor: number): string[] {
  void version();
  if (port === null) return [];
  try {
    return port.commentReactionsOn(channelId, postCursor, commentCursor);
  } catch {
    return [];
  }
}

/** The nonces of this account's comments under the post. Reactive. */
export function ownCommentNoncesOn(channelId: Uint8Array, postCursor: number): Uint8Array[] {
  void version();
  if (port === null) return [];
  try {
    return port.commentNoncesOn(channelId, postCursor);
  } catch {
    return [];
  }
}

/** Split a flat run of 16-byte nonces. */
export function splitNonces(flat: Uint8Array): Uint8Array[] {
  const out: Uint8Array[] = [];
  for (let i = 0; i + 16 <= flat.length; i += 16) out.push(flat.slice(i, i + 16));
  return out;
}
