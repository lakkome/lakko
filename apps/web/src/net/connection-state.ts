// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether this device can reach the Lakko servers - the one source behind the
 * "Waiting for network..." / "Connecting..." header (`docs/ux/connection-state.md`).
 *
 * The signal is this device's own requests: a request that got no HTTP answer
 * (`fetch` rejected with a `TypeError`: no route, DNS, refused, reset, TLS)
 * is a network failure; any HTTP answer, whatever its status, means the
 * network works; a request this device aborted is neither. The browser's
 * `navigator.onLine` and its `online` / `offline` events are an early hint,
 * never the last word: an answer always returns the state to online.
 *
 * Fed by the DS client (every journal, mailbox and statement request) and by
 * `sessionFetch` (every bearer request). The desktop host answers a request it
 * could not forward itself; it marks that answer with
 * {@link UNREACHABLE_HEADER} and it counts as a network failure.
 *
 * The transition table ({@link nextConnectionPhase}) is pure so it can be run
 * by a test; vitest here has no jsdom and no Solid transform.
 */

import { createSignal, type Accessor } from "solid-js";

/** The three states of section 1. */
export type ConnectionPhase = "online" | "waiting" | "connecting";

/** What can happen to the state (section 3). */
export type ConnectionEvent =
  /** A request got no HTTP answer. */
  | "network-failure"
  /** A request got an HTTP answer, any status. */
  | "answered"
  /** The browser says there is no route. */
  | "os-offline"
  /** The browser says a route is back. */
  | "os-online";

/** Header the desktop host sets on an answer it gave because it could not
 *  reach the server (`apps/desktop/src-tauri/src/proxy.rs`). */
export const UNREACHABLE_HEADER = "x-lakko-unreachable";

/** The transition table of `docs/ux/connection-state.md` section 3. Pure. */
export function nextConnectionPhase(
  phase: ConnectionPhase,
  event: ConnectionEvent,
): ConnectionPhase {
  switch (event) {
    case "network-failure":
    case "os-offline":
      return "waiting";
    case "answered":
      return "online";
    case "os-online":
      return phase === "waiting" ? "connecting" : phase;
  }
}

/**
 * Whether a rejected request is a network failure. A `fetch` rejects with a
 * `TypeError` when no HTTP answer came; an abort is a `DOMException` named
 * `AbortError` and is not one, nor is anything a caller threw itself. Pure.
 */
export function isNetworkFailure(
  err: unknown,
  signal?: AbortSignal | null,
): boolean {
  if (signal?.aborted === true) return false;
  if (err instanceof Error && err.name === "AbortError") return false;
  return err instanceof TypeError;
}

const [phase, setPhase] = createSignal<ConnectionPhase>("online");
const [announced, setAnnounced] = createSignal<ConnectionPhase | null>(null);
const returnListeners = new Set<() => void>();
const phaseListeners = new Set<
  (now: ConnectionPhase, before: ConnectionPhase) => void
>();
const answerListeners = new Set<() => void>();

/** Reactive: the current phase. */
export const connectionPhase: Accessor<ConnectionPhase> = phase;

/** Reactive: the phase last entered by a transition, for the polite live
 *  region; `null` until the first transition. */
export const connectionAnnouncement: Accessor<ConnectionPhase | null> =
  announced;

/** Feed one event. A transition to a different phase is announced once. */
export function noteConnectionEvent(event: ConnectionEvent): void {
  if (event === "answered") {
    for (const listener of [...answerListeners]) {
      try {
        listener();
      } catch {
        // A listener's failure is its own.
      }
    }
  }
  const before = phase();
  const after = nextConnectionPhase(before, event);
  if (after === before) return;
  setPhase(after);
  setAnnounced(after);
  for (const listener of [...phaseListeners]) {
    try {
      listener(after, before);
    } catch {
      // A listener's failure is its own.
    }
  }
  if (after === "connecting") {
    for (const listener of [...returnListeners]) {
      try {
        listener();
      } catch {
        // A listener only wakes a sleeping loop; its failure is not ours.
      }
    }
  }
}

/** Calls `listener` on every change of phase, with the new and the old one. */
export function onConnectionPhaseChange(
  listener: (now: ConnectionPhase, before: ConnectionPhase) => void,
): () => void {
  phaseListeners.add(listener);
  return () => {
    phaseListeners.delete(listener);
  };
}

/** Calls `listener` on every answer from the servers, whatever the phase
 *  was: what tells a page that just loaded, whose phase reads Online before
 *  any request, that the servers really answer (section 7.3). */
export function onServersAnswer(listener: () => void): () => void {
  answerListeners.add(listener);
  return () => {
    answerListeners.delete(listener);
  };
}

/** Calls `listener` when the network comes back (Waiting -> Connecting), so a
 *  receive loop retries at once instead of finishing its backoff. */
export function onNetworkReturned(listener: () => void): () => void {
  returnListeners.add(listener);
  return () => {
    returnListeners.delete(listener);
  };
}

/** Record the outcome of one request that settled with a response. */
export function noteResponse(response: Response): void {
  noteConnectionEvent(
    response.headers.get(UNREACHABLE_HEADER) === "1"
      ? "network-failure"
      : "answered",
  );
}

/** Record a rejected request; anything but a network failure changes nothing. */
export function noteRejection(err: unknown, signal?: AbortSignal | null): void {
  if (isNetworkFailure(err, signal)) noteConnectionEvent("network-failure");
}

/** Await `request`, recording its outcome, and pass it through unchanged. */
export async function observeRequest(
  request: Promise<Response>,
  signal?: AbortSignal | null,
): Promise<Response> {
  try {
    const response = await request;
    noteResponse(response);
    return response;
  } catch (err) {
    noteRejection(err, signal);
    throw err;
  }
}

let installed = false;

/** Listen to the browser's own online / offline signal. Called once at boot. */
export function installConnectionListeners(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  if (typeof navigator !== "undefined" && navigator.onLine === false) {
    noteConnectionEvent("os-offline");
  }
  window.addEventListener("offline", () => noteConnectionEvent("os-offline"));
  window.addEventListener("online", () => noteConnectionEvent("os-online"));
}

/** Test seam: back to the starting state with no listeners. */
export function resetConnectionStateForTest(): void {
  setPhase("online");
  setAnnounced(null);
  returnListeners.clear();
  phaseListeners.clear();
}
