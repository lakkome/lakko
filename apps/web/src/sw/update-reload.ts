// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the update banner's Reload does, so it always reloads (seen once in a
 * walk on 2026-11-04: "Reloading..." stayed for over 10 s and nothing
 * happened; a plain navigation then loaded the new version).
 *
 * The happy path is unchanged: post `SKIP_WAITING` to the waiting worker, and
 * reload when it takes control (`controllerchange`). Three ways that never
 * comes, each handled:
 *
 *   - the worker is already in control, or past waiting: another tab of the
 *     same site pressed Reload first (the swap and its `controllerchange`
 *     happened before this tab listened), or the browser activated it on
 *     its own. Reload at once.
 *   - the worker became redundant: a newer one replaced it while the banner
 *     was up, so a message to it does nothing. Reload at once; the newer one
 *     is offered again if it still waits.
 *   - the swap is held up (a client the browser waits for, a worker that
 *     never answers). After {@link UPDATE_RELOAD_FALLBACK_MS} the page
 *     reloads anyway; the plain reload is what worked in the walk.
 *
 * Reloads at most once. Pure apart from the injected effects.
 */

/** How long a swap may take before the page reloads anyway. Chosen, not
 *  measured: a normal swap takes well under a second. */
export const UPDATE_RELOAD_FALLBACK_MS = 3_000;

/** The parts of a `ServiceWorker` this reads. */
export interface UpdateWorker {
  readonly state: string;
  postMessage(message: unknown): void;
  addEventListener(type: "statechange", fn: () => void): void;
}

/** The parts of `navigator.serviceWorker` this reads. */
export interface UpdateContainer {
  readonly controller: unknown;
  addEventListener(type: "controllerchange", fn: () => void, opts?: { once?: boolean }): void;
}

export interface UpdateReloadDeps {
  readonly worker: UpdateWorker;
  readonly container: UpdateContainer | null;
  readonly reload: () => void;
  readonly setTimer: (fn: () => void, ms: number) => unknown;
}

/** Why the reload happened, for the log line and the tests. */
export type UpdateReloadReason = "took-control" | "already-active" | "redundant" | "activated" | "timeout";

/** Press Reload. Returns how the reload came about via `onReload`. */
export function reloadIntoUpdate(deps: UpdateReloadDeps, onReload?: (why: UpdateReloadReason) => void): void {
  let done = false;
  const go = (why: UpdateReloadReason): void => {
    if (done) return;
    done = true;
    onReload?.(why);
    deps.reload();
  };
  const w = deps.worker;
  if (deps.container !== null && deps.container.controller === w) {
    go("already-active");
    return;
  }
  if (w.state === "redundant") {
    go("redundant");
    return;
  }
  if (w.state === "activating" || w.state === "activated") {
    go("already-active");
    return;
  }
  deps.container?.addEventListener("controllerchange", () => go("took-control"), { once: true });
  w.addEventListener("statechange", () => {
    if (w.state === "activated") go("activated");
    else if (w.state === "redundant") go("redundant");
  });
  deps.setTimer(() => go("timeout"), UPDATE_RELOAD_FALLBACK_MS);
  try {
    w.postMessage({ type: "SKIP_WAITING" });
  } catch {
    go("redundant");
  }
}
