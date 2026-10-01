// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The address bar shows the origin and nothing else.
 *
 * Every screen change inside the app is private: a chat id, a group id, an
 * invite secret or a `?wake=` parameter never appears in the address bar, in
 * the browser history list, in a bookmark or in a screenshot of the window.
 * The router still has real paths; they live in `history.state` instead of
 * the URL, and every entry this module writes has the URL `/`.
 *
 * - **Entry.** A page load reads its path once: from `history.state` when the
 *   load is a reload or a back / forward into this app (the screen the user
 *   was on), otherwise from the URL it was opened with (an invite link, a
 *   notification's `/?wake=`, a typed address). The URL is then replaced with
 *   `/` before the app renders.
 * - **Navigation.** A push or replace writes `{ path, state }` into
 *   `history.state` at the URL `/`, so back and forward keep working and the
 *   router's navigation state survives them and a reload.
 * - **Readers of the entry URL** (`WakeNavigator`, `CallActionListener`) read
 *   `currentInternalPath()` rather than `window.location`, which is `/` by the
 *   time they run.
 */
import type { LocationChange, MemoryHistory } from "@solidjs/router";

const PATH_KEY = "lakkoPath";
const STATE_KEY = "lakkoState";

interface Entry {
  readonly value: string;
  readonly state: unknown;
}

function isEntryState(s: unknown): s is Record<string, unknown> {
  return typeof s === "object" && s !== null && typeof (s as Record<string, unknown>)[PATH_KEY] === "string";
}

let entryPath = "/";

function readEntry(): Entry {
  const s: unknown = window.history.state;
  if (isEntryState(s)) {
    return { value: s[PATH_KEY] as string, state: s[STATE_KEY] ?? undefined };
  }
  return { value: entryPath, state: undefined };
}

function write(value: string, state: unknown, replace: boolean): void {
  const prev: unknown = window.history.state;
  const base = typeof prev === "object" && prev !== null ? (prev as Record<string, unknown>) : {};
  // Keep the router's own bookkeeping (`_depth`) on a replace; a push starts
  // a fresh entry.
  const next = replace
    ? { ...base, [PATH_KEY]: value, [STATE_KEY]: state }
    : { [PATH_KEY]: value, [STATE_KEY]: state };
  if (replace) window.history.replaceState(next, "", "/");
  else window.history.pushState(next, "", "/");
}

/**
 * Capture the entry path and clear the address bar. Call once, before the
 * app renders.
 */
export function installOriginHistory(): void {
  if (typeof window === "undefined") return;
  const s: unknown = window.history.state;
  if (isEntryState(s)) {
    entryPath = s[PATH_KEY] as string;
    if (window.location.pathname !== "/" || window.location.search !== "" || window.location.hash !== "") {
      // A reload of an address someone typed over ours: the typed one wins.
      entryPath = `${window.location.pathname}${window.location.search}${window.location.hash}`;
      write(entryPath, undefined, true);
    }
    return;
  }
  entryPath = `${window.location.pathname}${window.location.search}${window.location.hash}`;
  write(entryPath, undefined, true);
}

/**
 * The query string of the path this page load started on (`?wake=...`), for
 * the one-shot readers. Fixed for the life of the page, so the order the
 * readers run in does not matter; each strips its own parameters with
 * [`replaceInternalPath`] (or a replacing navigation) so a reload is inert.
 */
export function entrySearch(): string {
  const q = entryPath.indexOf("?");
  if (q < 0) return "";
  const h = entryPath.indexOf("#", q);
  return h < 0 ? entryPath.slice(q) : entryPath.slice(q, h);
}

/** The path the router is on, query and fragment included. */
export function currentInternalPath(): string {
  return readEntry().value;
}

/**
 * Rewrite the current entry's path without a navigation: for a reader that
 * consumed a one-shot query parameter and must not see it again on reload.
 */
export function replaceInternalPath(path: string): void {
  write(path, readEntry().state, true);
}

/**
 * The `MemoryRouter` history backed by `window.history`. `get` and the
 * listener hand the router `{ value, state }`; `createRouter` accepts either
 * a string or that object, the published type only names the string.
 */
export const originHistory: MemoryHistory = {
  get: () => readEntry() as unknown as string,
  set: ({ value, replace, scroll, state }: LocationChange) => {
    write(value, state, replace === true);
    if (scroll === true) window.scrollTo(0, 0);
  },
  go: (delta: number) => window.history.go(delta),
  listen: (listener: (value: string) => void) => {
    const handler = (): void => listener(readEntry() as unknown as string);
    window.addEventListener("popstate", handler);
    return () => window.removeEventListener("popstate", handler);
  },
};
