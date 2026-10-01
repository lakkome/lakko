// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §4 — the client half of the GIF proxy (`services/gif-proxy`).
 *
 * Everything the picker shows and everything a pick sends goes through Lakko's
 * own proxy, same-origin under `/api/gifs`: the service's paths start with
 * `/gifs/`, so a search is `/api/gifs/gifs/search?q=&offset=`, and an item's
 * relative `preview_url` / `full_url` (`/gifs/media/{id}/{variant}`) are
 * loaded as `/api/gifs` + that path. No URL from a response is ever used
 * unless it is exactly that relative proxy shape — the client never follows a
 * response somewhere else.
 *
 * Every route needs `Authorization: Bearer <session token>`, which an `<img>`
 * cannot send; previews are therefore fetched here and shown from object URLs.
 *
 * Failure mapping, per UX spec §5 states:
 *
 *   * 503 `gif-provider-not-configured` → `not-configured` (the tab hides)
 *   * 429 `rate-limited` + `Retry-After`  → `rate-limited`
 *   * 401                                  → `unauthorized`
 *   * 502 / anything else / network error  → `unavailable` (Retry)
 */
import { t } from "../l10n";

export const GIF_API_BASE = "/api/gifs";
/** UX spec §5 — about 350 ms after typing stops. */
export const GIF_SEARCH_DEBOUNCE_MS = 350;
/** Mirrors `services/gif-proxy` `MAX_QUERY_CHARS`. */
export const MAX_GIF_QUERY_CHARS = 100;
/** Mirrors the proxy's full-rendition cap. */
export const MAX_GIF_FULL_BYTES = 8 * 1024 * 1024;
/** Mirrors the proxy's preview cap. */
export const MAX_GIF_PREVIEW_BYTES = 2 * 1024 * 1024;
/** ADR-0047 §1 — attachment dimensions are bounded to this. */
export const MAX_GIF_DIMENSION = 4096;
/** Content types the proxy may answer media with (ADR-0047 §4). */
export const GIF_MEDIA_TYPES: readonly string[] = ["image/gif", "image/webp", "video/mp4"];

export interface GifItem {
  readonly id: string;
  /** Absolute same-origin path, already prefixed with `GIF_API_BASE`. */
  readonly previewUrl: string;
  readonly fullUrl: string;
  readonly width: number;
  readonly height: number;
}

export interface GifPage {
  readonly items: readonly GifItem[];
  readonly nextOffset: number | null;
}

export type GifFailure =
  | { readonly kind: "not-configured" }
  | { readonly kind: "rate-limited"; readonly retryAfterSecs: number }
  | { readonly kind: "unauthorized" }
  | { readonly kind: "unavailable" };

export type GifResult<T> = { readonly kind: "ok"; readonly value: T } | GifFailure;

/** The list URL for `query` (trimmed; empty means Trending) at `offset`. */
export function gifListUrl(query: string, offset: number): string {
  const q = query.trim().slice(0, MAX_GIF_QUERY_CHARS);
  const off = Number.isInteger(offset) && offset > 0 ? offset : 0;
  if (q.length === 0) return `${GIF_API_BASE}/gifs/trending?offset=${off}`;
  return `${GIF_API_BASE}/gifs/search?q=${encodeURIComponent(q)}&offset=${off}`;
}

const MEDIA_PATH = /^\/gifs\/media\/[A-Za-z0-9]{1,64}\/(preview|full)$/;

/** `GIF_API_BASE` + `relative` when `relative` is exactly a proxy media path;
 *  `null` otherwise (absolute URLs, other hosts, traversal, query strings). */
export function gifMediaUrl(relative: unknown): string | null {
  if (typeof relative !== "string" || !MEDIA_PATH.test(relative)) return null;
  return `${GIF_API_BASE}${relative}`;
}

function positiveDimension(v: unknown): number | null {
  return typeof v === "number" && Number.isInteger(v) && v >= 1 && v <= MAX_GIF_DIMENSION
    ? v
    : null;
}

/** Parse a list response body. Items that do not match the contract are
 *  dropped individually; a body that is not a list at all is `null`. */
export function parseGifPage(body: unknown): GifPage | null {
  if (body === null || typeof body !== "object") return null;
  const raw = body as { items?: unknown; next_offset?: unknown };
  if (!Array.isArray(raw.items)) return null;
  const items: GifItem[] = [];
  for (const it of raw.items) {
    if (it === null || typeof it !== "object") continue;
    const r = it as Record<string, unknown>;
    const previewUrl = gifMediaUrl(r.preview_url);
    const fullUrl = gifMediaUrl(r.full_url);
    const width = positiveDimension(r.width);
    const height = positiveDimension(r.height);
    if (
      typeof r.id !== "string" ||
      previewUrl === null ||
      fullUrl === null ||
      width === null ||
      height === null
    ) {
      continue;
    }
    items.push({ id: r.id, previewUrl, fullUrl, width, height });
  }
  const next = raw.next_offset;
  const nextOffset = typeof next === "number" && Number.isInteger(next) && next > 0 ? next : null;
  return { items, nextOffset };
}

/** Map a non-2xx response to the failure the UI shows. */
export function failureFromResponse(response: Response): GifFailure {
  if (response.status === 503) return { kind: "not-configured" };
  if (response.status === 401) return { kind: "unauthorized" };
  if (response.status === 429) {
    const header = Number(response.headers.get("retry-after"));
    const retryAfterSecs = Number.isFinite(header) && header > 0 ? Math.ceil(header) : 1;
    return { kind: "rate-limited", retryAfterSecs };
  }
  return { kind: "unavailable" };
}

