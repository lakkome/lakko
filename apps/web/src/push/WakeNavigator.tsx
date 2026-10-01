// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Push notification-tap deep-link reader — the GLUE half (G-36).
 *
 * Renders nothing. Mounted once as the `<Router root>` layout in
 * `App.tsx` so it outlives every navigation — including the
 * `/` -> `/login` -> `/` bounce a cold open performs when the session
 * needs a password re-entry. All of its decisions live in the pure,
 * unit-tested `wake-target.ts`; this file is only the two input
 * sources, the reactive wait, and the navigate call.
 *
 * ## The two inputs (both from `public/sw.js::notificationclick`)
 *
 *   1. **Cold open** — no Lakko tab existed, so the SW called
 *      `clients.openWindow("/?wake=<hex>")`. We read the param at
 *      mount and immediately strip it (replace, not push) so a
 *      refresh cannot re-navigate.
 *   2. **Already-open tab** — the SW focused the existing client and
 *      posted `{ type: LAKKO_NOTIFICATION_TAP, groupHex }` to it.
 *
 * Both funnel into the same `pending` signal and the same resolver,
 * so the two paths cannot drift.
 *
 * ## The wait
 *
 * A cold open races the app's own boot: the auth context starts in
 * `loading`, and the MLS group registry is empty until
 * `rehydrateMlsState(...)` has run. The resolver therefore holds the
 * target until the session is live AND `identitySecrets()` is present
 * (which `AuthContext` only sets AFTER awaiting the rehydrate), then
 * navigates if the group is known and drops the target if it is not.
 * Nothing blocks or spins in the meantime: an unresolved target is
 * simply today's behaviour — the user stays on the default view.
 *
 * ## Why the navigation is deferred by a macrotask
 *
 * A cold open never has in-memory identity secrets (they are not
 * persisted — a page reload always asks for the password again), so
 * on that path `ready` only flips while the user is ON the login /
 * signup form. Both forms navigate themselves the instant their
 * `await auth.login(...)` resolves:
 *
 *     await auth.login(...);                       // flips our signals
 *     navigate(readReturnUrl(location), { replace: true });
 *
 * Solid flushes effects synchronously inside those signal writes, so
 * an immediate `navigate(chatPath)` here would run BEFORE that line
 * and be replaced by it a microtask later. Scheduling ours on a
 * macrotask puts it strictly after the form's own redirect — the last
 * writer wins, and that is the deep link the user actually tapped.
 * Cost in the warm already-open-tab case is one event-loop turn.
 */
import { createEffect, createSignal, onCleanup, onMount } from "solid-js";
import type { JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { currentInternalPath, entrySearch } from "../routes/origin-history";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import {
  decideWakeNavigation,
  parseTapMessage,
  parseWakeTarget,
  readWakeParam,
  stripWakeParam,
  type WakeTarget,
} from "./wake-target";

/**
 * Consumes the service worker's notification-tap signal and routes to
 * the chat it points at. Returns `null` — it has no UI.
 */
export function WakeNavigator(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const navigate = useNavigate();
  const [pending, setPending] = createSignal<WakeTarget | null>(null);

  // Single in-flight deferred navigation; cleared if the app tears
  // down first so a detached component can never navigate.
  let hop: ReturnType<typeof setTimeout> | null = null;
  onCleanup(() => {
    if (hop !== null) clearTimeout(hop);
  });

  onMount(() => {
    // (1) Cold open: `/?wake=<hex>`.
    // The address bar is already `/` (`routes/origin-history.ts`): read the
    // entry path's query, strip from the router's current path.
    const raw = readWakeParam(entrySearch());
    if (raw !== null) {
      // Parse BEFORE the strip — the strip rewrites the URL we read.
      // A malformed value yields `null` and is silently ignored, but
      // the param is stripped either way so a reload is inert.
      setPending(parseWakeTarget(raw));
      const cleaned = stripWakeParam(new URL(currentInternalPath(), window.location.origin).href);
      if (cleaned !== null) navigate(cleaned, { replace: true });
    }

    // (2) Already-open tab: the SW posts the tap to the focused client.
    if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) {
      return;
    }
    const sw = navigator.serviceWorker;
    const onMessage = (event: MessageEvent): void => {
      const hex = parseTapMessage(event.data);
      if (hex === null) return; // not a tap (e.g. LAKKO_PUSH_WAKE)
      const target = parseWakeTarget(hex);
      if (target !== null) setPending(target);
    };
    sw.addEventListener("message", onMessage);
    // Load-bearing: per the Service Workers spec a `ServiceWorkerContainer`
    // only starts dispatching queued messages once `onmessage` is ASSIGNED
    // or `startMessages()` is called. We register via `addEventListener`
    // (so we never clobber another listener), so without this a tap that
    // arrives before/around the listener registration would sit in the
    // queue forever. Idempotent, and absent on a few older engines.
    if (typeof sw.startMessages === "function") sw.startMessages();
    onCleanup(() => sw.removeEventListener("message", onMessage));
  });

  // Resolver. Re-runs whenever the auth state, the identity secrets,
  // the group registry (read through `chats.get`) or the pending
  // target change — so a target captured before the rehydrate lands
  // resolves the moment the groups appear.
  createEffect(() => {
    const target = pending();
    if (target === null) return;
    const ready =
      auth.state().kind === "session" && auth.identitySecrets() !== null;
    const decision = decideWakeNavigation(target, ready, (groupIdB64) =>
      // `ChatApi.get` reads the `groups` signal, so this subscribes
      // the effect to registry changes.
      chats.get(groupIdB64) !== undefined,
    );
    if (decision.kind === "wait") return;
    // Single-shot either way: consumed on navigate, discarded on drop.
    setPending(null);
    if (decision.kind !== "navigate") return;
    const path = decision.path;
    if (hop !== null) clearTimeout(hop);
    hop = setTimeout(() => {
      hop = null;
      navigate(path);
    }, 0);
  });

  return null;
}
