// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Push notification-tap deep-link target — the PURE half.
 *
 * ## What this closes
 *
 * `public/sw.js` has always routed a notification tap somewhere:
 *
 *   * **No tab open** — `clients.openWindow("/?wake=<hex>")`.
 *   * **A tab already open** — `client.focus()`.
 *
 * ...but nothing in `src/` ever read `?wake=`, and the click path
 * posted no message at all, so the tap only ever landed the user on
 * the app shell (or merely raised the existing tab). That loose end
 * is recorded in `docs/security/known-gaps.md` G-36.
 *
 * This module owns every decision in the fix; the Solid glue that
 * consumes it lives in `WakeNavigator.tsx`. Splitting them is
 * deliberate: `apps/web`'s vitest config runs the plain `node`
 * environment with no jsdom / solid-testing-library, so a `.tsx`
 * component is not unit-testable here — the logic therefore lives in
 * a `.ts` module that is.
 *
 * ## What the hex is (ADR-0042)
 *
 * The SW hands us a lowercase-hex 32-byte id that is EITHER:
 *
 *   * the REAL DS `group_id` — the SW reverse-resolved the wake's
 *     opaque `push_gid` for the bound account through the
 *     `lakko-push-routing`/`account_routes` index (`sw.js::admitWake`),
 *     or the wake was a legacy pre-ADR-0042 raw-group_id one; or
 *   * the raw `push_gid` itself, from a notification shown by an older
 *     worker build (since 2026-09-30 the worker shows nothing for a wake
 *     it cannot resolve for the bound account).
 *
 * A `push_gid` is `keyed-BLAKE3(secret, …)` — algebraically unrelated
 * to any `group_id` — so the second case simply never matches a group
 * in the registry and is dropped. We do not, and must not, try to
 * resolve it any further: see the trust note below.
 *
 * ## Trust
 *
 * The `?wake=` query parameter is ATTACKER-INFLUENCEABLE — anyone can
 * hand a user a `https://lakko.me/?wake=<anything>` link. It is
 * therefore treated as a pure UX hint with exactly one permitted
 * effect: *navigate to a chat this device ALREADY has in its
 * in-memory registry*. It never creates, fetches, subscribes to, or
 * decrypts anything, and it cannot make an unknown id become known.
 * Worst case for a hostile link is that the victim lands on one of
 * their own existing chats.
 */
