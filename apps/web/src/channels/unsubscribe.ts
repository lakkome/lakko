// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 3 - leaving a channel on the server.
 *
 * `DELETE /v1/channels/{id}/subscribers/{pk}` removes the subscriber row, its
 * seed envelopes and its member card. It is signed by the subscriber key the
 * path names: the header named `unsubscribeSigHeaderName()`, whose value
 * `ChannelSubscriberKeypair.unsubscribeRequestHeader(channelId, nowMs)`
 * returns (`<issuedAtMs>.<signature>`, accepted within two minutes of the
 * server's clock).
 *
 * The caller drops the channel locally whatever this returns: the person
 * asked to stop following, and a server that could not be told keeps a row
 * the 90-day subscriber prune removes anyway. The outcome is for the log.
 */
import { loadWasm, type Wasm } from "../wasm";
import { apiError, CHANNELS_UNREACHABLE, unsubscribeSubscriber } from "./api";
import { bytesToHex, channelIdFromHex, type ChannelSubscriberKeys } from "./composer";

/** How the server side of leaving went. */
export type LeaveOutcome =
  /** Removed, or already gone (`404` counts as done). */
  | { readonly kind: "left" }
  /** The request never reached the server. */
  | { readonly kind: "network" }
  /** The server answered with a refusal (or something that is not the
   *  channels service). */
  | { readonly kind: "refused"; readonly status: number; readonly code: string }
  /** The request could not be signed (a key that is not a current Ed25519
   *  subscriber key, a malformed channel id). */
  | { readonly kind: "unsigned" };

export interface LeaveDeps {
  readonly wasm?: Wasm;
  readonly now?: () => number;
  readonly send?: typeof unsubscribeSubscriber;
}

/** Tell the channels service this subscriber leaves. Never throws. */
export async function leaveChannelOnServer(
  channelIdHex: string,
  keys: ChannelSubscriberKeys,
  deps: LeaveDeps = {},
): Promise<LeaveOutcome> {
  const wasm = deps.wasm ?? (await loadWasm());
  const now = deps.now ?? ((): number => Date.now());
  const send = deps.send ?? unsubscribeSubscriber;
  let value: string;
  try {
    const kp = new wasm.ChannelSubscriberKeypair(keys.secret);
    try {
      value = kp.unsubscribeRequestHeader(channelIdFromHex(channelIdHex), BigInt(Math.floor(now())));
    } finally {
      kp.free();
    }
  } catch {
    return { kind: "unsigned" };
  }
  try {
    await send(channelIdHex, bytesToHex(keys.public), {
      headerName: wasm.unsubscribeSigHeaderName(),
      value,
    });
    return { kind: "left" };
  } catch (err) {
    if (apiError.is(err)) {
      if (err.status === 404 && err.code !== CHANNELS_UNREACHABLE) return { kind: "left" };
      return { kind: "refused", status: err.status, code: err.code };
    }
    return { kind: "network" };
  }
}
