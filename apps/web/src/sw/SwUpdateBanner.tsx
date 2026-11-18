// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "New version available — Reload" banner (Phase 1.18f).
 *
 * Listens for the `lakko:sw-update-available` event that
 * `register.ts` dispatches when the SW reaches `installed` AND
 * a prior worker is already controlling the page (= update,
 * not first install). On click:
 *
 *   1. Subscribes to `controllerchange` on
 *      `navigator.serviceWorker` — this fires when the new SW
 *      transitions to controlling the page.
 *   2. Posts `{ type: "SKIP_WAITING" }` to the waiting SW.
 *   3. SW activates → `controllerchange` fires → `location.reload()`.
 *   (`update-reload.ts` reloads too when that never comes.)
 *
 * The reload is the load-bearing piece: without it the page
 * would keep running the prior JS while a new SW serves new
 * bytes — exactly the silent-mismatch footgun the explicit
 * banner is meant to avoid.
 *
 * Mounted once at the AppShell level; same instance covers
 * every protected route + the unauthenticated `/login` /
 * `/signup` / `/i/:slug` paths (the auth-shell layout renders
 * the banner too via a sibling mount).
 */
import { createEffect, createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { SW_UPDATE_EVENT, waitingServiceWorker } from "./register";
import { reloadIntoUpdate } from "./update-reload";
import { t } from "../l10n";
import { setBottomStripPx } from "../ui/bottom-strip";

/** The attribute and the custom property on `<html>` that hand the banner's
 *  strip to the layout: `#root` (every route, its forms and composers)
 *  ends above it, so the banner never sits on a control (base walk
 *  2026-11-13: on a 1024x768 window it covered New channel's `Create`). */
export const BANNER_ATTR = "data-update-banner";
export const BANNER_HEIGHT_VAR = "--lk-update-banner-h";

/** Reserve `heightPx` at the bottom of the viewport for the banner, or give
 *  it back with `null`. */
export function reserveBannerStrip(root: HTMLElement, heightPx: number | null): void {
  if (heightPx === null) {
    root.removeAttribute(BANNER_ATTR);
    root.style.removeProperty(BANNER_HEIGHT_VAR);
    setBottomStripPx(0);
    return;
  }
  const px = Math.max(0, Math.ceil(heightPx));
  root.setAttribute(BANNER_ATTR, "");
  root.style.setProperty(BANNER_HEIGHT_VAR, `${px}px`);
  // The collapsed call window places itself from script (CallWindow.tsx).
  setBottomStripPx(px);
}

export function SwUpdateBanner(): JSX.Element {
  const [waiting, setWaiting] = createSignal<ServiceWorker | null>(null);
  const [reloading, setReloading] = createSignal(false);

  onMount(() => {
    const onUpdate = (e: Event): void => {
      // CustomEvent<ServiceWorker> is the shape `register.ts`
      // dispatches; safe-cast here because we own both sides of
      // the contract.
      const detail = (e as CustomEvent<ServiceWorker>).detail;
      setWaiting(detail);
    };
    window.addEventListener(SW_UPDATE_EVENT, onUpdate);
    // An update found before this banner mounted: the event is gone, the
    // worker is not.
    const already = waitingServiceWorker();
    if (already !== null) setWaiting(already);
    onCleanup(() => {
      window.removeEventListener(SW_UPDATE_EVENT, onUpdate);
    });
  });

  function reload(): void {
    const w = waiting();
    if (w === null) return;
    setReloading(true);
    // `update-reload.ts`: the SKIP_WAITING handshake, and a reload however
    // the swap goes, including when it never signals (base walk
    // 2026-11-04: "Reloading..." stood still).
    reloadIntoUpdate(
      {
        worker: w,
        container: typeof navigator !== "undefined" && "serviceWorker" in navigator ? navigator.serviceWorker : null,
        reload: () => window.location.reload(),
        setTimer: (fn, ms) => setTimeout(fn, ms),
      },
      (why) => {
        if (why !== "took-control") {
          // eslint-disable-next-line no-console
          console.warn(`[sw] update reload without the handshake: ${why}`);
        }
      },
    );
  }

  // While it shows, the banner owns a strip at the bottom of the viewport and
  // the page ends above it (`styles.css`, `html[data-update-banner]`).
  let strip: HTMLDivElement | undefined;
  createEffect(() => {
    const html = document.documentElement;
    if (waiting() === null || strip === undefined) {
      reserveBannerStrip(html, null);
      return;
    }
    const el = strip;
    const measure = (): void => reserveBannerStrip(html, el.getBoundingClientRect().height);
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(el);
    onCleanup(() => {
      observer?.disconnect();
      reserveBannerStrip(html, null);
    });
  });

  return (
    <Show when={waiting() !== null}>
      <div ref={(el) => (strip = el)} class="lk-update-banner" role="status" aria-live="polite">
        <div class="lk-update-banner-inner">
          <span class="lk-update-banner-text">{t("app.update.ready")}</span>
          <button class="primary" type="button" onClick={reload} disabled={reloading()}>
            {reloading() ? t("app.update.reloading") : t("app.update.reload")}
          </button>
        </div>
      </div>
    </Show>
  );
}
