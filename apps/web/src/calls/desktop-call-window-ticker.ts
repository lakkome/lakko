// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The desktop call window's clock (`docs/ux/calls.md` section 4.5.4a): a
 * dedicated worker that posts `tick` at the interval it is started with.
 *
 * The main page's own timers fire once a second while Lakko's window is
 * minimised; a worker's are throttled far less. Measured with the desktop
 * probe on macOS 27.2 (WKWebView), window minimised, per second: a 50 ms
 * main-page timer 1, a 33 ms worker timer 8.3, a 50 ms one 7.3, a 100 ms one
 * 5.3. Windows and Linux are not measured.
 *
 * Messages in: `{ every: ms }` starts (or restarts) the clock, `stop` stops
 * it. Out: `"tick"`.
 */

const scope = self as unknown as DedicatedWorkerGlobalScope;
let timer: ReturnType<typeof setInterval> | null = null;

scope.onmessage = (event: MessageEvent<unknown>) => {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
  const data = event.data;
  if (typeof data === "object" && data !== null && "every" in data) {
    const every = Number((data as { every: unknown }).every);
    if (Number.isFinite(every) && every > 0) timer = setInterval(() => scope.postMessage("tick"), every);
  }
};