function authHeaders(sessionToken: string): HeadersInit {
  return { authorization: `Bearer ${sessionToken}` };
}

export type FetchFn = (input: string, init?: RequestInit) => Promise<Response>;

export async function fetchGifPage(
  query: string,
  offset: number,
  sessionToken: string,
  fetchFn: FetchFn = (i, init) => fetch(i, init),
  signal?: AbortSignal,
): Promise<GifResult<GifPage>> {
  let response: Response;
  try {
    response = await fetchFn(gifListUrl(query, offset), {
      method: "GET",
      headers: authHeaders(sessionToken),
      cache: "no-store",
      ...(signal === undefined ? {} : { signal }),
    });
  } catch {
    return { kind: "unavailable" };
  }
  if (response.status === 503) {
    // Only the proxy's own body counts as "not configured": a 503 from a
    // proxy in front of it (nginx with the service down) is an outage, and
    // hiding the tab for an outage would be wrong.
    const body = await readJson(response);
    const code = body !== null && typeof body === "object" ? (body as { error?: unknown }).error : undefined;
    return code === "gif-provider-not-configured" ? { kind: "not-configured" } : { kind: "unavailable" };
  }
  if (!response.ok) return failureFromResponse(response);
  const page = parseGifPage(await readJson(response));
  return page === null ? { kind: "unavailable" } : { kind: "ok", value: page };
}

async function readJson(response: Response): Promise<unknown> {
  try {
    return (await response.json()) as unknown;
  } catch {
    return null;
  }
}

export interface GifMedia {
  readonly bytes: Uint8Array;
  /** The response's type, parameters stripped; always in `GIF_MEDIA_TYPES`. */
  readonly mime: string;
}

/**
 * Fetch one rendition through the proxy. Refuses a response whose type is not
 * an allowed media type or whose body exceeds `maxBytes`, so nothing outside
 * the contract is ever handed to an element or encrypted into a message.
 */
export async function fetchGifMedia(
  url: string,
  sessionToken: string,
  maxBytes: number,
  fetchFn: FetchFn = (i, init) => fetch(i, init),
  signal?: AbortSignal,
): Promise<GifResult<GifMedia>> {
  if (!url.startsWith(`${GIF_API_BASE}/gifs/media/`)) return { kind: "unavailable" };
  let response: Response;
  try {
    response = await fetchFn(url, {
      method: "GET",
      headers: authHeaders(sessionToken),
      ...(signal === undefined ? {} : { signal }),
    });
  } catch {
    return { kind: "unavailable" };
  }
  if (!response.ok) {
    if (response.status === 503) {
      const body = await readJson(response);
      const code = body !== null && typeof body === "object" ? (body as { error?: unknown }).error : undefined;
      return code === "gif-provider-not-configured" ? { kind: "not-configured" } : { kind: "unavailable" };
    }
    return failureFromResponse(response);
  }
  const type = (response.headers.get("content-type") ?? "").split(";")[0]?.trim().toLowerCase() ?? "";
  if (!GIF_MEDIA_TYPES.includes(type)) return { kind: "unavailable" };
  const declared = Number(response.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > maxBytes) return { kind: "unavailable" };
  let buffer: ArrayBuffer;
  try {
    buffer = await response.arrayBuffer();
  } catch {
    return { kind: "unavailable" };
  }
  if (buffer.byteLength === 0 || buffer.byteLength > maxBytes) return { kind: "unavailable" };
  return { kind: "ok", value: { bytes: new Uint8Array(buffer), mime: type } };
}

export interface Timers {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

const realTimers: Timers = {
  setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
  clearTimeout: (h) => globalThis.clearTimeout(h as ReturnType<typeof setTimeout>),
};

/**
 * Trailing-edge debounce: `schedule(fn)` runs the LAST scheduled `fn` once
 * `delayMs` pass with no newer call. `flush` runs a pending call now (Enter in
 * the search field); `cancel` drops it (the picker closed).
 */
export function createDebouncer(delayMs: number, timers: Timers = realTimers) {
  let handle: unknown = null;
  let pending: (() => void) | null = null;
  return {
    schedule(fn: () => void): void {
      if (handle !== null) timers.clearTimeout(handle);
      pending = fn;
      handle = timers.setTimeout(() => {
        handle = null;
        const run = pending;
        pending = null;
        run?.();
      }, delayMs);
    },
    flush(): void {
      if (handle !== null) timers.clearTimeout(handle);
      handle = null;
      const run = pending;
      pending = null;
      run?.();
    },
    cancel(): void {
      if (handle !== null) timers.clearTimeout(handle);
      handle = null;
      pending = null;
    },
  };
}

// ----- copy (UX spec §5) -------------------------------------------------

export const GIF_PRIVACY_NOTE = (): string => t("chat.gif.privacy_note");
export const GIF_NO_RESULTS = (): string => t("chat.gif.no_results");
export const GIF_UNAVAILABLE = (): string => t("chat.gif.unavailable");
export const GIF_RATE_LIMITED = (): string => t("chat.gif.rate_limited");
export const GIF_ATTRIBUTION = (): string => t("chat.gif.attribution");
export const GIF_PRIVACY_NOTE_STORAGE_KEY = "lakko.gif.privacyNoteDismissed";
