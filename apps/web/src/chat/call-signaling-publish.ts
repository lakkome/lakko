// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Outbound call-signaling publisher — the `onOutboundSignaling` sink a 1×1
 * `CallSession` hands its SDP offer / answer / ICE candidates to.
 *
 * ## Why this is a module and not a closure in `Chat.tsx`
 *
 * It was a closure in `Chat.tsx`, and it was the ONLY
 * `createApplicationMessage` call site in `apps/web/src` outside
 * `chat/client.ts` — it sealed and published on its own, bypassing both
 * disciplines that method documents as security-critical:
 *
 *   - the per-group MLS lock, so a signaling message could be sealed at one
 *     epoch and published across a commit that changes it. `lakko-mls` never
 *     sets `max_past_epochs`, so it is openmls's default of **0** and such a
 *     message is not late but permanently undecryptable for the peer — a lost
 *     answer or ICE candidate silently kills the call;
 *   - the fail-closed durable persist, so the gen-N ciphertext reached the DS
 *     while the N+1 sender ratchet lived only in the in-memory provider. A 1×1
 *     call emits one of these per offer / answer / ICE candidate — easily a
 *     dozen unpersisted ratchet advances — and closing the tab after a call is
 *     the ordinary user action, which re-opens the nonce-reuse-on-crash window
 *     `ChatClient.sealAndPublish` exists to close.
 *
 * {@link ChatClient.broadcastControl} is exactly that seam: it takes
 * `withGroupMlsLock`, persists fail-closed before publishing, registers the DS
 * id so the poll drops the sender's own echo, and skips the push wake (call
 * signaling should wake nobody). Both natives already routed signaling through
 * their equivalent — iOS `ChatViewModel` → `publishApplicationMessage`, Android
 * `ChatViewModel` → the locked send — so web was the outlier.
 *
 * Living in a `.ts` module rather than inside the `.tsx` component is what
 * makes the rule testable: vitest here runs in plain node with no JSX
 * transform, so nothing reachable only from `Chat.tsx` can be asserted at all.
 *
 * ## The push wake
 *
 * `broadcastControl` fires no wake by default — the ADR-0042 rule that a
 * control envelope must not wake devices. That rule swept up the call offer,
 * so a callee with a closed or backgrounded tab never rang
 * (`docs/ux/calls.md` §2.1). This publisher is the one caller that opts back
 * in: the session states with each frame whether it rings someone or stops a
 * ring (the dial offer, the End of a call that never connected), and the
 * publisher wakes only when it asked and the bytes are an offer or an end.
 * The bytes alone cannot decide: a mid-call renegotiation offer and the End
 * of a connected call carry the same tags, and a call wake on either rang the
 * peer's iPhone (`calls/call-signaling-wake.ts`).
 */
import type { ChatClient } from "./client";
import {
  shouldWakeForCallSignaling,
  type CallSignalingWake,
} from "../calls/call-signaling-wake";
import type { IdentitySecrets } from "../auth/AuthContext";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";

/** Everything the publisher needs, read lazily so it always sees the live
 *  session and the current `Group` handle (which `ChatContext` replaces on an
 *  ADR-0040 commit persist-abort). */
export interface CallSignalingPublisherDeps {
  readonly chat: ChatClient;
  readonly identity: () => IdentitySecrets | null;
  readonly group: () => WasmGroup;
  /** Reporting sink for a failed publish. Defaults to a console warning. */
  readonly onError?: (err: unknown) => void;
}

/**
 * Build the publisher for group-call control frames (start, join, leave, end).
 *
 * Every one of them is published with no wake, `end` included. A group call
 * never rings a closed device (start / join / leave were always silent), so a
 * group `end` stops no ring anywhere; its call wake reached iPhones as a VoIP
 * push, which iOS reports to CallKit as an incoming call, and showed a phantom
 * incoming call after every group call ended.
 */
export function makeGroupCallControlPublisher(
  deps: CallSignalingPublisherDeps,
): (bytes: Uint8Array) => Promise<void> {
  const publish = makeCallSignalingPublisher(deps);
  return (bytes: Uint8Array): Promise<void> => publish(bytes, "none");
}

/**
 * Build the `onOutboundSignaling` sink.
 *
 * The returned function NEVER rejects — a failed publish is reported through
 * `onError` and swallowed, because `CallSession` calls it synchronously and
 * would have nowhere to put a rejection. It still returns the promise so a
 * caller (and this module's tests) can await the publish; `CallSession`'s
 * `void`-returning callback type accepts it and ignores it.
 */
export function makeCallSignalingPublisher(
  deps: CallSignalingPublisherDeps,
): (bytes: Uint8Array, wake: CallSignalingWake) => Promise<void> {
  return async (bytes: Uint8Array, wake: CallSignalingWake): Promise<void> => {
    try {
      // The wake rides the DS publish itself as an `x-lakko-wake` header
      // (ADR-0054); the DS forwards it to push-gw after the row is appended.
      // Resolving it never fails the publish: a device without the group's
      // `push_gid` publishes silently, so asking for one here cannot fail the
      // call.
      await deps.chat.broadcastControl(
        deps.identity(),
        deps.group(),
        bytes,
        shouldWakeForCallSignaling(bytes, wake) ? { wake: "call" } : {},
      );
    } catch (err) {
      if (deps.onError !== undefined) {
        deps.onError(err);
        return;
      }
      // eslint-disable-next-line no-console
      console.warn("call signaling publish failed", err);
    }
  };
}
