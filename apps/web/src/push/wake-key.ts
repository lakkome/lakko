// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * push-gw's X25519 wake key (ADR-0078 section 3).
 *
 * Every `x-lakko-wake` header is a wake sealed to this key, so the Delivery
 * Service forwards a blob it cannot read. Two sources, the device-attest key
 * pattern:
 *
 *   * a **build-time pin**, `VITE_LAKKO_PUSH_WAKE_KEY` (base64url, 32 bytes):
 *     when set, the key is never fetched;
 *   * otherwise `GET /push/wake-key`, fetched once per page and cached in
 *     memory.
 *
 * A wrong key costs a wake (push-gw cannot open it), never confidentiality:
 * the sealed wake carries nothing the server did not see in the clear before.
 * When the key cannot be had, a publish goes out without a wake header, and
 * recipients catch up on their next poll.
 */
import { base64UrlDecodeNoPad } from "../auth/account-id";
import { PUSH_KEY_LEN, PushClient } from "./client";

const PINNED_B64URL =
  (import.meta.env.VITE_LAKKO_PUSH_WAKE_KEY as string | undefined)?.trim() ?? "";

let cached: Promise<Uint8Array | null> | null = null;

/** The pinned key, or `null` when none is configured (or it is malformed). */
export function pinnedWakeKey(raw: string = PINNED_B64URL): Uint8Array | null {
  if (raw.length === 0) return null;
  try {
    const key = base64UrlDecodeNoPad(raw);
    return key.length === PUSH_KEY_LEN ? key : null;
  } catch {
    return null;
  }
}

/**
 * push-gw's wake key, or `null` when it cannot be had right now. A failed
 * fetch is not cached, so the next publish tries again.
 */
export function pushWakeKey(client: () => PushClient = () => new PushClient()): Promise<Uint8Array | null> {
  const pinned = pinnedWakeKey();
  if (pinned !== null) return Promise.resolve(pinned);
  if (cached !== null) return cached;
  const attempt = client()
    .wakeKey()
    .then(
      (k): Uint8Array | null => k,
      (err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("push wake key unavailable; publishing without a wake", err);
        cached = null;
        return null;
      },
    );
  cached = attempt;
  return attempt;
}

/** Forget the cached key (tests). */
export function resetPushWakeKeyForTests(): void {
  cached = null;
}
