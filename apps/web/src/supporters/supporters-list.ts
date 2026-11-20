// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The supporters list (ADR-0097 section 2, `docs/ux/supporters.md`
 * section 2): a list of names signed by the offline release key and served
 * as a static file at `/.well-known/lakko-supporters` on lakko.me.
 *
 * - Verified in the wasm core (`verifySupportersList`) under the build-time
 *   pinned release key (`pinnedReleaseKey`, `VITE_LAKKO_RELEASE_KEY`); no key
 *   pinned means no list and no About row.
 * - A list that fails the check is ignored as if absent; one whose version
 *   is not above the held one is ignored.
 * - The held list is kept in `localStorage` (no account in its key; every
 *   access wrapped), so the About row shows at once on the next open.
 * - Fetched when About opens (forced) and otherwise at most once a day.
 *
 * Nothing here is an E2EE control: a stripped or replayed list only hides
 * or keeps an older thank-you.
 */

/** The `localStorage` key of the held list. */
export const SUPPORTERS_STORE_KEY = "lakko.supporters.v1";

/** Same-origin path of the signed list. */
export const SUPPORTERS_PATH = "/.well-known/lakko-supporters";

/** A list far larger than any real one is refused before it is parsed. */
export const MAX_SUPPORTERS_BYTES = 1024 * 1024;

/** At most one background fetch a day. */
export const SUPPORTERS_REFRESH_MS = 24 * 60 * 60 * 1000;

/** A verified list as held on the device. */
export interface HeldSupporters {
  /** `list_version` (decimal). */
  readonly version: string;
  readonly names: readonly string[];
  /** When this device last asked the server (Unix ms). */
  readonly fetchedAtMs: number;
}

export type SupportersStorage = Pick<Storage, "getItem" | "setItem">;

export function defaultSupportersStorage(): SupportersStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** The held list, or `null`. */
export function readHeldSupporters(storage: SupportersStorage | null): HeldSupporters | null {
  if (storage === null) return null;
  try {
    const raw = storage.getItem(SUPPORTERS_STORE_KEY);
    if (raw === null) return null;
    const v = JSON.parse(raw) as { version?: unknown; names?: unknown; fetchedAtMs?: unknown };
    if (
      typeof v.version !== "string" ||
      !/^[0-9]+$/.test(v.version) ||
      !Array.isArray(v.names) ||
      !v.names.every((n) => typeof n === "string") ||
      typeof v.fetchedAtMs !== "number"
    ) {
      return null;
    }
    return { version: v.version, names: v.names as string[], fetchedAtMs: v.fetchedAtMs };
  } catch {
    return null;
  }
}

function writeHeld(storage: SupportersStorage | null, held: HeldSupporters): void {
  if (storage === null) return;
  try {
    storage.setItem(SUPPORTERS_STORE_KEY, JSON.stringify(held));
  } catch {
    // The list stays in memory for this page.
  }
}

/** Whether the About row shows: a verified list with at least one name. */
export function showsSupportersRow(held: HeldSupporters | null): boolean {
  return held !== null && held.names.length > 0;
}

/** Whether a background fetch is due (`force`: About was opened). */
export function supportersFetchDue(
  held: HeldSupporters | null,
  nowMs: number,
  force: boolean,
): boolean {
  if (force || held === null) return true;
  return nowMs - held.fetchedAtMs >= SUPPORTERS_REFRESH_MS || nowMs < held.fetchedAtMs;
}

/** The newer of the held list and a freshly verified one: a fetched list
 *  replaces the held one only when its version is strictly higher. */
export function keepNewer(
  held: HeldSupporters | null,
  fetched: { readonly version: bigint; readonly names: readonly string[] },
  nowMs: number,
): HeldSupporters {
  if (held !== null && fetched.version <= BigInt(held.version)) {
    return { ...held, fetchedAtMs: nowMs };
  }
  return { version: fetched.version.toString(), names: [...fetched.names], fetchedAtMs: nowMs };
}

/** The subset of the wasm core used here (injectable in tests). */
export interface SupportersWasm {
  verifySupportersList(
    bytes: Uint8Array,
    key: Uint8Array,
  ): { readonly listVersion: bigint; readonly names: string[]; free(): void };
}

export interface SupportersDeps {
  readonly wasm: () => Promise<SupportersWasm>;
  readonly fetch: typeof fetch;
  /** The pinned release key, or `null` (no list at all). */
  readonly key: Uint8Array | null;
  readonly storage: SupportersStorage | null;
  readonly nowMs: () => number;
  readonly url?: string;
}

/** What a refresh ended with. `failed`: no verified list came back (network,
 *  a non-2xx, an over-large body or a failed check). */
export type RefreshResult =
  | { readonly kind: "ok"; readonly held: HeldSupporters | null }
  | { readonly kind: "failed"; readonly held: HeldSupporters | null }
  | { readonly kind: "dormant" };

/**
 * Fetch, verify and keep the list. Never throws. With no release key pinned
 * the result is `dormant` and nothing is fetched.
 */
export async function refreshSupporters(deps: SupportersDeps, force: boolean): Promise<RefreshResult> {
  if (deps.key === null) return { kind: "dormant" };
  const held = readHeldSupporters(deps.storage);
  if (!supportersFetchDue(held, deps.nowMs(), force)) return { kind: "ok", held };
  let bytes: Uint8Array;
  try {
    const response = await deps.fetch(deps.url ?? SUPPORTERS_PATH, {
      method: "GET",
      credentials: "omit",
      referrerPolicy: "no-referrer",
      cache: "no-store",
    });
    if (!response.ok) return { kind: "failed", held };
    const buf = await response.arrayBuffer();
    if (buf.byteLength === 0 || buf.byteLength > MAX_SUPPORTERS_BYTES) return { kind: "failed", held };
    bytes = new Uint8Array(buf);
  } catch {
    return { kind: "failed", held };
  }
  let fetched: { version: bigint; names: string[] };
  try {
    const wasm = await deps.wasm();
    const verified = wasm.verifySupportersList(bytes, deps.key);
    try {
      fetched = { version: verified.listVersion, names: [...verified.names] };
    } finally {
      verified.free();
    }
  } catch {
    // eslint-disable-next-line no-console
    console.warn("supporters list: refused");
    return { kind: "failed", held };
  }
  const next = keepNewer(held, fetched, deps.nowMs());
  writeHeld(deps.storage, next);
  return { kind: "ok", held: next };
}