import { base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { paths } from "../routes/paths";

/** Query-string parameter `sw.js::notificationclick` opens a cold tab with. */
export const WAKE_QUERY_PARAM = "wake";

/**
 * `postMessage` discriminator the SW sends to an ALREADY-OPEN tab from
 * its `notificationclick` handler.
 *
 * Deliberately distinct from the pre-existing `LAKKO_PUSH_WAKE` the SW
 * broadcasts from its `push` handler: that one fires on message
 * ARRIVAL, for every open tab, and navigating on it would yank a
 * reading user into another chat on every inbound message. Only an
 * explicit tap may navigate.
 */
export const NOTIFICATION_TAP_MESSAGE = "LAKKO_NOTIFICATION_TAP";

/**
 * A Lakko `group_id` is 32 bytes, i.e. 64 hex chars — the same gate
 * `public/sw-push-decrypt.js` applies to a wake's `g`.
 */
const GROUP_ID_HEX_LEN = 64;

/** A validated tap target: the registry key plus the route to open. */
export interface WakeTarget {
  /** Standard-alphabet base64 of the 32-byte id — the `ChatApi.get` key. */
  readonly groupIdB64: string;
  /** `/chat/<base64url-no-pad>` — the route `paths.chat(...)` builds. */
  readonly chatPath: string;
}

/** What the caller should do with a pending target right now. */
export type WakeDecision =
  /** Open this route and forget the target. */
  | { readonly kind: "navigate"; readonly path: string }
  /** The app cannot yet tell whether the group exists — keep waiting. */
  | { readonly kind: "wait" }
  /** Nothing to do (no target, or a target that will never resolve). */
  | { readonly kind: "drop" };

/**
 * Turn the SW-supplied hex into a navigable target, or `null` when the
 * input is anything other than exactly 64 hex characters.
 *
 * Silently rejects: `null` / `undefined` / empty, odd length, wrong
 * length, non-hex characters, whitespace, embedded NULs. Case is
 * normalised (the SW always emits lowercase; a hand-copied link may
 * not, and the case carries no meaning once decoded).
 */
export function parseWakeTarget(
  raw: string | null | undefined,
): WakeTarget | null {
  if (typeof raw !== "string") return null;
  if (raw.length !== GROUP_ID_HEX_LEN) return null;
  const hex = raw.toLowerCase();
  const bytes = hexToBytes(hex);
  if (bytes === null) return null;
  return {
    groupIdB64: base64Encode(bytes),
    chatPath: paths.chat(base64UrlEncodeNoPad(bytes)),
  };
}

/**
 * Decide what to do with a pending tap target.
 *
 * `ready` means "this device has finished restoring its MLS state for
 * the signed-in account", i.e. the auth context reports a live session
 * AND the in-memory identity secrets are present. That second half is
 * the load-bearing one: `AuthContext` awaits `rehydrateMlsState(...)`
 * BEFORE it calls `setIdentitySecrets(...)` on every session-
 * establishing path (warm login, signup, recovery-phrase completion),
 * so once secrets are in hand the group registry is fully populated
 * and a miss is a REAL miss rather than a race.
 *
 * That is also why there is no timer here. A cold reload restores a
 * `session` with no secrets (the app asks for the password again), so
 * the target simply waits — nothing is blocked, nothing spins, and if
 * the user re-enters their password minutes later the tap still lands
 * them on the right chat. If they never do, the target dies with the
 * page having done nothing.
 */
export function decideWakeNavigation(
  target: WakeTarget | null,
  ready: boolean,
  isGroupKnown: (groupIdB64: string) => boolean,
): WakeDecision {
  if (target === null) return { kind: "drop" };
  if (!ready) return { kind: "wait" };
  if (!isGroupKnown(target.groupIdB64)) return { kind: "drop" };
  return { kind: "navigate", path: target.chatPath };
}

/**
 * Read the raw `?wake=` value out of a `location.search` string.
 * Returns `null` when absent or empty. No validation — that is
 * `parseWakeTarget`'s job.
 */
export function readWakeParam(search: string): string | null {
  let params: URLSearchParams;
  try {
    params = new URLSearchParams(search);
  } catch {
    return null;
  }
  const raw = params.get(WAKE_QUERY_PARAM);
  if (raw === null || raw.length === 0) return null;
  return raw;
}

/**
 * Drop `?wake=` from an absolute href and return the router-relative
 * `pathname + search + hash` remainder, so a refresh cannot re-fire
 * the navigation. Other query parameters and the hash are preserved.
 *
 * Returns `null` when the href does not parse (never expected —
 * callers pass `window.location.href`) so the caller can skip the
 * rewrite rather than navigate somewhere unintended.
 */
export function stripWakeParam(href: string): string | null {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return null;
  }
  url.searchParams.delete(WAKE_QUERY_PARAM);
  return `${url.pathname}${url.search}${url.hash}`;
}

/**
 * Extract the group hex from a service-worker `message` event payload,
 * or `null` when the payload is not a notification tap.
 *
 * Deliberately strict about the discriminator so the SW's unrelated
 * `LAKKO_PUSH_WAKE` broadcast (and any future message kind) can never
 * drive a navigation.
 */
export function parseTapMessage(data: unknown): string | null {
  if (data === null || typeof data !== "object") return null;
  const msg = data as { type?: unknown; groupHex?: unknown };
  if (msg.type !== NOTIFICATION_TAP_MESSAGE) return null;
  return typeof msg.groupHex === "string" ? msg.groupHex : null;
}

/** Lowercase-hex → bytes. `null` on any non-hex or odd-length input. */
function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    const hi = hexDigit(hex.charCodeAt(i * 2));
    const lo = hexDigit(hex.charCodeAt(i * 2 + 1));
    if (hi === null || lo === null) return null;
    out[i] = (hi << 4) | lo;
  }
  return out;
}

/** One lowercase-hex digit → its value, or `null`. */
function hexDigit(code: number): number | null {
  if (code >= 0x30 && code <= 0x39) return code - 0x30; // 0-9
  if (code >= 0x61 && code <= 0x66) return code - 0x61 + 10; // a-f
  return null;
}
