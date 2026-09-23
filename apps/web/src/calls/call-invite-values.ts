// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The values a call link carries now that nobody is asked about them
 * (`docs/ux/call-links.md` section 4).
 *
 * `CallInviteCapability` still has fields for media, expiry, use count and
 * label. Section 1 deleted the form that asked for them, so the client fills
 * them, and section 4 says what with:
 *
 * | Field       | Value                  | Why                                           |
 * |-------------|------------------------|-----------------------------------------------|
 * | `call_id`   | the live call's        | What makes the link lead HERE — the caller's. |
 * | `media`     | audio + video          | One kind of call.                             |
 * | `expiry_ms` | a fixed window         | Never the never-expires sentinel.             |
 * | `max_uses`  | unbounded              | The host admits each arrival.                 |
 * | `label`     | empty                  | Nothing to say the call screen does not.      |
 *
 * **The two that are decided, not merely typed, live in Rust.** The expiry
 * window and the use count are fields of a signed capability, so the wire is
 * what they have to agree with, and `lakko_contacts::call_invite` holds them:
 * `callInviteExpiryMs` and `callInviteMaxUses` here are the wasm surface over
 * that one copy. This module existed for a day with its own twelve-hour
 * constant while Android carried twenty-four, which is exactly the drift
 * section 4 asks to avoid and exactly what three hand-written copies of one
 * decision produce.
 *
 * The other two stay here because there is nothing to decide about them: the
 * media flag and the empty label are what this product sends, not what the
 * format constrains.
 */
import { loadWasm } from "../wasm";

/** Audio and video. One kind of call; the camera is a control inside it. */
export const CALL_INVITE_MEDIA_VIDEO = true;

/** Nothing to label. The call screen already says whose call it is. */
export const CALL_INVITE_LABEL = "";

/**
 * The wire `expiry_ms` for a link minted at `nowMs`, from the shared rule.
 *
 * Never `0n`. That is the never-expires sentinel, and a link that works
 * forever is the stalking risk the field's own documentation names; the
 * shared implementation saturates rather than wrapping, so no clock can
 * produce it.
 *
 * Throws when wasm is not loaded, which cannot happen on the path that calls
 * it: minting a link needs the wasm composer anyway, so a guess here would be
 * a second, quieter expiry rule rather than a fallback.
 */
export function callInviteExpiryMs(nowMs: number): bigint {
  const wasm = rules();
  if (wasm === null) throw new Error("call link values are not ready yet");
  return wasm.callInviteExpiryMs(BigInt(Math.trunc(nowMs)));
}

/**
 * No ceiling on redemptions, from the shared value.
 *
 * Throws for the same reason, and never guesses: a client that quietly
 * substituted its own cap would sign a capability the rest of the product
 * does not agree with.
 */
export function callInviteMaxUses(): number {
  const wasm = rules();
  if (wasm === null) throw new Error("call link values are not ready yet");
  return wasm.callInviteMaxUses();
}

let loaded: Awaited<ReturnType<typeof loadWasm>> | null = null;
void loadWasm().then((w) => {
  loaded = w;
});

function rules(): Awaited<ReturnType<typeof loadWasm>> | null {
  return loaded;
}
