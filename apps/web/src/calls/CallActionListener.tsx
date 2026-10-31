// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Accept and Decline pressed on a ringing notification — the GLUE half.
 *
 * Renders nothing. Mounted once in the router's root layout, beside
 * `WakeNavigator` and for the same reason: it must outlive every navigation,
 * including the `/` -> `/login` -> `/` bounce a cold open performs when the
 * session needs a password re-entry, because the whole point is to survive
 * long enough for a call the user pressed Accept on to reach the slot.
 *
 * On the desktop there is no service worker: the host reports a press of the
 * system notification's buttons (`platform/host.ts`), and that is a third
 * input source of the same intent.
 *
 * Every decision lives in the pure, unit-tested `call-notification-intent.ts`.
 * This file is only the two input sources, the reactive wait, and the two
 * calls into the call host — which is also why it is a `.tsx` with no logic:
 * vitest here runs plain node with no Solid transform, so anything decided in
 * this file would be untestable.
 *
 * ## The wait, and why there is one
 *
 * On a cold open the user has already pressed Accept, but nothing is ringing
 * yet: the page still has to boot, restore the session, and let the
 * background receive pull the offer off the DS journal. So a pending intent
 * is HELD and re-decided as the app comes up. It is bounded by the 45 s ring
 * timeout (read, not redefined — see `CALL_ACTION_MAX_WAIT_MS`): past that
 * nothing can match, and holding it longer would risk answering a LATER call.
 *
 * ## Why a ticking signal rather than a timer that acts
 *
 * The wait ends on one of two events — the call starts ringing, or the budget
 * runs out — and only the first is a state change Solid can observe. A slow
 * tick supplies the second as an ordinary reactive input, so both are handled
 * by the one `createEffect` and there is no second code path that can answer
 * a call.
 */
import { createEffect, createSignal, onCleanup, onMount } from "solid-js";
import type { JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { onHostCallAction } from "../platform/host";
import { currentInternalPath, entrySearch, replaceInternalPath } from "../routes/origin-history";
import { useCallHost } from "./CallHostContext";
import {
  decideCallAction,
  parseCallActionMessage,
  readCallActionParams,
  stripCallActionParams,
  type CallNotificationIntent,
} from "./call-notification-intent";

/** How often the pending intent is re-decided while it waits. */
const TICK_MS = 500;

export function CallActionListener(): JSX.Element {
  const auth = useAuth();
  const host = useCallHost();
  const [pending, setPending] = createSignal<CallNotificationIntent | null>(null);
  const [tick, setTick] = createSignal(0);
  let arrivedAt = 0;

  function arm(intent: CallNotificationIntent | null): void {
    if (intent === null) return;
    arrivedAt = Date.now();
    setPending(intent);
  }

  onMount(() => {
    // (1) Cold open: the worker had no window to talk to and opened one.
    //     Parse BEFORE the strip — the strip rewrites the URL we read — and
    //     strip either way, so a refresh can never answer a call twice.
    //     The address bar is already `/` (`routes/origin-history.ts`): read
    //     the entry path's query and strip from the router's current path.
    arm(readCallActionParams(entrySearch()));
    const here = currentInternalPath();
    const cleaned = stripCallActionParams(new URL(here, window.location.origin).href);
    if (cleaned !== null && cleaned !== here) {
      // Rewrite the entry rather than navigate: the only change is to the
      // query, and a navigation here would race `WakeNavigator`'s own strip
      // of `?wake=` on the very same entry.
      replaceInternalPath(cleaned);
    }

    // The desktop host: a press on the system notification.
    onCleanup(
      onHostCallAction(({ action, callId, video }) => {
        arm({ kind: action, callIdHex: callId, groupHex: "", video });
      }),
    );

    // (2) A window was already open: the worker posted the press to it.
    if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) return;
    const sw = navigator.serviceWorker;
    const onMessage = (event: MessageEvent): void => {
      arm(parseCallActionMessage(event.data));
    };
    sw.addEventListener("message", onMessage);
    // Load-bearing, same as in `WakeNavigator`: a `ServiceWorkerContainer`
    // only starts dispatching queued messages once `onmessage` is assigned or
    // `startMessages()` is called, and we register with `addEventListener` so
    // as not to clobber another listener.
    if (typeof sw.startMessages === "function") sw.startMessages();
    onCleanup(() => sw.removeEventListener("message", onMessage));
  });

  // The clock half of the wait. Runs only while something is pending, so an
  // idle app carries no timer.
  createEffect(() => {
    if (pending() === null) return;
    const id = setInterval(() => setTick((n) => n + 1), TICK_MS);
    onCleanup(() => clearInterval(id));
  });

  createEffect(() => {
    const intent = pending();
    if (intent === null) return;
    tick(); // re-decide on every tick as well as on every state change
    const decision = decideCallAction(intent, {
      ready: auth.state().kind === "session" && auth.identitySecrets() !== null,
      ringingCallIdHex: host.ringingCallIdHex(),
      waitedMs: Date.now() - arrivedAt,
    });
    if (decision.kind === "wait") return;
    // Single-shot either way: consumed on action, discarded on drop. Cleared
    // BEFORE acting so a re-entrant state change cannot act on it twice.
    setPending(null);
    if (decision.kind === "accept") {
      void host.acceptIncomingCall(decision.withVideo).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("accepting a call from a notification failed", err);
      });
    } else if (decision.kind === "decline") {
      void host.declineIncomingCall().catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("declining a call from a notification failed", err);
      });
    }
  });

  return null;
}
